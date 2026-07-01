//! tinyporto driver — a generic wgpu host that executes a Wyn-compiled SPIR-V
//! frame-graph (`app::GRAPH`). No game concepts live here; see `graph.rs`.

mod app;
mod gfx;
mod graph;
mod wync;

/// Everything build.rs generates from the wyn descriptor: the embedded SPIR-V
/// table (`SHADER_MODULES`), the per-pipeline binding tables (`*_BINDINGS`), and
/// the dispatch/output-size calculations as `const fn`.
mod generated {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use gfx::Gfx;
use graph::*;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[derive(Parser, Debug)]
#[command(about = "Generic wgpu host for Wyn SPIR-V pipelines (tiny porto).")]
struct Args {
    #[arg(long, default_value_t = 1280)]
    width: u32,
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Render N frames then exit (headless smoke test). 0 = run forever.
    #[arg(long, default_value_t = 0)]
    frames: u32,
    /// Render a scripted scenario offscreen to this PNG and exit (no window).
    #[arg(long)]
    screenshot: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy)]
struct Input {
    mouse_x: f32,
    mouse_y: f32,
    held: bool,
    /// One-frame key pulses (set on keydown edge, cleared after each frame).
    tab_pulse: bool,
    line_pulse: bool,
    /// Accumulated scroll zoom in [0,1] (0 = far/top-down, 1 = near/flat).
    zoom: f32,
}

impl Default for Input {
    fn default() -> Self {
        Self { mouse_x: 0.0, mouse_y: 0.0, held: false, tab_pulse: false, line_pulse: false, zoom: 0.4 }
    }
}

// ---- built (concrete GPU) passes ----

// `sets` is indexed by frame parity (len 1 if the pass has no ping-pong
// binding, else 2). Each entry is the (set, bind group) list for that parity.
// `stages` are the ordered (pipeline, dispatch-dims) the entry lowered to; they
// share `sets` (one binding interface) and run sequentially.
struct BuiltCompute {
    stages: Vec<(wgpu::ComputePipeline, [u32; 3])>,
    sets: Vec<Vec<(u32, wgpu::BindGroup)>>,
}

struct BuiltItem {
    pipeline: wgpu::RenderPipeline,
    sets: Vec<Vec<(u32, wgpu::BindGroup)>>,
    draw_args: &'static str,
}

struct BuiltRender {
    depth: Option<&'static str>,
    color: &'static [ColorTarget],
    items: Vec<BuiltItem>,
}

enum BuiltPass {
    Compute(BuiltCompute),
    Render(BuiltRender),
}

struct Renderer {
    gfx: Gfx,
    buffers: HashMap<&'static str, wgpu::Buffer>,
    images: HashMap<&'static str, wgpu::Texture>,
    image_views: HashMap<&'static str, wgpu::TextureView>,
    samplers: HashMap<&'static str, wgpu::Sampler>,
    sys: Vec<(&'static str, SysUniform)>,
    depth_view: Option<wgpu::TextureView>,
    passes: Vec<BuiltPass>,
    start: Instant,
    frame: u32,
}

/// Borrowed bundle of the physical GPU resources a bind group resolves against —
/// storage/uniform buffers, ping-pong pairs, image views, and samplers.
#[derive(Clone, Copy)]
struct Res<'a> {
    buffers: &'a HashMap<&'static str, wgpu::Buffer>,
    pp: &'a HashMap<&'static str, [wgpu::Buffer; 2]>,
    views: &'a HashMap<&'static str, wgpu::TextureView>,
    samplers: &'a HashMap<&'static str, wgpu::Sampler>,
}

impl Renderer {
    fn new(gfx: Gfx, graph: &Graph) -> Result<Self> {
        let device = &gfx.device;

        let mut modules: HashMap<&str, wgpu::ShaderModule> = HashMap::new();
        for &(key, bytes) in generated::SHADER_MODULES {
            modules.insert(key, wync::load_module_bytes(device, key, bytes));
        }

        // Byte size of each buffer that is a compute output (resource name ->
        // bytes), from each compute pass's generated size calc applied to its
        // StorageWrite bindings. Buffers with no declared size are sized from here.
        let mut derived: HashMap<&'static str, u64> = HashMap::new();
        for pass in graph.passes {
            if let Pass::Compute(cp) = pass {
                for &(_, binding, kind, name) in cp.bindings {
                    if matches!(kind, BindingKind::StorageWrite | BindingKind::StorageReadWrite) {
                        derived.insert(name_to_resource(graph, name), (cp.out_bytes)(binding));
                    }
                }
            }
        }
        let size_of = |name: &'static str, declared: Option<u64>| -> u64 {
            declared.unwrap_or_else(|| *derived.get(name).unwrap_or_else(|| panic!("no size for '{name}'")))
        };

