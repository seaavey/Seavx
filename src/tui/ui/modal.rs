use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Clear,
};

use crate::tui::app::{App, ModalSelection};

mod effort;
mod model;
mod session;

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
        ModalSelection::Session { rows, selected } => session::render(area, frame, rows, *selected),
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
