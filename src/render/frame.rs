//! Builds one frame's quads and text items from editor state.

use ropey::RopeSlice;
use unicode_width::UnicodeWidthStr;

use super::quads::Quad;
use super::text::TextItem;
use crate::editor::movement::{self as mv, char_width};
use crate::editor::{Editor, MsgKind};
use crate::keymap;
use crate::theme::{Rgba, Theme};
use crate::ui::layout::{
    Layout, PaneLayout, PickerLayout, Rect, file_menu_layout, tab_rects, toolbar_buttons,
};
use crate::ui::minibuffer::PromptKind;

#[derive(Default)]
pub struct Layer {
    pub quads: Vec<Quad>,
    pub text: Vec<TextItem>,
}

/// Two layers: the editor itself, and overlays (file picker) drawn completely on top.
pub struct FrameData {
    pub layers: [Layer; 2],
}

/// Mouse state of the toolbar, owned by the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiState {
    pub hover_button: Option<usize>,
    pub pressed_button: Option<usize>,
    /// File menu dropdown is open; `menu_hover` is the highlighted row.
    pub menu_open: bool,
    pub menu_hover: Option<usize>,
    /// Mouse is over (or dragging) the line between the views.
    pub divider_active: bool,
}

struct Builder<'a> {
    layers: [Layer; 2],
    /// Layer that `rect`/`label` draw into.
    current: usize,
    linear: bool,
    theme: &'a Theme,
}

impl Builder<'_> {
    fn rect(&mut self, r: Rect, color: Rgba) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.layers[self.current].quads.push(Quad {
            pos: [r.x, r.y],
            size: [r.w, r.h],
            color: if self.linear {
                color.linear()
            } else {
                color.srgb()
            },
        });
    }

    fn label(&mut self, text: impl Into<String>, x: f32, y: f32, color: Rgba, clip: Rect) {
        self.layers[self.current].text.push(TextItem {
            text: text.into(),
            x,
            y,
            color,
            clip,
        });
    }
}

/// Visible part of a line: tabs expanded, columns `[left, left + cols)`.
fn display_slice(line: RopeSlice, left: usize, cols: usize, tab: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let right = left + cols;
    for c in line.chars() {
        if c == '\n' {
            break;
        }
        let w = char_width(c, col, tab);
        let end = col + w;
        if end > left {
            if col < left || c == '\t' {
                // Partially scrolled-off wide char, or a tab: pad with spaces.
                out.extend(std::iter::repeat_n(' ', end - col.max(left)));
            } else if c.is_control() {
                out.push('·');
            } else {
                out.push(c);
            }
        }
        col = end;
        if col >= right {
            break;
        }
    }
    out
}

pub fn build(
    ed: &Editor,
    layout: &Layout,
    theme: &Theme,
    ui: UiState,
    cursor_on: bool,
    linear: bool,
) -> FrameData {
    let mut b = Builder {
        layers: Default::default(),
        current: 0,
        linear,
        theme,
    };
    let full = Rect::new(0.0, 0.0, layout.width, layout.height);
    b.rect(full, theme.bg);

    for pl in &layout.panes {
        let focused = pl.pane == ed.focus;
        draw_text_area(&mut b, ed, layout, pl, focused, cursor_on);
        draw_tab_bar(&mut b, ed, layout, pl, focused);
    }
    if let Some(line) = layout.divider_line {
        let color = if ui.divider_active {
            theme.accent
        } else {
            theme.border
        };
        b.rect(line, color);
    }
    draw_toolbar(&mut b, layout, ui);
    let tooltip = ui
        .hover_button
        .filter(|_| !ui.menu_open)
        .and_then(|i| toolbar_buttons(layout).into_iter().nth(i))
        .map(|btn| match keymap::shortcut_for(btn.cmd) {
            Some(keys) => format!("{} ({keys})", btn.cmd.description()),
            None => btn.cmd.description(),
        });
    draw_status_bar(&mut b, ed, layout, tooltip);
    draw_minibuffer(&mut b, ed, layout, cursor_on);

    b.current = 1;
    if let Some(pl) = &layout.picker {
        draw_picker(&mut b, ed, layout, pl, cursor_on);
    }
    if ui.menu_open {
        draw_file_menu(&mut b, layout, ui);
    }

    FrameData { layers: b.layers }
}

