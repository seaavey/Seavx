use crate::agent::events::{AgentEvent, FailureKind, SUMMARY_LIMIT, ToolEvent, ToolStatus};
use crate::session::{
    CriterionState, GoalState, INTERRUPTED_RESULT, LoadedSession, MessageStatus, NewEvent,
    PAYLOAD_FORMAT, RESUME_BUDGET_BYTES, SCHEMA_VERSION, SessionError, SessionStore, StoredMessage,
    TaskState, TurnState, WRITE_BUDGET, export_json, render_text, replay,
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

    let store = SessionStore::open_at(&path).unwrap();
    // A write is what makes SQLite create the WAL and its shared-memory index.
    store.create("/tmp/ws", Some("model"), None).unwrap();

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&seavx), 0o700);
    assert_eq!(mode(&path), 0o600);
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = std::path::PathBuf::from(name);
        // The WAL exists after the write; the index may already be gone.
        if suffix == "-wal" {
            assert!(sidecar.exists(), "the write created a WAL");
        }
        if sidecar.exists() {
            assert_eq!(mode(&sidecar), 0o600, "{suffix} is owner-only");
        }
    }
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
        slug: "svx-00000001".to_string(),
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
            crate::tui::app::Segment::Text(text) | crate::tui::app::Segment::Notice(text) => {
                text.clone()
            }
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

#[test]
fn every_session_gets_a_short_svx_slug() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let first = store.create("/ws", None, None).unwrap();
    let second = store.create("/ws", None, None).unwrap();

    let mut slugs = Vec::new();
    for id in [first, second] {
        let slug = store.load(id).unwrap().slug;
        assert_eq!(slug.len(), 12, "svx- plus eight hex digits: {slug}");
        assert!(slug.starts_with("svx-"), "{slug}");
        assert!(
            slug[4..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex only: {slug}"
        );
        slugs.push(slug);
    }
    assert_ne!(slugs[0], slugs[1], "two sessions never share a name");
}

#[test]
fn a_slug_and_a_bare_number_both_resolve_to_the_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let id = store.create("/ws", None, None).unwrap();
    let slug = store.load(id).unwrap().slug;

    assert_eq!(store.id_for(&slug).unwrap(), id);
    assert_eq!(
        store.id_for(&id.to_string()).unwrap(),
        id,
        "a number typed out of habit still resolves"
    );
    assert!(matches!(
        store.id_for("svx-00000000"),
        Err(SessionError::UnknownKey(_))
    ));
    assert!(matches!(
        store.id_for("999999"),
        Err(SessionError::UnknownKey(_))
    ));
}

