use crate::session::{
    INTERRUPTED_RESULT, LoadedSession, MessageStatus, PAYLOAD_FORMAT, RESUME_BUDGET_BYTES,
    SCHEMA_VERSION, SessionError, SessionStore, StoredMessage, TurnState, WRITE_BUDGET,
    export_json, render_text, replay,
};
use genai::chat::{ChatMessage, MessageContent, ToolCall, ToolResponse};

fn tool_call(call_id: &str, name: &str) -> ToolCall {
    serde_json::from_value(serde_json::json!({
        "call_id": call_id,
        "fn_name": name,
        "fn_arguments": { "path": "x" },
    }))
    .unwrap()
}

fn call_message(calls: Vec<ToolCall>) -> ChatMessage {
    ChatMessage::assistant(MessageContent::from_tool_calls(calls))
}

fn result_message(call_id: &str, content: &str) -> ChatMessage {
    ChatMessage::from(ToolResponse::new(call_id, content))
}

fn row_count(store: &SessionStore) -> i64 {
    store
        .connection()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap()
}

fn seqs(store: &SessionStore) -> Vec<i64> {
    let mut stmt = store
        .connection()
        .prepare("SELECT seq FROM messages ORDER BY seq")
        .unwrap();
    let rows = stmt.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn migration_creates_the_schema_and_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    store.migrate().unwrap();

    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let tables: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions','messages')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);

    store.migrate().unwrap();
    assert_eq!(PAYLOAD_FORMAT, "genai-chat-v1");
}

#[test]
fn a_newer_user_version_is_refused_and_the_file_is_untouched() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE marker(x); PRAGMA user_version = 99;")
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();

    match SessionStore::open_at(&path) {
        Err(SessionError::Newer { found, supported }) => {
            assert_eq!(found, 99);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Ok(_) => panic!("expected a newer-version refusal, got a store"),
        Err(other) => panic!("expected a newer-version refusal, got {other}"),
    }
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the file was touched"
    );
}

#[test]
fn garbage_bytes_do_not_panic() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    std::fs::write(&path, b"not a database").unwrap();

    assert!(SessionStore::open_at(&path).is_err());
}

#[cfg(unix)]
#[test]
fn database_and_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    // A legacy loose directory and file are tightened, not refused.
    let seavx = dir.path().join(".seavx");
    std::fs::create_dir_all(&seavx).unwrap();
    std::fs::set_permissions(&seavx, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = seavx.join("sessions.db");
    std::fs::write(&path, b"").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let _store = SessionStore::open_at(&path).unwrap();

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&seavx), 0o700);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn seq_is_allocated_inside_the_insert_transaction() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/tmp/ws", Some("model"), None).unwrap();

    let mut writers = Vec::new();
    for worker in 0..2 {
        let path = path.clone();
        writers.push(std::thread::spawn(move || {
            // A second connection, like a second process.
            let store = SessionStore::open_at(&path).unwrap();
            for index in 0..50 {
                store
                    .record_user(id, &format!("worker-{worker}-{index}"))
                    .unwrap();
            }
        }));
    }
    for writer in writers {
        writer.join().unwrap();
    }

    assert_eq!(seqs(&store), (1..=100).collect::<Vec<i64>>());
}

#[test]
fn an_assistant_tool_call_is_committed_before_its_result() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/tmp/ws", None, None).unwrap();
    store.record_user(id, "read two files").unwrap();

    let calls = call_message(vec![
        tool_call("call_a", "read_file"),
        tool_call("call_b", "read_file"),
    ]);
    store
        .record_message(id, &calls, MessageStatus::Complete)
        .unwrap();

    let before = row_count(&store);
    let error = store
        .record_message(
            id,
            &result_message("call_unknown", "nope"),
            MessageStatus::Complete,
        )
        .unwrap_err();
    assert!(matches!(error, SessionError::Refused(_)), "{error}");
    assert_eq!(row_count(&store), before, "a refused row is not inserted");

    store
        .record_message(
            id,
            &result_message("call_a", "first"),
            MessageStatus::Complete,
        )
        .unwrap();
    store
        .record_message(
            id,
            &result_message("call_b", "second"),
            MessageStatus::Complete,
        )
        .unwrap();
    assert_eq!(seqs(&store), vec![1, 2, 3, 4]);
}

