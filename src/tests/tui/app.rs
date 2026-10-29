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
        Segment::Thinking(_) => false,
    });
    assert!(
        notice,
        "a refused switch explains itself instead of vanishing"
    );
}

#[test]
fn switching_modes_does_not_write_to_the_transcript() {
    let mut app = test_app();
    app.toggle_mode();
    app.toggle_mode();
    assert_eq!(app.mode, AgentMode::Build);
    assert!(app.segments.is_empty());
}

#[test]
fn second_plan_command_is_idempotent() {
    let mut app = test_app();
    run_command(&mut app, "/plan");
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Plan);
    assert!(app.segments.is_empty(), "no notice for a no-op switch");
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
        workspace: crate::session::workspace_key(),
        title: Some("restored".to_string()),
        model: Some("fake-model".to_string()),
        payload_format: crate::session::PAYLOAD_FORMAT.to_string(),
        read_only: None,
        messages,
        total_tokens: 42,
        prompt_tokens: 21,
    }
}

fn transcript(app: &App) -> String {
    app.segments
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) => text.clone(),
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
