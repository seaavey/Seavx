use super::*;
use crate::agent::engine::EFFORT_LEVELS;
use crate::tui::app::test_app;
use ratatui::{Terminal, backend::TestBackend};
use std::time::Instant;

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

/// The transcript painted at `width` x `height`, as flat text.
fn transcript_screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
fn slash_menu_lists_every_match_inside_its_border() {
    let filtered = screen("/m");
    assert!(filtered.contains("/model"), "{filtered}");
    assert!(filtered.contains("/memory"), "{filtered}");

    // The whole table is taller than a 24-row terminal, so the menu scrolls
    // instead of drawing past the last row: the selected entry stays visible.
    let mut app = test_app();
    app.input.insert_str("/");
    app.slash_selected = crate::tui::commands::COMMANDS.len() - 1;
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let all: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
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
fn header_shows_the_mode_the_shortcut_cycled_to() {
    let mut app = test_app();
    app.cycle_mode();
    app.cycle_mode();
    let screen = screen_of(&app);
    assert!(screen.contains("[accept-edits]"), "{screen}");
    assert!(!screen.contains("[plan]"), "{screen}");
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

/// A markdown paragraph is one logical line but many wrapped rows, so a scroll
/// offset counted in lines stops short and cuts the tail off the screen.
#[test]
fn auto_scroll_pins_the_last_wrapped_row() {
    let mut app = test_app();
    app.push_text(&format!(
        "{}\n\nLAST-ROW\n",
        "lorem ipsum dolor ".repeat(40)
    ));
    app.auto_scroll = true;

    let text = transcript_screen(&app, 40, 20);
    assert!(text.contains("LAST-ROW"), "tail is off screen: {text}");
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
    let distinct = |state: AgentState| {
        (0..40u128)
            .map(|i| orb_cells(&state, i * 45))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    assert!(distinct(AgentState::Thinking) > 1, "thinking moves");
    assert!(distinct(AgentState::Streaming) > 1, "streaming moves");
    assert!(
        distinct(AgentState::ExecutingTool("run_command".to_string())) > 1,
        "a running tool moves"
    );
    assert_eq!(distinct(AgentState::Idle), 1, "idle orb stays still");

    // A blank orb reads as a broken status line: the crop must always find a
    // glyph, in every state, at any tick.
    for state in [
        AgentState::Idle,
        AgentState::Thinking,
        AgentState::Streaming,
        AgentState::Checking,
        AgentState::Compacting,
        AgentState::ExecutingTool("run_command".to_string()),
    ] {
        for i in 0..40u128 {
            let cells = orb_cells(&state, i * 45);
            assert!(
                cells.chars().any(|cell| cell != ' '),
                "{state:?} at {}ms drew nothing: {cells:?}",
                i * 45
            );
        }
    }
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
    app.config.model = Some("vendor/mystery-1".to_string());
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

    app.config.model = Some("deepseek-chat".to_string());
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

#[test]
fn the_footer_keeps_the_model_token_and_effort_columns_in_a_wide_frame() {
    let mut app = test_app();
    app.config.model = Some("deepseek-chat".to_string());
    app.config.reasoning_effort = Some("high".to_string());
    app.total_tokens = 1234;
    app.prompt_tokens = 1234;

    let mut terminal = Terminal::new(TestBackend::new(170, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let footer: String = (0..170).map(|x| buffer[(x, 23)].symbol()).collect();

    assert!(footer.contains("deepseek-chat"), "{footer}");
    assert!(footer.contains("~1k/128k"), "{footer}");
    assert!(footer.contains("effort high"), "{footer}");
}

#[test]
fn the_footer_drops_hints_before_it_clips_the_token_count() {
    let mut app = test_app();
    app.config.model = Some("rv/deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string());
    app.total_tokens = 1234;
    app.prompt_tokens = 1234;

    let footer = |width: u16| -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..width).map(|x| buffer[(x, 23)].symbol()).collect()
    };

    // The pane this app opens in: the long model name alone must not eat the
    // counter; the shortest hint tier gives way instead.
    let narrow = footer(100);
    assert!(narrow.contains("/128k"), "{narrow}");
    assert!(narrow.contains("ctrl+c"), "{narrow}");

    // Too narrow for any hint: they go, the data stays.
    let tight = footer(60);
    assert!(tight.contains("/128k"), "{tight}");
    assert!(tight.contains("effort"), "{tight}");
    assert!(!tight.contains("tab complete"), "{tight}");

    // Narrower than the line itself: the model is the column that folds.
    let tiny = footer(44);
    assert!(tiny.contains('…'), "{tiny}");
    assert!(tiny.contains("/128k"), "{tiny}");
}

#[test]
fn streamed_reasoning_deltas_stay_in_one_paragraph() {
    let mut app = test_app();
    // The engine emits one delta per token, none of them ending in a newline.
    for delta in ["Saya ", "perlu ", "memeriksa ", "berkas ", "ini."] {
        app.push_thinking(delta);
    }
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..100).map(|x| buffer[(x, y)].symbol()).collect() };

    let thinking_y = (0..24)
        .find(|y| row(*y).contains("thinking"))
        .expect("thinking header");
    assert_eq!(
        row(thinking_y + 1).trim_end(),
        "  │ Saya perlu memeriksa berkas ini.",
        "a delta joins the open block instead of starting a line"
    );
}

#[test]
fn reasoning_uses_the_whole_width_of_a_wide_terminal() {
    let mut app = test_app();
    app.push_thinking(&"kata ".repeat(80));
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..120).map(|x| buffer[(x, y)].symbol()).collect() };

    let thinking_y = (0..24)
        .find(|y| row(*y).contains("thinking"))
        .expect("thinking header");
    let first = row(thinking_y + 1);
    assert!(
        first.trim_end().chars().count() > 100,
        "a wide pane is used, not a narrow column: {first:?}"
    );
}

#[test]
fn a_tool_call_draws_its_verb_and_its_object_above_the_outcome() {
    let mut app = test_app();
    app.push_text("\n⚡ run  cargo clippy --all-targets\n\n");
    app.push_text("  └ completed 11.7s\n");

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };

    let call_y = (0..24)
        .find(|y| row(*y).contains("cargo clippy"))
        .expect("the call line");
    assert!(
        row(call_y).starts_with('⚡') && row(call_y).contains("run  cargo clippy --all-targets"),
        "the verb and the command, not the argument list: {:?}",
        row(call_y)
    );
    let outcome_y = (call_y + 1..call_y + 3)
        .find(|y| row(*y).contains("completed 11.7s"))
        .unwrap_or_else(|| panic!("the outcome sits under the call: {:?}", row(call_y + 1)));
    assert!(outcome_y > call_y, "below the call, never above it");
}