        // Resources.
        let mut buffers: HashMap<&'static str, wgpu::Buffer> = HashMap::new();
        let mut pingpong: HashMap<&'static str, [wgpu::Buffer; 2]> = HashMap::new();
        let mut images: HashMap<&'static str, wgpu::Texture> = HashMap::new();
        let mut image_views: HashMap<&'static str, wgpu::TextureView> = HashMap::new();
        let mut samplers: HashMap<&'static str, wgpu::Sampler> = HashMap::new();
        let mut sys: Vec<(&'static str, SysUniform)> = Vec::new();
        let mut has_depth = false;
        let (cw, ch) = (gfx.config.width, gfx.config.height);
        for res in graph.resources {
            match *res {
                Resource::SysUniform { name, kind } => {
                    buffers.insert(name, make_uniform(device, name));
                    sys.push((name, kind));
                }
                Resource::Buffer(def) => {
                    let buf = make_storage(device, &gfx.queue, def.name, size_of(def.name, def.size), def.init, def.indirect);
                    buffers.insert(def.name, buf);
                }
                Resource::PingPong { name, size } => {
                    let size = size_of(name, size);
                    let a = make_storage_raw(device, &format!("{name}#0"), size, false);
                    let b = make_storage_raw(device, &format!("{name}#1"), size, false);
                    pingpong.insert(name, [a, b]);
                }
                Resource::Image { name, format, size, mips } => {
                    let (w, h) = match size {
                        ImgSize::Window => (cw, ch),
                        ImgSize::Fixed { w, h } => (w, h),
                    };
                    let usage = image_usage(graph, name);
                    let tex = create_image(device, name, format, w, h, mips, usage);
                    image_views.insert(name, tex.create_view(&wgpu::TextureViewDescriptor::default()));
                    images.insert(name, tex);
                }
                Resource::Sampler { name } => {
                    samplers.insert(name, make_sampler(device, name));
                }
                Resource::Depth => has_depth = true,
            }
        }
        // Compiler-internal scratch: any sized write whose name no graph resource
        // claimed (e.g. a fused `filter`'s gather/count buffer). Auto-allocate it,
        // keyed by the binding name, so the pass can bind it.
        for (&name, &size) in &derived {
            if !buffers.contains_key(name) && !pingpong.contains_key(name) {
                buffers.insert(name, make_storage_raw(device, name, size, false));
            }
        }
        let depth_view =
            has_depth.then(|| create_depth(device, gfx.config.width, gfx.config.height));

        let res = Res { buffers: &buffers, pp: &pingpong, views: &image_views, samplers: &samplers };

        // Passes (preserve order).
        let mut passes = Vec::new();
        for pass in graph.passes {
            match pass {
                Pass::Compute(cp) => {
                    let module = modules.get(cp.module).expect("module");
                    passes.push(BuiltPass::Compute(build_compute(device, module, cp, res, graph)));
                }
                Pass::Render(rp) => {
                    let mut items = Vec::new();
                    for it in rp.items {
                        let module = modules.get(it.module).expect("module");
                        items.push(build_item(device, gfx.config.format, rp.color, rp.depth.is_some(), module, it, res, graph));
                    }
                    passes.push(BuiltPass::Render(BuiltRender { depth: rp.depth, color: rp.color, items }));
                }
            }
        }

        Ok(Self {
            gfx,
            buffers,
            images,
            image_views,
            samplers,
            sys,
            depth_view,
            passes,
            start: Instant::now(),
            frame: 0,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.gfx.resize(w, h);
        if self.depth_view.is_some() {
            self.depth_view = Some(create_depth(&self.gfx.device, self.gfx.config.width, self.gfx.config.height));
        }
    }

    fn update_uniforms(&self, input: &Input) {
        let q = &self.gfx.queue;
        let w = self.gfx.config.width as f32;
        let h = self.gfx.config.height as f32;
        let t = self.start.elapsed().as_secs_f32();
        for (name, kind) in &self.sys {
            let buf = &self.buffers[name];
            match kind {
                // Discrete key pulses are written as u32 (the shader reads vec4u32).
                SysUniform::Keys => {
                    let data: [u32; 4] = [input.tab_pulse as u32, input.line_pulse as u32, 0, 0];
                    q.write_buffer(buf, 0, bytemuck::cast_slice(&data));
                }
                _ => {
                    let data: [f32; 4] = match kind {
                        SysUniform::Resolution => [w, h, if h > 0.0 { w / h } else { 1.0 }, 0.0],
                        SysUniform::Time => [t, 0.0, 0.0, 0.0],
                        SysUniform::Mouse => {
                            [input.mouse_x, input.mouse_y, if input.held { 1.0 } else { 0.0 }, 0.0]
                        }
                        SysUniform::Frame => [f32::from_bits(self.frame), 0.0, 0.0, 0.0],
                        SysUniform::Cam => [input.zoom, 0.0, 0.0, 0.0],
                        SysUniform::Keys => unreachable!(),
                    };
                    q.write_buffer(buf, 0, bytemuck::cast_slice(&data));
                }
            }
        }
    }

    /// Record all passes for one frame into `enc`, drawing into `target`. Shared
    /// by the window path (`render`) and the offscreen path (`screenshot`).
    fn record(&self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let parity = (self.frame & 1) as usize;
        for pass in &self.passes {
            match pass {
                BuiltPass::Compute(c) => {
                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("compute"),
                        timestamp_writes: None,
                    });
                    let set_list = &c.sets[parity % c.sets.len()];
                    // Stages share the binding interface; each writes disjoint
                    // outputs from the shared inputs, so order is free — run them
                    // in sequence within the one pass.
                    for (pipeline, groups) in &c.stages {
                        cp.set_pipeline(pipeline);
                        for (set, bg) in set_list {
                            cp.set_bind_group(*set, bg, &[]);
                        }
                        cp.dispatch_workgroups(groups[0], groups[1], groups[2]);
                    }
                }
                BuiltPass::Render(r) => {
                    let depth_attach = if r.depth.is_some() {
                        self.depth_view.as_ref().map(|dv| wgpu::RenderPassDepthStencilAttachment {
                            view: dv,
                            depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                            stencil_ops: None,
                        })
                    } else {
                        None
                    };
                    // One attachment per declared color target, in location order:
                    // the surface (target: None) is the pass's `target` view, extra
                    // targets are graph image resources (e.g. the MRT depth buffer).
                    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = r
                        .color
                        .iter()
                        .map(|ct| {
                            let view = match ct.target {
                                None => target,
                                Some(name) => &self.image_views[name],
                            };
                            let c = ct.clear;
                            Some(wgpu::RenderPassColorAttachment {
                                view,
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color { r: c[0], g: c[1], b: c[2], a: c[3] }),
                                    store: wgpu::StoreOp::Store,
                                },
                            })
                        })
                        .collect();
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("render"),
                        color_attachments: &color_attachments,
                        depth_stencil_attachment: depth_attach,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    for it in &r.items {
                        rp.set_pipeline(&it.pipeline);
                        for (set, bg) in &it.sets[parity % it.sets.len()] {
                            rp.set_bind_group(*set, bg, &[]);
                        }
                        rp.draw_indirect(&self.buffers[it.draw_args], 0);
                    }
                }
            }
        }
    }

    fn render(&mut self, input: &Input) -> Result<()> {
        self.update_uniforms(input);
        let surface = self.gfx.surface.as_ref().expect("window mode has a surface");
        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                surface.configure(&self.gfx.device, &self.gfx.config);
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("acquire frame: {e:?}")),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        self.record(&mut enc, &view);
        self.gfx.queue.submit(Some(enc.finish()));
        frame.present();
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    /// Headless: render a scripted scenario into an offscreen texture and write it
    /// to `path` as a PNG. Used to eyeball the pipeline without a window.
    fn screenshot(&mut self, path: &std::path::Path) -> Result<()> {
        let (w, h) = (self.gfx.config.width, self.gfx.config.height);
        let tex = self.gfx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.gfx.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Script a water-stroke drag across the middle of the screen, then release,
        // so the shot exercises capture -> tessellation -> ribbon, not just the base.
        let total = 20u32;
        let mut frame_ms: Vec<f32> = Vec::new();
        for f in 0..total {
            let t = f as f32 / (total - 1).max(1) as f32;
            // A curved sweep (one sine arch) so the ribbon shows the spline curving.
            let input = Input {
                mouse_x: (0.25 + 0.50 * t) * w as f32,
                mouse_y: (0.5 - 0.18 * (t * std::f32::consts::PI).sin()) * h as f32,
                held: f + 4 < total, // release near the end
                ..Input::default()
            };
            self.update_uniforms(&input);
            let t0 = Instant::now();
            let mut enc = self
                .gfx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("offscreen") });
            self.record(&mut enc, &view);
            self.gfx.queue.submit(Some(enc.finish()));
            let _ = self.gfx.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            frame_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
            self.frame = self.frame.wrapping_add(1);
        }
        let mut steady: Vec<f32> = frame_ms.split_off(5);
        steady.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!("frame time: median {:.1} ms", steady[steady.len() / 2]);

        // Read the offscreen texture back (rows padded to 256 bytes).
        let bpp = 4u32;
        let unpadded = w * bpp;
        let padded = unpadded.div_ceil(256) * 256;
        let readback = self.gfx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("copy") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.gfx.queue.submit(Some(enc.finish()));

        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gfx.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let data = readback.slice(..).get_mapped_range();

        // Drop the row padding into a tight RGBA8 buffer.
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();

        let file = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&pixels)?;
        eprintln!("wrote {} ({w}x{h})", path.display());
        Ok(())
    }
}

