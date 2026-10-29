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

#[tokio::test]
async fn command_output_is_capped_and_environment_is_cleared() {
    let (_temp, _root, security) = prepare().await;
    let registry = ToolRegistry::with_default_tools();
    let run = |command: &'static str| {
        let security = security.clone();
        let tool = ToolRegistry::with_default_tools();
        async move {
            tool.get("run_command")
                .unwrap()
                .execute(json!({ "command": command }), &security)
                .await
        }
    };
    let _ = &registry;

    let big = run("yes x | head -c 200000").await.unwrap();
    assert!(
        big.contains("bytes omitted"),
        "a 200 KB stream must be capped: {}",
        &big[big.len().saturating_sub(120)..]
    );

    // SAFETY: this test owns the variable for its lifetime; nothing else in the
    // suite reads it.
    unsafe { std::env::set_var("SEAVX_API_KEY", "sk-live-test") };
    let echoed = run("printf '%s' \"$SEAVX_API_KEY\"").await.unwrap();
    unsafe { std::env::remove_var("SEAVX_API_KEY") };
    assert!(
        !echoed.contains("sk-live-test"),
        "the agent's environment reached the child: {echoed}"
    );
    assert!(echoed.contains("stdout:"), "{echoed}");
}

#[tokio::test]
async fn recursive_search_output_withholds_protected_lines() {
    let (_temp, root, security) = prepare().await;
    for rel in ["deploy.pem", ".env", ".git/config"] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "PRIVATE_KEY here\n").unwrap();
    }
    std::fs::write(root.join("notes.md"), "PRIVATE_KEY here\n").unwrap();

    let output = ToolRegistry::with_default_tools()
        .get("run_command")
        .unwrap()
        .execute(json!({ "command": "grep -rn PRIVATE_KEY ." }), &security)
        .await
        .unwrap();

    assert!(
        output.contains("notes.md"),
        "an ordinary hit must stay: {output}"
    );
    for leaked in ["deploy.pem", ".env", ".git"] {
        assert!(!output.contains(leaked), "{leaked} leaked: {output}");
    }
    assert!(
        output.contains("lines withheld by policy"),
        "unexpected output: {output}"
    );
}

#[tokio::test]
async fn a_command_outliving_its_timeout_leaves_no_child() {
    let (_temp, root, security) = prepare().await;
    let pid_file = root.join("child.pid");
    let command = format!("echo $$ > {}; exec sleep 60", pid_file.display());

    let result = super::run_shell(&command, &security, std::time::Duration::from_millis(300)).await;
    assert!(result.is_err(), "the timeout must be reported");

    let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
    let pid = pid.trim();
    assert!(!pid.is_empty(), "the child never started");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "child {pid} survived the timeout"
    );
}

#[tokio::test]
async fn ambiguous_snippet_is_refused_with_count_and_samples() {
    let (_temp, root, security) = prepare().await;
    std::fs::write(root.join("notes.txt"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    let before = std::fs::read(root.join("notes.txt")).unwrap();

    let error = ToolRegistry::with_default_tools()
        .get("edit_file")
        .unwrap()
        .execute(
            json!({ "path": "notes.txt", "old_string": "fn", "new_string": "pub fn" }),
            &security,
        )
        .await
        .unwrap_err();

    assert!(error.contains("old_string occurs 3 times"), "{error}");
    assert!(error.contains("line 1: fn a() {}"), "{error}");
    assert!(
        error.matches("line ").count() <= 3,
        "at most three samples: {error}"
    );
    assert_eq!(
        std::fs::read(root.join("notes.txt")).unwrap(),
        before,
        "a refused edit must not touch the file"
    );
}

#[tokio::test]
async fn replace_all_changes_every_occurrence() {
    let (_temp, root, security) = prepare().await;
    std::fs::write(root.join("notes.txt"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();

    let result = ToolRegistry::with_default_tools()
        .get("edit_file")
        .unwrap()
        .execute(
            json!({ "path": "notes.txt", "old_string": "fn", "new_string": "pub fn", "replace_all": true }),
            &security,
        )
        .await
        .unwrap();

    let text = std::fs::read_to_string(root.join("notes.txt")).unwrap();
    assert_eq!(text, "pub fn a() {}\npub fn b() {}\npub fn c() {}\n");
    assert!(result.contains('3'), "the count must be reported: {result}");
}

#[tokio::test]
async fn an_empty_old_string_and_a_noop_edit_are_handled() {
    let (_temp, root, security) = prepare().await;
    std::fs::write(root.join("notes.txt"), "one line\n").unwrap();
    let tool = ToolRegistry::with_default_tools();
    let edit_file = tool.get("edit_file").unwrap();

    let error = edit_file
        .execute(
            json!({ "path": "notes.txt", "old_string": "", "new_string": "x" }),
            &security,
        )
        .await
        .unwrap_err();
    assert!(error.contains("must not be empty"), "{error}");

    let result = edit_file
        .execute(
            json!({ "path": "notes.txt", "old_string": "one line", "new_string": "one line" }),
            &security,
        )
        .await
        .unwrap();
    assert!(result.to_lowercase().contains("no change"), "{result}");
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "one line\n"
    );
}
