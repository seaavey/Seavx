use std::path::PathBuf;

use base64::Engine;
use genai::chat::{Binary, ChatMessage, ContentPart, MessageContent};
use sha2::{Digest, Sha256};

/// One image riding on a turn. `bytes` are encoded in `media_type`; the decoded
/// pixels are never carried, so the store and the request hold the same file.
pub struct Attachment {
    pub sha256: String,
    /// Chip and wire name: `image-001.png`. The extension follows `media_type`,
    /// which is what tells a resumed transcript and the provider the format.
    pub name: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub source: PasteSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasteSource {
    Clipboard,
    Path(PathBuf),
}

impl Attachment {
    pub fn new(
        name: String,
        media_type: String,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
        source: PasteSource,
    ) -> Self {
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        Self {
            sha256,
            name,
            media_type,
            width,
            height,
            bytes,
            source,
        }
    }

    /// The transcript line: `[image #1 1280×720 png 214 KiB]`. Built from the
    /// header and the byte count, never from the payload.
    pub fn summary(&self, index: usize) -> String {
        format!(
            "image #{index} {}×{} {} {}",
            self.width,
            self.height,
            self.extension(),
            size_label(self.bytes.len())
        )
    }

    pub fn extension(&self) -> &str {
        match self.media_type.as_str() {
            "image/jpeg" => "jpeg",
            _ => "png",
        }
    }

    pub fn content_part(&self) -> ContentPart {
        let base64 = base64::engine::general_purpose::STANDARD.encode(&self.bytes);
        ContentPart::Binary(Binary::from_base64(
            self.media_type.clone(),
            base64,
            Some(self.name.clone()),
        ))
    }
}

/// The user message a turn sends and the row the store holds: one text part, then
/// one binary per attachment.
pub fn user_message(prompt: &str, attachments: &[Attachment]) -> ChatMessage {
    if attachments.is_empty() {
        return ChatMessage::user(prompt);
    }
    let mut parts = vec![ContentPart::Text(prompt.to_string())];
    parts.extend(attachments.iter().map(Attachment::content_part));
    ChatMessage::user(MessageContent::from_parts(parts))
}

/// What a stored binary says on a resumed transcript: the same words as the live
/// line, read back from the encoded bytes because the row carries no attachment
/// table.
pub fn stored_summary(index: usize, name: &str, content_type: &str, base64: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64)
        .unwrap_or_default();
    let (width, height) = dimensions(&bytes).unwrap_or_default();
    let extension = if content_type == "image/jpeg" {
        "jpeg"
    } else {
        "png"
    };
    if width == 0 || height == 0 {
        return format!(
            "image #{index} {name} {extension} {}",
            size_label(bytes.len())
        );
    }
    format!(
        "image #{index} {width}×{height} {extension} {}",
        size_label(bytes.len())
    )
}

/// Header-only read: no decode of the pixels, so a resumed transcript never pays
/// for the image it is only labelling.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Byte counts as a person reads them: `214 KiB`, `1.4 MiB`.
pub fn size_label(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let size = bytes as f64;
    if size < KIB {
        format!("{bytes} B")
    } else if size < KIB * KIB {
        format!("{:.0} KiB", size / KIB)
    } else {
        format!("{:.1} MiB", size / (KIB * KIB))
    }
}

#[cfg(test)]
#[path = "../tests/core/attachment.rs"]
mod tests;
