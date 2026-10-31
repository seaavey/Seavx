use super::App;
use crate::session::Attachment;

/// A paste above this is cut off: a terminal can emit megabytes at once and the
/// composer needs one prompt, not a file.
const MAX_PASTE_TEXT: usize = 256 * 1024;

/// Pasted text becomes prompt text: `\r\n` and `\r` collapse to `\n`, every C0
/// control but tab and newline is dropped (an ESC would otherwise reach the
/// renderer as a terminal sequence), and the rest is capped. The flag reports
/// that the cap was hit so the caller can say so instead of losing text quietly.
pub(super) fn sanitize(text: &str) -> Option<(String, bool)> {
    let mut clean = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let kept = match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                Some('\n')
            }
            '\n' | '\t' => Some(ch),
            control if control.is_control() => None,
            other => Some(other),
        };
        let Some(kept) = kept else { continue };
        if clean.len() + kept.len_utf8() > MAX_PASTE_TEXT {
            return Some((clean, true));
        }
        clean.push(kept);
    }
    if clean.is_empty() {
        None
    } else {
        Some((clean, false))
    }
}

/// Add one image to the turn being composed. The same bytes twice is one chip:
/// the cap and the wire both count distinct images.
#[allow(dead_code)] // wired by the image-paste path, which lands next
pub(super) fn attach(app: &mut App<'_>, attachment: Attachment) {
    let existing = app
        .pending_attachments
        .iter()
        .position(|held| held.sha256 == attachment.sha256);
    match existing {
        Some(index) => app.push_notice(format!("already attached as #{}", index + 1)),
        None => app.pending_attachments.push(attachment),
    }
}

/// One paste, applied as composer state only: it never submits and never becomes
/// a keystroke.
pub(super) fn handle(app: &mut App<'_>, text: &str) {
    // The gates `keys::handle` applies. A paste arrives off the key path, so it
    // has to ask who owns the keyboard itself.
    if app.pending_question.is_some() || app.pending_permission.is_some() {
        app.push_notice("Paste ignored: answer the prompt on screen first.");
        return;
    }
    if app.modal.is_some() {
        app.push_notice("Paste ignored: close the picker first.");
        return;
    }
    let Some((text, truncated)) = sanitize(text) else {
        return;
    };
    // The ghost is drawn as a live selection: inserting over it leaves its index
    // stale, and the next `clear_ghost` would cut the pasted text.
    app.clear_ghost();
    app.reset_history_nav();
    app.input.insert_str(&text);
    app.refresh_ghost();
    if truncated {
        app.push_notice(format!(
            "Pasted text truncated at {} KiB.",
            MAX_PASTE_TEXT / 1024
        ));
    }
}

#[cfg(test)]
#[path = "../../tests/tui/paste.rs"]
mod tests;
