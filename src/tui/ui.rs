use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

use crate::tui::app::App;

// The child test module reaches these through `use super::*`.
#[cfg(test)]
use {
    crate::agent::engine::AgentMode,
    crate::tui::app::ModalSelection,
    crate::tui::event::AgentState,
    ratatui::style::{Color, Modifier},
};

mod composer;
mod conversation;
mod footer;
mod header;
mod modal;
mod permission;
mod popups;
mod splash;
mod status_line;
mod theme;
mod toast;

// The child test module reaches these through `use super::*`.
#[cfg(test)]
pub(crate) use {conversation::transcript_lines, splash::SPLASH, status_line::Orb};

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
    toast::render(app, frame.area(), input_area, frame);
}

#[cfg(test)]
#[path = "../tests/tui/ui.rs"]
mod tests;
