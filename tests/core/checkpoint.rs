use super::*;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn workspace_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    for (rel, body) in files {
        let path = temp.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    temp
}

fn modified_input(root: &Path, rel: &str) -> CheckpointInput {
    let path = root.join(rel);
    CheckpointInput {
        target: CheckpointTarget::Workspace(path.clone()),
        state: EntryState::Modified,
        original: Some(fs::read(&path).unwrap()),
    }
}

fn manifest_of(checkpoint: &Path) -> String {
    fs::read_to_string(checkpoint.join(MANIFEST)).unwrap()
}

fn rewrite_manifest(checkpoint: &Path, manifest: &str) {
    fs::write(checkpoint.join(MANIFEST), manifest).unwrap();
}

/// Replace one field of the single manifest line.
fn tamper_field(checkpoint: &Path, index: usize, value: &str) {
    let manifest = manifest_of(checkpoint);
    let mut fields: Vec<String> = manifest
        .trim_end()
        .split('\t')
        .map(str::to_string)
        .collect();
    fields[index] = value.to_string();
    rewrite_manifest(checkpoint, &format!("{}\n", fields.join("\t")));
}

fn backup_path(checkpoint: &Path) -> PathBuf {
    let manifest = manifest_of(checkpoint);
    checkpoint.join(manifest.trim_end().split('\t').nth(1).unwrap())
}

#[test]
fn checkpoint_manifest_round_trips_all_entry_fields() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();

    let entries = CheckpointStore::validate(&checkpoint, ws.path()).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target, ws.path().join("notes.txt"));
    assert_eq!(entries[0].state, EntryState::Modified);
    let backup = entries[0].backup.as_ref().unwrap();
    assert_eq!(fs::read(backup).unwrap(), b"before");

    let manifest = manifest_of(&checkpoint);
    let fields: Vec<&str> = manifest.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 7, "{manifest}");
    assert_eq!(fields[0], "notes.txt");
    assert_eq!(fields[2], "modified");
    assert_eq!(fields[4], "6", "backup length");
    assert_eq!(fields[5], fnv1a64(b"before").to_string(), "checksum");
    assert_eq!(fields[6], "workspace");
}

#[test]
fn checkpoint_records_a_created_target_without_a_backup() {
    let ws = workspace_with(&[]);
    let created = CheckpointInput {
        target: CheckpointTarget::Workspace(ws.path().join("new.txt")),
        state: EntryState::Created,
        original: None,
    };
    let checkpoint = CheckpointStore::create(ws.path(), &[created]).unwrap();

    let entries = CheckpointStore::validate(&checkpoint, ws.path()).unwrap();

    assert_eq!(entries[0].state, EntryState::Created);
    assert!(entries[0].backup.is_none());
}

#[test]
fn checkpoint_rejects_outside_and_parent_paths() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let outside = Path::new("../../etc/passwd");

    for tampered in [outside.to_string_lossy().to_string(), {
        let absolute = tempdir().unwrap().path().join("elsewhere.txt");
        absolute.to_string_lossy().to_string()
    }] {
        let checkpoint =
            CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
        tamper_field(&checkpoint, 0, &tampered);

        let error = CheckpointStore::validate(&checkpoint, ws.path()).unwrap_err();

        assert!(
            error.to_string().starts_with("checkpoint rejected"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(ws.path().join("notes.txt")).unwrap(),
            "before"
        );
        fs::remove_dir_all(&checkpoint).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn checkpoint_rejects_backup_symlinks() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    let backup = backup_path(&checkpoint);
    let copy = ws.path().join("copy.txt");
    fs::write(&copy, "before").unwrap();
    fs::remove_file(&backup).unwrap();
    std::os::unix::fs::symlink(&copy, &backup).unwrap();

    let error = CheckpointStore::validate(&checkpoint, ws.path()).unwrap_err();

    assert!(
        error.to_string().starts_with("checkpoint rejected"),
        "{error}"
    );
}

#[test]
fn checkpoint_rejects_wrong_backup_identity() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    let backup = backup_path(&checkpoint);
    // Same bytes, different inode: the replacement is created before the original
    // is removed, so the filesystem cannot hand back the same inode.
    let replacement = ws.path().join("replacement.bin");
    fs::write(&replacement, "before").unwrap();
    fs::remove_file(&backup).unwrap();
    fs::rename(&replacement, &backup).unwrap();

    let error = CheckpointStore::validate(&checkpoint, ws.path()).unwrap_err();

    assert!(error.to_string().contains("identity"), "{error}");
}

