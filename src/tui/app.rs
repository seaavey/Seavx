use std::io::stdout;
use std::time::Duration;

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyEvent, KeyModifiers,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::agent::engine::{AgentMode, EFFORT_LEVELS};
use crate::tui::event::{Action, AgentState};
use crate::tui::ui;

mod completion;
mod input;
mod references;
mod transcript;

pub(crate) use input::new_input;
use references::AtCache;
// Tests name both root paths through `use super::*`; the binary reaches the
// scanner through `at_matches`.
#[cfg(test)]
pub use references::scan_workspace_entries;
#[cfg(test)]
pub(in crate::tui::app) use references::scan_workspace_entries_at;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Agent output: rendered as markdown, so prose reflows to the pane.
    Text(String),
    /// Command and system output (reports, listings, errors): plain text whose
    /// newlines are the layout. Markdown would fold every row into a paragraph.
    Notice(String),
    Thinking(String),
}

/// Wrap-around previous index; no-op-safe when the list is empty.
fn prev_index(sel: usize, len: usize) -> usize {
    if sel == 0 {
        len.saturating_sub(1)
    } else {
        sel - 1
    }
}

/// Wrap-around next index; no-op-safe when the list is empty.
fn next_index(sel: usize, len: usize) -> usize {
    if sel + 1 >= len { 0 } else { sel + 1 }
}

/// Case-insensitive substring match for the model picker's filter.
fn model_matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// The rows the model picker shows: the names containing the query. Rendering
/// and Enter both read this, so the cursor always lands on the row on screen.
pub fn visible_models<'a>(models: &'a [String], query: &str) -> Vec<&'a String> {
    models
        .iter()
        .filter(|model| model_matches(model, query))
        .collect()
}

/// The picker's search box: a printable key appends, Backspace deletes one
/// character. `None` means the key is not text, so the caller keeps it.
fn edit_query(query: &str, key: &KeyEvent) -> Option<String> {
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Backspace => {
            let mut trimmed = query.to_string();
            trimmed.pop();
            Some(trimmed)
        }
        KeyCode::Char(c) => Some(format!("{query}{c}")),
        _ => None,
    }
}

