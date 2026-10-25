use std::sync::Arc;

use anyhow::{Context, Result, bail};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

/// Where frames go: a window surface, or an offscreen texture (headless mode).
pub enum Target {
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        window: Arc<Window>,
    },
    Offscreen {
        texture: wgpu::Texture,
    },
}

/// wgpu device + render target.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub target: Target,
}

pub enum Frame {
    Surface(wgpu::SurfaceTexture),
    Offscreen(wgpu::TextureView),
    /// Surface not usable right now (minimized, outdated...); try again on the next redraw.
    Skip,
}

/// Format of the headless render target.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

async fn open_device(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    software: bool,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            // An editor does not need the discrete GPU; save battery.
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: surface,
            force_fallback_adapter: software,
            ..Default::default()
        })
        .await
        .context("no suitable GPU adapter found")?;
    log::info!("GPU adapter: {:?}", adapter.get_info());
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("jdc device"),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(adapter.limits()),
            ..Default::default()
        })
        .await
        .context("cannot open GPU device")?;
    Ok((adapter, device, queue))
}

fn offscreen_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OFFSCREEN_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

impl Gpu {
    pub async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let surface = instance
            .create_surface(window.clone())
            .context("cannot create window surface")?;
        let (adapter, device, queue) = open_device(&instance, Some(&surface), false).await?;

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
            format,
            width: config.width,
            height: config.height,
            target: Target::Surface {
                surface,
                config,
                window,
            },
        })
    }

    /// GPU without a window, rendering into a texture that can be read back.
    pub async fn new_offscreen(width: u32, height: u32, software: bool) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let (_adapter, device, queue) = open_device(&instance, None, software).await?;
        let (width, height) = (width.max(1), height.max(1));
        let texture = offscreen_texture(&device, width, height);
        Ok(Self {
            instance,
            device,
            queue,
            format: OFFSCREEN_FORMAT,
            width,
            height,
            target: Target::Offscreen { texture },
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.width = width;
        self.height = height;
        match &mut self.target {
            Target::Surface {
                surface, config, ..
            } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            Target::Offscreen { texture } => {
                *texture = offscreen_texture(&self.device, width, height);
            }
        }
    }

    pub fn acquire(&mut self) -> Frame {
        let (surface, config, window) = match &mut self.target {
            Target::Offscreen { texture } => {
                return Frame::Offscreen(
                    texture.create_view(&wgpu::TextureViewDescriptor::default()),
                );
            }
            Target::Surface {
                surface,
                config,
                window,
            } => (surface, config, window),
        };
        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Frame::Surface(frame),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                Frame::Skip
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // The texture must be released before the surface is configured again.
                drop(frame);
                surface.configure(&self.device, config);
                Frame::Skip
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                surface.configure(&self.device, config);
                Frame::Skip
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                match self.instance.create_surface(window.clone()) {
                    Ok(s) => {
                        *surface = s;
                        surface.configure(&self.device, config);
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

    /// Read the offscreen target back as tightly packed RGBA8 rows.
    pub fn capture(&self) -> Result<(u32, u32, Vec<u8>)> {
        let Target::Offscreen { texture } = &self.target else {
            bail!("capture is only available in headless mode");
        };
        let (w, h) = (self.width, self.height);
        let row = w * 4;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture"),
            });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("GPU poll failed")?;
        rx.recv()
            .context("map callback dropped")?
            .context("cannot map capture buffer")?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| anyhow::anyhow!("cannot read capture buffer: {e:?}"))?;
        let mut pixels = Vec::with_capacity((row * h) as usize);
        for y in 0..h as usize {
            let start = y * padded as usize;
            pixels.extend_from_slice(&data[start..start + row as usize]);
        }
        drop(data);
        buffer.unmap();
        Ok((w, h, pixels))
    }
}
