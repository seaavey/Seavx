//! User home directory, portable across Unix (`HOME`) and Windows (`USERPROFILE`).

use std::path::PathBuf;

pub fn dir() -> Option<PathBuf> {
    // Windows does not set HOME, so USERPROFILE is the fallback there.
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
