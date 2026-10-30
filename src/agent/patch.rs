//! Strict unified-diff parsing and application to in-memory buffers.
//!
//! No dependency and no fuzz: every hunk must match exactly and the announced
//! line numbers are checked, so a patch either applies as written or names the
//! file, hunk and line that did not. Nothing here touches the filesystem — the
//! caller decides when to commit, after every file has been applied in memory.

use std::path::{Component, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<HunkLine>,
    /// The last line of the hunk is marked `\ No newline at end of file`.
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub path: PathBuf,
    pub hunks: Vec<Hunk>,
    pub create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchStats {
    pub added: usize,
    pub removed: usize,
    pub hunks: usize,
}

/// Parse a unified diff. Headers may carry the `a/`/`b/` prefix; a `..`
/// component or an absolute path is refused here, before any file is read.
pub fn parse(diff: &str) -> Result<Vec<FilePatch>, String> {
    let all: Vec<&str> = diff.lines().collect();
    let mut files = Vec::new();
    let mut index = 0;

    while index < all.len() {
        let line = all[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        let Some(old_header) = line.strip_prefix("--- ") else {
            return Err(format!(
                "line {}: expected a `--- ` file header, found `{line}`",
                index + 1
            ));
        };
        let Some(new_header) = all
            .get(index + 1)
            .and_then(|next| next.strip_prefix("+++ "))
        else {
            return Err(format!(
                "line {}: `--- ` without a following `+++ `",
                index + 1
            ));
        };

        let old_path = header_path(old_header)?;
        let new_path = header_path(new_header)?;
        let create = old_path == std::path::Path::new("/dev/null");
        if create {
            if new_path == std::path::Path::new("/dev/null") {
                return Err("line 1: both headers are /dev/null".to_string());
            }
        } else if old_path != new_path {
            return Err(format!(
                "line {}: renaming is not supported (`{}` -> `{}`)",
                index + 1,
                old_path.display(),
                new_path.display()
            ));
        }

        index += 2;
        let mut hunks = Vec::new();
        while index < all.len() && !all[index].starts_with("--- ") {
            if all[index].trim().is_empty() {
                index += 1;
                continue;
            }
            if !all[index].starts_with("@@") {
                return Err(format!(
                    "line {}: expected a `@@` hunk header, found `{}`",
                    index + 1,
                    all[index]
                ));
            }
            let (hunk, next) = parse_hunk(&all, index)?;
            hunks.push(hunk);
            index = next;
        }

        if hunks.is_empty() {
            return Err(format!(
                "{}: no hunks (deletion is not supported)",
                new_path.display()
            ));
        }
        if create && hunks.iter().any(|hunk| hunk.old_count != 0) {
            return Err(format!(
                "{}: a created file must use `-0,0`",
                new_path.display()
            ));
        }

        files.push(FilePatch {
            path: new_path,
            hunks,
            create,
        });
    }

    if files.is_empty() {
        return Err("no file headers found".to_string());
    }
    Ok(files)
}

fn header_path(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.split('\t').next().unwrap_or_default().trim();
    if raw == "/dev/null" {
        return Ok(PathBuf::from("/dev/null"));
    }
    let stripped = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    let path = PathBuf::from(stripped);
    if path.is_absolute() {
        return Err(format!("absolute path in a diff header: `{raw}`"));
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(format!("parent path in a diff header: `{raw}`"));
    }
    if stripped.is_empty() {
        return Err("empty path in a diff header".to_string());
    }
    Ok(path)
}

fn parse_hunk(all: &[&str], start: usize) -> Result<(Hunk, usize), String> {
    let header = all[start];
    let (old_start, old_count, new_start, new_count) = parse_hunk_header(header)
        .ok_or_else(|| format!("line {}: malformed hunk header `{header}`", start + 1))?;

    let mut lines = Vec::new();
    let mut no_newline = false;
    let mut index = start + 1;
    while index < all.len() {
        let line = all[index];
        if line.starts_with("@@") || line.starts_with("--- ") {
            break;
        }
        if line == "\\ No newline at end of file" {
            no_newline = true;
            index += 1;
            continue;
        }
        if line.is_empty() {
            break;
        }
        let mut chars = line.chars();
        let marker = chars.next().unwrap_or_default();
        let text = chars.as_str().to_string();
        let kind = match marker {
            ' ' => HunkLine::Context(text),
            '-' => HunkLine::Remove(text),
            '+' => HunkLine::Add(text),
            _ => {
                return Err(format!(
                    "line {}: unexpected line in a hunk: `{line}`",
                    index + 1
                ));
            }
        };
        lines.push(kind);
        index += 1;
    }

    let context = lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_)))
        .count();
    let removed = lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Remove(_)))
        .count();
    let added = lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Add(_)))
        .count();
    if context + removed != old_count || context + added != new_count {
        return Err(format!(
            "line {}: hunk counts do not match its header (`{header}`)",
            start + 1
        ));
    }

    Ok((
        Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines,
            no_newline,
        },
        index,
    ))
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = match rest.find(" @@") {
        Some(position) => &rest[..position],
        None => rest.trim_end(),
    };
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_range(text: &str) -> Option<(usize, usize)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.trim().parse().ok()?, count.trim().parse().ok()?)),
        None => Some((text.trim().parse().ok()?, 1)),
    }
}

