use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::modal::{next_index, prev_index};
use super::{App, ModalSelection};

pub(super) fn handle(app: &mut App, mouse: MouseEvent, screen: Rect) {
    let at_matches = app.at_matches();

    // Handle mouse click/scroll on `@` popup if active
    if !app.popups_dismissed() && !at_matches.is_empty() {
        let layout = app.at_popup_layout(screen, at_matches.len());
        let area = layout.area;

        let in_menu_x = mouse.column >= area.x && mouse.column < area.x + area.width;
        let in_menu_y = mouse.row >= area.y && mouse.row < area.y + area.height;

        if in_menu_x && in_menu_y {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    app.at_selected = prev_index(app.at_selected, at_matches.len());
                    return;
                }
                MouseEventKind::ScrollDown => {
                    app.at_selected = next_index(app.at_selected, at_matches.len());
                    return;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let relative_row = mouse.row.saturating_sub(area.y + 1);
                    let clicked_idx = layout.window_start + relative_row as usize;
                    if clicked_idx < at_matches.len() {
                        let (is_dir, path) = &at_matches[clicked_idx];
                        app.insert_at_tag(*is_dir, path);
                        app.at_selected = 0;
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    // An open picker owns the wheel: the transcript behind it
    // is not what the pointer is aiming at.
    if matches!(app.modal, Some(ModalSelection::Model { .. })) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.step_model_picker(false);
                return;
            }
            MouseEventKind::ScrollDown => {
                app.step_model_picker(true);
                return;
            }
            _ => {}
        }
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => app.scroll_up(3),
        MouseEventKind::ScrollDown => app.scroll_down(3),
        _ => {}
    }
}
