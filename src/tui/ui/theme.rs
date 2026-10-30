use ratatui::style::{Color, Style};

use crate::agent::engine::AgentMode;

/// Gray for secondary text: separators, workspace path, key hints.
pub(super) fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Mode tag the header shows, plus the style that paints mode text: the header
/// tag and the footer's mode label.
pub(super) fn mode_tag(mode: AgentMode) -> (&'static str, Style) {
    match mode {
        AgentMode::Build => ("[build]", dim()),
        AgentMode::Plan => ("[plan]", Style::default().fg(Color::Yellow)),
    }
}
