//! In-editor picker overlay: browse folders, open files or workspaces, choose folders, and
//! fuzzy-search workspace files, with mouse or keyboard.

use std::fs;
use std::path::{Path, PathBuf};

use crate::workspace::files::{IndexedFile, fuzzy_score};
use crate::workspace::is_workspace_file;

/// What the picker is for. Folder-browsing kinds list a directory; the others a fixed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// Open a file (browse folders).
    OpenFile,
    /// Open a `.jmxws` workspace file (browse folders).
    OpenWorkspace,
    /// Pick a folder to add to the workspace (browse folders).
    ChooseFolder,
    /// Pick one of the workspace folders to remove (fixed list).
    RemoveFolder,
    /// Fuzzy search over the workspace file index (fixed list).
    QuickOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Normal,
    /// The `..` row.
    Parent,
    /// "Add this folder" row of the folder chooser.
    ChooseHere,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Text shown in the list.
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: EntryKind,
}

/// What the picker wants the editor to do after a key/click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickResult {
    None,
    Open(PathBuf),
    /// Open a file and go to a 1-based line (and column).
    OpenAt(PathBuf, usize, Option<usize>),
    OpenWorkspace(PathBuf),
    AddFolder(PathBuf),
    RemoveFolder(PathBuf),
}

#[derive(Debug)]
pub struct FilePicker {
    pub kind: PickerKind,
    /// Folder being browsed (folder kinds only).
    pub dir: PathBuf,
    /// All entries before filtering.
    all: Vec<Entry>,
    pub filter: String,
    /// Indices into `all` that match the filter, in display order.
    visible: Vec<usize>,
    pub selected: usize,
    /// First visible row of the list.
    pub scroll: usize,
    pub error: Option<String>,
    /// Extra note shown under the list (e.g. the Quick Open index was truncated).
    pub note: Option<String>,
    /// Quick Open: the fuzzy query the list was last filtered with.
    filtered_for: Option<String>,
}

impl FilePicker {
    /// File picker starting in `dir`.
    pub fn new(dir: PathBuf) -> Self {
        Self::browse(PickerKind::OpenFile, dir)
    }

    /// A folder-browsing picker of the given kind.
    pub fn browse(kind: PickerKind, dir: PathBuf) -> Self {
        let mut p = Self::empty(kind);
        p.enter(dir);
        p
    }

    /// Pick one of the workspace folders.
    pub fn remove_folder(folders: &[PathBuf]) -> Self {
        let mut p = Self::empty(PickerKind::RemoveFolder);
        p.all = folders
            .iter()
            .map(|f| Entry {
                name: f.display().to_string(),
                path: f.clone(),
                is_dir: true,
                kind: EntryKind::Normal,
            })
            .collect();
        p.refilter();
        p
    }

    /// Fuzzy file search over an index.
    pub fn quick_open(files: Vec<IndexedFile>, truncated: bool) -> Self {
        let mut p = Self::empty(PickerKind::QuickOpen);
        p.all = files
            .into_iter()
            .map(|f| Entry {
                name: f.rel,
                path: f.abs,
                is_dir: false,
                kind: EntryKind::Normal,
            })
            .collect();
        if truncated {
            p.note = Some(format!("Only the first {} files are indexed", p.all.len()));
        }
        p.refilter();
        p
    }

    fn empty(kind: PickerKind) -> Self {
        Self {
            kind,
            dir: PathBuf::new(),
            all: Vec::new(),
            filter: String::new(),
            visible: Vec::new(),
            selected: 0,
            scroll: 0,
            error: None,
            note: None,
            filtered_for: None,
        }
    }

    fn browses(&self) -> bool {
        matches!(
            self.kind,
            PickerKind::OpenFile | PickerKind::OpenWorkspace | PickerKind::ChooseFolder
        )
    }

