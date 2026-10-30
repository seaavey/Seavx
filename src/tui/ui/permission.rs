use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::theme;
use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    if let Some(pending) = app.pending_question.as_ref() {
        use crate::session::QuestionKind;
        let hint = |text: &'static str| Span::styled(text, theme::dim());
        let mut body = vec![Line::from(vec![
            Span::raw(" "),
            Span::styled(
                pending.question.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];

        if pending.kind != QuestionKind::Text {
            for (index, option) in pending.options.iter().enumerate() {
                let picked = pending.kind == QuestionKind::Multi && pending.chosen.contains(&index);
                let marker = if index == pending.selected {
                    "❯ "
                } else {
                    "  "
                };
                let box_ = match (pending.kind, picked) {
                    (QuestionKind::Multi, true) => "[x] ",
                    (QuestionKind::Multi, false) => "[ ] ",
                    _ => "",
                };
                let style = if index == pending.selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                body.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(format!("{marker}{box_}{option}"), style),
                ]));
            }
        }

        body.push(Line::from(vec![
            Span::raw(" "),
            hint(match pending.kind {
                QuestionKind::Text => "type the answer   ",
                QuestionKind::Multi => "↑↓ move   space toggle   ",
                QuestionKind::Single => "↑↓ move   ",
            }),
            hint("enter answer   "),
            hint("ctrl+s skip   "),
            hint("esc cancel"),
        ]));

        let block = Block::default()
            .title(Span::styled(
                " question ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        frame.render_widget(
            Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    if let Some(pending) = app.pending_permission.as_ref() {
        let request = &pending.request;
        let key = |label: &'static str, color: Color| {
            Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        };
        let hint = |text: &'static str| Span::styled(text, theme::dim());

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
