//! Explorer sidebar: a file tree of the workspace folders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::workspace::Workspace;
use crate::workspace::files::list_dir;

pub const DEFAULT_WIDTH_COLS: f32 = 32.0;
pub const MIN_WIDTH_COLS: f32 = 15.0;
pub const MAX_WIDTH_COLS: f32 = 60.0;

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub is_root: bool,
    pub expanded: bool,
}

/// What the editor should do after an Explorer action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerAction {
    None,
    Open(PathBuf),
}

#[derive(Debug)]
pub struct Explorer {
    pub visible: bool,
    /// Keyboard focus is in the Explorer (instead of a view).
    pub focused: bool,
    pub width_cols: f32,
    pub expanded: HashSet<PathBuf>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub scroll: usize,
    /// Number of list rows on screen, updated by the layout.
    pub visible_rows: usize,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            visible: false,
            focused: false,
            width_cols: DEFAULT_WIDTH_COLS,
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            visible_rows: 20,
        }
    }
}

impl Explorer {
    /// Re-read the tree, keeping the selected path selected if it still exists.
    pub fn rebuild(&mut self, ws: Option<&Workspace>) {
        let keep = self.selected_row().map(|r| r.path.clone());
        self.rows.clear();
        if let Some(ws) = ws {
            for root in &ws.folders {
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string());
                let expanded = self.expanded.contains(root);
                self.rows.push(Row {
                    path: root.clone(),
                    name,
                    depth: 0,
                    is_dir: true,
                    is_root: true,
                    expanded,
                });
                if expanded {
                    self.push_children(root, 1, ws.respect_gitignore);
                }
            }
        }
        self.selected = keep
            .and_then(|p| self.rows.iter().position(|r| r.path == p))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
        self.ensure_visible();
    }

    fn push_children(&mut self, dir: &Path, depth: usize, gitignore: bool) {
        for e in list_dir(dir, gitignore) {
            let expanded = e.is_dir && self.expanded.contains(&e.path);
            let path = e.path.clone();
            self.rows.push(Row {
                path: e.path,
                name: e.name,
                depth,
                is_dir: e.is_dir,
                is_root: false,
                expanded,
            });
            if expanded {
                self.push_children(&path, depth + 1, gitignore);
            }
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.ensure_visible();
    }

    pub fn select(&mut self, index: usize) {
        if index < self.rows.len() {
            self.selected = index;
            self.ensure_visible();
        }
    }

    pub fn ensure_visible(&mut self) {
        let rows = self.visible_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(self.visible_rows.max(1));
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    /// Expand or collapse the folder at `index`.
    pub fn set_expanded(&mut self, index: usize, expand: bool, ws: Option<&Workspace>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if !row.is_dir || row.expanded == expand {
            return;
        }
        let path = row.path.clone();
        if expand {
            self.expanded.insert(path);
        } else {
            self.expanded.remove(&path);
        }
        self.selected = index;
        self.rebuild(ws);
    }

    /// Right arrow: expand a folder, or step into an already expanded one.
    pub fn expand_or_enter(&mut self, ws: Option<&Workspace>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_dir && !row.expanded {
            self.set_expanded(self.selected, true, ws);
        } else if row.is_dir && self.selected + 1 < self.rows.len() {
            self.move_selection(1);
        }
    }

    /// Left arrow: collapse a folder, or jump to the parent folder.
    pub fn collapse_or_parent(&mut self, ws: Option<&Workspace>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_dir && row.expanded {
            self.set_expanded(self.selected, false, ws);
            return;
        }
        let depth = row.depth;
        if depth == 0 {
            return;
        }
        if let Some(i) = (0..self.selected)
            .rev()
            .find(|&i| self.rows[i].depth < depth)
        {
            self.select(i);
        }
    }

    /// Enter: open a file, or toggle a folder.
    pub fn activate(&mut self, ws: Option<&Workspace>) -> ExplorerAction {
        let Some(row) = self.selected_row() else {
            return ExplorerAction::None;
        };
        if row.is_dir {
            let expand = !row.expanded;
            self.set_expanded(self.selected, expand, ws);
            ExplorerAction::None
        } else {
            ExplorerAction::Open(row.path.clone())
        }
    }

    /// Jump to the next row whose name starts with `c` (wrapping).
    pub fn jump_to_letter(&mut self, c: char) {
        let c = c.to_lowercase().next().unwrap_or(c);
        let n = self.rows.len();
        for k in 1..=n {
            let i = (self.selected + k) % n;
            if self.rows[i].name.to_lowercase().starts_with(c) {
                self.select(i);
                return;
            }
        }
    }

    /// Forget expansion state below a removed folder.
    pub fn forget(&mut self, root: &Path) {
        self.expanded.retain(|p| !p.starts_with(root));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn expand_collapse_and_navigate() {
        let dir = std::env::temp_dir().join(format!("jmx-expl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/utils")).unwrap();
        fs::write(dir.join("src/utils/csv.js"), "").unwrap();
        fs::write(dir.join("src/App.vue"), "").unwrap();
        fs::write(dir.join("README.md"), "").unwrap();
        let mut ws = Workspace::new("t");
        ws.folders.push(dir.clone());

        let mut ex = Explorer::default();
        ex.expanded.insert(dir.clone());
        ex.rebuild(Some(&ws));
        let names = |ex: &Explorer| ex.rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
        let root = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(names(&ex), [root.as_str(), "src", "README.md"]);

        ex.select(1);
        ex.expand_or_enter(Some(&ws)); // expand src
        assert_eq!(
            names(&ex),
            [root.as_str(), "src", "utils", "App.vue", "README.md"]
        );
        ex.expand_or_enter(Some(&ws)); // step into src
        assert_eq!(ex.selected_row().unwrap().name, "utils");
        ex.collapse_or_parent(Some(&ws)); // utils is collapsed: jump to parent
        assert_eq!(ex.selected_row().unwrap().name, "src");
        ex.collapse_or_parent(Some(&ws)); // collapse src
        assert_eq!(names(&ex), [root.as_str(), "src", "README.md"]);

        ex.jump_to_letter('r');
        assert_eq!(ex.selected_row().unwrap().name, "README.md");
        assert_eq!(
            ex.activate(Some(&ws)),
            ExplorerAction::Open(dir.join("README.md"))
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
