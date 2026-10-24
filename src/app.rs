use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window, WindowId};

use crate::editor::{Editor, MESSAGE_TIMEOUT};
use crate::keymap;
use crate::render::{Renderer, frame};
use crate::theme;
use crate::ui::layout::{Layout, tab_rects, toolbar_buttons};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub struct App {
    editor: Editor,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    layout: Option<Layout>,
    modifiers: ModifiersState,
    focused: bool,
    blink_start: Instant,
    mouse: (f32, f32),
    /// Left button held after a single click in the text area.
    dragging: bool,
    last_click: Option<(Instant, (f32, f32))>,
    click_count: u32,
    /// Sub-line remainder of pixel-precise (touchpad) scrolling.
    scroll_accum: (f32, f32),
    cursor_icon: CursorIcon,
    /// Toolbar hover/press state.
    ui: frame::UiState,
}

impl App {
    pub fn new(editor: Editor) -> Self {
        Self {
            editor,
            window: None,
            renderer: None,
            layout: None,
            modifiers: ModifiersState::empty(),
            focused: true,
            blink_start: Instant::now(),
            mouse: (0.0, 0.0),
            dragging: false,
            last_click: None,
            click_count: 0,
            scroll_accum: (0.0, 0.0),
            cursor_icon: CursorIcon::Default,
            ui: frame::UiState::default(),
        }
    }

    fn scale(&self) -> f32 {
        self.window
            .as_ref()
            .map_or(1.0, |w| w.scale_factor() as f32)
    }

    /// Recompute layout from the window size and font metrics; tell the editor its viewport size.
    fn relayout(&mut self) {
        let scale = self.scale();
        let (Some(renderer), Some(window)) = (&mut self.renderer, &self.window) else {
            return;
        };
        let cfg = &self.editor.config;
        renderer
            .text
            .set_font(cfg.font_size * scale, cfg.line_height);
        let size = window.inner_size();
        let layout = Layout::compute(
            &self.editor,
            size.width as f32,
            size.height as f32,
            renderer.text.cell_w,
            renderer.text.line_h,
            scale,
        );
        self.editor.rows = layout.rows;
        self.editor.cols = layout.cols;
        if let Some(p) = &layout.picker {
            self.editor.picker_rows = p.rows;
        }
        self.layout = Some(layout);
    }

    /// Common tail of every input event.
    fn after_input(&mut self, event_loop: &ActiveEventLoop) {
        if self.editor.should_quit {
            event_loop.exit();
            return;
        }
        self.blink_start = Instant::now();
        self.relayout();
        if let Some(w) = &self.window {
            let doc = self.editor.doc();
            let dirty = if doc.is_dirty() { " ●" } else { "" };
            w.set_title(&format!("{}{dirty} - JMX", doc.title()));
            w.request_redraw();
        }
    }

    fn redraw(&mut self) {
        let started = Instant::now();
        self.relayout();
        let cursor_on = !self.focused || self.cursor_phase_on();
        let (Some(renderer), Some(layout)) = (&mut self.renderer, &self.layout) else {
            return;
        };
        let data = frame::build(
            &self.editor,
            layout,
            &theme::DARK,
            self.ui,
            cursor_on,
            renderer.linear,
        );
        if !renderer.draw(&data)
            && let Some(w) = &self.window
        {
            w.request_redraw();
        }
        log::debug!("frame in {:?}", started.elapsed());
    }

    fn blink_period(&self) -> Duration {
        Duration::from_millis(self.editor.config.cursor_blink_ms.max(1))
    }

    fn cursor_phase_on(&self) -> bool {
        let period = self.blink_period().as_millis();
        (self.blink_start.elapsed().as_millis() / period).is_multiple_of(2)
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        if self.focused {
            let period = self.blink_period();
            let elapsed = self.blink_start.elapsed();
            let n = elapsed.as_millis() / period.as_millis() + 1;
            next = Some(self.blink_start + period * n as u32);
        }
        if let Some(m) = &self.editor.message {
            let t = m.at + MESSAGE_TIMEOUT;
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        next
    }

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

    fn on_mouse_press(&mut self) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let (x, y) = self.mouse;
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
        if layout.tab_bar.contains(x, y) {
            if let Some(t) = tab_rects(&self.editor, &layout)
                .into_iter()
                .find(|t| t.rect.contains(x, y))
            {
                self.editor.active = t.index;
            }
            return;
        }
        let in_text = y >= layout.text.y && y < layout.text.bottom();
        if !in_text {
            return;
        }
        self.register_click();
        let (row, col) = layout.cell_at(x, y);
        let pos = self.editor.pos_at_cell(row, col);
        self.editor
            .click(pos, self.modifiers.shift_key(), self.click_count);
        self.dragging = self.click_count == 1;
    }