fn is_mode_shortcut(key: &KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Whether a key edits the prompt text (as opposed to only moving the cursor).
/// Only these regrow the ghost suggestion after the raw key is applied.
fn edits_text(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(_) => {
            !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
        }
        KeyCode::Backspace | KeyCode::Enter | KeyCode::Tab => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalSelection {
    Model {
        models: Vec<String>,
        /// Type-to-filter query, matched case-insensitively on the name.
        query: String,
        selected: usize,
    },
    Effort {
        model: String,
        options: Vec<&'static str>,
        selected: usize,
    },
    /// One row per stored session, newest first: the order `/sessions <n>` reads.
    Session { rows: Vec<String>, selected: usize },
}

/// Cached markdown render of the whole transcript, invalidated by `revision`.
pub struct TranscriptCache {
    pub revision: u64,
    pub width: u16,
    pub lines: Vec<ratatui::text::Line<'static>>,
}

pub struct App<'a> {
    pub workspace: String,
    /// The canonical workspace root: the directory the checkpoint store and the
    /// agent's security policy both use.
    pub root: std::path::PathBuf,
    pub input: TextArea<'a>,
    pub segments: Vec<Segment>,
    pub revision: u64,
    pub transcript_cache: std::cell::RefCell<Option<TranscriptCache>>,
    /// Prompts the user submitted, recallable with the arrow keys.
    pub history: Vec<String>,
    history_view: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    /// Prompt history persisted to `.seavx/svx_history`.
    pub history_file: crate::history::ChatHistory,
    /// Last seen mtime of the history file, used to pick up external edits.
    history_mtime: Option<std::time::SystemTime>,
    /// Character index where the dimmed suggestion tail starts; `None` when
    /// no ghost is showing. The typed part is always `chars[..ghost_typed]`.
    ghost_typed: Option<usize>,
    /// Terminal width, refreshed each frame; fits ghost suggestions inside the box.
    pub terminal_width: u16,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub total_tokens: i32,
    pub prompt_tokens: i32,
    pub status: AgentState,
    pub session_title: Option<String>,
    /// Summary retained from `/compact` and prepended to later requests.
    pub session_context: Option<String>,
    pub should_quit: bool,
    /// The running turn's cancel handle. Fresh per turn.
    pub cancel: crate::agent::cancel::CancelToken,
    pub config: crate::agent::engine::AgentConfig,
    pub mode: AgentMode,
    pub modal: Option<ModalSelection>,
    /// A `/models` refresh is in flight in a worker; the picker shows a spinner
    /// and a second `r` is ignored until the answer lands.
    pub refreshing_models: bool,
    pub slash_selected: usize,
    pub at_selected: usize,
    /// `@` matches for the current query; rendering must not walk the workspace
    /// once per frame.
    at_cache: std::cell::RefCell<AtCache>,
    /// Prompt text whose popups the user already closed with Esc.
    dismissed_popup: Option<String>,
    pub toast: Option<(String, std::time::Instant)>,
    pub last_ctrl_c: Option<std::time::Instant>,
    /// The question the worker is blocked on, until the user answers it.
    pub pending_permission: Option<PendingPermission>,
    /// The one store handle; `None` when persistence is off for this run.
    pub store: Option<std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>>,
    /// Session this transcript belongs to, `None` until the first prompt.
    pub session_id: Option<i64>,
    /// One notice text when the store failed and the run continued unpersisted.
    pub persistence_error: Option<String>,
    /// Set when the loaded session cannot be appended to.
    pub read_only: Option<String>,
}

/// What the worker needs to run one turn: the raw prompt, the replayed history
/// and the store handle the rows are written through.
pub struct TurnRequest {
    pub prompt: String,
    pub session_id: Option<i64>,
    pub store: Option<std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>>,
    /// This turn's cancel handle, shared with the UI thread.
    pub cancel: crate::agent::cancel::CancelToken,
}

/// What Enter does with the line in the prompt.
pub enum EnterAction {
    /// A parsed slash command, ready for `commands::execute`.
    Command(crate::tui::commands::SlashCommand),
    /// A prompt for the agent.
    Turn(String),
    /// Nothing: an empty line, or a bare `/` that is not a command.
    Nothing,
}

/// A permission question and the channel its answer travels back on.
pub struct PendingPermission {
    pub request: crate::agent::permissions::PermissionRequest,
    pub reply: tokio::sync::oneshot::Sender<crate::agent::permissions::Answer>,
}

/// Answers asks through the event loop: the UI thread renders the question, the
/// worker waits on the oneshot. A dropped channel is a denial, so quitting with a
/// question on screen never approves anything.
pub struct TuiConfirmer {
    tx: mpsc::UnboundedSender<Action>,
}

impl TuiConfirmer {
    pub fn new(tx: mpsc::UnboundedSender<Action>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl crate::agent::permissions::Confirmer for TuiConfirmer {
    async fn confirm(
        &self,
        request: crate::agent::permissions::PermissionRequest,
    ) -> crate::agent::permissions::Answer {
        use crate::agent::permissions::Answer;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        if self
            .tx
            .send(Action::PermissionAsk { request, reply })
            .is_err()
        {
            return Answer::Deny;
        }
        receiver.await.unwrap_or(Answer::Deny)
    }
}

impl<'a> App<'a> {
    /// Answer the pending permission question with one key: `y` once, `a` this
    /// category, `u` unrestricted, `n`/`Esc` deny. Returns the answer sent, or
    /// `None` when the key is not an answer.
    pub fn answer_permission(
        &mut self,
        code: KeyCode,
    ) -> Option<crate::agent::permissions::Answer> {
        use crate::agent::permissions::Answer;
        let answer = match code {
            KeyCode::Char('y') => Answer::Once,
            KeyCode::Char('a') => Answer::Category,
            KeyCode::Char('u') => Answer::Unrestricted,
            KeyCode::Char('n') | KeyCode::Esc => Answer::Deny,
            _ => return None,
        };
        let pending = self.pending_permission.take()?;
        let _ = pending.reply.send(answer);
        Some(answer)
    }

    pub fn new() -> Self {
        Self::with_history(crate::history::ChatHistory::load(
            &crate::history::config_dir(),
        ))
    }

    /// Like [`App::new`] but with an explicit history store, so tests never
    /// touch the workspace's real `.seavx/svx_history`.
    fn with_history(history_file: crate::history::ChatHistory) -> Self {
        let config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));
        let history = history_file.entries();
        let history_mtime = history_file.modified();
        Self {
            workspace: std::env::current_dir()
                .map(|p| {
                    let path = p.to_string_lossy().to_string();
                    match crate::home::dir() {
                        Some(home) if path.starts_with(home.to_string_lossy().as_ref()) => {
                            path.replacen(home.to_string_lossy().as_ref(), "~", 1)
                        }
                        _ => path,
                    }
                })
                .unwrap_or_default(),
            root: crate::agent::security::WorkspaceSecurity::new(".")
                .root()
                .to_path_buf(),
            input: new_input(),
            segments: Vec::new(),
            revision: 0,
            transcript_cache: std::cell::RefCell::new(None),
            history,
            history_view: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            history_file,
            history_mtime,
            ghost_typed: None,
            terminal_width: 80,
            scroll: 0,
            auto_scroll: true,
            total_tokens: 0,
            prompt_tokens: 0,
            status: AgentState::Idle,
            session_title: None,
            session_context: None,
            should_quit: false,
            cancel: crate::agent::cancel::CancelToken::new(),
            config,
            mode: AgentMode::Build,
            modal: None,
            refreshing_models: false,
            slash_selected: 0,
            at_selected: 0,
            at_cache: std::cell::RefCell::new(None),
            dismissed_popup: None,
            toast: None,
            last_ctrl_c: None,
            pending_permission: None,
            store: None,
            session_id: None,
            persistence_error: None,
            read_only: None,
        }
    }

    /// Attach the store, or record the one notice and keep running unpersisted.
    /// A corrupt or newer database never fails the TUI.
    pub fn open_store(
        &mut self,
        store: Result<crate::session::SessionStore, crate::session::SessionError>,
    ) {
        match store {
            Ok(store) => self.attach_store(std::sync::Arc::new(std::sync::Mutex::new(store))),
            Err(error) => {
                let notice = format!("Session history disabled: {error}");
                self.persistence_error = Some(notice.clone());
                self.push_notice(notice);
            }
        }
    }

    /// Hand the app the store it writes through, or the run is unpersisted.
    pub fn attach_store(
        &mut self,
        store: std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>,
    ) {
        self.store = Some(store);
    }

    /// Rebuild the transcript from stored rows, prefill the pending prompt and
    /// report what the resume budget left out.
    pub fn load_session(&mut self, session: crate::session::LoadedSession) {
        // The load replaces the screen: the rows on it and the compaction
        // summary belong to whatever session was open before this one.
        self.clear_transcript();
        self.read_only = None;
        self.session_id = Some(session.id);
        self.session_title = session.title.clone();
        self.total_tokens = session.total_tokens as i32;
        self.prompt_tokens = session.prompt_tokens as i32;
        if let Some(reason) = session.read_only.clone() {
            self.read_only = Some(reason.clone());
            self.push_notice(format!("Session {} is read-only: {reason}", session.id));
        }

        let outcome = crate::session::replay(&session);
        let body = crate::session::render_messages(&outcome.history);
        if !body.is_empty() {
            self.push_text(body.trim_end());
            self.push_text("\n\n");
        }
        if outcome.omitted > 0 {
            self.push_notice(format!(
                "System: {} older messages omitted (resume budget).",
                outcome.omitted
            ));
        }
        if let Some(draft) = outcome.draft {
            self.push_text(&format!("User: {draft} (pending, not sent)\n\n"));
            self.set_input(&draft);
        } else {
            // A draft from the session that was open would otherwise ride along.
            self.input = new_input();
        }
        self.scroll = self.line_count();
        self.auto_scroll = true;
    }

    /// `/clear` and `/new`: a fresh session id, transcript and compaction
    /// context; nothing stored is deleted.
    pub fn start_new_session(&mut self) {
        self.clear_transcript();
        self.session_title = None;
        self.session_id = None;
        self.read_only = None;
        // The compaction context belongs to the session that produced it.
        self.session_context = None;
        // Usage is per session too, or the footer keeps the old count.
        self.total_tokens = 0;
        self.prompt_tokens = 0;
    }

    /// Show the prompt and hand the worker everything it needs. The user row is
    /// written by the worker, so the UI thread never waits on SQLite.
    pub fn submit_turn(&mut self, prompt: &str) -> Option<TurnRequest> {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return None;
        }
        if let Some(reason) = self.read_only.clone() {
            self.push_notice(format!(
                "This session is read-only ({reason}). Use /clear to start a new one."
            ));
            return None;
        }
        // One turn at a time: a second request would replace `self.cancel` and
        // leave the running turn with a token nobody can reach.
        if self.status != AgentState::Idle {
            self.push_notice("A task is already running. Press Esc to cancel it first.");
            return None;
        }

        self.remember_prompt(&prompt);
        if !self.segments.is_empty() {
            self.push_text("\n\n");
        }
        self.push_text(&format!("User: {prompt}\n\n"));
        self.scroll = self.line_count();
        self.auto_scroll = true;
        self.status = AgentState::Thinking;
        self.input = new_input();
        self.reset_history_nav();
        // A cancelled turn must not poison the next one.
        self.cancel = crate::agent::cancel::CancelToken::new();

        Some(TurnRequest {
            prompt,
            session_id: self.session_id,
            store: self.store.clone(),
            cancel: self.cancel.clone(),
        })
    }

    /// Show a title only for the session that asked for it. A title request is
    /// spawned, so its answer can arrive after the user started another session;
    /// renaming that one would be wrong.
    pub fn apply_title(&mut self, session: Option<i64>, title: String) -> bool {
        if session != self.session_id {
            return false;
        }
        self.session_title = Some(title);
        true
    }

    /// Switch modes only when no task is running. Returns whether the mode changed.
    /// The header and footer already show the active mode, so switching stays silent.
    pub fn set_mode(&mut self, mode: AgentMode) -> bool {
        if self.status != AgentState::Idle || self.mode == mode {
            return false;
        }

        self.mode = mode;
        true
    }

    /// Switch modes for the user's `/plan` or shortcut, explaining a refusal so
    /// a switch that cannot happen is never silent.
    pub fn request_mode(&mut self, mode: AgentMode) {
        if self.mode == mode {
            // A repeat is information, not a refusal: say what is already true.
            let label = match mode {
                AgentMode::Plan => "Plan mode is already active",
                AgentMode::Build => "Build mode is already active",
            };
            self.push_mode_notice(label);
            return;
        }
        if !self.set_mode(mode) {
            self.push_mode_notice("Cannot switch mode while a task is running");
            return;
        }
        let label = match mode {
            AgentMode::Plan => "Plan mode enabled — writes are disabled",
            AgentMode::Build => "Build mode enabled",
        };
        self.push_mode_notice(label);
    }

    pub fn toggle_mode(&mut self) {
        let next = match self.mode {
            AgentMode::Build => AgentMode::Plan,
            AgentMode::Plan => AgentMode::Build,
        };
        self.request_mode(next);
    }

    fn push_mode_notice(&mut self, text: &str) {
        self.push_text(&format!("\nSystem: {text}\n"));
        self.scroll = self.line_count();
        self.auto_scroll = true;
    }

    /// Esc, in order: close the ghost, close a popup, cancel the running turn,
    /// and only quit when nothing else on screen claims the key.
    pub fn escape(&mut self) {
        if self.ghost_active() {
            self.clear_ghost();
        } else if self.popup_open() {
            self.dismiss_popups();
        } else if self.status != AgentState::Idle {
            self.cancel.cancel();
        } else {
            self.should_quit = true;
        }
    }

    /// The turn ended: the spinner stops, and a cancel says so once.
    pub fn finish_turn(
        &mut self,
        status: crate::agent::cancel::TurnStatus,
        cleanup: Option<crate::agent::cancel::Cleanup>,
        pid: Option<u32>,
    ) {
        self.status = AgentState::Idle;
        if status != crate::agent::cancel::TurnStatus::Cancelled {
            return;
        }
        let mut line = String::from("System: cancelled");
        if cleanup == Some(crate::agent::cancel::Cleanup::Unconfirmed) {
            line.push('\n');
            line.push_str("System: ");
            line.push_str(&crate::agent::cancel::cleanup_notice(
                pid.unwrap_or_default(),
            ));
        }
        self.push_notice(line);
    }

    /// Open the cached-model picker. Returns `false` when the cache is empty.
    pub fn open_model_picker(&mut self) -> bool {
        let models = cached_models(self);
        if models.is_empty() {
            self.set_text("No cached models found. Run `seavx model` once to populate cache.");
            self.scroll = 0;
            self.auto_scroll = true;
            return false;
        }

        let current_idx = models
            .iter()
            .position(|m| *m == self.config.model)
            .unwrap_or(0);
        self.modal = Some(ModalSelection::Model {
            models,
            query: String::new(),
            selected: current_idx,
        });
        true
    }

    /// Move the open model picker's cursor by one row, wrapping. Arrows and the
    /// wheel both land here, so the cursor never leaves the filtered rows.
    pub fn step_model_picker(&mut self, down: bool) {
        let Some(ModalSelection::Model {
            models,
            query,
            selected,
        }) = self.modal.take()
        else {
            return;
        };
        let len = visible_models(&models, &query).len();
        let selected = if down {
            next_index(selected, len)
        } else {
            prev_index(selected, len)
        };
        self.modal = Some(ModalSelection::Model {
            models,
            query,
            selected,
        });
    }

    /// Land a finished `/models` refresh: the spinner stops and the picker shows
    /// the new list. A picker closed while the fetch ran stays closed, and only
    /// the toast reports the outcome.
    pub fn models_refreshed(&mut self, result: Result<Vec<String>, String>) {
        self.refreshing_models = false;
        match result {
            Ok(models) => {
                let count = models.len();
                // A refresh keeps the filter the user typed; the cursor follows
                // the active model through the same filtered list. Only a model
                // picker is replaced: an effort picker opened meanwhile stays.
                let query = match &self.modal {
                    Some(ModalSelection::Model { query, .. }) => Some(query.clone()),
                    _ => None,
                };
                if let Some(query) = query {
                    let selected = visible_models(&models, &query)
                        .iter()
                        .position(|model| **model == self.config.model)
                        .unwrap_or(0);
                    self.modal = Some(ModalSelection::Model {
                        models,
                        query,
                        selected,
                    });
                }
                self.toast = Some((
                    format!("✓ Refreshed {count} models from gateway"),
                    std::time::Instant::now(),
                ));
            }
            Err(error) => {
                self.toast = Some((
                    format!("✗ Refresh failed: {error}"),
                    std::time::Instant::now(),
                ));
            }
        }
    }

    /// Open the session picker from this store's listing for the current
    /// workspace. Returns `false` when there is nothing to pick, so the caller
    /// can say so instead of opening an empty modal.
    pub fn open_session_picker(&mut self, store: &crate::session::SessionStore) -> bool {
        let rows = match crate::cli::sessions::session_rows(store, &crate::session::workspace_key())
        {
            Ok(rows) if rows.is_empty() => {
                self.push_notice("No sessions yet. Start one by sending a prompt.");
                return false;
            }
            Ok(rows) => rows,
            Err(error) => {
                self.push_notice(format!("Sessions unavailable: {error}"));
                return false;
            }
        };

        self.modal = Some(ModalSelection::Session { rows, selected: 0 });
        true
    }

    /// Switch the model, optionally with a reasoning effort, and persist it.
    pub fn set_model(&mut self, target: &str, effort: Option<&str>) {
        let models = cached_models(self);
        let resolved_model = match resolve_model_target(&models, target) {
            Ok(model) => model,
            Err(error) => {
                self.push_notice(error);
                return;
            }
        };

        let config_path = crate::cli::config::workspace_config_path(std::path::Path::new("."));
        // One write: the model and the effort change together or not at all.
        let mut errors = Vec::new();
        if let Err(error) =
            crate::cli::config::set_model_and_effort(&config_path, &resolved_model, effort)
        {
            errors.push(error);
        }

        self.config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));

        let mut message = format!(
            "Switched model to: {}\nEffort: {}\nSaved to .seavx/config.toml",
            self.config.model,
            self.config.reasoning_effort.as_deref().unwrap_or("none")
        );
        for error in errors {
            message.push_str(&format!("\n{error}"));
        }

        self.set_text(message);
        self.scroll = 0;
        self.auto_scroll = true;
    }
}