// ---- resource creation ----

fn make_uniform(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn make_storage_raw(device: &wgpu::Device, label: &str, size: u64, indirect: bool) -> wgpu::Buffer {
    let mut usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    if indirect {
        usage |= wgpu::BufferUsages::INDIRECT;
    }
    device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
}

fn make_storage(device: &wgpu::Device, queue: &wgpu::Queue, name: &str, size: u64, init: BufInit, indirect: bool) -> wgpu::Buffer {
    let buf = make_storage_raw(device, name, size, indirect);
    if let BufInit::Iota = init {
        let count = (size / 4) as u32;
        let data: Vec<u32> = (0..count).collect();
        queue.write_buffer(&buf, 0, bytemuck::cast_slice(&data));
    }
    buf
}

fn create_depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn wgpu_format(f: TexFormat) -> wgpu::TextureFormat {
    match f {
        TexFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        TexFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        TexFormat::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
        TexFormat::R32Float => wgpu::TextureFormat::R32Float,
    }
}

/// Texture usage flags for image resource `name`: STORAGE_BINDING / TEXTURE_BINDING
/// per how the graph's bindings touch it, RENDER_ATTACHMENT if a render pass writes
/// it as a color target, plus COPY_DST/COPY_SRC (seed + readback).
fn image_usage(graph: &Graph, name: &str) -> wgpu::TextureUsages {
    let mut u = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;
    let touch = |t: BindTable, u: &mut wgpu::TextureUsages| {
        for &(_, _, kind, bname) in t {
            if name_to_resource(graph, bname) == name {
                match kind {
                    BindingKind::StorageImage { .. } => *u |= wgpu::TextureUsages::STORAGE_BINDING,
                    BindingKind::Texture => *u |= wgpu::TextureUsages::TEXTURE_BINDING,
                    _ => {}
                }
            }
        }
    };
    for pass in graph.passes {
        match pass {
            Pass::Compute(cp) => touch(cp.bindings, &mut u),
            Pass::Render(rp) => {
                if rp.color.iter().any(|ct| ct.target == Some(name)) {
                    u |= wgpu::TextureUsages::RENDER_ATTACHMENT;
                }
                for it in rp.items {
                    touch(it.vs_bindings, &mut u);
                    touch(it.fs_bindings, &mut u);
                }
            }
        }
    }
    u
}

fn create_image(
    device: &wgpu::Device,
    name: &str,
    format: TexFormat,
    w: u32,
    h: u32,
    mips: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(name),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: mips.max(1),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu_format(format),
        usage,
        view_formats: &[],
    })
}

