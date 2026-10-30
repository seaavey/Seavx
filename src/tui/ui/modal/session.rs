use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
};

pub(super) fn render(area: Rect, frame: &mut Frame, rows: &[String], selected: usize) {
    // Same as the model picker: a long list scrolls with the cursor.
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| ListItem::new(row.clone()).style(Style::default().fg(Color::White)))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Select Session (↑/↓: move, Enter: open, Esc: cancel) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}