fn draw_toolbar(b: &mut Builder, l: &Layout, ui: UiState) {
    let theme = b.theme;
    b.rect(l.toolbar, theme.toolbar_bg);
    let text_y = l.toolbar.y + (l.toolbar.h - l.line_h) / 2.0;
    let inset = (l.scale * 3.0).round();
    for (i, btn) in toolbar_buttons(l).iter().enumerate() {
        let r = btn.rect;
        let open_menu = ui.menu_open && btn.cmd == crate::commands::Command::FileMenu;
        let pressed = ui.pressed_button == Some(i) || open_menu;
        let hovered = ui.hover_button == Some(i);
        let face = Rect::new(
            r.x + inset,
            r.y + inset,
            r.w - 2.0 * inset,
            r.h - 2.0 * inset,
        );
        let fg = if pressed {
            b.rect(face, theme.accent);
            theme.accent_fg
        } else if hovered {
            b.rect(face, theme.button_hover);
            theme.tab_fg_active
        } else {
            theme.button_fg
        };
        let label_w = btn.label.width() as f32 * l.cell_w;
        b.label(
            btn.label.clone(),
            r.x + (r.w - label_w) / 2.0,
            text_y,
            fg,
            l.toolbar,
        );
    }
    b.rect(
        Rect::new(0.0, l.toolbar.bottom() - 1.0, l.width, 1.0),
        theme.border,
    );
}

fn draw_file_menu(b: &mut Builder, l: &Layout, ui: UiState) {
    let theme = b.theme;
    let menu = file_menu_layout(l);
    let panel = menu.panel;
    let border = l.scale.max(1.0).round();
    let sh = (l.cell_w * 0.6).round();
    b.rect(
        Rect::new(panel.x + sh, panel.y + sh, panel.w, panel.h),
        theme.shadow,
    );
    b.rect(
        Rect::new(
            panel.x - border,
            panel.y - border,
            panel.w + 2.0 * border,
            panel.h + 2.0 * border,
        ),
        theme.overlay_border,
    );
    b.rect(panel, theme.overlay_bg);
    let pad = l.cell_w * 1.5;
    for (i, row) in menu.rows.iter().enumerate() {
        let r = row.rect;
        if row.cmd.is_none() {
            let y = (r.y + r.h / 2.0).round();
            b.rect(
                Rect::new(r.x + l.cell_w, y, r.w - 2.0 * l.cell_w, 1.0),
                theme.border,
            );
            continue;
        }
        let ty = r.y + (r.h - l.line_h) / 2.0;
        let fg = if ui.menu_hover == Some(i) {
            b.rect(r, theme.picker_selected);
            b.rect(
                Rect::new(r.x, r.y, (2.0 * l.scale).round().max(2.0), r.h),
                theme.accent,
            );
            theme.tab_fg_active
        } else {
            theme.fg
        };
        b.label(row.label, r.x + pad, ty, fg, panel);
        if !row.keys.is_empty() {
            let kx = r.right() - pad - row.keys.width() as f32 * l.cell_w;
            b.label(row.keys.clone(), kx, ty, theme.fg_dim, panel);
        }
    }
}

