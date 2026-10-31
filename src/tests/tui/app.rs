use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app_with(history: &[&str]) -> App<'static> {
    let mut app = test_app();
    for entry in history {
        app.remember_prompt(entry);
    }
    app
}

fn typed(app: &App) -> String {
    app.input.lines().join("\n")
}

#[test]
fn workspace_scan_still_traverses_regular_directories_and_caps_results() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
    std::fs::write(root.join("nested/deeper/needle.rs"), "").unwrap();
    for index in 0..30 {
        std::fs::write(root.join(format!("match_{index:02}.txt")), "").unwrap();
    }

    let nested = scan_workspace_entries_at(root, "@needle");
    assert_eq!(
        nested,
        vec![(false, "nested/deeper/needle.rs".to_string())],
        "nested regular directories are still traversed"
    );

    let capped = scan_workspace_entries_at(root, "@match_");
    assert_eq!(capped.len(), 20, "the result cap is unchanged");
}

#[cfg(unix)]
#[test]
fn workspace_scan_does_not_follow_symlinked_directories() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside_secret.rs"), "").unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked")).unwrap();

    let entries = scan_workspace_entries_at(workspace.path(), "@outside_secret");
    assert!(entries.is_empty(), "{entries:?}");

    let linked = scan_workspace_entries_at(workspace.path(), "@linked");
    assert_eq!(
        linked,
        vec![(false, "linked".to_string())],
        "the link itself stays a non-directory candidate"
    );
}

#[cfg(unix)]
#[test]
fn workspace_scan_terminates_on_symlink_cycle_without_matches() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("a/b")).unwrap();
    std::os::unix::fs::symlink(workspace.path(), workspace.path().join("a/b/back")).unwrap();
    let root = workspace.path().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(scan_workspace_entries_at(&root, "@zzz_no_match"));
    });
    let entries = rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("completion scan did not terminate within one second");
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn arrows_recall_history_newest_first() {
    let mut app = app_with(&["first", "second", "third"]);

    app.history_previous();
    assert_eq!(typed(&app), "third");
    app.history_previous();
    assert_eq!(typed(&app), "second");
    app.history_previous();
    assert_eq!(typed(&app), "first");
    app.history_previous();
    assert_eq!(typed(&app), "first", "stays on the oldest entry");

    app.history_next();
    assert_eq!(typed(&app), "second");
    app.history_next();
    app.history_next();
    assert_eq!(
        typed(&app),
        "",
        "past the newest entry restores what was typed"
    );
}

#[test]
fn typed_prefix_filters_history_and_restores_the_draft() {
    let mut app = app_with(&["cargo test", "cargo clippy", "git status"]);
    app.input.insert_str("cargo");

    app.history_previous();
    assert_eq!(typed(&app), "cargo clippy");
    app.history_previous();
    assert_eq!(typed(&app), "cargo test");
    app.history_next();
    app.history_next();
    assert_eq!(
        typed(&app),
        "cargo",
        "draft is restored after the last match"
    );
}

#[test]
fn unmatched_prefix_leaves_the_input_alone() {
    let mut app = app_with(&["cargo test"]);
    app.input.insert_str("zig build");
    app.history_previous();
    assert_eq!(typed(&app), "zig build");
}

#[test]
fn ghost_suggestion_completes_the_typed_prefix() {
    let mut app = app_with(&["adrian"]);
    app.input.insert_str("adr");
    assert_eq!(app.history_suggestion().as_deref(), Some("adrian"));

    app.refresh_ghost();
    assert_eq!(typed(&app), "adrian", "tail is appended after the cursor");
    assert_eq!(app.input.cursor(), (0, 3), "cursor stays on the typed part");
    assert!(app.ghost_active());

    app.clear_ghost();
    assert_eq!(typed(&app), "adr");
    assert!(!app.ghost_active());
}

#[test]
fn ghost_is_skipped_when_it_would_overflow_the_box() {
    let mut app = app_with(&["adrian"]);
    app.terminal_width = 6; // 4 interior columns cannot fit "adrian"
    app.input.insert_str("adr");
    app.refresh_ghost();
    assert!(!app.ghost_active());
    assert_eq!(typed(&app), "adr");
}

#[test]
fn ghost_needs_a_prefix_longer_than_nothing() {
    let mut app = app_with(&["adrian"]);
    assert_eq!(app.history_suggestion(), None, "no ghost on empty input");

    app.input.insert_str("adrian");
    assert_eq!(app.history_suggestion(), None, "no ghost on an exact match");
}

#[test]
fn wide_characters_are_measured_in_cells() {
    let mut app = app_with(&["日本語テスト"]);
    app.terminal_width = 10; // 8 interior cells, the suggestion needs 12
    app.input.insert_str("日");
    assert!(app.history_suggestion().is_some());
    app.refresh_ghost();
    assert!(
        !app.ghost_active(),
        "a wrapped ghost could not be taken back"
    );
    assert_eq!(typed(&app), "日");
}

#[test]
fn multi_line_entries_never_grow_a_ghost() {
    let mut app = app_with(&["deploy\nstatus"]);
    app.input.insert_str("deploy");
    assert_eq!(app.history_suggestion(), None);
    app.refresh_ghost();
    assert_eq!(typed(&app), "deploy");
}

#[test]
fn ghost_prefers_the_newest_matching_entry() {
    let mut app = app_with(&["adrian", "adriana"]);
    app.input.insert_str("adri");
    assert_eq!(app.history_suggestion().as_deref(), Some("adriana"));

    let mut app = app_with(&["adriana", "adrian"]);
    app.input.insert_str("adri");
    assert_eq!(app.history_suggestion().as_deref(), Some("adrian"));
}

#[test]
fn typing_after_a_ghost_replaces_the_stale_tail() {
    let mut app = app_with(&["adrian"]);
    app.input.insert_str("adr");
    app.refresh_ghost();
    assert_eq!(typed(&app), "adrian");

    // Mirror the event loop: drop the tail, apply the key, regrow.
    app.clear_ghost();
    assert_eq!(typed(&app), "adr");
    app.input.insert_char('x');
    app.refresh_ghost();
    assert_eq!(
        typed(&app),
        "adrx",
        "no history entry matches the new prefix"
    );
}

#[test]
fn backspace_shrinks_the_ghost_completion() {
    let mut app = app_with(&["adrian"]);
    app.input.insert_str("adr");
    app.refresh_ghost();

    app.clear_ghost();
    app.input.delete_char();
    app.refresh_ghost();
    assert_eq!(app.input.cursor(), (0, 2));
    assert_eq!(typed(&app), "adrian", "\"ad\" plus the full completion");
}

#[test]
fn suggestion_hides_while_arrow_browsing_history() {
    let mut app = app_with(&["adrian"]);
    app.input.insert_str("adr");
    app.history_previous();
    assert_eq!(app.history_suggestion(), None);
}

