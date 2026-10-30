use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem},
};

pub(super) fn render(
    area: Rect,
    frame: &mut Frame,
    model: &str,
    options: &[&'static str],
    selected: usize,
    millis: u128,
) {
    let current_opt = options.get(selected).copied().unwrap_or("none");
    let border_color = effort_color(current_opt, millis);

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let prefix = if i == selected { "❯ " } else { "  " };
            let style = if i == selected {
                let opt_color = effort_color(opt, millis);
                Style::default().fg(opt_color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("{prefix}{opt}")).style(style)
        })
        .collect();

    let title = format!(" Select Reasoning Effort for {model} ");
    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );
    frame.render_widget(list, area);
}

/// Color for a reasoning-effort level; `max` cycles through a rainbow each tick.
fn effort_color(level: &str, millis: u128) -> Color {
    match level {
        "none" => Color::DarkGray,
        "minimal" => Color::Blue,
        "medium" => Color::Cyan,
        "high" => Color::Green,
        "highx" => Color::Magenta,
        "max" => {
            const RAINBOW: [Color; 7] = [
                Color::Red,
                Color::Rgb(255, 127, 0), // Orange
                Color::Yellow,
                Color::Green,
                Color::Cyan,
                Color::Blue,
                Color::Magenta,
            ];
            RAINBOW[((millis / 150) as usize) % RAINBOW.len()]
        }
        _ => Color::Yellow,
    }
}