fn make_sampler(device: &wgpu::Device, label: &str) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    })
}

// ---- bind groups (shared by compute + render) ----

/// Map a shader binding name to its resource name (via `graph.names`). An
/// unmapped name is a compiler-internal scratch buffer (e.g. a `filter`'s gather
/// buffer); it resolves to itself and is auto-allocated.
fn name_to_resource(graph: &Graph, binding_name: &'static str) -> &'static str {
    graph
        .names
        .iter()
        .find(|(n, _)| *n == binding_name)
        .map(|(_, r)| *r)
        .unwrap_or(binding_name)
}

/// Union of the storage-image accesses of `resource` across the whole graph. The
/// compiler emits one module global per image and gives it the union access (a
/// resource written by one entry and read by another is read-write), so every
/// pipeline's layout for that binding must use the union too — not the per-view
/// access the descriptor records.
fn image_union_access(graph: &Graph, resource: &str) -> ImgAccess {
    let (mut r, mut w) = (false, false);
    let mut scan = |t: BindTable| {
        for &(_, _, kind, bname) in t {
            if let BindingKind::StorageImage { access, .. } = kind {
                if name_to_resource(graph, bname) == resource {
                    match access {
                        ImgAccess::Read => r = true,
                        ImgAccess::Write => w = true,
                        ImgAccess::ReadWrite => {
                            r = true;
                            w = true;
                        }
                    }
                }
            }
        }
    };
    for pass in graph.passes {
        match pass {
            Pass::Compute(cp) => scan(cp.bindings),
            Pass::Render(rp) => {
                for it in rp.items {
                    scan(it.vs_bindings);
                    scan(it.fs_bindings);
                }
            }
        }
    }
    match (r, w) {
        (true, true) => ImgAccess::ReadWrite,
        (_, true) => ImgAccess::Write,
        _ => ImgAccess::Read,
    }
}

/// Resolve a generated binding table into driver `Binding`s: map each shader
/// binding name to a resource (via `graph.names`) and derive its role — a
/// ping-pong resource is read as Prev (StorageRead) / written as Next
/// (StorageWrite); everything else is Plain. Storage-image accesses are widened to
/// the resource's graph-wide union (see `image_union_access`).
fn resolve_table(
    table: BindTable,
    graph: &Graph,
    pp: &HashMap<&'static str, [wgpu::Buffer; 2]>,
) -> Vec<Binding> {
    table
        .iter()
        .map(|&(set, binding, kind, name)| {
            let resource = name_to_resource(graph, name);
            let kind = match kind {
                BindingKind::StorageImage { format, .. } => {
                    BindingKind::StorageImage { format, access: image_union_access(graph, resource) }
                }
                other => other,
            };
            let is_pp = pp.contains_key(resource);
            let role = match kind {
                BindingKind::StorageWrite if is_pp => Role::Next,
                BindingKind::StorageRead if is_pp => Role::Prev,
                _ => Role::Plain,
            };
            Binding { set, binding, resource, kind, role }
        })
        .collect()
}

/// Whether a resolved binding set needs both parities (has a ping-pong binding).
fn variant_count(bindings: &[Binding]) -> usize {
    if bindings.iter().any(|b| b.role != Role::Plain) { 2 } else { 1 }
}

/// Resolve a buffer binding to its physical buffer for `parity`. Ping-pong: this
/// frame's buffer is index `parity` (Next); last frame's is `1 - parity` (Prev).
/// Only called for buffer kinds; image/sampler kinds resolve via `res.views` /
/// `res.samplers`.
fn resolve<'a>(b: &Binding, parity: usize, res: Res<'a>) -> &'a wgpu::Buffer {
    match b.role {
        Role::Plain => res.buffers.get(b.resource).unwrap_or_else(|| panic!("no resource '{}'", b.resource)),
        Role::Next => &res.pp.get(b.resource).unwrap_or_else(|| panic!("no ping-pong '{}'", b.resource))[parity],
        Role::Prev => &res.pp.get(b.resource).unwrap_or_else(|| panic!("no ping-pong '{}'", b.resource))[1 - parity],
    }
}