#[test]
fn remembered_prompts_persist_across_sessions() {
    let dir = std::env::temp_dir()
        .join("seavx-tui-tests")
        .join(format!("persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut app = App::with_history(crate::history::ChatHistory::load(&dir));
    app.remember_prompt("adrian");

    let reloaded = App::with_history(crate::history::ChatHistory::load(&dir));
    assert_eq!(reloaded.history, vec!["adrian".to_string()]);
    assert_eq!(
        crate::history::ChatHistory::load(&dir).entries(),
        ["adrian"]
    );
}

#[test]
fn slash_commands_and_repeats_stay_out_of_history() {
    let mut app = test_app();
    app.remember_prompt("/model");
    app.remember_prompt("do the thing");
    app.remember_prompt("do the thing");
    app.remember_prompt("   ");
    assert_eq!(app.history, vec!["do the thing".to_string()]);
}

#[test]
fn inserting_a_file_tag_drops_the_ghost_tail() {
    let mut app = app_with(&["open @src/main.rs"]);
    app.input.insert_str("open @src");
    app.refresh_ghost();
    assert_eq!(typed(&app), "open @src/main.rs");

    app.insert_at_tag(false, "src/main.rs");
    // Mirror the event loop: the next key clears the ghost before typing.
    app.clear_ghost();
    app.input.insert_char(' ');
    assert_eq!(
        typed(&app),
        "open [file:src/main.rs]  ",
        "the tag survives instead of being cut to the old ghost index"
    );
}

#[test]
fn at_matches_are_cached_per_query() {
    let mut app = test_app();
    app.input.insert_str("@src");
    let first = app.at_matches();
    assert!(!first.is_empty(), "the checkout has a src/ directory");
    assert_eq!(
        app.at_cache
            .borrow()
            .as_ref()
            .map(|(query, _)| query.as_str()),
        Some("@src")
    );
    assert_eq!(app.at_matches(), first);
}

#[test]
fn esc_dismisses_the_slash_menu_until_the_prompt_changes() {
    let mut app = test_app();
    app.input.insert_str("/m");
    assert!(app.popup_open());
    app.dismiss_popups();
    assert!(!app.popup_open(), "the menu stays closed after Esc");
    app.input.insert_char('o');
    assert!(app.popup_open(), "typing brings the menu back");
}

#[test]
fn multiline_input_is_left_to_the_textarea() {
    let mut app = app_with(&["older prompt"]);
    app.input.insert_str("line one");
    app.input.insert_newline();
    app.input.insert_str("line two");
    app.history_previous();
    assert_eq!(typed(&app), "line one\nline two");
}

/// Run one slash command the way the event loop does.
fn run_command(app: &mut App, input: &str) {
    let command = crate::tui::commands::parse(input).expect("recognised slash command");
    crate::tui::commands::execute(app, command);
}

#[test]
fn plan_command_enters_and_leaves_plan_mode() {
    let mut app = test_app();
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Plan);
    run_command(&mut app, "/plan off");
    assert_eq!(app.mode, AgentMode::Build);
}

#[test]
fn mode_change_is_rejected_while_busy_and_says_so() {
    let mut app = test_app();
    app.status = AgentState::Streaming;
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Build);
    let notice = app.segments.iter().any(|segment| match segment {
        Segment::Text(text) => text.contains("Cannot switch mode"),
        Segment::Notice(_) | Segment::Thinking(_) => false,
    });
    assert!(
        notice,
        "a refused switch explains itself instead of vanishing"
    );
}

#[test]
fn ctrl_tab_walks_the_four_standing_modes_and_stays_silent() {
    // The header and footer already show the active mode; a switch must not add
    // a transcript row of its own.
    let mut app = test_app();
    for expected in [
        AgentMode::Plan,
        AgentMode::AcceptEdits,
        AgentMode::Auto,
        AgentMode::Build,
    ] {
        app.cycle_mode();
        assert_eq!(app.mode, expected);
        assert!(transcript(&app).is_empty(), "{}", transcript(&app));
    }

    let mut plan = test_app();
    plan.mode = AgentMode::Plan;
    run_command(&mut plan, "/plan off");
    assert_eq!(plan.mode, AgentMode::Build);
    assert!(transcript(&plan).is_empty(), "{}", transcript(&plan));
}

#[test]
fn repeating_the_mode_command_says_it_is_already_active() {
    let mut app = test_app();
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Plan);
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Plan);
    assert!(transcript(&app).contains("Plan mode is already active"));

    run_command(&mut app, "/plan off");
    assert_eq!(app.mode, AgentMode::Build);
    run_command(&mut app, "/plan off");
    assert_eq!(app.mode, AgentMode::Build);
    assert!(transcript(&app).contains("Build mode is already active"));
}

#[test]
fn a_bare_slash_is_a_noop_through_the_enter_path() {
    let mut app = test_app();
    app.input.insert_str("/");
    assert!(matches!(app.enter_action("/"), EnterAction::Nothing));
    assert_eq!(typed(&app), "", "the bare slash is dropped");
    assert!(app.segments.is_empty(), "no notice, no row");
    assert_eq!(app.status, AgentState::Idle, "no agent call");

    // The parser alone already answers None; the Enter path must agree.
    assert_eq!(crate::tui::commands::parse("/"), None);
}

#[test]
fn second_plan_command_is_idempotent() {
    let mut app = test_app();
    run_command(&mut app, "/plan");
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Plan, "the mode does not flip back");
    assert!(
        transcript(&app).contains("Plan mode is already active"),
        "the repeat explains itself instead of doing nothing silently"
    );
}

#[test]
fn mode_shortcuts_recognise_backtab_and_ctrl_tab() {
    assert!(is_mode_shortcut(&KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::NONE
    )));
    assert!(is_mode_shortcut(&KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::CONTROL
    )));
    assert!(!is_mode_shortcut(&KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE
    )));
}

fn stored(
    seq: i64,
    role: &str,
    message: &genai::chat::ChatMessage,
) -> crate::session::StoredMessage {
    crate::session::StoredMessage {
        seq,
        role: role.to_string(),
        status: crate::session::MessageStatus::Complete,
        payload: serde_json::to_string(message).unwrap(),
        created_at: 0,
    }
}

