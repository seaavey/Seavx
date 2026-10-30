use crate::agent::batch::{BatchError, FileEdit, MAX_BATCH_FILES, commit, commit_with_hook};
use crate::agent::security::WorkspaceSecurity;
use std::path::{Path, PathBuf};

fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        let path = temp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    std::fs::create_dir_all(temp.path().join(".seavx")).unwrap();
    temp
}

fn edit(root: &Path, rel: &str, content: &str) -> FileEdit {
    FileEdit {
        path: root.join(rel),
        content: content.as_bytes().to_vec(),
    }
}

fn checkpoint_dirs(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join(".seavx/checkpoints"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

fn temp_litter(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().contains("seavx-tmp"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn manifest_of(root: &Path) -> String {
    let Some(directory) = checkpoint_dirs(root).into_iter().next() else {
        return String::new();
    };
    std::fs::read_to_string(directory.join("manifest")).unwrap_or_default()
}

#[test]
fn preflight_counts_backup_bytes_and_new_payloads() {
    let ws = workspace(&[("a.txt", "a\n")]);
    let security = WorkspaceSecurity::new(ws.path());

    // Too many files.
    let many: Vec<FileEdit> = (0..=MAX_BATCH_FILES)
        .map(|index| edit(ws.path(), &format!("f{index}.txt"), "x\n"))
        .collect();
    let error = commit(&security, many).unwrap_err();
    assert!(
        format!("{error}").contains(&format!("exceeds {MAX_BATCH_FILES} files")),
        "{error}"
    );

    // Backups plus payloads over the staged cap, with nothing created.
    let mut heavy = Vec::new();
    for index in 0..9 {
        let name = format!("big{index}.bin");
        std::fs::write(ws.path().join(&name), vec![b'o'; 8 * 1024 * 1024]).unwrap();
        heavy.push(FileEdit {
            path: ws.path().join(&name),
            content: vec![b'n'; 8 * 1024 * 1024],
        });
    }
    let error = commit(&security, heavy).unwrap_err();
    let message = format!("{error}");
    assert!(message.contains("backup bytes"), "{message}");
    assert!(message.contains("new bytes"), "{message}");
    assert!(
        checkpoint_dirs(ws.path()).is_empty(),
        "a checkpoint was created"
    );
    assert!(temp_litter(ws.path()).is_empty(), "a temp was created");
    assert_eq!(
        std::fs::read(ws.path().join("big0.bin")).unwrap(),
        vec![b'o'; 8 * 1024 * 1024]
    );
}

#[test]
fn a_batch_lands_every_file_and_records_committed() {
    let ws = workspace(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let security = WorkspaceSecurity::new(ws.path());

    let report = commit(
        &security,
        vec![
            edit(ws.path(), "a.txt", "A\n"),
            edit(ws.path(), "b.txt", "B\n"),
        ],
    )
    .unwrap();

    assert_eq!(report.written.len(), 2);
    assert_eq!(
        std::fs::read_to_string(ws.path().join("a.txt")).unwrap(),
        "A\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join("b.txt")).unwrap(),
        "B\n"
    );
    assert!(
        manifest_of(ws.path()).contains("state\tcommitted"),
        "{}",
        manifest_of(ws.path())
    );
    assert!(temp_litter(ws.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_failed_rename_restores_everything_already_written() {
    use std::os::unix::fs::PermissionsExt;

    // `sub/c.txt` stays readable, so the identity re-check passes and only the
    // rename fails — the rollback then runs in a writable directory.
    let ws = workspace(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("sub/c.txt", "c\n")]);
    let security = WorkspaceSecurity::new(ws.path());
    let root = ws.path().to_path_buf();
    let sub = root.join("sub");

    let hook = |event: crate::agent::batch::Hook| {
        if let crate::agent::batch::Hook::Renamed(0) = event {
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
    };
    let result = commit_with_hook(
        &security,
        vec![
            edit(&root, "a.txt", "A\n"),
            edit(&root, "b.txt", "B\n"),
            edit(&root, "sub/c.txt", "C\n"),
        ],
        &hook,
    );
    let _ = std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755));

    match result {
        Err(BatchError::Commit {
            restored,
            partial,
            checkpoint,
            ..
        }) => {
            assert!(!partial, "a working rollback is not partial");
            assert!(restored.contains(&root.join("a.txt")), "{restored:?}");
            assert!(restored.contains(&root.join("b.txt")), "{restored:?}");
            assert!(checkpoint.exists(), "the checkpoint is kept");
        }
        other => panic!("expected a commit failure, got {other:?}"),
    }

    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b\n");
    assert_eq!(std::fs::read_to_string(sub.join("c.txt")).unwrap(), "c\n");
    assert!(
        manifest_of(&root).contains("state\trolled_back"),
        "{}",
        manifest_of(&root)
    );
    assert!(temp_litter(&root).is_empty(), "temp litter left behind");
}

#[cfg(unix)]
#[test]
fn a_restore_failure_keeps_the_checkpoint_and_says_so() {
    use std::os::unix::fs::PermissionsExt;

    let ws = workspace(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let security = WorkspaceSecurity::new(ws.path());
    let root = ws.path().to_path_buf();

    // The same read-only directory breaks both the rename and its restore, which
    // is exactly the `partial` case: the checkpoint has to survive.
    let hook = |event: crate::agent::batch::Hook| {
        if let crate::agent::batch::Hook::Renamed(0) = event {
            let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555));
        }
    };
    let result = commit_with_hook(
        &security,
        vec![edit(&root, "a.txt", "A\n"), edit(&root, "b.txt", "B\n")],
        &hook,
    );
    let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755));

    match result {
        Err(BatchError::Commit {
            partial,
            checkpoint,
            ..
        }) => {
            assert!(partial, "a failed restore must report partial");
            assert!(checkpoint.exists(), "the checkpoint must be kept");
            assert!(format!("{}", BatchError::Stage(String::new())).contains("staging"));
        }
        other => panic!("expected a partial commit, got {other:?}"),
    }
}

#[test]
fn an_in_place_edit_after_staging_aborts_the_commit() {
    let ws = workspace(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let security = WorkspaceSecurity::new(ws.path());
    let root = ws.path().to_path_buf();

    // Same inode, same mode, different bytes: identity alone would miss it.
    let hook = |event: crate::agent::batch::Hook| {
        if let crate::agent::batch::Hook::CheckpointWritten = event {
            std::fs::write(root.join("b.txt"), "edited by someone else\n").unwrap();
        }
    };
    let result = commit_with_hook(
        &security,
        vec![edit(&root, "a.txt", "A\n"), edit(&root, "b.txt", "B\n")],
        &hook,
    );

    match result {
        Err(BatchError::Conflict { paths }) => {
            assert_eq!(paths, vec![root.join("b.txt")]);
        }
        other => panic!("expected a conflict, got {other:?}"),
    }

    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "edited by someone else\n"
    );
    assert!(temp_litter(&root).is_empty());
}

#[test]
fn deleting_a_temp_never_touches_a_planted_file() {
    let ws = workspace(&[("a.txt", "a\n")]);
    let security = WorkspaceSecurity::new(ws.path());
    let root = ws.path().to_path_buf();
    let planted = root.join("planted.seavx-tmp-decoy");
    std::fs::write(&planted, "keep me").unwrap();

    let hook = |event: crate::agent::batch::Hook| {
        if let crate::agent::batch::Hook::CheckpointWritten = event {
            std::fs::write(root.join("a.txt"), "moved on\n").unwrap();
        }
    };
    let _ = commit_with_hook(&security, vec![edit(&root, "a.txt", "A\n")], &hook);

    assert_eq!(std::fs::read_to_string(&planted).unwrap(), "keep me");
    assert!(temp_litter(&root).iter().all(|path| path == &planted));
}

// ---- Phase 2 task 7/8: recovery and retention ------------------------------

fn interrupted_workspace() -> (tempfile::TempDir, WorkspaceSecurity) {
    let ws = workspace(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    let security = WorkspaceSecurity::new(ws.path());
    ws.path()
        .to_path_buf()
        .join(".seavx")
        .exists()
        .then_some(())
        .expect(".seavx exists");
    (ws, security)
}

#[test]
fn a_crash_after_the_first_rename_is_reported_with_every_target() {
    use crate::checkpoint::recover_pending;

    let (ws, security) = interrupted_workspace();
    let root = ws.path().to_path_buf();
    crate::agent::batch::simulate_death_after(
        &security,
        vec![
            edit(&root, "a.txt", "A\n"),
            edit(&root, "b.txt", "B\n"),
            edit(&root, "c.txt", "C\n"),
        ],
        0,
    )
    .unwrap();

    assert!(
        manifest_of(&root).contains("state\tcommitting"),
        "{}",
        manifest_of(&root)
    );
    assert!(manifest_of(&root).contains("renamed\ta.txt"));

    let batches = recover_pending(&root).unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.entries.len(), 3, "every manifest target is listed");
    let flags: Vec<bool> = batch
        .entries
        .iter()
        .map(|(_, confirmed)| *confirmed)
        .collect();
    assert_eq!(flags, vec![true, false, false]);
    assert!(manifest_of(&root).contains("state\tpartial"));
    assert!(batch.checkpoint.exists(), "the directory is kept");
    assert!(batch.render().contains("run /undo to restore"));
}

#[test]
fn a_rename_without_its_journal_line_is_still_reported() {
    use crate::checkpoint::recover_pending;

    let (ws, security) = interrupted_workspace();
    let root = ws.path().to_path_buf();
    crate::agent::batch::simulate_death_after(
        &security,
        vec![edit(&root, "a.txt", "A\n"), edit(&root, "b.txt", "B\n")],
        0,
    )
    .unwrap();

    // A death between the rename and its journal append: the file changed, the
    // journal does not know.
    let directory = checkpoint_dirs(&root).pop().unwrap();
    let manifest_path = directory.join("manifest");
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let without_journal: String = text
        .lines()
        .filter(|line| !line.starts_with("renamed\t"))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&manifest_path, without_journal).unwrap();

    let batches = recover_pending(&root).unwrap();
    assert_eq!(batches.len(), 1);
    let flags: Vec<bool> = batches[0]
        .entries
        .iter()
        .map(|(_, confirmed)| *confirmed)
        .collect();
    assert_eq!(flags, vec![false, false], "unconfirmed, but still listed");
}

#[test]
fn undo_after_recovery_restores_all_targets_and_is_idempotent() {
    use crate::checkpoint::{CheckpointStore, recover_pending};

    let (ws, security) = interrupted_workspace();
    let root = ws.path().to_path_buf();
    crate::agent::batch::simulate_death_after(
        &security,
        vec![
            edit(&root, "a.txt", "A\n"),
            edit(&root, "b.txt", "B\n"),
            edit(&root, "c.txt", "C\n"),
        ],
        0,
    )
    .unwrap();
    recover_pending(&root).unwrap();

    let restored = CheckpointStore::undo_latest(&root).unwrap();
    assert_eq!(restored.len(), 3);
    for (name, body) in [("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")] {
        assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), body);
    }

    // A second undo has nothing left to do and changes nothing.
    let again = CheckpointStore::undo_latest(&root).unwrap();
    assert!(again.is_empty());
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
}

