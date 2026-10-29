//! Checkpoint store: one directory of complete backups per tool call.
//!
//! The manifest is untrusted data on restore. Everything is validated before a
//! single target is touched, and a backup is only trusted when its `(dev, ino)`,
//! length and FNV-1a checksum still match what was recorded at capture time.

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::atomic_file::{ModePolicy, write_atomic};

pub(crate) const MANIFEST: &str = "manifest";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryState {
    Created,
    Modified,
    Skipped,
}

impl EntryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Skipped => "skipped",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "created" => Some(Self::Created),
            "modified" => Some(Self::Modified),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CheckpointTarget {
    Workspace(PathBuf),
    Global {
        path: PathBuf,
        allowed_root: PathBuf,
    },
}

#[derive(Debug)]
pub(crate) struct CheckpointInput {
    pub target: CheckpointTarget,
    pub state: EntryState,
    /// `None` means a newly created target, which undo removes.
    pub original: Option<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) struct RestoreEntry {
    pub target: PathBuf,
    pub state: EntryState,
    pub backup: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct CheckpointError {
    prefix: &'static str,
    detail: String,
}

impl CheckpointError {
    fn incomplete(error: &Self) -> Self {
        Self {
            prefix: "checkpoint incomplete",
            detail: error.detail.clone(),
        }
    }
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.prefix, self.detail)
    }
}

impl std::error::Error for CheckpointError {}

fn reject(message: impl std::fmt::Display) -> CheckpointError {
    CheckpointError {
        prefix: "checkpoint rejected",
        detail: message.to_string(),
    }
}

pub(crate) struct CheckpointStore;

impl CheckpointStore {
    /// Capture complete backups under `.seavx/checkpoints/<epoch_ms>-<pid>-<n>/`.
    pub(crate) fn create(
        workspace: &Path,
        entries: &[CheckpointInput],
    ) -> Result<PathBuf, CheckpointError> {
        let directory = allocate_directory(&workspace.join(".seavx/checkpoints"))?;
        match write_checkpoint(&directory, workspace, entries) {
            Ok(()) => Ok(directory),
            Err(error) => {
                // A half-written checkpoint must never be offered to undo.
                let _ = std::fs::remove_dir_all(&directory);
                Err(error)
            }
        }
    }

    pub(crate) fn validate(
        checkpoint: &Path,
        workspace: &Path,
    ) -> Result<Vec<RestoreEntry>, CheckpointError> {
        Self::validate_with_home(checkpoint, workspace, crate::home::dir().as_deref())
    }

    /// Restore the newest checkpoint and remove it. Nothing is restored unless
    /// the whole manifest validates, and the directory is only removed after
    /// every entry was written back.
    pub(crate) fn undo_latest(workspace: &Path) -> Result<Vec<PathBuf>, CheckpointError> {
        let Some(checkpoint) = latest_checkpoint(workspace) else {
            return Ok(Vec::new());
        };
        let entries = Self::validate(&checkpoint, workspace)
            .map_err(|error| CheckpointError::incomplete(&error))?;
        restore(&checkpoint, entries)
    }

    /// Test seam: `undo_latest` with the home directory injected.
    #[cfg(test)]
    pub(crate) fn undo_latest_with_home(
        workspace: &Path,
        home: Option<&Path>,
    ) -> Result<Vec<PathBuf>, CheckpointError> {
        let Some(checkpoint) = latest_checkpoint(workspace) else {
            return Ok(Vec::new());
        };
        let entries = Self::validate_with_home(&checkpoint, workspace, home)
            .map_err(|error| CheckpointError::incomplete(&error))?;
        restore(&checkpoint, entries)
    }

    /// Validate the whole manifest against `home`'s allowlist; no target is touched.
    pub(crate) fn validate_with_home(
        checkpoint: &Path,
        workspace: &Path,
        home: Option<&Path>,
    ) -> Result<Vec<RestoreEntry>, CheckpointError> {
        let manifest = std::fs::read_to_string(checkpoint.join(MANIFEST))
            .map_err(|error| reject(format!("unreadable manifest: {error}")))?;
        let allowlist = global_allowlist(home);

        let mut entries = Vec::new();
        for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 7 {
                return Err(reject(format!("malformed manifest line: {line}")));
            }
            let state = EntryState::parse(fields[2])
                .ok_or_else(|| reject(format!("unknown entry state: {}", fields[2])))?;
            let target = resolve_target(fields[0], fields[6], workspace, &allowlist)?;
            let backup = validate_backup(checkpoint, &fields, state)?;
            entries.push(RestoreEntry {
                target,
                state,
                backup,
            });
        }

