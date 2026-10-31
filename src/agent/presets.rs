//! Prompt presets: static text a user keeps for reuse.
//!
//! Two directories, project first: `.seavx/prompts/<name>.md` shadows
//! `~/.seavx/prompts/<name>.md`. A preset is text and nothing else — it is put
//! in the next prompt, never in the system prompt, and it cannot change the
//! mode, the permission policy or the security rules.

use std::path::{Path, PathBuf};

/// A preset larger than this is refused rather than silently truncated.
pub const PRESET_LIMIT: u64 = 64 * 1024;

/// A name the loader will accept: no separators, no dots, no traversal.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn project_dir(root: &Path) -> PathBuf {
    root.join(".seavx").join("prompts")
}

pub fn user_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".seavx").join("prompts"))
}

/// Every preset the directories hold, project first. A name that appears in
/// both is listed once: the project copy is the one that will be loaded.
pub fn list(root: &Path) -> Vec<(String, PathBuf)> {
    list_in(root, user_dir().as_deref())
}

/// `list` with the user directory named, so a test never has to move `HOME`.
pub fn list_in(root: &Path, user: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut dirs = vec![project_dir(root)];
    if let Some(user) = user
        && user != project_dir(root)
    {
        dirs.push(user.to_path_buf());
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".md"))
            else {
                continue;
            };
            if !valid_name(name) || found.iter().any(|(seen, _)| seen == name) {
                continue;
            }
            found.push((name.to_string(), path));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Read one preset. Every refusal names the file and the reason; nothing is
/// guessed and nothing partial is returned.
pub fn load(root: &Path, name: &str) -> Result<String, String> {
    load_in(root, user_dir().as_deref(), name)
}

/// `load` with the user directory named, so a test never has to move `HOME`.
pub fn load_in(root: &Path, user: Option<&Path>, name: &str) -> Result<String, String> {
    if !valid_name(name) {
        return Err(format!(
            "'{name}' is not a preset name; use letters, digits, '-' or '_'"
        ));
    }
    let mut candidates = vec![project_dir(root).join(format!("{name}.md"))];
    if let Some(user) = user {
        let path = user.join(format!("{name}.md"));
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }

    for path in candidates {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{} is a symlink; presets must be plain files",
                path.display()
            ));
        }
        if !metadata.is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        if metadata.len() > PRESET_LIMIT {
            return Err(format!(
                "{} is {} bytes; the limit is {PRESET_LIMIT}",
                path.display(),
                metadata.len()
            ));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        return Ok(text);
    }

    Err(format!(
        "no preset named '{name}'; try /preset to list what is there"
    ))
}

#[cfg(test)]
#[path = "../tests/agent/presets.rs"]
mod tests;
