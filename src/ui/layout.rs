//! Screen regions in physical pixels. Used by both rendering and mouse hit-testing.

use unicode_width::UnicodeWidthStr;

use crate::commands::Command;
use crate::editor::Editor;

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
    pub tab_bar: Rect,
    pub gutter: Rect,
    pub text: Rect,
    pub status: Rect,
    pub minibuffer: Option<Rect>,
    pub picker: Option<PickerLayout>,
    /// Digits reserved for line numbers.
    pub gutter_digits: usize,
    pub rows: usize,
    pub cols: usize,
}

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
        let tab_bar = Rect::new(0.0, toolbar.bottom(), width, bar_h);
        let minibuffer = ed
            .minibuffer
            .as_ref()
            .map(|_| Rect::new(0.0, height - bar_h, width, bar_h));
        let status_y = minibuffer.map_or(height, |r| r.y) - bar_h;
        let status = Rect::new(0.0, status_y, width, bar_h);

        let lines = ed.doc().rope.len_lines();
        let gutter_digits = lines.to_string().len().max(3);
        let gutter_w = if ed.config.line_numbers {
            ((gutter_digits + 2) as f32 * cell_w).round()
        } else {
            0.0
        };
        let body_y = tab_bar.bottom();
        let body_h = (status.y - body_y).max(0.0);
        let gutter = Rect::new(0.0, body_y, gutter_w, body_h);
        let pad = (cell_w * 0.5).round();
        let text = Rect::new(
            gutter_w + pad,
            body_y,
            (width - gutter_w - pad).max(0.0),
            body_h,
        );
        let rows = ((body_h / line_h).floor() as usize).max(1);
        let picker = ed.picker.as_ref().map(|_| {
            PickerLayout::compute(Rect::new(0.0, body_y, width, body_h), bar_h, cell_w, line_h)
        });
        let cols = ((text.w / cell_w).floor() as usize).max(1);
        Self {
            width,
            height,
            cell_w,
            line_h,
            scale,
            toolbar,
            tab_bar,
            gutter,
            text,
            status,
            minibuffer,
            picker,
            gutter_digits,
            rows,
            cols,
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

/// Tab positions, scrolled horizontally so the active tab is always visible.
pub fn tab_rects(ed: &Editor, layout: &Layout) -> Vec<TabRect> {
    let mut x = 0.0;
    let mut tabs: Vec<TabRect> = ed
        .docs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            // Layout: two spaces, title, space, modified dot, two spaces.
            let title = d.title();
            let w = (title.width() + 6) as f32 * layout.cell_w;
            let r = Rect::new(x, layout.tab_bar.y, w, layout.tab_bar.h);
            x += w + layout.scale.max(1.0);
            TabRect {
                rect: r,
                title,
                dirty: d.is_dirty(),
                index: i,
            }
        })
        .collect();
    if let Some(active) = tabs.get(ed.active) {
        let overflow = active.rect.right() - layout.width;
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
    pub label: &'static str,
    pub cmd: Command,
}

const TOOLBAR_LEFT: [(&str, Command); 4] = [
    ("New", Command::NewFile),
    ("Open", Command::OpenFile),
    ("Save", Command::Save),
    ("Save As", Command::SaveAs),
];
const TOOLBAR_RIGHT: [(&str, Command); 2] = [
    ("Help", Command::Help),
    ("Shortcuts", Command::ShowShortcuts),
];

/// Toolbar buttons: file actions on the left, help on the right.
pub fn toolbar_buttons(layout: &Layout) -> Vec<ToolButton> {
    let bar = layout.toolbar;
    let width = |label: &str| ((label.width() + 3) as f32 * layout.cell_w).round();
    let gap = (layout.cell_w * 0.5).round();
    let mut out = Vec::new();
    let mut x = gap;
    for (label, cmd) in TOOLBAR_LEFT {
        let w = width(label);
        out.push(ToolButton {
            rect: Rect::new(x, bar.y, w, bar.h),
            label,
            cmd,
        });
        x += w;
    }
    let right_w: f32 = TOOLBAR_RIGHT.iter().map(|(l, _)| width(l)).sum();
    let mut x = (bar.right() - right_w - gap).max(x + layout.cell_w * 2.0);
    for (label, cmd) in TOOLBAR_RIGHT {
        let w = width(label);
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
    fn toolbar_hit_testing() {
        let mut ed = Editor::new(Config::default(), &[]);
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        let buttons = toolbar_buttons(&l);
        let hit = |x: f32| {
            buttons
                .iter()
                .find(|b| b.rect.contains(x, l.toolbar.y + 5.0))
                .map(|b| b.cmd)
        };
        assert_eq!(hit(10.0), Some(Command::NewFile));
        assert_eq!(hit(995.0 - 5.0), Some(Command::ShowShortcuts));
        assert_eq!(hit(500.0), None);
        assert!(l.tab_bar.y >= l.toolbar.bottom());
        assert!(l.picker.is_none());

        ed.execute(Command::OpenFile);
        let l = Layout::compute(&ed, 1000.0, 600.0, 10.0, 20.0, 1.0);
        let p = l.picker.expect("picker layout");
        assert!(p.panel.x > 0.0 && p.panel.right() < 1000.0);
        assert_eq!(p.row_at(p.list.x + 1.0, p.list.y + 1.0), Some(0));
        assert_eq!(p.row_at(0.0, 0.0), None);
    }
}
