use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
};

use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    let matches = app.slash_matches();
    if !matches.is_empty() && !app.popups_dismissed() {
        // Two border rows plus one row per command; the table is short enough to fit.
        let menu_height = matches.len() as u16 + 2;
        let menu_width = 62.min(frame.area().width.saturating_sub(4));
        let menu_y = area.y.saturating_sub(menu_height);
        let menu_area = Rect::new(area.x + 2, menu_y, menu_width, menu_height);

        frame.render_widget(Clear, menu_area);

        let selected = app.slash_selected.min(matches.len() - 1);
        let items: Vec<ListItem> = matches
            .iter()
            .enumerate()
            .map(|(i, (cmd, desc))| {
                let is_sel = i == selected;
                let prefix = if is_sel { "> " } else { "  " };
                let cmd_style = if is_sel {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let line = Line::from(vec![
                    Span::styled(format!("{prefix}{cmd:<12}"), cmd_style),
                    Span::raw(" "),
                    Span::styled(*desc, Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let menu_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(List::new(items).block(menu_block), menu_area);
    }

    let matches = app.at_matches();
    if !matches.is_empty() && !app.popups_dismissed() {
        let layout = app.at_popup_layout(frame.area(), matches.len());
        let menu_area = layout.area;
        frame.render_widget(Clear, menu_area);

        let selected = app.at_selected.min(matches.len() - 1);
        // Sliding viewport calculation for auto-scrolling
        let window_start = layout.window_start;
        let window_end = (window_start + layout.max_visible).min(matches.len());

        let items: Vec<ListItem> = matches[window_start..window_end]
            .iter()
            .enumerate()
            .map(|(i, (is_dir, path))| {
                let actual_idx = window_start + i;
                let is_sel = actual_idx == selected;
                let prefix = if is_sel { "> " } else { "  " };
                let (kind_tag, kind_color) = if *is_dir {
                    ("[folder]", Color::Blue)
                } else {
                    ("[file]  ", Color::Green)
                };

                let line = Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Cyan)),
                    Span::styled(kind_tag, Style::default().fg(kind_color)),
                    Span::raw(" "),
                    Span::styled(
                        path,
                        if is_sel {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();

        let title = if matches.len() > layout.max_visible {
            format!(
                " Reference File/Folder [{}/{}] (↑/↓: scroll, Tab/Enter: insert) ",
                selected + 1,
                matches.len()
            )
        } else {
            " Reference File/Folder (↑/↓: move, Tab/Enter: insert) ".to_string()
        };

        let menu_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(List::new(items).block(menu_block), menu_area);
    }
}
