use ratatui::style::{Color, Style};

use crate::agent::engine::AgentMode;

/// Gray for secondary text: separators, workspace path, key hints.
pub(super) fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Mode tag the header shows, plus the style that paints mode text: the header
/// tag and the footer's mode label. The colour marks what the mode changes:
/// yellow restricts, green stops asking for writes, red stops asking at all.
pub(super) fn mode_tag(mode: AgentMode) -> (&'static str, Style) {
    match mode {
        AgentMode::Build => ("[build]", dim()),
        AgentMode::Plan => ("[plan]", Style::default().fg(Color::Yellow)),
        AgentMode::Ask => ("[ask]", Style::default().fg(Color::Cyan)),
        AgentMode::Review => ("[review]", Style::default().fg(Color::Magenta)),
        AgentMode::AcceptEdits => ("[accept-edits]", Style::default().fg(Color::Green)),
        AgentMode::Auto => ("[auto]", Style::default().fg(Color::Red)),
    }
}