#[test]
fn a_busy_database_gives_up_after_three_attempts_inside_the_deadline() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/tmp/ws", None, None).unwrap();

    let holder = rusqlite::Connection::open(&path).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let blocker = std::thread::spawn(move || {
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
        holder.execute_batch("ROLLBACK").unwrap();
    });
    ready_rx.recv().unwrap();

    let started = std::time::Instant::now();
    let error = store.record_user(id, "blocked").unwrap_err();
    let elapsed = started.elapsed();

    assert!(matches!(error, SessionError::Busy), "{error}");
    assert!(
        elapsed < WRITE_BUDGET + std::time::Duration::from_millis(500),
        "the write budget is a whole-operation deadline, took {elapsed:?}"
    );
    blocker.join().unwrap();
}

#[test]
fn record_user_creates_exactly_one_row() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/tmp/ws", None, None).unwrap();

    let seq = store.record_user(id, "hello store").unwrap();

    assert_eq!(seq, 1);
    assert_eq!(row_count(&store), 1);
    let (role, status, payload): (String, String, String) = store
        .connection()
        .query_row("SELECT role, status, payload FROM messages", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(role, "user");
    assert_eq!(status, "complete");
    let parsed: ChatMessage = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed.content.first_text(), Some("hello store"));
}

fn assistant_text(text: &str) -> ChatMessage {
    ChatMessage::assistant(text)
}

/// Derived turn state of one session, read back through `list`.
fn state_of(store: &SessionStore, workspace: &str, id: i64) -> TurnState {
    store
        .list(workspace)
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == id)
        .unwrap_or_else(|| panic!("session {id} is not listed"))
        .state
}

#[test]
fn list_filters_by_workspace_and_hides_empty_sessions() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();

    let older = store.create("/ws/a", None, Some("older")).unwrap();
    store.record_user(older, "q").unwrap();
    store
        .record_message(older, &assistant_text("a"), MessageStatus::Complete)
        .unwrap();

    let newer = store.create("/ws/a", None, Some("newer")).unwrap();
    store.record_user(newer, "q").unwrap();
    store
        .record_message(newer, &assistant_text("a"), MessageStatus::Complete)
        .unwrap();

    let empty = store.create("/ws/a", None, Some("empty")).unwrap();
    let other = store.create("/ws/b", None, Some("other")).unwrap();
    store.record_user(other, "q").unwrap();
    store
        .record_message(other, &assistant_text("a"), MessageStatus::Complete)
        .unwrap();

    // Same-second timestamps cannot order activity; pin them.
    store
        .connection()
        .execute(
            "UPDATE sessions SET updated_at = 100 WHERE id = ?1",
            [older],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE sessions SET updated_at = 200 WHERE id = ?1",
            [newer],
        )
        .unwrap();

    let listed = store.list("/ws/a").unwrap();
    let ids: Vec<i64> = listed.iter().map(|summary| summary.id).collect();
    assert_eq!(ids, vec![newer, older], "newest activity first");
    assert!(
        !ids.contains(&empty),
        "a session with no messages is not listed"
    );
    assert_eq!(listed[0].messages, 2);
    assert_eq!(listed[0].title, "newer");
    assert_eq!(listed[0].state, TurnState::Idle);
    assert!(!ids.contains(&other), "another workspace is filtered out");
}

