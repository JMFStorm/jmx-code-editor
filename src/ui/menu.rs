//! Drop-down menus of the menu bar (Workspace, File).

use unicode_width::UnicodeWidthStr;

use crate::commands::Command;
use crate::editor::Editor;
use crate::keymap;
use crate::ui::layout::{Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuId {
    Workspace,
    File,
    Selection,
    Panel,
}

impl MenuId {
    /// Menus in menu bar order.
    pub const ALL: [MenuId; 4] = [
        MenuId::Workspace,
        MenuId::File,
        MenuId::Selection,
        MenuId::Panel,
    ];

    /// The menu a toolbar command opens, if any.
    pub fn for_command(cmd: Command) -> Option<MenuId> {
        match cmd {
            Command::WorkspaceMenu => Some(MenuId::Workspace),
            Command::FileMenu => Some(MenuId::File),
            Command::SelectionMenu => Some(MenuId::Selection),
            Command::PanelMenu => Some(MenuId::Panel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    /// `None` for a separator.
    pub cmd: Option<Command>,
    pub enabled: bool,
    /// `Some` for toggles: shows a check box.
    pub checked: Option<bool>,
}

impl MenuEntry {
    fn item(label: &str, cmd: Command, enabled: bool) -> Self {
        Self {
            label: label.to_string(),
            cmd: Some(cmd),
            enabled,
            checked: None,
        }
    }

    fn toggle(label: &str, cmd: Command, enabled: bool, checked: bool) -> Self {
        Self {
            checked: Some(checked),
            ..Self::item(label, cmd, enabled)
        }
    }

    fn separator() -> Self {
        Self {
            label: String::new(),
            cmd: None,
            enabled: false,
            checked: None,
        }
    }

    pub fn selectable(&self) -> bool {
        self.cmd.is_some() && self.enabled
    }
}

/// Entries of a menu, enabled or not depending on the editor state.
pub fn menu_entries(menu: MenuId, ed: &Editor) -> Vec<MenuEntry> {
    use Command::*;
    match menu {
        MenuId::Workspace => {
            let ws = ed.workspace.as_ref();
            let open = ws.is_some();
            let has_folders = ws.is_some_and(|w| !w.folders.is_empty());
            vec![
                MenuEntry::item("New Workspace…", NewWorkspace, true),
                MenuEntry::item("Open Workspace…", OpenWorkspace, true),
                MenuEntry::item("Save Workspace", SaveWorkspace, open),
                MenuEntry::item("Save Workspace As…", SaveWorkspaceAs, open),
                MenuEntry::separator(),
                MenuEntry::item("Add Folder…", AddFolder, open),
                MenuEntry::item("Remove Folder…", RemoveFolder, has_folders),
                MenuEntry::toggle(
                    "Respect .gitignore",
                    ToggleGitignore,
                    open,
                    ws.is_none_or(|w| w.respect_gitignore),
                ),
                MenuEntry::toggle("Show Explorer", ToggleExplorer, true, ed.explorer.visible),
                MenuEntry::item("Quick Open…", QuickOpen, has_folders),
                MenuEntry::item("Find in Files…", FindInFiles, true),
                MenuEntry::item("Lint Workspace (ESLint)", LintWorkspace, true),
                MenuEntry::item("Run All Tests", RunAllTests, true),
                MenuEntry::item("Refresh Tests", RefreshTests, open),
                MenuEntry::separator(),
                MenuEntry::item("Close Workspace", CloseWorkspace, open),
            ]
        }
        MenuId::File => vec![
            MenuEntry::item("New", NewFile, true),
            MenuEntry::item("Open…", OpenFile, true),
            MenuEntry::item("Open Path…", OpenPath, true),
            MenuEntry::item("Save", Save, ed.has_doc()),
            MenuEntry::item("Save As…", SaveAs, ed.has_doc()),
            MenuEntry::item("Format", FormatDocument, ed.has_doc()),
            MenuEntry::item("Format and Save", FormatAndSave, ed.has_doc()),
            MenuEntry::item(
                "Revert File",
                RevertFile,
                ed.has_doc() && ed.doc().path.is_some(),
            ),
            MenuEntry::item("Close Tab", CloseFile, ed.has_doc()),
            MenuEntry::separator(),
            MenuEntry::item("Quit", Quit, true),
        ],
        MenuId::Selection => {
            let doc = ed.has_doc();
            let multi = doc && !ed.doc().extra.is_empty();
            vec![
                MenuEntry::item("Select All", SelectAll, doc),
                MenuEntry::item("Select Line", SelectLine, doc),
                MenuEntry::separator(),
                MenuEntry::item("Add Next Occurrence", AddNextOccurrence, doc),
                MenuEntry::item("Remove Last Occurrence", RemoveLastOccurrence, multi),
                MenuEntry::item("Select All Occurrences", SelectAllOccurrences, doc),
                MenuEntry::item("Single Cursor", Cancel, multi),
                MenuEntry::separator(),
                MenuEntry::item("Go to Definition", GoToDefinition, doc),
                MenuEntry::item("Go Back", GoBack, !ed.symbols.back.is_empty()),
                MenuEntry::item("Find All References", FindAllReferences, doc),
                MenuEntry::item("Parameter Hints", SignatureHelp, doc),
                MenuEntry::item("Open Docs in Browser…", OpenDocsInBrowser, doc),
            ]
        }
        MenuId::Panel => {
            // The page shown in the panel gets the check mark.
            let shown = ed
                .split
                .then(|| ed.panes[1].doc().and_then(|d| d.virtual_name.clone()))
                .flatten();
            // Counts as on the tabs.
            let (e, w, i) = ed.problems.counts3();
            let mut parts = Vec::new();
            for (n, g) in [(e, '✖'), (w, '⚠'), (i, 'ℹ')] {
                if n > 0 {
                    parts.push(format!("{g}{n}"));
                }
            }
            let problems_label = if parts.is_empty() {
                "Problems".into()
            } else {
                format!("Problems ({})", parts.join(" "))
            };
            let failed = ed.tests.tree.counts().1;
            let tests_label = if failed > 0 {
                format!("Tests ({failed} failed)")
            } else {
                "Tests".into()
            };
            let page = |label: &str, title: &str, cmd: Command| {
                MenuEntry::toggle(label, cmd, true, shown.as_deref() == Some(title))
            };
            vec![
                page("Help", crate::help::HELP_TITLE, Help),
                page("Shortcuts", crate::help::SHORTCUTS_TITLE, ShowShortcuts),
                page(&problems_label, crate::editor::PROBLEMS_TITLE, ShowProblems),
                page(&tests_label, crate::editor::TESTS_TITLE, ShowTests),
                MenuEntry::separator(),
                MenuEntry::toggle("Show Panel", ToggleSplit, true, ed.split),
                MenuEntry::item(
                    "Close Panel Tab",
                    ClosePanelTab,
                    !ed.panes[1].docs.is_empty(),
                ),
            ]
        }
    }
}

/// Next selectable entry from `from` in direction `dir` (wrapping), skipping separators and
/// disabled items.
pub fn next_item(entries: &[MenuEntry], from: Option<usize>, dir: isize) -> Option<usize> {
    let n = entries.len() as isize;
    let mut i = from.map_or(if dir > 0 { -1 } else { n }, |i| i as isize);
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if entries[i as usize].selectable() {
            return Some(i as usize);
        }
    }
    None
}

pub struct MenuRow {
    pub rect: Rect,
    pub entry: MenuEntry,
    /// Shortcut text shown right-aligned.
    pub keys: String,
}

pub struct MenuLayout {
    pub panel: Rect,
    pub rows: Vec<MenuRow>,
}

impl MenuLayout {
    /// Index of the selectable row under a point.
    pub fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.entry.selectable() && r.rect.contains(x, y))
    }
}

/// Geometry of `menu`, dropped down under its toolbar button.
pub fn menu_layout(layout: &Layout, ed: &Editor, menu: MenuId, anchor: Rect) -> MenuLayout {
    let entries = menu_entries(menu, ed);
    let keys: Vec<String> = entries
        .iter()
        .map(|e| e.cmd.and_then(keymap::shortcut_for).unwrap_or_default())
        .collect();
    let cols = entries
        .iter()
        .zip(&keys)
        .map(|(e, k)| e.label.width() + k.width())
        .max()
        .unwrap_or(10)
        + 10;
    let w = (cols as f32 * layout.cell_w).round();
    let row_h = (layout.line_h * 1.4).round();
    let sep_h = (layout.line_h * 0.6).round();
    let pad = (layout.line_h * 0.25).round();
    let mut y = anchor.bottom() + pad;
    let rows = entries
        .into_iter()
        .zip(keys)
        .map(|(entry, keys)| {
            let h = if entry.cmd.is_some() { row_h } else { sep_h };
            let r = MenuRow {
                rect: Rect::new(anchor.x, y, w, h),
                entry,
                keys,
            };
            y += h;
            r
        })
        .collect();
    MenuLayout {
        panel: Rect::new(anchor.x, anchor.bottom(), w, y + pad - anchor.bottom()),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::workspace::Workspace;

    #[test]
    fn workspace_items_depend_on_mode() {
        let mut ed = Editor::new(Config::default(), &[]);
        let enabled = |ed: &Editor| -> Vec<bool> {
            menu_entries(MenuId::Workspace, ed)
                .iter()
                .filter(|e| e.cmd.is_some())
                .map(|e| e.enabled)
                .collect()
        };
        // File mode: only New, Open and Show Explorer.
        assert_eq!(
            enabled(&ed),
            [
                true, true, false, false, false, false, false, true, false, true, true, true,
                false, false
            ]
        );
        let mut ws = Workspace::new("w");
        ws.folders.push("/tmp".into());
        ed.workspace = Some(ws);
        assert!(enabled(&ed).iter().all(|e| *e));
    }

    #[test]
    fn navigation_skips_separators_and_disabled() {
        let ed = Editor::new(Config::default(), &[]);
        let entries = menu_entries(MenuId::Workspace, &ed);
        assert_eq!(next_item(&entries, None, 1), Some(0));
        assert_eq!(next_item(&entries, Some(1), 1), Some(8)); // jumps to Show Explorer
        assert_eq!(next_item(&entries, Some(8), 1), Some(10)); // Find in Files
        assert_eq!(next_item(&entries, Some(12), 1), Some(0)); // wraps past disabled items
        assert_eq!(next_item(&entries, Some(0), -1), Some(12));
        let file = menu_entries(MenuId::File, &ed);
        assert_eq!(next_item(&file, Some(6), 1), Some(8)); // skips Revert: untitled tab
        assert_eq!(next_item(&file, Some(8), 1), Some(10)); // skips the separator
    }

    #[test]
    fn panel_menu_checks_the_shown_page() {
        let mut ed = Editor::new(Config::default(), &[]);
        let checked = |ed: &Editor| -> Vec<String> {
            menu_entries(MenuId::Panel, ed)
                .into_iter()
                .filter(|e| e.checked == Some(true))
                .map(|e| e.label)
                .collect()
        };
        assert_eq!(checked(&ed), ["Help", "Show Panel"]);
        ed.execute(Command::ShowTests);
        assert_eq!(checked(&ed), ["Tests", "Show Panel"]);
        ed.execute(Command::ShowProblems);
        assert_eq!(ed.panes[1].doc().unwrap().title(), "Problems");
        ed.execute(Command::ClosePanelTab);
        assert_eq!(ed.panes[1].doc().unwrap().title(), "Tests");
        ed.execute(Command::ToggleSplit);
        assert_eq!(checked(&ed), Vec::<String>::new());
    }
}
