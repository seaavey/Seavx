use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::theme;
use crate::tui::app::App;
use crate::tui::event::AgentState;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame, millis: u128) {
    let dim = theme::dim();
    let title = app.session_title.as_deref().unwrap_or("new session");
    let (state, state_style) = match &app.status {
        AgentState::Idle => ("ready", dim),
        AgentState::Thinking => ("thinking", Style::default().fg(Color::Yellow)),
        AgentState::Checking => ("checking", Style::default().fg(Color::Yellow)),
        AgentState::Compacting => ("compacting", Style::default().fg(Color::Yellow)),
        AgentState::Streaming => ("streaming", Style::default().fg(Color::Yellow)),
        AgentState::ExecutingTool(tool) => (tool.as_str(), Style::default().fg(Color::Cyan)),
    };
    let orb = Orb::for_state(&app.status);
    let used = title.chars().count() + state.chars().count() + 8;
    let fill = "─".repeat((area.width as usize).saturating_sub(used));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─ ", dim),
            Span::styled(title.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(format!(" {fill} "), dim),
            Span::styled(orb.glyph(millis), Style::default().fg(orb.color())),
            Span::styled(format!(" {state}"), state_style),
            Span::styled(" ─", dim),
        ])),
        area,
    );
}

/// Animated status orb, ported from libraries.dev thinking-orbs (dotted orb, 2D).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Orb {
    Idle,
    Breathing,
    Composing,
    Working,
}

impl Orb {
    fn for_state(state: &AgentState) -> Self {
        match state {
            AgentState::Idle => Orb::Idle,
            AgentState::Thinking => Orb::Breathing,
            AgentState::Checking | AgentState::Compacting => Orb::Working,
            AgentState::Streaming => Orb::Composing,
            AgentState::ExecutingTool(_) => Orb::Working,
        }
    }

    /// Frames per state: ring morph, undulating sash, particles on tilted orbits.
    fn frames(self) -> &'static [&'static str] {
        match self {
            Orb::Idle => &["○"],
            // Face-on ring filling and draining; `○`/`◌` alone read as a still dot.
            Orb::Breathing => &["○", "◔", "◐", "◕", "●", "◕", "◐", "◔"],
            Orb::Composing => &["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█", "▇", "▆", "▅", "▄"],
            Orb::Working => &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
        }
    }

    fn color(self) -> Color {
        match self {
            Orb::Idle => Color::DarkGray,
            _ => Color::Cyan,
        }
    }

    pub(super) fn glyph(self, millis: u128) -> &'static str {
        let frames = self.frames();
        frames[(millis / 90) as usize % frames.len()]
    }
}
