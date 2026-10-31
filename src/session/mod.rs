//! Versioned SQLite session store.
//!
//! One database per workspace: `<cwd>/.seavx/sessions.db` in debug mode, else
//! `<home>/.seavx/sessions.db` — the same rule `MEMORY.md` follows. The file is
//! `0600`, its directory `0700`, WAL and `synchronous=NORMAL` give the write
//! budget without holding a lock across a turn.

use std::path::{Path, PathBuf};
use std::time::Duration;

use genai::chat::{BinarySource, ChatMessage, ChatRole, ContentPart, ToolResponse};
use rusqlite::{Connection, OptionalExtension, params};

mod attachment;
mod goals;
mod questions;
mod store;

pub use attachment::{Attachment, PasteSource, size_label, user_message};

/// Serde format of a message payload. Bumped only if `genai` changes its
/// `ChatMessage` shape; a mismatch opens the session read-only.
pub const PAYLOAD_FORMAT: &str = "genai-chat-v1";
/// Schema version this build understands. 2 added the `slug` column; `migrate`
/// adds it to a v1 file and names every existing row. 3 added the `agent_events`
/// ledger, which every older file gains in the same transaction. 4 added goals,
/// criteria and tasks, and rebuilt that ledger so a goal can own its events.
/// 5 added resumable clarification questions and the `question` event kind.
/// 6 added fork/archive metadata: `parent_session_id`, `fork_seq`, `archived_at`.
pub const SCHEMA_VERSION: u32 = 6;

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
    /// The `svx-…` name the user types; `id` stays the key the tables join on.
    pub slug: String,
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

/// What kind of answer a question takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    Single,
    Multi,
    Text,
}

impl QuestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
            Self::Text => "text",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "multi" => Self::Multi,
            "text" => Self::Text,
            _ => Self::Single,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionState {
    Pending,
    Answered,
    Skipped,
    Cancelled,
}

impl QuestionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "answered" => Self::Answered,
            "skipped" => Self::Skipped,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

/// One durable clarification: what was asked, of whom, and what came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingQuestion {
    pub id: i64,
    pub workspace: String,
    pub session_id: Option<i64>,
    pub turn_id: u64,
    pub tool_call_id: String,
    pub question: String,
    pub kind: QuestionKind,
    pub options: Vec<String>,
    pub state: QuestionState,
    pub answer: String,
}

/// Total bytes a question and its option labels may occupy.
pub const QUESTION_LIMIT: usize = 4096;

/// A goal's place in its lifecycle. Only the user moves it to `active`; only
/// evidence moves it to `completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalState {
    Draft,
    Active,
    Blocked,
    Paused,
    Completed,
    Cancelled,
}

impl GoalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "active" => Self::Active,
            "blocked" => Self::Blocked,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Draft,
        }
    }

    /// Whether `next` is a legal move from this state.
    fn allows(self, next: Self) -> bool {
        match self {
            Self::Draft => matches!(next, Self::Active | Self::Cancelled),
            Self::Active => matches!(
                next,
                Self::Blocked | Self::Paused | Self::Completed | Self::Cancelled
            ),
            Self::Blocked => matches!(next, Self::Active | Self::Paused | Self::Cancelled),
            Self::Paused => matches!(next, Self::Active | Self::Cancelled),
            Self::Completed | Self::Cancelled => false,
        }
    }

    /// A goal holding the workspace turn; at most one may.
    fn is_live(self) -> bool {
        matches!(self, Self::Active | Self::Blocked)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running,
    Blocked,
    Completed,
    Cancelled,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "blocked" => Self::Blocked,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriterionState {
    Pending,
    Satisfied,
    Waived,
}

impl CriterionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Satisfied => "satisfied",
            Self::Waived => "waived",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "satisfied" => Self::Satisfied,
            "waived" => Self::Waived,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub position: i64,
    pub text: String,
    pub state: CriterionState,
    /// Bounded, redacted reference: a command and its status, a path and a line.
    pub evidence: String,
    /// Required when the user waives the criterion.
    pub waiver_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalTask {
    pub position: i64,
    pub title: String,
    pub state: TaskState,
    pub evidence: String,
}

/// A goal with its criteria and ordered tasks, as the store holds it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub id: i64,
    pub workspace: String,
    pub session_id: Option<i64>,
    pub objective: String,
    pub state: GoalState,
    pub criteria: Vec<Criterion>,
    pub tasks: Vec<GoalTask>,
}

