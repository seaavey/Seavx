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
            crate::util::ensure_private_dir(directory, 0o700)
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

        crate::util::ensure_private_file(path, 0o600)
            .map_err(|error| SessionError::Unavailable(error.to_string()))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=2000;",
        )?;
        // SQLite creates the WAL and its index itself, with the umask's mode.
        // They carry the same payloads as the database, so an existing one is
        // made owner-only, before and after the migration writes through it.
        tighten_sidecars(path);

        let store = Self { connection };
        store.migrate()?;
        tighten_sidecars(path);
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

        // A v1 file already has the table, so `CREATE TABLE IF NOT EXISTS`
        // could not add the column: alter it, then name every existing row.
        let mut columns = transaction.prepare("PRAGMA table_info(sessions)")?;
        let names: Vec<String> = columns
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        drop(columns);
        if !names.iter().any(|name| name == "slug") {
            transaction
                .execute_batch("ALTER TABLE sessions ADD COLUMN slug TEXT NOT NULL DEFAULT ''")?;
        }
        // v6 columns: nullable, so every existing row keeps its meaning.
        for (column, sql) in [
            (
                "parent_session_id",
                "ALTER TABLE sessions ADD COLUMN parent_session_id INTEGER",
            ),
            (
                "fork_seq",
                "ALTER TABLE sessions ADD COLUMN fork_seq INTEGER",
            ),
            (
                "archived_at",
                "ALTER TABLE sessions ADD COLUMN archived_at INTEGER",
            ),
        ] {
            if !names.iter().any(|name| name == column) {
                transaction.execute_batch(sql)?;
            }
        }
        // After the column, never in `SCHEMA`: an index on a column the v1
        // table does not have yet would fail the whole batch.
        transaction.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS sessions_slug ON sessions(slug) WHERE slug <> ''",
        )?;

        // v3's ledger has no `goal_id` and a `kind` CHECK that refused goal
        // events. SQLite cannot alter either in place, so the table is rebuilt
        // row for row inside this transaction, ids and sequence intact.
        let mut ledger_columns = transaction.prepare("PRAGMA table_info(agent_events)")?;
        let ledger_names: Vec<String> = ledger_columns
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        drop(ledger_columns);
        // v5 admits a `question` event; the CHECK is part of the table, so the
        // rebuild runs again for a v4 file.
        let ledger_ddl: String = transaction.query_row(
            "SELECT COALESCE((SELECT sql FROM sqlite_master WHERE name = 'agent_events'), '')",
            [],
            |row| row.get(0),
        )?;
        let needs_rebuild = !ledger_names.iter().any(|name| name == "goal_id")
            || !ledger_ddl.contains("'question'");
        if needs_rebuild {
            transaction.execute_batch(
                "ALTER TABLE agent_events RENAME TO agent_events_v3;
                 CREATE TABLE agent_events(
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
                     goal_id INTEGER REFERENCES goals(id) ON DELETE CASCADE,
                     seq INTEGER NOT NULL,
                     turn_id INTEGER NOT NULL,
                     kind TEXT NOT NULL
                         CHECK(kind IN ('tool','turn','goal','task','question')),
                     status TEXT NOT NULL,
                     failure TEXT,
                     tool_call_id TEXT,
                     summary TEXT NOT NULL DEFAULT '',
                     pid INTEGER,
                     created_at INTEGER NOT NULL,
                     UNIQUE(session_id, seq)
                 );
                 INSERT INTO agent_events(id, session_id, goal_id, seq, turn_id, kind,
                                          status, failure, tool_call_id, summary, pid, created_at)
                     SELECT id, session_id, NULL, seq, turn_id, kind, status, failure,
                            tool_call_id, summary, pid, created_at
                     FROM agent_events_v3;
                 DROP TABLE agent_events_v3;
                 CREATE INDEX IF NOT EXISTS agent_events_session_seq
                     ON agent_events(session_id, seq);
                 CREATE UNIQUE INDEX IF NOT EXISTS agent_events_goal_seq
                     ON agent_events(goal_id, seq) WHERE goal_id IS NOT NULL;",
            )?;
        }
        // After the rebuild, never in `SCHEMA`: on a v3 file the column does not
        // exist yet, and the whole batch would fail on the index alone.
        transaction.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS agent_events_goal_seq
                 ON agent_events(goal_id, seq) WHERE goal_id IS NOT NULL;",
        )?;

        let mut unnamed = transaction.prepare("SELECT id FROM sessions WHERE slug = ''")?;
        let pending: Vec<i64> = unnamed
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(unnamed);
        for id in pending {
            transaction.execute(
                "UPDATE sessions SET slug = ?1 WHERE id = ?2",
                params![new_slug(), id],
            )?;
        }

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
            // The unique index turns a collision into an error instead of two
            // sessions sharing a name; one retry with a fresh slug is enough.
            let mut slug = new_slug();
            let mut inserted = insert_session(connection, &slug, workspace, title, model, now);
            if let Err(rusqlite::Error::SqliteFailure(error, _)) = &inserted
                && error.code == rusqlite::ErrorCode::ConstraintViolation
            {
                slug = new_slug();
                inserted = insert_session(connection, &slug, workspace, title, model, now);
            }
            inserted?;
            Ok(connection.last_insert_rowid())
        })
    }

    // ---- fork, archive, search --------------------------------------------

    /// Copy the source session's prefix into a new independent session.
    ///
    /// `boundary` is the last message `seq` to copy; `None` means the newest
    /// complete turn. The source is only read: its rows, slug and timestamps are
    /// untouched, and a refusal happens before anything is written.
    pub fn fork(
        &self,
        workspace: &str,
        source: i64,
        boundary: Option<i64>,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let forked = (|| {
                let (owner, title, model): (String, Option<String>, Option<String>) = connection
                    .query_row(
                        "SELECT workspace, title, model FROM sessions WHERE id = ?1",
                        [source],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or(SessionError::Unknown(source))?;
                if owner != workspace {
                    return Err(SessionError::Unknown(source));
                }

                let boundary = match boundary {
                    Some(seq) => seq,
                    // Default: the newest finished turn. A session whose last
                    // row is an unanswered prompt still has one to fork.
                    None => connection
                        .query_row(
                            "SELECT MAX(seq) FROM messages
                             WHERE session_id = ?1 AND role = 'assistant' AND status = 'complete'",
                            [source],
                            |row| row.get::<_, Option<i64>>(0),
                        )
                        .optional()?
                        .flatten()
                        .ok_or_else(|| {
                            SessionError::Refused(format!(
                                "session {source} has no finished turn to fork"
                            ))
                        })?,
                };
                let (role, status): (String, String) = connection
                    .query_row(
                        "SELECT role, status FROM messages WHERE session_id = ?1 AND seq = ?2",
                        params![source, boundary],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        SessionError::Refused(format!(
                            "session {source} has no message {boundary}"
                        ))
                    })?;
                if role != "assistant" || status != "complete" {
                    return Err(SessionError::Refused(format!(
                        "message {boundary} is not a finished turn; fork only at the end of a completed turn"
                    )));
                }
                // The row after the boundary may only be the next turn's prompt.
                // Anything else means the fork would end inside a turn.
                let next_role: Option<String> = connection
                    .query_row(
                        "SELECT role FROM messages WHERE session_id = ?1 AND seq = ?2",
                        params![source, boundary + 1],
                        |row| row.get(0),
                    )
                    .optional()?;
                if next_role.as_deref().is_some_and(|role| role != "user") {
                    return Err(SessionError::Refused(format!(
                        "message {boundary} is inside a turn; fork only at the end of a completed turn"
                    )));
                }

                let mut slug = new_slug();
                let mut row = connection.execute(
                    "INSERT INTO sessions(slug, workspace, title, model, payload_format, status,
                                         created_at, updated_at, parent_session_id, fork_seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7, ?8)",
                    params![slug, workspace, title, model, PAYLOAD_FORMAT, now, source, boundary],
                );
                if let Err(rusqlite::Error::SqliteFailure(error, _)) = &row
                    && error.code == rusqlite::ErrorCode::ConstraintViolation
                {
                    slug = new_slug();
                    row = connection.execute(
                        "INSERT INTO sessions(slug, workspace, title, model, payload_format, status,
                                             created_at, updated_at, parent_session_id, fork_seq)
                         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7, ?8)",
                        params![slug, workspace, title, model, PAYLOAD_FORMAT, now, source, boundary],
                    );
                }
                row?;
                let copy = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO messages(session_id, seq, role, payload, status, created_at)
                     SELECT ?1, seq, role, payload, status, created_at
                     FROM messages WHERE session_id = ?2 AND seq <= ?3 ORDER BY seq",
                    params![copy, source, boundary],
                )?;
                Ok(copy)
            })();
            finish(connection, forked)
        })?
    }

    /// Hide a session from the default list without deleting a single row.
    pub fn archive(&self, workspace: &str, id: i64, archived: bool) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let written = (|| {
                let owner: String = connection
                    .query_row(
                        "SELECT workspace FROM sessions WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(SessionError::Unknown(id))?;
                if owner != workspace {
                    return Err(SessionError::Unknown(id));
                }
                connection.execute(
                    "UPDATE sessions SET archived_at = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, archived.then_some(now), now],
                )?;
                Ok(())
            })();
            finish(connection, written)
        })?
    }

    /// The session rows this workspace has, newest first. Archived sessions are
    /// left out unless `include_archived` asks for them.
    pub fn list_with_archived(
        &self,
        workspace: &str,
        include_archived: bool,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.slug, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1 AND (?2 OR s.archived_at IS NULL)
             GROUP BY s.id
             HAVING COUNT(m.id) > 0
             ORDER BY s.updated_at DESC, s.id DESC",
        )?;
        let rows: Vec<(i64, String, Option<String>, i64, i64)> = statement
            .query_map(params![workspace, include_archived], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(statement);
        Ok(rows
            .into_iter()
            .map(|(id, slug, title, updated_at, messages)| SessionSummary {
                id,
                slug,
                title: title.unwrap_or_else(|| format!("session {id}")),
                updated_at,
                messages: messages as usize,
                state: TurnState::Idle,
            })
            .collect())
    }

    /// Literal title and message-content search inside one workspace. The query
    /// is bound as a value, never spliced into SQL, and the result is capped.
    pub fn search(
        &self,
        workspace: &str,
        query: &str,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{}%", escape_like(query));
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.slug, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1 AND (?2 OR s.archived_at IS NULL)
               AND (COALESCE(s.title, '') LIKE ?3 ESCAPE '\\'
                    OR m.payload LIKE ?3 ESCAPE '\\')
             GROUP BY s.id
             ORDER BY s.updated_at DESC, s.id DESC
             LIMIT ?4",
        )?;
        let rows: Vec<(i64, String, Option<String>, i64, i64)> = statement
            .query_map(
                params![workspace, include_archived, pattern, limit as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, slug, title, updated_at, messages)| SessionSummary {
                id,
                slug,
                title: title.unwrap_or_else(|| format!("session {id}")),
                updated_at,
                messages: messages as usize,
                state: TurnState::Idle,
            })
            .collect())
    }

    /// Resolve the id a user typed: the `svx-…` slug, or a bare number from a
    /// build that had no slugs.
    pub fn id_for(&self, key: &str) -> Result<i64, SessionError> {
        let key = key.trim();
        let by_slug = self
            .connection
            .query_row("SELECT id FROM sessions WHERE slug = ?1", [key], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        if let Some(id) = by_slug {
            return Ok(id);
        }

        if let Ok(id) = key.parse::<i64>() {
            let exists = self
                .connection
                .query_row("SELECT 1 FROM sessions WHERE id = ?1", [id], |_| Ok(()))
                .optional()?;
            if exists.is_some() {
                return Ok(id);
            }
        }

        Err(SessionError::UnknownKey(key.to_string()))
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
            "SELECT s.id, s.slug, s.title, s.updated_at, COUNT(m.id)
             FROM sessions s JOIN messages m ON m.session_id = s.id
             WHERE s.workspace = ?1 AND s.archived_at IS NULL
             GROUP BY s.id
             HAVING COUNT(m.id) > 0
             ORDER BY s.updated_at DESC, s.id DESC",
        )?;
        let rows: Vec<(i64, String, Option<String>, i64, i64)> = statement
            .query_map([workspace], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(statement);

        let mut summaries = Vec::with_capacity(rows.len());
        for (id, slug, title, updated_at, messages) in rows {
            summaries.push(SessionSummary {
                id,
                slug,
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
                "SELECT slug, workspace, title, model, payload_format, total_tokens, prompt_tokens
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;

        let messages = self.rows(id)?;
        let mut read_only = (header.4 != PAYLOAD_FORMAT).then(|| {
            format!(
                "payload format {} does not match {}",
                header.4, PAYLOAD_FORMAT
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
            slug: header.0,
            workspace: header.1,
            title: header.2,
            model: header.3,
            payload_format: header.4,
            read_only,
            messages,
            total_tokens: header.5,
            prompt_tokens: header.6,
            pending_calls: self.pending_call_ids(id).unwrap_or_default(),
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

    // ---- clarifying questions ---------------------------------------------

    /// Persist a pending question and its `question_asked` event together, before
    /// anyone is shown it.
    // The argument list is the question's own shape; a struct with one caller
    // would only rename the same fields.
    #[allow(clippy::too_many_arguments)]
    pub fn ask_question(
        &self,
        workspace: &str,
        session: Option<i64>,
        turn_id: u64,
        tool_call_id: &str,
        question: &str,
        kind: QuestionKind,
        options: &[String],
    ) -> Result<i64, SessionError> {
        let text = crate::agent::security::redact(question);
        let labels: Vec<String> = options
            .iter()
            .map(|option| crate::agent::security::redact(option))
            .collect();
        let total = text.len() + labels.iter().map(String::len).sum::<usize>();
        if total > QUESTION_LIMIT {
            return Err(SessionError::Refused(format!(
                "the question and its options exceed {QUESTION_LIMIT} bytes"
            )));
        }
        let options_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let created = (|| {
                connection.execute(
                    "INSERT INTO pending_questions
                     (workspace, session_id, turn_id, tool_call_id, question, kind, options, state, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
                    params![
                        workspace,
                        session,
                        turn_id as i64,
                        tool_call_id,
                        text,
                        kind.as_str(),
                        options_json,
                        now
                    ],
                )?;
                let id = connection.last_insert_rowid();
                log_question_event(connection, session, id, "pending", tool_call_id, &text, now)?;
                Ok(id)
            })();
            finish(connection, created)
        })?
    }

    pub fn question(&self, workspace: &str, id: i64) -> Result<PendingQuestion, SessionError> {
        let question = self
            .connection
            .query_row(
                "SELECT id, workspace, session_id, turn_id, tool_call_id, question, kind,
                        options, state, answer
                 FROM pending_questions WHERE id = ?1 AND workspace = ?2",
                params![id, workspace],
                question_row,
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;
        Ok(question)
    }

    /// Every unanswered question of this workspace, oldest first.
    pub fn open_questions(&self, workspace: &str) -> Result<Vec<PendingQuestion>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT id, workspace, session_id, turn_id, tool_call_id, question, kind,
                    options, state, answer
             FROM pending_questions WHERE workspace = ?1 AND state = 'pending'
             ORDER BY id",
        )?;
        let rows = statement.query_map([workspace], question_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Resolve once. A second call returns the stored outcome and writes nothing,
    /// so a retried turn cannot append a second decision.
    pub fn resolve_question(
        &self,
        workspace: &str,
        id: i64,
        state: QuestionState,
        answer: &str,
    ) -> Result<PendingQuestion, SessionError> {
        if state == QuestionState::Pending {
            return Err(SessionError::Refused(
                "a question cannot resolve to pending".to_string(),
            ));
        }
        let answer = crate::agent::events::bounded(
            &crate::agent::security::redact(answer),
            crate::agent::events::SUMMARY_LIMIT,
        );
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let resolved = (|| {
                let current: Option<String> = connection
                    .query_row(
                        "SELECT state FROM pending_questions WHERE id = ?1 AND workspace = ?2",
                        params![id, workspace],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(current) = current else {
                    return Err(SessionError::Unknown(id));
                };
                if current != "pending" {
                    // Already resolved: the stored outcome stands, no new event.
                    return Ok(());
                }
                connection.execute(
                    "UPDATE pending_questions SET state = ?3, answer = ?4, resolved_at = ?5
                     WHERE id = ?1 AND workspace = ?2",
                    params![id, workspace, state.as_str(), answer, now],
                )?;
                let session: Option<i64> = connection.query_row(
                    "SELECT session_id FROM pending_questions WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )?;
                let call: String = connection.query_row(
                    "SELECT tool_call_id FROM pending_questions WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )?;
                log_question_event(connection, session, id, state.as_str(), &call, &answer, now)?;
                Ok(())
            })();
            finish(connection, resolved)
        })??;
        self.question(workspace, id)
    }

    /// Calls of this session whose question is still open: replay must not
    /// invent an interrupted result for them.
    pub fn pending_call_ids(&self, session: i64) -> Result<Vec<String>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT tool_call_id FROM pending_questions
             WHERE session_id = ?1 AND state = 'pending'",
        )?;
        let rows = statement.query_map([session], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---- goals -------------------------------------------------------------

    /// A new goal is a `draft`: nothing runs until the user approves it.
    pub fn create_goal(
        &self,
        workspace: &str,
        session: Option<i64>,
        objective: &str,
    ) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let objective = crate::agent::events::bounded(
            &crate::agent::security::redact(objective),
            crate::agent::events::SUMMARY_LIMIT,
        );
        if objective.trim().is_empty() {
            return Err(SessionError::Refused(
                "a goal needs an objective".to_string(),
            ));
        }
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let created = (|| {
                connection.execute(
                    "INSERT INTO goals(workspace, session_id, objective, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'draft', ?4, ?4)",
                    params![workspace, session, objective, now],
                )?;
                let id = connection.last_insert_rowid();
                log_goal_event(connection, id, session, "goal", "draft", &objective, now)?;
                Ok(id)
            })();
            finish(connection, created)
        })?
    }

    pub fn goal(&self, workspace: &str, id: i64) -> Result<Goal, SessionError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, workspace, session_id, objective, status FROM goals
                 WHERE id = ?1 AND workspace = ?2",
                params![id, workspace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(SessionError::Unknown(id))?;

        let mut criteria_stmt = self.connection.prepare(
            "SELECT position, text, status, evidence, waiver_reason FROM goal_criteria
             WHERE goal_id = ?1 ORDER BY position",
        )?;
        let criteria = criteria_stmt
            .query_map([id], |row| {
                Ok(Criterion {
                    position: row.get(0)?,
                    text: row.get(1)?,
                    state: CriterionState::parse(&row.get::<_, String>(2)?),
                    evidence: row.get(3)?,
                    waiver_reason: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut task_stmt = self.connection.prepare(
            "SELECT position, title, status, evidence FROM agent_tasks
             WHERE goal_id = ?1 ORDER BY position",
        )?;
        let tasks = task_stmt
            .query_map([id], |row| {
                Ok(GoalTask {
                    position: row.get(0)?,
                    title: row.get(1)?,
                    state: TaskState::parse(&row.get::<_, String>(2)?),
                    evidence: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Goal {
            id: row.0,
            workspace: row.1,
            session_id: row.2,
            objective: row.3,
            state: GoalState::parse(&row.4),
            criteria,
            tasks,
        })
    }

    /// Every goal in one workspace, newest first.
    pub fn goals(&self, workspace: &str) -> Result<Vec<Goal>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM goals WHERE workspace = ?1 ORDER BY updated_at DESC, id DESC",
        )?;
        let ids: Vec<i64> = statement
            .query_map([workspace], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(statement);
        ids.into_iter().map(|id| self.goal(workspace, id)).collect()
    }

    /// Replace a draft's objective. Only a draft is editable.
    pub fn set_goal_objective(
        &self,
        workspace: &str,
        id: i64,
        objective: &str,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let objective = crate::agent::events::bounded(
            &crate::agent::security::redact(objective),
            crate::agent::events::SUMMARY_LIMIT,
        );
        if objective.trim().is_empty() {
            return Err(SessionError::Refused(
                "a goal needs an objective".to_string(),
            ));
        }
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let written = (|| {
                let state = goal_state(connection, workspace, id)?;
                if state != GoalState::Draft {
                    return Err(SessionError::Refused(format!(
                        "goal {id} is {}; only a draft can be edited",
                        state.as_str()
                    )));
                }
                connection.execute(
                    "UPDATE goals SET objective = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, objective, now],
                )?;
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    "draft",
                    "objective edited",
                    now,
                )?;
                Ok(())
            })();
            finish(connection, written)
        })?
    }

    /// Replace the draft's criteria and ordered tasks. Only a draft is editable.
    pub fn set_goal_plan(
        &self,
        workspace: &str,
        id: i64,
        criteria: &[String],
        tasks: &[String],
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let written = (|| {
                let state = goal_state(connection, workspace, id)?;
                if state != GoalState::Draft {
                    return Err(SessionError::Refused(format!(
                        "goal {id} is {}; only a draft can be edited",
                        state.as_str()
                    )));
                }
                connection.execute("DELETE FROM goal_criteria WHERE goal_id = ?1", [id])?;
                connection.execute("DELETE FROM agent_tasks WHERE goal_id = ?1", [id])?;
                for (position, text) in criteria.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO goal_criteria(goal_id, position, text, status)
                         VALUES (?1, ?2, ?3, 'pending')",
                        params![id, position as i64, text.trim()],
                    )?;
                }
                for (position, title) in tasks.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO agent_tasks(goal_id, position, title, status, updated_at)
                         VALUES (?1, ?2, ?3, 'pending', ?4)",
                        params![id, position as i64, title.trim(), now],
                    )?;
                }
                connection.execute(
                    "UPDATE goals SET updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    "draft",
                    &format!("{} criteria, {} tasks", criteria.len(), tasks.len()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, written)
        })?
    }

    /// Move a goal. The user is the only one who can approve it, and completion
    /// is refused while a criterion or task is still open.
    pub fn set_goal_state(
        &self,
        workspace: &str,
        id: i64,
        next: GoalState,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let moved = (|| {
                let state = goal_state(connection, workspace, id)?;
                if !state.allows(next) {
                    return Err(SessionError::Refused(format!(
                        "goal {id} cannot go from {} to {}",
                        state.as_str(),
                        next.as_str()
                    )));
                }
                if next.is_live()
                    && let Some(other) = conflicting_goal(connection, workspace, id)?
                {
                    return Err(SessionError::Refused(format!(
                        "goal {other} is already active in this workspace"
                    )));
                }
                if next == GoalState::Completed {
                    let goal = self.goal(workspace, id)?;
                    if let Some(blocker) = goal.completion_blocker() {
                        return Err(SessionError::Refused(format!(
                            "goal {id} cannot be completed: {blocker}"
                        )));
                    }
                }
                connection.execute(
                    "UPDATE goals SET status = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, next.as_str(), now],
                )?;
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    next.as_str(),
                    &format!("goal {}", next.as_str()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, moved)
        })?
    }

    /// A task is complete only with evidence attached.
    pub fn set_task_state(
        &self,
        workspace: &str,
        id: i64,
        position: i64,
        next: TaskState,
        evidence: &str,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let evidence = crate::agent::events::bounded(
            &crate::agent::security::redact(evidence),
            crate::agent::events::SUMMARY_LIMIT,
        );
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let moved = (|| {
                goal_state(connection, workspace, id)?;
                if next == TaskState::Completed && evidence.trim().is_empty() {
                    return Err(SessionError::Refused(format!(
                        "task {} cannot be completed without evidence",
                        position + 1
                    )));
                }
                let changed = connection.execute(
                    "UPDATE agent_tasks SET status = ?3, evidence = ?4, updated_at = ?5
                     WHERE goal_id = ?1 AND position = ?2",
                    params![id, position, next.as_str(), evidence, now],
                )?;
                if changed == 0 {
                    return Err(SessionError::Unknown(id));
                }
                log_goal_event(
                    connection,
                    id,
                    None,
                    "task",
                    next.as_str(),
                    &format!("task {} {}", position + 1, next.as_str()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, moved)
        })?
    }

    /// Satisfy a criterion with evidence, or waive it with the user's reason.
    pub fn set_criterion_state(
        &self,
        workspace: &str,
        id: i64,
        position: i64,
        next: CriterionState,
        evidence: &str,
        waiver: Option<&str>,
    ) -> Result<(), SessionError> {
        let now = crate::util::epoch_secs() as i64;
        let evidence = crate::agent::events::bounded(
            &crate::agent::security::redact(evidence),
            crate::agent::events::SUMMARY_LIMIT,
        );
        let waiver = waiver.map(|reason| {
            crate::agent::events::bounded(
                &crate::agent::security::redact(reason),
                crate::agent::events::SUMMARY_LIMIT,
            )
        });
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let moved = (|| {
                goal_state(connection, workspace, id)?;
                match next {
                    CriterionState::Satisfied if evidence.trim().is_empty() => {
                        return Err(SessionError::Refused(format!(
                            "criterion {} cannot be satisfied without evidence",
                            position + 1
                        )));
                    }
                    CriterionState::Waived
                        if waiver
                            .as_deref()
                            .is_none_or(|reason| reason.trim().is_empty()) =>
                    {
                        return Err(SessionError::Refused(format!(
                            "criterion {} needs a reason to be waived",
                            position + 1
                        )));
                    }
                    _ => {}
                }
                let changed = connection.execute(
                    "UPDATE goal_criteria
                     SET status = ?3, evidence = ?4, waiver_reason = ?5
                     WHERE goal_id = ?1 AND position = ?2",
                    params![id, position, next.as_str(), evidence, waiver],
                )?;
                if changed == 0 {
                    return Err(SessionError::Unknown(id));
                }
                log_goal_event(
                    connection,
                    id,
                    None,
                    "goal",
                    next.as_str(),
                    &format!("criterion {} {}", position + 1, next.as_str()),
                    now,
                )?;
                Ok(())
            })();
            finish(connection, moved)
        })?
    }

    /// Append one trace row and return its per-session `seq`. `seq` is allocated
    /// inside the insert's transaction, so two writers cannot share a number.
    pub fn record_event(&self, session: i64, event: NewEvent<'_>) -> Result<i64, SessionError> {
        let now = crate::util::epoch_secs() as i64;
        // The last gate before the database: a caller that forgot to redact
        // cannot store a credential or an unbounded payload.
        let summary = crate::agent::events::bounded(
            &crate::agent::security::redact(event.summary),
            crate::agent::events::SUMMARY_LIMIT,
        );
        self.write(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let inserted = (|| {
                connection.execute(
                    "INSERT INTO agent_events(session_id, seq, turn_id, kind, status, failure, tool_call_id, summary, pid, created_at)
                     SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
                     FROM agent_events WHERE session_id = ?1",
                    params![
                        session,
                        event.turn_id,
                        event.kind,
                        event.status,
                        event.failure,
                        event.tool_call_id,
                        summary,
                        event.pid,
                        now
                    ],
                )?;
                connection.query_row(
                    "SELECT seq FROM agent_events WHERE id = last_insert_rowid()",
                    [],
                    |row| row.get(0),
                )
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

    /// The stored trace for one session, in database order.
    pub fn events(&self, session: i64) -> Result<Vec<StoredEvent>, SessionError> {
        let mut statement = self.connection.prepare(
            "SELECT seq, turn_id, kind, status, failure, tool_call_id, summary, pid, created_at
             FROM agent_events WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([session], |row| {
            Ok(StoredEvent {
                seq: row.get(0)?,
                turn_id: row.get(1)?,
                kind: row.get(2)?,
                status: row.get(3)?,
                failure: row.get(4)?,
                tool_call_id: row.get(5)?,
                summary: row.get(6)?,
                pid: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
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

#[cfg(test)]
#[path = "tests/core/session.rs"]
mod tests;
