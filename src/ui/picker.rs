//! In-editor file picker overlay: browse folders and open files with mouse or keyboard.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// What the picker wants the editor to do after a key/click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickResult {
    None,
    Open(PathBuf),
}

#[derive(Debug)]
pub struct FilePicker {
    pub dir: PathBuf,
    /// All entries of `dir` (sorted, `..` first), before filtering.
    all: Vec<Entry>,
    pub filter: String,
    /// Indices into `all` that match the filter.
    visible: Vec<usize>,
    pub selected: usize,
    /// First visible row of the list.
    pub scroll: usize,
    pub error: Option<String>,
}

impl FilePicker {
    pub fn new(dir: PathBuf) -> Self {
        let mut p = Self {
            dir: PathBuf::new(),
            all: Vec::new(),
            filter: String::new(),
            visible: Vec::new(),
            selected: 0,
            scroll: 0,
            error: None,
        };
        p.enter(dir);
        p
    }

    /// Switch to `dir` and list it.
    pub fn enter(&mut self, dir: PathBuf) {
        let dir = fs::canonicalize(&dir).unwrap_or(dir);
        self.all.clear();
        self.error = None;
        if dir.parent().is_some() {
            self.all.push(Entry {
                name: "..".into(),
                is_dir: true,
            });
        }
        match fs::read_dir(&dir) {
            Ok(rd) => {
                let mut entries: Vec<Entry> = rd
                    .flatten()
                    .map(|e| Entry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        is_dir: e.path().is_dir(),
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

    fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        let show_hidden = f.starts_with('.');
        self.visible = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if e.name == ".." {
                    return f.is_empty();
                }
                (show_hidden || !e.name.starts_with('.')) && e.name.to_lowercase().contains(&f)
            })
            .map(|(i, _)| i)
            .collect();
        // Prefer the first real match over "..".
        self.selected =
            usize::from(f.is_empty() && self.visible.len() > 1 && self.all[0].name == "..");
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
        } else {
            self.go_parent();
        }
    }

    pub fn go_parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let from = self
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            self.enter(parent);
            // Keep the folder we came from selected.
            let pos = from.and_then(|from| self.entries().position(|e| e.name == from));
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

    /// Enter: open the selected file / enter the selected folder. With a filter that matches
    /// nothing, open (or create) `dir/filter`.
    pub fn activate(&mut self) -> PickResult {
        match self.selected_entry().cloned() {
            Some(e) if e.name == ".." => {
                self.go_parent();
                PickResult::None
            }
            Some(e) if e.is_dir => {
                self.enter(self.dir.join(&e.name));
                PickResult::None
            }
            Some(e) => PickResult::Open(self.dir.join(&e.name)),
            None if !self.filter.is_empty() => PickResult::Open(self.dir.join(&self.filter)),
            None => PickResult::None,
        }
    }
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
        for f in ["b.txt", "A.md", ".hidden", "src/main.js"] {
            fs::write(dir.join(f), "").unwrap();
        }
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn listing_filter_and_navigation() {
        let dir = fixture("listing");
        let mut p = FilePicker::new(dir.clone());
        let names: Vec<_> = p.entries().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["..", "Assets", "src", "A.md", "b.txt"]);
        assert_eq!(p.selected_entry().unwrap().name, "Assets");

        p.type_text("S");
        let names: Vec<_> = p.entries().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["Assets", "src"]);
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
