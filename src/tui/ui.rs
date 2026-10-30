use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

// The header tests name the mode through `use super::*`.
#[cfg(test)]
use crate::agent::engine::AgentMode;
use crate::tui::app::App;
// The modal tests name the selection and the highlight through `use super::*`.
#[cfg(test)]
use crate::tui::app::ModalSelection;
#[cfg(test)]
use ratatui::style::Modifier;
// The status-line tests name the agent state through `use super::*`.
#[cfg(test)]
use crate::tui::event::AgentState;

mod composer;
mod conversation;
mod footer;
// The child test module reaches both through `use super::*`.
#[cfg(test)]
pub(crate) use conversation::transcript_lines;
mod header;
mod modal;
mod permission;
mod popups;
mod splash;
#[cfg(test)]
pub(crate) use splash::SPLASH;
mod status_line;
// The child test module reaches this through `use super::*`.
#[cfg(test)]
pub(crate) use status_line::Orb;
mod theme;

pub fn render(app: &App, frame: &mut Frame) {
    let input_height = app.input_height();
    // The permission box takes rows only while a question is on screen, so the
    // transcript keeps the height it had before the agent asked.
    let permission_height = if app.pending_permission.is_some() {
        4
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                 // Header: name, workspace, version
            Constraint::Min(3),                    // Conversation
            Constraint::Length(1),                 // Session title and agent state
            Constraint::Length(permission_height), // Permission question, when asked
            Constraint::Length(input_height),      // Prompt box
            Constraint::Length(1),                 // Footer: model, tokens, keys
        ])
        .split(frame.area());
    let header = chunks[0];
    let conversation = chunks[1];
    let status_bar = chunks[2];
    let permission_bar = chunks[3];
    let input_area = chunks[4];
    let footer = chunks[5];

    // 1. Header
    header::render(app, header, frame);

    let millis = crate::util::epoch_millis();

    // 2. Conversation
    conversation::render(app, conversation, frame);

    // 3. Session title on the left, agent state on the right
    status_line::render(app, status_bar, frame, millis);

    // 4. Permission question: its own box above the composer, the tool and its
    // effect on one row, the four answers on the next.
    permission::render(app, permission_bar, frame);
    composer::render(app, input_area, frame);

    // 4b. Autocomplete overlays: slash menu, then `@` file/folder popup
    popups::render(app, input_area, frame);

    // 5. Footer: model and context on the left, key hints on the right
    footer::render(app, footer, frame);

    // 6. Modal Selection Overlay (Model / Effort)
    modal::render(app, frame.area(), frame, millis);

    // 7. Toast Notification Popup (Bottom-Right)
    if let Some((ref msg, instant)) = app.toast
        && instant.elapsed() < std::time::Duration::from_secs(3)
    {
        let toast_width = (msg.len() as u16 + 4).min(frame.area().width.saturating_sub(2));
        let toast_area = Rect::new(
            frame.area().width.saturating_sub(toast_width + 2),
            input_area.y.saturating_sub(3),
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

#[cfg(test)]
#[path = "../tests/tui/ui.rs"]
mod tests;