    /// Title line of the overlay.
    pub fn title(&self) -> String {
        match self.kind {
            PickerKind::OpenFile => format!("Open  {}", self.dir.display()),
            PickerKind::OpenWorkspace => format!("Open Workspace  {}", self.dir.display()),
            PickerKind::ChooseFolder => format!("Add Folder  {}", self.dir.display()),
            PickerKind::RemoveFolder => "Remove Folder from Workspace".to_string(),
            PickerKind::QuickOpen => {
                let mut t = format!("Quick Open  ({} files)", self.all.len());
                if let Some((line, col)) = self.line_target() {
                    t.push_str(&format!("  → line {line}"));
                    if let Some(col) = col {
                        t.push_str(&format!(", column {col}"));
                    }
                }
                t
            }
        }
    }

    /// Hint shown in the empty filter line.
    pub fn hint(&self) -> &'static str {
        match self.kind {
            PickerKind::OpenFile => "type to filter, Enter opens, Backspace goes up",
            PickerKind::OpenWorkspace => "showing folders and .jmxws files, Enter opens",
            PickerKind::ChooseFolder => "Enter on the first row adds the folder, Backspace goes up",
            PickerKind::RemoveFolder => "Enter removes the selected folder",
            PickerKind::QuickOpen => "type part of a file name, :N goes to a line, e.g. appvue:42",
        }
    }

    /// Switch to `dir` and list it (folder kinds).
    pub fn enter(&mut self, dir: PathBuf) {
        let dir = fs::canonicalize(&dir).unwrap_or(dir);
        self.all.clear();
        self.error = None;
        if self.kind == PickerKind::ChooseFolder {
            self.all.push(Entry {
                name: format!("✓ Add this folder: {}", dir.display()),
                path: dir.clone(),
                is_dir: true,
                kind: EntryKind::ChooseHere,
            });
        }
        if let Some(parent) = dir.parent() {
            self.all.push(Entry {
                name: "..".into(),
                path: parent.to_path_buf(),
                is_dir: true,
                kind: EntryKind::Parent,
            });
        }
        match fs::read_dir(&dir) {
            Ok(rd) => {
                let kind = self.kind;
                let mut entries: Vec<Entry> = rd
                    .flatten()
                    .map(|e| Entry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        is_dir: e.path().is_dir(),
                        path: e.path(),
                        kind: EntryKind::Normal,
                    })
                    .filter(|e| match kind {
                        PickerKind::ChooseFolder => e.is_dir,
                        PickerKind::OpenWorkspace => e.is_dir || is_workspace_file(&e.path),
                        _ => true,
                    })
                    .collect();
                entries.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                self.all.extend(entries);
            }
            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
        }
        self.dir = dir;
        self.filter.clear();
        self.refilter();
    }

    /// Quick Open: the `:line[:col]` typed after the file name, if any.
    pub fn line_target(&self) -> Option<(usize, Option<usize>)> {
        if self.kind != PickerKind::QuickOpen {
            return None;
        }
        split_line_suffix(&self.filter).1
    }

    fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        if self.kind == PickerKind::QuickOpen {
            // Only the part before `:line` is matched; typing the line keeps the selection.
            let f = split_line_suffix(&f).0.to_string();
            if self.filtered_for.as_ref() == Some(&f) {
                return;
            }
            self.filtered_for = Some(f.clone());
            let mut scored: Vec<(i32, usize)> = self
                .all
                .iter()
                .enumerate()
                .filter_map(|(i, e)| fuzzy_score(&f, &e.name).map(|s| (s, i)))
                .collect();
            if !f.is_empty() {
                scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            }
            self.visible = scored.into_iter().map(|(_, i)| i).collect();
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        let show_hidden = f.starts_with('.') || !self.browses();
        self.visible = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, e)| match e.kind {
                EntryKind::ChooseHere => true,
                EntryKind::Parent => f.is_empty(),
                EntryKind::Normal => {
                    (show_hidden || !e.name.starts_with('.')) && e.name.to_lowercase().contains(&f)
                }
            })
            .map(|(i, _)| i)
            .collect();
        // Select the first real entry rather than "..".
        self.selected = self
            .visible
            .iter()
            .position(|&i| self.all[i].kind != EntryKind::Parent)
            .unwrap_or(0);
        self.scroll = 0;
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.visible.iter().map(|&i| &self.all[i])
    }

    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.visible.get(self.selected).map(|&i| &self.all[i])
    }

    pub fn type_text(&mut self, text: &str) {
        self.filter.extend(text.chars().filter(|c| !c.is_control()));
        self.refilter();
    }

    /// Backspace: edit the filter, or go to the parent folder when it is empty.
    pub fn backspace(&mut self) {
        if self.filter.pop().is_some() {
            self.refilter();
        } else if self.browses() {
            self.go_parent();
        }
    }

    pub fn go_parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let from = self.dir.clone();
            self.enter(parent);
            // Keep the folder we came from selected.
            let pos = self.entries().position(|e| e.path == from);
            if let Some(i) = pos {
                self.selected = i;
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
    }

    /// Keep the selection within `rows` visible rows.
    pub fn ensure_visible(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.visible.len().saturating_sub(rows.max(1));
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    /// Enter on the selected row.
    pub fn activate(&mut self) -> PickResult {
        let Some(e) = self.selected_entry().cloned() else {
            // Nothing matches: the file picker creates `dir/filter`.
            return if self.kind == PickerKind::OpenFile && !self.filter.is_empty() {
                PickResult::Open(self.dir.join(&self.filter))
            } else {
                PickResult::None
            };
        };
        match (self.kind, e.kind) {
            (_, EntryKind::Parent) => {
                self.go_parent();
                PickResult::None
            }
            (_, EntryKind::ChooseHere) => PickResult::AddFolder(e.path),
            (PickerKind::RemoveFolder, _) => PickResult::RemoveFolder(e.path),
            (PickerKind::QuickOpen, _) => match self.line_target() {
                Some((line, col)) => PickResult::OpenAt(e.path, line, col),
                None => PickResult::Open(e.path),
            },
            (_, _) if e.is_dir => {
                self.enter(e.path);
                PickResult::None
            }
            (PickerKind::OpenWorkspace, _) => PickResult::OpenWorkspace(e.path),
            _ => PickResult::Open(e.path),
        }
    }
}