/// The three ways out of the TUI, checked before anything that could swallow a
/// key: `Ctrl+D`/`Ctrl+Q` quit at once, `Ctrl+C` needs a second press within
/// 1.5 s. Returns `true` when the key was consumed.
fn quit_shortcut(app: &mut App, key: &KeyEvent) -> bool {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Char('d') | KeyCode::Char('q') => {
            app.should_quit = true;
            true
        }
        KeyCode::Char('c') => {
            if let Some(prev) = app.last_ctrl_c
                && prev.elapsed() < std::time::Duration::from_millis(1500)
            {
                app.should_quit = true;
                return true;
            }
            app.last_ctrl_c = Some(std::time::Instant::now());
            app.toast = Some((
                "Press Ctrl+C again to exit".to_string(),
                std::time::Instant::now(),
            ));
            true
        }
        _ => false,
    }
}

/// Load one session off the event loop and hand it back as an action. Both
/// `/sessions <n>` and its picker go through here, so a number means the same
/// thing in both.
fn spawn_session_resume(app: &mut App, tx: &mpsc::UnboundedSender<Action>, number: u32) {
    app.status = AgentState::Checking;
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let result = crate::session::SessionStore::open()
            .map_err(|error| error.to_string())
            .and_then(|store| {
                crate::cli::sessions::resume_by_number(
                    &store,
                    &crate::session::workspace_key(),
                    number,
                )
            });
        let _ = tx.send(Action::SessionResume(result));
    });
}

