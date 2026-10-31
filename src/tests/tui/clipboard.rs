use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::*;
use crate::tui::app::{App, Segment, keys, test_app};

struct FakeClipboard {
    answer: Result<String, String>,
    image: Result<ClipboardImage, String>,
    calls: AtomicUsize,
}

impl FakeClipboard {
    fn text(answer: &str) -> Self {
        Self {
            answer: Ok(answer.to_string()),
            image: Err("is empty".to_string()),
            calls: AtomicUsize::new(0),
        }
    }

    fn empty() -> Self {
        Self {
            answer: Err("is empty".to_string()),
            image: Err("is empty".to_string()),
            calls: AtomicUsize::new(0),
        }
    }

    fn image(image: ClipboardImage) -> Self {
        Self {
            answer: Err("not available as text".to_string()),
            image: Ok(image),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Clipboard for FakeClipboard {
    fn text(&self) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.answer.clone()
    }

    fn image(&self) -> Result<ClipboardImage, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.image.clone()
    }
}

/// Every method panics: a guard, not a tautology, for "no key, no clipboard".
struct PanicClipboard;

impl Clipboard for PanicClipboard {
    fn text(&self) -> Result<String, String> {
        panic!("the clipboard was read without a key")
    }

    fn image(&self) -> Result<ClipboardImage, String> {
        panic!("the clipboard was read without a key")
    }
}

fn transcript(app: &App) -> String {
    app.segments
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) | Segment::Notice(text) => text.clone(),
            Segment::Thinking(text) => text.clone(),
        })
        .collect()
}

async fn press(app: &mut App<'_>, code: KeyCode, modifiers: KeyModifiers) {
    let (tx, _rx) = mpsc::unbounded_channel();
    keys::handle(app, KeyEvent::new(code, modifiers), &tx).await;
}

fn rgba(width: u32, height: u32) -> ClipboardImage {
    ClipboardImage {
        width,
        height,
        bytes: vec![7u8; (width as usize) * (height as usize) * 4],
    }
}

#[tokio::test]
async fn ctrl_v_pastes_text_from_the_clipboard() {
    let mut app = test_app();
    let fake = Arc::new(FakeClipboard::text("pasted\r\ntext"));
    app.clipboard = fake.clone();

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert_eq!(
        app.input_text(),
        "pasted\ntext",
        "one paste, CRLF normalized"
    );
    assert_eq!(fake.calls.load(Ordering::Relaxed), 1, "read once, as text");
    assert!(app.pending_attachments.is_empty());
}

#[tokio::test]
async fn ctrl_v_with_an_empty_clipboard_says_so() {
    let mut app = test_app();
    app.clipboard = Arc::new(FakeClipboard::empty());

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert_eq!(app.input_text(), "");
    assert!(
        transcript(&app).contains("clipboard is empty"),
        "a silent no-op looks like a broken key: {}",
        transcript(&app)
    );
}

#[tokio::test]
async fn a_copied_image_becomes_one_attachment() {
    let mut app = test_app();
    app.clipboard = Arc::new(FakeClipboard::image(rgba(2, 2)));

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert_eq!(app.pending_attachments.len(), 1);
    let attached = &app.pending_attachments[0];
    assert_eq!(attached.media_type, "image/png", "pixels are encoded once");
    assert_eq!(attached.name, "image-001.png");
    assert_eq!((attached.width, attached.height), (2, 2));
    assert!(
        attached.summary(1).starts_with("image #1 2×2 png"),
        "the strip above the prompt reads this"
    );
    assert!(
        !transcript(&app).contains("image #1"),
        "a pending image is composer state, not transcript output: {}",
        transcript(&app)
    );
}

#[tokio::test]
async fn an_image_over_the_pixel_cap_is_refused() {
    let mut app = test_app();
    // The header alone is enough: the cap is checked before any decode.
    app.clipboard = Arc::new(FakeClipboard::image(ClipboardImage {
        width: 20_000,
        height: 20_000,
        bytes: vec![0u8; 16],
    }));

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert!(
        app.pending_attachments.is_empty(),
        "a 400 MP image is not decoded"
    );
    assert!(
        transcript(&app).contains("image refused"),
        "the refusal is named: {}",
        transcript(&app)
    );
}

#[tokio::test]
async fn text_wins_when_the_clipboard_holds_both() {
    let mut app = test_app();
    app.clipboard = Arc::new(FakeClipboard {
        answer: Ok("words".to_string()),
        image: Ok(rgba(2, 2)),
        calls: AtomicUsize::new(0),
    });

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert_eq!(app.input_text(), "words");
    assert!(app.pending_attachments.is_empty(), "text does not attach");
}

#[tokio::test]
async fn alt_v_forces_the_text_path() {
    let mut app = test_app();
    app.clipboard = Arc::new(FakeClipboard::text("forced"));

    press(&mut app, KeyCode::Char('v'), KeyModifiers::ALT).await;

    assert_eq!(app.input_text(), "forced");
}

#[tokio::test]
async fn no_clipboard_call_is_made_without_a_key() {
    let mut app = test_app();
    app.clipboard = Arc::new(PanicClipboard);

    press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).await;

    assert_eq!(app.input_text(), "a");
}

#[tokio::test]
async fn alt_backspace_falls_through_while_no_chip_exists() {
    let mut app = test_app();
    app.set_input("hello world");

    press(&mut app, KeyCode::Backspace, KeyModifiers::ALT).await;

    assert_eq!(
        app.input_text(),
        "hello",
        "with no chip, the word delete keeps working"
    );
}