fn loaded(messages: Vec<crate::session::StoredMessage>) -> crate::session::LoadedSession {
    crate::session::LoadedSession {
        id: 7,
        slug: "svx-00000007".to_string(),
        workspace: crate::session::workspace_key(),
        title: Some("restored".to_string()),
        model: Some("fake-model".to_string()),
        payload_format: crate::session::PAYLOAD_FORMAT.to_string(),
        read_only: None,
        messages,
        total_tokens: 42,
        prompt_tokens: 21,
        pending_calls: Vec::new(),
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

#[test]
fn load_session_rebuilds_segments_in_order_and_reports_omissions() {
    let mut app = test_app();
    let session = loaded(vec![
        stored(1, "user", &genai::chat::ChatMessage::user("old question")),
        stored(
            2,
            "assistant",
            &genai::chat::ChatMessage::assistant("x".repeat(100_000)),
        ),
        stored(3, "user", &genai::chat::ChatMessage::user("new question")),
        stored(
            4,
            "assistant",
            &genai::chat::ChatMessage::assistant("new answer"),
        ),
    ]);

    app.load_session(session);

    let text = transcript(&app);
    assert!(
        text.contains("System: 2 older messages omitted (resume budget)."),
        "{text}"
    );
    assert!(!text.contains("old question"), "the window dropped it");
    let question = text.find("User: new question").expect("user row rendered");
    let answer = text.find("new answer").expect("answer rendered");
    assert!(question < answer, "segments keep stored order: {text}");
    assert_eq!(app.session_id, Some(7));
    assert_eq!(app.total_tokens, 42);
}

#[test]
fn resuming_a_session_replaces_the_previous_screen_instead_of_appending_to_it() {
    let mut app = test_app();
    app.push_text("previous session output");
    app.push_notice("previous compaction notice");
    app.session_context = Some("old summary".to_string());
    app.set_input("unsent draft from the old session");
    app.read_only = Some("archived".to_string());

    app.load_session(loaded(vec![
        stored(1, "user", &genai::chat::ChatMessage::user("new question")),
        stored(
            2,
            "assistant",
            &genai::chat::ChatMessage::assistant("answer"),
        ),
    ]));

    let text = transcript(&app);
    assert!(text.contains("User: new question"), "{text}");
    assert!(
        !text.contains("previous session output") && !text.contains("previous compaction notice"),
        "the resume starts on a clean screen: {text}"
    );
    assert!(
        app.session_context.is_none(),
        "the old summary is not the loaded session's"
    );
    assert!(app.read_only.is_none(), "a normal session is not read-only");
    assert_eq!(typed(&app), "", "the old draft does not ride along");
    assert_eq!(app.session_id, Some(7));
}

#[test]
fn a_new_session_drops_the_previous_token_usage() {
    let mut app = test_app();
    app.load_session(loaded(vec![]));
    assert_eq!(app.total_tokens, 42);

    app.start_new_session();

    assert_eq!((app.total_tokens, app.prompt_tokens), (0, 0));
}

#[test]
fn typed_letters_are_the_model_filter_and_never_reach_the_row_list() {
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

    assert_eq!(
        edit_query("", &key(KeyCode::Char('g'))),
        Some("g".to_string())
    );
    assert_eq!(
        edit_query("g", &key(KeyCode::Char('p'))),
        Some("gp".to_string())
    );
    assert_eq!(
        edit_query("gp", &key(KeyCode::Backspace)),
        Some("g".to_string())
    );
    assert_eq!(
        edit_query("", &key(KeyCode::Backspace)),
        Some(String::new())
    );
    assert_eq!(
        edit_query("g", &key(KeyCode::Up)),
        None,
        "arrows still move"
    );
    assert_eq!(
        edit_query(
            "g",
            &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
        ),
        None,
        "ctrl+r still refreshes instead of typing"
    );

    let models = ["openai_codex/gpt-6-luna", "claude-sonnet", "GPT-4o"]
        .map(str::to_string)
        .to_vec();
    let visible = visible_models(&models, "GPT");
    assert_eq!(
        visible,
        vec![&models[0], &models[2]],
        "matching is case-insensitive and untrimmed"
    );
    assert!(
        visible_models(&models, "").len() == 3,
        "an empty query keeps every row"
    );
    assert!(visible_models(&models, "zen").is_empty());
}

#[test]
fn stepping_the_model_picker_wraps_inside_the_filtered_rows() {
    let mut app = test_app();
    app.modal = picker(&["alpha", "beta", "gamma"], 2);

    // The wheel and Down are the same call.
    app.step_model_picker(true);
    assert_eq!(selected_of(&app), 0, "the last row wraps to the first");
    app.step_model_picker(false);
    assert_eq!(selected_of(&app), 2, "and back up wraps to the last");

    // One matching row: stepping cannot leave it.
    app.modal = Some(ModalSelection::Model {
        models: ["alpha", "beta", "gamma"].map(str::to_string).to_vec(),
        query: "et".to_string(),
        selected: 0,
    });
    for down in [true, false, true] {
        app.step_model_picker(down);
        assert_eq!(selected_of(&app), 0, "the only filtered row stays selected");
    }

    // No picker at all: the wheel is a no-op instead of a panic.
    app.modal = None;
    app.step_model_picker(true);
}

fn selected_of(app: &App) -> usize {
    match &app.modal {
        Some(ModalSelection::Model { selected, .. }) => *selected,
        other => panic!("the model picker is open: {other:?}"),
    }
}

#[test]
fn a_refresh_keeps_the_filter_and_the_cursor_stays_inside_it() {
    let mut app = test_app();
    app.config.model = "beta".to_string();
    app.modal = Some(ModalSelection::Model {
        models: vec!["alpha".to_string()],
        query: "et".to_string(),
        selected: 0,
    });

    app.models_refreshed(Ok(vec![
        "alpha".to_string(),
        "beta".to_string(),
        "gamma".to_string(),
    ]));

    match &app.modal {
        Some(ModalSelection::Model {
            models,
            query,
            selected,
        }) => {
            assert_eq!(query, "et", "the typed filter survives the refresh");
            assert_eq!(models.len(), 3);
            assert_eq!(
                *selected, 0,
                "beta is the only filtered row, so it is row 0"
            );
        }
        other => panic!("the picker stays open: {other:?}"),
    }
}

#[test]
fn a_pending_turn_is_prefilled_shown_once_and_replayed_never() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = crate::session::SessionStore::open_at(&path).unwrap();
    let workspace = crate::session::workspace_key();
    let id = store.create(&workspace, None, None).unwrap();
    store.record_user(id, "left hanging").unwrap();

    let mut app = test_app();
    app.attach_store(std::sync::Arc::new(std::sync::Mutex::new(
        crate::session::SessionStore::open_at(&path).unwrap(),
    )));
    app.load_session(store.load(id).unwrap());

    let text = transcript(&app);
    assert_eq!(
        text.matches("left hanging").count(),
        1,
        "the pending turn is displayed once: {text}"
    );
    assert!(
        text.contains("User: left hanging (pending, not sent)"),
        "{text}"
    );
    assert_eq!(typed(&app), "left hanging", "the input is prefilled");
    assert_eq!(app.status, AgentState::Idle, "no request was issued");

    // One Enter: exactly one new user row and exactly one request.
    let request = app
        .submit_turn(&app.input_text())
        .expect("one request for the submitted prompt");
    assert_eq!(request.prompt, "left hanging");
    let (new_id, history) =
        crate::session::prepare_turn(&store, &workspace, "fake-model", Some(id), &request.prompt)
            .unwrap();
    assert_eq!(new_id, id);
    assert!(history.is_empty(), "the unanswered rows are never replayed");

    let rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE role = 'user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2, "exactly one new user row");
    assert_eq!(
        app.input_text(),
        "",
        "the input is cleared, so a second Enter sends nothing"
    );
    assert!(app.submit_turn(&app.input_text()).is_none());
}

#[test]
fn clear_starts_a_new_session_and_keeps_the_old_one_loadable() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = crate::session::SessionStore::open_at(&path).unwrap();

    let mut app = test_app();
    app.attach_store(std::sync::Arc::new(std::sync::Mutex::new(
        crate::session::SessionStore::open_at(&path).unwrap(),
    )));
    let first = app.submit_turn("first prompt").expect("request");
    let (first_id, _) = crate::session::prepare_turn(
        &store,
        &crate::session::workspace_key(),
        "fake-model",
        None,
        &first.prompt,
    )
    .unwrap();
    app.session_id = Some(first_id);
    // The turn finished; /clear is refused while the agent is busy.
    app.status = AgentState::Idle;

    run_command(&mut app, "/clear");
    assert_eq!(app.session_id, None, "/clear starts a new session");
    assert!(app.segments.is_empty());

    let second = app.submit_turn("second prompt").expect("request");
    let (second_id, _) = crate::session::prepare_turn(
        &store,
        &crate::session::workspace_key(),
        "fake-model",
        app.session_id,
        &second.prompt,
    )
    .unwrap();
    assert_ne!(first_id, second_id);
    let old = store.load(first_id).unwrap();
    assert_eq!(old.messages.len(), 1, "the old session is still there");
    assert!(
        old.messages[0].payload.contains("first prompt"),
        "nothing was deleted"
    );
}

