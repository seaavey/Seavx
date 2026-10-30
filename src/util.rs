//! Small shared helpers used across the CLI, TUI and agent modules.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The `--debug` flag, recorded once before anything reads [`is_debug`].
static DEBUG_FLAG: AtomicBool = AtomicBool::new(false);

/// Turn debug mode on for the rest of the process. Called by `main` when
/// `--debug` is present.
pub fn set_debug_flag() {
    DEBUG_FLAG.store(true, Ordering::Relaxed);
}

/// Debug mode: `--debug`, or `SEAVX_DEBUG=1|true` from the environment or
/// `.env`. Neither means the release paths — a debug build is not a debug run,
/// so `cargo run` writes to `~/.seavx` like an installed binary unless it is
/// asked otherwise.
pub fn is_debug() -> bool {
    DEBUG_FLAG.load(Ordering::Relaxed)
        || debug_flag(std::env::var("SEAVX_DEBUG").ok().as_deref()) == Some(true)
}

/// Record the resolved debug state in `dir/.env`, creating the file when it is
/// missing. The `SEAVX_DEBUG` line is rewritten in place and every other line is
/// left as it was, so a run never has to infer the state a later run will use.
pub fn persist_debug_flag(dir: &Path, debug: bool) -> std::io::Result<PathBuf> {
    let path = dir.join(".env");
    let line = format!("SEAVX_DEBUG={}", if debug { "true" } else { "false" });
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    match lines.iter_mut().find(|entry| is_debug_line(entry)) {
        Some(slot) if *slot == line => return Ok(path),
        Some(slot) => *slot = line,
        None => lines.push(line),
    }

    let mut content = lines.join("\n");
    content.push('\n');
    crate::atomic_file::write_atomic(
        &path,
        content.as_bytes(),
        crate::atomic_file::ModePolicy::Preserve,
    )?;
    Ok(path)
}

/// `SEAVX_DEBUG=…`, `SEAVX_DEBUG = …` and `export SEAVX_DEBUG=…` are the same
/// key; a comment or a longer name is not.
fn is_debug_line(line: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
    line.strip_prefix("SEAVX_DEBUG")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// The value of `SEAVX_DEBUG`: `Some(true)` for `1`/`true`, `Some(false)` for
/// `0`/`false`, `None` when unset or unrecognised — the caller then falls back
/// to the build profile.
fn debug_flag(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim) {
        Some("1") | Some("true") => Some(true),
        Some("0") | Some("false") => Some(false),
        _ => None,
    }
}

/// Seconds since the Unix epoch, `0` on clock failure.
pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Milliseconds since the Unix epoch, `0` on clock failure.
pub fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Global (per-user) config.toml path; `None` when the home directory is unset.
pub fn global_config_path() -> Option<PathBuf> {
    crate::home::dir().map(|home| home.join(".seavx/config.toml"))
}

/// Entries skipped by every workspace walk: hidden dirs, build artifacts, deps.
pub fn is_skipped(name: &str) -> bool {
    name.starts_with('.') || name == "target" || name == "node_modules"
}

/// Value of a `key: value` line in a markdown-like document, quotes stripped.
pub fn frontmatter_value(md: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    md.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .map(unquote)
        .map(str::to_string)
}

/// Trim whitespace, then a single pair of double or single quotes.
pub fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"').trim_matches('\'')
}

/// Create the directory and tighten an existing looser mode, so a legacy
/// `0755` directory is repaired instead of trusted. One log line per repair.
pub fn ensure_private_dir(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    tighten(path, mode)
}

/// Create the file (and its parent) and tighten an existing looser mode.
pub fn ensure_private_file(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
    }
    tighten(path, mode)
}

#[cfg(unix)]
fn tighten(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let current = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if current != mode {
        tracing::info!(
            path = %path.display(),
            from = format!("{current:o}"),
            to = format!("{mode:o}"),
            "tightened storage mode"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn tighten(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// The debug log under `dir`, with the directory `0700` and the file `0600`.
pub fn prepare_log_file(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    ensure_private_dir(dir, 0o700)?;
    let path = dir.join("debug.log");
    ensure_private_file(&path, 0o600)?;
    Ok(path)
}

#[cfg(test)]
#[path = "tests/core/util.rs"]
mod tests;
