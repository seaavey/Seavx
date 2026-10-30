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
    // config.toml holds the key; SOUL.md is injected into the system prompt
    // ahead of the security policy, so a tool must never rewrite it.
    if in_seavx && matches!(file_name, "config.toml" | "soul.md") {
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

/// Assignment keys whose value is masked to the end of the line.
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
];
/// Token shapes masked wherever they appear. Heuristic, not a secret scanner:
/// a determined format change slips through until it is added here.
const TOKEN_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "xoxb-", "xoxa-", "xoxp-", "ntn_", "AKIA", "Bearer ", "bearer ",
];

/// Mask every documented secret shape so tool text can reach a log, an error or
/// the model without carrying a credential. Ordinary text is returned unchanged.
pub fn redact(text: &str) -> String {
    mask_pem_bodies(text)
        .split('\n')
        .map(redact_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A private-key block keeps its BEGIN header and loses its body.
fn mask_pem_bodies(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_body = false;
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if in_body {
            if line.contains("-----END") {
                in_body = false;
                out.push_str(line);
            } else {
                out.push_str("[redacted]");
            }
            continue;
        }
        if line.starts_with("-----BEGIN") && line.contains("PRIVATE KEY-----") {
            in_body = true;
        }
        out.push_str(line);
    }
    out
}

fn redact_line(line: &str) -> String {
    if let Some(start) = secret_value_start(line) {
        return format!("{}[redacted]", &line[..start]);
    }
    mask_tokens(line)
}

/// Byte offset where an assignment's value begins, for the earliest key that has
/// a `:` or `=` separator after its name.
fn secret_value_start(line: &str) -> Option<usize> {
    let lower = line.to_ascii_lowercase();
    let mut earliest: Option<usize> = None;
    for key in SECRET_KEYS {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let start = from + found;
            let rest = &line[start + key.len()..];
            if let Some(value_start) = value_offset(rest, start + key.len()) {
                earliest = Some(earliest.map_or(value_start, |current| current.min(value_start)));
                break;
            }
            from = start + key.len();
        }
    }
    earliest
}

/// Offset of the value after `rest` (the text following a key), or `None` when
/// no separator follows. Covers `key=value`, `key: value` and the quoted form
/// `"key": "value"`. `base` is `rest`'s offset in the line.
fn value_offset(rest: &str, base: usize) -> Option<usize> {
    let mut cursor = skip_blanks(rest, 0);
    // The key was quoted: this quote closes it.
    if let Some(quote) = rest[cursor..]
        .chars()
        .next()
        .filter(|ch| matches!(ch, '"' | '\''))
    {
        cursor = skip_blanks(rest, cursor + quote.len_utf8());
    }
    let separator = rest[cursor..].chars().next()?;
    if !matches!(separator, '=' | ':') {
        return None;
    }
    cursor = skip_blanks(rest, cursor + separator.len_utf8());
    // The value itself may be quoted.
    if let Some(quote) = rest[cursor..]
        .chars()
        .next()
        .filter(|ch| matches!(ch, '"' | '\''))
    {
        cursor += quote.len_utf8();
    }
    if rest[cursor..].trim().is_empty() {
        return None;
    }
    Some(base + cursor)
}

fn skip_blanks(text: &str, from: usize) -> usize {
    from + text[from..].len() - text[from..].trim_start_matches([' ', '\t']).len()
}

fn mask_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < line.len() {
        let boundary = index == 0
            || !line[..index]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric());
        if boundary && let Some(len) = token_len_at(line, index) {
            out.push_str("[redacted]");
            index += len;
            continue;
        }
        let ch = line[index..].chars().next().unwrap_or_default();
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn token_len_at(line: &str, index: usize) -> Option<usize> {
    let rest = &line[index..];
    let prefix = TOKEN_PREFIXES
        .iter()
        .find(|prefix| rest.starts_with(**prefix))?;
    let mut len = prefix.len();
    let mut truncated = false;
    for ch in rest[prefix.len()..].chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            len += ch.len_utf8();
        } else if ch == '…' {
            // An ellipsis marks a secret that was already cut short for display.
            truncated = true;
            len += ch.len_utf8();
            break;
        } else {
            break;
        }
    }
    // A run shorter than this is prose ("risk-averse"), not a secret.
    let minimum = if *prefix == "AKIA" { 8 } else { 4 };
    (truncated || len - prefix.len() >= minimum).then_some(len)
}

/// Identity of a path as seen before opening it. Proving what was validated is
/// the point: the opened handle must match this, or the path changed in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSnapshot {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub len: u64,
    /// Modification time: catches an in-place edit that keeps the length, which
    /// `(dev, ino, len)` alone cannot see.
    pub modified: Option<std::time::SystemTime>,
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
            modified: meta.modified().ok(),
        })
    }
    #[cfg(not(unix))]
    {
        // Windows has no stable file id in `std`: identity degrades to length,
        // mode and modification time, which the README states.
        Ok(PathSnapshot {
            dev: 0,
            ino: 0,
            mode: 0,
            len: meta.len(),
            modified: meta.modified().ok(),
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
    // A line limit does not bound memory: refuse an oversized file before the
    // whole thing is read into the process.
    if before.len > crate::atomic_file::MAX_ATOMIC_BYTES {
        return Err(format!(
            "Failed to read file: {} bytes is over the {} byte read limit",
            before.len,
            crate::atomic_file::MAX_ATOMIC_BYTES
        ));
    }
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

    /// Whether `path` (which may not exist yet) resolves inside the workspace.
    /// Symlinks are resolved, so a link that points out is outside.
    pub fn contains(&self, path: &Path) -> bool {
        self.canonicalize_with_new_tail(path)
            .map(|canonical| canonical.starts_with(&self.root))
            .unwrap_or(false)
    }
}

#[cfg(test)]
#[path = "../tests/core/security.rs"]
mod tests;