#[test]
fn derived_state_is_idle_pending_then_interrupted() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();

    store.record_user(id, "first").unwrap();
    store
        .record_message(id, &assistant_text("answer"), MessageStatus::Complete)
        .unwrap();
    assert_eq!(state_of(&store, "/ws", id), TurnState::Idle);

    store.record_user(id, "second").unwrap();
    assert_eq!(state_of(&store, "/ws", id), TurnState::Pending);

    store
        .record_message(
            id,
            &call_message(vec![tool_call("call_a", "read_file")]),
            MessageStatus::Complete,
        )
        .unwrap();
    assert_eq!(
        state_of(&store, "/ws", id),
        TurnState::Pending,
        "an assistant tool call with no result is unanswered"
    );

    store
        .record_message(
            id,
            &result_message("call_a", "interrupted"),
            MessageStatus::Interrupted,
        )
        .unwrap();
    assert_eq!(state_of(&store, "/ws", id), TurnState::Interrupted);

    store.record_user(id, "third").unwrap();
    store
        .record_message(id, &assistant_text("done"), MessageStatus::Complete)
        .unwrap();
    assert_eq!(
        state_of(&store, "/ws", id),
        TurnState::Idle,
        "a later completed turn returns the state to idle"
    );
}

#[test]
fn a_second_store_sees_pending_while_the_first_is_mid_turn() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let writer = SessionStore::open_at(&path).unwrap();
    let id = writer.create("/ws", None, None).unwrap();
    writer.record_user(id, "still working").unwrap();

    let reader = SessionStore::open_at(&path).unwrap();
    assert_eq!(
        state_of(&reader, "/ws", id),
        TurnState::Pending,
        "a live writer is pending, never interrupted"
    );
}

fn stored(seq: i64, role: &str, status: MessageStatus, message: &ChatMessage) -> StoredMessage {
    StoredMessage {
        seq,
        role: role.to_string(),
        status,
        payload: serde_json::to_string(message).unwrap(),
        created_at: 0,
    }
}

fn loaded(messages: Vec<StoredMessage>) -> LoadedSession {
    LoadedSession {
        id: 1,
        workspace: "/ws".to_string(),
        title: None,
        model: None,
        payload_format: PAYLOAD_FORMAT.to_string(),
        read_only: None,
        messages,
        total_tokens: 0,
        prompt_tokens: 0,
    }
}

fn replayed_text(outcome: &crate::session::ReplayOutcome) -> Vec<String> {
    outcome
        .history
        .iter()
        .map(|message| message.content.texts().join(""))
        .collect()
}

