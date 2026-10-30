use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::theme;
use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    if let Some(pending) = app.pending_permission.as_ref() {
        let request = &pending.request;
        let key = |label: &'static str, color: Color| {
            Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        };
        let hint = |text: &'static str| Span::styled(text, Style::default().fg(Color::DarkGray));

        let body = vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    request.tool.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", theme::dim()),
                Span::styled(request.effect.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::raw(" "),
                key("y", Color::Green),
                hint(" once   "),
                key("a", Color::Green),
                hint(" this category   "),
                key("u", Color::Yellow),
                hint(" unrestricted   "),
                key("n", Color::Red),
                hint(" deny"),
            ]),
        ];
        let block = Block::default()
            .title(Span::styled(
                " permission ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(Paragraph::new(body).block(block), area);
    }
}