/// Apply a patch to a buffer. The announced line numbers decide where each hunk
/// starts; every context and removed line must match byte for byte.
pub fn apply_to(content: &str, patch: &FilePatch) -> Result<(String, PatchStats), String> {
    let source: Vec<&str> = content.split_inclusive('\n').collect();
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0usize;
    let mut previous_end = 0usize;
    let mut stats = PatchStats {
        added: 0,
        removed: 0,
        hunks: patch.hunks.len(),
    };

    for hunk in &patch.hunks {
        let start = if hunk.old_start == 0 {
            0
        } else {
            hunk.old_start - 1
        };
        if start < previous_end {
            return Err(mismatch(
                patch,
                hunk,
                format!(
                    "hunks are out of order or overlap at line {}",
                    hunk.old_start
                ),
            ));
        }
        if hunk.old_count > 0 && start >= source.len() || start > source.len() {
            return Err(mismatch(
                patch,
                hunk,
                format!(
                    "buffer has {} lines, hunk starts at {}",
                    source.len(),
                    hunk.old_start
                ),
            ));
        }

        for line in &source[cursor..start] {
            out.push_str(line);
        }

        let mut position = start;
        for body in &hunk.lines {
            match body {
                HunkLine::Add(text) => {
                    out.push_str(text);
                    out.push('\n');
                    stats.added += 1;
                }
                HunkLine::Context(text) => {
                    let Some(line) = source.get(position) else {
                        return Err(mismatch(
                            patch,
                            hunk,
                            format!("line {} is past the end of the buffer", position + 1),
                        ));
                    };
                    if line_text(line) != *text {
                        return Err(context_error(patch, hunk, position + 1, line, text));
                    }
                    out.push_str(line);
                    position += 1;
                }
                HunkLine::Remove(text) => {
                    let Some(line) = source.get(position) else {
                        return Err(mismatch(
                            patch,
                            hunk,
                            format!("line {} is past the end of the buffer", position + 1),
                        ));
                    };
                    if line_text(line) != *text {
                        return Err(context_error(patch, hunk, position + 1, line, text));
                    }
                    position += 1;
                    stats.removed += 1;
                }
            }
        }

        cursor = position;
        previous_end = position;
    }

    for line in &source[cursor..] {
        out.push_str(line);
    }

    // The file's trailing newline is data: keep it missing when the last line
    // was rewritten and the original had none, even if the diff omitted its
    // marker.
    let touched_end = cursor >= source.len();
    let last_is_add = patch
        .hunks
        .last()
        .and_then(|hunk| hunk.lines.last())
        .is_some_and(|line| matches!(line, HunkLine::Add(_)));
    let keep_missing = patch.hunks.last().is_some_and(|hunk| hunk.no_newline)
        || (touched_end && last_is_add && !content.is_empty() && !content.ends_with('\n'));
    if keep_missing && out.ends_with('\n') {
        out.pop();
    }

    Ok((out, stats))
}

/// The line without its terminator, for byte-exact comparison.
fn line_text(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

fn context_error(
    patch: &FilePatch,
    hunk: &Hunk,
    line_number: usize,
    actual: &str,
    expected: &str,
) -> String {
    let actual_text = line_text(actual);
    let detail = if actual_text.strip_suffix('\r') == Some(expected) {
        format!("line {line_number}: line endings differ (the file is CRLF, the hunk is LF)")
    } else {
        format!(
            "line {line_number}: expected `{expected}`, found `{}`",
            actual_text.trim_end_matches('\r')
        )
    };
    mismatch(patch, hunk, detail)
}

fn mismatch(patch: &FilePatch, hunk: &Hunk, detail: String) -> String {
    format!(
        "{}: @@ -{},{} +{},{} @@: {detail}",
        patch.path.display(),
        hunk.old_start,
        hunk.old_count,
        hunk.new_start,
        hunk.new_count
    )
}

#[cfg(test)]
#[path = "../tests/core/patch.rs"]
mod tests;