#[test]
fn replay_policy_table() {
    // An answered user row is replayed; an unanswered one followed by another
    // user row is dropped even though its status is `complete`.
    let rows = vec![
        stored(1, "user", MessageStatus::Complete, &ChatMessage::user("q1")),
        stored(
            2,
            "assistant",
            MessageStatus::Complete,
            &ChatMessage::assistant("a1"),
        ),
        stored(
            3,
            "user",
            MessageStatus::Complete,
            &ChatMessage::user("unanswered"),
        ),
        stored(4, "user", MessageStatus::Complete, &ChatMessage::user("q2")),
        stored(
            5,
            "assistant",
            MessageStatus::Complete,
            &ChatMessage::assistant("a2"),
        ),
    ];
    let outcome = replay(&loaded(rows));
    assert_eq!(replayed_text(&outcome), vec!["q1", "a1", "q2", "a2"]);
    assert_eq!(outcome.draft, None);
    assert_eq!(outcome.omitted, 1);

    // The last unanswered user row becomes the draft and is never replayed.
    let rows = vec![
        stored(1, "user", MessageStatus::Complete, &ChatMessage::user("q1")),
        stored(
            2,
            "assistant",
            MessageStatus::Complete,
            &ChatMessage::assistant("a1"),
        ),
        stored(
            3,
            "user",
            MessageStatus::Complete,
            &ChatMessage::user("draft"),
        ),
    ];
    let outcome = replay(&loaded(rows));
    assert_eq!(replayed_text(&outcome), vec!["q1", "a1"]);
    assert_eq!(outcome.draft.as_deref(), Some("draft"));

    // Assistant statuses: complete and partial-with-text replay, cancelled
    // without text is skipped.
    let rows = vec![
        stored(1, "user", MessageStatus::Complete, &ChatMessage::user("q")),
        stored(
            2,
            "assistant",
            MessageStatus::Cancelled,
            &ChatMessage::assistant(""),
        ),
        stored(
            3,
            "assistant",
            MessageStatus::Partial,
            &ChatMessage::assistant("half"),
        ),
        stored(
            4,
            "assistant",
            MessageStatus::Complete,
            &ChatMessage::assistant("full"),
        ),
    ];
    let outcome = replay(&loaded(rows));
    assert_eq!(replayed_text(&outcome), vec!["q", "half", "full"]);

    // Tool rows: complete replays, interrupted replays only with its call.
    let rows = vec![
        stored(1, "user", MessageStatus::Complete, &ChatMessage::user("q")),
        stored(
            2,
            "assistant",
            MessageStatus::Complete,
            &call_message(vec![tool_call("call_a", "read_file")]),
        ),
        stored(
            3,
            "tool",
            MessageStatus::Complete,
            &result_message("call_a", "content"),
        ),
        stored(
            4,
            "assistant",
            MessageStatus::Complete,
            &call_message(vec![tool_call("call_b", "read_file")]),
        ),
        stored(
            5,
            "tool",
            MessageStatus::Interrupted,
            &result_message("call_b", "interrupted"),
        ),
    ];
    let outcome = replay(&loaded(rows));
    assert_eq!(outcome.history.len(), 5);
    assert!(outcome.synthetic.is_empty());
    assert_eq!(outcome.omitted, 0);

    // An interrupted result whose call is not in the window is dropped.
    let rows = vec![
        stored(1, "user", MessageStatus::Complete, &ChatMessage::user("q")),
        stored(
            2,
            "tool",
            MessageStatus::Interrupted,
            &result_message("call_gone", "interrupted"),
        ),
    ];
    let outcome = replay(&loaded(rows));
    assert_eq!(replayed_text(&outcome), vec!["q"]);
}

#[test]
fn the_window_starts_at_a_user_row_and_keeps_an_oversized_turn_whole() {
    let block = |size: usize, seq: i64, user: &str| {
        vec![
            stored(
                seq,
                "user",
                MessageStatus::Complete,
                &ChatMessage::user(user),
            ),
            stored(
                seq + 1,
                "assistant",
                MessageStatus::Complete,
                &ChatMessage::assistant("x".repeat(size)),
            ),
        ]
    };
    let mut rows = block(60_000, 1, "oldest");
    rows.extend(block(30_000, 3, "middle"));
    rows.extend(block(10_000, 5, "newest"));

    let outcome = replay(&loaded(rows.clone()));
    assert_eq!(
        outcome.history.first().map(|message| message.role.clone()),
        Some(genai::chat::ChatRole::User),
        "the window starts at a user row"
    );
    assert_eq!(outcome.history.len(), 4, "the oldest turn was dropped");
    assert_eq!(outcome.omitted, 2);
    let total: usize = outcome
        .history
        .iter()
        .map(|message| serde_json::to_string(message).unwrap().len())
        .sum();
    assert!(
        total <= RESUME_BUDGET_BYTES + 4096,
        "kept history stays inside the budget, was {total}"
    );

    // A single turn over the budget is kept whole, never truncated.
    let oversized = block(120_000, 1, "huge");
    let outcome = replay(&loaded(oversized));
    assert_eq!(outcome.history.len(), 2);
    assert_eq!(outcome.omitted, 0);
    assert!(
        outcome.history[1].content.texts().join("").len() == 120_000,
        "a message is never truncated"
    );
}

#[test]
fn a_pending_row_is_never_rewritten_by_replay() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "unanswered prompt").unwrap();

    let outcome = replay(&store.load(id).unwrap());
    assert_eq!(outcome.draft.as_deref(), Some("unanswered prompt"));
    assert!(outcome.history.is_empty());

    let status: String = store
        .connection()
        .query_row("SELECT status FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(status, "complete", "replay never rewrites the stored row");
}

