//! wgpu context: instance/surface/device/queue + surface configuration.
//! `surface` is `None` in headless mode (`--screenshot` renders offscreen).

use anyhow::{Context, Result};
use std::sync::Arc;
use winit::window::Window;

pub struct Gfx {
    pub surface: Option<wgpu::Surface<'static>>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
}

/// Request the storage limits needed by the generated frame function.
async fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue)> {
    let mut limits = wgpu::Limits::default();
    limits.max_storage_buffers_per_shader_stage =
        adapter.limits().max_storage_buffers_per_shader_stage;
    let timestamps =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("tinyporto-device"),
            required_features: if adapter.features().contains(timestamps) {
                timestamps
            } else {
                wgpu::Features::empty()
            },
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        })
        .await
        .context("request_device")
}

impl Gfx {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        pollster::block_on(Self::new_async(window))
    }

    pub fn present_loading_frame(&self) -> Result<()> {
        let surface = self
            .surface
            .as_ref()
            .context("loading frame needs a window")?;
        let frame = surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loading background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.12,
                            g: 0.15,
                            b: 0.18,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    async fn new_async(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        let surface = instance
            .create_surface(window.clone())
            .context("create_surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no suitable GPU adapter")?;
        let (device, queue) = request_device(&adapter).await?;

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("surface incompatible with adapter")?;
        // Prefer an sRGB surface so the GPU does the final linear->sRGB encode;
        // shaders then output linear color (no manual gamma).
        let caps = surface.get_capabilities(&adapter);
        if let Some(srgb) = caps.formats.iter().copied().find(|f| f.is_srgb()) {
            config.format = srgb;
        }
        surface.configure(&device, &config);

        Ok(Self {
            surface: Some(surface),
            device,
            queue,
            config,
        })
    }

    /// Headless context (no window/surface) for offscreen rendering. `config`
    /// carries the offscreen format + size; sRGB so readback bytes are display-ready.
    pub fn new_headless(width: u32, height: u32) -> Result<Self> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .context("no suitable GPU adapter")?;
            let (device, queue) = request_device(&adapter).await?;
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                width: width.max(1),
                height: height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };
            Ok(Self {
                surface: None,
                device,
                queue,
                config,
            })
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            if let Some(surface) = &self.surface {
                surface.configure(&self.device, &self.config);
            }
        }
    }
}
