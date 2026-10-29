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

    let entries = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target, ws.path().join("notes.txt"));
    assert_eq!(entries[0].state, EntryState::Modified);
    let backup = entries[0].backup.as_ref().unwrap();
    assert_eq!(fs::read(backup).unwrap(), b"before");

    let manifest = manifest_of(checkpoint.path());
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

    let entries = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap();

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
        tamper_field(checkpoint.path(), 0, &tampered);

        let error = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap_err();

        assert!(
            error.to_string().starts_with("checkpoint rejected"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(ws.path().join("notes.txt")).unwrap(),
            "before"
        );
        fs::remove_dir_all(checkpoint.path()).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn checkpoint_rejects_backup_symlinks() {
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    let backup = backup_path(checkpoint.path());
    let copy = ws.path().join("copy.txt");
    fs::write(&copy, "before").unwrap();
    fs::remove_file(&backup).unwrap();
    std::os::unix::fs::symlink(&copy, &backup).unwrap();

    let error = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap_err();

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
    let backup = backup_path(checkpoint.path());
    // Same bytes, different inode: the replacement is created before the original
    // is removed, so the filesystem cannot hand back the same inode.
    let replacement = ws.path().join("replacement.bin");
    fs::write(&replacement, "before").unwrap();
    fs::remove_file(&backup).unwrap();
    fs::rename(&replacement, &backup).unwrap();

    let error = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap_err();

    assert!(error.to_string().contains("identity"), "{error}");
}

#[test]
fn checkpoint_rejects_same_inode_backup_edit() {
    use std::io::Write;
    let ws = workspace_with(&[("notes.txt", "before")]);
    let checkpoint =
        CheckpointStore::create(ws.path(), &[modified_input(ws.path(), "notes.txt")]).unwrap();
    let backup = backup_path(checkpoint.path());
    let mut file = fs::OpenOptions::new().append(true).open(&backup).unwrap();
    file.write_all(b" tampered").unwrap();
    drop(file);

    let error = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap_err();

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
    let backup = backup_path(checkpoint.path());
    fs::write(&backup, "bef").unwrap();

    let error = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap_err();

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
    rewrite_manifest(checkpoint.path(), "notes.txt\tbackup-0\n");

    let error = CheckpointStore::validate(checkpoint.path(), ws.path()).unwrap_err();

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
        CheckpointStore::validate_with_home(checkpoint.path(), ws.path(), Some(home.path()))
            .unwrap();

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
        CheckpointStore::validate_with_home(checkpoint.path(), ws.path(), Some(home.path()))
            .unwrap_err();

    assert!(
        error.to_string().starts_with("checkpoint rejected"),
        "{error}"
    );
}