#[test]
fn recovery_never_touches_a_committed_batch() {
    use crate::checkpoint::recover_pending;

    let (ws, security) = interrupted_workspace();
    let root = ws.path().to_path_buf();
    commit(&security, vec![edit(&root, "a.txt", "A\n")]).unwrap();

    let batches = recover_pending(&root).unwrap();
    assert!(batches.is_empty());
    assert_eq!(checkpoint_dirs(&root).len(), 1);
    assert!(manifest_of(&root).contains("state\tcommitted"));
}

#[test]
fn pruner_keeps_the_newest_two_hundred_directories() {
    use crate::checkpoint::prune;

    let ws = workspace(&[("keep.txt", "x\n")]);
    let root = ws.path().to_path_buf();
    let checkpoints = root.join(".seavx/checkpoints");
    std::fs::create_dir_all(&checkpoints).unwrap();
    for index in 0..205 {
        let directory = checkpoints.join(format!("{index:013}-1-0"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("manifest"),
            "keep\t-\tcreated\t-\t-\t-\tworkspace\n",
        )
        .unwrap();
    }

    prune(&root, 0).unwrap();

    let remaining: Vec<String> = std::fs::read_dir(&checkpoints)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(remaining.len(), 200);
    assert!(
        !remaining
            .iter()
            .any(|name| name.starts_with("0000000000000"))
    );
    assert!(
        remaining
            .iter()
            .any(|name| name.starts_with("0000000000204"))
    );
}

#[test]
fn pruner_never_evicts_a_committing_or_partial_directory() {
    use crate::checkpoint::prune;

    let ws = workspace(&[("keep.txt", "x\n")]);
    let root = ws.path().to_path_buf();
    let checkpoints = root.join(".seavx/checkpoints");
    std::fs::create_dir_all(&checkpoints).unwrap();
    // 200 committed, then three live ones with the oldest names.
    for index in 0..200 {
        let directory = checkpoints.join(format!("{:013}-1-0", index + 100));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("manifest"), "state\tcommitted\n").unwrap();
    }
    for index in 0..3 {
        let directory = checkpoints.join(format!("{index:013}-1-1"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("manifest"), "state\tpartial\n").unwrap();
    }

    prune(&root, 0).unwrap();
    let live: Vec<String> = std::fs::read_dir(&checkpoints)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with("-1-1"))
        .collect();
    assert_eq!(live.len(), 3, "live directories survive the prune");

    // Asking for more room than the prunable set can free is an error, not a
    // silent partial backup.
    let error = prune(&root, 2 * 1024 * 1024 * 1024).unwrap_err();
    assert!(format!("{error}").contains("bytes"), "{error}");
}