#[test]
fn checkpoint_rejects_same_inode_backup_edit() {
    use std::io::Write;
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    let backup = backup_path(&checkpoint);
    let mut file = fs::OpenOptions::new().append(true).open(&backup).unwrap();
    file.write_all(b" tampered").unwrap();
    drop(file);

    let error = CheckpointStore::validate(&checkpoint, ws.path()).unwrap_err();

    assert!(
        error.to_string().contains("checksum") || error.to_string().contains("length"),
        "{error}"
    );
}

#[test]
fn checkpoint_rejects_truncated_backup() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    let backup = backup_path(&checkpoint);
    fs::write(&backup, "bef").unwrap();

    let error = CheckpointStore::validate(&checkpoint, ws.path()).unwrap_err();

    assert!(
        error.to_string().starts_with("checkpoint rejected"),
        "{error}"
    );
}

#[test]
fn checkpoint_rejects_a_malformed_manifest() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    rewrite_manifest(&checkpoint, "notes.txt\tbackup-0\n");

    let error = CheckpointStore::validate(&checkpoint, ws.path()).unwrap_err();

    assert!(
        error.to_string().starts_with("checkpoint rejected"),
        "{error}"
    );
}

#[test]
fn checkpoint_accepts_a_global_target_inside_the_allowlist() {
    let ws = workspace_with(&[]);
    let home = tempdir().unwrap();
    let user_memory = home.path().join(".seavx/USER.md");
    fs::create_dir_all(user_memory.parent().unwrap()).unwrap();
    fs::write(&user_memory, "old preference").unwrap();

    let input = CheckpointInput {
        target: CheckpointTarget::Global {
            path: user_memory.clone(),
            allowed_root: user_memory.clone(),
        },
        state: EntryState::Modified,
        original: Some(fs::read(&user_memory).unwrap()),
    };
    let checkpoint = CheckpointStore::create(ws.path(), &[input]).unwrap();

    let entries =
        CheckpointStore::validate_with_home(&checkpoint, ws.path(), Some(home.path())).unwrap();

    assert_eq!(entries[0].target, user_memory);
    assert_eq!(entries[0].state, EntryState::Modified);
}

#[test]
fn checkpoint_rejects_a_global_target_outside_the_allowlist() {
    let ws = workspace_with(&[]);
    let home = tempdir().unwrap();
    let outside = home.path().join(".config/elsewhere.toml");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::write(&outside, "x").unwrap();

    let input = CheckpointInput {
        target: CheckpointTarget::Global {
            path: outside.clone(),
            allowed_root: home.path().join(".config"),
        },
        state: EntryState::Modified,
        original: Some(b"x".to_vec()),
    };
    let checkpoint = CheckpointStore::create(ws.path(), &[input]).unwrap();

    let error =
        CheckpointStore::validate_with_home(&checkpoint, ws.path(), Some(home.path())).unwrap_err();

    assert!(
        error.to_string().starts_with("checkpoint rejected"),
        "{error}"
    );
}

