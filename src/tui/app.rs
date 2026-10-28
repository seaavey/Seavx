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
    style::{Color, Style},
    widgets::{Block, BorderType, Borders},
};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::tui::event::{Action, AgentState};
use crate::tui::ui;

pub fn scan_workspace_entries(query: &str) -> Vec<(bool, String)> {
    let clean_query = query.trim_start_matches('@').to_lowercase();
    let mut results = Vec::new();

    fn walk_dir(dir: &std::path::Path, results: &mut Vec<(bool, String)>, query: &str) {
        if results.len() >= 20 {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if results.len() >= 20 {
                break;
            }
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            let rel = path
                .strip_prefix(".")
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let is_dir = path.is_dir();
            if query.is_empty() || rel.to_lowercase().contains(query) {
                results.push((is_dir, rel));
            }
            if is_dir {
                walk_dir(&path, results, query);
            }
        }
    }

    walk_dir(std::path::Path::new("."), &mut results, &clean_query);
    results.sort_by(|a, b| a.1.cmp(&b.1));
    results
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Thinking(String),
}

pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/model", "Switch model and reasoning effort"),
    ("/clear", "Clear session conversation output"),
    ("/help", "Show help and command palette"),
    ("/init", "Initialize workspace configuration"),
    ("/doctor", "Check environment and health"),
    ("/memory", "Inspect persistent memory"),
    ("/exit", "Quit Seavx agent TUI"),
];
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalSelection {
    Model {
        models: Vec<String>,
        selected: usize,
    },
    Effort {
        model: String,
        options: Vec<&'static str>,
        selected: usize,
    },
}

/// Cached markdown render of the whole transcript, invalidated by `revision`.
pub struct TranscriptCache {
    pub revision: u64,
    pub width: u16,
    pub lines: Vec<ratatui::text::Line<'static>>,
}

pub struct App<'a> {
    pub workspace: String,
    pub input: TextArea<'a>,
    pub segments: Vec<Segment>,
    pub revision: u64,
    pub transcript_cache: std::cell::RefCell<Option<TranscriptCache>>,
    /// Prompts the user submitted, recallable with the arrow keys.
    pub history: Vec<String>,
    history_view: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub total_tokens: i32,
    pub prompt_tokens: i32,
    pub status: AgentState,
    pub session_title: Option<String>,
    pub should_quit: bool,
    pub config: crate::agent::engine::AgentConfig,
    pub modal: Option<ModalSelection>,
    pub slash_selected: usize,
    pub at_selected: usize,
    pub toast: Option<(String, std::time::Instant)>,
    pub last_ctrl_c: Option<std::time::Instant>,
}

