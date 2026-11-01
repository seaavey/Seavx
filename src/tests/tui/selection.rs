use std::sync::{Arc, Mutex};

use super::Selection;
use crate::tui::app::clipboard::{Clipboard, ClipboardImage};
use crate::tui::app::test_app;
use ratatui::layout::Rect;

/// Records what was copied; a drag must never touch a display server in a test.
struct Recording(Arc<Mutex<Vec<String>>>);

impl Clipboard for Recording {
    fn text(&self) -> Result<String, String> {
        Err("unused".to_string())
    }

    fn image(&self) -> Result<ClipboardImage, String> {
        Err("unused".to_string())
    }

    fn write(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

fn picking(copied: &Arc<Mutex<Vec<String>>>) -> crate::tui::app::App<'static> {
    let mut app = test_app();
    app.clipboard = Arc::new(Recording(Arc::clone(copied)));
    // Three painted rows at x = 0, y = 1, ten cells wide.
    app.set_transcript(
        Rect::new(0, 1, 10, 3),
        0,
        vec![
            "hello worl".to_string(),
            "second row".to_string(),
            String::new(),
        ],
    );
    app
}

#[test]
fn a_drag_copies_exactly_the_characters_it_covered() {
    let copied = Arc::new(Mutex::new(Vec::new()));
    let mut app = picking(&copied);

    app.selection_begin(0, 1);
    app.selection_extend(5, 2);
    app.selection_finish();

    assert_eq!(
        copied.lock().unwrap().as_slice(),
        ["hello worl\nsecond"],
        "the block runs from the first cell of row 0 to column 5 of row 1"
    );
    let (toast, _) = app.toast.clone().expect("the copy is confirmed");
    assert!(toast.contains("2 lines"), "{toast}");
}

#[test]
fn a_click_selects_nothing_and_copies_nothing() {
    let copied = Arc::new(Mutex::new(Vec::new()));
    let mut app = picking(&copied);

    app.selection_begin(2, 2);
    app.selection_finish();

    assert!(copied.lock().unwrap().is_empty(), "a click copies nothing");
    assert!(app.selection.is_none(), "and leaves no block behind");
}

#[test]
fn a_drag_that_starts_outside_the_transcript_is_ignored() {
    let copied = Arc::new(Mutex::new(Vec::new()));
    let mut app = picking(&copied);

    app.selection_begin(2, 0);
    app.selection_extend(6, 2);
    app.selection_finish();

    assert!(
        app.selection.is_none(),
        "the pointer keeps its old behaviour"
    );
    assert!(copied.lock().unwrap().is_empty());
}

#[test]
fn scrolling_keeps_the_selection_on_the_rows_it_was_dragged_over() {
    let copied = Arc::new(Mutex::new(Vec::new()));
    let mut app = picking(&copied);

    app.selection_begin(0, 1);
    app.selection_extend(4, 1);

    // The same text, now scrolled: the selection is held in painted rows, so the
    // block follows its rows instead of staying at the screen coordinates.
    app.set_transcript(
        Rect::new(0, 1, 10, 3),
        7,
        vec!["row seven ".to_string(), String::new(), String::new()],
    );
    let selection = app.selection.as_ref().expect("the selection survives");
    assert_eq!(
        selection.anchor,
        (0, 0),
        "row 0 stays row 0 after scrolling"
    );
}

#[test]
fn escape_drops_the_block_before_it_cancels_anything() {
    let copied = Arc::new(Mutex::new(Vec::new()));
    let mut app = picking(&copied);
    app.selection_begin(0, 1);
    app.selection_extend(3, 1);

    app.escape();

    assert!(app.selection.is_none(), "Esc drops the block first");
    assert!(!app.should_quit, "and never quits");
}

#[test]
fn a_reversed_drag_copies_the_same_block() {
    let selection = Selection {
        anchor: (4, 6),
        cursor: (2, 3),
        width: 20,
        dragged: true,
    };
    let rows = ["one", "two", "three", "four", "five"]
        .map(str::to_string)
        .to_vec();

    let (text, lines) = selection.text(&rows, 0);

    assert_eq!(
        text, "ee\nfour\nfive",
        "a reversed drag covers the same block: {text}"
    );
    assert_eq!(lines, 3);
}

#[test]
fn a_wide_glyph_counts_for_the_cells_it_paints() {
    let rows = vec!["中文x".to_string()];
    let selection = Selection {
        anchor: (0, 0),
        cursor: (0, 1),
        width: 4,
        dragged: true,
    };

    let (text, _) = selection.text(&rows, 0);

    assert_eq!(text, "中", "both cells of one glyph select the glyph once");
}
