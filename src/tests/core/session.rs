use crate::session::{PAYLOAD_FORMAT, SCHEMA_VERSION, SessionError, SessionStore};

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