#[test]
fn a_pending_permission_renders_a_box_above_the_composer() {
    let mut app = test_app();
    let (reply, _answer) = tokio::sync::oneshot::channel();
    app.pending_permission = Some(crate::tui::app::PendingPermission {
        request: crate::agent::permissions::PermissionRequest::new(
            crate::agent::permissions::ActionKind::Write,
            "write_file",
            "notes.txt",
        ),
        reply,
    });

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };

    let title_y = (0..24)
        .find(|y| row(*y).contains("permission"))
        .expect("the box title");
    assert!(
        row(title_y).trim_start().starts_with('┌'),
        "a bordered box, not a bare line: {:?}",
        row(title_y)
    );
    assert!(
        row(title_y).contains("permission · write"),
        "the box names the category `a` would record: {:?}",
        row(title_y)
    );
    assert!(
        row(title_y + 1).contains("write notes.txt inside the workspace"),
        "the effect is spelled out in words: {:?}",
        row(title_y + 1)
    );
    assert!(
        !row(title_y + 1).contains("write_file"),
        "the tool name is not what a person needs to read: {:?}",
        row(title_y + 1)
    );
    for key in ["y", "a", "u", "n"] {
        assert!(
            row(title_y + 2).contains(&format!(" {key} ")),
            "the {key} answer is offered: {:?}",
            row(title_y + 2)
        );
    }
    assert!(
        row(title_y + 3).trim_start().starts_with('└'),
        "a closed box"
    );

    // The status bar keeps its own row: the box no longer paints over it.
    let status_y = (0..24)
        .find(|y| row(*y).contains("ready"))
        .expect("status bar");
    assert!(!row(status_y).contains("permission"), "{:?}", row(status_y));
}