#[test]
fn a_payload_format_mismatch_opens_read_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "old prompt").unwrap();
    store
        .connection()
        .execute(
            "UPDATE sessions SET payload_format = 'genai-0.6' WHERE id = ?1",
            [id],
        )
        .unwrap();

    let loaded = store.load(id).unwrap();
    let reason = loaded.read_only.clone().expect("read-only with a reason");
    assert!(reason.contains("genai-0.6"), "{reason}");
    assert!(
        !render_text(&loaded).is_empty(),
        "the transcript still renders"
    );

    let before = row_count(&store);
    let error = store
        .record_message(id, &ChatMessage::assistant("new"), MessageStatus::Complete)
        .unwrap_err();
    assert!(matches!(error, SessionError::ReadOnly(_)), "{error}");
    assert_eq!(row_count(&store), before, "no row is written read-only");
}

#[test]
fn recovery_writes_exactly_one_interrupted_row_per_missing_call() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "go").unwrap();
    store
        .record_message(
            id,
            &call_message(vec![
                tool_call("call_a", "read_file"),
                tool_call("call_b", "read_file"),
            ]),
            MessageStatus::Complete,
        )
        .unwrap();
    store
        .record_message(id, &result_message("call_a", "ok"), MessageStatus::Complete)
        .unwrap();

    assert_eq!(store.record_recovery(id).unwrap(), 1);
    assert_eq!(
        store.record_recovery(id).unwrap(),
        0,
        "a second recovery adds nothing"
    );

    let loaded = store.load(id).unwrap();
    let recovered: Vec<&StoredMessage> = loaded
        .messages
        .iter()
        .filter(|row| row.status == MessageStatus::Interrupted)
        .collect();
    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].payload.contains(INTERRUPTED_RESULT));

    let outcome = replay(&loaded);
    assert!(outcome.synthetic.is_empty(), "the synthetic row is stored");
    let last = outcome.history.last().unwrap();
    assert_eq!(last.content.tool_responses()[0].call_id, "call_b");
    assert!(
        export_json(&loaded).contains(INTERRUPTED_RESULT),
        "the model's view and the export agree"
    );
}

#[test]
fn export_stays_valid_json_with_an_unparseable_payload() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "kept").unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
             VALUES (?1, 99, 'assistant', 'complete', 'not json', 0)",
            [id],
        )
        .unwrap();

    let loaded = store.load(id).unwrap();
    assert!(loaded.read_only.is_some());
    let value: serde_json::Value = serde_json::from_str(&export_json(&loaded)).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["session"]["id"], id);
    let rows = value["messages"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|row| row["error"] == "unparseable message" && row["seq"] == 99),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row["seq"] == 1), "readable rows stay");
}

#[test]
fn a_full_transcript_renders_even_when_a_row_is_unreadable() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "keep me").unwrap();
    store
        .record_message(id, &assistant_text("answer"), MessageStatus::Complete)
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
             VALUES (?1, 99, 'assistant', 'complete', 'not json', 0)",
            [id],
        )
        .unwrap();

    let rendered = render_text(&store.load(id).unwrap());
    assert!(rendered.contains("User: keep me"), "{rendered}");
    assert!(rendered.contains("answer"), "{rendered}");
    assert!(
        rendered.contains("[unreadable message #99 (format mismatch)]"),
        "{rendered}"
    );
}

fn crashed(store: SessionStore, id: i64) -> i64 {
    store
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(store);
    id
}

