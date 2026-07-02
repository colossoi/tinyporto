//! wgpu context: instance/surface/device/queue + surface configuration.
//! Generic — no knowledge of the graph or the game. `surface` is `None` in
//! headless mode (used by `--screenshot`, which renders to an offscreen texture).

use std::sync::Arc;
use anyhow::{Context, Result};
use winit::window::Window;

pub struct Gfx {
    pub surface: Option<wgpu::Surface<'static>>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
}

/// Request a device, raising the per-stage storage-buffer limit (the `step`
/// entry binds well past the default 8) to the adapter's maximum.
async fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue)> {
    let mut limits = wgpu::Limits::default();
    limits.max_storage_buffers_per_shader_stage =
        adapter.limits().max_storage_buffers_per_shader_stage;
    // The deferred `light` pass binds 5 storage textures (G-buffer albedo/normal,
    // scene depth, sun shadow map, lit output) — past the default 4. Raise to the
    // adapter's maximum, as with storage buffers above.
    limits.max_storage_textures_per_shader_stage =
        adapter.limits().max_storage_textures_per_shader_stage;
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("tinyporto-device"),
            required_features: wgpu::Features::empty(),
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

    async fn new_async(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).context("create_surface")?;

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

        Ok(Self { surface: Some(surface), device, queue, config })
    }

    /// Headless context (no window/surface) for offscreen rendering. `config`
    /// carries the offscreen format + size; sRGB so readback bytes are display-ready.
    pub fn new_headless(width: u32, height: u32) -> Result<Self> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
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
            Ok(Self { surface: None, device, queue, config })
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let (Some(surface), true) = (&self.surface, width > 0 && height > 0) {
            self.config.width = width;
            self.config.height = height;
            surface.configure(&self.device, &self.config);
        }
    }
}
