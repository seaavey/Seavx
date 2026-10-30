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
    /// The path exactly as the manifest records it, so a `renamed` journal line
    /// can be matched to its entry without re-resolving it.
    pub relpath: String,
    pub state: EntryState,
    pub backup: Option<PathBuf>,
}

/// Lifecycle of one batch: written before the first rename, updated after each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchState {
    Staged,
    Committing,
    Committed,
    RolledBack,
    Partial,
}

impl BatchState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BatchState::Staged => "staged",
            BatchState::Committing => "committing",
            BatchState::Committed => "committed",
            BatchState::RolledBack => "rolled_back",
            BatchState::Partial => "partial",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "staged" => Some(BatchState::Staged),
            "committing" => Some(BatchState::Committing),
            "committed" => Some(BatchState::Committed),
            "rolled_back" => Some(BatchState::RolledBack),
            "partial" => Some(BatchState::Partial),
            _ => None,
        }
    }

    /// A live batch may still differ from its backups, so it is never pruned.
    pub(crate) fn is_live(self) -> bool {
        matches!(self, BatchState::Committing | BatchState::Partial)
    }
}

/// One parsed manifest: its state, its entries and the rename journal.
pub(crate) struct Manifest {
    pub(crate) state: BatchState,
    pub(crate) entries: Vec<RestoreEntry>,
    pub(crate) renamed: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct CheckpointError {
    prefix: &'static str,
    detail: String,
    /// Targets already written back when a restore failed halfway.
    restored: Vec<PathBuf>,
}

impl CheckpointError {
    fn incomplete(error: &Self) -> Self {
        Self {
            prefix: "checkpoint incomplete",
            detail: error.detail.clone(),
            restored: error.restored.clone(),
        }
    }

    fn with_restored(mut self, restored: &[PathBuf]) -> Self {
        self.restored = restored.to_vec();
        self
    }

    /// The paths this failed restore already changed, so the user is told what moved.
    pub(crate) fn restored(&self) -> &[PathBuf] {
        &self.restored
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
        restored: Vec::new(),
    }
}

pub(crate) struct CheckpointStore;

impl CheckpointStore {
    /// Capture complete backups under `.seavx/checkpoints/<epoch_ms>-<pid>-<n>/`.
    pub(crate) fn create(
        workspace: &Path,
        entries: &[CheckpointInput],
    ) -> Result<PathBuf, CheckpointError> {
        let incoming: u64 = entries
            .iter()
            .filter_map(|entry| entry.original.as_ref().map(|bytes| bytes.len() as u64))
            .sum();
        prune(workspace, incoming)?;
        let directory = allocate_directory(&workspace.join(".seavx/checkpoints"))?;
        match write_checkpoint(&directory, workspace, entries, None) {
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
        Ok(read_manifest(checkpoint, workspace, home)?.entries)
    }
}

/// A batch the last process never finished: every target may differ, and the
/// journal says which renames it happened to confirm.
pub(crate) struct InterruptedBatch {
    pub checkpoint: PathBuf,
    pub entries: Vec<(PathBuf, bool)>,
}

impl InterruptedBatch {
    /// The notice a human reads: every manifest target, confirmed or not.
    pub(crate) fn render(&self) -> String {
        let mut text = format!(
            "batch interrupted: {} file(s) may differ; checkpoint {}",
            self.entries.len(),
            self.checkpoint.display()
        );
        for (path, confirmed) in &self.entries {
            let mark = if *confirmed {
                "confirmed"
            } else {
                "unconfirmed"
            };
            text.push_str(&format!("\n  {mark}: {}", path.display()));
        }
        text.push_str("\nrun /undo to restore");
        text
    }
}

/// Rewrite every `committing` batch to `partial` and report it. Nothing is
/// written to a user file here: recovery is loud and undoable, never automatic.
pub(crate) fn recover_pending(workspace: &Path) -> Result<Vec<InterruptedBatch>, CheckpointError> {
    let home = crate::home::dir();
    let root = workspace.join(".seavx/checkpoints");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };

    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();

