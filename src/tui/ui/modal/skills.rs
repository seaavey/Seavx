use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
};

use super::cursor;
use crate::cli::skills::one_line;

pub(super) fn render(
    area: Rect,
    frame: &mut Frame,
    names: &[String],
    descriptions: &[String],
    selected: usize,
    marked: &[bool],
) {
    // One row per candidate: the mark is what `Enter` installs, the
    // description is the first sentence so a long one cannot wrap.
    let items: Vec<ListItem> = names
        .iter()
        .zip(descriptions)
        .enumerate()
        .map(|(index, (name, description))| {
            let mark = if marked.get(index).copied().unwrap_or(false) {
                "[x]"
            } else {
                "[ ]"
            };
            ListItem::new(format!("{mark} {name} — {}", one_line(description)))
                .style(Style::default().fg(Color::White))
        })
        .collect();

    let title = format!(
        " Install Skills {}/{} marked · space toggle · a all · enter install · esc ",
        marked.iter().filter(|mark| **mark).count(),
        names.len()
    );
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
