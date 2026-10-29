//! Versioned SQLite session store.
//!
//! One database per workspace: `<cwd>/.seavx/sessions.db` in debug mode, else
//! `<home>/.seavx/sessions.db` — the same rule `MEMORY.md` follows. The file is
//! `0600`, its directory `0700`, WAL and `synchronous=NORMAL` give the write
//! budget without holding a lock across a turn.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use genai::chat::{ChatMessage, ChatRole, ToolResponse};
use rusqlite::{Connection, OptionalExtension, params};

/// Serde format of a message payload. Bumped only if `genai` changes its
/// `ChatMessage` shape; a mismatch opens the session read-only.
pub const PAYLOAD_FORMAT: &str = "genai-chat-v1";
/// Schema version this build understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Total budget for one write: three attempts, 100 ms and 200 ms waits.
pub const WRITE_BUDGET: Duration = Duration::from_secs(5);
const ATTEMPTS: u32 = 3;
/// Per-attempt `busy_timeout` cap; the deadline shrinks it further when needed.
const BUSY_TIMEOUT_CAP_MS: u64 = 2000;

/// Status of one stored message, from the schema's `CHECK` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    Complete,
    Partial,
    Cancelled,
    Interrupted,
}

impl MessageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "partial" => Self::Partial,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => Self::Complete,
        }
    }
}

/// Turn state derived from the stored rows, never stored itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Idle,
    Pending,
    /// Pending with recovery evidence: an `interrupted` row written by a
    /// recovery pass. A live writer is only ever `Pending`.
    Interrupted,
}

impl TurnState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Pending => "pending",
            Self::Interrupted => "interrupted",
        }
    }
}

/// One row of `sessions list`.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: i64,
    pub title: String,
    pub updated_at: i64,
    pub messages: usize,
    pub state: TurnState,
}

/// One stored message, payload unparsed.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub seq: i64,
    pub role: String,
    pub status: MessageStatus,
    pub payload: String,
    pub created_at: i64,
}

/// A session read back for replay, rendering or export.
#[derive(Debug, Clone)]
pub struct LoadedSession {
    pub id: i64,
    pub workspace: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub payload_format: String,
    /// `Some(reason)` when the rows cannot be appended to: another payload
    /// format, or a payload that does not parse.
    pub read_only: Option<String>,
    pub messages: Vec<StoredMessage>,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
}

/// Stored payload bytes a resume may replay before it starts cutting turns.
pub const RESUME_BUDGET_BYTES: usize = 80_000;

/// The synthetic result for a tool call that never returned.
pub const INTERRUPTED_RESULT: &str =
    "Tool call interrupted: no result was recorded before the session stopped.";

/// What one replay pass produces: the provider history, the prompt to prefill,
/// and how many stored rows were left out.
#[derive(Debug, Clone, Default)]
pub struct ReplayOutcome {
    pub history: Vec<ChatMessage>,
    pub draft: Option<String>,
    pub omitted: usize,
    pub synthetic: Vec<ChatMessage>,
}