    let mut interrupted = Vec::new();
    for directory in directories {
        let Ok(manifest) = read_manifest(&directory, workspace, home.as_deref()) else {
            continue;
        };
        if manifest.state != BatchState::Committing {
            continue;
        }
        let _ = set_state(&directory, BatchState::Partial);
        let entries = manifest
            .entries
            .iter()
            .map(|entry| {
                let confirmed = manifest.renamed.iter().any(|path| path == &entry.relpath);
                (entry.target.clone(), confirmed)
            })
            .collect();
        interrupted.push(InterruptedBatch {
            checkpoint: directory,
            entries,
        });
    }
    Ok(interrupted)
}

/// Keep the newest 200 checkpoint directories and the store under 1 GB. A
/// `committing` or `partial` directory is never evicted: it is the only copy of
/// bytes a crash may still need.
pub(crate) fn prune(workspace: &Path, incoming_bytes: u64) -> Result<(), CheckpointError> {
    const MAX_DIRECTORIES: usize = 200;
    const MAX_BYTES: u64 = 1024 * 1024 * 1024;

    let root = workspace.join(".seavx/checkpoints");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(());
    };
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    // Names start with epoch milliseconds, so lexical order is age order.
    directories.sort();

    let mut kept: Vec<PathBuf> = Vec::new();
    let mut evictable: Vec<PathBuf> = Vec::new();
    let mut total: u64 = 0;
    for directory in directories {
        total += directory_size(&directory);
        if state_of(&directory).is_some_and(BatchState::is_live) {
            kept.push(directory);
        } else {
            evictable.push(directory);
        }
    }

    let mut remaining = kept.len() + evictable.len();
    for directory in evictable.clone() {
        if remaining <= MAX_DIRECTORIES && total + incoming_bytes <= MAX_BYTES {
            break;
        }
        let size = directory_size(&directory);
        std::fs::remove_dir_all(&directory)
            .map_err(|error| reject(format!("{}: {error}", directory.display())))?;
        total = total.saturating_sub(size);
        remaining -= 1;
    }

    if kept.len() > MAX_DIRECTORIES || total + incoming_bytes > MAX_BYTES {
        return Err(reject(format!(
            "checkpoint storage needs {incoming_bytes} bytes but only {} of {MAX_BYTES} are free after keeping {} live checkpoint(s)",
            MAX_BYTES.saturating_sub(total),
            kept.len()
        )));
    }
    Ok(())
}

fn state_of(directory: &Path) -> Option<BatchState> {
    let text = std::fs::read_to_string(directory.join(MANIFEST)).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("state\t"))
        .and_then(BatchState::parse)
        .or(Some(BatchState::Committed))
}

fn directory_size(directory: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| if meta.is_dir() { 0 } else { meta.len() })
        .sum()
}

/// Allocate a checkpoint directory for a whole batch, state `staged`.
pub(crate) fn create_batch(
    workspace: &Path,
    entries: &[CheckpointInput],
) -> Result<PathBuf, CheckpointError> {
    let incoming: u64 = entries
        .iter()
        .filter_map(|entry| entry.original.as_ref().map(|bytes| bytes.len() as u64))
        .sum();
    prune(workspace, incoming)?;
    let directory = allocate_directory(&workspace.join(".seavx/checkpoints"))?;
    match write_checkpoint(&directory, workspace, entries, Some(BatchState::Staged)) {
        Ok(()) => Ok(directory),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&directory);
            Err(error)
        }
    }
}

/// Rewrite the manifest's state line, keeping every entry and journal line.
pub(crate) fn set_state(directory: &Path, state: BatchState) -> Result<(), CheckpointError> {
    let path = directory.join(MANIFEST);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| reject(format!("unreadable manifest: {error}")))?;
    let body: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with("state\t"))
        .collect();
    let mut next = format!("state\t{}\n", state.as_str());
    for line in body {
        next.push_str(line);
        next.push('\n');
    }
    write_atomic(&path, next.as_bytes(), ModePolicy::Force(0o600))
        .map_err(|error| reject(format!("manifest: {error}")))
}