#[test]
fn undo_restores_latest_checkpoint() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let input = modified_input(ws.path(), "notes.txt");
    CheckpointStore::create(ws.path(), &[input]).unwrap();
    fs::write(ws.path().join("notes.txt"), "after").unwrap();

    let restored = CheckpointStore::undo_latest(ws.path()).unwrap();

    assert_eq!(restored, vec![ws.path().join("notes.txt")]);
    assert_eq!(
        fs::read_to_string(ws.path().join("notes.txt")).unwrap(),
        "before"
    );
    assert!(
        fs::read_dir(ws.path().join(".seavx/checkpoints"))
            .unwrap()
            .flatten()
            .next()
            .is_none(),
        "the undone checkpoint is removed"
    );
}

#[test]
fn undo_walks_back_one_checkpoint_at_a_time() {
    let ws = workspace_with(&[("notes.txt", "first")]);
    CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    fs::write(ws.path().join("notes.txt"), "second").unwrap();
    CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    fs::write(ws.path().join("notes.txt"), "third").unwrap();

    CheckpointStore::undo_latest(ws.path()).unwrap();
    assert_eq!(
        fs::read_to_string(ws.path().join("notes.txt")).unwrap(),
        "second"
    );
    CheckpointStore::undo_latest(ws.path()).unwrap();
    assert_eq!(
        fs::read_to_string(ws.path().join("notes.txt")).unwrap(),
        "first"
    );
    assert!(CheckpointStore::undo_latest(ws.path()).unwrap().is_empty());
}

#[test]
fn undo_removes_created_files() {
    let ws = workspace_with(&[]);
    let created = CheckpointInput {
        target: CheckpointTarget::Workspace(ws.path().join("new.txt")),
        state: EntryState::Created,
        original: None,
    };
    CheckpointStore::create(ws.path(), &[created]).unwrap();
    fs::write(ws.path().join("new.txt"), "written by the agent").unwrap();

    let restored = CheckpointStore::undo_latest(ws.path()).unwrap();

    assert_eq!(restored, vec![ws.path().join("new.txt")]);
    assert!(!ws.path().join("new.txt").exists());
}

#[test]
fn undo_rejects_incomplete_checkpoint_without_changes() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    rewrite_manifest(
        &checkpoint,
        "notes.txt\t../escape.txt\tmodified\t0:0\t0\t0\tworkspace\n",
    );
    fs::write(ws.path().join("notes.txt"), "after").unwrap();

    let error = CheckpointStore::undo_latest(ws.path()).unwrap_err();

    assert!(
        error.to_string().contains("checkpoint incomplete"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(ws.path().join("notes.txt")).unwrap(),
        "after"
    );
    assert!(checkpoint.exists(), "a rejected checkpoint is kept");
}

#[test]
fn undo_restores_allowed_global_target_under_temp_home() {
    let ws = workspace_with(&[]);
    let home = tempdir().unwrap();
    let user_memory = home.path().join(".seavx/USER.md");
    fs::create_dir_all(user_memory.parent().unwrap()).unwrap();
    fs::write(&user_memory, "old preference").unwrap();
    let input = CheckpointInput {
        target: CheckpointTarget::Global {
            path: user_memory.clone(),
            allowed_root: user_memory.clone(),
        },
        state: EntryState::Modified,
        original: Some(fs::read(&user_memory).unwrap()),
    };
    CheckpointStore::create(ws.path(), &[input]).unwrap();
    fs::write(&user_memory, "new preference").unwrap();

    let restored = CheckpointStore::undo_latest_with_home(ws.path(), Some(home.path())).unwrap();

    assert_eq!(restored, vec![user_memory.clone()]);
    assert_eq!(fs::read_to_string(&user_memory).unwrap(), "old preference");
}

