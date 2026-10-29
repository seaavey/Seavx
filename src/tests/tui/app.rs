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
fn switching_modes_prints_one_line() {
    let mut app = test_app();
    app.toggle_mode();
    assert!(transcript(&app).contains("Plan mode enabled — writes are disabled"));
    // BackTab is the same call, so the shortcut cannot go silent again.
    app.toggle_mode();
    assert!(transcript(&app).contains("Build mode enabled"));

    let mut plan = test_app();
    plan.mode = AgentMode::Plan;
    run_command(&mut plan, "/plan off");
    assert!(transcript(&plan).contains("Build mode enabled"));
}

#[test]
fn repeating_the_mode_command_says_it_is_already_active() {
    let mut app = test_app();
    run_command(&mut app, "/plan");
    assert!(transcript(&app).contains("Plan mode enabled — writes are disabled"));
    run_command(&mut app, "/plan");
    assert_eq!(app.mode, AgentMode::Plan);
    assert!(transcript(&app).contains("Plan mode is already active"));

    run_command(&mut app, "/plan off");
    assert!(transcript(&app).contains("Build mode enabled"));
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
