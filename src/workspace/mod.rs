//! Workspaces: a named set of folders plus the full editor state, saved as a `.jmxws` file
//! (pretty JSON). Paths under the workspace file's folder are stored relative to it.

pub mod files;
pub mod grep;

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::editor::document::Document;
use crate::editor::search::SearchOptions;
use crate::editor::selection::Selection;
use crate::editor::{Editor, Pane};
use crate::help;
use crate::ui::explorer::{DEFAULT_WIDTH_COLS, MAX_WIDTH_COLS, MIN_WIDTH_COLS};

pub const EXTENSION: &str = "jmxws";
const FORMAT_VERSION: u32 = 1;

/// The open workspace (workspace mode). `None` on the editor means file mode.
#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub folders: Vec<PathBuf>,
    pub respect_gitignore: bool,
    /// Where the workspace is saved; `None` until the first save.
    pub file: Option<PathBuf>,
}

impl Workspace {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.trim().to_string(),
            folders: Vec::new(),
            respect_gitignore: true,
            file: None,
        }
    }

    /// Suggested file name, e.g. "My Project" → "My Project.jmxws".
    pub fn default_file_name(&self) -> String {
        let clean: String = self
            .name
            .chars()
            .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
            .collect();
        let clean = if clean.trim().is_empty() {
            "workspace".to_string()
        } else {
            clean
        };
        format!("{clean}.{EXTENSION}")
    }
}

pub fn is_workspace_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == EXTENSION)
}

