//! Window-independent editor session: input handling, layout and drawing.
//! Used by the winit app and by headless mode, so both run exactly the same code.

use std::time::{Duration, Instant};

use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::commands::Command;
use crate::editor::Editor;
use crate::keymap;
use crate::render::{Renderer, frame};
use crate::theme;
use crate::ui::explorer::{MAX_WIDTH_COLS, MIN_WIDTH_COLS};
use crate::ui::find_panel::Field;
use crate::ui::layout::{Layout, tab_rects, toolbar_buttons};
use crate::ui::menu::{MenuId, MenuLayout, menu_entries, menu_layout, next_item};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub struct Session {
    pub editor: Editor,
    pub layout: Option<Layout>,
    pub modifiers: ModifiersState,
    /// Ctrl+S was just pressed and Ctrl is still held: another S formats and saves.
    save_armed: bool,
    /// Target size in physical pixels and the display scale factor.
    size: (u32, u32),
    scale: f32,
    mouse: (f32, f32),
    /// Left button held after a single click in the text area.
    dragging: bool,
    /// Left button held on the line between the views.
    dragging_divider: bool,
    /// Left button held on the Explorer's right edge.
    dragging_sidebar: bool,
    /// Dragging a view's scrollbar thumb: (view, grab offset from the thumb's top).
    dragging_scrollbar: Option<(usize, f32)>,
    last_click: Option<(Instant, (f32, f32))>,
    click_count: u32,
    /// Sub-line remainder of pixel-precise (touchpad) scrolling.
    scroll_accum: (f32, f32),
    /// Toolbar, menu and divider hover/press state.
    ui: frame::UiState,
}

impl Session {
    pub fn new(editor: Editor, width: u32, height: u32, scale: f32) -> Self {
        Self {
            editor,
            layout: None,
            modifiers: ModifiersState::empty(),
            save_armed: false,
            size: (width, height),
            scale,
            mouse: (0.0, 0.0),
            dragging: false,
            dragging_divider: false,
            dragging_sidebar: false,
            dragging_scrollbar: None,
            last_click: None,
            click_count: 0,
            scroll_accum: (0.0, 0.0),
            ui: frame::UiState::default(),
        }
    }

    pub fn set_size(&mut self, width: u32, height: u32, scale: f32) {
        self.size = (width, height);
        self.scale = scale;
    }

    pub fn should_quit(&self) -> bool {
        self.editor.should_quit
    }

    /// Window title: file, workspace, app name.
    pub fn title(&self) -> String {
        let mut parts = Vec::new();
        if let Some(doc) = self.editor.pane().doc() {
            let dirty = if doc.is_dirty() { " ●" } else { "" };
            parts.push(format!("{}{dirty}", doc.title()));
        }
        if let Some(ws) = &self.editor.workspace {
            parts.push(ws.name.clone());
        }
        parts.push("JMX".to_string());
        parts.join(" - ")
    }

    pub fn open_menu_id(&self) -> Option<MenuId> {
        self.ui.menu
    }

    /// Recompute layout from the target size and font metrics; tell each view its size.
    pub fn relayout(&mut self, renderer: &mut Renderer) {
        self.editor.follow_active_file();
        let cfg = &self.editor.config;
        renderer
            .text
            .set_font(cfg.font_size * self.scale, cfg.line_height);
        let layout = Layout::compute(
            &self.editor,
            self.size.0 as f32,
            self.size.1 as f32,
            renderer.text.cell_w,
            renderer.text.line_h,
            self.scale,
        );
        for pl in &layout.panes {
            let pane = &mut self.editor.panes[pl.pane];
            pane.rows = pl.rows;
            pane.cols = pl.cols;
        }
        if let Some(p) = &layout.picker {
            self.editor.picker_rows = p.rows;
        }
        if let Some(f) = &layout.find {
            self.editor.find.visible_rows = f.rows;
        }
        if let Some(sb) = &layout.sidebar {
            self.editor.explorer.visible_rows = sb.rows;
        }
        self.layout = Some(layout);
        // Edges may have moved under (or away from) a still mouse, e.g. after Ctrl+F6.
        self.ui.divider_active = self.dragging_divider || self.over_divider();
        self.ui.sidebar_edge_active = self.dragging_sidebar || self.over_sidebar_edge();
    }