/// Single source for the prompt textarea so resets keep its styling.
pub fn new_input<'a>() -> TextArea<'a> {
    let mut input = TextArea::default();
    input.set_cursor_line_style(Style::default());
    input.set_placeholder_text("Ask anything, / for commands, @ for files");
    input.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    input
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));
        Self {
            workspace: std::env::current_dir()
                .map(|p| {
                    let path = p.to_string_lossy().to_string();
                    match std::env::var("HOME") {
                        Ok(home) if path.starts_with(&home) => path.replacen(&home, "~", 1),
                        _ => path,
                    }
                })
                .unwrap_or_default(),
            input: new_input(),
            segments: Vec::new(),
            revision: 0,
            transcript_cache: std::cell::RefCell::new(None),
            history: Vec::new(),
            history_view: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            scroll: 0,
            auto_scroll: true,
            total_tokens: 0,
            prompt_tokens: 0,
            status: AgentState::Idle,
            session_title: None,
            should_quit: false,
            config,
            modal: None,
            slash_selected: 0,
            at_selected: 0,
            toast: None,
            last_ctrl_c: None,
        }
    }

    /// Append assistant/tool transcript text, coalescing into the last text block.
    pub fn push_text(&mut self, text: &str) {
        self.revision += 1;
        match self.segments.last_mut() {
            Some(Segment::Text(buf)) => buf.push_str(text),
            _ => self.segments.push(Segment::Text(text.to_string())),
        }
    }

    /// Append a reasoning block; consecutive reasoning merges into one block.
    pub fn push_thinking(&mut self, text: &str) {
        self.revision += 1;
        match self.segments.last_mut() {
            Some(Segment::Thinking(buf)) => {
                buf.push('\n');
                buf.push_str(text);
            }
            _ => self.segments.push(Segment::Thinking(text.to_string())),
        }
    }

    /// Replace the whole transcript with a single status message.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.revision += 1;
        self.segments = vec![Segment::Text(text.into())];
    }

    pub fn clear_transcript(&mut self) {
        self.revision += 1;
        self.segments.clear();
    }

    /// Remember a submitted prompt; slash commands and repeats are not worth recalling.
    pub fn remember_prompt(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        if prompt.is_empty() || prompt.starts_with('/') {
            return;
        }
        if self.history.last().map(String::as_str) != Some(prompt) {
            self.history.push(prompt.to_string());
        }
        self.reset_history_nav();
    }

    /// Recall an older prompt, filtered by whatever is already typed.
    pub fn history_previous(&mut self) {
        if self.history_view.is_empty() {
            let typed = self.input.lines().join("\n").trim().to_string();
            if typed.contains('\n') {
                return;
            }
            self.history_view = self
                .history
                .iter()
                .filter(|entry| entry.starts_with(&typed))
                .cloned()
                .collect();
            if self.history_view.is_empty() {
                return;
            }
            self.history_draft = typed;
        }

        let next = match self.history_cursor {
            None => self.history_view.len() - 1,
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_cursor = Some(next);
        self.set_input(&self.history_view[next].clone());
    }

    /// Walk forward again, ending on the text the user was typing.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.history_view.len() {
            self.history_cursor = Some(index + 1);
            self.set_input(&self.history_view[index + 1].clone());
            return;
        }

        let draft = std::mem::take(&mut self.history_draft);
        self.reset_history_nav();
        self.set_input(&draft);
    }

    fn reset_history_nav(&mut self) {
        self.history_cursor = None;
        self.history_view.clear();
        self.history_draft.clear();
    }

    fn set_input(&mut self, text: &str) {
        self.input = new_input();
        self.input.insert_str(text);
    }

    /// Rendered line count for scroll math (each reasoning block adds a header line).
    pub fn line_count(&self) -> u16 {
        self.segments
            .iter()
            .map(|segment| match segment {
                Segment::Text(text) => text.lines().count(),
                Segment::Thinking(text) => text.lines().count() + 1,
            })
            .sum::<usize>() as u16
    }
}

/// Ends the turn however the worker exits, panic included, so the UI never
/// stays stuck on `thinking`.
struct DoneGuard(mpsc::UnboundedSender<Action>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Action::AgentDone);
    }
}

pub async fn run_agent_worker(
    prompt: String,
    tx: mpsc::UnboundedSender<Action>,
    is_first_turn: bool,
) {
    tokio::spawn(async move {
        let _done = DoneGuard(tx.clone());
        let security = crate::agent::security::WorkspaceSecurity::new(".");
        let config = crate::agent::engine::AgentConfig::load(security.root());
        let runtime = crate::agent::engine::AgentRuntime::new(config.clone(), security);

        // Generate quick concise title only on the first turn of the session
        if is_first_turn {
            let title_tx = tx.clone();
            let title_prompt = prompt.clone();
            let client_clone = runtime.client().clone();
            let model_name = config.model.clone();
            let title_options = runtime.chat_options();
            tokio::spawn(async move {
                let req = genai::chat::ChatRequest::from_user(format!(
                    "Generate a very short 3-5 word concise title for this task without punctuation or quotes:\n{}",
                    title_prompt
                ));
                if let Ok(res) = client_clone
                    .exec_chat(&model_name, req, Some(&title_options))
                    .await
                    && let Some(text) = res.into_first_text()
                {
                    let clean = text.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !clean.is_empty() {
                        let _ = title_tx.send(Action::AgentTitle(clean));
                    }
                }
            });
        }

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
                } else if evt.starts_with("Tool: ") {
                    let tool_label = evt.strip_prefix("Tool: ").unwrap_or(evt);
                    let _ = tx_clone.send(Action::AgentToolStart(tool_label.to_string()));
                } else if let Some(thoughts) = evt.strip_prefix("THINKING:") {
                    let _ = tx_clone.send(Action::AgentThinking(thoughts.to_string()));
                } else if let Some(note) = evt.strip_prefix("System: ") {
                    let _ = tx_clone.send(Action::AgentToken(format!("\n  {note}\n")));
                }
            })
            .await;

        match result {
            Ok(answer) => {
                let clean_answer = answer.trim();
                if !clean_answer.is_empty() {
                    let _ = tx.send(Action::AgentToken(format!("\n\n{clean_answer}\n")));
                }
            }
            Err(e) => {
                let _ = tx.send(Action::AgentToken(format!("\n\nError: {e}\n")));
            }
        }
    });
}

