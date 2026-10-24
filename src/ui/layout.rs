//! Screen regions in physical pixels. Used by both rendering and mouse hit-testing.

use unicode_width::UnicodeWidthStr;

use crate::commands::Command;
use crate::editor::Editor;
use crate::keymap;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub cell_w: f32,
    pub line_h: f32,
    pub scale: f32,
    pub toolbar: Rect,
    /// Visible views, left to right.
    pub panes: Vec<PaneLayout>,
    /// Grab area of the line between the views (split mode only).
    pub divider: Option<Rect>,
    /// The visible 1px line itself.
    pub divider_line: Option<Rect>,
    /// Explorer sidebar, when visible.
    pub sidebar: Option<SidebarLayout>,
    pub status: Rect,
    /// Problem counts at the right end of the status bar (click: Problems page).
    pub problems_chip: Option<Rect>,
    pub minibuffer: Option<Rect>,
    pub picker: Option<PickerLayout>,
    pub find: Option<FindLayout>,
}

/// Geometry of one view: its tab bar, line-number gutter and text area.
#[derive(Debug, Clone)]
pub struct PaneLayout {
    /// Index into `Editor::panes`.
    pub pane: usize,
    pub area: Rect,
    pub tab_bar: Rect,
    pub gutter: Rect,
    pub text: Rect,
    /// Digits reserved for line numbers.
    pub gutter_digits: usize,
    pub rows: usize,
    pub cols: usize,
    pub cell_w: f32,
    pub line_h: f32,
}

/// Each view keeps at least this many text columns when the divider is dragged.
const MIN_PANE_COLS: f32 = 20.0;
/// Extra grab width on each side of the divider line, in pixels (before scaling).
const DIVIDER_GRAB: f32 = 4.0;

impl Layout {
    pub fn compute(
        ed: &Editor,
        width: f32,
        height: f32,
        cell_w: f32,
        line_h: f32,
        scale: f32,
    ) -> Self {
        let bar_h = (line_h * 1.3).round();
        let toolbar = Rect::new(0.0, 0.0, width, bar_h);
        let minibuffer = ed
            .minibuffer
            .as_ref()
            .map(|_| Rect::new(0.0, height - bar_h, width, bar_h));
        let status_y = minibuffer.map_or(height, |r| r.y) - bar_h;
        let status = Rect::new(0.0, status_y, width, bar_h);
        let problems_chip = problems_chip_cols(ed).map(|cols| {
            let w = (cols as f32 * cell_w).round();
            Rect::new(width - w, status_y, w, bar_h)
        });
        let body_y = toolbar.bottom();
        let body_h = (status.y - body_y).max(0.0);

        // Explorer sidebar on the left; the views share the rest.
        let line_w = scale.max(1.0).round();
        let sidebar = ed.explorer.visible.then(|| {
            let w = (ed.explorer.width_cols * cell_w)
                .min(width * 0.6)
                .max(cell_w * 10.0)
                .round();
            let area = Rect::new(0.0, body_y, w, body_h);
            SidebarLayout::compute(ed, area, bar_h, line_h, line_w, scale)
        });
        let body_x = sidebar.as_ref().map_or(0.0, |s| s.area.right() + line_w);
        let body_w = (width - body_x).max(0.0);

        let mut panes = Vec::new();
        let (mut divider, mut divider_line) = (None, None);
        if ed.split {
            let min_w = MIN_PANE_COLS * cell_w;
            let mut left_w = (body_w * ed.split_ratio).round();
            if body_w - line_w >= 2.0 * min_w {
                left_w = left_w.clamp(min_w, body_w - line_w - min_w);
            }
            let left = Rect::new(body_x, body_y, left_w, body_h);
            let right_x = body_x + left_w + line_w;
            let right = Rect::new(right_x, body_y, width - right_x, body_h);
            panes.push(PaneLayout::compute(ed, 0, left, bar_h, cell_w, line_h));
            panes.push(PaneLayout::compute(ed, 1, right, bar_h, cell_w, line_h));
            let grab = (DIVIDER_GRAB * scale).round();
            let x = body_x + left_w;
            divider_line = Some(Rect::new(x, body_y, line_w, body_h));
            divider = Some(Rect::new(x - grab, body_y, line_w + 2.0 * grab, body_h));
        } else {
            let area = Rect::new(body_x, body_y, body_w, body_h);
            // The panel is hidden: only the editor view shows.
            panes.push(PaneLayout::compute(ed, 0, area, bar_h, cell_w, line_h));
        }

        let picker = ed.picker.as_ref().map(|_| {
            PickerLayout::compute(Rect::new(0.0, body_y, width, body_h), bar_h, cell_w, line_h)
        });
        let find = ed.find.open.then(|| {
            FindLayout::compute(Rect::new(0.0, body_y, width, body_h), bar_h, cell_w, line_h)
        });
        Self {
            width,
            height,
            cell_w,
            line_h,
            scale,
            find,
            problems_chip,
            toolbar,
            panes,
            divider,
            divider_line,
            sidebar,
            status,
            minibuffer,
            picker,
        }
    }