#[test]
fn resending_a_pending_prompt_creates_one_row_and_one_request() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = crate::session::SessionStore::open_at(&path).unwrap();
    let workspace = crate::session::workspace_key();
    let id = store.create(&workspace, None, None).unwrap();
    store.record_user(id, "retry me").unwrap();

    let mut app = test_app();
    app.attach_store(std::sync::Arc::new(std::sync::Mutex::new(
        crate::session::SessionStore::open_at(&path).unwrap(),
    )));
    app.load_session(store.load(id).unwrap());
    assert_eq!(app.status, AgentState::Idle, "nothing reaches the provider");

    let request = app
        .submit_turn(&app.input_text())
        .expect("exactly one request");
    let (same_id, history) = crate::session::prepare_turn(
        &store,
        &workspace,
        "fake-model",
        app.session_id,
        &request.prompt,
    )
    .unwrap();
    assert_eq!(same_id, id);
    assert!(history.is_empty(), "the prompt is not replayed");

    let (role, status, payload): (String, String, String) = store
        .connection()
        .query_row(
            "SELECT role, status, payload FROM messages ORDER BY seq DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((role.as_str(), status.as_str()), ("user", "complete"));
    let sent: genai::chat::ChatMessage = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        sent.content.texts().join(""),
        "retry me",
        "the text appears exactly once in the new row"
    );

    // A second Enter after the turn completes sends nothing again.
    assert!(app.submit_turn(&app.input_text()).is_none());
    let users: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE role = 'user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(users, 2, "the old unanswered row is never rewritten");
    let first: String = store
        .connection()
        .query_row("SELECT status FROM messages WHERE seq = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(first, "complete", "the stored status is untouched");
}

#[test]
fn esc_cancels_while_busy_and_quits_only_when_idle() {
    let mut busy = test_app();
    busy.status = AgentState::Streaming;
    busy.escape();
    assert!(busy.cancel.is_cancelled(), "Esc cancels the running turn");
    assert!(!busy.should_quit, "a running turn is not a reason to quit");
    assert_eq!(
        busy.status,
        AgentState::Streaming,
        "the worker owns the status"
    );

    let mut idle = test_app();
    idle.escape();
    assert!(idle.should_quit, "Esc quits when nothing is running");

    // With a popup on screen, Esc closes it and touches neither.
    let mut popup = test_app();
    popup.status = AgentState::Thinking;
    popup.input.insert_str("/he");
    assert!(popup.popup_open());
    popup.escape();
    assert!(!popup.popup_open(), "the popup closes first");
    assert!(!popup.cancel.is_cancelled(), "the turn keeps running");
    assert!(!popup.should_quit, "and the session is not ended");
}

#[test]
fn a_cancelled_turn_prints_one_line_and_returns_to_idle() {
    use crate::agent::cancel::{Cleanup, TurnStatus};

    let mut app = test_app();
    app.status = AgentState::Streaming;
    app.finish_turn(TurnStatus::Cancelled, Some(Cleanup::Confirmed), Some(4242));
    assert_eq!(app.status, AgentState::Idle);
    let text = transcript(&app);
    assert!(text.contains("System: cancelled"), "{text}");
    assert!(
        !text.contains("could not be confirmed"),
        "a confirmed kill claims nothing extra: {text}"
    );

    let mut unconfirmed = test_app();
    unconfirmed.status = AgentState::Streaming;
    unconfirmed.finish_turn(
        TurnStatus::Cancelled,
        Some(Cleanup::Unconfirmed),
        Some(4242),
    );
    let text = transcript(&unconfirmed);
    assert!(text.contains("System: cancelled"), "{text}");
    assert!(
        text.contains("cancellation could not be confirmed; pid 4242 may still be running"),
        "{text}"
    );
}

#[tokio::test]
async fn agent_done_fires_exactly_once_per_turn() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    {
        let _guard = DoneGuard(tx.clone());
    }
    assert!(matches!(rx.try_recv(), Ok(Action::AgentDone)));
    assert!(
        rx.try_recv().is_err(),
        "one turn, one AgentDone, even when the task returned early"
    );
}

// ---- Audit regressions ------------------------------------------------------

#[test]
fn a_second_prompt_is_refused_while_a_turn_runs() {
    use crate::tui::event::AgentState;

    let mut app = test_app();
    let first = app.submit_turn("first prompt").expect("the first request");
    assert_eq!(app.status, AgentState::Thinking);

    assert!(
        app.submit_turn("second prompt").is_none(),
        "one turn at a time: the running turn keeps its cancel token"
    );
    assert_eq!(
        app.status,
        AgentState::Thinking,
        "the running turn is untouched"
    );
    assert_eq!(
        transcript(&app).matches("User:").count(),
        1,
        "the refused prompt never reaches the transcript"
    );
    assert!(!first.cancel.is_cancelled());

    // The turn finished (or was cancelled): the next prompt goes through.
    app.status = AgentState::Idle;
    assert!(app.submit_turn("second prompt").is_some());
}

#[test]
fn a_title_for_another_session_does_not_rename_this_one() {
    let mut app = test_app();
    app.session_id = Some(2);

    assert!(
        !app.apply_title(Some(1), "old session".to_string()),
        "a late title belongs to the session that asked for it"
    );
    assert!(app.session_title.is_none());

    assert!(app.apply_title(Some(2), "this session".to_string()));
    assert_eq!(app.session_title.as_deref(), Some("this session"));
}