/// The one replay function, in the fixed order: unanswered-turn rule, then
/// status rules, then the byte window, then the tool-call repair.
pub fn replay(session: &LoadedSession) -> ReplayOutcome {
    let rows = &session.messages[..];

    // 1. Split into turns at user rows.
    let mut turns: Vec<&[StoredMessage]> = Vec::new();
    let mut start = 0;
    for (index, row) in rows.iter().enumerate() {
        if index > start && row.role == "user" {
            turns.push(&rows[start..index]);
            start = index;
        }
    }
    if start < rows.len() {
        turns.push(&rows[start..]);
    }

    // 2. An unanswered user row is not history, whatever its status; the last
    // one is the draft instead.
    let mut draft = None;
    let mut surviving: Vec<&[StoredMessage]> = Vec::new();
    let last_turn = turns.len().saturating_sub(1);
    for (index, turn) in turns.iter().enumerate() {
        if turn.len() == 1 && turn[0].role == "user" {
            if index == last_turn {
                draft = text_of(&turn[0].payload);
            }
            continue;
        }
        surviving.push(turn);
    }

    // 3. Per-status rules inside the surviving rows.
    let mut kept: Vec<(usize, ChatMessage)> = Vec::new();
    for row in surviving.into_iter().flatten() {
        let Ok(message) = serde_json::from_str::<ChatMessage>(&row.payload) else {
            continue;
        };
        let keep = match row.status {
            MessageStatus::Interrupted => row.role != "assistant",
            MessageStatus::Cancelled | MessageStatus::Partial => has_text(&message),
            MessageStatus::Complete => true,
        };
        if keep {
            kept.push((row.payload.len(), message));
        }
    }

    // 4. The window cuts at a user boundary, newest first; the newest turn is
    // kept whole even when it alone exceeds the budget.
    let mut groups: Vec<Vec<(usize, ChatMessage)>> = Vec::new();
    for (size, message) in kept {
        if message.role == ChatRole::User || groups.is_empty() {
            groups.push(Vec::new());
        }
        if let Some(group) = groups.last_mut() {
            group.push((size, message));
        }
    }
    let mut windowed: Vec<Vec<(usize, ChatMessage)>> = Vec::new();
    let mut used = 0;
    for group in groups.into_iter().rev() {
        let size: usize = group.iter().map(|(size, _)| size).sum();
        if !windowed.is_empty() && used + size > RESUME_BUDGET_BYTES {
            break;
        }
        used += size;
        windowed.push(group);
    }
    windowed.reverse();

    let kept_rows: usize = windowed.iter().map(Vec::len).sum();
    let omitted = rows.len().saturating_sub(kept_rows);

    // 5. Repair: a tool row whose call is outside the window is dropped, a call
    // inside it with no result gets the synthetic interrupted row.
    let flat: Vec<ChatMessage> = windowed.into_iter().flatten().map(|(_, m)| m).collect();
    let called: Vec<String> = flat.iter().flat_map(call_ids_of).collect();
    let answered: Vec<String> = flat
        .iter()
        .filter(|message| message.role == ChatRole::Tool)
        .filter_map(|message| {
            message
                .content
                .tool_responses()
                .first()
                .map(|response| response.call_id.clone())
        })
        .collect();

    let mut history = Vec::with_capacity(flat.len());
    let mut synthetic = Vec::new();
    for message in flat {
        if message.role == ChatRole::Tool
            && message
                .content
                .tool_responses()
                .first()
                .is_none_or(|response| !called.contains(&response.call_id))
        {
            continue;
        }
        let missing: Vec<String> = call_ids_of(&message)
            .into_iter()
            .filter(|call_id| !answered.contains(call_id))
            .collect();
        history.push(message);
        for call_id in missing {
            let patch = ChatMessage::from(ToolResponse::new(call_id, INTERRUPTED_RESULT));
            synthetic.push(patch.clone());
            history.push(patch);
        }
    }

    ReplayOutcome {
        history,
        draft,
        omitted,
        synthetic,
    }
}

fn call_ids_of(message: &ChatMessage) -> Vec<String> {
    message
        .content
        .tool_calls()
        .iter()
        .map(|call| call.call_id.clone())
        .collect()
}

/// The workspace key `create`, `list` and the CLI agree on.
pub fn workspace_key() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// The worker's pre-request step: ensure the session row exists, write the user
/// row, then return the history the request replays. Runs off the UI thread.
pub fn prepare_turn(
    store: &SessionStore,
    workspace: &str,
    model: &str,
    session: Option<i64>,
    prompt: &str,
) -> Result<(i64, Vec<ChatMessage>), SessionError> {
    let id = match session {
        Some(id) => id,
        None => store.create(workspace, Some(model), Some(&title_from(prompt)))?,
    };
    store.record_user(id, prompt)?;
    Ok((id, replay(&store.load(id)?).history))
}

/// A session title: the first line of the prompt, 60 chars at most.
pub fn title_from(prompt: &str) -> String {
    prompt
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(60)
        .collect()
}

fn has_text(message: &ChatMessage) -> bool {
    message
        .content
        .first_text()
        .is_some_and(|text| !text.trim().is_empty())
}