    /// The view under a point.
    pub fn pane_at(&self, x: f32, y: f32) -> Option<&PaneLayout> {
        self.panes.iter().find(|p| p.area.contains(x, y))
    }

    /// Layout of view `pane`, if it is visible.
    pub fn pane(&self, pane: usize) -> Option<&PaneLayout> {
        self.panes.iter().find(|p| p.pane == pane)
    }

    /// Split ratio for a divider dragged to `x` (the views start right of the sidebar).
    pub fn ratio_at(&self, x: f32) -> f32 {
        let start = self.panes.first().map_or(0.0, |p| p.area.x);
        let width = self.width - start;
        if width <= 0.0 {
            0.5
        } else {
            (x - start) / width
        }
    }

    /// Explorer width in columns for a sidebar edge dragged to `x`.
    pub fn sidebar_cols_at(&self, x: f32) -> f32 {
        (x / self.cell_w).round()
    }
}

/// Geometry of the Explorer sidebar.
#[derive(Debug, Clone)]
pub struct SidebarLayout {
    pub area: Rect,
    /// Title bar with the workspace name.
    pub header: Rect,
    /// "+" (add folder) button in the header, in workspace mode.
    pub add_button: Option<Rect>,
    pub list: Rect,
    pub rows: usize,
    /// Visible 1px line at the right edge.
    pub edge_line: Rect,
    /// Grab area for resizing.
    pub edge: Rect,
    pub line_h: f32,
}

impl SidebarLayout {
    fn compute(ed: &Editor, area: Rect, bar_h: f32, line_h: f32, line_w: f32, scale: f32) -> Self {
        let header = Rect::new(area.x, area.y, area.w, bar_h);
        let add_button = ed
            .workspace
            .as_ref()
            .map(|_| Rect::new(area.right() - bar_h, area.y, bar_h, bar_h));
        let list = Rect::new(area.x, header.bottom(), area.w, (area.h - bar_h).max(0.0));
        let grab = (DIVIDER_GRAB * scale).round();
        Self {
            area,
            header,
            add_button,
            list,
            rows: ((list.h / line_h).floor() as usize).max(1),
            edge_line: Rect::new(area.right(), area.y, line_w, area.h),
            edge: Rect::new(area.right() - grab, area.y, line_w + 2.0 * grab, area.h),
            line_h,
        }
    }

    /// List row (relative to the scroll offset) under a point.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.list
            .contains(x, y)
            .then(|| ((y - self.list.y) / self.line_h).floor() as usize)
            .filter(|r| *r < self.rows)
    }
}

impl PaneLayout {
    fn compute(ed: &Editor, pane: usize, area: Rect, bar_h: f32, cell_w: f32, line_h: f32) -> Self {
        let tab_bar = Rect::new(area.x, area.y, area.w, bar_h);
        let lines = ed.panes[pane].doc().map_or(1, |d| d.rope.len_lines());
        let gutter_digits = lines.to_string().len().max(3);
        let gutter_w = if ed.config.line_numbers {
            ((gutter_digits + 2) as f32 * cell_w).round()
        } else {
            0.0
        };
        let body_y = tab_bar.bottom();
        let body_h = (area.bottom() - body_y).max(0.0);
        let gutter = Rect::new(area.x, body_y, gutter_w, body_h);
        let pad = (cell_w * 0.5).round();
        let text = Rect::new(
            area.x + gutter_w + pad,
            body_y,
            (area.w - gutter_w - pad).max(0.0),
            body_h,
        );
        Self {
            pane,
            area,
            tab_bar,
            gutter,
            text,
            gutter_digits,
            rows: ((body_h / line_h).floor() as usize).max(1),
            cols: ((text.w / cell_w).floor() as usize).max(1),
            cell_w,
            line_h,
        }
    }