#[test]
fn the_model_picker_scrolls_the_list_to_the_selection() {
    let mut app = test_app();
    let models: Vec<String> = (0..40).map(|index| format!("model-{index}")).collect();
    app.config.model = Some("model-30".to_string());
    app.modal = Some(ModalSelection::Model {
        models,
        query: String::new(),
        selected: 30,
    });

    let text = screen_of(&app);
    assert!(
        text.contains("model-30"),
        "the selected row must be scrolled into view: {text}"
    );
    assert!(
        !text.contains("model-0 ") && !text.contains("model-1 "),
        "the window follows the selection instead of pinning to the top: {text}"
    );

    // The real cache holds 15 models in a 24-row terminal: the last one is the
    // row that used to fall past the fold and stay unreachable.
    let mut app = test_app();
    let models: Vec<String> = (0..15).map(|index| format!("model-{index}")).collect();
    app.modal = Some(ModalSelection::Model {
        models,
        query: String::new(),
        selected: 14,
    });
    let text = screen_of(&app);
    assert!(
        text.contains("model-14"),
        "the last model is reachable: {text}"
    );
}

#[test]
fn typing_narrows_the_model_picker_to_matching_rows() {
    let mut app = test_app();
    app.config.model = Some("alpha".to_string());
    app.modal = Some(ModalSelection::Model {
        models: ["alpha", "beta", "gamma"].map(str::to_string).to_vec(),
        query: "et".to_string(),
        selected: 0,
    });

    let text = screen_of(&app);
    assert!(text.contains("beta"), "{text}");
    assert!(
        !text.contains("gamma"),
        "the filter hides the rows that do not match: {text}"
    );
    assert!(
        text.contains("Select Model 1/3"),
        "the title says how many rows matched: {text}"
    );
    assert!(text.contains("filter: et"), "{text}");
}

#[test]
fn a_notice_keeps_the_rows_it_was_printed_with() {
    let mut app = test_app();
    app.push_notice(
        "Seavx configuration\n\nmodel             fake-model   (workspace)\nreasoning_effort  high         (default)\nsystem_prompt     default      (workspace)",
    );

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };
    let rows: Vec<String> = (0..24).map(row).collect();

    let model_row = rows
        .iter()
        .find(|line| line.contains("fake-model"))
        .expect("the model row is drawn");
    assert!(
        model_row.contains("(workspace)"),
        "a row keeps its own origin: {model_row:?}"
    );
    let prompt_row = rows
        .iter()
        .find(|line| line.contains("system_prompt"))
        .expect("the system_prompt row is drawn");
    assert!(
        prompt_row.contains("(workspace)"),
        "markdown used to fold every row into one paragraph: {prompt_row:?}"
    );
}

#[test]
fn a_running_refresh_is_visible_in_the_picker_title() {
    let mut app = test_app();
    app.config.base_url = Some("https://gateway.test/v1/".to_string());
    app.modal = Some(ModalSelection::Model {
        models: vec!["alpha".to_string()],
        query: String::new(),
        selected: 0,
    });
    app.refreshing_models = true;

    let busy = screen_of(&app);
    assert!(
        busy.contains("Fetching models from gateway.test"),
        "a slow fetch names the host and says it is working: {busy}"
    );

    app.refreshing_models = false;
    let idle = screen_of(&app);
    assert!(idle.contains("Select Model"), "{idle}");
    assert!(!idle.contains("Fetching models"), "{idle}");
}

/// Border colour of the toast box whose body contains `msg`.
fn toast_border_color(app: &App, msg: &str) -> Color {
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };
    let y = (0..24)
        .find(|y| row(*y).contains(msg))
        .expect("the toast message is drawn");
    let x = (0..90)
        .find(|x| buffer[(*x, y - 1)].symbol() == "┌")
        .expect("the toast has a border");
    let border = buffer[(x, y - 1)]
        .style()
        .fg
        .expect("the border is coloured");
    // The body and the border read the same variable; a border-only assertion
    // would miss a body painted in a different colour.
    assert_eq!(
        buffer[(x + 1, y)].style().fg,
        Some(border),
        "the message takes the border colour: {:?}",
        row(y)
    );
    border
}

#[test]
fn a_success_toast_is_green_and_an_error_toast_is_red() {
    let mut app = test_app();
    app.toast = Some((
        "✓ Refreshed 3 models from gateway".to_string(),
        Instant::now(),
    ));
    assert_eq!(
        toast_border_color(&app, "Refreshed 3 models from gateway"),
        Color::Green,
        "a ✓ prefix means success"
    );

    app.toast = Some(("✗ Gateway refused: 401".to_string(), Instant::now()));
    assert_eq!(
        toast_border_color(&app, "Gateway refused: 401"),
        Color::Red,
        "anything else is a failure"
    );
}

