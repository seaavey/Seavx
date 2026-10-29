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
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, BorderType, Borders},
};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::agent::engine::{AgentMode, EFFORT_LEVELS};
use crate::tui::event::{Action, AgentState};
use crate::tui::ui;

const MAX_COMPLETION_RESULTS: usize = 20;

pub fn scan_workspace_entries(query: &str) -> Vec<(bool, String)> {
    scan_workspace_entries_at(std::path::Path::new("."), query)
}

/// Root-aware core of [`scan_workspace_entries`]. Tests scan an isolated tree
/// without changing the process working directory.
fn scan_workspace_entries_at(root: &std::path::Path, query: &str) -> Vec<(bool, String)> {
    let clean_query = query.trim_start_matches('@').to_lowercase();
    let mut results = Vec::new();

    fn walk_dir(
        dir: &std::path::Path,
        root: &std::path::Path,
        results: &mut Vec<(bool, String)>,
        query: &str,
    ) {
        if results.len() >= MAX_COMPLETION_RESULTS {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if results.len() >= MAX_COMPLETION_RESULTS {
                break;
            }
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if crate::util::is_skipped(name) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            // `DirEntry::file_type` does not follow symlinks: a linked directory
            // is listed as a candidate but never traversed, so it cannot expose
            // names outside the workspace or walk a cycle.
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if query.is_empty() || rel.to_lowercase().contains(query) {
                results.push((is_dir, rel));
            }
            if is_dir {
                walk_dir(&path, root, results, query);
            }
        }
    }

    walk_dir(root, root, &mut results, &clean_query);
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
    ("/compact", "Summarize the session and free context"),
    ("/config", "Show or edit effective configuration"),
    ("/plan", "Enter plan mode (read-only)"),
    ("/clear", "Clear session conversation output"),
    ("/help", "Show help and command palette"),
    ("/init", "Initialize workspace configuration"),
    ("/doctor", "Check environment and health"),
    ("/memory", "Inspect persistent memory"),
    ("/undo", "Restore files from the latest checkpoint"),
    ("/exit", "Quit Seavx agent TUI"),
];

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

/// Cached `@` query and its matches, so rendering never walks the workspace.
type AtCache = Option<(String, Vec<(bool, String)>)>;

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
    pub config: crate::agent::engine::AgentConfig,
    pub mode: AgentMode,
    pub modal: Option<ModalSelection>,
    pub slash_selected: usize,
    pub at_selected: usize,
    /// `@` matches for the current query; rendering must not walk the workspace
    /// once per frame.
    at_cache: std::cell::RefCell<AtCache>,
    /// Prompt text whose popups the user already closed with Esc.
    dismissed_popup: Option<String>,
    pub toast: Option<(String, std::time::Instant)>,
    pub last_ctrl_c: Option<std::time::Instant>,
}

