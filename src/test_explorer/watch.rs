//! Watching the workspace folders for changes made outside the editor (git checkout, other
//! tools), so tests, lint results and builds stay in sync.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub struct FsWatcher {
    /// Shared with the thread that adds the folders' watches.
    watcher: Arc<Mutex<RecommendedWatcher>>,
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
        .is_some_and(|n| n.to_string_lossy().ends_with(".jdc-tmp"))
}

impl FsWatcher {
    /// Watch the folders' directories that matter: not `node_modules`, build output or
    /// anything gitignored (watching those costs a kernel watch per directory and floods the
    /// editor with events on every install or build). The directories are listed and watched
    /// on a background thread; directories created later are added as they appear.
    pub fn new(folders: &[PathBuf]) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })
        .map_err(|e| log::warn!("file watcher unavailable: {e}"))
        .ok()?;
        // The roots right away (changes there show at once), their subfolders in the background.
        let mut watcher = watcher;
        for f in folders {
            if let Err(e) = watcher.watch(f, RecursiveMode::NonRecursive) {
                log::warn!("cannot watch {}: {e}", f.display());
            }
        }
        let watcher = Arc::new(Mutex::new(watcher));
        let shared = watcher.clone();
        let folders = folders.to_vec();
        std::thread::Builder::new()
            .name("jdc-watch-setup".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let mut n = 0;
                for f in &folders {
                    for e in watched_dirs(f).into_iter().filter(|d| d != f) {
                        let r = shared
                            .lock()
                            .map(|mut w| w.watch(&e, RecursiveMode::NonRecursive));
                        match r {
                            Ok(Ok(())) => n += 1,
                            Ok(Err(err)) => log::warn!("cannot watch {}: {err}", e.display()),
                            Err(_) => return,
                        }
                    }
                }
                log::info!("watching {n} folders ({:?})", started.elapsed());
            })
            .ok()?;
        Some(Self { watcher, rx })
    }

    /// Changes since the last call, without duplicates.
    pub fn drain(&self) -> Vec<Change> {
        let mut out: Vec<Change> = Vec::new();
        let mut at: HashMap<PathBuf, usize> = HashMap::new();
        for ev in self.rx.try_iter().flatten() {
            let removed = matches!(ev.kind, EventKind::Remove(_));
            if matches!(ev.kind, EventKind::Access(_)) {
                continue;
            }
            let created = matches!(ev.kind, EventKind::Create(_));
            for path in ev.paths {
                if ignored(&path) {
                    continue;
                }
                // A rename shows up as remove + create; the last event for a path wins.
                let removed = removed || !path.exists();
                // A new folder: watch it too.
                if created
                    && !removed
                    && path.is_dir()
                    && let Ok(mut w) = self.watcher.lock()
                {
                    let _ = w.watch(&path, RecursiveMode::NonRecursive);
                }
                match at.get(&path) {
                    Some(&k) => out[k].removed = removed,
                    None => {
                        at.insert(path.clone(), out.len());
                        out.push(Change { path, removed });
                    }
                }
            }
        }
        out
    }
}

/// The directories of `folder` worth watching (gitignore-aware, dependencies and build output
/// left out).
fn watched_dirs(folder: &Path) -> Vec<PathBuf> {
    crate::workspace::files::walker(folder, true)
        .filter_entry(|e| !ignored(e.path()))
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_dir()))
        .map(|e| e.into_path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_build_and_dependency_folders() {
        assert!(ignored(Path::new("/w/node_modules/x/a.js")));
        assert!(ignored(Path::new("/w/src/Api/obj/Debug/a.cs")));
        assert!(ignored(Path::new("/w/.a.js.jdc-tmp")));
        assert!(!ignored(Path::new("/w/src/a.spec.js")));
    }

    #[test]
    fn watches_subfolders_but_not_dependencies() {
        let dir = std::env::temp_dir().join(format!("jdc-watch-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let Some(w) = FsWatcher::new(std::slice::from_ref(&dir)) else {
            return;
        };
        // Let the background setup add the subfolders.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let deep = dir.join("src/deep/a.js");
        std::fs::write(&deep, "x").unwrap();
        std::fs::write(dir.join("node_modules/pkg/b.js"), "y").unwrap();
        let mut got = Vec::new();
        for _ in 0..50 {
            got.extend(w.drain());
            if got.iter().any(|c: &Change| c.path == deep) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(got.iter().any(|c| c.path == deep), "{got:?}");
        assert!(!got.iter().any(|c| ignored(&c.path)), "{got:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reports_changes() {
        let dir = std::env::temp_dir().join(format!("jdc-watch-{}", std::process::id()));
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
