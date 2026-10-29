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
    AgentTitle(String),
    AgentUsage {
        total_tokens: i32,
        prompt_tokens: i32,
    },
    AgentDone,
    DoctorDone(DoctorReport),
    CompactDone {
        result: Result<crate::tui::compact::CompactResult, String>,
    },
    UndoDone(Result<Vec<std::path::PathBuf>, String>),
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