#[test]
fn crash_after_the_user_row_leaves_one_pending_turn() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let id = {
        let store = SessionStore::open_at(&path).unwrap();
        let id = store.create("/ws", None, None).unwrap();
        store.record_user(id, "was I sent?").unwrap();
        crashed(store, id)
    };

    let store = SessionStore::open_at(&path).unwrap();
    let loaded = store.load(id).unwrap();
    let outcome = replay(&loaded);
    assert_eq!(outcome.draft.as_deref(), Some("was I sent?"));
    assert!(
        outcome.history.is_empty(),
        "an unanswered row is not history"
    );
    assert_eq!(loaded.messages.len(), 1, "no invented assistant row");
    assert_eq!(state_of(&store, "/ws", id), TurnState::Pending);
    assert_eq!(
        store.record_recovery(id).unwrap(),
        0,
        "recovery invents nothing for a pending user row"
    );
}

#[test]
fn crash_after_the_tool_call_row_writes_one_synthetic_result() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let id = {
        let store = SessionStore::open_at(&path).unwrap();
        let id = store.create("/ws", None, None).unwrap();
        store.record_user(id, "read two").unwrap();
        store
            .record_message(
                id,
                &call_message(vec![
                    tool_call("call_a", "read_file"),
                    tool_call("call_b", "read_file"),
                ]),
                MessageStatus::Complete,
            )
            .unwrap();
        crashed(store, id)
    };

    let store = SessionStore::open_at(&path).unwrap();
    assert_eq!(
        store.record_recovery(id).unwrap(),
        2,
        "one per missing call"
    );
    assert_eq!(store.record_recovery(id).unwrap(), 0, "persisted once");

    let loaded = store.load(id).unwrap();
    let outcome = replay(&loaded);
    assert!(
        outcome.synthetic.is_empty(),
        "the rows are stored, not patched"
    );
    let answered: Vec<String> = outcome
        .history
        .iter()
        .flat_map(|message| message.content.tool_responses())
        .map(|response| response.call_id.clone())
        .collect();
    assert_eq!(answered, vec!["call_a", "call_b"]);
}

#[test]
fn a_rolled_back_tool_result_leaves_the_previous_message() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "go").unwrap();
    store
        .record_message(
            id,
            &call_message(vec![tool_call("call_a", "read_file")]),
            MessageStatus::Complete,
        )
        .unwrap();
    store
        .record_message(
            id,
            &result_message("call_a", "first result"),
            MessageStatus::Complete,
        )
        .unwrap();

    // A crash mid-write: the insert never commits.
    let connection = store.connection();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
        .execute(
            "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
             VALUES (?1, 99, 'tool', 'complete', 'half a payload', 0)",
            [id],
        )
        .unwrap();
    connection.execute_batch("ROLLBACK").unwrap();

    assert_eq!(seqs(&store), vec![1, 2, 3]);
    let loaded = store.load(id).unwrap();
    assert_eq!(loaded.messages.len(), 3);
    assert!(
        loaded.messages[2].payload.contains("first result"),
        "the previous message survives"
    );
}

#[test]
fn a_held_write_lock_degrades_instead_of_blocking_list() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "listed while locked").unwrap();

    let holder = rusqlite::Connection::open(&path).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let blocker = std::thread::spawn(move || {
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
        holder.execute_batch("ROLLBACK").unwrap();
    });
    ready_rx.recv().unwrap();

    let started = std::time::Instant::now();
    let listed = store.list("/ws").unwrap();
    assert_eq!(listed.len(), 1);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "list runs under WAL while another process writes"
    );

    let error = store.record_user(id, "blocked").unwrap_err();
    assert!(matches!(error, SessionError::Busy), "{error}");
    blocker.join().unwrap();
}

#[test]
fn a_corrupt_database_disables_persistence_for_the_run() {
    use crate::tui::app::test_app;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    std::fs::write(&path, b"not a database").unwrap();
    let Err(error) = SessionStore::open_at(&path) else {
        panic!("a corrupt database must not open");
    };
    assert!(
        error.to_string().starts_with("session store unavailable"),
        "{error}"
    );

    // The TUI path: one notice, then the run continues unpersisted.
    let expected = format!("Session history disabled: {error}");
    let mut app = test_app();
    app.open_store(Err(error));
    assert!(app.store.is_none());
    let notice = app.persistence_error.clone().expect("one notice");
    assert_eq!(notice, expected);
    let request = app
        .submit_turn("still runs")
        .expect("a turn without a store");
    assert!(request.store.is_none());
}

