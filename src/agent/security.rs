use std::path::{Component, Path, PathBuf};

/// Metadata and cloud/SSH/GPG config directories. A component match refuses the
/// whole subtree, because everything below them is either a secret or a clue.
pub const PROTECTED_DIRS: &[&str] = &[".git", ".ssh", ".gnupg", ".aws"];
/// Whole file names that are credential stores under any extension convention.
pub const PROTECTED_NAMES: &[&str] = &[
    "credentials",
    "credentials.json",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "secrets.toml",
    ".npmrc",
    ".pypirc",
    ".htpasswd",
];
/// Prefixes: `.env` and its variants, SSH private keys, `.netrc`.
pub const PROTECTED_PREFIXES: &[&str] = &[".env", "id_rsa", "id_ed25519", "id_ecdsa", ".netrc"];
/// Suffixes that are key material whatever the base name is.
pub const PROTECTED_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".kdbx",
    ".jks",
    ".keystore",
    ".ppk",
];
/// Session database files: agents may neither read nor write the store.
const INTERNAL_STORE_NAMES: &[&str] = &["sessions.db", "sessions.db-wal", "sessions.db-shm"];

/// Refuse credentials and Seavx's internal store. Matching is case-insensitive,
/// and every rule is a list entry so a new class is data, not a new branch.
pub fn check_file(path: &Path) -> Result<(), String> {
    let names: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();

    if let Some(dir) = names
        .iter()
        .find(|name| PROTECTED_DIRS.contains(&name.as_str()))
    {
        return Err(format!("Access denied to protected sensitive file: {dir}"));
    }

    if names
        .windows(2)
        .any(|pair| pair[0] == ".seavx" && pair[1] == "checkpoints")
    {
        return Err("Access denied to internal store: .seavx/checkpoints".to_string());
    }

    if names.iter().any(|name| name == ".seavx")
        && let Some(name) = names
            .iter()
            .find(|name| INTERNAL_STORE_NAMES.contains(&name.as_str()))
    {
        return Err(format!("Access denied to internal store: {name}"));
    }

    let file_name = names.last().map(String::as_str).unwrap_or_default();
    let in_seavx = names.len() >= 2 && names[names.len() - 2] == ".seavx";
    if in_seavx && file_name == "config.toml" {
        return Err(format!(
            "Access denied to protected sensitive file: {file_name}"
        ));
    }

    let protected = PROTECTED_PREFIXES
        .iter()
        .any(|prefix| file_name.starts_with(prefix))
        || PROTECTED_NAMES.contains(&file_name)
        || PROTECTED_SUFFIXES
            .iter()
            .any(|suffix| file_name.ends_with(suffix));
    if protected {
        return Err(format!(
            "Access denied to protected sensitive file: {file_name}"
        ));
    }

    Ok(())
}

/// Identity of a path as seen before opening it. Proving what was validated is
/// the point: the opened handle must match this, or the path changed in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSnapshot {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub len: u64,
}

/// Snapshot without following symlinks, so a link is described as a link.
pub fn snapshot(path: &Path) -> std::io::Result<PathSnapshot> {
    snapshot_of(&std::fs::symlink_metadata(path)?)
}

fn snapshot_of(meta: &std::fs::Metadata) -> std::io::Result<PathSnapshot> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(PathSnapshot {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            len: meta.len(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(PathSnapshot {
            dev: 0,
            ino: 0,
            mode: 0,
            len: meta.len(),
        })
    }
}

/// Open a file that was validated against `expected`, and prove the handle is the
/// same object. The pre-check runs before the open because opening a FIFO waits
/// for a writer; on Unix `O_NONBLOCK` keeps that window from blocking either.
pub async fn open_verified(
    path: &Path,
    expected: &PathSnapshot,
) -> std::io::Result<tokio::fs::File> {
    let pre = std::fs::symlink_metadata(path)?;
    if !pre.is_file() {
        return Err(std::io::Error::other(format!(
            "refusing to open {}: it is not a regular file",
            path.display()
        )));
    }

    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path).await?;

    let opened = file.metadata().await?;
    if !opened.is_file() || snapshot_of(&opened)? != *expected {
        return Err(std::io::Error::other(
            "path changed between validation and open",
        ));
    }

    Ok(file)
}

/// Read a path through the validated-and-verified sequence in one step.
pub async fn read_verified(path: &Path) -> Result<String, String> {
    let before = snapshot(path).map_err(|e| format!("Failed to read file: {e}"))?;
    let mut file = open_verified(path, &before)
        .await
        .map_err(|e| format!("Failed to read file: {e}"))?;
    let mut body = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut file, &mut body)
        .await
        .map_err(|e| format!("Failed to read file: {e}"))?;
    Ok(body)
}

/// An existing file with another link may be written through that other link, so
/// a write is refused while the read stays allowed. Unix-only, like `nlink`.
pub fn check_write_target(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.is_file()
            && meta.nlink() > 1
        {
            return Err(format!(
                "file has {} hard links; refusing to write a file with links outside the workspace",
                meta.nlink()
            ));
        }
    }
    let _ = path;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct WorkspaceSecurity {
    root: PathBuf,
}

impl WorkspaceSecurity {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let canonical = root.canonicalize().unwrap_or(root);
        Self { root: canonical }
    }

    pub fn validate_path(&self, requested: &str) -> Result<PathBuf, String> {
        let path = Path::new(requested);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };

        let canonical = self.canonicalize_with_new_tail(&resolved)?;

        if !canonical.starts_with(&self.root) {
            return Err(format!(
                "Security violation: path '{}' escapes workspace '{}'",
                requested,
                self.root.display()
            ));
        }

        check_file(&canonical)?;
        Ok(canonical)
    }

    /// Canonicalise the deepest existing ancestor and re-join the tail, so a
    /// nested path that does not exist yet still resolves through the real tree.
    fn canonicalize_with_new_tail(&self, resolved: &Path) -> Result<PathBuf, String> {
        let mut ancestor = resolved;
        let mut missing = Vec::new();
        while !ancestor.exists() {
            let name = ancestor
                .file_name()
                .ok_or_else(|| format!("Invalid path: {}", resolved.display()))?;
            missing.push(name.to_os_string());
            ancestor = ancestor
                .parent()
                .ok_or_else(|| format!("Invalid path: {}", resolved.display()))?;
        }

        let mut canonical = if ancestor.as_os_str().is_empty() {
            self.root.clone()
        } else {
            ancestor
                .canonicalize()
                .map_err(|e| format!("Failed to canonicalize path: {e}"))?
        };
        for name in missing.iter().rev() {
            canonical.push(name);
        }
        Ok(canonical)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
#[path = "../tests/core/security.rs"]
mod tests;