#[test]
fn a_v1_file_gains_a_slug_for_every_existing_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    workspace TEXT NOT NULL,
                    title TEXT,
                    model TEXT,
                    payload_format TEXT NOT NULL,
                    status TEXT NOT NULL,
                    total_tokens INTEGER NOT NULL DEFAULT 0,
                    prompt_tokens INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                INSERT INTO sessions(workspace, payload_format, status, created_at, updated_at)
                VALUES ('/ws', 'genai-chat-v1', 'active', 1, 1);
                PRAGMA user_version = 1;",
            )
            .unwrap();
    }

    let store = SessionStore::open_at(&path).unwrap();
    let slug: String = store
        .connection()
        .query_row("SELECT slug FROM sessions WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(slug.starts_with("svx-"), "the row is named: {slug}");

    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

/// The table set a 0.1.2 binary wrote: v2 is this database without
/// `agent_events`. Written out rather than taken from `SCHEMA`, which is now v3
/// and would pre-create the table the migration is supposed to add.
const V2_SCHEMA: &str = "
CREATE TABLE sessions(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL DEFAULT '',
    workspace TEXT NOT NULL,
    title TEXT,
    model TEXT,
    payload_format TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','ended')),
    total_tokens INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE messages(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'complete'
        CHECK(status IN ('complete','partial','cancelled','interrupted')),
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);
CREATE UNIQUE INDEX sessions_slug ON sessions(slug) WHERE slug <> '';
CREATE INDEX messages_session_seq ON messages(session_id, seq);
CREATE INDEX sessions_workspace_updated ON sessions(workspace, updated_at DESC);
";

#[test]
fn events_append_with_monotonic_per_session_sequence_and_survive_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let (first, second) = {
        let store = SessionStore::open_at(&path).unwrap();
        let first = store.create("ws", None, Some("first")).unwrap();
        let second = store.create("ws", None, Some("second")).unwrap();
        let mut seqs = Vec::new();
        for status in [
            ToolStatus::Queued,
            ToolStatus::Running,
            ToolStatus::Completed,
        ] {
            seqs.push(
                store
                    .record_event(
                        first,
                        NewEvent {
                            turn_id: 1,
                            kind: "tool",
                            status: status.as_str(),
                            failure: None,
                            tool_call_id: Some("call-1"),
                            summary: "read_file(note.txt)",
                            pid: None,
                        },
                    )
                    .unwrap(),
            );
        }
        assert_eq!(seqs, vec![1, 2, 3], "one sequence per session");
        let other = store
            .record_event(
                second,
                NewEvent {
                    turn_id: 1,
                    kind: "turn",
                    status: "completed",
                    failure: None,
                    tool_call_id: None,
                    summary: "",
                    pid: None,
                },
            )
            .unwrap();
        assert_eq!(other, 1, "a second session numbers its own events");
        (first, second)
    };

    let store = SessionStore::open_at(&path).unwrap();
    let stored = store.events(first).unwrap();
    assert_eq!(
        stored.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the database order survives a reopen"
    );
    assert_eq!(stored[0].status, "queued");
    assert_eq!(stored[2].status, "completed");
    assert_eq!(stored[0].kind, "tool");
    assert_eq!(stored[0].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(stored[0].turn_id, 1);
    assert_eq!(stored[0].summary, "read_file(note.txt)");
    assert_eq!(stored[0].failure, None);
    assert_eq!(store.events(second).unwrap().len(), 1);
    assert!(
        store.events(999).unwrap().is_empty(),
        "no rows for no session"
    );
}

#[test]
fn event_summaries_are_redacted_and_limited_at_the_store_boundary() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let session = store.create("ws", None, None).unwrap();

    let long = "é".repeat(900);
    store
        .record_event(
            session,
            NewEvent {
                turn_id: 1,
                kind: "tool",
                status: "queued",
                failure: None,
                tool_call_id: Some("call-1"),
                summary: &long,
                pid: None,
            },
        )
        .unwrap();
    let capped = &store.events(session).unwrap()[0].summary;
    assert_eq!(capped.chars().count(), SUMMARY_LIMIT + 1);
    assert!(capped.ends_with('…'));

    store
        .record_event(
            session,
            NewEvent {
                turn_id: 1,
                kind: "tool",
                status: "failed",
                failure: Some("tool"),
                tool_call_id: Some("call-2"),
                summary: "run_command(curl -H 'Authorization: Bearer sk-live-4f3c2b1a')",
                pid: None,
            },
        )
        .unwrap();
    let masked = &store.events(session).unwrap()[1].summary;
    assert!(!masked.contains("sk-live-4f3c2b1a"), "{masked}");
}

#[test]
fn only_lifecycle_events_are_durable_and_they_map_to_their_rows() {
    let tool = AgentEvent::Tool(ToolEvent {
        call_id: "call-7".to_string(),
        name: "run_command".to_string(),
        summary: "run_command(ls)".to_string(),
        status: ToolStatus::Failed,
        failure: Some(FailureKind::PermissionDenied),
        started_at: 1,
        elapsed: Some(std::time::Duration::from_millis(12)),
    });
    let row = NewEvent::from_event(3, &tool).expect("a tool transition is durable");
    assert_eq!(row.turn_id, 3);
    assert_eq!(row.kind, "tool");
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure, Some("permission_denied"));
    assert_eq!(row.tool_call_id, Some("call-7"));
    assert_eq!(row.summary, "run_command(ls)");
    assert_eq!(row.pid, None);

    let turn = AgentEvent::Turn {
        status: crate::agent::cancel::TurnStatus::Cancelled,
        failure: Some(FailureKind::CleanupUnconfirmed),
        cleanup: Some(crate::agent::cancel::Cleanup::Unconfirmed),
        pid: Some(4242),
    };
    let row = NewEvent::from_event(3, &turn).expect("a turn outcome is durable");
    assert_eq!(row.kind, "turn");
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.failure, Some("cleanup_unconfirmed"));
    assert_eq!(row.pid, Some(4242));

    // Presentation is not trace.
    for event in [
        AgentEvent::Token("a".to_string()),
        AgentEvent::Thinking("b".to_string()),
        AgentEvent::Notice("c".to_string()),
        AgentEvent::Usage {
            total_tokens: 1,
            prompt_tokens: 1,
        },
    ] {
        assert!(NewEvent::from_event(1, &event).is_none(), "{event:?}");
    }
}