impl Goal {
    /// Completion needs every criterion satisfied with evidence, or waived by
    /// the user with a reason. The model cannot assert this.
    pub fn completion_blocker(&self) -> Option<String> {
        for criterion in &self.criteria {
            match criterion.state {
                CriterionState::Satisfied if criterion.evidence.trim().is_empty() => {
                    return Some(format!(
                        "criterion {} is satisfied without evidence",
                        criterion.position + 1
                    ));
                }
                CriterionState::Waived
                    if criterion
                        .waiver_reason
                        .as_deref()
                        .is_none_or(|reason| reason.trim().is_empty()) =>
                {
                    return Some(format!(
                        "criterion {} is waived without a reason",
                        criterion.position + 1
                    ));
                }
                CriterionState::Pending => {
                    return Some(format!(
                        "criterion {} is still pending",
                        criterion.position + 1
                    ));
                }
                _ => {}
            }
        }
        // Tasks are the ordered plan, not the gate: the spec ties completion to
        // criteria, which carry evidence or a user waiver.
        None
    }
}

fn question_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingQuestion> {
    let options: String = row.get(7)?;
    Ok(PendingQuestion {
        id: row.get(0)?,
        workspace: row.get(1)?,
        session_id: row.get(2)?,
        turn_id: row.get::<_, i64>(3)? as u64,
        tool_call_id: row.get(4)?,
        question: row.get(5)?,
        kind: QuestionKind::parse(&row.get::<_, String>(6)?),
        options: serde_json::from_str(&options).unwrap_or_default(),
        state: QuestionState::parse(&row.get::<_, String>(8)?),
        answer: row.get(9)?,
    })
}

/// A question event shares the session's sequence when it has one.
fn log_question_event(
    connection: &Connection,
    session: Option<i64>,
    question: i64,
    status: &str,
    tool_call_id: &str,
    summary: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let seq: i64 = match session {
        Some(session) => connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_events WHERE session_id = ?1",
            [session],
            |row| row.get(0),
        )?,
        None => connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_events WHERE session_id IS NULL",
            [],
            |row| row.get(0),
        )?,
    };
    connection.execute(
        "INSERT INTO agent_events(session_id, goal_id, seq, turn_id, kind, status, tool_call_id, summary, created_at)
         VALUES (?1, NULL, ?2, 0, 'question', ?3, ?4, ?5, ?6)",
        params![session, seq, status, tool_call_id, summary, now],
    )?;
    let _ = question;
    Ok(())
}

/// The goal's state, after proving it belongs to this workspace.
fn goal_state(connection: &Connection, workspace: &str, id: i64) -> rusqlite::Result<GoalState> {
    let state: Option<String> = connection
        .query_row(
            "SELECT status FROM goals WHERE id = ?1 AND workspace = ?2",
            params![id, workspace],
            |row| row.get(0),
        )
        .optional()?;
    match state {
        Some(state) => Ok(GoalState::parse(&state)),
        None => Err(rusqlite::Error::QueryReturnedNoRows),
    }
}

/// The other goal of this workspace that holds the turn, if one does.
fn conflicting_goal(
    connection: &Connection,
    workspace: &str,
    id: i64,
) -> rusqlite::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT id FROM goals
             WHERE workspace = ?1 AND id <> ?2 AND status IN ('active','blocked')",
            params![workspace, id],
            |row| row.get(0),
        )
        .optional()
}

/// Append one goal-scoped lifecycle event, in the caller's transaction.
fn log_goal_event(
    connection: &Connection,
    goal: i64,
    session: Option<i64>,
    kind: &str,
    status: &str,
    summary: &str,
    now: i64,
) -> rusqlite::Result<()> {
    // One sequence per scope: a session's goal events join that session's
    // sequence, an unattended goal numbers its own.
    let seq: i64 = match session {
        Some(session) => connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_events WHERE session_id = ?1",
            [session],
            |row| row.get(0),
        )?,
        None => connection.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_events WHERE goal_id = ?1",
            [goal],
            |row| row.get(0),
        )?,
    };
    connection.execute(
        "INSERT INTO agent_events(session_id, goal_id, seq, turn_id, kind, status, summary, created_at)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7)",
        params![session, goal, seq, kind, status, summary, now],
    )?;
    Ok(())
}

/// Commit the caller's transaction, or roll it back and keep the first error.
fn finish<T, E>(connection: &Connection, outcome: Result<T, E>) -> rusqlite::Result<Result<T, E>> {
    match outcome {
        Ok(value) => {
            connection.execute_batch("COMMIT")?;
            Ok(Ok(value))
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Ok(Err(error))
        }
    }
}