    /// Render one frame. Returns false if it was skipped and should be retried.
    pub fn draw(&mut self, renderer: &mut Renderer, cursor_on: bool) -> bool {
        let started = Instant::now();
        self.relayout(renderer);
        self.editor.update_syntax();
        let Some(layout) = &self.layout else {
            return false;
        };
        let data = frame::build(
            &self.editor,
            layout,
            &theme::DARK,
            self.ui,
            cursor_on,
            renderer.linear,
        );
        let drawn = renderer.draw(&data);
        log::debug!("frame in {:?}", started.elapsed());
        drawn
    }

    // ---------------------------------------------------------------- keyboard

    /// A key press: menu navigation, then shortcut lookup, otherwise typed text.
    /// Modifier keys changed. Releasing Ctrl ends a Ctrl+S, S gesture.
    pub fn set_modifiers(&mut self, state: ModifiersState) {
        if !state.control_key() {
            self.save_armed = false;
        }
        self.modifiers = state;
    }

    /// A key press: `key` as typed, `base` the same key without modifiers (layout-independent
    /// shortcuts are matched on it first).
    pub fn key(&mut self, key: &Key, base: &Key, text: Option<&str>) {
        let mut cmd =
            keymap::lookup(self.modifiers, base).or_else(|| keymap::lookup(self.modifiers, key));
        // Ctrl+S, S (Ctrl held): the second press formats and saves.
        if cmd == Some(Command::Save) {
            if self.save_armed {
                cmd = Some(Command::FormatAndSave);
            }
            self.save_armed = true;
        } else {
            self.save_armed = false;
        }
        log::trace!(
            "key {key:?} mods {:?} text {text:?} -> {cmd:?}",
            self.modifiers
        );
        if self.ui.menu.is_some() {
            if self.menu_key(key, cmd) {
                return;
            }
            // Any other key closes the menu and then does its normal job.
            self.close_menu();
        }
        if let Some(menu) = cmd.and_then(MenuId::for_command) {
            self.open_menu(menu, true);
            return;
        }
        if let Some(cmd) = cmd {
            self.editor.execute(cmd);
        } else if let Some(text) = text {
            let ctrl = self.modifiers.control_key();
            let alt = self.modifiers.alt_key();
            // Plain typing, or AltGr (reported as Ctrl+Alt on Windows).
            if ctrl == alt && !text.chars().any(char::is_control) {
                self.editor.insert_text(text);
            }
        }
    }

    // ---------------------------------------------------------------- menus

    /// Open a menu; from the keyboard the first item is highlighted.
    fn open_menu(&mut self, menu: MenuId, keyboard: bool) {
        self.ui.menu = Some(menu);
        self.ui.menu_hover = if keyboard {
            next_item(&menu_entries(menu, &self.editor), None, 1)
        } else {
            None
        };
    }

    fn close_menu(&mut self) {
        self.ui.menu = None;
        self.ui.menu_hover = None;
    }

    fn run_menu_item(&mut self, index: usize) {
        let Some(menu) = self.ui.menu else {
            return;
        };
        self.close_menu();
        let entries = menu_entries(menu, &self.editor);
        if let Some(cmd) = entries
            .get(index)
            .filter(|e| e.selectable())
            .and_then(|e| e.cmd)
        {
            log::debug!("menu item {cmd:?}");
            self.editor.execute(cmd);
        }
    }

    /// The open menu's geometry, under its toolbar button.
    fn menu_geometry(&self) -> Option<MenuLayout> {
        let menu = self.ui.menu?;
        let layout = self.layout.as_ref()?;
        let anchor = toolbar_buttons(layout, &self.editor)
            .into_iter()
            .find(|b| MenuId::for_command(b.cmd) == Some(menu))?
            .rect;
        Some(menu_layout(layout, &self.editor, menu, anchor))
    }

