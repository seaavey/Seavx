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
