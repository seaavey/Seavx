use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::agent::engine::AgentMode;
use crate::tui::event::{Action, AgentState};

mod actions;
mod completion;
mod effects;
mod input;
mod keys;
mod modal;
mod mouse;
mod references;
mod run;
mod store;
mod transcript;
mod worker;

// The binary and `sessions resume` enter the TUI by this path.
pub use run::start_tui;

use effects::{apply_command_effect, spawn_session_resume};
// The root tests call the shortcut directly; production reaches it through
// `keys::handle`.
#[cfg(test)]
use keys::quit_shortcut;

pub(crate) use input::new_input;
use references::AtCache;
// Tests name both root paths through `use super::*`; the binary reaches the
// scanner through `at_matches`.
#[cfg(test)]
pub use references::scan_workspace_entries;
#[cfg(test)]
pub(in crate::tui::app) use references::scan_workspace_entries_at;
// The key handler, the modal renderer and the tests reach these by path.
pub(in crate::tui::app) use modal::edit_query;
#[cfg(test)]
pub(in crate::tui::app) use modal::resolve_model_target;
pub use modal::visible_models;
// The worker stays reachable as `crate::tui::app::run_agent_worker` for both
// submit paths and for callers outside the module.
#[cfg(test)]
use worker::DoneGuard;
pub use worker::run_agent_worker;

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
}

/// Geometry of the `@` reference popup. Shared so drawing and mouse clicks agree.
pub struct AtPopupLayout {
    pub area: Rect,
    pub window_start: usize,
    pub max_visible: usize,
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