fn text_of(payload: &str) -> Option<String> {
    let message: ChatMessage = serde_json::from_str(payload).ok()?;
    message
        .content
        .first_text()
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

/// The TUI's transcript shape, one stored row at a time. A row that does not
/// parse still renders, from its stored seq, so a read-only session stays
/// readable.
pub fn render_text(session: &LoadedSession) -> String {
    let mut out = String::new();
    for row in &session.messages {
        match serde_json::from_str::<ChatMessage>(&row.payload) {
            Ok(message) => out.push_str(&render_message(&message)),
            Err(_) => out.push_str(&format!(
                "[unreadable message #{} (format mismatch)]\n\n",
                row.seq
            )),
        }
    }
    out
}

/// The same shape for a list of messages already parsed (a replayed history).
pub fn render_messages(messages: &[ChatMessage]) -> String {
    messages.iter().map(render_message).collect()
}

fn render_message(message: &ChatMessage) -> String {
    let mut out = String::new();
    if let Some(text) = message
        .content
        .first_text()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        if message.role == ChatRole::User {
            out.push_str(&format!("User: {text}\n\n"));
        } else {
            out.push_str(&format!("{text}\n\n"));
        }
    }
    for call in message.content.tool_calls() {
        out.push_str(&format!("⚡ {}({})\n\n", call.fn_name, call.fn_arguments));
    }
    for response in message.content.tool_responses() {
        out.push_str(&format!("{}\n\n", response.content.trim()));
    }
    out
}