#[test]
fn the_effort_modal_marks_the_current_level() {
    let mut app = test_app();
    let options = EFFORT_LEVELS.to_vec();
    let selected = options.iter().position(|level| *level == "high").unwrap();
    app.modal = Some(ModalSelection::Effort {
        model: "fake-model".to_string(),
        options,
        selected,
    });

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };
    let rows: Vec<String> = (0..24).map(row).collect();

    for level in EFFORT_LEVELS {
        // A row is `❯ level` or `  level`, padded to the box width; the trailing
        // space keeps `high` from matching the `highx` row.
        assert!(
            rows.iter().any(|line| line.contains(&format!("  {level} "))
                || line.contains(&format!("❯ {level} "))),
            "every level is offered, {level} is missing: {rows:?}"
        );
    }

    let prefix = "❯ high ";
    let y = rows
        .iter()
        .position(|line| line.contains(prefix))
        .expect("the current level carries the cursor");
    // Two cells of cursor glyph, then the level name.
    let start = rows[y][..rows[y].find(prefix).unwrap()].chars().count() as u16 + 2;
    let style = buffer[(start, y as u16)].style();
    assert_eq!(style.fg, Some(Color::Green), "{:?}", rows[y]);
    assert!(
        style.add_modifier.contains(Modifier::BOLD),
        "the selected level is emphasised: {:?}",
        rows[y]
    );

    // The cursor must not repaint every row.
    let below = buffer[(start, y as u16 + 1)].style();
    assert_eq!(
        below.fg,
        Some(Color::White),
        "an unselected level keeps the default colour: {:?}",
        rows[y + 1]
    );
    assert!(
        !below.add_modifier.contains(Modifier::BOLD),
        "only the current level is emphasised: {:?}",
        rows[y + 1]
    );
}

#[test]
fn the_session_modal_highlights_the_selected_row() {
    let mut app = test_app();
    let rows: Vec<String> = (0..20).map(|index| format!("session-{index:02}")).collect();
    app.modal = Some(ModalSelection::Session { rows, selected: 0 });

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };
    let rows: Vec<String> = (0..24).map(row).collect();

    assert!(
        rows.iter().any(|line| line.contains("Select Session")),
        "the box names itself: {rows:?}"
    );
    let y = rows
        .iter()
        .position(|line| line.contains("session-00"))
        .expect("the first session is drawn");
    let start = rows[y][..rows[y].find("session-00").unwrap()]
        .chars()
        .count() as u16;
    assert_eq!(
        buffer[(start, y as u16)].style().bg,
        Some(Color::Yellow),
        "the selected row has a background, not only a glyph: {:?}",
        rows[y]
    );
    assert_ne!(
        buffer[(start, y as u16 + 1)].style().bg,
        Some(Color::Yellow),
        "only the selected row is highlighted: {:?}",
        rows[y + 1]
    );
    assert!(
        !rows.iter().any(|line| line.contains("session-19")),
        "a list longer than the box is clipped, not drawn over the frame: {rows:?}"
    );
}

#[test]
fn the_skill_picker_lists_candidates_and_their_marks() {
    let mut app = test_app();
    app.modal = Some(ModalSelection::Skills {
        names: ["pdf", "charts", "deploy"].map(str::to_string).to_vec(),
        descriptions: [
            "Reads and fills PDF forms.",
            "Draws a chart. It refuses to overwrite an existing image.",
            "Ships the release.",
        ]
        .map(str::to_string)
        .to_vec(),
        selected: 1,
        marked: vec![false, true, false],
    });

    let text = screen_of(&app);
    for name in ["pdf", "charts", "deploy"] {
        assert!(text.contains(name), "{name} is missing: {text}");
    }
    assert!(
        text.contains("[x] charts"),
        "the marked row is checked: {text}"
    );
    assert!(text.contains("[ ] pdf"), "an unmarked row is blank: {text}");
    assert!(
        text.contains("Install Skills 1/3"),
        "the title counts the marks: {text}"
    );
    assert!(text.contains("Draws a chart"), "{text}");
    assert!(
        !text.contains("It refuses"),
        "only the first sentence is drawn: {text}"
    );
}

#[test]
fn the_at_popup_tags_folders_and_files() {
    let mut app = test_app();
    app.input.insert_str("@src");

    let text = screen_of(&app);
    assert!(text.contains("[folder]"), "no folder candidate: {text}");
    assert!(text.contains("[file]"), "no file candidate: {text}");
}

