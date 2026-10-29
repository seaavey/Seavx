//! Journaled multi-file commits.
//!
//! Preflight, then stage, then commit: every buffer is validated and every temp
//! written before a single rename, the checkpoint records `staged`, the manifest
//! says `committing` before the first rename, each confirmed rename is appended
//! to the journal, and the manifest ends at `committed`. A failure after some
//! renames restores those files from the same bytes the checkpoint holds; a
//! restore that also fails leaves the directory and says so.

use std::path::{Path, PathBuf};

use crate::agent::security::{self, PathSnapshot, WorkspaceSecurity};
use crate::atomic_file::{MAX_ATOMIC_BYTES, ModePolicy, StagedFile, stage};
use crate::checkpoint::{
    BatchState, CheckpointInput, CheckpointTarget, EntryState, append_renamed, create_batch,
    set_state,
};

pub(crate) const MAX_BATCH_FILES: usize = 64;
pub(crate) const MAX_BATCH_STAGED_BYTES: u64 = 128 * 1024 * 1024;

pub(crate) struct FileEdit {
    pub path: PathBuf,
    pub content: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct BatchReport {
    pub checkpoint: PathBuf,
    pub written: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum BatchError {
    Preflight(String),
    Conflict {
        paths: Vec<PathBuf>,
    },
    Stage(String),
    Commit {
        failed: PathBuf,
        restored: Vec<PathBuf>,
        checkpoint: PathBuf,
        partial: bool,
        skipped: Vec<PathBuf>,
    },
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchError::Preflight(detail) => write!(f, "batch refused: {detail}"),
            BatchError::Conflict { paths } => write!(
                f,
                "batch conflict: {} changed after staging",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            BatchError::Stage(detail) => write!(f, "batch staging failed: {detail}"),
            BatchError::Commit {
                failed,
                restored,
                checkpoint,
                partial,
                skipped,
            } => {
                write!(
                    f,
                    "batch commit failed at {}: {} file(s) restored",
                    failed.display(),
                    restored.len()
                )?;
                for path in restored {
                    write!(f, "\nrestored: {}", path.display())?;
                }
                for path in skipped {
                    write!(
                        f,
                        "\nrollback skipped: {} changed after commit",
                        path.display()
                    )?;
                }
                write!(f, "\ncheckpoint: {}", checkpoint.display())?;
                if *partial {
                    write!(f, "\ncheckpoint partial: restore by hand")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BatchError {}

/// Where a hook fires, so a test can fail at every stage of the protocol.
#[derive(Debug)]
pub(crate) enum Hook {
    TempWritten(usize),
    Synced(usize),
    CheckpointWritten,
    Renamed(usize),
}

struct Target {
    path: PathBuf,
    /// `None` for a target that does not exist yet.
    snapshot: Option<PathSnapshot>,
    original: Option<Vec<u8>>,
}

pub(crate) fn commit(
    security: &WorkspaceSecurity,
    edits: Vec<FileEdit>,
) -> Result<BatchReport, BatchError> {
    run(security, edits, None, None)
}

#[cfg(test)]
pub(crate) fn commit_with_hook(
    security: &WorkspaceSecurity,
    edits: Vec<FileEdit>,
    hook: &dyn Fn(Hook),
) -> Result<BatchReport, BatchError> {
    run(security, edits, Some(hook), None)
}

/// Return after `renames` successful renames without rolling anything back: what
/// a real death looks like, so the recovery path is tested against a journal.
#[cfg(test)]
pub(crate) fn simulate_death_after(
    security: &WorkspaceSecurity,
    edits: Vec<FileEdit>,
    renames: usize,
) -> Result<BatchReport, BatchError> {
    run(security, edits, None, Some(renames))
}

fn run(
    security: &WorkspaceSecurity,
    edits: Vec<FileEdit>,
    hook: Option<&dyn Fn(Hook)>,
    die_after: Option<usize>,
) -> Result<BatchReport, BatchError> {
    let fire = |event: Hook| {
        // Every stage is traceable, and the payload is what makes it useful.
        match &event {
            Hook::TempWritten(index) => tracing::trace!(index, "batch temp written"),
            Hook::Synced(index) => tracing::trace!(index, "batch temp synced"),
            Hook::CheckpointWritten => tracing::trace!("batch checkpoint written"),
            Hook::Renamed(index) => tracing::trace!(index, "batch rename confirmed"),
        }
        if let Some(hook) = hook {
            hook(event);
        }
    };

    // 1. Preflight: capture identity and bytes before anything is created.
    if edits.is_empty() {
        return Err(BatchError::Preflight("no edits in the batch".to_string()));
    }
    if edits.len() > MAX_BATCH_FILES {
        return Err(BatchError::Preflight(format!(
            "batch exceeds {MAX_BATCH_FILES} files"
        )));
    }

    let mut targets = Vec::with_capacity(edits.len());
    for edit in &edits {
        if !edit.path.starts_with(security.root()) {
            return Err(BatchError::Preflight(format!(
                "refusing to write outside the workspace: {}",
                edit.path.display()
            )));
        }
        // Defence in depth: the tool validates too, but the commit itself must
        // never stage a protected file.
        security::check_file(&edit.path).map_err(BatchError::Preflight)?;
        if edit.content.len() as u64 > MAX_ATOMIC_BYTES {
            return Err(BatchError::Preflight(
                "refusing to atomically rewrite a file over 16 MB".to_string(),
            ));
        }

        let snapshot = match std::fs::symlink_metadata(&edit.path) {
            Ok(meta) if meta.is_file() => {
                let bytes = std::fs::read(&edit.path).map_err(|error| {
                    BatchError::Preflight(format!("{}: {error}", edit.path.display()))
                })?;
                if bytes.len() as u64 > MAX_ATOMIC_BYTES {
                    return Err(BatchError::Preflight(
                        "refusing to atomically rewrite a file over 16 MB".to_string(),
                    ));
                }
                Some(security::snapshot(&edit.path).map_err(|error| {
                    BatchError::Preflight(format!("{}: {error}", edit.path.display()))
                })?)
                .map(|snapshot| (snapshot, bytes))
            }
            Ok(_) => {
                return Err(BatchError::Preflight(format!(
                    "{}: not a regular file",
                    edit.path.display()
                )));
            }
            Err(_) => None,
        };
        let (snapshot, original) = match snapshot {
            Some((snapshot, bytes)) => (Some(snapshot), Some(bytes)),
            None => (None, None),
        };
        targets.push(Target {
            path: edit.path.clone(),
            snapshot,
            original,
        });
    }

    let backup_bytes: u64 = targets
        .iter()
        .filter_map(|target| target.original.as_ref().map(|bytes| bytes.len() as u64))
        .sum();
    let new_bytes: u64 = edits.iter().map(|edit| edit.content.len() as u64).sum();
    if backup_bytes + new_bytes > MAX_BATCH_STAGED_BYTES {
        return Err(BatchError::Preflight(format!(
            "batch stages {backup_bytes} backup bytes plus {new_bytes} new bytes, over the {MAX_BATCH_STAGED_BYTES} byte cap"
        )));
    }

    // 2. Stage: every temp is written and synced before the first rename.
    let mut staged: Vec<Option<StagedFile>> = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let file = stage(&edit.path, &edit.content, ModePolicy::Preserve)
            .map_err(|error| BatchError::Stage(format!("{}: {error}", edit.path.display())))?;
        fire(Hook::TempWritten(index));
        fire(Hook::Synced(index));
        staged.push(Some(file));
    }
    let discard_staged = |staged: &mut Vec<Option<StagedFile>>| {
        for file in staged.drain(..).flatten() {
            file.discard();
        }
    };
    let stage_failure = |staged: &mut Vec<Option<StagedFile>>, error: BatchError| {
        discard_staged(staged);
        error
    };

    // 3. Checkpoint, then `committing` before the first rename.
    let inputs: Vec<CheckpointInput> = targets
        .iter()
        .map(|target| CheckpointInput {
            target: CheckpointTarget::Workspace(target.path.clone()),
            state: if target.snapshot.is_some() {
                EntryState::Modified
            } else {
                EntryState::Created
            },
            original: target.original.clone(),
        })
        .collect();
    let checkpoint = match create_batch(security.root(), &inputs) {
        Ok(directory) => directory,
        Err(error) => {
            return Err(stage_failure(
                &mut staged,
                BatchError::Stage(format!("checkpoint: {error}")),
            ));
        }
    };
    fire(Hook::CheckpointWritten);
    if let Err(error) = set_state(&checkpoint, BatchState::Committing) {
        return Err(stage_failure(
            &mut staged,
            BatchError::Stage(format!("checkpoint: {error}")),
        ));
    }

    // 4. Commit: re-check identity and bytes, rename, journal, repeat.
    let mut written: Vec<PathBuf> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        if changed_since_capture(edit, &targets[index]) {
            // Nothing of this edit was renamed; earlier ones are put back, so a
            // conflict leaves every file as the user had it.
            let _ = rollback(&targets, &edits, &written);
            let _ = set_state(&checkpoint, BatchState::RolledBack);
            return Err(BatchError::Conflict {
                paths: vec![edit.path.clone()],
            });
        }

        let file = staged[index].take().expect("one staged file per edit");
        if let Err(error) = file.commit(&edit.path) {
            tracing::warn!(%error, path = %edit.path.display(), "batch rename failed");
            let (restored, skipped, partial) = rollback(&targets, &edits, &written);
            // `partial` keeps the directory so the user can restore by hand.
            let state = if partial {
                BatchState::Partial
            } else {
                BatchState::RolledBack
            };
            let _ = set_state(&checkpoint, state);
            return Err(BatchError::Commit {
                failed: edit.path.clone(),
                restored,
                checkpoint,
                partial,
                skipped,
            });
        }

        let relative = relative_path(security.root(), &edit.path);
        append_renamed(&checkpoint, &relative)
            .map_err(|error| BatchError::Stage(format!("journal: {error}")))?;
        written.push(edit.path.clone());
        fire(Hook::Renamed(index));

        if die_after == Some(index) {
            return Ok(BatchReport {
                checkpoint,
                written,
            });
        }
    }

    set_state(&checkpoint, BatchState::Committed)
        .map_err(|error| BatchError::Stage(format!("checkpoint: {error}")))?;
    Ok(BatchReport {
        checkpoint,
        written,
    })
}

fn changed_since_capture(edit: &FileEdit, target: &Target) -> bool {
    match &target.snapshot {
        Some(snapshot) => {
            let now = security::snapshot(&edit.path).ok();
            let bytes = std::fs::read(&edit.path).ok();
            now.as_ref() != Some(snapshot) || bytes != target.original
        }
        None => edit.path.exists(),
    }
}

/// Restore the targets this commit already renamed, from the same bytes the
/// checkpoint holds. A file whose current bytes are not the ones this commit
/// wrote was edited afterwards: skip it, never overwrite newer work. The third
/// value is `partial`: a restore itself failed, so the checkpoint must stay.
fn rollback(
    targets: &[Target],
    edits: &[FileEdit],
    written: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>, bool) {
    let mut restored = Vec::new();
    let mut skipped = Vec::new();
    let mut partial = false;

    for path in written {
        let Some(index) = targets.iter().position(|target| &target.path == path) else {
            continue;
        };
        let written_bytes = edits.get(index).map(|edit| edit.content.as_slice());
        if std::fs::read(path).ok().as_deref() != written_bytes {
            skipped.push(path.clone());
            continue;
        }

        let outcome = match &targets[index].original {
            Some(bytes) => crate::atomic_file::write_atomic(path, bytes, ModePolicy::Preserve)
                .map_err(|error| error.to_string()),
            None => std::fs::remove_file(path).map_err(|error| error.to_string()),
        };
        match outcome {
            Ok(()) => restored.push(path.clone()),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "batch rollback failed");
                skipped.push(path.clone());
                partial = true;
            }
        }
    }

    (restored, skipped, partial)
}

fn relative_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
#[path = "../tests/core/batch.rs"]
mod tests;