        Ok(entries)
    }
}

fn restore(checkpoint: &Path, entries: Vec<RestoreEntry>) -> Result<Vec<PathBuf>, CheckpointError> {
    let mut restored = Vec::new();
    for entry in entries {
        match entry.state {
            EntryState::Created => {
                if entry.target.exists() {
                    std::fs::remove_file(&entry.target)
                        .map_err(|error| reject(format!("{}: {error}", entry.target.display())))?;
                }
            }
            EntryState::Modified => {
                let backup = entry.backup.as_ref().ok_or_else(|| {
                    reject(format!(
                        "{}: modified entry without a backup",
                        entry.target.display()
                    ))
                })?;
                let bytes = std::fs::read(backup)
                    .map_err(|error| reject(format!("{}: {error}", backup.display())))?;
                write_atomic(&entry.target, &bytes, ModePolicy::Preserve)
                    .map_err(|error| reject(format!("{}: {error}", entry.target.display())))?;
            }
            EntryState::Skipped => continue,
        }
        restored.push(entry.target);
    }

    // Only a complete restore removes the checkpoint, so a failure stays retryable.
    std::fs::remove_dir_all(checkpoint)
        .map_err(|error| reject(format!("{}: {error}", checkpoint.display())))?;
    Ok(restored)
}

fn latest_checkpoint(workspace: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(workspace.join(".seavx/checkpoints")).ok()?;
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    // Names start with epoch milliseconds, so lexical order is capture order.
    directories.sort();
    directories.pop()
}