fn draw_picker(b: &mut Builder, ed: &Editor, l: &Layout, pl: &PickerLayout, cursor_on: bool) {
    let Some(p) = &ed.picker else {
        return;
    };
    let theme = b.theme;
    let panel = pl.panel;
    let border = l.scale.max(1.0).round();

    // Shadow, border, body.
    let sh = (l.cell_w * 0.6).round();
    b.rect(
        Rect::new(panel.x + sh, panel.y + sh, panel.w, panel.h),
        theme.shadow,
    );
    b.rect(
        Rect::new(
            panel.x - border,
            panel.y - border,
            panel.w + 2.0 * border,
            panel.h + 2.0 * border,
        ),
        theme.overlay_border,
    );
    b.rect(panel, theme.overlay_bg);

    // Title: current folder.
    b.rect(pl.title, theme.overlay_title_bg);
    let pad = l.cell_w;
    let ty = pl.title.y + (pl.title.h - l.line_h) / 2.0;
    let max_cols = ((pl.title.w - 2.0 * pad) / l.cell_w).max(0.0) as usize;
    let title = format!("Open  {}", p.dir.display());
    b.label(
        truncate_left(&title, max_cols),
        pl.title.x + pad,
        ty,
        theme.tab_fg_active,
        pl.title,
    );

    // Filter line.
    let fy = pl.filter.y + (pl.filter.h - l.line_h) / 2.0;
    let prompt = "Filter: ";
    b.label(prompt, pl.filter.x + pad, fy, theme.prompt_fg, pl.filter);
    let fx = pl.filter.x + pad + prompt.width() as f32 * l.cell_w;
    if p.filter.is_empty() {
        b.label(
            "type to filter, Enter opens, Backspace goes up",
            fx,
            fy,
            theme.fg_dim,
            pl.filter,
        );
    } else {
        b.label(p.filter.clone(), fx, fy, theme.fg, pl.filter);
    }
    if cursor_on {
        let cx = fx + p.filter.width() as f32 * l.cell_w;
        b.rect(
            Rect::new(cx, fy, (2.0 * l.scale).round().max(2.0), l.line_h),
            theme.cursor,
        );
    }
    b.rect(
        Rect::new(pl.filter.x, pl.filter.bottom() - 1.0, pl.filter.w, 1.0),
        theme.border,
    );

    // Entries.
    if let Some(err) = &p.error {
        b.label(
            err.clone(),
            pl.list.x + pad,
            pl.list.y,
            theme.error,
            pl.list,
        );
    } else if p.len() == 0 {
        let msg = if p.filter.is_empty() {
            "(empty folder)".to_string()
        } else {
            format!("No matches. Enter creates \"{}\"", p.filter)
        };
        b.label(msg, pl.list.x + pad, pl.list.y, theme.fg_dim, pl.list);
    }
    for (row, (i, e)) in p
        .entries()
        .enumerate()
        .skip(p.scroll)
        .take(pl.rows)
        .enumerate()
    {
        let y = pl.list.y + row as f32 * l.line_h;
        if i == p.selected {
            b.rect(
                Rect::new(pl.list.x, y, pl.list.w, l.line_h),
                theme.picker_selected,
            );
            b.rect(
                Rect::new(pl.list.x, y, (2.0 * l.scale).round().max(2.0), l.line_h),
                theme.accent,
            );
        }
        let (name, color) = if e.is_dir {
            (format!("{}/", e.name), theme.picker_dir)
        } else {
            (e.name.clone(), theme.fg)
        };
        b.label(name, pl.list.x + pad, y, color, pl.list);
    }

    // Scrollbar.
    if p.len() > pl.rows {
        let w = (l.cell_w * 0.5).round();
        let h = (pl.rows as f32 / p.len() as f32 * pl.list.h).max(l.line_h);
        let y = pl.list.y + p.scroll as f32 / p.len() as f32 * pl.list.h;
        b.rect(Rect::new(pl.list.right() - w, y, w, h), theme.scrollbar);
    }
}

/// Centered hint shown in a view with no open tabs.
fn draw_empty_pane(b: &mut Builder, l: &Layout, pl: &PaneLayout) {
    let lines = [
        "No file open",
        "",
        "Ctrl+O   open a file",
        "Ctrl+N   new file",
        "F1       help",
        "F2       keyboard shortcuts",
    ];
    let w = lines.iter().map(|s| s.width()).max().unwrap_or(0) as f32 * l.cell_w;
    let x = (pl.area.x + (pl.area.w - w) / 2.0).max(pl.area.x + l.cell_w);
    let mut y = pl.text.y + (pl.text.h - lines.len() as f32 * l.line_h) / 3.0;
    for (i, line) in lines.iter().enumerate() {
        if !line.is_empty() {
            let color = if i == 0 {
                b.theme.statusbar_fg
            } else {
                b.theme.fg_dim
            };
            b.label(*line, x, y, color, pl.area);
        }
        y += l.line_h;
    }
}

