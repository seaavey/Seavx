use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use super::cursor;

/// A searchable list with the selected preset's first lines under it, so the
/// user reads the text before it lands in the prompt.
pub(super) fn render(
    area: Rect,
    frame: &mut Frame,
    names: &[String],
    previews: &[String],
    query: &str,
    selected: usize,
) {
    let halves = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let visible = crate::tui::app::visible_presets(names, query);
    let items: Vec<ListItem> = visible
        .iter()
        .map(|index| ListItem::new(names[*index].clone()).style(Style::default().fg(Color::White)))
        .collect();
    let title = if query.is_empty() {
        " Presets (type to search, ↑/↓: move, Enter: apply, Esc: cancel) ".to_string()
    } else {
        format!(" Presets /{query} ")
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
    let mut state = ListState::default().with_selected((!visible.is_empty()).then_some(selected));
    frame.render_stateful_widget(list, halves[0], &mut state);

    let preview = visible
        .get(selected)
        .map(|index| previews[*index].as_str())
        .unwrap_or("No preset matches this search.");
    frame.render_widget(
        Paragraph::new(preview)
            .block(
                Block::default()
                    .title(" Preview ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: false }),
        halves[1],
    );
}
