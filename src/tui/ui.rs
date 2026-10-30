use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

// The header tests name the mode through `use super::*`.
#[cfg(test)]
use crate::agent::engine::AgentMode;
use crate::tui::app::{App, ModalSelection};
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
    if let Some(ref modal) = app.modal {
        let area = centered_rect(60, 50, frame.area());
        frame.render_widget(Clear, area);

        match modal {
            ModalSelection::Model {
                models,
                query,
                selected,
            } => {
                // The list is longer than the box: the items are plain text and
                // the stateful render scrolls the window to the selected row.
                // Without it the window stayed pinned to the first rows and the
                // cursor walked off the bottom, so the picker looked frozen.
                let visible = crate::tui::app::visible_models(models, query);
                let items: Vec<ListItem> = visible
                    .iter()
                    .map(|model| {
                        let suffix = if model.as_str() == app.config.model {
                            " [active]"
                        } else {
                            ""
                        };
                        let style = if model.as_str() == app.config.model {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        ListItem::new(format!("{model}{suffix}")).style(style)
                    })
                    .collect();

                let title = if app.refreshing_models {
                    // A spinner is the only sign of life while the gateway is
                    // slow; without it the frozen picker reads as a hang.
                    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                    let frame = FRAMES[(millis / 80) as usize % FRAMES.len()];
                    format!(" Refreshing /models {frame} (Esc: close) ")
                } else {
                    // The filter is the picker's search box: type to narrow,
                    // Backspace to widen.
                    let filter = if query.is_empty() {
                        "type to filter".to_string()
                    } else {
                        format!("filter: {query}")
                    };
                    format!(
                        " Select Model {}/{} · {filter} · ↑/↓ move · ctrl+r refresh · Esc ",
                        visible.len(),
                        models.len()
                    )
                };
                let list = List::new(items)
                    .block(
                        Block::default()
                            .title(title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    // A background, not just a color: the cursor must survive a
                    // font that has no ❯ glyph.
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(list, area, &mut state);
            }
            ModalSelection::Effort {
                model,
                options,
                selected,
            } => {
                let current_opt = options.get(*selected).copied().unwrap_or("none");
                let border_color = effort_color(current_opt, millis);

                let items: Vec<ListItem> = options
                    .iter()
                    .enumerate()
                    .map(|(i, opt)| {
                        let prefix = if i == *selected { "❯ " } else { "  " };
                        let style = if i == *selected {
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
            ModalSelection::Session { rows, selected } => {
                // Same as the model picker: a long list scrolls with the cursor.
                let items: Vec<ListItem> = rows
                    .iter()
                    .map(|row| ListItem::new(row.clone()).style(Style::default().fg(Color::White)))
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .title(" Select Session (↑/↓: move, Enter: open, Esc: cancel) ")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(list, area, &mut state);
            }
        }
    }

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

#[cfg(test)]
#[path = "../tests/tui/ui.rs"]
mod tests;