fn draw_text_area(
    b: &mut Builder,
    ed: &Editor,
    l: &Layout,
    pl: &PaneLayout,
    focused: bool,
    cursor_on: bool,
) {
    let theme = b.theme;
    let Some(doc) = ed.panes[pl.pane].doc() else {
        draw_empty_pane(b, l, pl);
        return;
    };
    let rope = &doc.rope;
    let tab = ed.config.tab_width;
    let (top, left) = (doc.view.top_line, doc.view.left_col);
    let cursor_line = rope.char_to_line(doc.selection.head);
    let (sel_s, sel_e) = doc.selection.range();
    // Find highlights belong to the focused document only.
    let query = if focused { ed.active_query() } else { None };
    let body = Rect::new(pl.area.x, pl.text.y, pl.area.w, pl.text.h);

    b.rect(pl.gutter, theme.gutter_bg);

    for row in 0..=pl.rows {
        let line = top + row;
        if line >= rope.len_lines() {
            break;
        }
        let y = pl.text.y + row as f32 * l.line_h;
        let ls = rope.line_to_char(line);
        let le = mv::line_content_end(rope, line);
        let col_x = |p: usize| -> f32 {
            let c = mv::width_of(rope.slice(ls..p), 0, tab) as f32 - left as f32;
            pl.text.x + c * l.cell_w
        };

        if line == cursor_line && focused {
            b.rect(
                Rect::new(
                    pl.text.x - l.cell_w * 0.5,
                    y,
                    pl.text.w + l.cell_w * 0.5,
                    l.line_h,
                ),
                theme.current_line,
            );
        }

        if let Some(q) = &query {
            let text: std::borrow::Cow<str> = rope.slice(ls..le).into();
            for m in q.matches_in(&text) {
                let (s, e) = (ls + m, ls + m + q.len());
                let x0 = col_x(s).max(pl.text.x);
                let x1 = col_x(e).min(pl.text.right());
                let color = if (s, e) == (sel_s, sel_e) {
                    theme.find_match_active
                } else {
                    theme.find_match
                };
                b.rect(Rect::new(x0, y, x1 - x0, l.line_h), color);
            }
        }

        // Selection, including one cell for a selected newline.
        if sel_s < sel_e && sel_s <= le && sel_e > ls {
            let s = sel_s.max(ls);
            let e = sel_e.min(le);
            let x0 = col_x(s).max(pl.text.x);
            let mut x1 = col_x(e);
            if sel_e > le && line + 1 < rope.len_lines() {
                x1 += l.cell_w * 0.6;
            }
            b.rect(
                Rect::new(x0, y, (x1 - x0).min(pl.text.right() - x0), l.line_h),
                theme.selection,
            );
        }

        if ed.config.line_numbers {
            let num = format!("{:>w$}", line + 1, w = pl.gutter_digits);
            let color = if line == cursor_line && focused {
                theme.gutter_fg_active
            } else {
                theme.gutter_fg
            };
            b.label(num, pl.gutter.x + l.cell_w, y, color, body);
        }

        let visible = display_slice(rope.line(line), left, pl.cols + 2, tab);
        if !visible.trim_end().is_empty() {
            b.label(visible, pl.text.x, y, theme.fg, pl.text);
        }

        if line == cursor_line && focused && cursor_on && ed.minibuffer.is_none() {
            let x = col_x(doc.selection.head);
            if x >= pl.text.x - 1.0 && x <= pl.text.right() {
                let w = (2.0 * l.scale).round().max(2.0);
                b.rect(Rect::new(x, y, w, l.line_h), theme.cursor);
            }
        }
    }

    // Scrollbar.
    let total = rope.len_lines() + pl.rows.saturating_sub(1);
    if rope.len_lines() > pl.rows {
        let w = (l.cell_w * 0.6).round();
        let h = (pl.rows as f32 / total as f32 * pl.text.h).max(l.line_h);
        let y = pl.text.y + top as f32 / total as f32 * pl.text.h;
        b.rect(Rect::new(pl.area.right() - w, y, w, h), theme.scrollbar);
    }
}