/// The wgpu layout binding type for one resolved binding.
fn layout_type(kind: BindingKind) -> wgpu::BindingType {
    match kind {
        BindingKind::Uniform => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        BindingKind::StorageRead | BindingKind::StorageWrite | BindingKind::StorageReadWrite => {
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage {
                    read_only: matches!(kind, BindingKind::StorageRead),
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            }
        }
        BindingKind::Texture => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        BindingKind::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        BindingKind::StorageImage { format, access } => wgpu::BindingType::StorageTexture {
            access: match access {
                ImgAccess::Read => wgpu::StorageTextureAccess::ReadOnly,
                ImgAccess::Write => wgpu::StorageTextureAccess::WriteOnly,
                ImgAccess::ReadWrite => wgpu::StorageTextureAccess::ReadWrite,
            },
            format: wgpu_format(format),
            view_dimension: wgpu::TextureViewDimension::D2,
        },
    }
}

fn build_sets(
    device: &wgpu::Device,
    label: &str,
    bindings: &[Binding],
    visibility: wgpu::ShaderStages,
    res: Res,
    parity: usize,
) -> (Vec<wgpu::BindGroupLayout>, Vec<(u32, wgpu::BindGroup)>) {
    let max_set = bindings.iter().map(|b| b.set).max().unwrap_or(0);
    let mut layouts = Vec::new();
    let mut sets = Vec::new();

    for set in 0..=max_set {
        let in_set: Vec<&Binding> = bindings.iter().filter(|b| b.set == set).collect();
        let entries: Vec<wgpu::BindGroupLayoutEntry> = in_set
            .iter()
            .map(|b| wgpu::BindGroupLayoutEntry {
                binding: b.binding,
                visibility,
                ty: layout_type(b.kind),
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let bg_entries: Vec<wgpu::BindGroupEntry> = in_set
            .iter()
            .map(|&b| {
                let resource = match b.kind {
                    BindingKind::Texture | BindingKind::StorageImage { .. } => wgpu::BindingResource::TextureView(
                        res.views.get(b.resource).unwrap_or_else(|| panic!("no image view '{}'", b.resource)),
                    ),
                    BindingKind::Sampler => wgpu::BindingResource::Sampler(
                        res.samplers.get(b.resource).unwrap_or_else(|| panic!("no sampler '{}'", b.resource)),
                    ),
                    _ => resolve(b, parity, res).as_entire_binding(),
                };
                wgpu::BindGroupEntry { binding: b.binding, resource }
            })
            .collect();
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &bg_entries,
        });
        sets.push((set, bg));
        layouts.push(layout);
    }
    (layouts, sets)
}

