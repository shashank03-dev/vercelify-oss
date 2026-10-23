//! Tiny helpers for reading kernel pseudo-files.

use std::fs;
use std::path::Path;

/// Read a sysfs attribute, trimmed. `None` if missing or unreadable.
pub fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn read_parse<T: std::str::FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read_trim(path)?.parse().ok()
}

/// Sorted child entries of a directory (empty if it does not exist).
pub fn children(dir: impl AsRef<Path>) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    out.sort();
    out
}
