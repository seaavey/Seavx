use crate::session::{
    MessageStatus, PAYLOAD_FORMAT, SCHEMA_VERSION, SessionError, SessionStore, WRITE_BUDGET,
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