/// The task text one approved goal becomes: the objective, its criteria and the
/// ordered tasks, so a single Build turn works through the plan.
pub fn goal_task_text(goal: &Goal) -> String {
    let mut text = format!(
        "Goal {}: {}\n\nWork through these tasks in order:\n",
        goal.id, goal.objective
    );
    for task in &goal.tasks {
        text.push_str(&format!("{}. {}\n", task.position + 1, task.title));
    }
    if !goal.criteria.is_empty() {
        text.push_str("\nDone when:\n");
        for criterion in &goal.criteria {
            text.push_str(&format!("- {}\n", criterion.text));
        }
    }
    text.push_str("\nUse the tools as needed. Report what you changed and what you verified; do not claim a test you did not run.");
    text
}

/// One row of the execution trace, before it is stored. Only the lifecycle
/// carries durable text: the summary is redacted and bounded here, at the trust
/// boundary, so no caller can write raw arguments or output through it.
pub struct NewEvent<'a> {
    pub turn_id: u64,
    /// `"tool"` or `"turn"`.
    pub kind: &'a str,
    pub status: &'a str,
    pub failure: Option<&'a str>,
    pub tool_call_id: Option<&'a str>,
    pub summary: &'a str,
    /// Only set on a turn row whose process-tree cleanup was not confirmed.
    pub pid: Option<u32>,
}

impl<'a> NewEvent<'a> {
    /// `None` for the presentation events: a streamed token is not a trace.
    pub fn from_event(turn_id: u64, event: &'a crate::agent::events::AgentEvent) -> Option<Self> {
        use crate::agent::events::AgentEvent;
        match event {
            AgentEvent::Tool(tool) => Some(Self {
                turn_id,
                kind: "tool",
                status: tool.status.as_str(),
                failure: tool.failure.map(|failure| failure.as_str()),
                tool_call_id: Some(&tool.call_id),
                summary: &tool.summary,
                pid: None,
            }),
            AgentEvent::Turn {
                status,
                failure,
                pid,
                ..
            } => Some(Self {
                turn_id,
                kind: "turn",
                status: status.as_str(),
                failure: failure.map(|failure| failure.as_str()),
                tool_call_id: None,
                summary: "",
                pid: *pid,
            }),
            AgentEvent::Token(_)
            | AgentEvent::Thinking(_)
            | AgentEvent::Notice(_)
            | AgentEvent::Steer(_)
            | AgentEvent::Usage { .. } => None,
        }
    }
}

/// One stored trace row. `seq` is the order, never the wall clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub seq: i64,
    pub turn_id: u64,
    pub kind: String,
    pub status: String,
    pub failure: Option<String>,
    pub tool_call_id: Option<String>,
    pub summary: String,
    pub pid: Option<u32>,
    pub created_at: i64,
}