fn build_compute(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    cp: &ComputePass,
    res: Res,
    graph: &Graph,
) -> BuiltCompute {
    let binds = resolve_table(cp.bindings, graph, res.pp);
    let (layouts, sets0) =
        build_sets(device, cp.label, &binds, wgpu::ShaderStages::COMPUTE, res, 0);
    let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(cp.label),
        bind_group_layouts: &layout_refs,
        push_constant_ranges: &[],
    });
    // One pipeline per ordered stage, all sharing the pass's layout (the full
    // binding interface; a stage that touches only some bindings is fine).
    let stages = cp
        .stages
        .iter()
        .map(|st| {
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(st.entry),
                layout: Some(&pl),
                module,
                entry_point: Some(st.entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
            (pipeline, st.groups)
        })
        .collect();
    let mut sets = vec![sets0];
    for p in 1..variant_count(&binds) {
        sets.push(build_sets(device, cp.label, &binds, wgpu::ShaderStages::COMPUTE, res, p).1);
    }
    BuiltCompute { stages, sets }
}

fn build_item(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    colors: &[ColorTarget],
    has_depth: bool,
    module: &wgpu::ShaderModule,
    it: &RenderItem,
    res: Res,
    graph: &Graph,
) -> BuiltItem {
    // Vertex + fragment share one pipeline layout: merge their binding tables,
    // deduping shared (set, binding) slots (e.g. iResolution/iCam in both stages).
    let mut binds = resolve_table(it.vs_bindings, graph, res.pp);
    for b in resolve_table(it.fs_bindings, graph, res.pp) {
        if !binds.iter().any(|x| x.set == b.set && x.binding == b.binding) {
            binds.push(b);
        }
    }
    let (layouts, sets0) =
        build_sets(device, it.label, &binds, wgpu::ShaderStages::VERTEX_FRAGMENT, res, 0);
    let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(it.label),
        bind_group_layouts: &layout_refs,
        push_constant_ranges: &[],
    });
    let depth_stencil = if has_depth {
        // Depth-writers test LessEqual: protruding geometry self-occludes, while
        // coplanar fragments at equal depth let the later draw win, preserving
        // painter order within the geometry stream. Non-writers test Always.
        Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: it.depth_write,
            depth_compare: if it.depth_write { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Always },
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        })
    } else {
        None
    };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(it.label),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(it.vs),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(it.fs),
            // One target per color attachment, in location order. The surface uses
            // the surface format; extra targets use their declared image format.
            targets: &colors
                .iter()
                .map(|ct| {
                    // The surface blends REPLACE; data targets (e.g. R32Float depth)
                    // aren't blendable, so no blend state.
                    Some(wgpu::ColorTargetState {
                        format: ct.format.map(wgpu_format).unwrap_or(surface_format),
                        blend: ct.format.is_none().then_some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })
                })
                .collect::<Vec<_>>(),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    let mut sets = vec![sets0];
    for p in 1..variant_count(&binds) {
        sets.push(build_sets(device, it.label, &binds, wgpu::ShaderStages::VERTEX_FRAGMENT, res, p).1);
    }
    BuiltItem { pipeline, sets, draw_args: it.draw_args }
}