#[test]
fn a_v2_database_fixture_migrates_to_v4_and_keeps_its_messages() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let user = serde_json::to_string(&ChatMessage::user("old prompt")).unwrap();
    let answer = serde_json::to_string(&ChatMessage::assistant("old answer")).unwrap();
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(V2_SCHEMA).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 2;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions(slug, workspace, title, model, payload_format, status, created_at, updated_at)
                 VALUES ('svx-old', 'ws', 'old session', 'm', ?1, 'active', 1, 1)",
                [PAYLOAD_FORMAT],
            )
            .unwrap();
        for (seq, role, payload) in [(1, "user", &user), (2, "assistant", &answer)] {
            connection
                .execute(
                    "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
                     VALUES (1, ?1, ?2, 'complete', ?3, 1)",
                    rusqlite::params![seq, role, payload],
                )
                .unwrap();
        }
    }

    let store = SessionStore::open_at(&path).expect("a v2 file opens");
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 4, "a v2 file lands on the current schema");

    let payloads: Vec<String> = store
        .connection()
        .prepare("SELECT payload FROM messages ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        payloads,
        vec![user, answer],
        "the transcript bytes are kept"
    );

    let loaded = store.load(1).expect("the stored turn is readable");
    assert_eq!(loaded.title.as_deref(), Some("old session"));
    let texts: Vec<String> = replay(&loaded)
        .history
        .iter()
        .filter_map(|message| message.content.first_text().map(str::to_string))
        .collect();
    assert_eq!(texts, vec!["old prompt", "old answer"]);

    store
        .record_event(
            1,
            NewEvent {
                turn_id: 1,
                kind: "turn",
                status: "completed",
                failure: None,
                tool_call_id: None,
                summary: "",
                pid: None,
            },
        )
        .expect("the new table accepts a row after the migration");
    assert_eq!(store.events(1).unwrap().len(), 1);

    // The transcript is still writable through the old path.
    store
        .record_message(1, &ChatMessage::user("new prompt"), MessageStatus::Complete)
        .unwrap();
    assert_eq!(row_count(&store), 3);
}

/// The v3 ledger, before goals existed: `kind` admits only tool and turn, and
/// every row has a session.
const V3_EVENTS: &str = "
CREATE TABLE agent_events(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    turn_id INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('tool','turn')),
    status TEXT NOT NULL,
    failure TEXT,
    tool_call_id TEXT,
    summary TEXT NOT NULL DEFAULT '',
    pid INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);
CREATE INDEX agent_events_session_seq ON agent_events(session_id, seq);
";

