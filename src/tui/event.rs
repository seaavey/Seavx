use crossterm::event::{KeyEvent, MouseEvent};

use crate::cli::doctor::DoctorReport;

#[derive(Debug)]
pub enum Action {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    AgentToken(String),
    AgentThinking(String),
    AgentToolStart(String),
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
    /// `/sessions`: the numbered list, or the reason it could not be read.
    /// `/sessions <n>`: the session to open, or the refusal with its reason.
    SessionResume(Result<crate::session::LoadedSession, String>),
    /// A tool call needs an answer. The worker blocks on `reply`; dropping it
    /// (quit, panic) resolves as a denial.
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
