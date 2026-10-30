use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};

use super::theme;
use crate::tui::app::App;

/// Splash art for an empty session; 44 columns so it fits standard terminals.
pub(crate) const SPLASH: [&str; 5] = [
    " ██████  ████████   ████   ██    ██ ██    ██",
    "██       ██        ██  ██  ██    ██  ██  ██ ",
    " █████   ██████   ████████ ██    ██   ████  ",
    "      ██ ██       ██    ██  ██  ██   ██  ██ ",
    "██████   ████████ ██    ██   ████   ██    ██",
];

/// Centered SEAVX splash, degrading to a single centered word on narrow panes.
pub(super) fn render(_app: &App, area: Rect, frame: &mut Frame) {
    let style = theme::dim();
    let art_width = SPLASH[0].chars().count() as u16;
    if area.width < art_width || area.height < SPLASH.len() as u16 {
        frame.render_widget(
            Paragraph::new("SEAVX")
                .alignment(Alignment::Center)
                .style(style),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }

    let slot = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(SPLASH.len() as u16),
            Constraint::Fill(1),
        ])
        .split(area)[1];
    let lines: Vec<Line> = SPLASH.iter().map(|row| Line::styled(*row, style)).collect();
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), slot);
}