/// `{"version":1,"session":{…},"messages":[…]}`; an unreadable payload becomes
/// an error object instead of failing the whole export.
pub fn export_json(session: &LoadedSession) -> String {
    let messages: Vec<serde_json::Value> = session
        .messages
        .iter()
        .map(
            |row| match serde_json::from_str::<serde_json::Value>(&row.payload) {
                Ok(payload) => serde_json::json!({
                    "seq": row.seq,
                    "role": row.role,
                    "status": row.status.as_str(),
                    "created_at": row.created_at,
                    "payload": payload,
                }),
                Err(_) => serde_json::json!({ "error": "unparseable message", "seq": row.seq }),
            },
        )
        .collect();

    serde_json::json!({
        "version": 1,
        "session": {
            "id": session.id,
            "workspace": session.workspace,
            "title": session.title,
            "model": session.model,
            "payload_format": session.payload_format,
            "total_tokens": session.total_tokens,
            "prompt_tokens": session.prompt_tokens,
            "read_only": session.read_only,
        },
        "messages": messages,
    })
    .to_string()
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace TEXT NOT NULL,
    title TEXT,
    model TEXT,
    payload_format TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','ended')),
    total_tokens INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messages(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'complete'
        CHECK(status IN ('complete','partial','cancelled','interrupted')),
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);
CREATE INDEX IF NOT EXISTS messages_session_seq ON messages(session_id, seq);
CREATE INDEX IF NOT EXISTS sessions_workspace_updated ON sessions(workspace, updated_at DESC);
";

#[derive(Debug)]
pub enum SessionError {
    /// The file was written by a newer build: refuse rather than downgrade.
    Newer { found: u32, supported: u32 },
    /// Cannot be opened, read or migrated; the run continues unpersisted.
    Unavailable(String),
    /// No session with this id.
    Unknown(i64),
    /// The session predates this build's payload format or payload.
    ReadOnly(String),
    /// The write itself broke an ordering invariant; a programming error.
    Refused(String),
    /// Three attempts inside the write budget could not get the lock.
    Busy,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Newer { found, supported } => write!(
                f,
                "session database version {found} is newer than this build ({supported}); refusing to open it"
            ),
            SessionError::Unavailable(detail) => write!(f, "session store unavailable: {detail}"),
            SessionError::Unknown(id) => write!(f, "unknown session {id}"),
            SessionError::ReadOnly(reason) => write!(f, "session is read-only: {reason}"),
            SessionError::Refused(detail) => write!(f, "session write refused: {detail}"),
            SessionError::Busy => write!(
                f,
                "session store busy: gave up after {ATTEMPTS} attempts inside {} s",
                WRITE_BUDGET.as_secs()
            ),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<rusqlite::Error> for SessionError {
    fn from(error: rusqlite::Error) -> Self {
        SessionError::Unavailable(error.to_string())
    }
}

pub struct SessionStore {
    connection: Connection,
}

impl SessionStore {
    /// The default location for this run.
    pub fn path() -> Option<PathBuf> {
        if crate::util::is_debug() {
            return Some(PathBuf::from(".seavx/sessions.db"));
        }
        crate::home::dir().map(|home| home.join(".seavx/sessions.db"))
    }

    pub fn open() -> Result<Self, SessionError> {
        let path = Self::path()
            .ok_or_else(|| SessionError::Unavailable("home directory is not set".to_string()))?;
        Self::open_at(&path)
    }

    /// Open and migrate. A newer `user_version` is refused before any write, so a
    /// build that does not understand the file never touches it.
    pub fn open_at(path: &Path) -> Result<Self, SessionError> {
        if let Some(directory) = path.parent() {
            create_owner_only(directory, 0o700)
                .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        }

        let connection = Connection::open(path)?;
        let found: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        if found > SCHEMA_VERSION {
            return Err(SessionError::Newer {
                found,
                supported: SCHEMA_VERSION,
            });
        }

        create_owner_only(path, 0o600)
            .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=2000;",
        )?;

        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    /// Create the schema in one transaction and bump `user_version` only after it
    /// commits; a second call is a no-op.
    pub fn migrate(&self) -> Result<(), SessionError> {
        let version: u32 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == SCHEMA_VERSION {
            return Ok(());
        }

        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute_batch(SCHEMA)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
        Ok(())
    }

    /// The connection behind every later read and write.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Insert a session row and return its id.
    pub fn create(
        &self,
        workspace: &str,
        model: Option<&str>,
        title: Option<&str>,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute(
                "INSERT INTO sessions(workspace, title, model, payload_format, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5)",
                params![workspace, title, model, PAYLOAD_FORMAT, now],
            )?;
            Ok(connection.last_insert_rowid())
        })
    }

    /// The one row a submitted prompt writes; never rewritten afterwards.
    pub fn record_user(&self, session: i64, prompt: &str) -> Result<i64, SessionError> {
        self.record_message(session, &ChatMessage::user(prompt), MessageStatus::Complete)
    }

    /// One transaction per message: validate first, then allocate `seq` and
    /// insert under `BEGIN IMMEDIATE`.
    pub fn record_message(
        &self,
        session: i64,
        message: &ChatMessage,
        status: MessageStatus,
    ) -> Result<i64, SessionError> {
        self.ensure_writable(session)?;
        self.check_tool_pairs(session, message)?;
        let payload = serde_json::to_string(message)
            .map_err(|error| SessionError::Refused(error.to_string()))?;
        self.insert_row(session, role_name(&message.role), status, &payload)
    }

    pub fn record_title(&self, session: i64, title: &str) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection
                .execute(
                    "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
                    params![session, title, now],
                )
                .map(|_| ())
        })
    }

    pub fn set_usage(&self, session: i64, total: i64, prompt: i64) -> Result<(), SessionError> {
        self.write(|connection| {
            connection
                .execute(
                    "UPDATE sessions SET total_tokens = ?2, prompt_tokens = ?3 WHERE id = ?1",
                    params![session, total, prompt],
                )
                .map(|_| ())
        })
    }

    /// Sessions of one workspace with at least one message, newest activity
    /// first, each with its derived turn state.
    pub fn list(&self, workspace: &str) -> Result<Vec<SessionSummary>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1
             GROUP BY s.id
             HAVING COUNT(m.id) > 0
             ORDER BY s.updated_at DESC, s.id DESC",
        )?;
        let rows: Vec<(i64, Option<String>, i64, i64)> = statement
            .query_map([workspace], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        drop(statement);

        let mut summaries = Vec::with_capacity(rows.len());
        for (id, title, updated_at, messages) in rows {
            summaries.push(SessionSummary {
                id,
                title: title.unwrap_or_default(),
                updated_at,
                messages: messages as usize,
                state: self.turn_state(id)?,
            });
        }
        Ok(summaries)
    }

    /// Read one session back, rows in `seq` order, marking it read-only when the
    /// payload format or one payload cannot be replayed by this build.
    pub fn load(&self, id: i64) -> Result<LoadedSession, SessionError> {
        let header = self
            .connection
            .query_row(
                "SELECT workspace, title, model, payload_format, total_tokens, prompt_tokens
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;

        let messages = self.rows(id)?;
        let mut read_only = (header.3 != PAYLOAD_FORMAT).then(|| {
            format!(
                "payload format {} does not match {}",
                header.3, PAYLOAD_FORMAT
            )
        });
        if read_only.is_none()
            && let Some(unreadable) = messages
                .iter()
                .find(|message| serde_json::from_str::<ChatMessage>(&message.payload).is_err())
        {
            read_only = Some(format!(
                "message #{} cannot be read as a chat message",
                unreadable.seq
            ));
        }

        Ok(LoadedSession {
            id,
            workspace: header.0,
            title: header.1,
            model: header.2,
            payload_format: header.3,
            read_only,
            messages,
            total_tokens: header.4,
            prompt_tokens: header.5,
        })
    }

    /// Write the synthetic interrupted result for every tool call of the last
    /// assistant row that never got a result. Idempotent, never rewrites a row.
    pub fn record_recovery(&self, session: i64) -> Result<usize, SessionError> {
        let loaded = self.load(session)?;
        if let Some(reason) = loaded.read_only {
            return Err(SessionError::ReadOnly(reason));
        }
        let Some(last) = loaded
            .messages
            .iter()
            .rposition(|row| row.role == "assistant")
        else {
            return Ok(0);
        };
        let answered: Vec<String> = loaded.messages[last + 1..]
            .iter()
            .filter(|row| row.role == "tool")
            .filter_map(|row| response_call_id(&row.payload))
            .collect();

        let mut written = 0;
        for call_id in call_ids(&loaded.messages[last].payload) {
            if answered.contains(&call_id) {
                continue;
            }
            self.record_message(
                session,
                &ChatMessage::from(ToolResponse::new(call_id, INTERRUPTED_RESULT)),
                MessageStatus::Interrupted,
            )?;
            written += 1;
        }
        Ok(written)
    }

    fn rows(&self, session: i64) -> Result<Vec<StoredMessage>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT seq, role, status, payload, created_at FROM messages
             WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([session], |row| {
            Ok(StoredMessage {
                seq: row.get(0)?,
                role: row.get(1)?,
                status: MessageStatus::parse(&row.get::<_, String>(2)?),
                payload: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn turn_state(&self, session: i64) -> Result<TurnState, SessionError> {
        let rows = self.rows(session)?;
        Ok(derive_turn_state(&rows))
    }

    /// A session written by another payload format, or one that does not exist.
    fn ensure_writable(&self, session: i64) -> Result<(), SessionError> {
        let format: Option<String> = self
            .connection
            .query_row(
                "SELECT payload_format FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        match format {
            None => Err(SessionError::Unknown(session)),
            Some(format) if format != PAYLOAD_FORMAT => Err(SessionError::ReadOnly(format!(
                "payload format {format} does not match {PAYLOAD_FORMAT}"
            ))),
            Some(_) => Ok(()),
        }
    }

    /// An assistant row carrying tool calls is committed first; a result whose
    /// `call_id` is not in the newest assistant row is a programming error.
    fn check_tool_pairs(&self, session: i64, message: &ChatMessage) -> Result<(), SessionError> {
        let responses = message.content.tool_responses();
        if responses.is_empty() {
            return Ok(());
        }
        let last_call_row: Option<String> = self
            .connection
            .query_row(
                "SELECT payload FROM messages WHERE session_id = ?1 AND role = 'assistant'
                 ORDER BY seq DESC LIMIT 1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = last_call_row else {
            return Err(SessionError::Refused(
                "a tool result was written before any assistant row".to_string(),
            ));
        };
        let calls: Vec<String> = match serde_json::from_str::<ChatMessage>(&payload) {
            Ok(message) => message
                .content
                .tool_calls()
                .iter()
                .map(|call| call.call_id.clone())
                .collect(),
            Err(_) => Vec::new(),
        };
        for response in responses {
            if !calls.contains(&response.call_id) {
                tracing::error!(
                    call_id = %response.call_id,
                    surface = "session-store",
                    "tool result has no committed assistant call"
                );
                return Err(SessionError::Refused(format!(
                    "tool result for unknown call_id {}",
                    response.call_id
                )));
            }
        }
        Ok(())
    }

    fn insert_row(
        &self,
        session: i64,
        role: &str,
        status: MessageStatus,
        payload: &str,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let inserted = (|| {
                // `seq` is allocated inside the same transaction as the insert,
                // so two writers cannot receive the same number.
                connection.execute(
                    "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
                     SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5
                     FROM messages WHERE session_id = ?1",
                    params![session, role, status.as_str(), payload, now],
                )?;
                let seq: i64 = connection.query_row(
                    "SELECT seq FROM messages WHERE id = last_insert_rowid()",
                    [],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "UPDATE sessions SET updated_at = ?2, status = 'active' WHERE id = ?1",
                    params![session, now],
                )?;
                Ok(seq)
            })();
            match inserted {
                Ok(seq) => {
                    connection.execute_batch("COMMIT")?;
                    Ok(seq)
                }
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        })
    }

    /// The single retry table: 3 attempts, 100/200 ms waits, one deadline
    /// measured from `Instant::now()`, `busy_timeout` capped by what is left.
    fn write<T>(
        &self,
        mut operation: impl FnMut(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, SessionError> {
        let deadline = Instant::now() + WRITE_BUDGET;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(100 << (attempt - 1)));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let busy_timeout = remaining.as_millis().min(BUSY_TIMEOUT_CAP_MS as u128) as u64;
            self.connection
                .pragma_update(None, "busy_timeout", busy_timeout)?;
            match operation(&self.connection) {
                Ok(value) => return Ok(value),
                Err(error) if is_busy(&error) => {
                    tracing::warn!(attempt, %error, "session store is busy");
                }
                Err(error) => return Err(SessionError::from(error)),
            }
        }
        Err(SessionError::Busy)
    }
}

fn role_name(role: &ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// State of the last turn: an unanswered user row or tool call is `pending`;
/// recovery evidence outranks the per-status answer.
fn derive_turn_state(rows: &[StoredMessage]) -> TurnState {
    let Some(start) = rows.iter().rposition(|row| row.role == "user") else {
        return TurnState::Idle;
    };
    let turn = &rows[start..];
    if turn.len() == 1 {
        return TurnState::Pending;
    }
    let Some(last_assistant) = turn.iter().rposition(|row| row.role == "assistant") else {
        return TurnState::Idle;
    };
    if turn[last_assistant..]
        .iter()
        .any(|row| row.status == MessageStatus::Interrupted)
    {
        return TurnState::Interrupted;
    }

    let answered = call_ids(&turn[last_assistant].payload)
        .iter()
        .all(|call_id| {
            turn[last_assistant + 1..].iter().any(|row| {
                row.role == "tool" && response_call_id(&row.payload).as_deref() == Some(call_id)
            })
        });
    if answered {
        TurnState::Idle
    } else {
        TurnState::Pending
    }
}

/// Tool-call ids of a payload; an unreadable payload has no calls.
fn call_ids(payload: &str) -> Vec<String> {
    serde_json::from_str::<ChatMessage>(payload)
        .map(|message| {
            message
                .content
                .tool_calls()
                .iter()
                .map(|call| call.call_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The `call_id` a tool-result payload answers.
fn response_call_id(payload: &str) -> Option<String> {
    serde_json::from_str::<ChatMessage>(payload)
        .ok()?
        .content
        .tool_responses()
        .first()
        .map(|response| response.call_id.clone())
}

fn create_owner_only(path: &Path, mode: u32) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::create_dir_all(path)?;
    } else if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tighten, never refuse: a legacy 0755 directory is repaired.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/core/session.rs"]
mod tests;