#[cfg(unix)]
#[test]
fn restore_rejects_a_symlinked_parent_inside_the_workspace() {
    let ws = workspace_with(&[]);
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("victim.txt"), "outside\n").unwrap();
    fs::create_dir_all(ws.path().join("link")).unwrap();
    fs::write(ws.path().join("link/victim.txt"), "inside\n").unwrap();
    let input = CheckpointInput {
        target: CheckpointTarget::Workspace(ws.path().join("link/victim.txt")),
        state: EntryState::Modified,
        original: Some(b"inside\n".to_vec()),
    };
    CheckpointStore::create(ws.path(), &[input]).unwrap();
    // The directory is swapped for a symlink after the capture, before the undo.
    std::fs::remove_dir_all(ws.path().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), ws.path().join("link")).unwrap();

    let error = CheckpointStore::undo_latest(ws.path()).unwrap_err();

    assert!(error.to_string().contains("symlinked ancestor"), "{error}");
    assert_eq!(
        fs::read_to_string(outside.path().join("victim.txt")).unwrap(),
        "outside\n"
    );
}

#[cfg(unix)]
#[test]
fn restore_rejects_a_symlinked_parent_in_a_global_root() {
    let ws = workspace_with(&[]);
    let home = tempdir().unwrap();
    let skills = home.path().join(".agents/skills");
    let skill = skills.join("link/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "old skill").unwrap();
    let input = CheckpointInput {
        target: CheckpointTarget::Global {
            path: skill.clone(),
            allowed_root: skills.clone(),
        },
        state: EntryState::Modified,
        original: Some(b"old skill".to_vec()),
    };
    let checkpoint = CheckpointStore::create(ws.path(), &[input]).unwrap();
    // The entry's parent becomes a symlink to a directory outside the root.
    let elsewhere = tempdir().unwrap();
    fs::create_dir_all(elsewhere.path().join("link")).unwrap();
    fs::write(elsewhere.path().join("link/SKILL.md"), "elsewhere").unwrap();
    fs::remove_dir_all(skills.join("link")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("link"), skills.join("link")).unwrap();

    let error = CheckpointStore::undo_latest_with_home(ws.path(), Some(home.path())).unwrap_err();

    assert!(error.to_string().contains("symlinked ancestor"), "{error}");
    assert_eq!(
        fs::read_to_string(elsewhere.path().join("link/SKILL.md")).unwrap(),
        "elsewhere"
    );
    assert!(checkpoint.exists(), "a rejected checkpoint is kept");
}

#[test]
fn restore_accepts_a_regular_nested_directory() {
    let ws = workspace_with(&[("sub/dir/file.txt", "before")]);
    CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "sub/dir/file.txt")]).unwrap();
    fs::write(ws.path().join("sub/dir/file.txt"), "after").unwrap();

    let restored = CheckpointStore::undo_latest(ws.path()).unwrap();

    assert_eq!(restored, vec![ws.path().join("sub/dir/file.txt")]);
    assert_eq!(
        fs::read_to_string(ws.path().join("sub/dir/file.txt")).unwrap(),
        "before"
    );
}

#[cfg(unix)]
#[test]
fn undo_reports_the_path_that_failed_and_keeps_the_checkpoint() {
    use std::os::unix::fs::PermissionsExt;
    let ws = workspace_with(&[("ok.txt", "old ok"), ("blocked/file.txt", "old blocked")]);
    let inputs = [
        modified_input(ws.path(), "ok.txt"),
        modified_input(ws.path(), "blocked/file.txt"),
    ];
    let checkpoint = CheckpointStore::create(ws.path(), &inputs).unwrap();
    fs::write(ws.path().join("ok.txt"), "new ok").unwrap();
    fs::write(ws.path().join("blocked/file.txt"), "new blocked").unwrap();
    let blocked_dir = ws.path().join("blocked");
    fs::set_permissions(&blocked_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let error = CheckpointStore::undo_latest(ws.path()).unwrap_err();

    let message = error.to_string();
    assert!(message.contains("blocked/file.txt"), "{message}");
    assert_eq!(error.restored(), [ws.path().join("ok.txt")]);
    assert_eq!(
        fs::read_to_string(ws.path().join("ok.txt")).unwrap(),
        "old ok"
    );
    assert!(checkpoint.exists(), "the checkpoint stays retryable");

    fs::set_permissions(&blocked_dir, fs::Permissions::from_mode(0o755)).unwrap();
}