#[test]
fn a_v3_database_fixture_migrates_to_v4_and_preserves_messages_and_event_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let user = serde_json::to_string(&ChatMessage::user("old prompt")).unwrap();
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(V2_SCHEMA).unwrap();
        connection.execute_batch(V3_EVENTS).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 3;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions(slug, workspace, title, model, payload_format, status, created_at, updated_at)
                 VALUES ('svx-v3', 'ws', 'v3 session', 'm', ?1, 'active', 1, 1)",
                [PAYLOAD_FORMAT],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages(session_id, seq, role, status, payload, created_at)
                 VALUES (1, 1, 'user', 'complete', ?1, 1)",
                [&user],
            )
            .unwrap();
        for (seq, status) in [(1, "queued"), (2, "completed")] {
            connection
                .execute(
                    "INSERT INTO agent_events(session_id, seq, turn_id, kind, status, tool_call_id, summary, created_at)
                     VALUES (1, ?1, 1, 'tool', ?2, 'call-1', 'read_file(x)', 1)",
                    rusqlite::params![seq, status],
                )
                .unwrap();
        }
    }

    let store = SessionStore::open_at(&path).expect("a v3 file opens");
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 4);

    // Transcript bytes and ids are untouched.
    let payloads: Vec<String> = store
        .connection()
        .prepare("SELECT payload FROM messages ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(payloads, vec![user]);

    // The ledger kept every row, in order, with the new column empty.
    let events = store.events(1).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(events[1].status, "completed");
    let goal_ids: Vec<Option<i64>> = store
        .connection()
        .prepare("SELECT goal_id FROM agent_events ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(goal_ids, vec![None, None]);

    // And a goal can now be created in that file.
    store.create_goal("ws", Some(1), "ship it").unwrap();
}

#[test]
fn goal_transitions_and_events_commit_atomically_and_survive_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let id = {
        let store = SessionStore::open_at(&path).unwrap();
        let id = store.create_goal("ws", None, "ship the release").unwrap();
        assert_eq!(store.goal("ws", id).unwrap().state, GoalState::Draft);
        store
            .set_goal_plan(
                "ws",
                id,
                &["all tests pass".to_string()],
                &["write code".to_string()],
            )
            .unwrap();
        store.set_goal_state("ws", id, GoalState::Active).unwrap();
        id
    };

    let store = SessionStore::open_at(&path).unwrap();
    let goal = store.goal("ws", id).unwrap();
    assert_eq!(goal.state, GoalState::Active);
    assert_eq!(goal.criteria.len(), 1);
    assert_eq!(goal.tasks.len(), 1);

    // draft, plan, active: one event each, in that order.
    let statuses: Vec<String> = store
        .connection
        .prepare("SELECT status FROM agent_events WHERE goal_id = ?1 ORDER BY seq")
        .unwrap()
        .query_map([id], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(statuses, vec!["draft", "draft", "active"]);
}

#[test]
fn goal_and_task_queries_are_workspace_scoped() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let mine = store.create_goal("/mine", None, "mine").unwrap();
    store.create_goal("/theirs", None, "theirs").unwrap();

    assert!(
        store.goal("/theirs", mine).is_err(),
        "another workspace cannot read it"
    );
    assert!(store.goal("/mine", mine).is_ok());
    assert_eq!(store.goals("/mine").unwrap().len(), 1);
    assert_eq!(store.goals("/theirs").unwrap().len(), 1);
}

#[test]
fn a_second_active_goal_is_refused_with_the_existing_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let first = store.create_goal("ws", None, "first").unwrap();
    let second = store.create_goal("ws", None, "second").unwrap();
    store
        .set_goal_state("ws", first, GoalState::Active)
        .unwrap();

    let error = store
        .set_goal_state("ws", second, GoalState::Active)
        .unwrap_err()
        .to_string();
    assert!(error.contains(&first.to_string()), "{error}");
    assert_eq!(store.goal("ws", second).unwrap().state, GoalState::Draft);

    // Another workspace is untouched.
    let elsewhere = store.create_goal("/other", None, "elsewhere").unwrap();
    assert!(
        store
            .set_goal_state("/other", elsewhere, GoalState::Active)
            .is_ok()
    );
}

#[test]
fn illegal_transitions_and_foreign_ids_do_not_change_state() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let id = store.create_goal("ws", None, "goal").unwrap();

    // draft cannot jump to completed or paused.
    for next in [GoalState::Completed, GoalState::Paused] {
        assert!(store.set_goal_state("ws", id, next).is_err(), "{next:?}");
    }
    // A foreign workspace cannot move it either.
    assert!(
        store
            .set_goal_state("/elsewhere", id, GoalState::Active)
            .is_err()
    );
    assert!(
        store
            .set_task_state("ws", id, 0, TaskState::Completed, "x")
            .is_err()
    );
    assert_eq!(store.goal("ws", id).unwrap().state, GoalState::Draft);

    // Edit closes once the goal is active.
    store.set_goal_state("ws", id, GoalState::Active).unwrap();
    assert!(
        store
            .set_goal_plan("ws", id, &["c".to_string()], &[])
            .is_err()
    );
    // And a completed goal takes no further moves.
    store
        .set_goal_state("ws", id, GoalState::Cancelled)
        .unwrap();
    assert!(store.set_goal_state("ws", id, GoalState::Active).is_err());
}

