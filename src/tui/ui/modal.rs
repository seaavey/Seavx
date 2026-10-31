use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::Clear,
};

use crate::tui::app::{App, ModalSelection};

mod effort;
mod model;
mod preset;
mod session;
mod skills;

/// Cursor look for a selectable list: a background, not just a colour, so it
/// survives a terminal font that has no `❯` glyph.
pub(super) fn cursor() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame, millis: u128) {
    let Some(modal) = app.modal.as_ref() else {
        return;
    };
    let area = centered_rect(60, 50, area);
    frame.render_widget(Clear, area);

    match modal {
        ModalSelection::Model {
            models,
            query,
            selected,
        } => model::render(app, area, frame, models, query, *selected, millis),
        ModalSelection::Effort {
            model,
            options,
            selected,
        } => effort::render(area, frame, model, options, *selected, millis),
        ModalSelection::Preset {
            names,
            previews,
            query,
            selected,
        } => preset::render(area, frame, names, previews, query, *selected),
        ModalSelection::Session { rows, selected } => session::render(area, frame, rows, *selected),
        ModalSelection::Skills {
            names,
            descriptions,
            selected,
            marked,
        } => skills::render(area, frame, names, descriptions, *selected, marked),
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
