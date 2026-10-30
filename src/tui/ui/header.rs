use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::theme;
use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    let dim = theme::dim();
    let (tag, tag_style) = theme::mode_tag(app.mode);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "seavx",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {tag}"), tag_style),
            Span::styled("  ", dim),
            Span::styled(app.workspace.as_str(), dim),
        ])),
        area,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(concat!("v", env!("CARGO_PKG_VERSION")), dim))
            .alignment(Alignment::Right),
        area,
    );
}