// ---------------------------------------------------------------- file format

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceFile {
    pub version: u32,
    pub name: String,
    pub folders: Vec<String>,
    pub respect_gitignore: bool,
    #[serde(default)]
    pub state: SessionState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionState {
    pub split: bool,
    pub split_ratio: f32,
    pub focus: usize,
    pub font_size: f32,
    pub explorer: ExplorerState,
    pub panes: Vec<PaneState>,
    pub search: SearchState,
}

/// Last Find in Files query, file filter and search options.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchState {
    pub query: String,
    pub include: String,
    pub options: SearchOptions,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            split: true,
            split_ratio: 0.5,
            focus: 0,
            font_size: 15.0,
            explorer: ExplorerState::default(),
            panes: Vec::new(),
            search: SearchState::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExplorerState {
    pub visible: bool,
    pub width_cols: f32,
    pub expanded: Vec<String>,
}

impl Default for ExplorerState {
    fn default() -> Self {
        Self {
            visible: true,
            width_cols: DEFAULT_WIDTH_COLS,
            expanded: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct PaneState {
    pub active: usize,
    pub tabs: Vec<TabState>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct CursorState {
    pub anchor: usize,
    pub head: usize,
    pub top_line: usize,
    pub left_col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TabState {
    File {
        path: String,
        #[serde(flatten)]
        cursor: CursorState,
        /// Full text of unsaved edits (the file on disk is left untouched).
        #[serde(default)]
        unsaved: Option<String>,
    },
    Untitled {
        #[serde(flatten)]
        cursor: CursorState,
        #[serde(default)]
        unsaved: Option<String>,
    },
    Help,
    Shortcuts,
}

/// Store `path` relative to `base` when it lies under it (with `/` separators).
pub fn to_stored(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Ok(_) => ".".to_string(),
        Err(_) => path.display().to_string(),
    }
}

pub fn from_stored(s: &str, base: &Path) -> PathBuf {
    let p = Path::new(s);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        normalize(&base.join(p))
    }
}

/// Remove `.` components (no symlink resolution).
fn normalize(p: &Path) -> PathBuf {
    p.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

pub fn read_file(path: &Path) -> Result<WorkspaceFile> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let wf: WorkspaceFile = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid workspace file", path.display()))?;
    if wf.version > FORMAT_VERSION {
        bail!(
            "{} was written by a newer JMX (format {})",
            path.display(),
            wf.version
        );
    }
    Ok(wf)
}

/// Atomic write: temp file next to the target, then rename over it.
pub fn write_file(path: &Path, wf: &WorkspaceFile) -> Result<()> {
    let json = serde_json::to_string_pretty(wf)? + "\n";
    let name = path
        .file_name()
        .context("invalid workspace file name")?
        .to_string_lossy()
        .into_owned();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let tmp = path.with_file_name(format!(".{name}.jmx-tmp"));
    fs::write(&tmp, json).with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("cannot replace {}", path.display()));
    }
    Ok(())
}

fn base_of(file: &Path) -> PathBuf {
    file.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- snapshot / restore

fn cursor_of(doc: &Document) -> CursorState {
    CursorState {
        anchor: doc.selection.anchor,
        head: doc.selection.head,
        top_line: doc.view.top_line,
        left_col: doc.view.left_col,
    }
}

fn apply_cursor(doc: &mut Document, c: CursorState) {
    let len = doc.rope.len_chars();
    doc.selection = Selection::new(c.anchor, c.head).clamp(len);
    doc.view.top_line = c.top_line.min(doc.rope.len_lines().saturating_sub(1));
    doc.view.left_col = c.left_col;
}

impl Editor {
    /// Everything needed to restore the editor later, with paths relative to `base`.
    pub fn snapshot(&self, base: &Path) -> SessionState {
        let panes = self
            .panes
            .iter()
            .map(|pane| PaneState {
                active: pane.active,
                tabs: pane
                    .docs
                    .iter()
                    .map(|d| match (&d.virtual_name, &d.path) {
                        (Some(n), _) if n == help::SHORTCUTS_TITLE => TabState::Shortcuts,
                        (Some(_), _) => TabState::Help,
                        (None, Some(p)) => TabState::File {
                            path: to_stored(p, base),
                            cursor: cursor_of(d),
                            unsaved: d.is_dirty().then(|| d.rope.to_string()),
                        },
                        (None, None) => TabState::Untitled {
                            cursor: cursor_of(d),
                            unsaved: (d.rope.len_chars() > 0).then(|| d.rope.to_string()),
                        },
                    })
                    .collect(),
            })
            .collect();
        let mut expanded: Vec<String> = self
            .explorer
            .expanded
            .iter()
            .map(|p| to_stored(p, base))
            .collect();
        expanded.sort();
        SessionState {
            split: self.split,
            split_ratio: self.split_ratio,
            focus: self.focus,
            font_size: self.config.font_size,
            explorer: ExplorerState {
                visible: self.explorer.visible,
                width_cols: self.explorer.width_cols,
                expanded,
            },
            panes,
            search: SearchState {
                query: self.find.query.clone(),
                include: self.find.include.clone(),
                options: self.search_opts,
            },
        }
    }

    /// The contents of the workspace file for the current workspace.
    pub fn workspace_file(&self, ws: &Workspace, file: &Path) -> WorkspaceFile {
        let base = base_of(file);
        WorkspaceFile {
            version: FORMAT_VERSION,
            name: ws.name.clone(),
            folders: ws.folders.iter().map(|f| to_stored(f, &base)).collect(),
            respect_gitignore: ws.respect_gitignore,
            state: self.snapshot(&base),
        }
    }

    /// Replace all tabs and layout with `state`. Returns the files that could not be restored.
    pub fn restore(&mut self, state: &SessionState, base: &Path) -> Vec<String> {
        let mut missing = Vec::new();
        let tab = self.config.tab_width;
        let mut panes = [Pane::default(), Pane::default()];
        for (pane, ps) in panes.iter_mut().zip(&state.panes) {
            for t in &ps.tabs {
                let doc = match t {
                    TabState::Help => {
                        Some(Document::new_virtual(help::HELP_TITLE, &help::help_text()))
                    }
                    TabState::Shortcuts => Some(Document::new_virtual(
                        help::SHORTCUTS_TITLE,
                        &help::shortcuts_text(),
                    )),
                    TabState::Untitled { cursor, unsaved } => {
                        let mut d = Document::new_empty(None, tab);
                        if let Some(text) = unsaved {
                            d.restore_unsaved(text);
                        }
                        apply_cursor(&mut d, *cursor);
                        Some(d)
                    }
                    TabState::File {
                        path,
                        cursor,
                        unsaved,
                    } => {
                        let abs = from_stored(path, base);
                        if !abs.exists() && unsaved.is_none() {
                            missing.push(abs.display().to_string());
                            None
                        } else {
                            match Document::open(&abs, tab) {
                                Ok(mut d) => {
                                    if let Some(text) = unsaved {
                                        d.restore_unsaved(text);
                                    }
                                    apply_cursor(&mut d, *cursor);
                                    Some(d)
                                }
                                Err(e) => {
                                    log::warn!("workspace restore: {e:#}");
                                    missing.push(abs.display().to_string());
                                    None
                                }
                            }
                        }
                    }
                };
                pane.docs.extend(doc);
            }
            pane.active = ps.active.min(pane.docs.len().saturating_sub(1));
        }
        // Files belong in the editor view (older workspace files may have them on the right).
        let [editor, panel] = &mut panes;
        let (files, pages): (Vec<Document>, Vec<Document>) = std::mem::take(&mut panel.docs)
            .into_iter()
            .partition(|d| d.virtual_name.is_none());
        if !files.is_empty() {
            editor.docs.extend(files);
            panel.active = 0;
        }
        panel.docs = pages;
        panel.active = panel.active.min(panel.docs.len().saturating_sub(1));
        self.panes = panes;
        self.split = state.split;
        self.set_split_ratio(state.split_ratio);
        self.focus = if self.split { state.focus.min(1) } else { 0 };
        self.config.font_size = state
            .font_size
            .clamp(crate::config::MIN_FONT_SIZE, crate::config::MAX_FONT_SIZE);
        self.explorer.visible = state.explorer.visible;
        self.explorer.width_cols = state
            .explorer
            .width_cols
            .clamp(MIN_WIDTH_COLS, MAX_WIDTH_COLS);
        self.explorer.expanded = state
            .explorer
            .expanded
            .iter()
            .map(|s| from_stored(s, base))
            .collect();
        self.find.open = false;
        self.find.clear_results();
        self.find.query = state.search.query.clone();
        self.find.include = state.search.include.clone();
        self.search_opts = state.search.options;
        missing
    }

    /// Load a workspace file and make it the current workspace.
    pub fn load_workspace(&mut self, file: &Path) -> Result<Vec<String>> {
        let wf = read_file(file)?;
        let base = base_of(file);
        let ws = Workspace {
            name: wf.name.clone(),
            folders: wf.folders.iter().map(|f| from_stored(f, &base)).collect(),
            respect_gitignore: wf.respect_gitignore,
            file: Some(file.to_path_buf()),
        };
        let missing = self.restore(&wf.state, &base);
        for f in &ws.folders {
            if !f.is_dir() {
                log::warn!("workspace folder missing: {}", f.display());
            }
        }
        self.workspace = Some(ws);
        self.explorer.focused = false;
        self.explorer.rebuild(self.workspace.as_ref());
        Ok(missing)
    }

    /// Write the current workspace to its file (no-op without a workspace or file).
    pub fn write_workspace(&self) -> Result<()> {
        let Some(ws) = &self.workspace else {
            return Ok(());
        };
        let Some(file) = &ws.file else {
            return Ok(());
        };
        write_file(file, &self.workspace_file(ws, file))?;
        log::info!("saved workspace {} to {}", ws.name, file.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Command;
    use crate::config::Config;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jmx-ws-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        dir
    }

    #[test]
    fn files_in_the_panel_move_to_the_editor_on_restore() {
        let dir = temp("legacy");
        fs::write(dir.join("proj/a.txt"), "a").unwrap();
        let state: SessionState = serde_json::from_str(
            r#"{"split": true, "focus": 1, "panes": [
                {"active": 0, "tabs": [{"kind": "untitled"}]},
                {"active": 1, "tabs": [{"kind": "help"}, {"kind": "file", "path": "proj/a.txt"}]}
            ]}"#,
        )
        .unwrap();
        let mut ed = Editor::new(Config::default(), &[]);
        assert!(ed.restore(&state, &dir).is_empty());
        let titles = |p: usize| {
            ed.panes[p]
                .docs
                .iter()
                .map(|d| d.title())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(0), ["untitled", "a.txt"]);
        assert_eq!(titles(1), [crate::help::HELP_TITLE]);
        assert_eq!(ed.panes[1].active, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stored_paths_are_relative_under_base() {
        let base = Path::new("/home/u/work");
        assert_eq!(
            to_stored(Path::new("/home/u/work/proj/a.rs"), base),
            "proj/a.rs"
        );
        assert_eq!(to_stored(Path::new("/home/u/work"), base), ".");
        assert_eq!(to_stored(Path::new("/etc/hosts"), base), "/etc/hosts");
        assert_eq!(
            from_stored("proj/a.rs", base),
            Path::new("/home/u/work/proj/a.rs")
        );
        assert_eq!(from_stored(".", base), Path::new("/home/u/work"));
        assert_eq!(from_stored("/etc/hosts", base), Path::new("/etc/hosts"));
    }

    #[test]
    fn round_trip_with_unsaved_text_and_cursor() {
        let dir = temp("rt");
        let file = dir.join("proj/src/a.txt");
        fs::write(&file, "on disk\n").unwrap();
        let ws_file = dir.join("demo.jmxws");

        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        let mut ws = Workspace::new("demo");
        ws.folders.push(dir.join("proj"));
        ws.file = Some(ws_file.clone());
        ed.workspace = Some(ws);
        ed.explorer.expanded.insert(dir.join("proj"));
        ed.execute(Command::Move(crate::commands::Motion::End, false));
        ed.insert_text(" edited");
        ed.split_ratio = 0.3;
        ed.execute(Command::FocusRight);
        ed.execute(Command::NewFile);
        ed.insert_text("scratch");
        ed.write_workspace().unwrap();

        let json = fs::read_to_string(&ws_file).unwrap();
        assert!(json.contains("\"proj/src/a.txt\""), "{json}");
        assert!(json.contains("\"folders\": [\n    \"proj\"\n  ]"), "{json}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "on disk\n");

        let mut ed2 = Editor::new(Config::default(), &[]);
        let missing = ed2.load_workspace(&ws_file).unwrap();
        assert!(missing.is_empty());
        let ws2 = ed2.workspace.as_ref().unwrap();
        assert_eq!(ws2.folders, [dir.join("proj")]);
        assert_eq!(ed2.split_ratio, 0.3);
        assert_eq!(ed2.focus, 0);
        assert_eq!(ed2.doc().rope.to_string(), "scratch");
        let left = &ed2.panes[0].docs[0];
        assert_eq!(left.rope.to_string(), "on disk edited\n");
        assert!(left.is_dirty());
        assert_eq!(left.selection.head, 14);
        assert!(ed2.explorer.expanded.contains(&dir.join("proj")));

        // Undo goes back to the file on disk.
        ed2.execute(Command::GotoTab(0));
        ed2.execute(Command::Undo);
        assert_eq!(ed2.doc().rope.to_string(), "on disk\n");
        assert!(!ed2.doc().is_dirty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_files_are_reported() {
        let dir = temp("missing");
        let file = dir.join("proj/gone.txt");
        fs::write(&file, "x").unwrap();
        let ws_file = dir.join("w.jmxws");
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        let mut ws = Workspace::new("w");
        ws.file = Some(ws_file.clone());
        ed.workspace = Some(ws);
        ed.write_workspace().unwrap();
        fs::remove_file(&file).unwrap();

        let mut ed2 = Editor::new(Config::default(), &[]);
        let missing = ed2.load_workspace(&ws_file).unwrap();
        assert_eq!(missing, [file.display().to_string()]);
        assert!(ed2.panes[0].docs.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quit_and_close_in_workspace_mode() {
        let dir = temp("quit");
        let file = dir.join("proj/a.txt");
        fs::write(&file, "a").unwrap();
        let ws_file = dir.join("q.jmxws");
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        let mut ws = Workspace::new("q");
        ws.folders.push(dir.join("proj"));
        ws.file = Some(ws_file.clone());
        ed.workspace = Some(ws);
        ed.insert_text("unsaved ");

        // Quit saves the workspace (with the unsaved edit) instead of asking.
        ed.execute(Command::Quit);
        assert!(ed.should_quit);
        assert!(ed.minibuffer.is_none());
        assert!(fs::read_to_string(&ws_file).unwrap().contains("unsaved a"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "a");

        // Close returns to file mode with the startup layout.
        ed.should_quit = false;
        ed.execute(Command::CloseWorkspace);
        assert!(ed.workspace.is_none());
        assert!(!ed.explorer.visible);
        assert_eq!(ed.panes[0].docs.len(), 1);
        assert!(ed.panes[0].docs[0].path.is_none());
        assert_eq!(ed.panes[1].docs[0].title(), help::HELP_TITLE);

        // A workspace that was never saved asks before closing.
        ed.workspace = Some(Workspace::new("unsaved"));
        ed.execute(Command::CloseWorkspace);
        assert!(ed.workspace.is_some());
        assert!(matches!(
            ed.minibuffer.as_ref().map(|m| &m.kind),
            Some(crate::ui::minibuffer::PromptKind::ConfirmSaveWorkspace(_))
        ));
        ed.insert_text("n");
        assert!(ed.workspace.is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_files() {
        let dir = temp("bad");
        let f = dir.join("bad.jmxws");
        fs::write(&f, "{ not json").unwrap();
        assert!(read_file(&f).is_err());
        fs::write(
            &f,
            r#"{"version":99,"name":"x","folders":[],"respect_gitignore":true}"#,
        )
        .unwrap();
        assert!(read_file(&f).is_err());
        assert_eq!(Workspace::new("a/b").default_file_name(), "a_b.jmxws");
        fs::remove_dir_all(&dir).unwrap();
    }
}