#[test]
fn the_resume_picker_lists_this_workspaces_sessions_newest_first() {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap();
    let workspace = crate::session::workspace_key();
    let mut app = test_app();

    // The rows the effect reads off the event loop, then hands to the app.
    let rows = crate::cli::sessions::session_rows(&store, &workspace).unwrap();
    assert!(
        !app.show_session_picker(rows),
        "an empty store opens no modal"
    );
    assert!(app.modal.is_none());

    for prompt in ["first prompt", "second prompt"] {
        crate::session::prepare_turn(&store, &workspace, "fake-model", None, prompt).unwrap();
    }

    let rows = crate::cli::sessions::session_rows(&store, &workspace).unwrap();
    assert!(app.show_session_picker(rows));
    let Some(ModalSelection::Session { rows, selected }) = app.modal.clone() else {
        panic!("expected the session picker, got {:?}", app.modal);
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(selected, 0, "the newest session starts highlighted");
    assert!(rows[0].starts_with("1  svx-"), "{}", rows[0]);
    assert!(rows[0].contains("second prompt"), "{}", rows[0]);
    assert!(rows[1].contains("first prompt"), "{}", rows[1]);
}

#[test]
fn the_session_verbs_run_off_thread_against_the_store() {
    use crate::tui::app::effects::run_sessions_op;
    use crate::tui::commands::SessionsOp;

    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap();
    let workspace = "ws";
    let source = store.create(workspace, None, Some("parser work")).unwrap();
    for message in [
        genai::chat::ChatMessage::user("first"),
        genai::chat::ChatMessage::assistant(genai::chat::MessageContent::from_text("one")),
        genai::chat::ChatMessage::user("second"),
        genai::chat::ChatMessage::assistant(genai::chat::MessageContent::from_text("two")),
    ] {
        store
            .record_message(source, &message, crate::session::MessageStatus::Complete)
            .unwrap();
    }
    let other = store.create(workspace, None, Some("unrelated")).unwrap();
    store
        .record_message(
            other,
            &genai::chat::ChatMessage::user("nothing here"),
            crate::session::MessageStatus::Complete,
        )
        .unwrap();
    // A session in another workspace, so the scope is proven, not assumed.
    let elsewhere = store
        .create("/other", None, Some("elsewhere notes"))
        .unwrap();
    store
        .record_message(
            elsewhere,
            &genai::chat::ChatMessage::user("elsewhere body"),
            crate::session::MessageStatus::Complete,
        )
        .unwrap();
    let before: Vec<i64> = store
        .list_with_archived(workspace, true)
        .unwrap()
        .iter()
        .map(|summary| summary.id)
        .collect();

    // Fork the source at an explicit completed boundary.
    let report = run_sessions_op(
        &store,
        workspace,
        SessionsOp::Fork {
            id: source.to_string(),
            at: Some(4),
        },
    )
    .expect("forks");
    assert!(report.starts_with("Forked session"), "{report}");
    let forked = store
        .list_with_archived(workspace, true)
        .unwrap()
        .into_iter()
        .find(|summary| !before.contains(&summary.id))
        .expect("the fork is a new session of this workspace");
    assert_eq!(
        store.load(forked.id).unwrap().messages.len(),
        4,
        "the prefix"
    );
    assert_eq!(
        store.load(source).unwrap().messages.len(),
        4,
        "the source keeps every message"
    );

    // Archive hides a row from the default list and search, never deletes it.
    let report = run_sessions_op(
        &store,
        workspace,
        SessionsOp::Archive {
            id: other.to_string(),
            archived: true,
        },
    )
    .expect("archives");
    assert!(report.contains("hidden, not deleted"), "{report}");
    let listed = crate::cli::sessions::list_text(&store, workspace, false).unwrap();
    assert!(!listed.contains("unrelated"), "{listed}");
    assert!(
        crate::cli::sessions::list_text(&store, workspace, true)
            .unwrap()
            .contains("unrelated")
    );
    assert_eq!(store.load(other).unwrap().messages.len(), 1, "no data loss");

    // Search is literal, workspace-scoped and archived-aware.
    let found = run_sessions_op(
        &store,
        workspace,
        SessionsOp::Search {
            query: "parser work".to_string(),
            all: false,
        },
    )
    .expect("searches");
    assert!(found.contains("parser work"), "{found}");
    let archived_hit = run_sessions_op(
        &store,
        workspace,
        SessionsOp::Search {
            query: "nothing here".to_string(),
            all: false,
        },
    )
    .expect("searches");
    assert!(
        archived_hit.contains("No session matches"),
        "{archived_hit}"
    );
    let with_archived = run_sessions_op(
        &store,
        workspace,
        SessionsOp::Search {
            query: "nothing here".to_string(),
            all: true,
        },
    )
    .expect("searches");
    assert!(with_archived.contains("unrelated"), "{with_archived}");
    assert!(
        run_sessions_op(
            &store,
            workspace,
            SessionsOp::Search {
                query: "elsewhere".to_string(),
                all: true,
            },
        )
        .expect("searches")
        .contains("No session matches"),
        "another workspace's session is never a hit"
    );
    assert!(
        run_sessions_op(
            &store,
            "/other",
            SessionsOp::Search {
                query: "elsewhere".to_string(),
                all: true,
            },
        )
        .expect("searches")
        .contains("elsewhere notes"),
        "its own workspace still finds it"
    );

    // A key that is not in the store is refused with what to do next.
    let unknown = run_sessions_op(
        &store,
        workspace,
        SessionsOp::Archive {
            id: "svx-missing".to_string(),
            archived: true,
        },
    )
    .unwrap_err();
    assert!(unknown.contains("/sessions"), "{unknown}");
}

#[test]
fn quitting_is_never_blocked_by_a_pending_permission() {
    let mut app = test_app();
    let (reply, _answer) = tokio::sync::oneshot::channel();
    app.pending_permission = Some(PendingPermission {
        request: crate::agent::permissions::PermissionRequest::new(
            crate::agent::permissions::ActionKind::Write,
            "write_file",
            "notes.txt",
        ),
        reply,
    });

    let ctrl = |code: KeyCode| KeyEvent::new(code, KeyModifiers::CONTROL);
    assert!(
        quit_shortcut(&mut app, &ctrl(KeyCode::Char('d'))),
        "Ctrl+D quits while a question is on screen"
    );
    assert!(app.should_quit);
    // The question was not answered: the dropped reply is the denial.
    assert!(app.pending_permission.is_some());

    let mut app = test_app();
    assert!(quit_shortcut(&mut app, &ctrl(KeyCode::Char('q'))));
    assert!(app.should_quit);
}

#[test]
fn ctrl_c_needs_a_second_press_and_a_plain_key_is_not_a_shortcut() {
    let mut app = test_app();
    let ctrl = |code: KeyCode| KeyEvent::new(code, KeyModifiers::CONTROL);

    assert!(quit_shortcut(&mut app, &ctrl(KeyCode::Char('c'))));
    assert!(!app.should_quit, "one press only warns");
    assert!(app.toast.is_some());

    assert!(quit_shortcut(&mut app, &ctrl(KeyCode::Char('c'))));
    assert!(app.should_quit, "the second press exits");

    let mut app = test_app();
    assert!(!quit_shortcut(
        &mut app,
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)
    ));
    assert!(!app.should_quit, "a bare d is still text");
}

#[test]
fn model_index_outside_the_cached_list_is_an_error_not_a_name() {
    let cached: Vec<String> = ["alpha", "beta"].iter().map(|m| m.to_string()).collect();

    assert_eq!(resolve_model_target(&cached, "1").unwrap(), "alpha");
    assert_eq!(resolve_model_target(&cached, "2").unwrap(), "beta");
    assert_eq!(
        resolve_model_target(&cached, "gpt-4o").unwrap(),
        "gpt-4o",
        "a name is never looked up in the cache"
    );

    for out_of_range in ["0", "3", "99"] {
        assert!(
            resolve_model_target(&cached, out_of_range).is_err(),
            "`{out_of_range}` must not be saved as a model name"
        );
    }
    assert!(
        resolve_model_target(&[], "1").is_err(),
        "a cold cache rejects every index"
    );
}

fn picker(models: &[&str], selected: usize) -> Option<ModalSelection> {
    Some(ModalSelection::Model {
        models: models.iter().map(|model| model.to_string()).collect(),
        query: String::new(),
        selected,
    })
}

#[test]
fn a_finished_refresh_replaces_the_open_picker_and_stops_the_spinner() {
    let mut app = test_app();
    app.config.model = "beta".to_string();
    app.modal = picker(&["alpha"], 0);
    app.refreshing_models = true;

    app.models_refreshed(Ok(vec!["alpha".to_string(), "beta".to_string()]));

    assert!(!app.refreshing_models, "the spinner stops");
    match &app.modal {
        Some(ModalSelection::Model {
            models, selected, ..
        }) => {
            assert_eq!(models, &["alpha", "beta"]);
            assert_eq!(*selected, 1, "the active model is where the cursor lands");
        }
        other => panic!("the picker stays open: {other:?}"),
    }
    let (toast, _) = app.toast.clone().expect("the count is reported");
    assert!(toast.starts_with('✓'), "{toast}");
}

#[test]
fn a_refresh_that_lands_after_the_picker_closed_only_toasts() {
    let mut app = test_app();
    app.modal = None;
    app.refreshing_models = true;

    app.models_refreshed(Ok(vec!["alpha".to_string()]));

    assert!(!app.refreshing_models);
    assert!(
        app.modal.is_none(),
        "Esc keeps the picker closed instead of it popping back open"
    );
    assert!(app.toast.is_some());
}

#[test]
fn a_failed_refresh_stops_the_spinner_and_says_why() {
    let mut app = test_app();
    app.modal = picker(&["alpha"], 0);
    app.refreshing_models = true;

    app.models_refreshed(Err("gateway returned error status: 429".to_string()));

    assert!(!app.refreshing_models);
    assert!(matches!(
        app.modal,
        Some(ModalSelection::Model { ref models, .. }) if models == &["alpha"]
    ));
    let (toast, _) = app.toast.clone().expect("the failure is reported");
    assert!(toast.contains("429"), "{toast}");
}