    /// Text cell under a pixel position, relative to the text area (may be out of range).
    pub fn cell_at(&self, x: f32, y: f32) -> (isize, isize) {
        let row = ((y - self.text.y) / self.line_h).floor() as isize;
        let col = ((x - self.text.x) / self.cell_w).round() as isize;
        (row, col)
    }
}

pub struct TabRect {
    pub rect: Rect,
    pub title: String,
    pub dirty: bool,
    pub index: usize,
}

/// Tab positions of one view, scrolled horizontally so its active tab is always visible.
pub fn tab_rects(ed: &Editor, pl: &PaneLayout, scale: f32) -> Vec<TabRect> {
    let pane = &ed.panes[pl.pane];
    let mut x = pl.tab_bar.x;
    let mut tabs: Vec<TabRect> = pane
        .docs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            // Layout: two spaces, title, space, modified dot, two spaces.
            let title = d.title();
            let w = (title.width() + 6) as f32 * pl.cell_w;
            let r = Rect::new(x, pl.tab_bar.y, w, pl.tab_bar.h);
            x += w + scale.max(1.0);
            TabRect {
                rect: r,
                title,
                dirty: d.is_dirty(),
                index: i,
            }
        })
        .collect();
    if let Some(active) = tabs.get(pane.active) {
        let overflow = active.rect.right() - pl.tab_bar.right();
        if overflow > 0.0 {
            for t in &mut tabs {
                t.rect.x -= overflow;
            }
        }
    }
    tabs
}

/// Width in columns of the status bar problem counts (`✖ 2  ⚠ 3`), if there are any.
fn problems_chip_cols(ed: &Editor) -> Option<usize> {
    let (e, w) = ed.problems.counts();
    if e == 0 && w == 0 {
        return None;
    }
    let part = |n: usize| {
        if n > 0 {
            format!("✖ {n}").width() + 2
        } else {
            0
        }
    };
    Some(1 + part(e) + part(w) + 1)
}

/// Geometry of the Find in Files panel.
#[derive(Debug, Clone)]
pub struct FindLayout {
    pub panel: Rect,
    pub title: Rect,
    /// Option chips in the title bar: match case, whole word, regex.
    pub chips: [Rect; 3],
    pub query: Rect,
    pub include: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub rows: usize,
    pub line_h: f32,
}

/// Labels of the search option chips, in `FindLayout::chips` order.
pub const CHIP_LABELS: [&str; 3] = ["Aa", "ab", ".*"];

/// Search option chips right-aligned in `bar` (find panel title, Find prompt).
pub fn chip_rects(bar: Rect, cell_w: f32, line_h: f32) -> [Rect; 3] {
    let chip_w = (cell_w * 4.0).round();
    let gap = (cell_w * 0.5).round();
    let chip_h = (line_h * 1.1).round().min(bar.h);
    let y = (bar.y + (bar.h - chip_h) / 2.0).round();
    std::array::from_fn(|i| {
        let x = bar.right() - cell_w - (3 - i) as f32 * (chip_w + gap) + gap;
        Rect::new(x.round(), y, chip_w, chip_h)
    })
}

