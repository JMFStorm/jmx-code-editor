//! Builds one frame's quads and text items from editor state.

use ropey::RopeSlice;
use unicode_width::UnicodeWidthStr;

use super::quads::Quad;
use super::text::TextItem;
use crate::editor::movement::{self as mv, char_width};
use crate::editor::search::SearchOptions;
use crate::editor::{Editor, MsgKind, PANEL};
use crate::keymap;
use crate::lint::Severity;
use crate::syntax::{Span, Token};
use crate::test_explorer::Status as TestStatus;
use crate::theme::{Rgba, Theme};
use crate::ui::find_panel::{Field, Row};
use crate::ui::layout::{
    CHIP_LABELS, FindLayout, Layout, MARKS_COLS, PaneLayout, PickerLayout, Rect, SidebarLayout,
    chip_rects, tab_rects, toolbar_buttons,
};
use crate::ui::menu::{MenuId, menu_layout};
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
    /// Open drop-down menu; `menu_hover` is the highlighted row.
    pub menu: Option<MenuId>,
    pub menu_hover: Option<usize>,
    /// Mouse is over (or dragging) the line between the views.
    pub divider_active: bool,
    /// Mouse is over (or dragging) the Explorer's right edge.
    pub sidebar_edge_active: bool,
    /// View whose scrollbar the mouse is over or dragging.
    pub scrollbar_hot: Option<usize>,
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
        self.colored_label(text.into(), Vec::new(), x, y, color, clip);
    }

    /// A label with colored byte ranges; the rest uses `color`.
    fn colored_label(
        &mut self,
        text: String,
        spans: Vec<(std::ops::Range<usize>, Rgba)>,
        x: f32,
        y: f32,
        color: Rgba,
        clip: Rect,
    ) {
        self.layers[self.current].text.push(TextItem {
            text,
            x,
            y,
            color,
            clip,
            spans,
        });
    }
}

/// Visible part of a line: tabs expanded, columns `[left, left + cols)`.
#[cfg(test)]
fn display_slice(line: RopeSlice, left: usize, cols: usize, tab: usize) -> String {
    display_spans(line, left, cols, tab, &[], |_| Rgba(0, 0, 0, 0)).0
}

