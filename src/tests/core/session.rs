use crate::session::{
    LoadedSession, MessageStatus, PAYLOAD_FORMAT, RESUME_BUDGET_BYTES, SCHEMA_VERSION,
    SessionError, SessionStore, StoredMessage, TurnState, WRITE_BUDGET, replay,
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
