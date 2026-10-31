use super::{App, clipboard};
use crate::session::{Attachment, PasteSource, size_label};

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

/// Caps on one pasted image: 24 MP is ~96 MiB decoded, 8192 px is the widest a
/// screenshot tool produces, and 4 MiB is what one turn can carry per image.
const MAX_IMAGE_EDGE: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 24_000_000;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Add one image to the turn being composed. The same bytes twice is one chip:
/// the cap and the wire both count distinct images.
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
    if refused(app) {
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

/// One pasted image: normalized, capped, deduped, then announced — a paste that
/// lands leaves a mark even before the chip row exists.
pub(super) fn handle_image(app: &mut App<'_>, image: clipboard::ClipboardImage) {
    if refused(app) {
        return;
    }
    let index = app.pending_attachments.len() + 1;
    match normalize(image, format!("image-{index:03}")) {
        // No notice: the strip above the composer shows what landed, and the
        // transcript stays the conversation.
        Ok(attachment) => attach(app, attachment),
        Err(reason) => app.push_notice(format!("image refused: {reason}")),
    }
}

/// The gates `keys::handle` applies. A paste arrives off the key path, so it has
/// to ask who owns the keyboard itself.
fn refused(app: &mut App<'_>) -> bool {
    if app.pending_question.is_some() || app.pending_permission.is_some() {
        app.push_notice("Paste ignored: answer the prompt on screen first.");
        return true;
    }
    if app.modal.is_some() {
        app.push_notice("Paste ignored: close the picker first.");
        return true;
    }
    false
}

/// Decoded pixels become PNG once, which is the encoded image the store and the
/// wire both take.
fn normalize(image: clipboard::ClipboardImage, stem: String) -> Result<Attachment, String> {
    let clipboard::ClipboardImage {
        width,
        height,
        bytes,
    } = image;
    check_size(width, height)?;
    let bytes = encode_png(width, height, bytes)?;
    let (name, media_type) = (format!("{stem}.png"), "image/png".to_string());
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "{} is over the 4 MiB cap for one image",
            size_label(bytes.len())
        ));
    }
    Ok(Attachment::new(
        name,
        media_type,
        width,
        height,
        bytes,
        PasteSource::Clipboard,
    ))
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width) * u64::from(height);
    if width > MAX_IMAGE_EDGE || height > MAX_IMAGE_EDGE || pixels > MAX_IMAGE_PIXELS {
        return Err(format!(
            "{width}×{height} is over the 24 MP cap for a pasted image"
        ));
    }
    Ok(())
}

fn encode_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or("the clipboard image data was the wrong size")?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(out.into_inner())
}

#[cfg(test)]
#[path = "../../tests/tui/paste.rs"]
mod tests;
