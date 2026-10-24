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
use crate::ui::layout::{FILE_MENU, Layout, file_menu_layout, tab_rects, toolbar_buttons};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub struct Session {
    pub editor: Editor,
    pub layout: Option<Layout>,
    pub modifiers: ModifiersState,
    /// Target size in physical pixels and the display scale factor.
    size: (u32, u32),
    scale: f32,
    mouse: (f32, f32),
    /// Left button held after a single click in the text area.
    dragging: bool,
    /// Left button held on the line between the views.
    dragging_divider: bool,
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
            size: (width, height),
            scale,
            mouse: (0.0, 0.0),
            dragging: false,
            dragging_divider: false,
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

    pub fn title(&self) -> String {
        match self.editor.pane().doc() {
            Some(doc) => {
                let dirty = if doc.is_dirty() { " ●" } else { "" };
                format!("{}{dirty} - JMX", doc.title())
            }
            None => "JMX".to_string(),
        }
    }

    pub fn menu_open(&self) -> bool {
        self.ui.menu_open
    }

    /// Recompute layout from the target size and font metrics; tell each view its size.
    pub fn relayout(&mut self, renderer: &mut Renderer) {
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
        self.layout = Some(layout);
        // The divider may have moved under (or away from) a still mouse, e.g. after Ctrl+F6.
        self.ui.divider_active = self.dragging_divider || self.over_divider();
    }

    /// Render one frame. Returns false if it was skipped and should be retried.
    pub fn draw(&mut self, renderer: &mut Renderer, cursor_on: bool) -> bool {
        let started = Instant::now();
        self.relayout(renderer);
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
    pub fn key(&mut self, key: &Key, text: Option<&str>) {
        let cmd = keymap::lookup(self.modifiers, key);
        log::trace!(
            "key {key:?} mods {:?} text {text:?} -> {cmd:?}",
            self.modifiers
        );
        if self.ui.menu_open {
            if self.menu_key(key, cmd) {
                return;
            }
            // Any other key closes the menu and then does its normal job.
            self.close_menu();
        }
        if cmd == Some(Command::FileMenu) {
            self.open_menu(true);
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

    // ---------------------------------------------------------------- menu

    /// Open the File menu; from the keyboard the first item is highlighted.
    fn open_menu(&mut self, keyboard: bool) {
        self.ui.menu_open = true;
        self.ui.menu_hover = keyboard.then(|| next_menu_item(None, 1));
    }

    fn close_menu(&mut self) {
        self.ui.menu_open = false;
        self.ui.menu_hover = None;
    }

    fn run_menu_item(&mut self, index: usize) {
        self.close_menu();
        if let Some(Some((_, cmd))) = FILE_MENU.get(index) {
            log::debug!("menu item {cmd:?}");
            self.editor.execute(*cmd);
        }
    }

    /// Keys while the menu is open. Returns false if the key should close it and run normally.
    fn menu_key(&mut self, key: &Key, cmd: Option<Command>) -> bool {
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                self.ui.menu_hover = Some(next_menu_item(self.ui.menu_hover, 1));
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.ui.menu_hover = Some(next_menu_item(self.ui.menu_hover, -1));
            }
            Key::Named(NamedKey::Enter) => match self.ui.menu_hover {
                Some(i) => self.run_menu_item(i),
                None => self.close_menu(),
            },
            Key::Named(NamedKey::Escape) => self.close_menu(),
            _ if cmd == Some(Command::FileMenu) => self.close_menu(),
            _ => return false,
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
        toolbar_buttons(layout)
            .iter()
            .position(|b| b.rect.contains(x, y))
    }

    fn over_divider(&self) -> bool {
        let (x, y) = self.mouse;
        self.layout
            .as_ref()
            .and_then(|l| l.divider)
            .is_some_and(|d| d.contains(x, y))
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

        // An open menu takes the click: run the item, or just close the menu.
        if self.ui.menu_open {
            let menu = file_menu_layout(&layout);
            match menu.item_at(x, y) {
                Some(i) => self.run_menu_item(i),
                None if menu.panel.contains(x, y) => {}
                None => self.close_menu(),
            }
            return;
        }
        // Toolbar buttons act on release, like ordinary buttons.
        if layout.toolbar.contains(x, y) {
            self.ui.pressed_button = self.button_at_mouse();
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
        if self.dragging_divider {
            self.dragging_divider = false;
            self.ui.divider_active = self.over_divider();
        }
        if let Some(i) = self.ui.pressed_button.take()
            && self.button_at_mouse() == Some(i)
            && let Some(layout) = &self.layout
            && let Some(btn) = toolbar_buttons(layout).into_iter().nth(i)
        {
            log::debug!("toolbar button {:?}", btn.cmd);
            if btn.cmd == Command::FileMenu {
                self.open_menu(false);
            } else {
                self.editor.execute(btn.cmd);
            }
        }
    }

    /// Returns true if the frame needs redrawing.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.mouse = (x, y);
        let before = self.ui;
        self.ui.hover_button = self.button_at_mouse();
        if self.ui.menu_open
            && let Some(layout) = &self.layout
        {
            let menu = file_menu_layout(layout);
            if let Some(i) = menu.item_at(x, y) {
                self.ui.menu_hover = Some(i);
            } else if !menu.panel.contains(x, y) {
                self.ui.menu_hover = None;
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

    /// Mouse left the window. Returns true if the frame needs redrawing.
    pub fn mouse_leave(&mut self) -> bool {
        let before = self.ui;
        self.ui.hover_button = None;
        if !self.dragging_divider {
            self.ui.divider_active = false;
        }
        self.ui != before
    }

    pub fn focus_lost(&mut self) {
        self.dragging = false;
        self.dragging_divider = false;
    }

    /// Mouse cursor shape for the current position.
    pub fn cursor_icon(&self) -> CursorIcon {
        let Some(layout) = &self.layout else {
            return CursorIcon::Default;
        };
        let (x, y) = self.mouse;
        if self.ui.menu_open && file_menu_layout(layout).panel.contains(x, y) {
            return if self.ui.menu_hover.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if self.ui.hover_button.is_some() {
            return CursorIcon::Pointer;
        }
        if self.dragging_divider || self.over_divider() {
            return CursorIcon::ColResize;
        }
        let over_picker = layout
            .picker
            .as_ref()
            .is_some_and(|p| p.panel.contains(x, y));
        let over_text = layout
            .pane_at(x, y)
            .is_some_and(|pl| pl.text.contains(x, y) && self.editor.panes[pl.pane].doc().is_some());
        if over_text && !over_picker {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    /// Wheel in lines (mouse wheel notches already multiplied). Scrolls the view under the mouse.
    pub fn wheel_lines(&mut self, mut dx: f32, mut dy: f32) {
        if self.editor.picker.is_some() {
            self.editor.picker_scroll(-dy as isize);
            return;
        }
        if self.modifiers.shift_key() {
            std::mem::swap(&mut dx, &mut dy);
        }
        let (x, y) = self.mouse;
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

/// Next selectable File menu row from `from` in direction `dir` (wrapping, skipping separators).
fn next_menu_item(from: Option<usize>, dir: isize) -> usize {
    let n = FILE_MENU.len() as isize;
    let mut i = from.map_or(if dir > 0 { -1 } else { n }, |i| i as isize);
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if FILE_MENU[i as usize].is_some() {
            return i as usize;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_navigation_skips_separator() {
        assert_eq!(next_menu_item(None, 1), 0);
        assert_eq!(next_menu_item(None, -1), FILE_MENU.len() - 1);
        assert_eq!(next_menu_item(Some(5), 1), 7); // 6 is the separator
        assert_eq!(next_menu_item(Some(7), -1), 5);
        assert_eq!(next_menu_item(Some(7), 1), 0);
    }
}
