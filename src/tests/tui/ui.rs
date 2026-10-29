use super::*;
use crate::tui::app::test_app;
use ratatui::{Terminal, backend::TestBackend};

fn screen(input: &str) -> String {
    let mut app = test_app();
    app.input.insert_str(input);
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn slash_menu_lists_every_match_inside_its_border() {
    let filtered = screen("/m");
    assert!(filtered.contains("/model"), "{filtered}");
    assert!(filtered.contains("/memory"), "{filtered}");

    let all = screen("/");
    assert!(all.contains("/exit"), "{all}");
}

#[test]
fn ghost_completion_renders_after_the_typed_text() {
    let mut app = test_app();
    app.history.clear();
    app.history.push("adrian".to_string());
    app.input.insert_str("adr");
    app.refresh_ghost();

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = (0..24)
        .find(|y| {
            let text: String = (0..90).map(|x| buffer[(x, *y)].symbol()).collect();
            text.contains("adrian")
        })
        .expect("ghost completion is drawn");
    let text: String = (0..90).map(|x| buffer[(x, row)].symbol()).collect();
    assert!(text.contains("adrian"), "{text}");

    // The tail past the typed prefix is dimmed like the placeholder.
    // Convert the byte offset from `find` to a cell (char) offset: the
    // border glyph is multi-byte, so they differ.
    let start = text[..text.find("adrian").unwrap()].chars().count() as u16;
    assert_eq!(buffer[(start + 4, row)].style().fg, Some(Color::DarkGray));
}

fn screen_of(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(app, frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn header_and_footer_show_build_mode() {
    let app = test_app();
    let screen = screen_of(&app);
    assert!(screen.contains("[build]"), "{screen}");
    assert!(screen.contains("build"), "{screen}");
}

#[test]
fn header_and_footer_show_plan_mode() {
    let mut app = test_app();
    app.mode = AgentMode::Plan;
    let screen = screen_of(&app);
    assert!(screen.contains("[plan]"), "{screen}");
    assert!(screen.contains("plan"), "{screen}");
    assert!(!screen.contains("[build]"), "{screen}");
}

#[test]
fn transcript_renders_markdown_instead_of_raw_syntax() {
    let mut app = test_app();
    app.push_text("## Status\n\n- **bold** and `code`\n\n```bash\ncargo test\n```\n");
    let screen = screen_of(&app);
    assert!(screen.contains("Status"), "{screen}");
    assert!(screen.contains("• bold and code"), "{screen}");
    assert!(screen.contains("cargo test"), "{screen}");
    assert!(
        !screen.contains("**"),
        "markdown delimiters leaked: {screen}"
    );
    assert!(!screen.contains("##"), "heading hashes leaked: {screen}");
    assert!(!screen.contains("```"), "fence leaked: {screen}");
}

#[test]
fn transcript_cache_tracks_content_and_width() {
    let mut app = test_app();
    app.push_text("first **answer**\n");
    let narrow = transcript_lines(&app, 40);
    let wider = transcript_lines(&app, 60);
    assert_eq!(narrow.len(), wider.len(), "same content, same block count");

    app.push_text("- second item\n");
    let after_push = transcript_lines(&app, 60);
    assert!(
        after_push.len() > wider.len(),
        "cache must invalidate on new text"
    );

    app.set_text("replaced");
    assert_eq!(
        transcript_lines(&app, 60).len(),
        1,
        "cache must invalidate on set_text"
    );

    app.clear_transcript();
    assert!(
        transcript_lines(&app, 60).is_empty(),
        "cache must invalidate on clear"
    );
}

#[test]
fn orb_animates_for_active_states_and_holds_idle() {
    let distinct = |orb: Orb| {
        (0..40u128)
            .map(|i| orb.glyph(i * 45))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    assert!(distinct(Orb::Breathing) > 1);
    assert!(distinct(Orb::Composing) > 1);
    assert!(distinct(Orb::Working) > 1);
    assert_eq!(distinct(Orb::Idle), 1, "idle orb stays still");
}

#[test]
fn reasoning_renders_gray_and_indented_above_the_answer() {
    let mut app = test_app();
    app.push_thinking("checking the loader\nsecond line");
    app.push_text("\nDone.\n");
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();

    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };
    let thinking_y = (0..24)
        .find(|y| row(*y).contains("thinking"))
        .expect("thinking header");
    let header = row(thinking_y);
    assert!(header.starts_with("  "), "header is indented: {header:?}");
    assert!(
        !header.trim_start().starts_with("thinking"),
        "orb glyph precedes the label: {header:?}"
    );
    assert!(row(thinking_y + 1).starts_with("  │ checking the loader"));
    assert_eq!(
        buffer[(4, thinking_y + 1)].style().fg,
        Some(Color::DarkGray),
        "reasoning text is gray"
    );
    let answer_y = (0..24).find(|y| row(*y).starts_with("Done.")).unwrap();
    assert!(answer_y > thinking_y, "reasoning sits above the answer");
}

#[test]
fn reasoning_wraps_long_lines_with_aligned_prefix() {
    let mut app = test_app();
    app.push_thinking("This is a very long reasoning text that will exceed forty columns easily and must wrap nicely.");
    let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();

    let row = |y: u16| -> String { (0..40).map(|x| buffer[(x, y)].symbol()).collect() };
    let thinking_y = (0..24)
        .find(|y| row(*y).contains("thinking"))
        .expect("thinking header");
    assert!(row(thinking_y + 1).starts_with("  │ This is a very"));
    assert!(row(thinking_y + 2).starts_with("  │ "));
    assert!(!row(thinking_y + 2).starts_with("  │   "));
}

#[test]
fn empty_session_shows_splash_until_transcript_starts() {
    let mut app = test_app();
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    let draw = |app: &App, terminal: &mut Terminal<TestBackend>| -> Vec<String> {
        terminal.draw(|frame| render(app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..24)
            .map(|y| (0..90).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    };

    let rows = draw(&app, &mut terminal);
    let art_y = rows
        .iter()
        .position(|row| row.contains(SPLASH[2]))
        .expect("splash on an empty session");
    // 44 columns in a 90-column pane, horizontally centered.
    assert_eq!(rows[art_y].find(SPLASH[2]), Some((90 - 44) / 2));
    assert!(rows[art_y + 1].starts_with(&format!("{}{}", " ".repeat(23), SPLASH[3])));

    app.push_text("hello");
    assert!(
        !draw(&app, &mut terminal)
            .iter()
            .any(|r| r.contains(SPLASH[2]))
    );
}

#[test]
fn the_footer_marks_an_unrecognised_model_as_assumed() {
    // Wide enough that the hints do not clip the limit.
    let mut app = test_app();
    app.config.model = "vendor/mystery-1".to_string();
    let mut terminal = Terminal::new(TestBackend::new(170, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("/64k"), "the conservative default: {text}");
    assert!(text.contains("(assumed)"), "{text}");

    app.config.model = "deepseek-chat".to_string();
    let mut terminal = Terminal::new(TestBackend::new(170, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("/128k"), "{text}");
    assert!(!text.contains("(assumed)"), "{text}");
}
