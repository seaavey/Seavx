use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::*;
use crate::tui::app::{App, Segment, keys, test_app};

struct FakeClipboard {
    answer: Result<String, String>,
    calls: AtomicUsize,
}

impl Clipboard for FakeClipboard {
    fn text(&self) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.answer.clone()
    }
}

/// Every method panics: a guard, not a tautology, for "no key, no clipboard".
struct PanicClipboard;

impl Clipboard for PanicClipboard {
    fn text(&self) -> Result<String, String> {
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

#[tokio::test]
async fn ctrl_v_pastes_text_from_the_clipboard() {
    let mut app = test_app();
    let fake = Arc::new(FakeClipboard {
        answer: Ok("pasted\r\ntext".to_string()),
        calls: AtomicUsize::new(0),
    });
    app.clipboard = fake.clone();

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert_eq!(
        app.input_text(),
        "pasted\ntext",
        "one paste, CRLF normalized"
    );
    assert_eq!(fake.calls.load(Ordering::Relaxed), 1, "read exactly once");
}

#[tokio::test]
async fn ctrl_v_with_an_empty_clipboard_says_so() {
    let mut app = test_app();
    app.clipboard = Arc::new(FakeClipboard {
        answer: Ok(String::new()),
        calls: AtomicUsize::new(0),
    });

    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL).await;

    assert_eq!(app.input_text(), "");
    assert!(
        transcript(&app).contains("clipboard is empty"),
        "a silent no-op looks like a broken key: {}",
        transcript(&app)
    );
}

#[tokio::test]
async fn alt_v_forces_the_text_path() {
    let mut app = test_app();
    app.clipboard = Arc::new(FakeClipboard {
        answer: Ok("forced".to_string()),
        calls: AtomicUsize::new(0),
    });

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
