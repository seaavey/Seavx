use super::{App, new_input};

impl App<'_> {
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

    pub(super) fn reset_history_nav(&mut self) {
        self.history_cursor = None;
        self.history_view.clear();
        self.history_draft.clear();
    }

    pub(crate) fn set_input(&mut self, text: &str) {
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
}
