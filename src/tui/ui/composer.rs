use ratatui::{Frame, layout::Rect};

use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    frame.render_widget(&app.input, area);
}
