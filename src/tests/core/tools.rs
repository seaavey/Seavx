use super::*;
use crate::checkpoint::CheckpointStore;
use serde_json::json;

async fn prepare() -> (tempfile::TempDir, std::path::PathBuf, WorkspaceSecurity) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".seavx")).unwrap();
    std::fs::write(root.join("notes.txt"), "before").unwrap();
    let security = WorkspaceSecurity::new(&root);
    (temp, root, security)
}

#[tokio::test]
async fn write_file_captures_a_restorable_checkpoint() {
    let (_temp, root, security) = prepare().await;
    let registry = ToolRegistry::with_default_tools();

    registry
        .get("write_file")
        .unwrap()
        .execute(
            json!({ "path": "notes.txt", "content": "after" }),
            &security,
        )
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "after"
    );

    let checkpoint = std::fs::read_dir(root.join(".seavx/checkpoints"))
        .unwrap()
        .flatten()
        .next()
        .expect("a checkpoint directory")
        .path();
    let entries = CheckpointStore::validate_with_home(&checkpoint, &root, None).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target, root.join("notes.txt"));
    assert_eq!(entries[0].state, EntryState::Modified);
    assert_eq!(
        std::fs::read(entries[0].backup.as_ref().unwrap()).unwrap(),
        b"before"
    );
}

#[tokio::test]
async fn write_file_aborts_when_the_checkpoint_cannot_be_written() {
    let (_temp, root, security) = prepare().await;
    // A file where the checkpoint directory belongs makes capture impossible.
    std::fs::write(root.join(".seavx/checkpoints"), "not a directory").unwrap();
    let registry = ToolRegistry::with_default_tools();

    let result = registry
        .get("write_file")
        .unwrap()
        .execute(
            json!({ "path": "notes.txt", "content": "after" }),
            &security,
        )
        .await;

    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "before"
    );
}

#[tokio::test]
async fn edit_file_captures_the_pre_edit_bytes() {
    let (_temp, root, security) = prepare().await;
    let registry = ToolRegistry::with_default_tools();

    registry
        .get("edit_file")
        .unwrap()
        .execute(
            json!({ "path": "notes.txt", "old_string": "before", "new_string": "after" }),
            &security,
        )
        .await
        .unwrap();

    let checkpoint = std::fs::read_dir(root.join(".seavx/checkpoints"))
        .unwrap()
        .flatten()
        .next()
        .expect("a checkpoint directory")
        .path();
    let entries = CheckpointStore::validate_with_home(&checkpoint, &root, None).unwrap();
    assert_eq!(
        std::fs::read(entries[0].backup.as_ref().unwrap()).unwrap(),
        b"before"
    );
}

#[cfg(unix)]
fn unreadable_target(root: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("locked.txt");
    std::fs::write(&path, "secret-ish").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();
    path
}

#[tokio::test]
async fn checkpoint_never_records_created_for_an_existing_target() {
    let (_temp, root, security) = prepare().await;
    let registry = ToolRegistry::with_default_tools();

    registry
        .get("write_file")
        .unwrap()
        .execute(
            json!({ "path": "notes.txt", "content": "after" }),
            &security,
        )
        .await
        .unwrap();

    let checkpoint = std::fs::read_dir(root.join(".seavx/checkpoints"))
        .unwrap()
        .flatten()
        .next()
        .expect("a checkpoint directory")
        .path();
    let manifest = std::fs::read_to_string(checkpoint.join(crate::checkpoint::MANIFEST)).unwrap();
    let fields: Vec<&str> = manifest.trim_end().split('\t').collect();
    assert_eq!(fields[2], "modified", "{manifest}");
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_fails_closed_when_the_target_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, root, security) = prepare().await;
    let locked = unreadable_target(&root);
    let registry = ToolRegistry::with_default_tools();

    let result = registry
        .get("write_file")
        .unwrap()
        .execute(
            json!({ "path": "locked.txt", "content": "overwritten" }),
            &security,
        )
        .await;

    let error = result.unwrap_err();
    assert!(error.contains("cannot be read"), "{error}");
    assert_eq!(
        std::fs::metadata(&locked).unwrap().len() as usize,
        "secret-ish".len(),
        "the bytes are untouched"
    );
    assert!(
        !root.join(".seavx/checkpoints").exists(),
        "a refused write creates no checkpoint"
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn undo_does_not_delete_an_unreadable_target() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, root, security) = prepare().await;
    let locked = unreadable_target(&root);
    let registry = ToolRegistry::with_default_tools();

    let _ = registry
        .get("write_file")
        .unwrap()
        .execute(json!({ "path": "locked.txt", "content": "x" }), &security)
        .await;

    let restored = crate::checkpoint::CheckpointStore::undo_latest(&root).unwrap();
    assert!(restored.is_empty(), "nothing was checkpointed");
    assert!(
        locked.exists(),
        "the tool never recorded the file as created"
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn tool_output_and_logs_never_contain_a_fake_key() {
    let (_temp, root, security) = prepare().await;
    let key = "sk-live-test";
    std::fs::write(
        root.join(".seavx/config.toml"),
        format!("api_key = \"{key}\"\n"),
    )
    .unwrap();

    let registry = ToolRegistry::with_default_tools();

    // A read of the config is refused, and the refusal does not echo the key.
    let refused = registry
        .get("read_file")
        .unwrap()
        .execute(json!({ "path": ".seavx/config.toml" }), &security)
        .await
        .unwrap_err();
    assert!(
        !refused.contains(key),
        "the error echoed the key: {refused}"
    );

    // A search over the workspace returns no line from it.
    let found = registry
        .get("search_files")
        .unwrap()
        .execute(json!({ "pattern": "api_key" }), &security)
        .await
        .unwrap_or_default();
    assert!(!found.contains(key), "the search echoed the key: {found}");

    // The log sinks are written by the engine's own helpers, so a real file on
    // disk is the evidence, not the redactor's unit test.
    let debug_log = root.join(".seavx/debug.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&debug_log)
        .unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(file)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    crate::agent::engine::AgentRuntime::log_tool_call(
        "read_file",
        &format!("api_key = \"{key}\""),
        "call-1",
    );
    crate::agent::engine::AgentRuntime::log_tool_result(
        "read_file",
        true,
        &format!("Error: api_key = \"{key}\""),
    );
    crate::agent::engine::AgentRuntime::log_request_payload(
        "test-model",
        0,
        &genai::chat::ChatRequest::from_user(format!("api_key = \"{key}\"")),
        &genai::chat::ChatOptions::default(),
        &root,
    );

    let debug = std::fs::read_to_string(&debug_log).unwrap();
    assert!(
        debug.contains("Agent tool completed"),
        "the log sink received nothing: {debug}"
    );
    assert!(!debug.contains(key), "debug.log kept the key: {debug}");

    let request = std::fs::read_to_string(root.join(".seavx/request.log")).unwrap();
    assert!(
        !request.contains(key),
        "request.log kept the key: {request}"
    );
}