/// Apply the event-loop effects a command asked for.
fn apply_command_effect(
    app: &mut App,
    effect: crate::tui::commands::CommandEffect,
    tx: &mpsc::UnboundedSender<Action>,
) {
    use crate::tui::commands::CommandEffect;

    match effect {
        CommandEffect::Handled => {}
        CommandEffect::Quit => app.should_quit = true,
        CommandEffect::OpenModelPicker => {
            app.open_model_picker();
        }
        CommandEffect::RunDoctor { offline } => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let options = crate::cli::doctor::DoctorOptions {
                offline,
                workspace_root: std::path::PathBuf::from("."),
                home: crate::home::dir(),
            };
            tokio::spawn(async move {
                let report = crate::cli::doctor::run(options).await;
                let _ = tx.send(Action::DoctorDone(report));
            });
        }
        CommandEffect::RunSessions { number } => spawn_session_resume(app, tx, number),
        CommandEffect::OpenSessionPicker => match crate::session::SessionStore::open() {
            Ok(store) => {
                app.open_session_picker(&store);
            }
            Err(error) => app.push_notice(format!("Sessions unavailable: {error}")),
        },
        CommandEffect::RunUndo => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            let root = app.root.clone();
            tokio::task::spawn_blocking(move || {
                let result =
                    crate::checkpoint::CheckpointStore::undo_latest(&root).map_err(|error| {
                        // A half-done undo must say what it already changed.
                        let restored = error.restored();
                        if restored.is_empty() {
                            return error.to_string();
                        }
                        let paths: Vec<String> = restored
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect();
                        format!(
                            "{error}\nRestored {} file(s) before the failure:\n{}",
                            paths.len(),
                            paths.join("\n")
                        )
                    });
                let _ = tx.send(Action::UndoDone(result));
            });
        }
        CommandEffect::RunCompact { instructions } => {
            let transcript = crate::tui::compact::transcript_from_segments(&app.segments);
            if crate::tui::compact::is_too_short(&transcript) {
                app.push_notice(format!(
                    "Transcript is too short to compact ({} lines).",
                    transcript.lines().count()
                ));
                return;
            }

            app.status = AgentState::Compacting;
            let tx = tx.clone();
            let config = app.config.clone();
            tokio::spawn(async move {
                let summarizer = crate::tui::compact::ModelSummarizer::from_config(&config);
                let result = crate::tui::compact::run_compaction(
                    &transcript,
                    instructions.as_deref(),
                    &summarizer,
                )
                .await;
                let _ = tx.send(Action::CompactDone { result });
            });
        }
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