fn draw_tab_bar(b: &mut Builder, ed: &Editor, l: &Layout, pl: &PaneLayout, focused: bool) {
    let theme = b.theme;
    let bar = pl.tab_bar;
    b.rect(bar, theme.tabbar_bg);
    let text_y = bar.y + (bar.h - l.line_h) / 2.0;
    let active_index = ed.panes[pl.pane].active;
    for t in tab_rects(ed, pl, l.scale) {
        let active = t.index == active_index;
        if active {
            b.rect(t.rect, theme.tab_active_bg);
            // Accent line marks the tab that has the cursor.
            let mark = if focused { theme.accent } else { theme.border };
            b.rect(
                Rect::new(t.rect.x, t.rect.y, t.rect.w, (2.0 * l.scale).round()),
                mark,
            );
        }
        let color = if active && focused {
            theme.tab_fg_active
        } else if active {
            theme.statusbar_fg
        } else {
            theme.tab_fg
        };
        b.label(format!("  {}", t.title), t.rect.x, text_y, color, bar);
        if t.dirty {
            let x = t.rect.x + (t.title.width() + 3) as f32 * l.cell_w;
            b.label("●", x, text_y, theme.modified, bar);
        }
        b.rect(
            Rect::new(
                t.rect.right(),
                t.rect.y + l.line_h * 0.25,
                l.scale.max(1.0),
                t.rect.h - l.line_h * 0.5,
            ),
            theme.border,
        );
    }
    b.rect(
        Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0),
        theme.border,
    );
}

pub fn mode_label(ed: &Editor) -> &'static str {
    if ed.picker.is_some() {
        return "PICK";
    }
    match ed.minibuffer.as_ref().map(|m| &m.kind) {
        None if ed.has_doc() && ed.doc().read_only => "VIEW",
        None => "EDIT",
        Some(PromptKind::Open) => "OPEN",
        Some(PromptKind::SaveAs(_)) => "SAVE",
        Some(PromptKind::Find { .. }) => "FIND",
        Some(PromptKind::GotoLine) => "GOTO",
        Some(PromptKind::ConfirmClose { .. } | PromptKind::ConfirmQuit) => "CONFIRM",
    }
}