// ---- Phase 3 key handling ----

#[tokio::test]
async fn escape_closes_the_ghost_then_the_popup_then_cancels_the_turn() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    app.history.clear();
    app.history.push("open @src/main.rs".to_string());
    app.input.insert_str("open @src");
    app.refresh_ghost();
    app.status = crate::tui::event::AgentState::Thinking;

    assert!(app.ghost_active(), "the suggestion grew a dimmed tail");
    assert!(app.popup_open(), "the @ popup is showing");

    let esc = || KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

    super::keys::handle(&mut app, esc(), &tx).await;
    assert!(
        !app.ghost_active(),
        "the first Esc takes back the suggestion only"
    );
    assert!(app.popup_open(), "the popup survives that press");
    assert!(!app.cancel.is_cancelled(), "the turn keeps running");

    super::keys::handle(&mut app, esc(), &tx).await;
    assert!(!app.popup_open(), "the second Esc closes the popup");
    assert!(!app.cancel.is_cancelled(), "and still does not cancel");

    super::keys::handle(&mut app, esc(), &tx).await;
    assert!(app.cancel.is_cancelled(), "the third Esc cancels");
    assert_eq!(
        app.status,
        crate::tui::event::AgentState::Thinking,
        "cancelling is the worker's business, not the key handler's"
    );

    let mut idle = test_app();
    super::keys::handle(&mut idle, esc(), &tx).await;
    assert!(
        idle.should_quit,
        "Esc with nothing open and nothing running quits"
    );
}

#[tokio::test]
async fn a_modal_owns_the_arrow_keys_before_the_transcript_does() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    app.modal = picker(&["alpha", "beta", "gamma"], 1);
    app.scroll = 7;

    // Regression: the transcript must not scroll while a picker owns the arrows.
    // Shift+Up is the key the plain prompt would claim for scrolling, so a
    // picker that lost the priority moves `app.scroll` instead of the cursor.
    super::keys::handle(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
        &tx,
    )
    .await;

    assert_eq!(selected_of(&app), 0, "Up moves the picker selection");
    assert_eq!(app.scroll, 7, "and leaves the transcript where it was");
}

fn skill_picker(selected: usize, marked: Vec<bool>) -> Option<ModalSelection> {
    Some(ModalSelection::Skills {
        names: ["pdf", "charts", "deploy"].map(str::to_string).to_vec(),
        descriptions: vec![String::new(); 3],
        selected,
        marked,
    })
}

#[tokio::test]
async fn the_skill_picker_toggles_marks_and_installs_the_current_row() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    app.pending_skills = None;
    app.modal = skill_picker(1, vec![false; 3]);

    super::keys::handle(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        &tx,
    )
    .await;

    match &app.modal {
        Some(ModalSelection::Skills { marked, .. }) => assert_eq!(
            marked,
            &[false, true, false],
            "Space marks the row under the cursor"
        ),
        other => panic!("the picker stays open after a toggle: {other:?}"),
    }

    super::keys::handle(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    )
    .await;

    assert!(app.modal.is_none(), "Enter closes the picker");
    match rx.try_recv() {
        Ok(Action::SkillsInstallMarked(indices)) => {
            assert_eq!(indices, vec![1], "the marked row is what Enter installs")
        }
        other => panic!("Enter sends the pick: {other:?}"),
    }
}

#[tokio::test]
async fn escape_closes_the_skill_picker_without_installing() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    app.pending_skills = None;
    app.modal = skill_picker(0, vec![true, false, false]);

    super::keys::handle(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &tx,
    )
    .await;

    assert!(app.modal.is_none(), "Esc closes the picker");
    assert!(app.pending_skills.is_none(), "and drops the checkout");
    assert!(rx.try_recv().is_err(), "Esc installs nothing");
}

#[test]
fn submit_turn_assigns_monotonic_turn_ids() {
    let mut app = test_app();

    let first = app
        .submit_turn("first prompt")
        .expect("an idle app accepts");
    assert_eq!(first.turn_id, 1);

    // Busy: the refusal happens before a number is taken.
    let refused = app.submit_turn("while busy");
    assert!(refused.is_none());
    assert_eq!(app.turn_id, 1, "a refused turn takes no number");

    app.status = AgentState::Idle;
    let second = app.submit_turn("second prompt").expect("idle again");
    assert_eq!(second.turn_id, 2);
}

#[test]
fn a_tool_event_renders_its_outcome_and_never_completes_a_cancelled_call() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    let tool = |status, elapsed, failure| crate::agent::events::ToolEvent {
        call_id: "call-1".to_string(),
        name: "read_file".to_string(),
        summary: "note.txt".to_string(),
        status,
        failure,
        started_at: 1,
        elapsed,
    };

    super::actions::apply(
        &mut app,
        Action::AgentTool(tool(crate::agent::events::ToolStatus::Queued, None, None)),
        &tx,
    );
    let queued = crate::tui::compact::transcript_from_segments(&app.segments);
    assert!(queued.contains("⚡ read  note.txt"), "{queued}");

    super::actions::apply(
        &mut app,
        Action::AgentTool(tool(
            crate::agent::events::ToolStatus::Completed,
            Some(std::time::Duration::from_millis(430)),
            None,
        )),
        &tx,
    );
    let done = crate::tui::compact::transcript_from_segments(&app.segments);
    assert!(done.contains("completed"), "{done}");
    assert!(done.contains("0.4s"), "{done}");

    super::actions::apply(
        &mut app,
        Action::AgentTool(tool(
            crate::agent::events::ToolStatus::Cancelled,
            Some(std::time::Duration::from_millis(2100)),
            None,
        )),
        &tx,
    );
    let cancelled = crate::tui::compact::transcript_from_segments(&app.segments);
    assert!(cancelled.contains("cancelled"), "{cancelled}");
    assert_eq!(
        cancelled.matches("completed").count(),
        1,
        "the cancelled call adds no second completion: {cancelled}"
    );
}

#[test]
fn a_failed_tool_event_names_its_failure() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    super::actions::apply(
        &mut app,
        Action::AgentTool(crate::agent::events::ToolEvent {
            call_id: "call-2".to_string(),
            name: "write_file".to_string(),
            summary: "path=\"made.txt\"".to_string(),
            status: crate::agent::events::ToolStatus::Failed,
            failure: Some(crate::agent::events::FailureKind::PermissionDenied),
            started_at: 1,
            elapsed: Some(std::time::Duration::from_millis(4)),
        }),
        &tx,
    );
    let text = crate::tui::compact::transcript_from_segments(&app.segments);
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("permission_denied"), "{text}");
}

fn clone_question(source: &crate::tui::app::PendingQuestion) -> crate::tui::app::PendingQuestion {
    crate::tui::app::PendingQuestion {
        id: source.id,
        question: source.question.clone(),
        kind: source.kind,
        options: source.options.clone(),
        selected: source.selected,
        chosen: source.chosen.clone(),
        text: source.text.clone(),
        reply: None,
    }
}

