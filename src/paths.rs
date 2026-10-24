//! Per-user folders.

use std::path::PathBuf;

fn env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Folder for state that survives restarts (last workspace, logs).
pub fn state_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env("LOCALAPPDATA").map(|d| d.join("jmx"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/jmx"))
    } else {
        env("XDG_STATE_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local/state")))
            .map(|d| d.join("jmx"))
    }
}

/// Where `AppState` is kept by default.
pub fn state_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("state.json"))
}
