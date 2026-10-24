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
    pub status: Rect,
    pub minibuffer: Option<Rect>,
    pub picker: Option<PickerLayout>,
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
        let body_y = toolbar.bottom();
        let body_h = (status.y - body_y).max(0.0);

        let mut panes = Vec::new();
        let (mut divider, mut divider_line) = (None, None);
        if ed.split {
            let line_w = scale.max(1.0).round();
            let min_w = MIN_PANE_COLS * cell_w;
            let mut left_w = (width * ed.split_ratio).round();
            if width - line_w >= 2.0 * min_w {
                left_w = left_w.clamp(min_w, width - line_w - min_w);
            }
            let left = Rect::new(0.0, body_y, left_w, body_h);
            let right = Rect::new(left_w + line_w, body_y, width - left_w - line_w, body_h);
            panes.push(PaneLayout::compute(ed, 0, left, bar_h, cell_w, line_h));
            panes.push(PaneLayout::compute(ed, 1, right, bar_h, cell_w, line_h));
            let grab = (DIVIDER_GRAB * scale).round();
            divider_line = Some(Rect::new(left_w, body_y, line_w, body_h));
            divider = Some(Rect::new(
                left_w - grab,
                body_y,
                line_w + 2.0 * grab,
                body_h,
            ));
        } else {
            let area = Rect::new(0.0, body_y, width, body_h);
            panes.push(PaneLayout::compute(
                ed, ed.focus, area, bar_h, cell_w, line_h,
            ));
        }

        let picker = ed.picker.as_ref().map(|_| {
            PickerLayout::compute(Rect::new(0.0, body_y, width, body_h), bar_h, cell_w, line_h)
        });
        Self {
            width,
            height,
            cell_w,
            line_h,
            scale,
            toolbar,
            panes,
            divider,
            divider_line,
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

    /// Split ratio for a divider dragged to `x`.
    pub fn ratio_at(&self, x: f32) -> f32 {
        if self.width <= 0.0 {
            0.5
        } else {
            x / self.width
        }
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

/// Menu bar: the File menu on the left, Help and Shortcuts (with their keys) on the right.
pub fn toolbar_buttons(layout: &Layout) -> Vec<ToolButton> {
    let bar = layout.toolbar;
    let width = |label: &str| ((label.width() + 3) as f32 * layout.cell_w).round();
    let gap = (layout.cell_w * 0.5).round();
    let file = "File".to_string();
    let file_w = width(&file);
    let mut out = vec![ToolButton {
        rect: Rect::new(gap, bar.y, file_w, bar.h),
        label: file,
        cmd: Command::FileMenu,
    }];
    let right = [
        (with_key("Help", Command::Help), Command::Help),
        (
            with_key("Shortcuts", Command::ShowShortcuts),
            Command::ShowShortcuts,
        ),
    ];
    let right_w: f32 = right.iter().map(|(l, _)| width(l)).sum();
    let mut x = (bar.right() - right_w - gap).max(gap + file_w + layout.cell_w * 2.0);
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

/// Entries of the File menu; `None` is a separator.
pub const FILE_MENU: [Option<(&str, Command)>; 8] = [
    Some(("New", Command::NewFile)),
    Some(("Open…", Command::OpenFile)),
    Some(("Open Path…", Command::OpenPath)),
    Some(("Save", Command::Save)),
    Some(("Save As…", Command::SaveAs)),
    Some(("Close Tab", Command::CloseFile)),
    None,
    Some(("Quit", Command::Quit)),
];

pub struct MenuRow {
    pub rect: Rect,
    pub label: &'static str,
    /// Shortcut text shown right-aligned.
    pub keys: String,
    /// `None` for a separator.
    pub cmd: Option<Command>,
}

pub struct MenuLayout {
    pub panel: Rect,
    pub rows: Vec<MenuRow>,
}

impl MenuLayout {
    /// Index of the (non-separator) row under a point.
    pub fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.cmd.is_some() && r.rect.contains(x, y))
    }
}

/// The File dropdown, anchored under the File button.
pub fn file_menu_layout(layout: &Layout) -> MenuLayout {
    let anchor = toolbar_buttons(layout)
        .into_iter()
        .next()
        .map_or(layout.toolbar, |b| b.rect);
    let items: Vec<(&'static str, String, Option<Command>)> = FILE_MENU
        .iter()
        .map(|e| match e {
            Some((label, cmd)) => (
                *label,
                keymap::shortcut_for(*cmd).unwrap_or_default(),
                Some(*cmd),
            ),
            None => ("", String::new(), None),
        })
        .collect();
    let cols = items
        .iter()
        .map(|(l, k, _)| l.width() + k.width())
        .max()
        .unwrap_or(10)
        + 8;
    let w = (cols as f32 * layout.cell_w).round();
    let row_h = (layout.line_h * 1.4).round();
    let sep_h = (layout.line_h * 0.6).round();
    let pad = (layout.line_h * 0.25).round();
    let mut y = anchor.bottom() + pad;
    let rows = items
        .into_iter()
        .map(|(label, keys, cmd)| {
            let h = if cmd.is_some() { row_h } else { sep_h };
            let r = MenuRow {
                rect: Rect::new(anchor.x, y, w, h),
                label,
                keys,
                cmd,
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

    #[test]
    fn toolbar_and_menu_hit_testing() {
        let mut ed = Editor::new(Config::default(), &[]);
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        let buttons = toolbar_buttons(&l);
        let hit = |x: f32| {
            buttons
                .iter()
                .find(|b| b.rect.contains(x, l.toolbar.y + 5.0))
                .map(|b| b.cmd)
        };
        assert_eq!(hit(10.0), Some(Command::FileMenu));
        assert_eq!(hit(990.0), Some(Command::ShowShortcuts));
        assert_eq!(hit(500.0), None);
        assert!(buttons[1].label.ends_with("F1"));
        assert!(buttons[2].label.ends_with("F2"));

        let menu = file_menu_layout(&l);
        let first = &menu.rows[0];
        assert_eq!(
            menu.item_at(first.rect.x + 2.0, first.rect.y + 2.0),
            Some(0)
        );
        let sep = &menu.rows[6];
        assert_eq!(sep.cmd, None);
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

        // One view: the focused view fills the width.
        ed.split = false;
        ed.focus = 1;
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        assert_eq!(l.panes.len(), 1);
        assert_eq!(l.panes[0].pane, 1);
        assert_eq!(l.panes[0].area.w, 1000.0);
        assert!(l.divider.is_none());
    }
}