/// Geometry of the `@` reference popup. Shared so drawing and mouse clicks agree.
pub struct AtPopupLayout {
    pub area: Rect,
    pub window_start: usize,
    pub max_visible: usize,
}

/// Cached models for the configured gateway; empty when the cache is cold.
/// A cache past its TTL still answers: an old list beats an empty picker, and
/// `seavx model` is what refreshes it.
fn cached_models(app: &App) -> Vec<String> {
    let base_url = app
        .config
        .base_url
        .as_deref()
        .unwrap_or(crate::agent::engine::DEFAULT_BASE_URL);
    let model_mgr = crate::agent::models::ModelManager::new(std::path::Path::new("."));
    model_mgr
        .get_cached_models_allow_stale(&crate::agent::models::ModelManager::domain_slug(base_url))
        .unwrap_or_default()
}

/// Resolve a `/model` argument: a 1-based index into the cached list, or a
/// literal model name. An index outside the list is an error, never a name —
/// `/model 9` against three cached models must not save `model = "9"`.
fn resolve_model_target(models: &[String], target: &str) -> Result<String, String> {
    let Ok(number) = target.parse::<usize>() else {
        return Ok(target.to_string());
    };
    number
        .checked_sub(1)
        .and_then(|index| models.get(index))
        .cloned()
        .ok_or_else(|| {
            format!(
                "No model number {number} in the cached list ({} known). Run `seavx model` to refresh it.",
                models.len()
            )
        })
}

/// The store is behind a mutex: the connection is `Send` but not `Sync`, and
/// every write holds the lock for one insert.
fn lock_store(
    store: &std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>,
) -> std::sync::MutexGuard<'_, crate::session::SessionStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub async fn run_agent_worker(
    request: TurnRequest,
    tx: mpsc::UnboundedSender<Action>,
    is_first_turn: bool,
    mode: AgentMode,
    session_context: Option<String>,
) {
    tokio::spawn(async move {
        let _done = DoneGuard(tx.clone());
        let security = crate::agent::security::WorkspaceSecurity::new(".");
        let config = crate::agent::engine::AgentConfig::load(security.root());
        let runtime = crate::agent::engine::AgentRuntime::new(config.clone(), security);

        let TurnRequest {
            prompt,
            session_id,
            mut store,
            cancel,
        } = request;
        let mut session_id = session_id;

        // The user row and the history for this request, off the UI thread.
        let mut history = Vec::new();
        if let Some(handle) = store.clone() {
            let prepared = {
                let guard = lock_store(&handle);
                crate::session::prepare_turn(
                    &guard,
                    &crate::session::workspace_key(),
                    &config.model,
                    session_id,
                    &prompt,
                )
            };
            match prepared {
                Ok((id, replayed)) => {
                    session_id = Some(id);
                    history = replayed;
                    let _ = tx.send(Action::SessionStarted(id));
                }
                Err(error) => {
                    let _ = tx.send(Action::SessionFailed(format!(
                        "Session history disabled: {error}"
                    )));
                    store = None;
                }
            }
        }

        // Generate quick concise title only on the first turn of the session
        if is_first_turn {
            let title_tx = tx.clone();
            let title_session = session_id;
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
                    let clean = crate::util::unquote(text.trim()).to_string();
                    if !clean.is_empty() {
                        let _ = title_tx.send(Action::AgentTitle {
                            title: clean,
                            session: title_session,
                        });
                    }
                }
            });
        }

        let tx_clone = tx.clone();
        let row_store = store.clone();
        let row_tx = tx.clone();
        // Set by the first streamed token: the answer is already on screen, so
        // the turn must not append it a second time.
        let streamed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streamed_evt = streamed.clone();
        let task = crate::tui::compact::build_agent_task(&prompt, session_context.as_deref());
        let result = runtime
            .execute_task(
                &task,
                mode,
                history,
                &TuiConfirmer::new(tx.clone()),
                &cancel,
                move |evt| {
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
                    } else if let Some(token) = evt.strip_prefix("TOKEN:") {
                        let first = !streamed_evt.swap(true, std::sync::atomic::Ordering::Relaxed);
                        let frame = if first {
                            format!("\n\n{token}")
                        } else {
                            token.to_string()
                        };
                        let _ = tx_clone.send(Action::AgentToken(frame));
                    } else if evt.starts_with("Tool: ") {
                        let tool_label = evt.strip_prefix("Tool: ").unwrap_or(evt);
                        let _ = tx_clone.send(Action::AgentToolStart(tool_label.to_string()));
                    } else if let Some(thoughts) = evt.strip_prefix("THINKING:") {
                        let _ = tx_clone.send(Action::AgentThinking(thoughts.to_string()));
                    } else if let Some(note) = evt.strip_prefix("System: ") {
                        let _ = tx_clone.send(Action::AgentToken(format!("\n  {note}\n")));
                    }
                },
                // The single persistence channel: every appended message, once.
                move |message, status| {
                    let (Some(handle), Some(id)) = (&row_store, session_id) else {
                        return;
                    };
                    let guard = lock_store(handle);
                    if let Err(error) = guard.record_message(id, message, status) {
                        let _ = row_tx.send(Action::SessionFailed(format!(
                            "Session history disabled: {error}"
                        )));
                    }
                },
            )
            .await;

        let _ = tx.send(Action::TurnFinished {
            status: result.status,
            cleanup: result.cleanup,
            pid: result.pid,
        });

        match result.result {
            Ok(answer) => {
                if streamed.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = tx.send(Action::AgentToken("\n".to_string()));
                } else {
                    let clean_answer = answer.trim();
                    if !clean_answer.is_empty() {
                        let _ = tx.send(Action::AgentToken(format!("\n\n{clean_answer}\n")));
                    }
                }
            }
            // A cancelled turn already said so through its status; the error it
            // carries is the same news, and printing both reads as a failure.
            Err(_) if result.status == crate::agent::cancel::TurnStatus::Cancelled => {}
            Err(e) => {
                let _ = tx.send(Action::AgentToken(format!("\n\nError: {e}\n")));
            }
        }
    });
}