fn allocate_directory(root: &Path) -> Result<PathBuf, CheckpointError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(root).map_err(|error| reject(error.to_string()))?;
    make_owner_only(root)?;

    for _ in 0..100 {
        let name = format!(
            "{}-{}-{}",
            crate::util::epoch_millis(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = root.join(name);
        match std::fs::create_dir(&directory) {
            Ok(()) => {
                make_owner_only(&directory)?;
                return Ok(directory);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(reject(error.to_string())),
        }
    }

    Err(reject("could not allocate a checkpoint directory"))
}

fn write_checkpoint(
    directory: &Path,
    workspace: &Path,
    entries: &[CheckpointInput],
) -> Result<(), CheckpointError> {
    let mut lines = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        let (path_field, scope) = describe_target(workspace, &entry.target)?;
        let backup = match (entry.state, entry.original.as_deref()) {
            (EntryState::Modified, Some(_)) => Some(index),
            (EntryState::Modified, None) => {
                return Err(reject(format!(
                    "{path_field}: a modified target needs its original bytes"
                )));
            }
            _ => None,
        };

        let line = match backup {
            Some(index) => {
                let name = format!("backup-{index}");
                let path = directory.join(&name);
                write_atomic(
                    &path,
                    entry.original.as_deref().unwrap_or_default(),
                    ModePolicy::Force(0o600),
                )
                .map_err(|error| reject(format!("{path_field}: {error}")))?;
                // Record what is actually on disk, not what was meant to be written.
                let stored = std::fs::read(&path)
                    .map_err(|error| reject(format!("{path_field}: {error}")))?;
                let (dev, ino) =
                    identity_of(&path).map_err(|error| reject(format!("{path_field}: {error}")))?;
                format!(
                    "{path_field}\t{name}\t{}\t{dev}:{ino}\t{}\t{}\t{scope}",
                    entry.state.as_str(),
                    stored.len(),
                    fnv1a64(&stored)
                )
            }
            None => format!(
                "{path_field}\t-\t{}\t-\t-\t-\t{scope}",
                entry.state.as_str()
            ),
        };
        lines.push(line);
    }

    let mut manifest = lines.join("\n");
    manifest.push('\n');
    write_atomic(
        &directory.join(MANIFEST),
        manifest.as_bytes(),
        ModePolicy::Force(0o600),
    )
    .map_err(|error| reject(format!("manifest: {error}")))
}

fn describe_target(
    workspace: &Path,
    target: &CheckpointTarget,
) -> Result<(String, String), CheckpointError> {
    match target {
        CheckpointTarget::Workspace(path) => {
            let relative = path
                .strip_prefix(workspace)
                .map_err(|_| reject(format!("{} is outside the workspace", path.display())))?;
            Ok((
                relative.to_string_lossy().to_string(),
                "workspace".to_string(),
            ))
        }
        CheckpointTarget::Global { path, allowed_root } => {
            if !path.is_absolute() || !allowed_root.is_absolute() || !path.starts_with(allowed_root)
            {
                return Err(reject(format!(
                    "{} is not inside {}",
                    path.display(),
                    allowed_root.display()
                )));
            }
            Ok((
                path.to_string_lossy().to_string(),
                allowed_root.to_string_lossy().to_string(),
            ))
        }
    }
}

fn global_allowlist(home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    vec![
        home.join(".seavx/MEMORY.md"),
        home.join(".seavx/USER.md"),
        home.join(".seavx/skills"),
        home.join(".agents/skills"),
    ]
}

fn resolve_target(
    path: &str,
    scope: &str,
    workspace: &Path,
    allowlist: &[PathBuf],
) -> Result<PathBuf, CheckpointError> {
    let target = Path::new(path);

    if scope == "workspace" {
        let plain = target
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        if target.is_absolute() || !plain {
            return Err(reject(format!("unsafe workspace path: {path}")));
        }
        return Ok(workspace.join(target));
    }

    let root = Path::new(scope);
    let plain = target
        .components()
        .all(|component| !matches!(component, Component::ParentDir));
    let allowed = target.is_absolute()
        && root.is_absolute()
        && plain
        && allowlist
            .iter()
            .any(|allowed_root| allowed_root == root && target.starts_with(root));
    if !allowed {
        return Err(reject(format!(
            "{path} is outside the allowed global roots"
        )));
    }

    Ok(target.to_path_buf())
}

fn validate_backup(
    checkpoint: &Path,
    fields: &[&str],
    state: EntryState,
) -> Result<Option<PathBuf>, CheckpointError> {
    if state != EntryState::Modified {
        if fields[1] != "-" {
            return Err(reject(format!(
                "unexpected backup {} for a {} entry",
                fields[1],
                state.as_str()
            )));
        }
        return Ok(None);
    }

    let name = fields[1];
    let name_path = Path::new(name);
    let plain = name_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if name == "-" || name_path.components().count() != 1 || !plain {
        return Err(reject(format!("unsafe backup name: {name}")));
    }

    let backup = checkpoint.join(name_path);
    let metadata = std::fs::symlink_metadata(&backup)
        .map_err(|error| reject(format!("unreadable backup {name}: {error}")))?;
    if !metadata.is_file() {
        return Err(reject(format!("backup {name} is not a regular file")));
    }

    let recorded_identity = parse_identity(fields[3])
        .ok_or_else(|| reject(format!("malformed identity: {}", fields[3])))?;
    let identity = identity_of(&backup).map_err(|error| reject(format!("{name}: {error}")))?;
    if identity != recorded_identity {
        return Err(reject(format!("backup {name} identity changed")));
    }

    let bytes = std::fs::read(&backup).map_err(|error| reject(format!("{name}: {error}")))?;
    let recorded_len: u64 = fields[4]
        .parse()
        .map_err(|_| reject(format!("malformed length: {}", fields[4])))?;
    if bytes.len() as u64 != recorded_len {
        return Err(reject(format!("backup {name} length changed")));
    }

    let recorded_sum: u64 = fields[5]
        .parse()
        .map_err(|_| reject(format!("malformed checksum: {}", fields[5])))?;
    if fnv1a64(&bytes) != recorded_sum {
        return Err(reject(format!("backup {name} checksum changed")));
    }

    Ok(Some(backup))
}

fn parse_identity(raw: &str) -> Option<(u64, u64)> {
    let (dev, ino) = raw.split_once(':')?;
    Some((dev.parse().ok()?, ino.parse().ok()?))
}

#[cfg(unix)]
fn identity_of(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).map(|metadata| (metadata.dev(), metadata.ino()))
}

// shortcut: Windows has no inode, so identity collapses to length and checksum
// there. Upgrade when a Windows target needs inode identity.
#[cfg(not(unix))]
fn identity_of(_path: &Path) -> std::io::Result<(u64, u64)> {
    Ok((0, 0))
}

#[cfg(unix)]
fn make_owner_only(path: &Path) -> Result<(), CheckpointError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| reject(format!("{}: {error}", path.display())))
}

#[cfg(not(unix))]
fn make_owner_only(_path: &Path) -> Result<(), CheckpointError> {
    Ok(())
}

pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
#[path = "../tests/core/checkpoint.rs"]
mod tests;
