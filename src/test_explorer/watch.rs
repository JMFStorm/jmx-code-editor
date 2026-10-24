//! Watching the workspace folders for changes made outside the editor (git checkout, other
//! tools), so tests, lint results and builds stay in sync.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
}

/// A changed path, and whether it was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub removed: bool,
}

/// Folders whose changes never matter (build output, dependencies, VCS data).
fn ignored(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git" | "node_modules" | "bin" | "obj" | "dist" | "coverage" | "target" | ".vs")
        )
    }) || path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(".jmx-tmp"))
}

impl FsWatcher {
    pub fn new(folders: &[PathBuf]) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })
        .map_err(|e| log::warn!("file watcher unavailable: {e}"))
        .ok()?;
        for f in folders {
            if let Err(e) = watcher.watch(f, RecursiveMode::Recursive) {
                log::warn!("cannot watch {}: {e}", f.display());
            }
        }
        Some(Self {
            _watcher: watcher,
            rx,
        })
    }

    /// Changes since the last call, without duplicates.
    pub fn drain(&self) -> Vec<Change> {
        let mut out: Vec<Change> = Vec::new();
        for ev in self.rx.try_iter().flatten() {
            let removed = matches!(ev.kind, EventKind::Remove(_));
            if matches!(ev.kind, EventKind::Access(_)) {
                continue;
            }
            for path in ev.paths {
                if ignored(&path) {
                    continue;
                }
                // A rename shows up as remove + create; the last event for a path wins.
                let removed = removed || !path.exists();
                out.retain(|c| c.path != path);
                out.push(Change { path, removed });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_build_and_dependency_folders() {
        assert!(ignored(Path::new("/w/node_modules/x/a.js")));
        assert!(ignored(Path::new("/w/src/Api/obj/Debug/a.cs")));
        assert!(ignored(Path::new("/w/.a.js.jmx-tmp")));
        assert!(!ignored(Path::new("/w/src/a.spec.js")));
    }

    #[test]
    fn reports_changes() {
        let dir = std::env::temp_dir().join(format!("jmx-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let Some(w) = FsWatcher::new(std::slice::from_ref(&dir)) else {
            return; // no watcher support here
        };
        let file = dir.join("a.spec.js");
        std::fs::write(&file, "it('x', () => {})").unwrap();
        let mut got = Vec::new();
        for _ in 0..100 {
            got.extend(w.drain());
            if got.iter().any(|c: &Change| c.path == file) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(got.iter().any(|c| c.path == file && !c.removed), "{got:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
