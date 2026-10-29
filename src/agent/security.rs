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
