pub mod frame;
pub mod gpu;
pub mod quads;
pub mod text;

use std::sync::Arc;

use anyhow::Result;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use frame::FrameData;
use gpu::{Frame, Gpu};
use quads::QuadRenderer;
use text::TextSystem;

pub struct Renderer {
    pub gpu: Gpu,
    /// One quad batch per layer (editor, overlay).
    quads: [QuadRenderer; 2],
    pub text: TextSystem,
    /// Surface is sRGB, so quad colors are converted to linear.
    pub linear: bool,
}

impl Renderer {
    pub async fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        fonts: &[&str],
    ) -> Result<Self> {
        let gpu = Gpu::new(window, event_loop).await?;
        let format = gpu.config.format;
        let quads = [
            QuadRenderer::new(&gpu.device, format),
            QuadRenderer::new(&gpu.device, format),
        ];
        let text = TextSystem::new(&gpu.device, &gpu.queue, format, fonts);
        Ok(Self {
            gpu,
            quads,
            text,
            linear: format.is_srgb(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
    }

    /// Returns false if the frame was skipped and should be retried.
    pub fn draw(&mut self, frame: &FrameData) -> bool {
        let Frame::Ready(surface_tex) = self.gpu.acquire() else {
            return false;
        };
        let (w, h) = (self.gpu.config.width, self.gpu.config.height);
        let Gpu { device, queue, .. } = &self.gpu;
        for (quads, layer) in self.quads.iter_mut().zip(&frame.layers) {
            quads.prepare(device, queue, w as f32, h as f32, &layer.quads);
        }
        let text = [&frame.layers[0].text[..], &frame.layers[1].text[..]];
        if let Err(e) = self.text.prepare(device, queue, w, h, text) {
            log::error!("text prepare failed: {e}");
        }

        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Each layer's text is drawn over its own quads, and the overlay over everything.
            for (i, quads) in self.quads.iter().enumerate() {
                quads.render(&mut pass);
                if let Err(e) = self.text.render(i, &mut pass) {
                    log::error!("text render failed: {e}");
                }
            }
        }
        queue.submit(Some(encoder.finish()));
        queue.present(surface_tex);
        self.text.trim();
        true
    }
}
