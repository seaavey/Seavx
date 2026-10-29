//! Chat prompt history persisted per workspace under `.seavx/`.
//!
//! One prompt per line so shell-style prefix matching stays simple; embedded
//! newlines are escaped as literal `\n` inside the file. The file lives next
//! to `config.toml` at `.seavx/svx_history`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Newest prompts the history file may keep before the front is dropped.
const MAX_ENTRIES: usize = 500;

/// Where Seavx keeps its per-workspace data (config.toml, history, ...).
pub fn config_dir() -> PathBuf {
    PathBuf::from(".seavx")
}

pub struct ChatHistory {
    path: PathBuf,
}

impl ChatHistory {
    pub fn load(dir: &Path) -> Self {
        Self {
            path: dir.join("svx_history"),
        }
    }

    /// All stored prompts in submit order, oldest first. Missing file = empty.
    pub fn entries(&self) -> Vec<String> {
        let Ok(file) = File::open(&self.path) else {
            return Vec::new();
        };
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .map(|line| self::unescape(&line))
            .filter(|entry| !entry.trim().is_empty())
            .collect()
    }

    /// Last modification time of the file, if it exists. The TUI watches this
    /// to notice prompts written by a manual edit or another session.
    pub fn modified(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(&self.path)
            .and_then(|meta| meta.modified())
            .ok()
    }

    /// Append a prompt. A repeat of the newest entry is skipped, and passing
    /// `MAX_ENTRIES` trims the oldest rows so the file stays small.
    pub fn append(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return;
        }
        if let Some(parent) = self.path.parent()
            && let Err(error) = crate::util::ensure_private_dir(parent, 0o700)
        {
            tracing::warn!(%error, path = %parent.display(), "cannot prepare the history directory");
        }

        let mut entries = self.entries();
        if matches!(entries.last(), Some(last) if last == prompt) {
            return;
        }
        entries.push(prompt.to_string());

        if entries.len() > MAX_ENTRIES {
            let dropped = entries.len() - MAX_ENTRIES;
            self.write_all(&entries.into_iter().skip(dropped).collect::<Vec<_>>());
        } else {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(mut file) => {
                    if let Err(error) = crate::util::ensure_private_file(&self.path, 0o600) {
                        tracing::warn!(%error, path = %self.path.display(), "cannot tighten the history file");
                    }
                    // A hand-edited file may lack its trailing newline; without
                    // this the new prompt would merge into the previous line.
                    let unterminated = std::fs::read(&self.path)
                        .is_ok_and(|bytes| bytes.last().is_some_and(|byte| *byte != b'\n'));
                    if unterminated {
                        let _ = file.write_all(b"\n");
                    }
                    if let Err(error) = writeln!(file, "{}", escape(prompt)) {
                        tracing::warn!(%error, path = %self.path.display(), "history write failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, path = %self.path.display(), "cannot open the history file");
                }
            }
        }
    }

    /// Rewrite through a temporary file: truncating in place would lose the
    /// whole history if the process died mid-write.
    fn write_all(&self, entries: &[String]) {
        let mut body = String::new();
        for entry in entries {
            body.push_str(&escape(entry));
            body.push('\n');
        }
        let tmp = self.path.with_extension("tmp");
        if let Err(error) = std::fs::write(&tmp, body) {
            tracing::warn!(%error, path = %tmp.display(), "history rewrite failed");
            return;
        }
        // The temp file is created with the umask, so it is tightened before it
        // replaces the owner-only history file.
        if let Err(error) = crate::util::ensure_private_file(&tmp, 0o600) {
            tracing::warn!(%error, path = %tmp.display(), "cannot tighten the history file");
        }
        if std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Embedded `\` and `\n` become literal `\\` and `\n` so one prompt = one line.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

/// Inverse of [`escape`], keeping unknown escape pairs the same.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                out.push('\n');
            }
            Some('\\') => {
                chars.next();
                out.push('\\');
            }
            _ => {
                out.push('\\');
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "tests/core/history.rs"]
mod tests;
