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
    // The footer paints the mode label; only the style carries over.
    let (_, mode_style) = theme::mode_tag(app.mode);
    let config = &app.config;
    let effort_label = config.reasoning_effort.as_deref().unwrap_or("none");

    // One table, shared with the budget and `doctor`.
    let limit = crate::agent::engine::model_context_limit(config.model_name());
    let max_context_limit = limit.tokens;
    let assumed = if limit.assumed { " (assumed)" } else { "" };

    let total = app.total_tokens.max(app.prompt_tokens);

    let token_display = if max_context_limit >= 1_000_000 {
        if total >= 1_000 {
            format!("~{}k/{}M", total / 1_000, max_context_limit / 1_000_000)
        } else {
            format!("{total}/{}M", max_context_limit / 1_000_000)
        }
    } else if total >= 1_000 {
        format!("~{}k/{}k", total / 1_000, max_context_limit / 1_000)
    } else {
        format!("{total}/{}k", max_context_limit / 1_000)
    };

    // Hints step down rather than push the token count off the row: the counter
    // is data, the key list is a reminder.
    const HINTS: [&str; 3] = [
        "↑↓ history  ·  tab complete  ·  ⏎ send  ·  esc esc clear  ·  ctrl+c exit",
        "↑↓ history  ·  ⏎ send  ·  ctrl+c exit",
        "⏎ send · ctrl+c",
    ];
    const SEPARATOR: &str = "  ·  ";

    let mut model = config.model.as_deref().unwrap_or("no model").to_string();
    let left_width = |model: &str| {
        app.mode.label().chars().count()
            + SEPARATOR.chars().count()
            + model.chars().count()
            + SEPARATOR.chars().count()
            + token_display.chars().count()
            + assumed.chars().count()
            + SEPARATOR.chars().count()
            + format!("effort {effort_label}").chars().count()
    };
    let hints = HINTS
        .iter()
        .find(|hints| left_width(&model) + hints.chars().count() + 2 <= area.width as usize)
        .copied()
        .unwrap_or("");
    // Last resort on a pane too narrow for both: the model gives up its tail.
    if hints.is_empty() && left_width(&model) > area.width as usize {
        let over = left_width(&model) - area.width as usize;
        let keep = model.chars().count().saturating_sub(over + 1);
        model = format!("{}…", model.chars().take(keep).collect::<String>());
    }

    let separator = Span::styled(SEPARATOR, dim);
    let reserved = if hints.is_empty() {
        0
    } else {
        hints.chars().count() as u16 + 2
    };
    let left_area = Rect {
        width: area.width.saturating_sub(reserved),
        ..area
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(app.mode.label(), mode_style),
            separator.clone(),
            Span::styled(
                model,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            separator.clone(),
            Span::styled(
                format!("{token_display}{assumed}"),
                Style::default().fg(Color::Cyan),
            ),
            separator,
            Span::styled(format!("effort {effort_label}"), dim),
        ])),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(hints, dim)).alignment(Alignment::Right),
        area,
    );
}
