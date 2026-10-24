use std::sync::Arc;

use anyhow::{Context, Result};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

/// wgpu device + window surface.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub window: Arc<Window>,
}

pub enum Frame {
    Ready(wgpu::SurfaceTexture),
    /// Surface not usable right now (minimized, outdated...); try again on the next redraw.
    Skip,
}

impl Gpu {
    pub async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let surface = instance
            .create_surface(window.clone())
            .context("cannot create window surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                // An editor does not need the discrete GPU; save battery.
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("no suitable GPU adapter found")?;
        log::info!("GPU adapter: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("jmx device"),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .context("cannot open GPU device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| caps.formats.first().copied())
            .context("surface reports no formats")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        Ok(Self {
            instance,
            device,
            queue,
            surface,
            config,
            window,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn acquire(&mut self) -> Frame {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Frame::Ready(frame),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                Frame::Skip
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.config);
                Frame::Skip
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                match self.instance.create_surface(self.window.clone()) {
                    Ok(s) => {
                        self.surface = s;
                        self.surface.configure(&self.device, &self.config);
                    }
                    Err(e) => log::error!("cannot recreate surface: {e}"),
                }
                Frame::Skip
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("surface validation error");
                Frame::Skip
            }
        }
    }
}