#[test]
fn a_question_view_maps_each_kind_to_one_outcome() {
    use crate::session::{QuestionKind, QuestionState};
    use crate::tui::app::PendingQuestion;

    let text = PendingQuestion {
        id: 1,
        question: "name?".to_string(),
        kind: QuestionKind::Text,
        options: Vec::new(),
        selected: 0,
        chosen: Default::default(),
        text: "widget".to_string(),
        reply: None,
    };
    assert_eq!(
        text.outcome(),
        (QuestionState::Answered, "widget".to_string())
    );

    let single = PendingQuestion {
        selected: 1,
        kind: QuestionKind::Single,
        options: vec!["a".to_string(), "b".to_string()],
        text: String::new(),
        ..clone_question(&text)
    };
    assert_eq!(single.outcome(), (QuestionState::Answered, "b".to_string()));

    let multi = PendingQuestion {
        kind: QuestionKind::Multi,
        chosen: [0, 2].into_iter().collect(),
        options: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        ..clone_question(&text)
    };
    assert_eq!(
        multi.outcome(),
        (QuestionState::Answered, "a, c".to_string())
    );

    // Nothing picked is a skip, never an empty answer.
    let empty = PendingQuestion {
        kind: QuestionKind::Multi,
        chosen: Default::default(),
        ..clone_question(&text)
    };
    assert_eq!(empty.outcome(), (QuestionState::Skipped, String::new()));
}

#[test]
fn typing_into_a_text_question_keeps_the_prompt_on_screen_until_enter() {
    use crate::tui::app::PendingQuestion;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app();
    let (reply, mut answered) = tokio::sync::oneshot::channel();
    app.pending_question = Some(PendingQuestion {
        id: 4,
        question: "which file?".to_string(),
        kind: crate::session::QuestionKind::Text,
        options: Vec::new(),
        selected: 0,
        chosen: Default::default(),
        text: String::new(),
        reply: Some(reply),
    });

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert!(app.answer_question(&key(KeyCode::Char('a'))).is_none());
    assert!(app.answer_question(&key(KeyCode::Char('b'))).is_none());
    assert_eq!(app.pending_question.as_ref().unwrap().text, "ab");
    assert!(answered.try_recv().is_err(), "nothing sent before Enter");

    let notice = app.answer_question(&key(KeyCode::Enter)).unwrap();
    assert_eq!(notice, "Answered: ab");
    assert!(app.pending_question.is_none());
    assert_eq!(
        answered.try_recv().unwrap(),
        (crate::session::QuestionState::Answered, "ab".to_string())
    );
}

#[test]
fn escaping_a_question_cancels_it_instead_of_answering() {
    use crate::tui::app::PendingQuestion;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app();
    let (reply, mut answered) = tokio::sync::oneshot::channel();
    app.pending_question = Some(PendingQuestion {
        id: 5,
        question: "which file?".to_string(),
        kind: crate::session::QuestionKind::Single,
        options: vec!["a.rs".to_string()],
        selected: 0,
        chosen: Default::default(),
        text: String::new(),
        reply: Some(reply),
    });

    app.answer_question(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        answered.try_recv().unwrap(),
        (crate::session::QuestionState::Cancelled, String::new()),
        "cancel carries no answer"
    );
}