pub async fn start_tui(initial_prompt: Option<String>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Action>();

    // Terminal event loop (Keyboard + Mouse via EventStream)
    let input_tx = tx.clone();
    tokio::spawn(async move {
        use crossterm::event::EventStream;
        use futures::StreamExt;

        let mut reader = EventStream::new();
        while let Some(Ok(event)) = reader.next().await {
            match event {
                CEvent::Key(key) => {
                    let _ = input_tx.send(Action::Key(key));
                }
                CEvent::Mouse(mouse) => {
                    let _ = input_tx.send(Action::Mouse(mouse));
                }
                _ => {}
            }
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
    if let Some(prompt) = initial_prompt
        && !prompt.trim().is_empty()
    {
        app.remember_prompt(&prompt);
        app.push_text(&format!("User: {prompt}\n\n"));
        app.status = AgentState::Thinking;
        run_agent_worker(prompt, tx.clone(), true).await;
    }

    while !app.should_quit {
        terminal.draw(|frame| ui::render(&app, frame))?;

        if let Some(action) = rx.recv().await {
            match action {
                Action::Tick => {}
                Action::Mouse(mouse) => {
                    let input_text = app.input.lines().join("\n").trim().to_string();
                    let at_query = input_text
                        .split_whitespace()
                        .last()
                        .filter(|word| word.starts_with('@'));

                    let at_matches = if let Some(query) = at_query {
                        scan_workspace_entries(query)
                    } else {
                        Vec::new()
                    };

                    // Handle mouse click/scroll on `@` popup if active
                    if !at_matches.is_empty() {
                        let max_visible_items = 8;
                        let menu_height =
                            (at_matches.len().min(max_visible_items) as u16 + 2).min(10);
                        let menu_width =
                            70.min(terminal.size().unwrap_or_default().width.saturating_sub(4));
                        let term_height = terminal.size().unwrap_or_default().height;
                        let input_y = term_height.saturating_sub(4);
                        let menu_y = input_y.saturating_sub(menu_height);

                        let in_menu_x = mouse.column >= 2 && mouse.column <= 2 + menu_width;
                        let in_menu_y = mouse.row >= menu_y && mouse.row <= menu_y + menu_height;

                        if in_menu_x && in_menu_y {
                            match mouse.kind {
                                crossterm::event::MouseEventKind::ScrollUp => {
                                    app.at_selected = if app.at_selected == 0 {
                                        at_matches.len().saturating_sub(1)
                                    } else {
                                        app.at_selected - 1
                                    };
                                    continue;
                                }
                                crossterm::event::MouseEventKind::ScrollDown => {
                                    app.at_selected = if app.at_selected + 1 >= at_matches.len() {
                                        0
                                    } else {
                                        app.at_selected + 1
                                    };
                                    continue;
                                }
                                crossterm::event::MouseEventKind::Down(
                                    crossterm::event::MouseButton::Left,
                                ) => {
                                    let relative_row = mouse.row.saturating_sub(menu_y + 1);
                                    let selected = app.at_selected.min(at_matches.len() - 1);
                                    let window_start = if selected >= max_visible_items {
                                        selected + 1 - max_visible_items
                                    } else {
                                        0
                                    };
                                    let clicked_idx = window_start + relative_row as usize;
                                    if clicked_idx < at_matches.len() {
                                        let (is_dir, ref path) = at_matches[clicked_idx];
                                        let tag = if is_dir {
                                            format!("[folder:{path}] ")
                                        } else {
                                            format!("[file:{path}] ")
                                        };
                                        let full_input = app.input.lines().join("\n");
                                        if let Some(last_at_idx) = full_input.rfind('@') {
                                            let mut new_text =
                                                full_input[..last_at_idx].to_string();
                                            new_text.push_str(&tag);
                                            app.input = new_input();
                                            app.input.insert_str(new_text);
                                        }
                                        app.at_selected = 0;
                                        continue;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    match mouse.kind {
                        crossterm::event::MouseEventKind::ScrollUp => {
                            let lines = app.line_count();
                            if app.auto_scroll {
                                app.scroll = lines;
                            }
                            app.auto_scroll = false;
                            app.scroll = app.scroll.saturating_sub(3);
                        }
                        crossterm::event::MouseEventKind::ScrollDown => {
                            let lines = app.line_count();
                            let next = app.scroll.saturating_add(3);
                            if next >= lines {
                                app.scroll = lines;
                                app.auto_scroll = true;
                            } else {
                                app.scroll = next;
                            }
                        }
                        _ => {}
                    }
                }
                Action::Key(key) => {
                    // Ctrl+C double press exit with confirmation toast
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        if let Some(prev) = app.last_ctrl_c
                            && prev.elapsed() < std::time::Duration::from_millis(1500)
                        {
                            app.should_quit = true;
                            continue;
                        }
                        app.last_ctrl_c = Some(std::time::Instant::now());
                        app.toast = Some((
                            "Press Ctrl+C again to exit".to_string(),
                            std::time::Instant::now(),
                        ));
                        continue;
                    }

                    // Global Ctrl shortcuts (Ctrl+D, Ctrl+Q) for immediate exit
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            KeyCode::Char('d') | KeyCode::Char('q') => {
                                app.should_quit = true;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    // Modal Key Interceptor (Arrow selection + Enter)
                    if let Some(modal) = app.modal.clone() {
                        match modal {
                            ModalSelection::Model { models, selected } => match key.code {
                                KeyCode::Esc => {
                                    app.modal = None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let new_sel = if selected == 0 {
                                        models.len().saturating_sub(1)
                                    } else {
                                        selected - 1
                                    };
                                    app.modal = Some(ModalSelection::Model {
                                        models,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let new_sel = if selected + 1 >= models.len() {
                                        0
                                    } else {
                                        selected + 1
                                    };
                                    app.modal = Some(ModalSelection::Model {
                                        models,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    // Refresh live models from remote gateway with validation & toast
                                    let base_url = app
                                        .config
                                        .base_url
                                        .as_deref()
                                        .unwrap_or(crate::agent::engine::DEFAULT_BASE_URL);
                                    let model_mgr = crate::agent::models::ModelManager::new(
                                        std::path::Path::new("."),
                                    );
                                    match model_mgr
                                        .refresh_remote_models(
                                            base_url,
                                            app.config.api_key.as_deref(),
                                        )
                                        .await
                                    {
                                        Ok(fresh) => {
                                            let count = fresh.len();
                                            let new_idx = fresh
                                                .iter()
                                                .position(|m| *m == app.config.model)
                                                .unwrap_or(0);
                                            app.modal = Some(ModalSelection::Model {
                                                models: fresh,
                                                selected: new_idx,
                                            });
                                            app.toast = Some((
                                                format!("✓ Refreshed {count} models from gateway"),
                                                std::time::Instant::now(),
                                            ));
                                        }
                                        Err(err) => {
                                            app.toast = Some((
                                                format!("✗ Refresh failed: {err}"),
                                                std::time::Instant::now(),
                                            ));
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if !models.is_empty() && selected < models.len() {
                                        let chosen_model = models[selected].clone();
                                        let options = vec![
                                            "none", "minimal", "medium", "high", "highx", "max",
                                        ];
                                        let current_eff = app
                                            .config
                                            .reasoning_effort
                                            .as_deref()
                                            .unwrap_or("none");
                                        let eff_sel = options
                                            .iter()
                                            .position(|o| *o == current_eff)
                                            .unwrap_or(0);
                                        app.modal = Some(ModalSelection::Effort {
                                            model: chosen_model,
                                            options,
                                            selected: eff_sel,
                                        });
                                    } else {
                                        app.modal = None;
                                    }
                                }
                                _ => {}
                            },
                            ModalSelection::Effort {
                                model,
                                options,
                                selected,
                            } => match key.code {
                                KeyCode::Esc => {
                                    app.modal = None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let new_sel = if selected == 0 {
                                        options.len().saturating_sub(1)
                                    } else {
                                        selected - 1
                                    };
                                    app.modal = Some(ModalSelection::Effort {
                                        model,
                                        options,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let new_sel = if selected + 1 >= options.len() {
                                        0
                                    } else {
                                        selected + 1
                                    };
                                    app.modal = Some(ModalSelection::Effort {
                                        model,
                                        options,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Enter => {
                                    let chosen_effort = options[selected].to_string();
                                    app.config.model = model.clone();
                                    app.config.reasoning_effort = Some(chosen_effort.clone());

                                    // Persist to .seavx/config.toml
                                    let config_dir = std::path::PathBuf::from(".seavx");
                                    let _ = std::fs::create_dir_all(&config_dir);
                                    let config_path = config_dir.join("config.toml");
                                    if let Ok(toml_str) = toml::to_string_pretty(&app.config) {
                                        let _ = std::fs::write(config_path, toml_str);
                                    }

                                    app.set_text(format!(
                                        "✓ Switched model to: {}\n✓ Reasoning effort: {}\nSaved to .seavx/config.toml",
                                        app.config.model,
                                        app.config.reasoning_effort.as_deref().unwrap_or("none")
                                    ));
                                    app.scroll = 0;
                                    app.auto_scroll = true;
                                    app.modal = None;
                                }
                                _ => {}
                            },
                        }
                        continue;
                    }

                    let input_text = app.input.lines().join("\n").trim().to_string();
                    let at_query = input_text
                        .split_whitespace()
                        .last()
                        .filter(|word| word.starts_with('@'));

                    let at_matches = if let Some(query) = at_query {
                        scan_workspace_entries(query)
                    } else {
                        Vec::new()
                    };

                    if !at_matches.is_empty() {
                        match key.code {
                            KeyCode::Up => {
                                app.at_selected = if app.at_selected == 0 {
                                    at_matches.len().saturating_sub(1)
                                } else {
                                    app.at_selected - 1
                                };
                                continue;
                            }
                            KeyCode::Down => {
                                app.at_selected = if app.at_selected + 1 >= at_matches.len() {
                                    0
                                } else {
                                    app.at_selected + 1
                                };
                                continue;
                            }
                            KeyCode::Tab | KeyCode::Enter => {
                                let (is_dir, ref path) =
                                    at_matches[app.at_selected.min(at_matches.len() - 1)];
                                let tag = if is_dir {
                                    format!("[folder:{path}] ")
                                } else {
                                    format!("[file:{path}] ")
                                };

                                // Replace the trailing @word with tag
                                let full_input = app.input.lines().join("\n");
                                if let Some(last_at_idx) = full_input.rfind('@') {
                                    let mut new_text = full_input[..last_at_idx].to_string();
                                    new_text.push_str(&tag);
                                    app.input = new_input();
                                    app.input.insert_str(new_text);
                                }
                                app.at_selected = 0;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    let in_slash_mode = input_text.starts_with('/') && !input_text.contains(' ');
                    let filtered_cmds: Vec<(&str, &str)> = if in_slash_mode {
                        SLASH_COMMANDS
                            .iter()
                            .copied()
                            .filter(|(cmd, _)| cmd.starts_with(&input_text))
                            .collect()
                    } else {
                        Vec::new()
                    };

                    if in_slash_mode && !filtered_cmds.is_empty() {
                        match key.code {
                            KeyCode::Up => {
                                app.slash_selected = if app.slash_selected == 0 {
                                    filtered_cmds.len().saturating_sub(1)
                                } else {
                                    app.slash_selected - 1
                                };
                                continue;
                            }
                            KeyCode::Down => {
                                app.slash_selected =
                                    if app.slash_selected + 1 >= filtered_cmds.len() {
                                        0
                                    } else {
                                        app.slash_selected + 1
                                    };
                                continue;
                            }
                            KeyCode::Tab => {
                                let (selected_cmd, _) =
                                    filtered_cmds[app.slash_selected.min(filtered_cmds.len() - 1)];
                                app.input = new_input();
                                app.input.insert_str(format!("{selected_cmd} "));
                                app.slash_selected = 0;
                                continue;
                            }
                            KeyCode::Enter => {
                                let (selected_cmd, _) =
                                    filtered_cmds[app.slash_selected.min(filtered_cmds.len() - 1)];
                                if selected_cmd == "/model" {
                                    app.input = new_input();
                                    let base_url = app
                                        .config
                                        .base_url
                                        .as_deref()
                                        .unwrap_or(crate::agent::engine::DEFAULT_BASE_URL);
                                    let model_mgr = crate::agent::models::ModelManager::new(
                                        std::path::Path::new("."),
                                    );
                                    let models = model_mgr
                                        .get_cached_models(
                                            &crate::agent::models::ModelManager::domain_slug(
                                                base_url,
                                            ),
                                        )
                                        .unwrap_or_default();
                                    if !models.is_empty() {
                                        let current_idx = models
                                            .iter()
                                            .position(|m| *m == app.config.model)
                                            .unwrap_or(0);
                                        app.modal = Some(ModalSelection::Model {
                                            models,
                                            selected: current_idx,
                                        });
                                    } else {
                                        app.set_text("No cached models found. Run `seavx model` once to populate cache.");
                                        app.scroll = 0;
                                        app.auto_scroll = true;
                                    }
                                    continue;
                                } else if selected_cmd == "/clear" {
                                    app.input = new_input();
                                    app.clear_transcript();
                                    app.session_title = None;
                                    app.scroll = 0;
                                    continue;
                                } else if selected_cmd == "/exit" {
                                    app.should_quit = true;
                                    continue;
                                } else {
                                    app.input = new_input();
                                    app.input.insert_str(format!("{selected_cmd} "));
                                    continue;
                                }
                            }
                            _ => {}
                        }
                    }

                    match key.code {
                        KeyCode::Esc => app.should_quit = true,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.should_quit = true;
                        }
                        KeyCode::PageUp => {
                            let lines = app.line_count();
                            if app.auto_scroll {
                                app.scroll = lines;
                            }
                            app.auto_scroll = false;
                            app.scroll = app.scroll.saturating_sub(5);
                        }
                        KeyCode::PageDown => {
                            let lines = app.line_count();
                            let next = app.scroll.saturating_add(5);
                            if next >= lines {
                                app.scroll = lines;
                                app.auto_scroll = true;
                            } else {
                                app.scroll = next;
                            }
                        }
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            let lines = app.line_count();
                            if app.auto_scroll {
                                app.scroll = lines;
                            }
                            app.auto_scroll = false;
                            app.scroll = app.scroll.saturating_sub(2);
                        }
                        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            let lines = app.line_count();
                            let next = app.scroll.saturating_add(2);
                            if next >= lines {
                                app.scroll = lines;
                                app.auto_scroll = true;
                            } else {
                                app.scroll = next;
                            }
                        }
                        // Single-line prompt: arrows recall history, filtered by the typed prefix.
                        KeyCode::Up if app.input.lines().len() == 1 => app.history_previous(),
                        KeyCode::Down if app.input.lines().len() == 1 => app.history_next(),
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
                                let is_model_cmd =
                                    prompt == "/model" || prompt.starts_with("/model ");
                                if is_model_cmd {
                                    let parts: Vec<&str> = prompt.split_whitespace().collect();
                                    app.input = new_input();

                                    let base_url = app
                                        .config
                                        .base_url
                                        .as_deref()
                                        .unwrap_or(crate::agent::engine::DEFAULT_BASE_URL);
                                    let model_mgr = crate::agent::models::ModelManager::new(
                                        std::path::Path::new("."),
                                    );
                                    let models = model_mgr
                                        .get_cached_models(
                                            &crate::agent::models::ModelManager::domain_slug(
                                                base_url,
                                            ),
                                        )
                                        .unwrap_or_default();

                                    if parts.len() == 1 || (parts.len() == 2 && parts[1] == "list")
                                    {
                                        if !models.is_empty() {
                                            let current_idx = models
                                                .iter()
                                                .position(|m| *m == app.config.model)
                                                .unwrap_or(0);
                                            app.modal = Some(ModalSelection::Model {
                                                models,
                                                selected: current_idx,
                                            });
                                        } else {
                                            app.set_text("No cached models found. Run `seavx model` once to populate cache.");
                                            app.scroll = 0;
                                            app.auto_scroll = true;
                                        }
                                    } else {
                                        let target_arg = parts[1];
                                        let resolved_model = if let Ok(num) =
                                            target_arg.parse::<usize>()
                                            && num > 0
                                            && num <= models.len()
                                        {
                                            models[num - 1].clone()
                                        } else {
                                            target_arg.to_string()
                                        };

                                        let new_effort = parts.get(2).map(|s| s.to_string());
                                        app.config.model = resolved_model.clone();
                                        if let Some(ref eff) = new_effort {
                                            app.config.reasoning_effort = Some(eff.clone());
                                        }

                                        let config_dir = std::path::PathBuf::from(".seavx");
                                        let _ = std::fs::create_dir_all(&config_dir);
                                        let config_path = config_dir.join("config.toml");
                                        if let Ok(toml_str) = toml::to_string_pretty(&app.config) {
                                            let _ = std::fs::write(config_path, toml_str);
                                        }

                                        app.set_text(format!(
                                            "Switched model to: {}\nEffort: {}\nSaved to .seavx/config.toml",
                                            app.config.model,
                                            app.config
                                                .reasoning_effort
                                                .as_deref()
                                                .unwrap_or("none")
                                        ));
                                        app.scroll = 0;
                                        app.auto_scroll = true;
                                    }
                                    continue;
                                }

                                app.remember_prompt(&prompt);
                                if !app.segments.is_empty() {
                                    app.push_text("\n\n");
                                }
                                app.push_text(&format!("User: {prompt}\n\n"));
                                app.scroll = app.line_count();
                                app.auto_scroll = true;
                                app.status = AgentState::Thinking;
                                app.input = new_input();
                                let is_first_turn = app.session_title.is_none();
                                run_agent_worker(prompt, tx.clone(), is_first_turn).await;
                            }
                        }
                        _ => {
                            app.reset_history_nav();
                            app.input.input(key);
                        }
                    }
                }
                Action::AgentToken(token) => {
                    app.status = AgentState::Streaming;
                    app.push_text(&token);
                    if app.auto_scroll {
                        let lines = app.line_count();
                        // Keep pinned to latest lines
                        app.scroll = lines;
                    }
                }
                Action::AgentThinking(thoughts) => {
                    app.push_thinking(&thoughts);
                    if app.auto_scroll {
                        app.scroll = app.line_count();
                    }
                }
                Action::AgentTitle(title) => {
                    app.session_title = Some(title);
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
                    // Clean compact Hermes-style tool call indicator without brackets
                    app.push_text(&format!("\n⚡ {tool}\n\n"));
                    if app.auto_scroll {
                        let lines = app.line_count();
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
    if let Some(ref effort) = config.reasoning_effort
        && !effort.trim().is_empty()
        && effort.trim().to_lowercase() != "none"
    {
        let log_path = std::path::PathBuf::from(".seavx/debug.log");
        if log_path.exists() {
            let _ = std::fs::write(&log_path, "");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(history: &[&str]) -> App<'static> {
        let mut app = App::new();
        for entry in history {
            app.remember_prompt(entry);
        }
        app
    }

    fn typed(app: &App) -> String {
        app.input.lines().join("\n")
    }

    #[test]
    fn arrows_recall_history_newest_first() {
        let mut app = app_with(&["first", "second", "third"]);

        app.history_previous();
        assert_eq!(typed(&app), "third");
        app.history_previous();
        assert_eq!(typed(&app), "second");
        app.history_previous();
        assert_eq!(typed(&app), "first");
        app.history_previous();
        assert_eq!(typed(&app), "first", "stays on the oldest entry");

        app.history_next();
        assert_eq!(typed(&app), "second");
        app.history_next();
        app.history_next();
        assert_eq!(
            typed(&app),
            "",
            "past the newest entry restores what was typed"
        );
    }

    #[test]
    fn typed_prefix_filters_history_and_restores_the_draft() {
        let mut app = app_with(&["cargo test", "cargo clippy", "git status"]);
        app.input.insert_str("cargo");

        app.history_previous();
        assert_eq!(typed(&app), "cargo clippy");
        app.history_previous();
        assert_eq!(typed(&app), "cargo test");
        app.history_next();
        app.history_next();
        assert_eq!(
            typed(&app),
            "cargo",
            "draft is restored after the last match"
        );
    }

    #[test]
    fn unmatched_prefix_leaves_the_input_alone() {
        let mut app = app_with(&["cargo test"]);
        app.input.insert_str("zig build");
        app.history_previous();
        assert_eq!(typed(&app), "zig build");
    }

    #[test]
    fn slash_commands_and_repeats_stay_out_of_history() {
        let mut app = App::new();
        app.remember_prompt("/model");
        app.remember_prompt("do the thing");
        app.remember_prompt("do the thing");
        app.remember_prompt("   ");
        assert_eq!(app.history, vec!["do the thing".to_string()]);
    }

    #[test]
    fn multiline_input_is_left_to_the_textarea() {
        let mut app = app_with(&["older prompt"]);
        app.input.insert_str("line one");
        app.input.insert_newline();
        app.input.insert_str("line two");
        app.history_previous();
        assert_eq!(typed(&app), "line one\nline two");
    }
}
