use crossterm::event::KeyEvent;

#[derive(Debug)]
pub enum Action {
    Tick,
    Key(KeyEvent),
    AgentToken(String),
    AgentToolStart(String),
    AgentUsage { total_tokens: i32, prompt_tokens: i32 },
    AgentDone,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool(String),
}