#[test]
fn preflight_refuses_a_protected_target() {
    let ws = workspace(&[("notes.md", "n\n")]);
    std::fs::write(ws.path().join(".env"), "SECRET=1\n").unwrap();
    let security = WorkspaceSecurity::new(ws.path());

    let error = commit(&security, vec![edit(ws.path(), ".env", "SECRET=2\n")]).unwrap_err();
    assert!(format!("{error}").contains("Access denied"), "{error}");
    assert_eq!(
        std::fs::read_to_string(ws.path().join(".env")).unwrap(),
        "SECRET=1\n"
    );
    assert!(checkpoint_dirs(ws.path()).is_empty());
}

#[test]
fn the_interrupted_notice_reaches_the_transcript() {
    use crate::checkpoint::InterruptedBatch;

    let batch = InterruptedBatch {
        checkpoint: PathBuf::from("/w/.seavx/checkpoints/1-2-0"),
        entries: vec![
            (PathBuf::from("/w/a.txt"), true),
            (PathBuf::from("/w/b.txt"), false),
        ],
        error: None,
    };
    let rendered = batch.render();
    assert!(
        rendered.contains("batch interrupted: 2 file(s) may differ"),
        "{rendered}"
    );
    assert!(rendered.contains("confirmed: /w/a.txt"), "{rendered}");
    assert!(rendered.contains("unconfirmed: /w/b.txt"), "{rendered}");
    assert!(rendered.ends_with("run /undo to restore"), "{rendered}");

    // The TUI shows the same text through its notice sink.
    let mut app = crate::tui::app::test_app();
    app.push_notice(rendered);
    let text = app
        .segments
        .iter()
        .filter_map(|segment| match segment {
            crate::tui::app::Segment::Text(text) | crate::tui::app::Segment::Notice(text) => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<String>();
    assert!(text.contains("batch interrupted"), "{text}");
    assert!(text.contains("run /undo to restore"), "{text}");
}

#[test]
fn a_rewrite_of_the_last_line_keeps_a_missing_trailing_newline() {
    let patch =
        crate::agent::patch::parse("--- a/f.txt\n+++ b/f.txt\n@@ -2,1 +2,1 @@\n-b\n+B\n").unwrap();

    let (out, _) = crate::agent::patch::apply_to("a\nb", &patch[0]).unwrap();
    assert_eq!(
        out, "a\nB",
        "the file had no trailing newline; it keeps none"
    );
}

#[cfg(unix)]
#[test]
fn a_journal_failure_after_a_rename_rolls_the_batch_back() {
    use std::os::unix::fs::PermissionsExt;

    let ws = workspace(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let security = WorkspaceSecurity::new(ws.path());
    let root = ws.path().to_path_buf();

    // The first rename is journaled, then the manifest stops accepting appends:
    // the second rename cannot be recorded.
    let hook_root = root.clone();
    let hook = move |event: crate::agent::batch::Hook| {
        if let crate::agent::batch::Hook::Renamed(0) = event {
            for directory in checkpoint_dirs(&hook_root) {
                let _ = std::fs::set_permissions(
                    directory.join("manifest"),
                    std::fs::Permissions::from_mode(0o400),
                );
            }
        }
    };
    let result = commit_with_hook(
        &security,
        vec![edit(&root, "a.txt", "A\n"), edit(&root, "b.txt", "B\n")],
        &hook,
    );

    match result {
        Err(BatchError::Commit {
            restored,
            reason,
            partial,
            ..
        }) => {
            assert!(
                reason.contains("journal"),
                "the reason names the journal: {reason}"
            );
            assert!(!partial, "a working rollback is not partial");
            assert!(
                restored.contains(&root.join("a.txt")),
                "the renamed file is put back: {restored:?}"
            );
        }
        other => panic!("a journal failure is a commit failure, got {other:?}"),
    }

    // Both files are as the user had them: a journal failure is not "not applied".
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b\n");
}