fn draw_status_bar(b: &mut Builder, ed: &Editor, l: &Layout, tooltip: Option<String>) {
    let theme = b.theme;
    let r = l.status;
    b.rect(r, theme.statusbar_bg);
    b.rect(Rect::new(0.0, r.y, l.width, 1.0), theme.border);
    let y = r.y + (r.h - l.line_h) / 2.0;
    let doc = ed.pane().doc();

    // Mode chip, vim style.
    let mode = format!(" {} ", mode_label(ed));
    let chip_w = mode.width() as f32 * l.cell_w;
    b.rect(Rect::new(0.0, r.y, chip_w, r.h), theme.accent);
    b.label(mode, 0.0, y, theme.accent_fg, r);

    // Right side: selection, position, indent, encoding, line ending.
    let mut right = String::new();
    if let Some(doc) = doc {
        let head = doc.selection.head;
        let line = doc.rope.char_to_line(head);
        let col = mv::col_of(&doc.rope, head, ed.config.tab_width);
        if !doc.selection.is_empty() {
            let (s, e) = doc.selection.range();
            right.push_str(&format!("({} selected)  ", e - s));
        }
        right.push_str(&format!(
            "Ln {}, Col {}  {}  UTF-8  {} ",
            line + 1,
            col + 1,
            doc.indent.label(),
            doc.line_ending.label()
        ));
    }
    let right_w = right.width() as f32 * l.cell_w;
    let right_x = l.width - right_w - l.cell_w;
    b.label(right, right_x, y, theme.statusbar_fg, r);

    // Left side: file path, cut from the left so the file name stays visible.
    let mut x = chip_w + l.cell_w;
    let avail_cols = ((right_x - x) / l.cell_w).max(0.0) as usize;
    let budget = if ed.message.is_some() {
        avail_cols / 2
    } else {
        avail_cols
    }
    .saturating_sub(5);
    let name = match doc {
        Some(d) => match &d.path {
            Some(p) => truncate_left(&shorten_home(&p.display().to_string()), budget),
            None => d.title(),
        },
        None => "No file".to_string(),
    };
    b.label(name.clone(), x, y, theme.statusbar_fg, r);
    x += name.width() as f32 * l.cell_w;
    if doc.is_some_and(|d| d.is_dirty()) {
        b.label(" [+]", x, y, theme.modified, r);
        x += 4.0 * l.cell_w;
    }

    let shown = match tooltip {
        Some(t) => Some((t, theme.tab_fg_active)),
        None => ed.message.as_ref().map(|msg| {
            let color = match msg.kind {
                MsgKind::Info => theme.fg_dim,
                MsgKind::Warn => theme.warning,
                MsgKind::Error => theme.error,
            };
            (msg.text.clone(), color)
        }),
    };
    if let Some((text, color)) = shown {
        let mx = x + 2.0 * l.cell_w;
        let clip = Rect::new(mx, r.y, (right_x - mx - l.cell_w).max(0.0), r.h);
        b.label(text, mx, y, color, clip);
    }
}

fn draw_minibuffer(b: &mut Builder, ed: &Editor, l: &Layout, cursor_on: bool) {
    let (Some(mb), Some(r)) = (&ed.minibuffer, l.minibuffer) else {
        return;
    };
    let theme = b.theme;
    b.rect(r, theme.minibuffer_bg);
    let y = r.y + (r.h - l.line_h) / 2.0;
    let label = mb.label(&ed.doc().title());
    let x0 = l.cell_w;
    b.label(label.clone(), x0, y, theme.prompt_fg, r);
    let ix = x0 + label.width() as f32 * l.cell_w;
    b.label(mb.input.clone(), ix, y, theme.fg, r);
    if cursor_on && !mb.is_confirm() {
        let before: String = mb.input.chars().take(mb.cursor).collect();
        let cx = ix + before.width() as f32 * l.cell_w;
        let w = (2.0 * l.scale).round().max(2.0);
        b.rect(Rect::new(cx, y, w, l.line_h), theme.cursor);
    }
}

/// Keep the last `max` columns of `s`, prefixed with an ellipsis when cut.
fn truncate_left(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = 1; // the ellipsis
    for c in s.chars().rev() {
        let cw = c.to_string().width();
        if w + cw > max {
            break;
        }
        w += cw;
        out.push(c);
    }
    std::iter::once('…').chain(out.into_iter().rev()).collect()
}

fn shorten_home(path: &str) -> String {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
    match home {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn truncate_keeps_tail() {
        assert_eq!(truncate_left("/a/b/file.rs", 20), "/a/b/file.rs");
        assert_eq!(truncate_left("/a/b/file.rs", 8), "…file.rs");
    }

    #[test]
    fn display_slice_expands_and_scrolls() {
        let r = Rope::from_str("\tab日c\n");
        assert_eq!(display_slice(r.line(0), 0, 80, 4), "    ab日c");
        assert_eq!(display_slice(r.line(0), 2, 80, 4), "  ab日c");
        assert_eq!(display_slice(r.line(0), 7, 80, 4), " c"); // half of the wide char
        assert_eq!(display_slice(r.line(0), 0, 5, 4), "    a");
    }
}