/// A session read back for replay, rendering or export.
#[derive(Debug, Clone)]
pub struct LoadedSession {
    pub id: i64,
    /// The `svx-…` name this session is addressed by.
    pub slug: String,
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
    /// Calls still waiting on a human answer: replay must not repair them.
    pub pending_calls: Vec<String>,
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
            // A question still waiting on the user is not an interruption.
            .filter(|call_id| !session.pending_calls.contains(call_id))
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
    attachments: &[Attachment],
) -> Result<(i64, Vec<ChatMessage>), SessionError> {
    // An image-only turn still needs a title the sidebar can show.
    let title = match attachments.first() {
        Some(attachment) if prompt.trim().is_empty() => attachment.name.clone(),
        _ => title_from(prompt),
    };
    let id = match session {
        Some(id) => id,
        None => store.create(workspace, Some(model), Some(&title))?,
    };
    store.record_message(
        id,
        &user_message(prompt, attachments),
        MessageStatus::Complete,
    )?;
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

/// A short, human-typed session id: `svx-` and eight lowercase hex digits. It
/// is random, not sequential, so it says nothing about how many sessions exist.
fn new_slug() -> String {
    use std::hash::{BuildHasher, Hasher};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    format!("svx-{:08x}", hasher.finish() as u32)
}

/// The one `INSERT` behind `create`; kept out of the closure so a slug
/// collision can be retried without repeating the statement.
/// `LIKE` treats `%` and `_` as patterns; a search for them is literal.
fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn insert_session(
    connection: &Connection,
    slug: &str,
    workspace: &str,
    title: Option<&str>,
    model: Option<&str>,
    now: i64,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO sessions(slug, workspace, title, model, payload_format, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
        params![slug, workspace, title, model, PAYLOAD_FORMAT, now],
    )
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
        // The same words the live transcript uses: a verb and its object.
        let detail = crate::agent::tools::describe_call(&call.fn_name, &call.fn_arguments);
        out.push_str(&format!(
            "⚡ {}\n\n",
            crate::agent::tools::call_label(&call.fn_name, &detail)
        ));
    }
    for response in message.content.tool_responses() {
        out.push_str(&format!("{}\n\n", response.content.trim()));
    }
    // A resumed transcript labels an image with the same words the live one
    // does, read back from the bytes: the row carries no attachment table.
    let images = message
        .content
        .parts()
        .iter()
        .filter_map(|part| match part {
            ContentPart::Binary(binary) => Some(binary),
            _ => None,
        });
    for (index, binary) in images.enumerate() {
        let payload = match &binary.source {
            BinarySource::Base64(text) => text.to_string(),
            BinarySource::Url(url) => url.clone(),
        };
        out.push_str(&format!(
            "[{}]\n\n",
            attachment::stored_summary(
                index + 1,
                binary.name.as_deref().unwrap_or("image"),
                &binary.content_type,
                &payload
            )
        ));
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
    slug TEXT NOT NULL DEFAULT '',
    workspace TEXT NOT NULL,
    title TEXT,
    model TEXT,
    payload_format TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','ended')),
    total_tokens INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    -- v6: where a fork came from, and when this session was archived.
    parent_session_id INTEGER,
    fork_seq INTEGER,
    archived_at INTEGER
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
CREATE TABLE IF NOT EXISTS agent_events(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- A goal event has no session: the goal is the scope then.
    session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
    goal_id INTEGER REFERENCES goals(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    turn_id INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('tool','turn','goal','task','question')),
    status TEXT NOT NULL,
    failure TEXT,
    tool_call_id TEXT,
    summary TEXT NOT NULL DEFAULT '',
    pid INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);
CREATE INDEX IF NOT EXISTS agent_events_session_seq ON agent_events(session_id, seq);
CREATE TABLE IF NOT EXISTS goals(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace TEXT NOT NULL,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    objective TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK(status IN ('draft','active','blocked','paused','completed','cancelled')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS goals_workspace ON goals(workspace, updated_at DESC);
-- At most one goal may hold the turn: active, or blocked waiting on the user.
CREATE UNIQUE INDEX IF NOT EXISTS goals_one_live
    ON goals(workspace) WHERE status IN ('active','blocked');
CREATE TABLE IF NOT EXISTS goal_criteria(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    goal_id INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','satisfied','waived')),
    evidence TEXT NOT NULL DEFAULT '',
    waiver_reason TEXT,
    UNIQUE(goal_id, position)
);
CREATE TABLE IF NOT EXISTS pending_questions(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace TEXT NOT NULL,
    session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id INTEGER NOT NULL,
    tool_call_id TEXT NOT NULL,
    question TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('single','multi','text')),
    options TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL CHECK(state IN ('pending','answered','skipped','cancelled')),
    answer TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    resolved_at INTEGER,
    UNIQUE(workspace, session_id, turn_id, tool_call_id)
);
CREATE INDEX IF NOT EXISTS pending_questions_open
    ON pending_questions(workspace, state);
CREATE TABLE IF NOT EXISTS agent_tasks(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    goal_id INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK(status IN ('pending','running','blocked','completed','cancelled')),
    evidence TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL,
    UNIQUE(goal_id, position)
);
";

#[derive(Debug)]
pub enum SessionError {
    /// The file was written by a newer build: refuse rather than downgrade.
    Newer { found: u32, supported: u32 },
    /// Cannot be opened, read or migrated; the run continues unpersisted.
    Unavailable(String),
    /// No session with this id.
    Unknown(i64),
    /// No session with this slug or numeric id.
    UnknownKey(String),
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
            SessionError::UnknownKey(key) => write!(f, "unknown session {key}"),
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

/// The WAL and its shared-memory index are created by SQLite, not by this
/// module, so their mode follows the umask instead of the database's `0600`.
#[cfg(unix)]
fn tighten_sidecars(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        if sidecar.exists() {
            let _ = std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(not(unix))]
fn tighten_sidecars(_path: &Path) {}

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

#[cfg(test)]
#[path = "../tests/core/session.rs"]
mod tests;
