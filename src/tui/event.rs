use crossterm::event::{KeyEvent, MouseEvent};

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
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool(String),
}