#[test]
fn an_unparseable_payload_opens_the_session_read_only() {
    use crate::tui::app::test_app;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    store.record_user(id, "kept").unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
             VALUES (?1, 99, 'assistant', 'complete', 'not json', 0)",
            [id],
        )
        .unwrap();

    let loaded = store.load(id).unwrap();
    let reason = loaded.read_only.clone().expect("read-only with a reason");
    assert!(reason.contains("cannot be read"), "{reason}");

    // New turns are refused with the reason; export still works.
    let mut app = test_app();
    app.open_store(Ok(store));
    app.load_session(loaded);
    assert!(app.submit_turn("more").is_none());
    let transcript = app
        .segments
        .iter()
        .map(|segment| match segment {
            crate::tui::app::Segment::Text(text) => text.clone(),
            crate::tui::app::Segment::Thinking(text) => text.clone(),
        })
        .collect::<String>();
    assert!(transcript.contains("read-only"), "{transcript}");
}

#[test]
fn redaction_covers_tool_text_but_not_prompts_or_checkpoints() {
    use crate::agent::security;
    use crate::checkpoint::{CheckpointInput, CheckpointStore, CheckpointTarget, EntryState};

    let key = "sk-live-0123456789abcdef";
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open_at(&path).unwrap();
    let id = store.create("/ws", None, None).unwrap();

    // A prompt is stored verbatim: it is not tool output.
    store
        .record_user(id, &format!("my key is {key}, remember it"))
        .unwrap();
    let prompt_payload: String = store
        .connection()
        .query_row("SELECT payload FROM messages WHERE seq = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(prompt_payload.contains(key), "prompts persist verbatim");

    // The tool result is redacted before it is persisted, never at read time.
    store
        .record_message(
            id,
            &call_message(vec![tool_call("call_a", "read_file")]),
            MessageStatus::Complete,
        )
        .unwrap();
    let result = security::redact(&format!("OPENAI_API_KEY={key}"));
    store
        .record_message(
            id,
            &result_message("call_a", &result),
            MessageStatus::Complete,
        )
        .unwrap();
    let tool_payload: String = store
        .connection()
        .query_row("SELECT payload FROM messages WHERE seq = 3", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(!tool_payload.contains(key), "{tool_payload}");
    assert!(tool_payload.contains("[redacted]"));

    // A checkpoint keeps the file byte-for-byte: undo must restore it.
    let workspace = tempfile::TempDir::new().unwrap();
    let target = workspace.path().join("notes.txt");
    let bytes = format!("OPENAI_API_KEY={key}\n").into_bytes();
    std::fs::write(&target, &bytes).unwrap();
    let checkpoint = CheckpointStore::create(
        workspace.path(),
        &[CheckpointInput {
            target: CheckpointTarget::Workspace(target.clone()),
            state: EntryState::Modified,
            original: Some(bytes.clone()),
        }],
    )
    .unwrap();
    let backup = std::fs::read_dir(&checkpoint)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .find(|path| std::fs::read(path).is_ok_and(|data| data == bytes));
    assert!(
        backup.is_some(),
        "the checkpoint backup holds the original bytes"
    );
}

#[cfg(unix)]
#[test]
fn creators_tighten_existing_loose_modes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    let seavx = dir.path().join(".seavx");
    std::fs::create_dir_all(&seavx).unwrap();
    std::fs::set_permissions(&seavx, std::fs::Permissions::from_mode(0o755)).unwrap();
    for name in ["debug.log", "svx_history"] {
        let path = seavx.join(name);
        std::fs::write(&path, b"x\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    // The three creators run the way a start does.
    crate::util::prepare_log_file(&seavx).unwrap();
    crate::history::ChatHistory::load(&seavx).append("a prompt");
    SessionStore::open_at(&seavx.join("sessions.db")).unwrap();

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&seavx), 0o700, "the directory is tightened");
    for name in ["debug.log", "svx_history", "sessions.db"] {
        assert_eq!(mode(&seavx.join(name)), 0o600, "{name} is owner-only");
    }

    // A path that cannot be created reports one error instead of continuing.
    let blocker = dir.path().join("plain-file");
    std::fs::write(&blocker, b"").unwrap();
    let error = crate::util::ensure_private_dir(&blocker.join("nested"), 0o700).unwrap_err();
    assert!(!error.to_string().is_empty(), "{error}");
}

#[test]
fn a_v1_database_fixture_opens_and_migrates() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    // The file a released 0.1.1-rc.2 wrote: the v1 tables, a version stamp, and
    // one stored turn. No `.db` blob is committed.
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(super::SCHEMA).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 1;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions(workspace, title, model, payload_format, status, created_at, updated_at)
                 VALUES ('ws', 'old session', 'm', ?1, 'active', 1, 1)",
                [PAYLOAD_FORMAT],
            )
            .unwrap();
        let user = serde_json::to_string(&ChatMessage::user("old prompt")).unwrap();
        let answer = serde_json::to_string(&ChatMessage::assistant("old answer")).unwrap();
        for (seq, role, payload) in [(1, "user", user), (2, "assistant", answer)] {
            connection
                .execute(
                    "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
                     VALUES (1, ?1, ?2, 'complete', ?3, 1)",
                    rusqlite::params![seq, role, payload],
                )
                .unwrap();
        }
    }

    let store = SessionStore::open_at(&path).expect("a v1 file opens");
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let loaded = store.load(1).expect("the stored turn is readable");
    assert_eq!(loaded.title.as_deref(), Some("old session"));
    assert!(loaded.read_only.is_none(), "{:?}", loaded.read_only);
    let outcome = replay(&loaded);
    let texts: Vec<&str> = outcome
        .history
        .iter()
        .filter_map(|message| message.content.first_text())
        .collect();
    assert_eq!(texts, vec!["old prompt", "old answer"]);
}