#[test]
fn answering_a_restored_question_appends_exactly_one_result() {
    use crate::session::{
        LoadedSession, MessageStatus, PAYLOAD_FORMAT, QuestionKind, SessionStore, StoredMessage,
        workspace_key,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = tempfile::TempDir::new().unwrap();
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        SessionStore::open_at(&dir.path().join("sessions.db")).unwrap(),
    ));
    let session = {
        let guard = store.lock().unwrap();
        let session = guard
            .create(&workspace_key(), None, Some("asking"))
            .unwrap();
        let call = genai::chat::ChatMessage::assistant(
            genai::chat::MessageContent::from_tool_calls(vec![
                serde_json::from_value(serde_json::json!({
                    "call_id": "call_ask",
                    "fn_name": "ask_user",
                    "fn_arguments": { "question": "Which file?" },
                }))
                .unwrap(),
            ]),
        );
        guard
            .record_message(session, &call, MessageStatus::Complete)
            .unwrap();
        guard
            .ask_question(
                &workspace_key(),
                Some(session),
                1,
                "call_ask",
                "Which file?",
                QuestionKind::Text,
                &[],
            )
            .unwrap();
        session
    };

    let mut app = test_app();
    app.store = Some(store.clone());
    let loaded = LoadedSession {
        id: session,
        slug: format!("svx-{session:08}"),
        workspace: workspace_key(),
        title: None,
        model: None,
        payload_format: PAYLOAD_FORMAT.to_string(),
        read_only: None,
        messages: Vec::<StoredMessage>::new(),
        total_tokens: 0,
        prompt_tokens: 0,
        pending_calls: vec!["call_ask".to_string()],
    };
    app.load_session(loaded);
    assert!(
        app.pending_question.is_some(),
        "the open question comes back with the session"
    );

    for c in "main.rs".chars() {
        app.answer_question(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let notice = app
        .answer_question(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(notice, "Answered: main.rs");
    assert!(app.pending_question.is_none());

    let guard = store.lock().unwrap();
    let question = guard.question(&workspace_key(), 1).unwrap();
    assert_eq!(question.state, crate::session::QuestionState::Answered);
    assert_eq!(question.answer, "main.rs");
    let loaded = guard.load(session).unwrap();
    assert!(
        loaded.pending_calls.is_empty(),
        "the question is no longer open"
    );
    let results: Vec<String> = crate::session::replay(&loaded)
        .history
        .iter()
        .flat_map(|message| message.content.tool_responses())
        .map(|response| response.call_id.clone())
        .collect();
    assert_eq!(results, vec!["call_ask".to_string()], "exactly one result");
}

#[test]
fn a_queued_prompt_is_dropped_when_the_turn_does_not_succeed() {
    use crate::agent::cancel::TurnStatus;

    let mut app = test_app();
    app.status = AgentState::Thinking;
    app.queued_prompt = Some("run the tests".to_string());

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    super::actions::apply(
        &mut app,
        crate::tui::event::Action::TurnFinished {
            status: TurnStatus::Cancelled,
            cleanup: None,
            pid: None,
        },
        &tx,
    );

    assert!(
        app.queued_prompt.is_none(),
        "a cancelled turn must not run the queue"
    );
    assert_eq!(app.status, AgentState::Idle);
    let text = transcript(&app);
    assert!(text.contains("Queued prompt dropped"), "{text}");
}

#[test]
fn criteria_can_be_satisfied_or_waived_so_a_goal_reaches_completed() {
    use crate::session::{CriterionState, GoalState};
    use crate::tui::app::effects::run_goal;
    use crate::tui::commands::GoalAction;

    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap();
    let workspace = "ws";
    let id = store.create_goal(workspace, None, "ship 0.1.3").unwrap();
    store
        .set_goal_plan(
            workspace,
            id,
            &["tests pass".to_string(), "docs updated".to_string()],
            &["write the code".to_string()],
        )
        .unwrap();
    store
        .set_goal_state(workspace, id, GoalState::Active)
        .unwrap();
    let mut start = None;

    let report = run_goal(
        &store,
        workspace,
        GoalAction::CriteriaSatisfy {
            index: 0,
            evidence: "cargo test: 535 passed".to_string(),
        },
        &mut start,
    )
    .expect("a criterion can be satisfied");
    assert!(report.contains("criterion 1"), "{report}");

    // One criterion is still pending, so completion is still refused.
    assert!(
        store
            .set_goal_state(workspace, id, GoalState::Completed)
            .is_err(),
        "a pending criterion must still block completion"
    );

    let report = run_goal(
        &store,
        workspace,
        GoalAction::CriteriaWaive {
            index: 1,
            reason: "docs land with 0.1.4".to_string(),
        },
        &mut start,
    )
    .expect("a criterion can be waived with a reason");
    assert!(report.contains("criterion 2"), "{report}");

    // A refusal names the index that does not exist instead of doing nothing.
    let refused = run_goal(
        &store,
        workspace,
        GoalAction::CriteriaWaive {
            index: 7,
            reason: "nope".to_string(),
        },
        &mut start,
    )
    .expect_err("there is no criterion 8");
    assert!(refused.contains('8'), "{refused}");

    // Completion goes through the command the user has, and the store still
    // enforces the criteria gate underneath it.
    let report = run_goal(&store, workspace, GoalAction::Complete(None), &mut start)
        .expect("every criterion is satisfied or waived");
    assert!(report.contains("completed"), "{report}");
    let goal = store.goal(workspace, id).unwrap();
    assert_eq!(goal.state, GoalState::Completed);

    // A finished goal is neither live nor draft, and must still be readable.
    let shown = run_goal(&store, workspace, GoalAction::Show(None), &mut start)
        .expect("a completed goal is still inspectable");
    assert!(shown.contains("completed"), "{shown}");
    assert!(shown.contains("satisfied"), "{shown}");
    // And the completion cannot be claimed a second time.
    assert!(
        run_goal(&store, workspace, GoalAction::Complete(None), &mut start).is_err(),
        "a completed goal never completes again"
    );
    let states: Vec<CriterionState> = goal.criteria.iter().map(|row| row.state).collect();
    assert_eq!(
        states,
        vec![CriterionState::Satisfied, CriterionState::Waived]
    );
}

#[test]
fn the_test_command_is_vetted_by_the_permission_matrix() {
    use crate::agent::engine::AgentConfig;
    use crate::agent::permissions::{ActionKind, Verdict};

    let mut config = AgentConfig {
        tool_permissions: "deny".to_string(),
        ..Default::default()
    };
    assert_eq!(
        super::actions::test_verdict(&config, AgentMode::Build, ActionKind::Execute),
        Verdict::Deny,
        "tool_permissions = deny must refuse the command /test proposes"
    );

    config.tool_permissions = "ask".to_string();
    assert_eq!(
        super::actions::test_verdict(&config, AgentMode::Build, ActionKind::Execute),
        Verdict::Ask
    );

    config.permitted_actions = vec!["execute".to_string()];
    assert_eq!(
        super::actions::test_verdict(&config, AgentMode::Build, ActionKind::Execute),
        Verdict::Allow,
        "an already-permitted category is not asked again"
    );

    // The command is classified as what it is. Labelling it a write is what
    // let a `deny` policy through before.
    let temp = tempfile::tempdir().unwrap();
    let security = crate::agent::security::WorkspaceSecurity::new(temp.path());
    assert_eq!(
        crate::agent::shell::classify("cargo test", &security).kind,
        ActionKind::Execute
    );
}

async fn wait_for_task(
    store: &std::sync::Arc<std::sync::Mutex<crate::session::SessionStore>>,
    workspace: &str,
    id: i64,
) -> crate::session::TaskState {
    for _ in 0..200 {
        {
            let guard = store.lock().unwrap();
            let goal = guard.goal(workspace, id).unwrap();
            if goal.tasks[0].state != crate::session::TaskState::Running {
                return goal.tasks[0].state;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the task state never left Running");
}

#[tokio::test]
async fn a_failed_turn_leaves_the_goal_task_blocked_not_completed() {
    use crate::agent::cancel::TurnStatus;
    use crate::session::{GoalState, TaskState};

    let temp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap(),
    ));
    let workspace = crate::session::workspace_key();
    let (id, session) = {
        let guard = store.lock().unwrap();
        let session = guard.create(&workspace, None, Some("goal turn")).unwrap();
        let id = guard
            .create_goal(&workspace, Some(session), "ship it")
            .unwrap();
        guard
            .set_goal_plan(
                &workspace,
                id,
                &["tests pass".to_string()],
                &["write the code".to_string()],
            )
            .unwrap();
        guard
            .set_goal_state(&workspace, id, GoalState::Active)
            .unwrap();
        guard
            .set_task_state(&workspace, id, 0, TaskState::Running, "")
            .unwrap();
        (id, session)
    };

    let mut app = test_app();
    app.store = Some(store.clone());
    app.session_id = Some(session);
    app.goal = Some(format!("{id} active"));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    super::actions::apply(
        &mut app,
        crate::tui::event::Action::TurnFinished {
            status: TurnStatus::Failed,
            cleanup: None,
            pid: None,
        },
        &tx,
    );

    assert_eq!(
        wait_for_task(&store, &workspace, id).await,
        TaskState::Blocked,
        "a failed turn must not record the task as completed"
    );

    // The evidence names the turn that left it blocked, so `/tasks` is honest.
    let guard = store.lock().unwrap();
    let goal = guard.goal(&workspace, id).unwrap();
    assert!(
        goal.tasks[0].evidence.contains("failed"),
        "{}",
        goal.tasks[0].evidence
    );
}

#[test]
fn a_draft_goal_can_be_planned_then_approved_through_the_commands() {
    use crate::tui::app::effects::run_goal;
    use crate::tui::commands::GoalAction;

    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap();
    let workspace = "ws";
    let mut start = None;

    run_goal(
        &store,
        workspace,
        GoalAction::New("ship the release".to_string()),
        &mut start,
    )
    .expect("a goal is drafted");
    let id = store.goals(workspace).unwrap()[0].id;

    // The editing verbs only accept a draft, so resolving to the live goal alone
    // made them unreachable: nothing could plan the goal the user had just made.
    run_goal(
        &store,
        workspace,
        GoalAction::CriteriaAdd("tests pass".to_string()),
        &mut start,
    )
    .expect("criteria add finds the draft");
    run_goal(
        &store,
        workspace,
        GoalAction::TaskAdd("write the code".to_string()),
        &mut start,
    )
    .expect("task add finds the draft");

    let goal = store.goal(workspace, id).unwrap();
    assert_eq!(goal.criteria.len(), 1, "the criterion was stored");
    assert_eq!(goal.tasks.len(), 1, "the task was stored");

    run_goal(&store, workspace, GoalAction::Approve, &mut start).expect("the draft is approved");
    assert_eq!(
        store.goal(workspace, id).unwrap().state,
        crate::session::GoalState::Active
    );
    assert!(start.is_some(), "approval hands back the turn text to run");
}

#[test]
fn test_tui_redraw_interval_tracks_live_animation_work() {
    let mut app = test_app();
    assert_eq!(
        run::redraw_interval(&app),
        std::time::Duration::from_secs(1)
    );

    app.status = AgentState::Thinking;
    assert_eq!(
        run::redraw_interval(&app),
        std::time::Duration::from_millis(16)
    );

    app.status = AgentState::Idle;
    app.refreshing_models = true;
    assert_eq!(
        run::redraw_interval(&app),
        std::time::Duration::from_millis(16)
    );

    app.refreshing_models = false;
    app.modal = Some(ModalSelection::Effort {
        model: "test".to_string(),
        options: crate::agent::engine::EFFORT_LEVELS.to_vec(),
        selected: crate::agent::engine::EFFORT_LEVELS
            .iter()
            .position(|level| *level == "max")
            .unwrap(),
    });
    assert_eq!(
        run::redraw_interval(&app),
        std::time::Duration::from_millis(16)
    );

    if let Some(ModalSelection::Effort { selected, .. }) = app.modal.as_mut() {
        *selected = 0;
    }
    assert_eq!(
        run::redraw_interval(&app),
        std::time::Duration::from_secs(1)
    );
}

#[test]
fn expired_toast_returns_to_the_idle_redraw_interval() {
    let mut app = test_app();
    app.toast = Some((
        "expired".to_string(),
        std::time::Instant::now() - std::time::Duration::from_secs(4),
    ));
    assert_eq!(
        run::redraw_interval(&app),
        std::time::Duration::from_secs(1)
    );
}