pub async fn start_tui(
    initial_prompt: Option<String>,
    loaded: Option<crate::session::LoadedSession>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            terminal.backend_mut(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();

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

    // One store handle for the run; a store that cannot open degrades to one
    // notice and an unpersisted session instead of failing the TUI.
    app.open_store(crate::session::SessionStore::open());

    // `seavx sessions resume` loads before the loop starts, so the indicator is
    // the transcript itself: no async restore to wait for.
    if let Some(session) = loaded {
        app.load_session(session);
    }

    // Trigger initial prompt if provided via CLI args
    if let Some(prompt) = initial_prompt
        && let Some(request) = app.submit_turn(&prompt)
    {
        let is_first_turn = app.session_title.is_none();
        run_agent_worker(request, tx.clone(), is_first_turn, AgentMode::Build, None).await;
    }

    // A batch the last process never finished is reported once, loudly, and
    // only undone when the user asks for it.
    for batch in crate::checkpoint::recover_pending(&app.root).unwrap_or_default() {
        app.push_notice(batch.render());
    }

    while !app.should_quit {
        // Ghost suggestions must fit inside the prompt box, so track resize.
        app.terminal_width = terminal
            .size()
            .map(|area| area.width)
            .unwrap_or(app.terminal_width);
        // Pick up prompts written to `.seavx/svx_history` while running.
        app.sync_history();
        terminal.draw(|frame| ui::render(&app, frame))?;

        if let Some(action) = rx.recv().await {
            match action {
                Action::Tick => {}
                Action::Mouse(mouse) => {
                    let at_matches = app.at_matches();

                    // Handle mouse click/scroll on `@` popup if active
                    if !app.popups_dismissed() && !at_matches.is_empty() {
                        let size = terminal.size().unwrap_or_default();
                        let screen = Rect::new(0, 0, size.width, size.height);
                        let layout = app.at_popup_layout(screen, at_matches.len());
                        let area = layout.area;

                        let in_menu_x =
                            mouse.column >= area.x && mouse.column < area.x + area.width;
                        let in_menu_y = mouse.row >= area.y && mouse.row < area.y + area.height;

                        if in_menu_x && in_menu_y {
                            match mouse.kind {
                                crossterm::event::MouseEventKind::ScrollUp => {
                                    app.at_selected = prev_index(app.at_selected, at_matches.len());
                                    continue;
                                }
                                crossterm::event::MouseEventKind::ScrollDown => {
                                    app.at_selected = next_index(app.at_selected, at_matches.len());
                                    continue;
                                }
                                crossterm::event::MouseEventKind::Down(
                                    crossterm::event::MouseButton::Left,
                                ) => {
                                    let relative_row = mouse.row.saturating_sub(area.y + 1);
                                    let clicked_idx = layout.window_start + relative_row as usize;
                                    if clicked_idx < at_matches.len() {
                                        let (is_dir, path) = &at_matches[clicked_idx];
                                        app.insert_at_tag(*is_dir, path);
                                        app.at_selected = 0;
                                        continue;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    // An open picker owns the wheel: the transcript behind it
                    // is not what the pointer is aiming at.
                    if matches!(app.modal, Some(ModalSelection::Model { .. })) {
                        match mouse.kind {
                            crossterm::event::MouseEventKind::ScrollUp => {
                                app.step_model_picker(false);
                                continue;
                            }
                            crossterm::event::MouseEventKind::ScrollDown => {
                                app.step_model_picker(true);
                                continue;
                            }
                            _ => {}
                        }
                    }

                    match mouse.kind {
                        crossterm::event::MouseEventKind::ScrollUp => app.scroll_up(3),
                        crossterm::event::MouseEventKind::ScrollDown => app.scroll_down(3),
                        _ => {}
                    }
                }
                Action::Key(key) => {
                    // Quitting is never blocked. A pending question is answered by
                    // dropping its reply, which the confirmer treats as a denial, so
                    // Ctrl+D cannot approve anything on the way out.
                    if quit_shortcut(&mut app, &key) {
                        continue;
                    }

                    // A pending question owns the next key: y/a/u answer it, n/Esc
                    // denies. Nothing else runs until it is resolved.
                    if app.pending_permission.is_some() {
                        match app.answer_permission(key.code) {
                            Some(answer) => {
                                use crate::agent::permissions::Answer;
                                let label = match answer {
                                    Answer::Once => "approved once",
                                    Answer::Category => "approved for this category",
                                    Answer::Unrestricted => "switched to allow",
                                    Answer::Deny => "denied",
                                };
                                app.push_notice(format!("Permission {label}."));
                            }
                            None => app.push_notice("Answer the permission prompt: y / a / u / n"),
                        }
                        continue;
                    }

                    // Modal Key Interceptor (Arrow selection + Enter)
                    if let Some(modal) = app.modal.clone() {
                        match modal {
                            ModalSelection::Model {
                                models,
                                query,
                                selected,
                            } => {
                                // Typing is the search box, so 'j'/'k' no longer
                                // move: arrows do.
                                if let Some(query) = edit_query(&query, &key) {
                                    app.modal = Some(ModalSelection::Model {
                                        models,
                                        query,
                                        selected: 0,
                                    });
                                    continue;
                                }
                                match key.code {
                                    KeyCode::Esc => {
                                        app.modal = None;
                                    }
                                    KeyCode::Up => app.step_model_picker(false),
                                    KeyCode::Down => app.step_model_picker(true),
                                    KeyCode::Char('r')
                                        if key.modifiers.contains(KeyModifiers::CONTROL)
                                            && !app.refreshing_models =>
                                    {
                                        // The fetch runs off the UI thread: awaiting it
                                        // here froze the terminal for its whole timeout.
                                        app.refreshing_models = true;
                                        let base_url =
                                            app.config.base_url.clone().unwrap_or_else(|| {
                                                crate::agent::engine::DEFAULT_BASE_URL.to_string()
                                            });
                                        let api_key = app.config.api_key.clone();
                                        let tx = tx.clone();
                                        tokio::spawn(async move {
                                            let model_mgr = crate::agent::models::ModelManager::new(
                                                std::path::Path::new("."),
                                            );
                                            let outcome = model_mgr
                                                .refresh_remote_models(
                                                    &base_url,
                                                    api_key.as_deref(),
                                                )
                                                .await;
                                            let _ = tx.send(Action::ModelsRefreshed(outcome));
                                        });
                                    }
                                    KeyCode::Enter => {
                                        let chosen = visible_models(&models, &query)
                                            .get(selected)
                                            .map(|model| (*model).clone());
                                        let options = EFFORT_LEVELS.to_vec();
                                        let current_eff = app
                                            .config
                                            .reasoning_effort
                                            .as_deref()
                                            .unwrap_or("none");
                                        let eff_sel = options
                                            .iter()
                                            .position(|o| *o == current_eff)
                                            .unwrap_or(0);
                                        app.modal = chosen.map(|model| ModalSelection::Effort {
                                            model,
                                            options,
                                            selected: eff_sel,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            ModalSelection::Effort {
                                model,
                                options,
                                selected,
                            } => match key.code {
                                KeyCode::Esc => {
                                    app.modal = None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let new_sel = prev_index(selected, options.len());
                                    app.modal = Some(ModalSelection::Effort {
                                        model,
                                        options,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let new_sel = next_index(selected, options.len());
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

                                    // Persist through the one config writer.
                                    let saved = app.config.save(std::path::Path::new("."));

                                    let saved_line = match &saved {
                                        Ok(()) => "Saved to .seavx/config.toml".to_string(),
                                        Err(error) => format!("Failed to save config: {error}"),
                                    };
                                    app.set_text(format!(
                                        "Switched model to: {}\nReasoning effort: {}\n{saved_line}",
                                        app.config.model,
                                        app.config.reasoning_effort.as_deref().unwrap_or("none")
                                    ));
                                    app.scroll = 0;
                                    app.auto_scroll = true;
                                    app.modal = None;
                                }
                                _ => {}
                            },
                            ModalSelection::Session { rows, selected } => match key.code {
                                KeyCode::Esc => {
                                    app.modal = None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let new_sel = prev_index(selected, rows.len());
                                    app.modal = Some(ModalSelection::Session {
                                        rows,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let new_sel = next_index(selected, rows.len());
                                    app.modal = Some(ModalSelection::Session {
                                        rows,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Enter => {
                                    app.modal = None;
                                    // Rows are the `/sessions` order, so the index is the number.
                                    if selected < rows.len() {
                                        spawn_session_resume(&mut app, &tx, (selected + 1) as u32);
                                    }
                                }
                                _ => {}
                            },
                        }
                        continue;
                    }

                    if is_mode_shortcut(&key) {
                        app.toggle_mode();
                        continue;
                    }

                    let at_matches = app.at_matches();

                    if !app.popups_dismissed() && !at_matches.is_empty() {
                        match key.code {
                            KeyCode::Up => {
                                app.at_selected = prev_index(app.at_selected, at_matches.len());
                                continue;
                            }
                            KeyCode::Down => {
                                app.at_selected = next_index(app.at_selected, at_matches.len());
                                continue;
                            }
                            KeyCode::Tab | KeyCode::Enter => {
                                let (is_dir, path) =
                                    &at_matches[app.at_selected.min(at_matches.len() - 1)];
                                app.insert_at_tag(*is_dir, path);
                                app.at_selected = 0;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    let filtered_cmds = app.slash_matches();

                    if !app.popups_dismissed() && !filtered_cmds.is_empty() {
                        match key.code {
                            KeyCode::Up => {
                                app.slash_selected =
                                    prev_index(app.slash_selected, filtered_cmds.len());
                                continue;
                            }
                            KeyCode::Down => {
                                app.slash_selected =
                                    next_index(app.slash_selected, filtered_cmds.len());
                                continue;
                            }
                            KeyCode::Tab => {
                                let (selected_cmd, _) =
                                    filtered_cmds[app.slash_selected.min(filtered_cmds.len() - 1)];
                                app.input = new_input();
                                app.input.insert_str(format!("{selected_cmd} "));
                                app.slash_selected = 0;
                                app.refresh_ghost();
                                continue;
                            }
                            KeyCode::Enter => {
                                let (selected_cmd, _) =
                                    filtered_cmds[app.slash_selected.min(filtered_cmds.len() - 1)];
                                app.input = new_input();
                                if let Some(command) = crate::tui::commands::parse(selected_cmd) {
                                    let effect = crate::tui::commands::execute(&mut app, command);
                                    apply_command_effect(&mut app, effect, &tx);
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }

                    match key.code {
                        KeyCode::Esc => app.escape(),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.should_quit = true;
                        }
                        KeyCode::PageUp => app.scroll_up(5),
                        KeyCode::PageDown => app.scroll_down(5),
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            app.scroll_up(2);
                        }
                        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            app.scroll_down(2);
                        }
                        // Single-line prompt: arrows recall history, filtered by the typed prefix.
                        KeyCode::Up if app.input.lines().len() == 1 => app.history_previous(),
                        KeyCode::Down if app.input.lines().len() == 1 => app.history_next(),
                        // Accept or eat the ghost suggestion at the end of the line.
                        KeyCode::Tab => {
                            if app.ghost_active() {
                                // Accept: the tail becomes part of the prompt.
                                app.set_input(&app.input.lines()[0].clone());
                                app.refresh_ghost();
                            } else {
                                app.reset_history_nav();
                                app.input.input(key);
                            }
                        }
                        KeyCode::Right => {
                            if app.ghost_active() {
                                app.set_input(&app.input.lines()[0].clone());
                                app.refresh_ghost();
                            } else {
                                app.clear_ghost();
                                app.input.input(key);
                            }
                        }
                        KeyCode::Delete => {
                            if app.ghost_active() {
                                // First Delete swallows the suggestion, like fish.
                                app.clear_ghost();
                            } else {
                                app.reset_history_nav();
                                app.input.input(key);
                            }
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
                            let prompt = lines.join("\n");
                            match app.enter_action(&prompt) {
                                EnterAction::Command(command) => {
                                    let effect = crate::tui::commands::execute(&mut app, command);
                                    apply_command_effect(&mut app, effect, &tx);
                                    continue;
                                }
                                EnterAction::Nothing => continue,
                                // One Enter: one request, one new user row. The
                                // worker writes the row before it sends.
                                EnterAction::Turn(prompt) => {
                                    if let Some(request) = app.submit_turn(&prompt) {
                                        let is_first_turn = app.session_title.is_none();
                                        run_agent_worker(
                                            request,
                                            tx.clone(),
                                            is_first_turn,
                                            app.mode,
                                            app.session_context.clone(),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        _ => {
                            app.reset_history_nav();
                            // Cursor moves dismiss the ghost; edits regrow it.
                            app.clear_ghost();
                            app.input.input(key);
                            if edits_text(&key) {
                                app.refresh_ghost();
                            }
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
                Action::AgentTitle { title, session } => {
                    // The title is stored against the session that asked for it,
                    // and only shown when that session is still the open one.
                    if let (Some(store), Some(id)) = (app.store.clone(), session) {
                        let stored = title.clone();
                        tokio::task::spawn_blocking(move || {
                            let guard = lock_store(&store);
                            if let Err(error) = guard.record_title(id, &stored) {
                                tracing::warn!(%error, "could not store the session title");
                            }
                        });
                    }
                    app.apply_title(session, title);
                }
                Action::AgentUsage {
                    total_tokens,
                    prompt_tokens,
                } => {
                    app.total_tokens = total_tokens;
                    app.prompt_tokens = prompt_tokens;
                    if let (Some(store), Some(id)) = (app.store.clone(), app.session_id) {
                        tokio::task::spawn_blocking(move || {
                            let guard = lock_store(&store);
                            if let Err(error) =
                                guard.set_usage(id, total_tokens as i64, prompt_tokens as i64)
                            {
                                tracing::warn!(%error, "could not store session usage");
                            }
                        });
                    }
                }
                Action::SessionStarted(id) => {
                    app.session_id = Some(id);
                }
                Action::SessionFailed(message) => {
                    app.store = None;
                    app.persistence_error = Some(message.clone());
                    app.push_notice(message);
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
                Action::TurnFinished {
                    status,
                    cleanup,
                    pid,
                } => app.finish_turn(status, cleanup, pid),
                Action::AgentDone => {
                    app.status = AgentState::Idle;
                }
                Action::PermissionAsk { request, reply } => {
                    app.push_notice(format!(
                        "Permission required: {} wants to {}.\n  y = once  ·  a = this category  ·  u = unrestricted  ·  n = deny",
                        request.tool, request.effect
                    ));
                    app.pending_permission = Some(PendingPermission { request, reply });
                }
                Action::ModelsRefreshed(result) => app.models_refreshed(result),
                Action::DoctorDone(report) => {
                    app.status = AgentState::Idle;
                    app.push_notice(crate::cli::doctor::render(&report));
                }
                Action::SessionResume(result) => {
                    app.status = AgentState::Idle;
                    match result {
                        Ok(session) => app.load_session(session),
                        Err(error) => app.push_notice(error),
                    }
                }
                Action::UndoDone(result) => {
                    app.status = AgentState::Idle;
                    match result {
                        Ok(paths) if paths.is_empty() => app.push_notice("Nothing to undo."),
                        Ok(paths) => {
                            let mut text = format!("Restored {} file(s):", paths.len());
                            for path in paths {
                                text.push_str(&format!("\n{}", path.display()));
                            }
                            app.push_notice(text);
                        }
                        Err(error) => app.push_notice(format!("Undo failed: {error}")),
                    }
                }
                Action::CompactDone { result } => {
                    app.status = AgentState::Idle;
                    match result {
                        Ok(result) => app.apply_compaction(result),
                        Err(error) => {
                            app.push_notice(format!("Compaction failed: {error}"));
                        }
                    }
                }
            }
        }
    }

    if keyboard_enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
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

/// App with an isolated history file so tests never read or write the
/// workspace's real `.seavx/svx_history`.
#[cfg(test)]
pub(crate) fn test_app() -> App<'static> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join("seavx-tui-tests").join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    App::with_history(crate::history::ChatHistory::load(&dir))
}

#[cfg(test)]
#[path = "../tests/tui/app.rs"]
mod tests;