#[test]
fn stored_payloads_round_trip_every_message_shape() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let id = store.create("ws", Some("m"), Some("shapes")).unwrap();

    let shapes = vec![
        ChatMessage::user("plain prompt"),
        ChatMessage::assistant("plain answer"),
        call_message(vec![
            tool_call("call_a", "read_file"),
            tool_call("call_b", "list_directory"),
        ]),
        result_message("call_a", "file body"),
        result_message("call_b", "listing"),
        ChatMessage::assistant("with reasoning")
            .with_reasoning_content(Some("a thought".to_string())),
    ];
    for message in &shapes {
        store
            .record_message(id, message, MessageStatus::Complete)
            .unwrap();
    }

    // Re-serialisation is stable, shape by shape.
    for original in &shapes {
        let payload = serde_json::to_string(original).unwrap();
        let parsed: ChatMessage = serde_json::from_str(&payload).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), payload);
    }

    let loaded = store.load(id).unwrap();
    let outcome = replay(&loaded);
    let calls: Vec<String> = outcome
        .history
        .iter()
        .flat_map(|message| message.content.tool_calls())
        .map(|call| call.call_id.clone())
        .collect();
    assert_eq!(
        calls,
        vec!["call_a", "call_b"],
        "the replayed call ids keep their order"
    );
    let answered: Vec<String> = outcome
        .history
        .iter()
        .flat_map(|message| message.content.tool_responses())
        .map(|response| response.call_id.clone())
        .collect();
    assert_eq!(answered, vec!["call_a", "call_b"]);
    assert!(
        outcome
            .history
            .iter()
            .any(|message| !message.content.reasoning_contents().is_empty()),
        "the reasoning part survives the round trip"
    );
    assert_eq!(
        outcome.omitted, 0,
        "nothing was dropped by the resume window"
    );
}