/// Single source for the prompt textarea so resets keep its styling.
pub fn new_input<'a>() -> TextArea<'a> {
    let mut input = TextArea::default();
    input.set_cursor_line_style(Style::default());
    // Ghost-completions are drawn with the dim selection style. No selection
    // exists elsewhere in the app, so this style is never seen otherwise.
    input.set_selection_style(Style::default().fg(Color::DarkGray));
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
            config,
            mode: AgentMode::Build,
            modal: None,
            slash_selected: 0,
            at_selected: 0,
            at_cache: std::cell::RefCell::new(None),
            dismissed_popup: None,
            toast: None,
            last_ctrl_c: None,
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
        if !self.set_mode(mode) && self.mode != mode {
            self.push_mode_notice("Cannot switch mode while a task is running");
        }
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
        self.session_context = None;
    }

    /// Replace the transcript with the compacted session block and remember
    /// the summary for later requests.
    pub fn apply_compaction(&mut self, result: crate::tui::compact::CompactResult) {
        self.session_context = Some(result.summary.clone());
        self.revision += 1;

        let text = format!(
            "Session compacted\n\nBefore: ~{} lines, ~{} tokens (estimated)\nAfter:  ~{} lines, ~{} tokens (estimated)\n\n{}",
            result.before_lines,
            result.before_chars.div_ceil(4),
            result.after_lines,
            result.after_chars.div_ceil(4),
            result.summary
        );
        self.segments = vec![Segment::Text(text)];
        self.scroll = 0;
        self.auto_scroll = true;
    }

    /// Append a command report, stripped of terminal control characters.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        let text = crate::tui::commands::sanitize(&text.into());
        self.push_text(&format!("\n{text}\n"));
        self.scroll = self.line_count();
        self.auto_scroll = true;
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
            selected: current_idx,
        });
        true
    }

    /// Switch the model, optionally with a reasoning effort, and persist it.
    pub fn set_model(&mut self, target: &str, effort: Option<&str>) {
        let models = cached_models(self);
        let resolved_model = if let Ok(num) = target.parse::<usize>()
            && num > 0
            && num <= models.len()
        {
            models[num - 1].clone()
        } else {
            target.to_string()
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

    /// Remember a submitted prompt; slash commands and repeats are not worth recalling.
    /// Every kept prompt is appended to `.seavx/svx_history` so it survives
    /// restarts and feeds cross-session autocomplete.
    pub fn remember_prompt(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        if prompt.is_empty() || prompt.starts_with('/') {
            return;
        }
        if self.history.last().map(String::as_str) != Some(prompt) {
            self.history.push(prompt.to_string());
            self.history_file.append(prompt);
            self.history_mtime = self.history_file.modified();
        }
        self.reset_history_nav();
    }

    /// Re-read `.seavx/svx_history` when it changed on disk, so manually
    /// added prompts (and prompts from other sessions) feed autocomplete without
    /// restarting the TUI.
    pub fn sync_history(&mut self) {
        let modified = self.history_file.modified();
        if modified == self.history_mtime {
            return;
        }
        self.history = self.history_file.entries();
        self.history_mtime = modified;
        self.reset_history_nav();

        // Regrow the ghost from the new list instead of leaving a stale tail.
        if self.ghost_active() {
            self.clear_ghost();
            self.refresh_ghost();
        }
    }

    /// The typed prefix (everything left of the cursor) of a single-line prompt.
    fn typed_prefix(&self) -> Option<String> {
        let lines = self.input.lines();
        if lines.len() != 1 {
            return None;
        }
        let (row, col) = self.input.cursor();
        if row != 0 {
            return None;
        }
        Some(lines[0].chars().take(col).collect())
    }

    /// Newest history entry branching from the typed text, for ghost autocomplete.
    /// Only offered at the end of a single-line prompt (where the user types),
    /// hidden while arrow browsing history or nothing is typed yet. Multi-line
    /// entries are skipped: their tail cannot be taken back once inserted.
    pub fn history_suggestion(&self) -> Option<String> {
        if self.history_cursor.is_some() {
            return None;
        }
        let lines = self.input.lines();
        if lines.len() != 1 {
            return None;
        }
        let (row, col) = self.input.cursor();
        if row != 0 || col != lines[0].chars().count() {
            return None;
        }
        let typed = lines[0].clone();
        if typed.is_empty() {
            return None;
        }
        self.history
            .iter()
            .rev()
            .find(|entry| {
                entry.as_str() != typed.as_str()
                    && entry.starts_with(&typed)
                    && !entry.contains('\n')
            })
            .cloned()
    }

    /// Whether the ghost suggestion currently fits the prompt box: the cursor
    /// cell plus the suggestion must leave room inside the bordered box.
    pub fn suggestion_fits(&self, suggestion: &str) -> bool {
        let interior = self.terminal_width.saturating_sub(2) as usize;
        // Display cells, not chars: CJK and emoji take two columns each. The
        // cursor sits past the last typed cell, so the text must stay narrower.
        unicode_width::UnicodeWidthStr::width(suggestion) < interior
    }

    /// Recall an older prompt, filtered by whatever is already typed.
    pub fn history_previous(&mut self) {
        if self.history_view.is_empty() {
            let Some(typed) = self.typed_prefix() else {
                return;
            };
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
        self.ghost_typed = None;
        self.input = new_input();
        self.input.insert_str(text);
    }

    /// Dim the ghost tail: select from `col` (end of the typed part) to the
    /// end of the single-line prompt so the tail characters render dimmed.
    fn dim_tail(&mut self, col: usize) {
        self.input.cancel_selection();
        let lines = self.input.lines().to_vec();
        if lines.len() != 1 || col >= lines[0].chars().count() {
            return;
        }
        self.input.move_cursor(tui_textarea::CursorMove::End);
        self.input.start_selection();
        self.input
            .move_cursor(tui_textarea::CursorMove::Jump(0, col as u16));
    }

    /// Whether a dimmed suggestion tail is currently shown.
    pub fn ghost_active(&self) -> bool {
        self.ghost_typed.is_some()
    }

    /// Drop the ghost tail while keeping the typed text and cursor position.
    pub fn clear_ghost(&mut self) {
        let Some(start) = self.ghost_typed.take() else {
            self.input.cancel_selection();
            return;
        };
        self.input.cancel_selection();
        let lines = self.input.lines().to_vec();
        if lines.len() == 1 && lines[0].chars().count() > start {
            let typed: String = lines[0].chars().take(start).collect();
            self.set_input(&typed);
        }
    }

    /// Keep the ghost suggestion in step with the prompt after a key edit.
    /// The caller clears the stale tail first, applies the raw key, then calls
    /// this: the newest history entry matching the typed prefix grows back as
    /// dimmed text past the cursor for the current single line.
    pub fn refresh_ghost(&mut self) {
        self.input.cancel_selection();
        let lines = self.input.lines().to_vec();
        let (row, col) = self.input.cursor();
        if lines.len() != 1 || row != 0 || col != lines[0].chars().count() {
            // Mid-line cursors and multi-line prompts never carry suggestions.
            return;
        }

        let Some(suggestion) = self.history_suggestion() else {
            return;
        };
        if !self.suggestion_fits(&suggestion) {
            return;
        }
        let tail: String = suggestion.chars().skip(col).collect();
        if tail.is_empty() {
            return;
        }
        self.input.insert_str(tail);
        self.ghost_typed = Some(col);
        self.dim_tail(col);
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

    /// Rendered prompt-box height; kept in sync with the vertical layout in `ui::render`.
    pub fn input_height(&self) -> u16 {
        (self.input.lines().len() as u16).clamp(1, 6) + 2
    }

    /// Geometry of the `@` reference popup, shared by rendering and mouse hit-testing.
    pub fn at_popup_layout(&self, area: Rect, match_count: usize) -> AtPopupLayout {
        const MAX_VISIBLE: usize = 8;

        let menu_height = (match_count.min(MAX_VISIBLE) as u16 + 2).min(10);
        let menu_width = 70.min(area.width.saturating_sub(4));
        // Prompt box starts where the footer (1) and prompt (input_height) leave room.
        let prompt_y = area.height.saturating_sub(self.input_height() + 2);
        let menu_y = prompt_y.saturating_sub(menu_height);
        let selected = self.at_selected.min(match_count.saturating_sub(1));

        AtPopupLayout {
            area: Rect::new(area.x + 2, menu_y, menu_width, menu_height),
            window_start: selected.saturating_sub(MAX_VISIBLE - 1),
            max_visible: MAX_VISIBLE,
        }
    }

    /// Scroll the transcript up `n` lines, turning off auto-scroll.
    pub fn scroll_up(&mut self, n: u16) {
        let lines = self.line_count();
        if self.auto_scroll {
            self.scroll = lines;
        }
        self.auto_scroll = false;
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll the transcript down `n` lines, re-enabling auto-scroll at the bottom.
    pub fn scroll_down(&mut self, n: u16) {
        let lines = self.line_count();
        let next = self.scroll.saturating_add(n);
        if next >= lines {
            self.scroll = lines;
            self.auto_scroll = true;
        } else {
            self.scroll = next;
        }
    }

    /// Current prompt as a single trimmed string.
    pub fn input_text(&self) -> String {
        self.input.lines().join("\n").trim().to_string()
    }

    /// Trailing `@word` in the prompt, if any.
    pub fn at_query(&self) -> Option<String> {
        self.input_text()
            .split_whitespace()
            .last()
            .filter(|word| word.starts_with('@'))
            .map(str::to_string)
    }

    /// Workspace entries matching the trailing `@word`, scanned once per query
    /// so neither key handling nor rendering repeats the tree walk.
    pub fn at_matches(&self) -> Vec<(bool, String)> {
        let Some(query) = self.at_query() else {
            return Vec::new();
        };
        if let Some((cached_query, matches)) = self.at_cache.borrow().as_ref()
            && *cached_query == query
        {
            return matches.clone();
        }
        let matches = scan_workspace_entries(&query);
        *self.at_cache.borrow_mut() = Some((query, matches.clone()));
        matches
    }

    /// Slash commands matching the prompt; empty unless it is a bare `/word`.
    pub fn slash_matches(&self) -> Vec<(&'static str, &'static str)> {
        let input = self.input_text();
        if !input.starts_with('/') || input.contains(' ') {
            return Vec::new();
        }
        SLASH_COMMANDS
            .iter()
            .copied()
            .filter(|(cmd, _)| cmd.starts_with(&input))
            .collect()
    }

    /// Whether the user closed the popups for exactly this prompt text; typing
    /// anything else brings them back.
    pub fn popups_dismissed(&self) -> bool {
        self.dismissed_popup.as_deref() == Some(self.input_text().as_str())
    }

    /// Hide the slash and `@` popups until the prompt text changes.
    pub fn dismiss_popups(&mut self) {
        self.dismissed_popup = Some(self.input_text());
    }

    /// Whether any autocomplete popup is showing, so Esc closes it first.
    pub fn popup_open(&self) -> bool {
        !self.popups_dismissed()
            && (!self.at_matches().is_empty() || !self.slash_matches().is_empty())
    }

    /// Replace the trailing `@word` with a `[folder:…]` or `[file:…]` tag.
    pub fn insert_at_tag(&mut self, is_dir: bool, path: &str) {
        let tag = if is_dir {
            format!("[folder:{path}] ")
        } else {
            format!("[file:{path}] ")
        };
        let full_input = self.input_text();
        if let Some(last_at_idx) = full_input.rfind('@') {
            let mut new_text = full_input[..last_at_idx].to_string();
            new_text.push_str(&tag);
            // The ghost tail belongs to the prompt being replaced; keeping the
            // index would let the next keypress truncate the new tag.
            self.ghost_typed = None;
            self.input = new_input();
            self.input.insert_str(new_text);
        }
    }
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
        CommandEffect::RunUndo => {
            app.status = AgentState::Checking;
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let result =
                    crate::checkpoint::CheckpointStore::undo_latest(std::path::Path::new("."))
                        .map_err(|error| {
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
fn cached_models(app: &App) -> Vec<String> {
    let base_url = app
        .config
        .base_url
        .as_deref()
        .unwrap_or(crate::agent::engine::DEFAULT_BASE_URL);
    let model_mgr = crate::agent::models::ModelManager::new(std::path::Path::new("."));
    model_mgr
        .get_cached_models(&crate::agent::models::ModelManager::domain_slug(base_url))
        .unwrap_or_default()
}

pub async fn run_agent_worker(
    prompt: String,
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
                    let clean = crate::util::unquote(text.trim()).to_string();
                    if !clean.is_empty() {
                        let _ = title_tx.send(Action::AgentTitle(clean));
                    }
                }
            });
        }

        let tx_clone = tx.clone();
        let task = crate::tui::compact::build_agent_task(&prompt, session_context.as_deref());
        let result = runtime
            .execute_task(
                &task,
                mode,
                &crate::agent::permissions::DenyingConfirmer,
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
                    } else if evt.starts_with("Tool: ") {
                        let tool_label = evt.strip_prefix("Tool: ").unwrap_or(evt);
                        let _ = tx_clone.send(Action::AgentToolStart(tool_label.to_string()));
                    } else if let Some(thoughts) = evt.strip_prefix("THINKING:") {
                        let _ = tx_clone.send(Action::AgentThinking(thoughts.to_string()));
                    } else if let Some(note) = evt.strip_prefix("System: ") {
                        let _ = tx_clone.send(Action::AgentToken(format!("\n  {note}\n")));
                    }
                },
            )
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

    // Trigger initial prompt if provided via CLI args
    if let Some(prompt) = initial_prompt
        && !prompt.trim().is_empty()
    {
        app.remember_prompt(&prompt);
        app.push_text(&format!("User: {prompt}\n\n"));
        app.status = AgentState::Thinking;
        run_agent_worker(prompt, tx.clone(), true, AgentMode::Build, None).await;
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

                    match mouse.kind {
                        crossterm::event::MouseEventKind::ScrollUp => app.scroll_up(3),
                        crossterm::event::MouseEventKind::ScrollDown => app.scroll_down(3),
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
                                    let new_sel = prev_index(selected, models.len());
                                    app.modal = Some(ModalSelection::Model {
                                        models,
                                        selected: new_sel,
                                    });
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let new_sel = next_index(selected, models.len());
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
                        KeyCode::Esc => {
                            // Close what is on screen before ending the session.
                            if app.ghost_active() {
                                app.clear_ghost();
                            } else if app.popup_open() {
                                app.dismiss_popups();
                            } else {
                                app.should_quit = true;
                            }
                        }
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
                            let prompt = lines.join("\n").trim().to_string();
                            if !prompt.is_empty() {
                                if prompt.starts_with('/') {
                                    app.input = new_input();
                                    let command = crate::tui::commands::parse(&prompt)
                                        .unwrap_or_else(|| {
                                            crate::tui::commands::SlashCommand::Unknown(
                                                prompt.clone(),
                                            )
                                        });
                                    let effect = crate::tui::commands::execute(&mut app, command);
                                    apply_command_effect(&mut app, effect, &tx);
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
                                run_agent_worker(
                                    prompt,
                                    tx.clone(),
                                    is_first_turn,
                                    app.mode,
                                    app.session_context.clone(),
                                )
                                .await;
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
                Action::DoctorDone(report) => {
                    app.status = AgentState::Idle;
                    app.push_notice(crate::cli::doctor::render(&report));
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