#[test]
fn the_session_line_shows_the_title_and_a_short_state_only() {
    let mut app = test_app();
    app.session_title = Some("port the loader".to_string());
    app.status = AgentState::ExecutingTool(
        "run  cd /home/seaavey/Projects/Seavx && timeout 25 cargo clippy --all-targets".to_string(),
    );

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..24)
        .map(|y| (0..90).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();

    let line = rows
        .iter()
        .find(|line| line.contains("port the loader"))
        .expect("the session title keeps its row");
    assert!(line.contains("working"), "the state is one word: {line:?}");
    assert!(
        !line.contains("cargo clippy"),
        "the running command stays out of the session line: {line:?}"
    );
    assert!(
        line.chars()
            .any(|cell| matches!(cell, '·' | '∘' | '•' | '●')),
        "the orb sits beside the state: {line:?}"
    );
}

#[test]
fn the_preset_picker_draws_the_names_and_the_selected_preview() {
    use crate::agent::presets;

    let root = tempfile::TempDir::new().unwrap();
    let dir = presets::project_dir(root.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("triage.md"), "Triage the failing tests first.").unwrap();
    std::fs::write(dir.join("release.md"), "Write the changelog entry.").unwrap();

    let mut app = test_app();
    app.root = root.path().to_path_buf();
    assert!(app.open_preset_picker(), "the picker opens with presets");

    let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
    terminal.draw(|frame| render(&app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String { (0..90).map(|x| buffer[(x, y)].symbol()).collect() };
    let rows: Vec<String> = (0..30).map(row).collect();

    assert!(
        rows.iter().any(|line| line.contains("Presets")),
        "the box names itself and its keys: {rows:?}"
    );
    assert!(
        rows.iter().any(|line| line.contains("release")),
        "every preset is a row: {rows:?}"
    );
    // Row 0 is selected, so its preview is the one on screen.
    assert!(
        rows.iter()
            .any(|line| line.contains("Write the changelog entry.")),
        "the selected preset is previewed: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|line| line.contains("Triage the failing tests first.")),
        "the row under the cursor is not previewed: {rows:?}"
    );

    let y = rows
        .iter()
        .position(|line| line.contains("release"))
        .expect("the first row is drawn");
    let start = rows[y][..rows[y].find("release").unwrap()].chars().count() as u16;
    assert_eq!(
        buffer[(start, y as u16)].style().bg,
        Some(Color::Yellow),
        "the selected row has a background: {:?}",
        rows[y]
    );
}

#[test]
fn scroll_counts_wrapped_rows_not_source_lines() {
    let mut app = test_app();
    // One source line, many painted rows: a line-based bound cannot scroll it.
    app.push_text(&format!("FIRST-ROW {}THE-END", "word ".repeat(200)));
    let painted = transcript_screen(&app, 40, 10);

    let bottom = app.max_scroll();
    assert!(bottom > 0, "a wrapped line runs past the viewport");
    assert!(painted.contains("THE-END"), "the tail is reachable at rest");

    app.scroll_up(1);
    assert!(!app.auto_scroll);
    assert_eq!(app.scroll, bottom - 1, "one row up, not one line");

    app.scroll_up(u16::MAX);
    assert_eq!(app.scroll, 0, "the top of the transcript is reachable");
    let top = transcript_screen(&app, 40, 10);
    assert!(top.contains("FIRST-ROW"), "first row is off screen: {top}");

    app.scroll_down(u16::MAX);
    assert!(app.auto_scroll, "scrolling past the bottom re-pins to it");
    assert_eq!(app.scroll, bottom);
    let back = transcript_screen(&app, 40, 10);
    assert!(back.contains("THE-END"), "last row is off screen: {back}");
}

#[test]
fn a_pending_image_is_drawn_above_the_prompt_not_in_the_transcript() {
    let mut app = test_app();
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    app.pending_attachments
        .push(crate::session::Attachment::new(
            "image-001.png".to_string(),
            "image/png".to_string(),
            2,
            2,
            bytes.into_inner(),
            crate::session::PasteSource::Clipboard,
        ));

    let drawn = transcript_screen(&app, 90, 24);

    assert!(drawn.contains("[image #1 2×2 png"), "{drawn}");
    assert!(
        !app.segments.iter().any(|segment| match segment {
            crate::tui::app::Segment::Text(text) | crate::tui::app::Segment::Notice(text) =>
                text.contains("image #1"),
            crate::tui::app::Segment::Thinking(text) => text.contains("image #1"),
        }),
        "the strip is composer state, not transcript output"
    );

    // A short terminal keeps room for the prompt: the strip clips, not the box.
    let short = transcript_screen(&app, 40, 12);
    assert!(short.contains("Ask anything"), "the prompt box still draws");
}