impl FindLayout {
    fn compute(body: Rect, bar_h: f32, cell_w: f32, line_h: f32) -> Self {
        let w = (body.w * 0.75)
            .max(60.0 * cell_w)
            .min(body.w - 2.0 * cell_w)
            .max(0.0)
            .round();
        let h = (body.h * 0.85)
            .max(bar_h * 4.0 + line_h * 3.0)
            .min(body.h)
            .round();
        let panel = Rect::new(
            (body.x + (body.w - w) / 2.0).round(),
            (body.y + (body.h - h) / 4.0).round(),
            w,
            h,
        );
        let title = Rect::new(panel.x, panel.y, panel.w, bar_h);
        let chips = chip_rects(title, cell_w, line_h);
        let query = Rect::new(panel.x, title.bottom(), panel.w, bar_h);
        let include = Rect::new(panel.x, query.bottom(), panel.w, bar_h);
        let footer = Rect::new(panel.x, panel.bottom() - bar_h, panel.w, bar_h);
        let list_y = include.bottom() + line_h * 0.25;
        let list_h = (footer.y - list_y - line_h * 0.25).max(line_h);
        let list = Rect::new(panel.x, list_y, panel.w, list_h);
        let rows = ((list.h / line_h).floor() as usize).max(1);
        Self {
            panel,
            title,
            chips,
            query,
            include,
            list,
            footer,
            rows,
            line_h,
        }
    }

    /// List row (relative to the scroll offset) under a point.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.list
            .contains(x, y)
            .then(|| ((y - self.list.y) / self.line_h).floor() as usize)
            .filter(|r| *r < self.rows)
    }

    /// Option chip under a point.
    pub fn chip_at(&self, x: f32, y: f32) -> Option<usize> {
        self.chips.iter().position(|c| c.contains(x, y))
    }
}

/// Geometry of the file picker overlay.
#[derive(Debug, Clone)]
pub struct PickerLayout {
    pub panel: Rect,
    pub title: Rect,
    pub filter: Rect,
    pub list: Rect,
    pub rows: usize,
    pub line_h: f32,
}

impl PickerLayout {
    fn compute(body: Rect, bar_h: f32, cell_w: f32, line_h: f32) -> Self {
        let w = (body.w * 0.6)
            .max(48.0 * cell_w)
            .min(body.w - 2.0 * cell_w)
            .max(0.0)
            .round();
        let h = (body.h * 0.75)
            .max(bar_h * 2.0 + line_h * 3.0)
            .min(body.h)
            .round();
        let panel = Rect::new(
            (body.x + (body.w - w) / 2.0).round(),
            (body.y + (body.h - h) / 3.0).round(),
            w,
            h,
        );
        let title = Rect::new(panel.x, panel.y, panel.w, bar_h);
        let filter = Rect::new(panel.x, title.bottom(), panel.w, bar_h);
        let list_h = (panel.bottom() - filter.bottom() - line_h * 0.5).max(line_h);
        let list = Rect::new(panel.x, filter.bottom() + line_h * 0.25, panel.w, list_h);
        let rows = ((list.h / line_h).floor() as usize).max(1);
        Self {
            panel,
            title,
            filter,
            list,
            rows,
            line_h,
        }
    }

    /// List row (relative to the scroll offset) under a point.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.list
            .contains(x, y)
            .then(|| ((y - self.list.y) / self.line_h).floor() as usize)
            .filter(|r| *r < self.rows)
    }
}

pub struct ToolButton {
    pub rect: Rect,
    pub label: String,
    pub cmd: Command,
}

/// Toolbar label with its shortcut, e.g. "Help  F1".
fn with_key(label: &str, cmd: Command) -> String {
    match keymap::shortcut_for(cmd) {
        Some(k) => format!("{label}  {k}"),
        None => label.to_string(),
    }
}

