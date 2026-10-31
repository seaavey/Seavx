use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
};

use super::cursor;
use crate::tui::app::App;

pub(super) fn render(
    app: &App,
    area: Rect,
    frame: &mut Frame,
    models: &[String],
    query: &str,
    selected: usize,
    millis: u128,
) {
    // The list is longer than the box: the items are plain text and
    // the stateful render scrolls the window to the selected row.
    // Without it the window stayed pinned to the first rows and the
    // cursor walked off the bottom, so the picker looked frozen.
    let visible = crate::tui::app::visible_models(models, query);
    let items: Vec<ListItem> = visible
        .iter()
        .map(|model| {
            let active = Some(model.as_str()) == app.config.model.as_deref();
            let suffix = if active { " [active]" } else { "" };
            let style = if active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("{model}{suffix}")).style(style)
        })
        .collect();

    let title = if app.refreshing_models {
        // A spinner is the only sign of life while the gateway is
        // slow; without it the frozen picker reads as a hang. The host
        // says whose list is being fetched.
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = FRAMES[(millis / 80) as usize % FRAMES.len()];
        let host = app
            .config
            .base_url
            .as_deref()
            .map(crate::agent::models::ModelManager::domain_slug)
            .unwrap_or_else(|| "no endpoint".to_string());
        format!(" Fetching models from {host} {frame} (Esc: close) ")
    } else {
        // The filter is the picker's search box: type to narrow,
        // Backspace to widen.
        let filter = if query.is_empty() {
            "type to filter".to_string()
        } else {
            format!("filter: {query}")
        };
        format!(
            " Select Model {}/{} · {filter} · ↑/↓ move · ctrl+r refresh · Esc ",
            visible.len(),
            models.len()
        )
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(cursor())
        .highlight_symbol("❯ ");
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}
