use crossterm::event::{KeyEvent, MouseEvent};

use crate::cli::doctor::DoctorReport;

#[derive(Debug)]
pub enum Action {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    AgentToken(String),
    /// One `/goal` or `/tasks` operation finished: its report, and the goal
    /// line `/status` should show from now on.
    GoalDone {
        result: Result<String, String>,
        goal: Option<String>,
        /// Set when the approved plan should start one Build turn.
        start: Option<String>,
    },
    /// `/review` finished resolving a Git target off the UI thread.
    ReviewResolved(Result<crate::agent::git_review::ReviewInput, String>),
    AgentThinking(String),
    AgentTool(crate::agent::events::ToolEvent),
    /// The session the title belongs to: a late answer must not rename the
    /// session the user already started.
    AgentTitle {
        title: String,
        session: Option<i64>,
    },
    AgentUsage {
        total_tokens: i32,
        prompt_tokens: i32,
    },
    /// The turn's terminal status, plus what the OS said about a killed
    /// process tree and which pid was abandoned.
    TurnFinished {
        status: crate::agent::cancel::TurnStatus,
        cleanup: Option<crate::agent::cancel::Cleanup>,
        pid: Option<u32>,
    },
    AgentDone,
    /// The worker created or adopted a session row.
    SessionStarted(i64),
    /// Persistence degraded for the rest of the run; one notice, no retry.
    SessionFailed(String),
    DoctorDone(DoctorReport),
    /// `/models` refresh finished: the fresh list, or why the gateway refused.
    ModelsRefreshed(Result<Vec<String>, String>),
    CompactDone {
        result: Result<crate::tui::compact::CompactResult, String>,
    },
    UndoDone(Result<Vec<std::path::PathBuf>, String>),
    SkillsInstalled(Result<std::path::PathBuf, String>),
    SkillsRemoved(Result<std::path::PathBuf, String>),
    /// The picker's Enter: the indices to install from the pending checkout.
    SkillsInstallMarked(Vec<usize>),
    /// One row per picked skill: its name and where it landed, or why not.
    SkillsInstalledBatch(Vec<(String, Result<std::path::PathBuf, String>)>),
    SkillsFetched(Result<crate::cli::skills::Fetched, String>),
    /// `/sessions`: the numbered list, or the reason it could not be read.
    /// `/sessions <n>`: the session to open, or the refusal with its reason.
    SessionResume(Result<crate::session::LoadedSession, String>),
    /// A tool call needs an answer. The worker blocks on `reply`; dropping it
    /// (quit, panic) resolves as a denial.
    /// A clarification, already persisted; the reply resolves it once.
    QuestionAsked {
        id: i64,
        question: String,
        kind: crate::session::QuestionKind,
        options: Vec<String>,
        reply: tokio::sync::oneshot::Sender<(crate::session::QuestionState, String)>,
    },
    PermissionAsk {
        request: crate::agent::permissions::PermissionRequest,
        reply: tokio::sync::oneshot::Sender<crate::agent::permissions::Answer>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool(String),
    Checking,
    Compacting,
}
