//! Versioned SQLite session store.
//!
//! One database per workspace: `<cwd>/.seavx/sessions.db` in debug mode, else
//! `<home>/.seavx/sessions.db` — the same rule `MEMORY.md` follows. The file is
//! `0600`, its directory `0700`, WAL and `synchronous=NORMAL` give the write
//! budget without holding a lock across a turn.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use genai::chat::{ChatMessage, ChatRole};
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