/// Menu bar: Workspace and File menus on the left, Help and Shortcuts (with keys) on the right.
pub fn toolbar_buttons(layout: &Layout, ed: &Editor) -> Vec<ToolButton> {
    let bar = layout.toolbar;
    let width = |label: &str| ((label.width() + 3) as f32 * layout.cell_w).round();
    let gap = (layout.cell_w * 0.5).round();
    let workspace = match &ed.workspace {
        Some(ws) => format!("Workspace: {}", ws.name),
        None => "Workspace".to_string(),
    };
    let mut out = Vec::new();
    let mut x = gap;
    for (label, cmd) in [
        (workspace, Command::WorkspaceMenu),
        ("File".to_string(), Command::FileMenu),
        ("Selection".to_string(), Command::SelectionMenu),
    ] {
        let w = width(&label);
        out.push(ToolButton {
            rect: Rect::new(x, bar.y, w, bar.h),
            label,
            cmd,
        });
        x += w;
    }
    let right = [
        (with_key("Help", Command::Help), Command::Help),
        (
            with_key("Shortcuts", Command::ShowShortcuts),
            Command::ShowShortcuts,
        ),
    ];
    let right_w: f32 = right.iter().map(|(l, _)| width(l)).sum();
    let mut x = (bar.right() - right_w - gap).max(x + layout.cell_w * 2.0);
    for (label, cmd) in right {
        let w = width(&label);
        out.push(ToolButton {
            rect: Rect::new(x, bar.y, w, bar.h),
            label,
            cmd,
        });
        x += w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn toolbar_and_menu_hit_testing() {
        use crate::ui::menu::{MenuId, menu_layout};
        let mut ed = Editor::new(Config::default(), &[]);
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        let buttons = toolbar_buttons(&l, &ed);
        let hit = |x: f32| {
            buttons
                .iter()
                .find(|b| b.rect.contains(x, l.toolbar.y + 5.0))
                .map(|b| b.cmd)
        };
        assert_eq!(hit(10.0), Some(Command::WorkspaceMenu));
        assert_eq!(hit(buttons[1].rect.x + 2.0), Some(Command::FileMenu));
        assert_eq!(hit(990.0), Some(Command::ShowShortcuts));
        assert_eq!(hit(buttons[2].rect.x + 2.0), Some(Command::SelectionMenu));
        assert_eq!(hit(500.0), None);
        assert!(buttons[3].label.ends_with("F1"));
        assert!(buttons[4].label.ends_with("F2"));

        let menu = menu_layout(&l, &ed, MenuId::File, buttons[1].rect);
        let first = &menu.rows[0];
        assert_eq!(
            menu.item_at(first.rect.x + 2.0, first.rect.y + 2.0),
            Some(0)
        );
        let sep = &menu.rows[9];
        assert_eq!(sep.entry.cmd, None);
        assert_eq!(menu.item_at(sep.rect.x + 2.0, sep.rect.y + 1.0), None);
        assert!(menu.panel.y >= l.toolbar.bottom());

        assert!(l.picker.is_none());
        ed.execute(Command::OpenFile);
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        let p = l.picker.expect("picker layout");
        assert!(p.panel.x > 0.0 && p.panel.right() < 1000.0);
        assert_eq!(p.row_at(p.list.x + 1.0, p.list.y + 1.0), Some(0));
        assert_eq!(p.row_at(0.0, 0.0), None);
    }

    #[test]
    fn split_geometry() {
        let mut ed = Editor::new(Config::default(), &[]);
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        assert_eq!(l.panes.len(), 2);
        assert_eq!(l.panes[0].area.w, 500.0);
        assert!(l.panes[1].area.x > 500.0);
        let d = l.divider.unwrap();
        assert!(d.contains(500.0, 300.0));
        assert_eq!(l.pane_at(100.0, 300.0).unwrap().pane, 0);
        assert_eq!(l.pane_at(900.0, 300.0).unwrap().pane, 1);

        // Ratio is clamped so each view keeps 20 columns.
        ed.split_ratio = 0.05;
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        assert_eq!(l.panes[0].area.w, 200.0);

        // Explorer sidebar takes space on the left; views split the rest.
        ed.split_ratio = 0.5;
        ed.explorer.visible = true;
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        let sb = l.sidebar.as_ref().unwrap();
        assert_eq!(sb.area.w, 320.0);
        assert!(l.panes[0].area.x > sb.area.right());
        assert!((l.panes[1].area.right() - 1000.0).abs() < 1.0);
        let mid = l.divider_line.unwrap().x;
        assert!((l.ratio_at(mid) - 0.5).abs() < 0.01);
        ed.explorer.visible = false;

        // Panel hidden: the editor view fills the width.
        ed.split = false;
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        assert_eq!(l.panes.len(), 1);
        assert_eq!(l.panes[0].pane, 0);
        assert_eq!(l.panes[0].area.w, 1000.0);
        assert!(l.divider.is_none());
    }
}
