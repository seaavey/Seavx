//! Small shared helpers used across the CLI, TUI and agent modules.

use std::path::PathBuf;

/// Debug mode: `SEAVX_DEBUG=1|true` or a debug build.
pub fn is_debug() -> bool {
    std::env::var("SEAVX_DEBUG")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false)
        || cfg!(debug_assertions)
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