/// Like `display_slice`, also mapping the syntax `spans` (char ranges of the line) to
/// colored byte ranges of the returned string. Later spans win where they overlap.
fn display_spans(
    line: RopeSlice,
    left: usize,
    cols: usize,
    tab: usize,
    spans: &[Span],
    color: impl Fn(Token) -> Rgba,
) -> (String, Vec<(std::ops::Range<usize>, Rgba)>) {
    // Token of each char of the line (the visible part is short, so this stays cheap).
    let len = line.len_chars();
    let mut tokens: Vec<Option<Token>> = vec![None; if spans.is_empty() { 0 } else { len }];
    for s in spans {
        for t in tokens.iter_mut().take(s.end.min(len)).skip(s.start) {
            *t = Some(s.token);
        }
    }
    let mut out = String::new();
    let mut ranges: Vec<(std::ops::Range<usize>, Rgba)> = Vec::new();
    let mut col = 0;
    let right = left + cols;
    for (i, c) in line.chars().enumerate() {
        if c == '\n' {
            break;
        }
        let w = char_width(c, col, tab);
        let end = col + w;
        if end > left {
            let from = out.len();
            if col < left || c == '\t' {
                // Partially scrolled-off wide char, or a tab: pad with spaces.
                out.extend(std::iter::repeat_n(' ', end - col.max(left)));
            } else if c.is_control() {
                out.push('·');
            } else {
                out.push(c);
            }
            if let Some(Some(t)) = tokens.get(i) {
                let rgba = color(*t);
                match ranges.last_mut() {
                    Some((r, c)) if r.end == from && *c == rgba => r.end = out.len(),
                    _ => ranges.push((from..out.len(), rgba)),
                }
            }
        }
        col = end;
        if col >= right {
            break;
        }
    }
    (out, ranges)
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

    if let Some(sb) = &layout.sidebar {
        draw_explorer(&mut b, ed, layout, sb);
        let color = if ui.sidebar_edge_active {
            theme.accent
        } else {
            theme.border
        };
        b.rect(sb.edge_line, color);
    }
    for pl in &layout.panes {
        // The Explorer can hold the keyboard focus instead of a view.
        let focused = pl.pane == ed.focus && !ed.explorer.focused;
        let hot = ui.scrollbar_hot == Some(pl.pane);
        draw_text_area(&mut b, ed, layout, pl, focused, cursor_on, hot);
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
    draw_toolbar(&mut b, ed, layout, ui);
    let tooltip = ui
        .hover_button
        .filter(|_| ui.menu.is_none())
        .and_then(|i| toolbar_buttons(layout, ed).into_iter().nth(i))
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
    if let Some(fl) = &layout.find {
        draw_find_panel(&mut b, ed, layout, fl, cursor_on);
    }
    // A hover popup, else the parameter hints of the call being typed.
    if let Some(h) = ed.hover.as_ref().or(ed.symbols.signature.as_ref()) {
        draw_hover(&mut b, ed, layout, h);
    }
    if let Some(menu) = ui.menu {
        draw_menu(&mut b, ed, layout, ui, menu);
    }

    FrameData { layers: b.layers }
}

fn draw_toolbar(b: &mut Builder, ed: &Editor, l: &Layout, ui: UiState) {
    let theme = b.theme;
    b.rect(l.toolbar, theme.toolbar_bg);
    let text_y = l.toolbar.y + (l.toolbar.h - l.line_h) / 2.0;
    let inset = (l.scale * 3.0).round();
    for (i, btn) in toolbar_buttons(l, ed).iter().enumerate() {
        let r = btn.rect;
        let open_menu = ui.menu.is_some() && MenuId::for_command(btn.cmd) == ui.menu;
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

fn draw_menu(b: &mut Builder, ed: &Editor, l: &Layout, ui: UiState, menu: MenuId) {
    let theme = b.theme;
    let Some(anchor) = toolbar_buttons(l, ed)
        .into_iter()
        .find(|btn| MenuId::for_command(btn.cmd) == Some(menu))
        .map(|btn| btn.rect)
    else {
        return;
    };
    let menu = menu_layout(l, ed, menu, anchor);
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
        let entry = &row.entry;
        if entry.cmd.is_none() {
            let y = (r.y + r.h / 2.0).round();
            b.rect(
                Rect::new(r.x + l.cell_w, y, r.w - 2.0 * l.cell_w, 1.0),
                theme.border,
            );
            continue;
        }
        let ty = r.y + (r.h - l.line_h) / 2.0;
        let fg = if !entry.enabled {
            theme.gutter_fg
        } else if ui.menu_hover == Some(i) {
            b.rect(r, theme.picker_selected);
            b.rect(
                Rect::new(r.x, r.y, (2.0 * l.scale).round().max(2.0), r.h),
                theme.accent,
            );
            theme.tab_fg_active
        } else {
            theme.fg
        };
        // Toggles get a check box in front of the label.
        let label = match entry.checked {
            Some(true) => format!("☑ {}", entry.label),
            Some(false) => format!("☐ {}", entry.label),
            None => format!("  {}", entry.label),
        };
        b.label(label, r.x + pad, ty, fg, panel);
        if !row.keys.is_empty() {
            let kx = r.right() - pad - row.keys.width() as f32 * l.cell_w;
            let kc = if entry.enabled {
                theme.fg_dim
            } else {
                theme.gutter_fg
            };
            b.label(row.keys.clone(), kx, ty, kc, panel);
        }
    }
}

fn draw_explorer(b: &mut Builder, ed: &Editor, l: &Layout, sb: &SidebarLayout) {
    let theme = b.theme;
    let ex = &ed.explorer;
    b.rect(sb.area, theme.tabbar_bg);

    // Header: workspace name, and a "+" button to add a folder.
    b.rect(sb.header, theme.toolbar_bg);
    let hy = sb.header.y + (sb.header.h - l.line_h) / 2.0;
    let title = match &ed.workspace {
        Some(ws) => format!("  {}", ws.name.to_uppercase()),
        None => "  EXPLORER".to_string(),
    };
    let title_color = if ex.focused {
        theme.tab_fg_active
    } else {
        theme.statusbar_fg
    };
    b.label(title, sb.header.x, hy, title_color, sb.header);
    if let Some(btn) = sb.add_button {
        let x = btn.x + (btn.w - l.cell_w) / 2.0;
        b.label("+", x, hy, theme.accent, sb.header);
    }
    b.rect(
        Rect::new(sb.header.x, sb.header.bottom() - 1.0, sb.header.w, 1.0),
        theme.border,
    );

    // Empty states.
    let hint = match &ed.workspace {
        None => Some([
            "No workspace open.",
            "Workspace → New Workspace…",
            "or Open Workspace…",
        ]),
        Some(ws) if ws.folders.is_empty() => Some([
            "No folders yet.",
            "Click here or use",
            "Workspace → Add Folder…",
        ]),
        _ => None,
    };
    if let Some(lines) = hint {
        let mut y = sb.list.y + l.line_h * 0.5;
        for (i, line) in lines.iter().enumerate() {
            let color = if i == 0 {
                theme.statusbar_fg
            } else {
                theme.fg_dim
            };
            b.label(*line, sb.list.x + l.cell_w, y, color, sb.list);
            y += l.line_h;
        }
        return;
    }

    // The active file of the focused view is highlighted in the tree.
    let active_path = ed.pane().doc().and_then(|d| d.path.clone());
    for (row_i, (i, row)) in ex
        .rows
        .iter()
        .enumerate()
        .skip(ex.scroll)
        .take(sb.rows)
        .enumerate()
    {
        let y = sb.list.y + row_i as f32 * l.line_h;
        let line = Rect::new(sb.list.x, y, sb.list.w, l.line_h);
        if i == ex.selected {
            b.rect(line, theme.picker_selected);
            if ex.focused {
                b.rect(
                    Rect::new(line.x, y, (2.0 * l.scale).round().max(2.0), l.line_h),
                    theme.accent,
                );
            }
        }
        let indent = (1 + row.depth * 2) as f32 * l.cell_w;
        let (marker, color) = if row.is_dir {
            let arrow = if row.expanded { "▾ " } else { "▸ " };
            let c = if row.is_root {
                theme.tab_fg_active
            } else {
                theme.picker_dir
            };
            (arrow, c)
        } else if active_path.as_ref() == Some(&row.path) {
            ("  ", theme.tab_fg_active)
        } else {
            ("  ", theme.fg)
        };
        let name = if row.is_root {
            row.name.to_uppercase()
        } else {
            row.name.clone()
        };
        b.label(
            format!("{marker}{name}"),
            sb.list.x + indent,
            y,
            color,
            sb.list,
        );
    }

    // Scrollbar.
    if ex.rows.len() > sb.rows {
        let w = (l.cell_w * 0.5).round();
        let h = (sb.rows as f32 / ex.rows.len() as f32 * sb.list.h).max(l.line_h);
        let y = sb.list.y + ex.scroll as f32 / ex.rows.len() as f32 * sb.list.h;
        b.rect(Rect::new(sb.list.right() - w, y, w, h), theme.scrollbar);
    }
}

fn draw_picker(b: &mut Builder, ed: &Editor, l: &Layout, pl: &PickerLayout, cursor_on: bool) {
    let Some(p) = &ed.picker else {
        return;
    };
    let theme = b.theme;
    draw_overlay_frame(b, l, pl.panel);

    // Title: current folder.
    b.rect(pl.title, theme.overlay_title_bg);
    let pad = l.cell_w;
    let ty = pl.title.y + (pl.title.h - l.line_h) / 2.0;
    let max_cols = ((pl.title.w - 2.0 * pad) / l.cell_w).max(0.0) as usize;
    let title = p.title();
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
        b.label(p.hint(), fx, fy, theme.fg_dim, pl.filter);
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

/// Popup with a test failure or problems, below its anchor (above it when there is no room),
/// kept inside the window. Long lines wrap.
fn draw_hover(b: &mut Builder, ed: &Editor, l: &Layout, h: &crate::editor::hover::Hover) {
    use crate::editor::hover::{HoverAt, HoverKind};
    let theme = b.theme;
    // The anchor: the mouse, or the cursor's cell.
    let (ax, ay, line_top) = match h.at {
        HoverAt::Point(x, y) => (x, y, y - l.line_h),
        HoverAt::Cell { pane, line, col } => {
            let Some(pl) = l.pane(pane) else {
                return;
            };
            let Some(doc) = ed.panes[pane].doc() else {
                return;
            };
            let row = line as f32 - doc.view.top_line as f32;
            let pos = doc.rope.line_to_char(line.min(doc.rope.len_lines() - 1)) + col;
            let c = mv::col_of(
                &doc.rope,
                pos.min(doc.rope.len_chars()),
                ed.config.tab_width,
            ) as f32
                - doc.view.left_col as f32;
            let y = pl.text.y + row * l.line_h;
            (pl.text.x + c * l.cell_w, y + l.line_h, y)
        }
    };
    let max_cols = ((l.width / l.cell_w) as usize)
        .saturating_sub(4)
        .clamp(20, 90);
    // Wrap at words.
    let mut rows: Vec<(String, HoverKind)> = Vec::new();
    for (text, kind) in &h.lines {
        if text.is_empty() {
            rows.push((String::new(), *kind));
            continue;
        }
        for piece in crate::editor::hover::wrap(text, max_cols) {
            rows.push((piece, *kind));
        }
    }
    let cols = rows
        .iter()
        .map(|(t, _)| t.width())
        .max()
        .unwrap_or(1)
        .max(10);
    let pad = l.cell_w;
    let w = cols as f32 * l.cell_w + 2.0 * pad;
    let h_px = rows.len() as f32 * l.line_h + l.line_h * 0.6;
    let fits_below = ay + h_px <= l.status.y;
    let fits_above = line_top - h_px - 2.0 >= l.toolbar.bottom();
    let y = if h.above && fits_above || !fits_below {
        (line_top - h_px - 2.0).max(l.toolbar.bottom())
    } else {
        ay + 2.0
    };
    let x = ax.min(l.width - w - pad).max(pad);
    let panel = Rect::new(x, y, w, h_px);
    draw_overlay_frame(b, l, panel);
    let mut ty = y + l.line_h * 0.3;
    for (text, kind) in rows {
        let color = match kind {
            HoverKind::Error => theme.error,
            HoverKind::Warning => theme.warning,
            HoverKind::Pass => theme.token(Token::Pass),
            HoverKind::Text => theme.fg,
            HoverKind::Dim => theme.fg_dim,
            HoverKind::Code => theme.token(Token::Function),
            HoverKind::Param => theme.token(Token::Type),
        };
        b.label(text, x + pad, ty, color, panel);
        ty += l.line_h;
    }
}

/// Shadow, border and body of an overlay panel.
fn draw_overlay_frame(b: &mut Builder, l: &Layout, panel: Rect) {
    let theme = b.theme;
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
}

/// Search option chips (match case, whole word, regex), lit when on.
fn draw_chips(b: &mut Builder, l: &Layout, rects: &[Rect; 3], opts: SearchOptions) {
    let theme = b.theme;
    let on = [opts.case_sensitive, opts.whole_word, opts.regex];
    for ((r, label), on) in rects.iter().zip(CHIP_LABELS).zip(on) {
        let (bg, fg) = if on {
            (theme.accent, theme.accent_fg)
        } else {
            (theme.button_hover, theme.fg_dim)
        };
        b.rect(*r, bg);
        let x = r.x + (r.w - label.width() as f32 * l.cell_w) / 2.0;
        let y = r.y + (r.h - l.line_h) / 2.0;
        b.label(label, x, y, fg, *r);
    }
}

/// One input line of the find panel: label, text (or hint) and the cursor when active.
#[allow(clippy::too_many_arguments)]
fn draw_field(
    b: &mut Builder,
    l: &Layout,
    r: Rect,
    label: &str,
    text: &str,
    hint: &str,
    active: bool,
    cursor_on: bool,
) {
    let theme = b.theme;
    let pad = l.cell_w;
    let y = r.y + (r.h - l.line_h) / 2.0;
    let label_w = 7.0 * l.cell_w;
    b.label(
        label,
        r.x + pad,
        y,
        if active {
            theme.prompt_fg
        } else {
            theme.fg_dim
        },
        r,
    );
    let x = r.x + pad + label_w;
    let field = Rect::new(
        x - pad * 0.5,
        r.y + 2.0,
        r.right() - x - pad * 0.5,
        r.h - 4.0,
    );
    b.rect(field, if active { theme.bg } else { theme.overlay_bg });
    if active {
        b.rect(
            Rect::new(field.x, field.bottom() - 1.0, field.w, 1.0),
            theme.accent,
        );
    }
    if text.is_empty() {
        b.label(hint, x, y, theme.fg_dim, field);
    } else {
        b.label(text, x, y, theme.fg, field);
    }
    if active && cursor_on {
        let cx = x + text.width() as f32 * l.cell_w;
        b.rect(
            Rect::new(cx, y, (2.0 * l.scale).round().max(2.0), l.line_h),
            theme.cursor,
        );
    }
}

fn draw_find_panel(b: &mut Builder, ed: &Editor, l: &Layout, fl: &FindLayout, cursor_on: bool) {
    let theme = b.theme;
    let f = &ed.find;
    draw_overlay_frame(b, l, fl.panel);
    let pad = l.cell_w;

    // Title with the scope, and the option chips.
    b.rect(fl.title, theme.overlay_title_bg);
    let ty = fl.title.y + (fl.title.h - l.line_h) / 2.0;
    let title_clip = Rect::new(
        fl.title.x,
        fl.title.y,
        (fl.chips[0].x - fl.title.x - pad).max(0.0),
        fl.title.h,
    );
    b.label(
        format!("Find in Files   in {}", f.scope),
        fl.title.x + pad,
        ty,
        theme.tab_fg_active,
        title_clip,
    );
    draw_chips(b, l, &fl.chips, ed.search_opts);

    draw_field(
        b,
        l,
        fl.query,
        "Find:",
        &f.query,
        "text to search (Alt+C case, Alt+O word, Alt+R regex)",
        f.field == Field::Query,
        cursor_on,
    );
    draw_field(
        b,
        l,
        fl.include,
        "Files:",
        &f.include,
        "all files; e.g. *.js, .vue, src/**, !*.spec.js  (Tab)",
        f.field == Field::Include,
        cursor_on,
    );
    b.rect(
        Rect::new(fl.include.x, fl.include.bottom() - 1.0, fl.include.w, 1.0),
        theme.border,
    );

    // Results: file headers, then matching lines with the matches highlighted.
    let digits = f
        .rows
        .iter()
        .skip(f.scroll)
        .take(fl.rows)
        .filter_map(|r| match r {
            Row::Hit(i, j) => Some(f.results[*i].hits[*j].line + 1),
            Row::File(_) => None,
        })
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
        .max(3);
    for (row, (i, r)) in f
        .rows
        .iter()
        .enumerate()
        .skip(f.scroll)
        .take(fl.rows)
        .enumerate()
    {
        let y = fl.list.y + row as f32 * l.line_h;
        if i == f.selected {
            b.rect(
                Rect::new(fl.list.x, y, fl.list.w, l.line_h),
                theme.picker_selected,
            );
            b.rect(
                Rect::new(fl.list.x, y, (2.0 * l.scale).round().max(2.0), l.line_h),
                theme.accent,
            );
        }
        match *r {
            Row::File(fi) => {
                let file = &f.results[fi];
                let count = format!("({})", file.hits.len());
                let cw = count.width() as f32 * l.cell_w;
                let max_cols = ((fl.list.w - 4.0 * pad - cw) / l.cell_w).max(1.0) as usize;
                b.label(
                    truncate_left(&file.rel, max_cols),
                    fl.list.x + pad,
                    y,
                    theme.picker_dir,
                    fl.list,
                );
                b.label(
                    count,
                    fl.list.right() - 2.0 * pad - cw,
                    y,
                    theme.fg_dim,
                    fl.list,
                );
            }
            Row::Hit(fi, hi) => {
                let hit = &f.results[fi].hits[hi];
                let num = format!("{:>digits$}", hit.line + 1);
                let x_num = fl.list.x + 3.0 * pad;
                b.label(num, x_num, y, theme.gutter_fg, fl.list);
                let x = x_num + (digits + 2) as f32 * l.cell_w;
                let col_x = |c: usize| -> f32 {
                    let before: String = hit.preview.chars().take(c).collect();
                    x + before.width() as f32 * l.cell_w
                };
                for &(s, e) in &hit.preview_ranges {
                    let (x0, x1) = (col_x(s), col_x(e).min(fl.list.right()));
                    b.rect(Rect::new(x0, y, x1 - x0, l.line_h), theme.find_match);
                }
                b.label(hit.preview.clone(), x, y, theme.fg, fl.list);
            }
        }
    }

    // Scrollbar.
    if f.rows.len() > fl.rows {
        let w = (l.cell_w * 0.5).round();
        let h = (fl.rows as f32 / f.rows.len() as f32 * fl.list.h).max(l.line_h);
        let y = fl.list.y + f.scroll as f32 / f.rows.len() as f32 * fl.list.h;
        b.rect(Rect::new(fl.list.right() - w, y, w, h), theme.scrollbar);
    }

    // Footer: progress / totals / error.
    b.rect(
        Rect::new(fl.footer.x, fl.footer.y, fl.footer.w, 1.0),
        theme.border,
    );
    let fy = fl.footer.y + (fl.footer.h - l.line_h) / 2.0;
    let color = if f.error.is_some() {
        theme.error
    } else {
        theme.fg_dim
    };
    b.label(f.status(), fl.footer.x + pad, fy, color, fl.footer);
    let keys = "Enter open · F8 next · Esc close";
    let kw = keys.width() as f32 * l.cell_w;
    if fl.footer.w > kw + 50.0 * l.cell_w {
        b.label(
            keys,
            fl.footer.right() - pad - kw,
            fy,
            theme.fg_dim,
            fl.footer,
        );
    }
}

/// Centered hint shown in a view with no open tabs.
fn draw_empty_pane(b: &mut Builder, l: &Layout, pl: &PaneLayout) {
    let lines: &[&str] = if pl.pane == PANEL {
        &[
            "Panel",
            "",
            "F1         help",
            "F2         keyboard shortcuts",
            "Shift+F6   hide the panel",
        ]
    } else {
        &[
            "No file open",
            "",
            "Ctrl+O   open a file",
            "Ctrl+P   quick open",
            "Ctrl+N   new file",
            "F1       help",
        ]
    };
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
    scrollbar_hot: bool,
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
    // All selections in multi-cursor mode (just the primary one otherwise).
    let sels = doc.all_selections();
    let diags = doc
        .canonical
        .as_ref()
        .map_or(&[][..], |p| ed.problems.for_file(p));
    let test_marks = doc
        .canonical
        .as_ref()
        .map(|p| ed.test_marks(p))
        .unwrap_or_default();
    // Find highlights belong to the focused document only.
    let query = if focused { ed.active_query() } else { None };
    let body = Rect::new(pl.area.x, pl.text.y, pl.area.w, pl.text.h);

    b.rect(pl.gutter, theme.gutter_bg);

    // The failure box under the selected test on the Tests page, and the message box under
    // the selected problem on the Problems page (its left bar in the severity color).
    let detail_box = match doc.syntax.lang {
        crate::syntax::Lang::Tests => ed.tests_detail_lines().map(|(a, b)| (a, b, None)),
        crate::syntax::Lang::Problems => ed.problem_detail.map(|(a, b, sev)| {
            let token = match sev {
                crate::lint::Severity::Error => crate::syntax::Token::Error,
                crate::lint::Severity::Warning => crate::syntax::Token::Warning,
                crate::lint::Severity::Info => crate::syntax::Token::Function,
            };
            (a, b, Some(theme.token(token)))
        }),
        _ => None,
    };
    if let Some((first, last, accent)) = detail_box
        && last >= top
        && first <= top + pl.rows
    {
        let indent = rope
            .line(first)
            .chars()
            .take_while(|c| *c == ' ')
            .count()
            .saturating_sub(left);
        let x = pl.text.x + indent as f32 * l.cell_w - l.cell_w * 0.4;
        let y0 = pl.text.y + first.saturating_sub(top) as f32 * l.line_h;
        let y1 = pl.text.y + (last + 1).saturating_sub(top).min(pl.rows + 1) as f32 * l.line_h;
        let w = (pl.text.right() - x - l.cell_w).max(0.0);
        let border = l.scale.max(1.0).round();
        b.rect(
            Rect::new(
                x - border,
                y0 - border,
                w + 2.0 * border,
                y1 - y0 + 2.0 * border,
            ),
            theme.overlay_border,
        );
        b.rect(Rect::new(x, y0, w, y1 - y0), theme.overlay_bg);
        if let Some(accent) = accent {
            b.rect(
                Rect::new(
                    x - border,
                    y0 - border,
                    3.0 * border,
                    y1 - y0 + 2.0 * border,
                ),
                accent,
            );
        }
    }

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
            for (ms, me) in q.matches_in(&text) {
                let (s, e) = (ls + ms, ls + me);
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

        // Selections, including one cell for a selected newline.
        for (sel_s, sel_e) in sels.iter().map(|s| s.range()) {
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
        }

        // Lint problems: a gutter mark for the line's worst one, underlines under each.
        let severity_color = |s: Severity| match s {
            Severity::Error => theme.error,
            _ => theme.warning,
        };
        if let Some(worst) = diags
            .iter()
            .filter(|d| d.line == line)
            .map(|d| d.severity)
            .min()
        {
            let w = (3.0 * l.scale).round().max(2.0);
            b.rect(
                Rect::new(pl.gutter.x, y, w, l.line_h),
                severity_color(worst),
            );
        }
        for d in diags
            .iter()
            .filter(|d| d.line <= line && d.end_line >= line)
        {
            let len = le - ls;
            let s = if d.line == line { d.col.min(len) } else { 0 };
            let mut e = if d.end_line == line {
                d.end_col.min(len)
            } else {
                len
            };
            if e <= s {
                e = s + 1;
            }
            let x0 = col_x(ls + s).max(pl.text.x);
            let x1 = if ls + e <= le {
                col_x(ls + e)
            } else {
                col_x(le) + l.cell_w
            }
            .min(pl.text.right());
            let h = (2.0 * l.scale).round().max(1.0);
            if x1 > x0 {
                b.rect(
                    Rect::new(x0, y + l.line_h - h, x1 - x0, h),
                    severity_color(d.severity),
                );
            }
        }

        // Test status glyph, centered in the gutter's marks column.
        if let Some((_, status)) = test_marks.iter().find(|(l, _)| *l == line) {
            let color = match status {
                TestStatus::Passed => theme.token(Token::Pass),
                TestStatus::Failed => theme.error,
                TestStatus::Running => theme.accent,
                TestStatus::Skipped => theme.warning,
                TestStatus::NotRun | TestStatus::NotRunnable => theme.fg_dim,
            };
            let glyph = crate::editor::status_glyph(*status).to_string();
            let x = pl.gutter.x + (MARKS_COLS * l.cell_w - l.cell_w) / 2.0 + l.cell_w * 0.15;
            b.label(glyph, x, y, color, body);
        }

        if ed.config.line_numbers {
            let num = format!("{:>w$}", line + 1, w = pl.gutter_digits);
            let color = if line == cursor_line && focused {
                theme.gutter_fg_active
            } else {
                theme.gutter_fg
            };
            b.label(num, pl.gutter.x + MARKS_COLS * l.cell_w, y, color, body);
        }

        let spans = doc.syntax.line_spans(rope, line).unwrap_or_default();
        let (visible, colors) =
            display_spans(rope.line(line), left, pl.cols + 2, tab, &spans, |t| {
                theme.token(t)
            });
        if !visible.trim_end().is_empty() {
            b.colored_label(visible, colors, pl.text.x, y, theme.fg, pl.text);
        }

        if focused && cursor_on && ed.minibuffer.is_none() {
            for s in &sels {
                if rope.char_to_line(s.head) != line {
                    continue;
                }
                let x = col_x(s.head);
                if x >= pl.text.x - 1.0 && x <= pl.text.right() {
                    let w = (2.0 * l.scale).round().max(2.0);
                    b.rect(Rect::new(x, y, w, l.line_h), theme.cursor);
                }
            }
        }
    }

    // Scrollbar: brighter while hovered or dragged.
    if let Some(thumb) = pl.scroll_thumb(top, rope.len_lines()) {
        let color = if scrollbar_hot {
            theme.scrollbar_active
        } else {
            theme.scrollbar
        };
        b.rect(thumb, color);
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
        // Problems / Tests tabs: a colored status icon in front of the title.
        let doc = &ed.panes[pl.pane].docs[t.index];
        let spans: Vec<(std::ops::Range<usize>, Rgba)> = crate::editor::hover::tab_title(ed, doc)
            .spans
            .into_iter()
            .map(|(r, kind)| {
                use crate::editor::hover::HoverKind;
                let c = match kind {
                    HoverKind::Error => theme.error,
                    HoverKind::Warning => theme.warning,
                    HoverKind::Pass => theme.token(Token::Pass),
                    HoverKind::Text | HoverKind::Code | HoverKind::Param => theme.accent,
                    HoverKind::Dim => theme.fg_dim,
                };
                (r.start + 2..r.end + 2, c)
            })
            .collect();
        let text = format!("  {}", t.title);
        b.colored_label(text, spans, t.rect.x, text_y, color, bar);
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
    if ed.find.open {
        return "SEARCH";
    }
    if ed.picker.is_some() {
        return "PICK";
    }
    if ed.explorer.focused && ed.minibuffer.is_none() {
        return "EXPL";
    }
    match ed.minibuffer.as_ref().map(|m| &m.kind) {
        None if ed.has_doc() && ed.doc().read_only => "VIEW",
        None => "EDIT",
        Some(PromptKind::Open) => "OPEN",
        Some(PromptKind::SaveAs(_)) => "SAVE",
        Some(PromptKind::Find { .. }) => "FIND",
        Some(PromptKind::GotoLine) => "GOTO",
        Some(
            PromptKind::ConfirmClose { .. }
            | PromptKind::ConfirmDiscard(_)
            | PromptKind::ConfirmSaveWorkspace(_)
            | PromptKind::ConfirmRemoveFolder(_)
            | PromptKind::ConfirmOpenUrl { .. },
        ) => "CONFIRM",
        Some(PromptKind::WorkspaceName | PromptKind::SaveWorkspaceAs(_)) => "WORKSPACE",
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

    // Right side: selection, position, language, indent, encoding, line ending.
    let mut right = String::new();
    if let Some(doc) = doc {
        let head = doc.selection.head;
        let line = doc.rope.char_to_line(head);
        let col = mv::col_of(&doc.rope, head, ed.config.tab_width);
        if !doc.extra.is_empty() {
            right.push_str(&format!("{} cursors  ", doc.extra.len() + 1));
        } else if !doc.selection.is_empty() {
            let (s, e) = doc.selection.range();
            right.push_str(&format!("({} selected)  ", e - s));
        }
        right.push_str(&format!(
            "Ln {}, Col {}  {}  {}  UTF-8  {} ",
            line + 1,
            col + 1,
            doc.syntax.lang.name(),
            doc.indent.label(),
            doc.line_ending.label()
        ));
    }
    // Problem counts at the far right (click: Problems page).
    let mut right_end = l.width;
    if let Some(chip) = l.problems_chip {
        let (e, w) = ed.problems.counts();
        let mut cx = chip.x + l.cell_w;
        if e > 0 {
            let t = format!("✖ {e}");
            b.label(t.clone(), cx, y, theme.error, r);
            cx += (t.width() + 2) as f32 * l.cell_w;
        }
        if w > 0 {
            b.label(format!("⚠ {w}"), cx, y, theme.warning, r);
        }
        right_end = chip.x;
    }
    let right_w = right.width() as f32 * l.cell_w;
    let right_x = right_end - right_w - l.cell_w;
    b.label(right, right_x, y, theme.statusbar_fg, r);

    // Left side: file path, cut from the left so the file name stays visible.
    let mut x = chip_w;
    // Workspace chip: shows which workspace is open.
    if let Some(ws) = &ed.workspace {
        let chip = format!(" ⧉ {} ", ws.name);
        let w = chip.width() as f32 * l.cell_w;
        b.rect(Rect::new(x, r.y, w, r.h), theme.minibuffer_bg);
        b.label(chip, x, y, theme.tab_fg_active, r);
        x += w;
    }
    x += l.cell_w;
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
        None => ed
            .message
            .as_ref()
            .map(|msg| {
                let color = match msg.kind {
                    MsgKind::Info => theme.fg_dim,
                    MsgKind::Warn => theme.warning,
                    MsgKind::Error => theme.error,
                };
                (msg.text.clone(), color)
            })
            // Otherwise the problem on the cursor line, if any, or a failed test there.
            .or_else(|| {
                ed.cursor_diagnostic().map(|d| {
                    let color = match d.severity {
                        Severity::Error => theme.error,
                        _ => theme.warning,
                    };
                    let code = d.code.as_deref().unwrap_or(d.source.name());
                    (format!("{code}: {}", d.message), color)
                })
            })
            .or_else(|| ed.cursor_test_failure().map(|m| (m, theme.error)))
            // Otherwise: tests running.
            .or_else(|| {
                let running = ed.tests.tree.counts().3;
                (running > 0).then(|| {
                    (
                        format!(
                            "{} Running {running} test{}…",
                            crate::editor::spinner(),
                            if running == 1 { "" } else { "s" }
                        ),
                        theme.accent,
                    )
                })
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
    if matches!(mb.kind, PromptKind::Find { .. }) {
        draw_chips(b, l, &chip_rects(r, l.cell_w, l.line_h), ed.search_opts);
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
    fn display_spans_map_colors_through_tabs_and_scrolling() {
        use crate::syntax::{Span, Token};
        let r = Rope::from_str("\tab日cd\n");
        let red = Rgba(255, 0, 0, 255);
        let spans = [Span {
            start: 2,
            end: 5,
            token: Token::String,
        }]; // "b日c"
        let (text, ranges) = display_spans(r.line(0), 0, 80, 4, &spans, |_| red);
        assert_eq!(text, "    ab日cd");
        assert_eq!(&text[ranges[0].0.clone()], "b日c");
        let (text, ranges) = display_spans(r.line(0), 7, 80, 4, &spans, |_| red);
        assert_eq!(text, " cd"); // scrolled into the wide char
        assert_eq!(&text[ranges[0].0.clone()], " c");
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
