//! Text rendering via glyphon. Each distinct visible string is shaped once into a
//! `glyphon::Buffer` and cached; buffers not used in a frame are dropped.

use std::collections::HashMap;

use anyhow::Result;
use glyphon::{
    Attrs, Buffer, Cache, ColorMode, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};

use crate::theme::Rgba;
use crate::ui::layout::Rect;

pub struct TextItem {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Rgba,
    pub clip: Rect,
}

pub struct TextSystem {
    font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    /// One renderer per layer (editor, overlay), sharing the atlas and shape cache.
    renderers: Vec<TextRenderer>,
    cache: HashMap<String, Buffer>,
    metrics: Metrics,
    font_px: f32,
    pub cell_w: f32,
    pub line_h: f32,
}

impl TextSystem {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        families: &[&str],
    ) -> Self {
        let mut font_system = FontSystem::new();
        let available = families.iter().find(|name| {
            font_system
                .db()
                .faces()
                .any(|f| f.families.iter().any(|(n, _)| n == *name))
        });
        match available {
            Some(name) => {
                log::info!("using font {name}");
                font_system.db_mut().set_monospace_family(*name);
            }
            None => log::warn!("none of the preferred fonts found; using system monospace"),
        }
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let color_mode = if format.is_srgb() {
            ColorMode::Accurate
        } else {
            ColorMode::Web
        };
        let mut atlas = TextAtlas::with_color_mode(device, queue, &cache, format, color_mode);
        let renderers = (0..2)
            .map(|_| TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None))
            .collect();
        Self {
            font_system,
            swash: SwashCache::new(),
            viewport,
            atlas,
            renderers,
            cache: HashMap::new(),
            metrics: Metrics::new(1.0, 1.0),
            font_px: 0.0,
            cell_w: 1.0,
            line_h: 1.0,
        }
    }

    /// Set font size in physical pixels; re-measures the cell size when it changes.
    pub fn set_font(&mut self, font_px: f32, line_height: f32) {
        if (font_px - self.font_px).abs() < f32::EPSILON {
            return;
        }
        self.font_px = font_px;
        self.line_h = (font_px * line_height).round();
        self.metrics = Metrics::new(font_px, self.line_h);
        self.cache.clear();
        let probe = self.shape("M");
        self.cell_w = probe
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| g.w))
            .filter(|w| *w > 0.0)
            .unwrap_or(font_px * 0.6);
    }

    fn shape(&mut self, text: &str) -> Buffer {
        let mut buf = Buffer::new(&mut self.font_system, self.metrics);
        buf.set_wrap(Wrap::None);
        buf.set_size(None, Some(self.line_h));
        if self.cell_w > 1.0 {
            buf.set_monospace_width(Some(self.cell_w));
        }
        buf.set_text(
            text,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.font_system, false);
        buf
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        layers: [&[TextItem]; 2],
    ) -> Result<()> {
        self.viewport.update(queue, Resolution { width, height });

        // Move buffers still in use into a fresh map; shape the new ones.
        let mut old = std::mem::take(&mut self.cache);
        for item in layers.iter().flat_map(|l| l.iter()) {
            if item.text.is_empty() || self.cache.contains_key(&item.text) {
                continue;
            }
            let buf = match old.remove(&item.text) {
                Some(b) => b,
                None => self.shape(&item.text),
            };
            self.cache.insert(item.text.clone(), buf);
        }
        drop(old);

        for (renderer, items) in self.renderers.iter_mut().zip(layers) {
            let cache = &self.cache;
            let areas = items.iter().filter_map(|item| {
                let buffer = cache.get(&item.text)?;
                Some(TextArea {
                    buffer,
                    left: item.x,
                    top: item.y,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: item.clip.x.floor() as i32,
                        top: item.clip.y.floor() as i32,
                        right: item.clip.right().ceil() as i32,
                        bottom: item.clip.bottom().ceil() as i32,
                    },
                    default_color: item.color.glyphon(),
                    custom_glyphs: &[],
                })
            });
            renderer.prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            )?;
        }
        Ok(())
    }

    pub fn render(&self, layer: usize, pass: &mut wgpu::RenderPass<'_>) -> Result<()> {
        self.renderers[layer].render(&self.atlas, &self.viewport, pass)?;
        Ok(())
    }

    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}
