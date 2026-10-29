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
