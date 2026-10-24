//! Small state kept between runs: the workspace to reopen on the next start.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Workspace file open when the editor last ran; `None` after Close Workspace.
    pub last_workspace: Option<PathBuf>,
}

impl AppState {
    /// Read the state file; a missing or unreadable file is an empty state.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("ignoring broken state file {}: {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("cannot read state file {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Write the state file atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let tmp = path.with_extension("json.jmx-tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_files() {
        let dir = std::env::temp_dir().join(format!("jmx-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("sub/state.json");
        assert_eq!(AppState::load(&path), AppState::default());
        let s = AppState {
            last_workspace: Some("/w/p.jmxws".into()),
        };
        s.save(&path).unwrap();
        assert_eq!(AppState::load(&path), s);
        fs::write(&path, "{not json").unwrap();
        assert_eq!(AppState::load(&path), AppState::default());
        fs::remove_dir_all(&dir).unwrap();
    }
}
