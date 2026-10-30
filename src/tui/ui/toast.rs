use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::tui::app::App;

pub(super) fn render(app: &App, area: Rect, prompt: Rect, frame: &mut Frame) {
    if let Some((ref msg, instant)) = app.toast
        && instant.elapsed() < std::time::Duration::from_secs(3)
    {
        let toast_width = (msg.len() as u16 + 4).min(area.width.saturating_sub(2));
        let toast_area = Rect::new(
            area.width.saturating_sub(toast_width + 2),
            prompt.y.saturating_sub(3),
            toast_width,
            3,
        );
        frame.render_widget(Clear, toast_area);
        let toast_color = if msg.starts_with('✓') {
            Color::Green
        } else {
            Color::Red
        };
        let toast_widget = Paragraph::new(msg.as_str())
            .style(Style::default().fg(toast_color))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(toast_color)),
            );
        frame.render_widget(toast_widget, toast_area);
    }
}
