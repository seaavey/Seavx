use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::theme;
use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    frame.render_widget(&app.input, area);
}

/// What waits on the next Enter, above the prompt box: a paste that landed is
/// composer state, not history, so it must not scroll away with the transcript.
/// The newest attachments are the ones shown when the strip cannot hold them all.
pub(super) fn render_attachments(app: &App, area: Rect, frame: &mut Frame) {
    let height = area.height as usize;
    let held = app.pending_attachments.len();
    if height == 0 || held == 0 {
        return;
    }
    let lines: Vec<Line> = app
        .pending_attachments
        .iter()
        .enumerate()
        .skip(held.saturating_sub(height))
        .map(|(index, attachment)| {
            Line::from(vec![
                Span::styled(" ▌", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!(" [{}]", attachment.summary(index + 1)),
                    theme::dim(),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}