    fn on_mouse_release(&mut self) {
        self.dragging = false;
        if let Some(i) = self.ui.pressed_button.take()
            && self.button_at_mouse() == Some(i)
            && let Some(layout) = &self.layout
            && let Some(btn) = toolbar_buttons(layout).into_iter().nth(i)
        {
            self.editor.execute(btn.cmd);
        }
    }

    fn on_mouse_move(&mut self) {
        let hover = self.button_at_mouse();
        if hover != self.ui.hover_button {
            self.ui.hover_button = hover;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        let Some(layout) = &self.layout else {
            return;
        };
        let (x, y) = self.mouse;
        let over_picker = layout
            .picker
            .as_ref()
            .is_some_and(|p| p.panel.contains(x, y));
        let icon = if hover.is_some() {
            CursorIcon::Pointer
        } else if layout.text.contains(x, y) && !over_picker {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            if let Some(w) = &self.window {
                w.set_cursor(icon);
            }
        }
        if self.dragging {
            let (row, col) = layout.cell_at(x, y);
            let pos = self.editor.pos_at_cell(row, col);
            self.editor.drag_to(pos);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let line_h = self.layout.as_ref().map_or(20.0, |l| l.line_h);
        let cell_w = self.layout.as_ref().map_or(10.0, |l| l.cell_w);
        let (mut dx, mut dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * 3.0, y * 3.0),
            MouseScrollDelta::PixelDelta(p) => {
                self.scroll_accum.0 += p.x as f32 / cell_w;
                self.scroll_accum.1 += p.y as f32 / line_h;
                let d = (self.scroll_accum.0.trunc(), self.scroll_accum.1.trunc());
                self.scroll_accum.0 -= d.0;
                self.scroll_accum.1 -= d.1;
                d
            }
        };
        if self.editor.picker.is_some() {
            self.editor.picker_scroll(-dy as isize);
            return;
        }
        if self.modifiers.shift_key() {
            std::mem::swap(&mut dx, &mut dy);
        }
        if dy != 0.0 {
            self.editor.scroll(-dy as isize);
        }
        if dx != 0.0 {
            self.editor.scroll_horizontal(-dx as isize);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("JMX")
            .with_inner_size(LogicalSize::new(1100.0, 720.0))
            .with_min_inner_size(LogicalSize::new(320.0, 200.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("cannot create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let fonts = self.editor.config.font_families.clone();
        match pollster::block_on(Renderer::new(window.clone(), event_loop, &fonts)) {
            Ok(r) => self.renderer = Some(r),
            Err(e) => {
                log::error!("GPU initialization failed: {e:#}");
                eprintln!("jmx: GPU initialization failed: {e:#}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
        self.after_input(event_loop);
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.editor.expire_message();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.next_wakeup() {
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.editor.execute(crate::commands::Command::Quit);
                self.after_input(event_loop);
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                self.after_input(event_loop);
                self.editor.ensure_cursor_visible();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.after_input(event_loop),
            WindowEvent::Focused(f) => {
                self.focused = f;
                if !f {
                    self.dragging = false;
                }
                self.after_input(event_loop);
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if let Some(cmd) = keymap::lookup(self.modifiers, &event.logical_key) {
                    self.editor.execute(cmd);
                } else if let Some(text) = &event.text {
                    let ctrl = self.modifiers.control_key();
                    let alt = self.modifiers.alt_key();
                    // Plain typing, or AltGr (reported as Ctrl+Alt on Windows).
                    if ctrl == alt && !text.chars().any(char::is_control) {
                        self.editor.insert_text(text);
                    }
                }
                self.after_input(event_loop);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x as f32, position.y as f32);
                self.on_mouse_move();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                match state {
                    ElementState::Pressed => self.on_mouse_press(),
                    ElementState::Released => self.on_mouse_release(),
                }
                self.after_input(event_loop);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.ui.hover_button.take().is_some()
                    && let Some(w) = &self.window
                {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(delta);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}
