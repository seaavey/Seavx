use ratatui::{
    style::{Color, Style},
    widgets::{Block, BorderType, Borders},
};
use tui_textarea::TextArea;

use super::{App, EnterAction};

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

impl App<'_> {
    /// Enter: a slash line is a command, anything else is a turn. A bare `/`
    /// clears the input and does nothing else — never a notice, never a call.
    pub fn enter_action(&mut self, line: &str) -> EnterAction {
        let prompt = line.trim();
        if prompt.is_empty() {
            // A pending image is enough to send.
            return if self.pending_attachments.is_empty() {
                EnterAction::Nothing
            } else {
                EnterAction::Turn(String::new())
            };
        }
        if !prompt.starts_with('/') {
            return EnterAction::Turn(prompt.to_string());
        }
        self.input = new_input();
        match crate::tui::commands::parse(prompt) {
            Some(command) => EnterAction::Command(command),
            None => EnterAction::Nothing,
        }
    }

    /// Rendered prompt-box height; kept in sync with the vertical layout in `ui::render`.
    pub fn input_height(&self) -> u16 {
        (self.input.lines().len() as u16).clamp(1, 6) + 2
    }

    /// Scroll the transcript up `n` rows, turning off auto-scroll.
    pub fn scroll_up(&mut self, n: u16) {
        if self.auto_scroll {
            self.scroll = self.max_scroll();
        }
        self.auto_scroll = false;
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll the transcript down `n` rows, re-enabling auto-scroll at the bottom.
    pub fn scroll_down(&mut self, n: u16) {
        // Already pinned to the newest rows: nothing below them to reach.
        if self.auto_scroll {
            return;
        }
        let bottom = self.max_scroll();
        let next = self.scroll.saturating_add(n);
        if next >= bottom {
            self.scroll = bottom;
            self.auto_scroll = true;
        } else {
            self.scroll = next;
        }
    }

    /// Current prompt as a single trimmed string.
    pub fn input_text(&self) -> String {
        self.input.lines().join("\n").trim().to_string()
    }
}