#[test]
fn a_goal_completes_only_with_evidence_or_a_user_waiver() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let id = store.create_goal("ws", None, "finish").unwrap();
    store
        .set_goal_plan(
            "ws",
            id,
            &["tests pass".to_string(), "docs explain it".to_string()],
            &["run tests".to_string()],
        )
        .unwrap();
    store.set_goal_state("ws", id, GoalState::Active).unwrap();

    // A pending criterion blocks completion, and the task is still open.
    let error = store
        .set_goal_state("ws", id, GoalState::Completed)
        .unwrap_err()
        .to_string();
    assert!(error.contains("criterion 1"), "{error}");

    // Satisfying needs evidence; waiving needs a reason.
    assert!(
        store
            .set_criterion_state("ws", id, 0, CriterionState::Satisfied, "", None)
            .is_err()
    );
    assert!(
        store
            .set_criterion_state("ws", id, 1, CriterionState::Waived, "", None)
            .is_err()
    );
    store
        .set_criterion_state(
            "ws",
            id,
            0,
            CriterionState::Satisfied,
            "cargo test: exit 0",
            None,
        )
        .unwrap();
    store
        .set_criterion_state(
            "ws",
            id,
            1,
            CriterionState::Waived,
            "",
            Some("no docs in this repo"),
        )
        .unwrap();

    // Tasks are the ordered plan, not the gate: completion follows the
    // criteria. A task still needs evidence before it can be marked complete.
    assert!(
        store
            .set_task_state("ws", id, 0, TaskState::Completed, "")
            .is_err()
    );
    store
        .set_task_state("ws", id, 0, TaskState::Completed, "cargo test: exit 0")
        .unwrap();
    store
        .set_goal_state("ws", id, GoalState::Completed)
        .unwrap();

    let goal = store.goal("ws", id).unwrap();
    assert_eq!(goal.state, GoalState::Completed);
    assert_eq!(goal.criteria[0].evidence, "cargo test: exit 0");
    assert_eq!(
        goal.criteria[1].waiver_reason.as_deref(),
        Some("no docs in this repo")
    );
}

#[test]
fn a_goal_task_text_carries_the_objective_its_tasks_and_its_criteria() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let id = store.create_goal("ws", None, "ship v0.1.3").unwrap();
    store
        .set_goal_plan(
            "ws",
            id,
            &["tests pass".to_string()],
            &["write code".to_string(), "run tests".to_string()],
        )
        .unwrap();

    let goal = store.goal("ws", id).unwrap();
    let text = crate::session::goal_task_text(&goal);
    assert!(text.contains("ship v0.1.3"), "{text}");
    assert!(text.contains("1. write code"), "{text}");
    assert!(text.contains("2. run tests"), "{text}");
    assert!(text.contains("- tests pass"), "{text}");
    assert!(
        text.contains("do not claim a test you did not run"),
        "{text}"
    );
}

#[test]
fn tasks_are_the_plan_and_never_the_completion_gate() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    let id = store.create_goal("ws", None, "g").unwrap();
    store
        .set_goal_plan("ws", id, &["c".to_string()], &["t".to_string()])
        .unwrap();
    store.set_goal_state("ws", id, GoalState::Active).unwrap();

    let goal = store.goal("ws", id).unwrap();
    assert_eq!(goal.tasks[0].state, TaskState::Pending);
    let blocker = goal
        .completion_blocker()
        .expect("the criterion is still open");
    assert!(blocker.contains("criterion"), "{blocker}");
    assert!(
        !blocker.contains("task"),
        "tasks never gate completion: {blocker}"
    );
}
