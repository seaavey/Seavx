use super::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::tempdir;

/// Changing the process umask is global, so the mode tests take turns.
static UMASK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct UmaskRestore(libc::mode_t);

impl Drop for UmaskRestore {
    fn drop(&mut self) {
        // SAFETY: restoring the value this guard replaced.
        unsafe { libc::umask(self.0) };
    }
}

/// Deterministic temp path inside `dir`, so collision and cleanup tests can name it.
fn temp_beside(target: &Path, dir: &Path) -> std::path::PathBuf {
    let name = target.file_name().unwrap().to_string_lossy().to_string();
    dir.join(format!(".{name}.seavx-tmp-test"))
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_existing_mode() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("script.sh");
    fs::write(&target, "old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

    write_atomic(&target, b"new", ModePolicy::Preserve).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    assert_eq!(mode_of(&target), 0o755);
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_umask_for_new_file() {
    let temp = tempdir().unwrap();
    // A plain write in the same directory shows what this process's umask allows.
    let reference = temp.path().join("reference.txt");
    fs::write(&reference, "reference").unwrap();

    let target = temp.path().join("created.txt");
    write_atomic(&target, b"created", ModePolicy::Preserve).unwrap();

    assert_eq!(mode_of(&target), mode_of(&reference));
}

#[cfg(unix)]
#[test]
fn atomic_write_forces_requested_mode() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("config.toml");
    fs::write(&target, "old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

    write_atomic(&target, b"new", ModePolicy::Force(0o600)).unwrap();

    assert_eq!(mode_of(&target), 0o600);
}

#[cfg(unix)]
#[test]
fn force_mode_ignores_a_permissive_umask() {
    let _serial = UMASK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: reading the process umask requires setting it; the guard restores it.
    let previous = unsafe { libc::umask(0) };
    let _restore = UmaskRestore(previous);

    let temp = tempdir().unwrap();
    let forced = temp.path().join("forced.toml");
    write_atomic(&forced, b"x", ModePolicy::Force(0o600)).unwrap();
    assert_eq!(mode_of(&forced), 0o600, "Force ignores the umask");

    // Preserve on a new file follows the umask the process really has.
    let permissive = temp.path().join("permissive.txt");
    write_atomic(&permissive, b"x", ModePolicy::Preserve).unwrap();
    assert_eq!(mode_of(&permissive), 0o666);

    // SAFETY: same guard, second value; the drop restores the original.
    unsafe { libc::umask(0o077) };
    let restricted = temp.path().join("restricted.txt");
    write_atomic(&restricted, b"x", ModePolicy::Preserve).unwrap();
    assert_eq!(mode_of(&restricted), 0o600);
}

#[cfg(unix)]
#[test]
fn atomic_temp_creation_is_private() {
    let temp = tempdir().unwrap();
    let staging = temp.path().join(".private.seavx-tmp-test");

    let file = create_temp_file(&staging).unwrap();

    assert_eq!(mode_of(&staging), 0o600);
    drop(file);
}

#[test]
fn atomic_write_leaves_original_on_failure() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("kept.txt");
    fs::write(&target, "original").unwrap();
    // Staging into a directory that does not exist has to fail before the rename.
    let missing = temp.path().join("missing");

    let error = write_atomic_with_temp(
        &target,
        b"replacement",
        ModePolicy::Preserve,
        &missing.join(".kept.seavx-tmp-test"),
    )
    .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    assert!(!missing.exists());
}

#[test]
fn atomic_write_does_not_follow_or_remove_a_planted_temp() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("target.txt");
    fs::write(&target, "original").unwrap();
    let staging = temp_beside(&target, temp.path());
    fs::write(&staging, "planted").unwrap();

    let error = write_atomic_with_temp(&target, b"replacement", ModePolicy::Preserve, &staging)
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&staging).unwrap(), "planted");
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
}

#[cfg(unix)]
#[test]
fn atomic_write_does_not_follow_a_symlinked_temp() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("target.txt");
    fs::write(&target, "original").unwrap();
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "outside").unwrap();
    let staging = temp_beside(&target, temp.path());
    std::os::unix::fs::symlink(&outside, &staging).unwrap();

    let error = write_atomic_with_temp(&target, b"replacement", ModePolicy::Preserve, &staging)
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
}

#[test]
fn atomic_write_cleans_temp_when_destination_is_directory() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("dir");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("child"), "child").unwrap();
    let staging = temp_beside(&target, temp.path());

    let error = write_atomic_with_temp(&target, b"replacement", ModePolicy::Preserve, &staging)
        .unwrap_err();

    assert!(!error.to_string().is_empty());
    assert!(!staging.exists(), "the staged temp must be cleaned up");
    assert!(target.is_dir(), "the directory itself is untouched");
    assert_eq!(fs::read_to_string(target.join("child")).unwrap(), "child");
}

#[test]
fn atomic_write_refuses_over_limit_before_staging() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("big.bin");
    fs::write(&target, "original").unwrap();
    let staging = temp_beside(&target, temp.path());
    let oversize = vec![b'x'; MAX_ATOMIC_BYTES as usize + 1];

    let error =
        write_atomic_with_temp(&target, &oversize, ModePolicy::Preserve, &staging).unwrap_err();

    assert!(error.to_string().contains("over 16 MB"), "{error}");
    assert!(
        !staging.exists(),
        "nothing is staged for an over-limit write"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
}

#[test]
fn atomic_write_refuses_over_limit_existing_file() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("big.bin");
    fs::write(&target, vec![b'x'; MAX_ATOMIC_BYTES as usize + 1]).unwrap();

    let error = write_atomic(&target, b"small", ModePolicy::Preserve).unwrap_err();

    assert!(error.to_string().contains("over 16 MB"), "{error}");
    assert_eq!(
        fs::read(&target).unwrap().len() as u64,
        MAX_ATOMIC_BYTES + 1
    );
}

#[test]
fn atomic_write_creates_missing_parents() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("a/b/c.txt");

    write_atomic(&target, b"nested", ModePolicy::Preserve).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "nested");
    assert!(
        fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains("seavx-tmp")),
        "no temp file is left behind"
    );
}