    /// Keys while a menu is open. Returns false if the key should close it and run normally.
    fn menu_key(&mut self, key: &Key, cmd: Option<Command>) -> bool {
        let Some(menu) = self.ui.menu else {
            return false;
        };
        let entries = menu_entries(menu, &self.editor);
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                self.ui.menu_hover = next_item(&entries, self.ui.menu_hover, 1);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.ui.menu_hover = next_item(&entries, self.ui.menu_hover, -1);
            }
            // Left/Right move between the menus of the bar.
            Key::Named(k @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) => {
                let all = MenuId::ALL;
                let i = all.iter().position(|&m| m == menu).unwrap_or(0);
                let step = if *k == NamedKey::ArrowRight {
                    1
                } else {
                    all.len() - 1
                };
                self.open_menu(all[(i + step) % all.len()], true);
            }
            Key::Named(NamedKey::Enter) => match self.ui.menu_hover {
                Some(i) => self.run_menu_item(i),
                None => self.close_menu(),
            },
            Key::Named(NamedKey::Escape) => self.close_menu(),
            _ => match cmd.and_then(MenuId::for_command) {
                Some(m) if m == menu => self.close_menu(),
                Some(m) => self.open_menu(m, true),
                None => return false,
            },
        }
        true
    }

    // ---------------------------------------------------------------- mouse

    /// Index of the toolbar button under the mouse.
    fn button_at_mouse(&self) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let (x, y) = self.mouse;
        if !layout.toolbar.contains(x, y) {
            return None;
        }
        toolbar_buttons(layout, &self.editor)
            .iter()
            .position(|b| b.rect.contains(x, y))
    }

    fn button_cmd(&self, index: usize) -> Option<Command> {
        let layout = self.layout.as_ref()?;
        toolbar_buttons(layout, &self.editor)
            .get(index)
            .map(|b| b.cmd)
    }

    fn over_divider(&self) -> bool {
        let (x, y) = self.mouse;
        self.layout
            .as_ref()
            .and_then(|l| l.divider)
            .is_some_and(|d| d.contains(x, y))
    }

    fn over_sidebar_edge(&self) -> bool {
        let (x, y) = self.mouse;
        self.layout
            .as_ref()
            .and_then(|l| l.sidebar.as_ref())
            .is_some_and(|s| s.edge.contains(x, y))
    }

    /// Count consecutive clicks at (about) the same spot.
    fn register_click(&mut self) -> u32 {
        let (x, y) = self.mouse;
        let now = Instant::now();
        self.click_count = match self.last_click {
            Some((t, (px, py)))
                if now.duration_since(t) < DOUBLE_CLICK
                    && (px - x).abs() < 4.0
                    && (py - y).abs() < 4.0 =>
            {
                self.click_count + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, (x, y)));
        self.click_count
    }

    pub fn mouse_press(&mut self) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let (x, y) = self.mouse;
        log::trace!("mouse press at {x},{y}");

        // An open menu takes the click: run the item, switch menus, or just close.
        if let Some(menu) = self.menu_geometry() {
            if let Some(i) = menu.item_at(x, y) {
                self.run_menu_item(i);
            } else if menu.panel.contains(x, y) {
                // Separator or disabled item: keep the menu open.
            } else if let Some(other) = self
                .button_at_mouse()
                .and_then(|i| self.button_cmd(i))
                .and_then(MenuId::for_command)
                .filter(|m| Some(*m) != self.ui.menu)
            {
                self.open_menu(other, false);
            } else {
                self.close_menu();
            }
            return;
        }
        // Problem counts in the status bar open the Problems page.
        if layout.problems_chip.is_some_and(|c| c.contains(x, y)) {
            self.editor.execute(Command::ShowProblems);
            return;
        }
        // Toolbar buttons act on release, like ordinary buttons.
        if layout.toolbar.contains(x, y) {
            self.ui.pressed_button = self.button_at_mouse();
            return;
        }
        if let Some(fl) = &layout.find {
            if fl.panel.contains(x, y) {
                if let Some(chip) = fl.chip_at(x, y) {
                    let cmd = [
                        Command::ToggleCase,
                        Command::ToggleWholeWord,
                        Command::ToggleRegex,
                    ][chip];
                    self.editor.execute(cmd);
                } else if fl.query.contains(x, y) {
                    self.editor.find_focus_field(Field::Query);
                } else if fl.include.contains(x, y) {
                    self.editor.find_focus_field(Field::Include);
                } else if let Some(row) = fl.row_at(x, y) {
                    let double = self.register_click() >= 2;
                    let index = self.editor.find.scroll + row;
                    self.editor.find_click(index, double);
                }
            } else {
                self.editor.find.open = false;
            }
            return;
        }
        if let Some(pl) = &layout.picker {
            if pl.panel.contains(x, y) {
                if let Some(row) = pl.row_at(x, y) {
                    let double = self.register_click() >= 2;
                    let index = self.editor.picker.as_ref().map_or(0, |p| p.scroll) + row;
                    self.editor.picker_click(index, double);
                }
            } else {
                self.editor.picker = None;
            }
            return;
        }
        if let Some(sb) = &layout.sidebar {
            if sb.edge.contains(x, y) {
                self.dragging_sidebar = true;
                self.ui.sidebar_edge_active = true;
                return;
            }
            if sb.area.contains(x, y) {
                if sb.add_button.is_some_and(|b| b.contains(x, y)) {
                    self.editor.execute(Command::AddFolder);
                } else if let Some(row) = sb.row_at(x, y) {
                    let index = self.editor.explorer.scroll + row;
                    if index < self.editor.explorer.rows.len() {
                        self.editor.explorer_click(index);
                    } else {
                        self.editor.explorer.focused = true;
                    }
                } else if self
                    .editor
                    .workspace
                    .as_ref()
                    .is_some_and(|w| w.folders.is_empty())
                    && sb.list.contains(x, y)
                {
                    // The "Add Folder…" hint in an empty workspace.
                    self.editor.execute(Command::AddFolder);
                } else {
                    self.editor.explorer.focused = true;
                }
                return;
            }
        }
        if layout.divider.is_some_and(|d| d.contains(x, y)) {
            if self.register_click() >= 2 {
                self.editor.split_ratio = 0.5;
            } else {
                self.dragging_divider = true;
                self.ui.divider_active = true;
            }
            return;
        }
        let Some(pl) = layout.pane_at(x, y) else {
            return;
        };
        // Scrollbar: drag the thumb, or jump to the click. Scrolling does not move focus.
        if pl.scrollbar.contains(x, y)
            && let Some(doc) = self.editor.panes[pl.pane].doc()
            && let Some(thumb) = pl.scroll_thumb(doc.view.top_line, doc.rope.len_lines())
        {
            let offset = if y >= thumb.y && y < thumb.bottom() {
                y - thumb.y
            } else {
                thumb.h / 2.0
            };
            self.dragging_scrollbar = Some((pl.pane, offset));
            self.ui.scrollbar_hot = Some(pl.pane);
            self.drag_scrollbar(y);
            return;
        }
        self.editor.explorer.focused = false;
        self.editor.focus_view(pl.pane);
        if pl.tab_bar.contains(x, y) {
            if let Some(t) = tab_rects(&self.editor, pl, layout.scale)
                .into_iter()
                .find(|t| t.rect.contains(x, y))
            {
                self.editor.panes[pl.pane].active = t.index;
            }
            return;
        }
        let in_text = pl.text.contains(x, y) || pl.gutter.contains(x, y);
        if !in_text || !self.editor.has_doc() {
            return;
        }
        self.register_click();
        let (row, col) = pl.cell_at(x, y);
        let pos = self.editor.pos_at_cell(row, col);
        self.editor
            .click(pos, self.modifiers.shift_key(), self.click_count);
        self.dragging = self.click_count == 1;
    }

    pub fn mouse_release(&mut self) {
        self.dragging = false;
        self.dragging_scrollbar = None;
        if self.dragging_divider {
            self.dragging_divider = false;
            self.ui.divider_active = self.over_divider();
        }
        if self.dragging_sidebar {
            self.dragging_sidebar = false;
            self.ui.sidebar_edge_active = self.over_sidebar_edge();
        }
        if let Some(i) = self.ui.pressed_button.take()
            && self.button_at_mouse() == Some(i)
            && let Some(cmd) = self.button_cmd(i)
        {
            log::debug!("toolbar button {cmd:?}");
            match MenuId::for_command(cmd) {
                Some(menu) => self.open_menu(menu, false),
                None => self.editor.execute(cmd),
            }
        }
    }

    /// Returns true if the frame needs redrawing.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.mouse = (x, y);
        let before = self.ui;
        self.ui.hover_button = self.button_at_mouse();
        if self.ui.menu.is_some() {
            // Sliding onto the other menu button switches menus, like a normal menu bar.
            if let Some(other) = self
                .ui
                .hover_button
                .and_then(|i| self.button_cmd(i))
                .and_then(MenuId::for_command)
                .filter(|m| Some(*m) != self.ui.menu)
            {
                self.open_menu(other, false);
            }
            if let Some(menu) = self.menu_geometry() {
                if let Some(i) = menu.item_at(x, y) {
                    self.ui.menu_hover = Some(i);
                } else if !menu.panel.contains(x, y) {
                    self.ui.menu_hover = None;
                }
            }
        }
        let mut redraw = false;
        if self.dragging_divider {
            if let Some(layout) = &self.layout {
                let ratio = layout.ratio_at(x);
                self.editor.set_split_ratio(ratio);
                redraw = true;
            }
        } else {
            self.ui.divider_active = self.over_divider();
        }
        if self.dragging_sidebar {
            if let Some(layout) = &self.layout {
                self.editor.explorer.width_cols = layout
                    .sidebar_cols_at(x)
                    .clamp(MIN_WIDTH_COLS, MAX_WIDTH_COLS);
                redraw = true;
            }
        } else {
            self.ui.sidebar_edge_active = self.over_sidebar_edge();
        }
        if self.dragging_scrollbar.is_some() {
            self.drag_scrollbar(y);
            return true;
        }
        // Hovering a scrollbar brightens its thumb.
        self.ui.scrollbar_hot = self.layout.as_ref().and_then(|l| {
            l.panes
                .iter()
                .find(|pl| pl.scrollbar.contains(x, y))
                .filter(|pl| {
                    self.editor.panes[pl.pane]
                        .doc()
                        .is_some_and(|d| d.rope.len_lines() > pl.rows)
                })
                .map(|pl| pl.pane)
        });
        // A text drag stays in the view where it started (the focused one).
        if self.dragging
            && let Some(pl) = self.layout.as_ref().and_then(|l| l.pane(self.editor.focus))
        {
            let (row, col) = pl.cell_at(x, y);
            let pos = self.editor.pos_at_cell(row, col);
            self.editor.drag_to(pos);
            redraw = true;
        }
        redraw || self.ui != before
    }

    /// Move the dragged scrollbar thumb so its grab point is at `y`.
    fn drag_scrollbar(&mut self, y: f32) {
        let Some((pane, offset)) = self.dragging_scrollbar else {
            return;
        };
        let Some(pl) = self.layout.as_ref().and_then(|l| l.pane(pane)) else {
            return;
        };
        let p = &mut self.editor.panes[pane];
        let i = p.active;
        if let Some(doc) = p.docs.get_mut(i) {
            doc.view.top_line = pl.top_for_thumb_y(y - offset, doc.rope.len_lines());
        }
    }

    /// Mouse left the window. Returns true if the frame needs redrawing.
    pub fn mouse_leave(&mut self) -> bool {
        let before = self.ui;
        self.ui.hover_button = None;
        if !self.dragging_divider {
            self.ui.divider_active = false;
        }
        if !self.dragging_sidebar {
            self.ui.sidebar_edge_active = false;
        }
        if self.dragging_scrollbar.is_none() {
            self.ui.scrollbar_hot = None;
        }
        self.ui != before
    }

    pub fn focus_lost(&mut self) {
        self.dragging = false;
        self.dragging_divider = false;
        self.dragging_sidebar = false;
    }

    /// Mouse cursor shape for the current position.
    pub fn cursor_icon(&self) -> CursorIcon {
        let Some(layout) = &self.layout else {
            return CursorIcon::Default;
        };
        let (x, y) = self.mouse;
        if let Some(menu) = self.menu_geometry()
            && menu.panel.contains(x, y)
        {
            return if menu.item_at(x, y).is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if self.ui.hover_button.is_some() || layout.problems_chip.is_some_and(|c| c.contains(x, y))
        {
            return CursorIcon::Pointer;
        }
        if self.dragging_divider
            || self.over_divider()
            || self.dragging_sidebar
            || self.over_sidebar_edge()
        {
            return CursorIcon::ColResize;
        }
        if self.ui.scrollbar_hot.is_some() && layout.find.is_none() && layout.picker.is_none() {
            return CursorIcon::Default;
        }
        if let Some(fl) = &layout.find
            && fl.panel.contains(x, y)
        {
            let over_row = fl
                .row_at(x, y)
                .is_some_and(|r| self.editor.find.scroll + r < self.editor.find.rows.len());
            return if fl.chip_at(x, y).is_some() || over_row {
                CursorIcon::Pointer
            } else if fl.query.contains(x, y) || fl.include.contains(x, y) {
                CursorIcon::Text
            } else {
                CursorIcon::Default
            };
        }
        let over_picker = layout
            .picker
            .as_ref()
            .is_some_and(|p| p.panel.contains(x, y));
        if over_picker {
            return CursorIcon::Default;
        }
        if let Some(sb) = &layout.sidebar
            && sb.area.contains(x, y)
        {
            let over_row = sb
                .row_at(x, y)
                .is_some_and(|r| self.editor.explorer.scroll + r < self.editor.explorer.rows.len());
            let over_button = sb.add_button.is_some_and(|b| b.contains(x, y));
            return if over_row || over_button {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        let over_text = layout
            .pane_at(x, y)
            .is_some_and(|pl| pl.text.contains(x, y) && self.editor.panes[pl.pane].doc().is_some());
        if over_text {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    /// Wheel in lines (mouse wheel notches already multiplied). Scrolls what is under the mouse.
    pub fn wheel_lines(&mut self, mut dx: f32, mut dy: f32) {
        if self.editor.find.open {
            self.editor.find.scroll_by(-dy as isize);
            return;
        }
        if self.editor.picker.is_some() {
            self.editor.picker_scroll(-dy as isize);
            return;
        }
        let (x, y) = self.mouse;
        if let Some(sb) = self.layout.as_ref().and_then(|l| l.sidebar.as_ref())
            && sb.area.contains(x, y)
        {
            self.editor.explorer.scroll_by(-dy as isize);
            return;
        }
        if self.modifiers.shift_key() {
            std::mem::swap(&mut dx, &mut dy);
        }
        let pane = self
            .layout
            .as_ref()
            .and_then(|l| l.pane_at(x, y))
            .map_or(self.editor.focus, |pl| pl.pane);
        if dy != 0.0 {
            self.editor.scroll_pane(pane, -dy as isize);
        }
        if dx != 0.0 {
            self.editor.scroll_pane_horizontal(pane, -dx as isize);
        }
    }

    /// Wheel in pixels (touchpads); accumulates until whole lines/columns.
    pub fn wheel_pixels(&mut self, px: f32, py: f32) {
        let line_h = self.layout.as_ref().map_or(20.0, |l| l.line_h);
        let cell_w = self.layout.as_ref().map_or(10.0, |l| l.cell_w);
        self.scroll_accum.0 += px / cell_w;
        self.scroll_accum.1 += py / line_h;
        let d = (self.scroll_accum.0.trunc(), self.scroll_accum.1.trunc());
        self.scroll_accum.0 -= d.0;
        self.scroll_accum.1 -= d.1;
        self.wheel_lines(d.0, d.1);
    }
}
