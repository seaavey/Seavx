use std::path::{Path, PathBuf};

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

        let canonical = if resolved.exists() {
            resolved
                .canonicalize()
                .map_err(|e| format!("Failed to canonicalize path: {e}"))?
        } else {
            let parent = resolved.parent().ok_or("Invalid path parent")?;
            let canonical_parent = if parent.as_os_str().is_empty() {
                self.root.clone()
            } else {
                parent
                    .canonicalize()
                    .map_err(|e| format!("Invalid path directory: {e}"))?
            };
            let file_name = resolved.file_name().ok_or("Missing file name")?;
            canonical_parent.join(file_name)
        };

        if !canonical.starts_with(&self.root) {
            return Err(format!(
                "Security violation: path '{}' escapes workspace '{}'",
                requested,
                self.root.display()
            ));
        }

        let sensitive_prefixes = [".env", "id_rsa", "id_ed25519"];
        let file_name = canonical
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        for s in &sensitive_prefixes {
            if file_name.starts_with(s) {
                return Err(format!(
                    "Access denied to protected sensitive file: {file_name}"
                ));
            }
        }

        if canonical.components().any(|c| c.as_os_str() == ".git") {
            return Err("Access denied to .git repository metadata".to_string());
        }

        Ok(canonical)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
