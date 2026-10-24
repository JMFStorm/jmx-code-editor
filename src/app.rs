//! winit shell: owns the window and turns window events into `Session` calls.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Icon, Window, WindowId};

use crate::commands::Command;
use crate::editor::{Editor, MESSAGE_TIMEOUT};
use crate::render::Renderer;
use crate::session::Session;

pub struct App {
    session: Session,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    focused: bool,
    blink_start: Instant,
    cursor_icon: CursorIcon,
    /// The window is hidden (minimized, covered): no frames until it shows again.
    occluded: bool,
    /// A frame could not be presented: try again at this time (not in a busy loop).
    retry_at: Option<Instant>,
    /// The cursor blink phase of the last frame drawn.
    drawn_phase: bool,
    /// The window title last set.
    title: String,
}

impl App {
    pub fn new(editor: Editor) -> Self {
        Self {
            session: Session::new(editor, 1, 1, 1.0),
            window: None,
            renderer: None,
            focused: true,
            occluded: false,
            retry_at: None,
            drawn_phase: true,
            title: String::new(),
            blink_start: Instant::now(),
            cursor_icon: CursorIcon::Default,
        }
    }

    fn sync_size(&mut self) {
        if let Some(w) = &self.window {
            let size = w.inner_size();
            self.session
                .set_size(size.width, size.height, w.scale_factor() as f32);
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Common tail of every input event.
    fn after_input(&mut self, event_loop: &ActiveEventLoop) {
        if self.session.should_quit() {
            log::info!("quit");
            event_loop.exit();
            return;
        }
        self.blink_start = Instant::now();
        self.sync_size();
        if let Some(r) = &mut self.renderer {
            self.session.relayout(r);
        }
        let title = self.session.title();
        if let Some(w) = &self.window {
            if title != self.title {
                w.set_title(&title);
                self.title = title;
            }
            w.request_redraw();
        }
    }

    fn update_cursor_icon(&mut self) {
        let icon = self.session.cursor_icon();
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            if let Some(w) = &self.window {
                w.set_cursor(icon);
            }
        }
    }

    fn blink_period(&self) -> Duration {
        Duration::from_millis(self.session.editor.config.cursor_blink_ms.max(1))
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
        if let Some(m) = &self.session.editor.message {
            let t = m.at + MESSAGE_TIMEOUT;
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        if let Some(t) = self.session.hover_deadline() {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        // Background search: debounce timer and result polling.
        if let Some(t) = self.session.editor.next_deadline() {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        if let Some(t) = self.retry_at {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        next
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("JDC")
            .with_inner_size(LogicalSize::new(1100.0, 720.0))
            .with_min_inner_size(LogicalSize::new(320.0, 200.0))
            .with_window_icon(window_icon());
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("cannot create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let fonts = self.session.editor.config.font_families.clone();
        match pollster::block_on(Renderer::new(window.clone(), event_loop, &fonts)) {
            Ok(r) => self.renderer = Some(r),
            Err(e) => {
                log::error!("GPU initialization failed: {e:#}");
                eprintln!("jdc: GPU initialization failed: {e:#}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
        self.after_input(event_loop);
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            // Redraw only when something visible changed: a message expired, background work
            // reported, a hover popup is due, the cursor blinked, or a frame must be retried.
            let mut changed = self.session.editor.expire_message();
            changed |= self.session.editor.poll_jobs();
            changed |= self.session.tick();
            if self.focused && self.cursor_phase_on() != self.drawn_phase {
                changed = true;
            }
            if self.retry_at.is_some_and(|t| Instant::now() >= t) {
                self.retry_at = None;
                changed = true;
            }
            if changed {
                self.request_redraw();
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
                self.session.editor.execute(Command::Quit);
                self.after_input(event_loop);
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                self.after_input(event_loop);
                self.session.editor.ensure_cursor_visible();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.after_input(event_loop),
            WindowEvent::Occluded(hidden) => {
                self.occluded = hidden;
                if !hidden {
                    self.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                self.focused = f;
                if !f {
                    self.session.focus_lost();
                }
                self.after_input(event_loop);
            }
            WindowEvent::ModifiersChanged(m) => self.session.set_modifiers(m.state()),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                // Shortcuts match the unshifted key, so Shift+Alt+. works on any layout.
                let base = event.key_without_modifiers();
                self.session
                    .key(&event.logical_key, &base, event.text.as_deref());
                self.after_input(event_loop);
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self
                    .session
                    .mouse_move(position.x as f32, position.y as f32)
                {
                    self.request_redraw();
                }
                self.update_cursor_icon();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                match state {
                    ElementState::Pressed => self.session.mouse_press(),
                    ElementState::Released => self.session.mouse_release(),
                }
                self.after_input(event_loop);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.session.mouse_leave() {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                match delta {
                    MouseScrollDelta::LineDelta(x, y) => self.session.wheel_lines(x * 3.0, y * 3.0),
                    MouseScrollDelta::PixelDelta(p) => {
                        self.session.wheel_pixels(p.x as f32, p.y as f32)
                    }
                }
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if self.occluded {
                    return;
                }
                self.sync_size();
                let cursor_on = !self.focused || self.cursor_phase_on();
                self.drawn_phase = cursor_on;
                if let Some(r) = &mut self.renderer {
                    if !self.session.draw(r, cursor_on) {
                        // Not presented (surface lost or busy): try again shortly.
                        self.retry_at = Some(Instant::now() + Duration::from_millis(50));
                    } else if self.session.editor.syntax_pending {
                        // Highlighting is spread over frames: continue with the next one.
                        self.request_redraw();
                    }
                }
            }
            _ => {}
        }
    }
}

/// The app icon for the title bar and taskbar, decoded from the embedded PNG.
fn window_icon() -> Option<Icon> {
    let decode = || -> anyhow::Result<Icon> {
        let bytes = include_bytes!("../assets/icon-64.png");
        let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes[..])).read_info()?;
        let mut buf = vec![0; reader.output_buffer_size().unwrap_or(64 * 64 * 4)];
        let info = reader.next_frame(&mut buf)?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            anyhow::bail!("icon must be 8-bit RGBA, not {:?}", info.color_type);
        }
        buf.truncate(info.buffer_size());
        Ok(Icon::from_rgba(buf, info.width, info.height)?)
    };
    decode()
        .map_err(|e| log::warn!("window icon unavailable: {e:#}"))
        .ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_icon_decodes() {
        assert!(super::window_icon().is_some());
    }
}
