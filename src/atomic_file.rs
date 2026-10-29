//! One atomic byte writer shared by every mutating surface.
//!
//! The target is replaced by a unique same-directory temp that is created
//! exclusively at `0o600`, written, synced and renamed. Cleanup removes only
//! the temp this operation created, verified by `(dev, ino)`.

use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Ceiling for the existing target, the new content and a checkpoint backup.
pub(crate) const MAX_ATOMIC_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModePolicy {
    /// Keep the target's permissions; a new file follows `umask`.
    Preserve,
    /// Apply exactly this mode.
    Force(u32),
}

pub(crate) fn write_atomic(path: &Path, content: &[u8], policy: ModePolicy) -> std::io::Result<()> {
    write_atomic_with_temp(path, content, policy, &unique_temp_path(path))
}

fn write_atomic_with_temp(
    path: &Path,
    content: &[u8],
    policy: ModePolicy,
    temp_path: &Path,
) -> std::io::Result<()> {
    let existing = std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file);
    let existing_len = existing.as_ref().map(std::fs::Metadata::len).unwrap_or(0);
    if existing_len > MAX_ATOMIC_BYTES || content.len() as u64 > MAX_ATOMIC_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "refusing to atomically rewrite a file over 16 MB",
        ));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = create_temp_file(temp_path)?;
    let created = identity_of_handle(&file);
    let staged = file
        .write_all(content)
        .and_then(|()| file.sync_all())
        .and_then(|()| apply_mode(&file, existing.as_ref(), policy));
    drop(file);

    if let Err(error) = staged {
        remove_created(temp_path, created);
        return Err(error);
    }

    // shortcut: no directory fsync, so a power cut after the rename may lose
    // this write. Upgrade when a write must survive power loss.
    std::fs::rename(temp_path, path).inspect_err(|_| remove_created(temp_path, created))
}

/// Staging file: exclusive, same directory, `0o600` until the content is complete.
fn create_temp_file(temp_path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(temp_path)
}

pub(crate) fn unique_temp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(
        ".{name}.seavx-tmp-{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(unix)]
fn apply_mode(
    file: &File,
    existing: Option<&std::fs::Metadata>,
    policy: ModePolicy,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match policy {
        ModePolicy::Force(mode) => mode,
        ModePolicy::Preserve => match existing {
            Some(metadata) => metadata.permissions().mode() & 0o7777,
            None => 0o666 & !umask(),
        },
    };
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn apply_mode(
    _file: &File,
    _existing: Option<&std::fs::Metadata>,
    _policy: ModePolicy,
) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn umask() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Umask:").map(str::to_string))
        })
        .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
        .unwrap_or(0o022)
}

/// Other Unix systems have no `/proc`, so the umask has to be read by clearing
/// it for an instant. The value is cached: the window exists once per process.
#[cfg(all(unix, not(target_os = "linux")))]
fn umask() -> u32 {
    static UMASK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *UMASK.get_or_init(|| {
        // SAFETY: reading the umask requires setting it; it is restored at once.
        unsafe {
            let current = libc::umask(0);
            libc::umask(current);
            current as u32
        }
    })
}

#[cfg(unix)]
fn identity_of_handle(file: &File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn identity_of_handle(_file: &File) -> Option<(u64, u64)> {
    None
}

/// Unlink the temp only when it is still the file this operation created.
#[cfg(unix)]
fn remove_created(temp_path: &Path, created: Option<(u64, u64)>) {
    use std::os::unix::fs::MetadataExt;
    let Some(created) = created else {
        return;
    };
    let still_ours = std::fs::symlink_metadata(temp_path)
        .map(|metadata| (metadata.dev(), metadata.ino()) == created)
        .unwrap_or(false);
    if still_ours {
        let _ = std::fs::remove_file(temp_path);
    }
}

#[cfg(not(unix))]
fn remove_created(temp_path: &Path, _created: Option<(u64, u64)>) {
    let _ = std::fs::remove_file(temp_path);
}

#[cfg(test)]
#[path = "tests/core/atomic_file.rs"]
mod tests;