/// Split a trailing `:line` or `:line:col` off a Quick Open filter. A bare or half-typed
/// suffix (`csv:`, `csv:4:`) is still split off so the file list does not change while typing.
pub fn split_line_suffix(filter: &str) -> (&str, Option<(usize, Option<usize>)>) {
    let num = |s: &str| s.parse::<usize>().ok().filter(|&n| n > 0);
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if let Some((head, last)) = filter.rsplit_once(':')
        && digits(last)
    {
        // `name:line:col`
        if let Some((name, line)) = head.rsplit_once(':')
            && digits(line)
            && !line.is_empty()
        {
            return (name, num(line).map(|l| (l, num(last))));
        }
        return (head, num(last).map(|l| (l, None)));
    }
    (filter, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test directory: tests run in parallel and each deletes its own fixture.
    fn fixture(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jmx-picker-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("Assets")).unwrap();
        for f in ["b.txt", "A.md", ".hidden", "src/main.js", "proj.jmxws"] {
            fs::write(dir.join(f), "").unwrap();
        }
        fs::canonicalize(dir).unwrap()
    }

    fn names(p: &FilePicker) -> Vec<String> {
        p.entries().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn listing_filter_and_navigation() {
        let dir = fixture("listing");
        let mut p = FilePicker::new(dir.clone());
        assert_eq!(
            names(&p),
            ["..", "Assets", "src", "A.md", "b.txt", "proj.jmxws"]
        );
        assert_eq!(p.selected_entry().unwrap().name, "Assets");

        p.type_text("S");
        assert_eq!(names(&p), ["Assets", "src", "proj.jmxws"]);
        p.move_selection(1);
        assert_eq!(p.activate(), PickResult::None); // entered src/
        assert_eq!(p.dir, dir.join("src"));
        p.move_selection(5);
        assert_eq!(p.activate(), PickResult::Open(dir.join("src/main.js")));

        p.backspace(); // empty filter -> parent, src stays selected
        assert_eq!(p.dir, dir);
        assert_eq!(p.selected_entry().unwrap().name, "src");

        p.type_text(".h");
        assert_eq!(p.selected_entry().unwrap().name, ".hidden");
        p.type_text("zz");
        assert_eq!(p.len(), 0);
        assert_eq!(p.activate(), PickResult::Open(dir.join(".hzz")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn workspace_and_folder_kinds() {
        let dir = fixture("kinds");
        let p = FilePicker::browse(PickerKind::OpenWorkspace, dir.clone());
        assert_eq!(names(&p), ["..", "Assets", "src", "proj.jmxws"]);

        let mut p = FilePicker::browse(PickerKind::ChooseFolder, dir.clone());
        assert_eq!(p.entries().count(), 4); // add-this, .., Assets, src
        assert_eq!(p.activate(), PickResult::AddFolder(dir.clone()));
        p.move_selection(3); // src
        assert_eq!(p.activate(), PickResult::None);
        assert_eq!(p.activate(), PickResult::AddFolder(dir.join("src")));

        let mut p = FilePicker::remove_folder(&[dir.join("src"), dir.join("Assets")]);
        p.type_text("assets");
        assert_eq!(p.activate(), PickResult::RemoveFolder(dir.join("Assets")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quick_open_ranks_fuzzy_matches() {
        let files = ["src/App.vue", "src/components/AppHeader.vue", "README.md"]
            .iter()
            .map(|r| IndexedFile {
                abs: PathBuf::from("/w").join(r),
                rel: r.to_string(),
            })
            .collect();
        let mut p = FilePicker::quick_open(files, false);
        assert_eq!(p.len(), 3);
        p.type_text("appvue");
        assert_eq!(names(&p), ["src/App.vue", "src/components/AppHeader.vue"]);
        assert_eq!(
            p.activate(),
            PickResult::Open(PathBuf::from("/w/src/App.vue"))
        );
    }

    #[test]
    fn line_suffix() {
        assert_eq!(split_line_suffix("csv"), ("csv", None));
        assert_eq!(split_line_suffix("csv:"), ("csv", None));
        assert_eq!(split_line_suffix("csv:42"), ("csv", Some((42, None))));
        assert_eq!(split_line_suffix("csv:42:"), ("csv", Some((42, None))));
        assert_eq!(split_line_suffix("csv:42:7"), ("csv", Some((42, Some(7)))));
        assert_eq!(split_line_suffix("c:/x"), ("c:/x", None));
        assert_eq!(split_line_suffix(":3"), ("", Some((3, None))));
    }

    #[test]
    fn quick_open_line_keeps_selection() {
        let files = ["src/App.vue", "src/components/AppHeader.vue"]
            .iter()
            .map(|r| IndexedFile {
                abs: PathBuf::from("/w").join(r),
                rel: r.to_string(),
            })
            .collect();
        let mut p = FilePicker::quick_open(files, false);
        p.type_text("app");
        p.move_selection(1);
        p.type_text(":12");
        assert_eq!(p.selected, 1);
        assert!(p.title().ends_with("→ line 12"));
        p.type_text(":3");
        assert_eq!(
            p.activate(),
            PickResult::OpenAt(
                PathBuf::from("/w/src/components/AppHeader.vue"),
                12,
                Some(3)
            )
        );
        for _ in 0..5 {
            p.backspace();
        }
        assert_eq!(p.selected, 1); // still filtered by "app"
        assert_eq!(
            p.activate(),
            PickResult::Open(PathBuf::from("/w/src/components/AppHeader.vue"))
        );
    }

    #[test]
    fn scrolling() {
        let dir = fixture("scrolling");
        let mut p = FilePicker::new(dir.clone());
        p.select_last();
        p.ensure_visible(2);
        assert_eq!(p.scroll, p.len() - 2);
        p.select_first();
        p.ensure_visible(2);
        assert_eq!(p.scroll, 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}
