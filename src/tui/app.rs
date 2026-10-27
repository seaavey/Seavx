use std::io::stdout;
use std::time::Duration;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    style::Style,
    widgets::{Block, Borders},
};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::tui::event::{Action, AgentState};
use crate::tui::ui;

pub struct App<'a> {
    pub input: TextArea<'a>,
    pub response: String,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub total_tokens: i32,
    pub prompt_tokens: i32,
    pub status: AgentState,
    pub should_quit: bool,
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_cursor_line_style(Style::default());
        input.set_placeholder_text("Plan a feature, then build it step by step...");
        input.set_block(Block::default().borders(Borders::NONE));
        Self {
            input,
            response: String::new(),
            scroll: 0,
            auto_scroll: true,
            total_tokens: 0,
            prompt_tokens: 0,
            status: AgentState::Idle,
            should_quit: false,
        }
    }
}

pub async fn run_agent_worker(prompt: String, tx: mpsc::UnboundedSender<Action>) {
    tokio::spawn(async move {
        let _ = tx.send(Action::AgentToolStart("Analyzing workspace".to_string()));

        let security = crate::agent::security::WorkspaceSecurity::new(".");
        let config = crate::agent::engine::AgentConfig::load(security.root());
        let runtime = crate::agent::engine::AgentRuntime::new(config, security);

        let tx_clone = tx.clone();
        let result = runtime
            .execute_task(&prompt, move |evt| {
                if let Some(rest) = evt.strip_prefix("USAGE:") {
                    let parts: Vec<&str> = rest.split(':').collect();
                    if parts.len() == 2 {
                        let total = parts[0].parse::<i32>().unwrap_or(0);
                        let prompt = parts[1].parse::<i32>().unwrap_or(0);
                        let _ = tx_clone.send(Action::AgentUsage {
                            total_tokens: total,
                            prompt_tokens: prompt,
                        });
                    }
                } else {
                    let _ = tx_clone.send(Action::AgentToken(format!("\n[{evt}]\n")));
                }
            })
            .await;

        match result {
            Ok(answer) => {
                let clean_answer = answer.trim();
                if !clean_answer.is_empty() {
                    let _ = tx.send(Action::AgentToken(format!("\n\n{clean_answer}\n")));
                } else {
                    let _ = tx.send(Action::AgentToken(
                        "\n\n[Agent finished work without final message]\n".to_string(),
                    ));
                }
            }
            Err(e) => {
                let _ = tx.send(Action::AgentToken(format!(
                    "\n\nError executing task: {e}\n"
                )));
            }
        }

        let _ = tx.send(Action::AgentDone);
    });
}

pub async fn start_tui(initial_prompt: Option<String>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Action>();

    // Terminal keyboard event loop
    let input_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if crossterm::event::poll(Duration::from_millis(20)).unwrap_or(false) {
                if let Ok(CEvent::Key(key)) = crossterm::event::read() {
                    let _ = input_tx.send(Action::Key(key));
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    // 60 FPS tick timer for smooth redraws
    let tick_tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(16));
        loop {
            interval.tick().await;
            if tick_tx.send(Action::Tick).is_err() {
                break;
            }
        }
    });

    let mut app = App::new();

    // Trigger initial prompt if provided via CLI args
    if let Some(prompt) = initial_prompt {
        if !prompt.trim().is_empty() {
            app.status = AgentState::Thinking;
            run_agent_worker(prompt, tx.clone()).await;
        }
    }

    while !app.should_quit {
        terminal.draw(|frame| ui::render(&app, frame))?;

        if let Some(action) = rx.recv().await {
            match action {
                Action::Tick => {}
                Action::Key(key) => match key.code {
                    KeyCode::Esc => app.should_quit = true,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.should_quit = true;
                    }
                    KeyCode::PageUp => {
                        app.auto_scroll = false;
                        app.scroll = app.scroll.saturating_sub(5);
                    }
                    KeyCode::PageDown => {
                        app.scroll = app.scroll.saturating_add(5);
                    }
                    KeyCode::Home => {
                        app.auto_scroll = false;
                        app.scroll = 0;
                    }
                    KeyCode::End => {
                        app.auto_scroll = true;
                    }
                    KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        let lines = app.input.lines().to_vec();
                        let prompt = lines.join("\n").trim().to_string();
                        if !prompt.is_empty() {
                            app.response.clear();
                            app.scroll = 0;
                            app.auto_scroll = true;
                            app.status = AgentState::Thinking;
                            app.input = TextArea::default();
                            app.input.set_cursor_line_style(Style::default());
                            run_agent_worker(prompt, tx.clone()).await;
                        }
                    }
                    _ => {
                        app.input.input(key);
                    }
                },
                Action::AgentToken(token) => {
                    app.status = AgentState::Streaming;
                    app.response.push_str(&token);
                    if app.auto_scroll {
                        let lines = app.response.lines().count() as u16;
                        // Keep pinned to latest lines
                        app.scroll = lines;
                    }
                }
                Action::AgentUsage {
                    total_tokens,
                    prompt_tokens,
                } => {
                    app.total_tokens = total_tokens;
                    app.prompt_tokens = prompt_tokens;
                }
                Action::AgentToolStart(tool) => {
                    app.status = AgentState::ExecutingTool(tool.clone());
                    app.response.push_str(&format!("\n⚡ [{tool}]\n"));
                    if app.auto_scroll {
                        let lines = app.response.lines().count() as u16;
                        app.scroll = lines;
                    }
                }
                Action::AgentDone => {
                    app.status = AgentState::Idle;
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Clear debug.log on exit if reasoning effort is enabled
    let config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));
    if let Some(ref effort) = config.reasoning_effort {
        if !effort.trim().is_empty() && effort.trim().to_lowercase() != "none" {
            let log_path = std::path::PathBuf::from(".seavx/debug.log");
            if log_path.exists() {
                let _ = std::fs::write(&log_path, "");
            }
        }
    }

    Ok(())
}