// winit 0.30 drives the app through `ApplicationHandler`: the window (and thus
// the GPU surface) is created in `resumed`, events arrive in `window_event`, and
// `about_to_wait` keeps redraws flowing.
struct App {
    args: Args,
    input: Input,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("tiny porto")
            .with_inner_size(LogicalSize::new(self.args.width, self.args.height));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("create_window: {e}");
                event_loop.exit();
                return;
            }
        };
        let renderer = Gfx::new(window.clone()).and_then(|gfx| Renderer::new(gfx, &app::GRAPH));
        match renderer {
            Ok(r) => {
                self.window = Some(window);
                self.renderer = Some(r);
            }
            Err(e) => {
                eprintln!("gpu init: {e:?}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(sz) => renderer.resize(sz.width, sz.height),
            WindowEvent::CursorMoved { position, .. } => {
                self.input.mouse_x = position.x as f32;
                self.input.mouse_y = position.y as f32;
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                self.input.held = state == ElementState::Pressed;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                self.input.zoom = (self.input.zoom + dy * 0.08).clamp(0.0, 1.0);
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                // Edge-detect keydown (ignore auto-repeat); set a one-frame pulse.
                // The driver doesn't interpret these — Wyn's `ui` pass does.
                if key_event.state == ElementState::Pressed && !key_event.repeat {
                    match key_event.physical_key {
                        PhysicalKey::Code(KeyCode::Tab) => self.input.tab_pulse = true,
                        PhysicalKey::Code(KeyCode::KeyL) => self.input.line_pulse = true,
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = renderer.render(&self.input) {
                    eprintln!("render error: {e:?}");
                }
                // Pulses last exactly one rendered frame.
                self.input.tab_pulse = false;
                self.input.line_pulse = false;
                if self.args.frames != 0 && renderer.frame >= self.args.frames {
                    println!("rendered {} frames; exiting (--frames)", renderer.frame);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Headless: render a scripted scenario offscreen to a PNG and exit.
    if let Some(path) = args.screenshot.clone() {
        let gfx = Gfx::new_headless(args.width, args.height)?;
        let mut renderer = Renderer::new(gfx, &app::GRAPH)?;
        return renderer.screenshot(&path);
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App { args, input: Input::default(), window: None, renderer: None };
    event_loop.run_app(&mut app)?;
    Ok(())
}