/// Append one confirmed rename. Append-only and synced: the journal is a lower
/// bound on what changed, never an upper one.
pub(crate) fn append_renamed(directory: &Path, relative: &Path) -> Result<(), CheckpointError> {
    use std::io::Write;
    let path = directory.join(MANIFEST);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|error| reject(format!("journal: {error}")))?;
    writeln!(file, "renamed\t{}", relative.display())
        .map_err(|error| reject(format!("journal: {error}")))?;
    file.sync_all()
        .map_err(|error| reject(format!("journal: {error}")))
}

/// Parse a manifest, including its state line and journal.
pub(crate) fn read_manifest(
    checkpoint: &Path,
    workspace: &Path,
    home: Option<&Path>,
) -> Result<Manifest, CheckpointError> {
    let text = std::fs::read_to_string(checkpoint.join(MANIFEST))
        .map_err(|error| reject(format!("unreadable manifest: {error}")))?;
    let allowlist = global_allowlist(home);

    let mut state = BatchState::Committed;
    let mut entries: Vec<RestoreEntry> = Vec::new();
    let mut renamed: Vec<String> = Vec::new();

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["state", value] => {
                state = BatchState::parse(value)
                    .ok_or_else(|| reject(format!("unknown batch state: {value}")))?;
            }
            ["renamed", path] => renamed.push((*path).to_string()),
            _ if fields.len() == 7 => {
                let entry_state = EntryState::parse(fields[2])
                    .ok_or_else(|| reject(format!("unknown entry state: {}", fields[2])))?;
                let target = resolve_target(fields[0], fields[6], workspace, &allowlist)?;
                let backup = validate_backup(checkpoint, &fields, entry_state)?;
                entries.push(RestoreEntry {
                    target,
                    relpath: fields[0].to_string(),
                    state: entry_state,
                    backup,
                });
            }
            _ => return Err(reject(format!("malformed manifest line: {line}"))),
        }
    }

    for path in &renamed {
        if !entries.iter().any(|entry| &entry.relpath == path) {
            return Err(reject(format!(
                "journal names a target the manifest does not record: {path}"
            )));
        }
    }

    Ok(Manifest {
        state,
        entries,
        renamed,
    })
}

fn restore(checkpoint: &Path, entries: Vec<RestoreEntry>) -> Result<Vec<PathBuf>, CheckpointError> {
    let mut restored = Vec::new();
    for entry in entries {
        if entry.state == EntryState::Skipped {
            continue;
        }
        if let Err(error) = restore_entry(&entry) {
            // The caller has to see which targets already moved; the checkpoint
            // stays on disk so the command can be retried.
            return Err(error.with_restored(&restored));
        }
        restored.push(entry.target);
    }

    // Only a complete restore removes the checkpoint.
    std::fs::remove_dir_all(checkpoint)
        .map_err(|error| reject(format!("{}: {error}", checkpoint.display())))?;
    Ok(restored)
}

fn restore_entry(entry: &RestoreEntry) -> Result<(), CheckpointError> {
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
        EntryState::Skipped => {}
    }
    Ok(())
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
    state: Option<BatchState>,
) -> Result<(), CheckpointError> {
    let mut lines = Vec::new();
    if let Some(state) = state {
        lines.push(format!("state\t{}", state.as_str()));
    }

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
        let target = workspace.join(target);
        ensure_real_ancestors(workspace, &target)?;
        return Ok(target);
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

    let target = target.to_path_buf();
    ensure_real_ancestors(root, &target)?;
    Ok(target)
}

/// A clean relative path is not enough: a rename through a symlinked directory
/// lands outside the workspace or the allowed global root. Every existing
/// ancestor below the root must be a real directory; a missing one is fine,
/// because the writer creates it.
fn ensure_real_ancestors(root: &Path, target: &Path) -> Result<(), CheckpointError> {
    let mut current = target.parent();
    while let Some(ancestor) = current {
        if ancestor == root || !ancestor.starts_with(root) || ancestor.as_os_str().is_empty() {
            break;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(reject(format!(
                    "{} has a symlinked ancestor {}",
                    target.display(),
                    ancestor.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(reject(format!(
                    "{} has a non-directory ancestor {}",
                    target.display(),
                    ancestor.display()
                )));
            }
            _ => {}
        }
        current = ancestor.parent();
    }
    Ok(())
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
#[path = "tests/core/checkpoint.rs"]
mod tests;
