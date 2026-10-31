use super::*;
use crate::tui::app::run::translate;
use crate::tui::app::{Segment, test_app};
use crate::tui::event::Action;
use crossterm::event::Event as CEvent;

fn transcript(app: &App) -> String {
    app.segments
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) | Segment::Notice(text) => text.clone(),
            Segment::Thinking(text) => text.clone(),
        })
        .collect()
}

#[test]
fn a_paste_event_is_not_dropped() {
    assert!(
        matches!(
            translate(CEvent::Paste("x".to_string())),
            Some(Action::Paste(text)) if text == "x"
        ),
        "a bracketed paste must reach the loop as data"
    );
    assert!(
        translate(CEvent::FocusGained).is_none(),
        "events the loop cannot act on stay dropped"
    );
}

#[test]
fn a_paste_of_crlf_becomes_lf() {
    let (text, truncated) = sanitize("a\r\nb\rc").expect("not empty");
    assert_eq!(text, "a\nb\nc");
    assert!(!truncated);
}

#[test]
fn a_paste_drops_c0_controls_but_keeps_tabs() {
    // An ESC in pasted text would otherwise reach the renderer as a sequence.
    let (text, _) = sanitize("a\tb\u{1b}[31m\u{7}c").expect("not empty");
    assert_eq!(text, "a\tb[31mc");
}

#[test]
fn an_oversized_paste_is_truncated_with_a_notice() {
    let mut app = test_app();
    let huge = "x".repeat(MAX_PASTE_TEXT + 1024);
    handle(&mut app, &huge);

    assert_eq!(app.input_text().len(), MAX_PASTE_TEXT, "cut at the cap");
    assert!(
        transcript(&app).contains("truncated at 256 KiB"),
        "the notice names the cap: {}",
        transcript(&app)
    );
}

#[test]
fn a_paste_inserts_one_multiline_content_and_never_submits() {
    let mut app = test_app();
    let pasted: String = (0..200)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\r\n");

    handle(&mut app, &pasted);

    assert_eq!(app.input.lines().len(), 200, "one paste, 200 lines");
    assert_eq!(app.status, crate::tui::event::AgentState::Idle, "no turn");
    assert!(
        !transcript(&app).contains("User:"),
        "nothing was submitted: {}",
        transcript(&app)
    );
}

#[test]
fn a_paste_while_a_question_is_open_is_refused() {
    let mut app = test_app();
    app.pending_question = Some(crate::tui::app::PendingQuestion {
        id: 1,
        question: "Which file?".to_string(),
        kind: crate::session::QuestionKind::Text,
        options: Vec::new(),
        selected: 0,
        chosen: std::collections::BTreeSet::new(),
        text: String::new(),
        reply: None,
    });

    handle(&mut app, "pasted");

    assert_eq!(app.input_text(), "", "the composer is untouched");
    assert!(transcript(&app).contains("Paste ignored"));
}
