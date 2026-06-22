//! tinyporto driver — a generic wgpu host that executes a Wyn-compiled SPIR-V
//! frame-graph (`app::GRAPH`). No game concepts live here; see `graph.rs`.

mod app;
mod gfx;
mod graph;
mod wync;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseButton, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::WindowBuilder;

use gfx::Gfx;
use graph::*;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("driver crate has a parent dir")
        .to_path_buf()
}

#[derive(Parser, Debug)]
#[command(about = "Generic wgpu host for Wyn SPIR-V pipelines (tiny porto).")]
struct Args {
    #[arg(long, default_value_t = 1280)]
    width: u32,
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Skip running `wyn compile` at startup (use existing .spv).
    #[arg(long)]
    no_compile: bool,
    #[arg(long, default_value = "wyn/main.wyn")]
    wyn_root: String,
    #[arg(long, default_value = "shaders/main.spv")]
    out_spv: String,
    /// Render N frames then exit (headless smoke test). 0 = run forever.
    #[arg(long, default_value_t = 0)]
    frames: u32,
}

#[derive(Default, Clone, Copy)]
struct Input {
    mouse_x: f32,
    mouse_y: f32,
    held: bool,
}

// ---- built (concrete GPU) passes ----

struct BuiltCompute {
    pipeline: wgpu::ComputePipeline,
    sets: Vec<(u32, wgpu::BindGroup)>,
    groups: u32,
}

struct BuiltItem {
    pipeline: wgpu::RenderPipeline,
    sets: Vec<(u32, wgpu::BindGroup)>,
    draw: Draw,
}

struct BuiltRender {
    depth: Option<&'static str>,
    clear: [f64; 4],
    items: Vec<BuiltItem>,
}

enum BuiltPass {
    Compute(BuiltCompute),
    Render(BuiltRender),
}

struct Renderer {
    gfx: Gfx,
    buffers: HashMap<&'static str, wgpu::Buffer>,
    sys: Vec<(&'static str, SysUniform)>,
    depth_view: Option<wgpu::TextureView>,
    passes: Vec<BuiltPass>,
    start: Instant,
    frame: u32,
}

impl Renderer {
    fn new(gfx: Gfx, graph: &Graph) -> Result<Self> {
        let base = repo_root();
        let device = &gfx.device;

        let mut modules: HashMap<&str, wgpu::ShaderModule> = HashMap::new();
        for (key, rel) in graph.modules {
            modules.insert(key, wync::load_module(device, key, &base.join(rel))?);
        }

        // Resources.
        let mut buffers: HashMap<&'static str, wgpu::Buffer> = HashMap::new();
        let mut sys: Vec<(&'static str, SysUniform)> = Vec::new();
        let mut depth_name: Option<&'static str> = None;
        for res in graph.resources {
            match *res {
                Resource::SysUniform { name, kind } => {
                    buffers.insert(name, make_uniform(device, name));
                    sys.push((name, kind));
                }
                Resource::Buffer(def) => {
                    buffers.insert(def.name, make_storage(device, &gfx.queue, def));
                }
                Resource::Depth { name } => depth_name = Some(name),
            }
        }
        let depth_view = depth_name.map(|_| create_depth(device, gfx.config.width, gfx.config.height));

        // Passes (preserve order).
        let mut passes = Vec::new();
        for pass in graph.passes {
            match pass {
                Pass::Compute(cp) => {
                    let module = modules.get(cp.module).expect("module");
                    passes.push(BuiltPass::Compute(build_compute(device, module, cp, &buffers, graph)));
                }
                Pass::Render(rp) => {
                    let mut items = Vec::new();
                    for it in rp.items {
                        let module = modules.get(it.module).expect("module");
                        items.push(build_item(device, gfx.config.format, rp.depth.is_some(), module, it, &buffers));
                    }
                    passes.push(BuiltPass::Render(BuiltRender { depth: rp.depth, clear: rp.clear, items }));
                }
            }
        }

        Ok(Self { gfx, buffers, sys, depth_view, passes, start: Instant::now(), frame: 0 })
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
            let data: [f32; 4] = match kind {
                SysUniform::Resolution => [w, h, if h > 0.0 { w / h } else { 1.0 }, 0.0],
                SysUniform::Time => [t, 0.0, 0.0, 0.0],
                SysUniform::Mouse => [input.mouse_x, input.mouse_y, if input.held { 1.0 } else { 0.0 }, 0.0],
                SysUniform::Frame => [f32::from_bits(self.frame), 0.0, 0.0, 0.0],
            };
            q.write_buffer(&self.buffers[name], 0, bytemuck::cast_slice(&data));
        }
    }

    fn render(&mut self, input: &Input) -> Result<()> {
        self.update_uniforms(input);

        let frame = match self.gfx.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.gfx.surface.configure(&self.gfx.device, &self.gfx.config);
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("acquire frame: {e:?}")),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

        for pass in &self.passes {
            match pass {
                BuiltPass::Compute(c) => {
                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("compute"),
                        timestamp_writes: None,
                    });
                    cp.set_pipeline(&c.pipeline);
                    for (set, bg) in &c.sets {
                        cp.set_bind_group(*set, bg, &[]);
                    }
                    cp.dispatch_workgroups(c.groups, 1, 1);
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
                    let c = r.clear;
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("render"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color { r: c[0], g: c[1], b: c[2], a: c[3] }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: depth_attach,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    for it in &r.items {
                        rp.set_pipeline(&it.pipeline);
                        for (set, bg) in &it.sets {
                            rp.set_bind_group(*set, bg, &[]);
                        }
                        match it.draw {
                            Draw::Direct { vertices, instances } => rp.draw(0..vertices, 0..instances),
                            Draw::Indirect { args } => rp.draw_indirect(&self.buffers[args], 0),
                        }
                    }
                }
            }
        }

        self.gfx.queue.submit(Some(enc.finish()));
        frame.present();
        self.frame = self.frame.wrapping_add(1);
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

fn make_storage(device: &wgpu::Device, queue: &wgpu::Queue, def: BufferDef) -> wgpu::Buffer {
    let mut usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    if def.indirect {
        usage |= wgpu::BufferUsages::INDIRECT;
    }
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(def.name),
        size: def.size,
        usage,
        mapped_at_creation: false,
    });
    if let BufInit::Iota = def.init {
        let count = (def.size / 4) as u32;
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

// ---- bind groups (shared by compute + render) ----

fn buffer_size(graph: &Graph, name: &str) -> u64 {
    for r in graph.resources {
        if let Resource::Buffer(def) = r {
            if def.name == name {
                return def.size;
            }
        }
    }
    panic!("no buffer named '{name}'")
}

/// Build per-set bind-group layouts (0..=max, empty where unused) and bind
/// groups, from the graph's binding data.
fn build_sets(
    device: &wgpu::Device,
    label: &str,
    bindings: &[Binding],
    visibility: wgpu::ShaderStages,
    buffers: &HashMap<&'static str, wgpu::Buffer>,
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
                ty: match b.kind {
                    BindingKind::Uniform => wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    BindingKind::StorageRead | BindingKind::StorageWrite | BindingKind::StorageReadWrite => {
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage {
                                read_only: matches!(b.kind, BindingKind::StorageRead),
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        }
                    }
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let bg_entries: Vec<wgpu::BindGroupEntry> = in_set
            .iter()
            .map(|b| wgpu::BindGroupEntry {
                binding: b.binding,
                resource: buffers.get(b.resource).expect("resource").as_entire_binding(),
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
    buffers: &HashMap<&'static str, wgpu::Buffer>,
    graph: &Graph,
) -> BuiltCompute {
    let (layouts, sets) = build_sets(device, cp.label, cp.bindings, wgpu::ShaderStages::COMPUTE, buffers);
    let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(cp.label),
        bind_group_layouts: &layout_refs,
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(cp.label),
        layout: Some(&pl),
        module,
        entry_point: cp.entry,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });
    let groups = match cp.dispatch {
        Dispatch::FromBufferElems { buffer, elem_bytes, workgroup } => {
            let elems = (buffer_size(graph, buffer) / elem_bytes as u64) as u32;
            elems.div_ceil(workgroup)
        }
    };
    BuiltCompute { pipeline, sets, groups }
}

fn build_item(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    has_depth: bool,
    module: &wgpu::ShaderModule,
    it: &RenderItem,
    buffers: &HashMap<&'static str, wgpu::Buffer>,
) -> BuiltItem {
    let (layouts, sets) = build_sets(device, it.label, it.bindings, wgpu::ShaderStages::VERTEX_FRAGMENT, buffers);
    let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(it.label),
        bind_group_layouts: &layout_refs,
        push_constant_ranges: &[],
    });
    let depth_stencil = if has_depth {
        Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: it.depth_write,
            depth_compare: wgpu::CompareFunction::Less,
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
            entry_point: it.vs,
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: it.fs,
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });
    BuiltItem { pipeline, sets, draw: it.draw }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let base = repo_root();
    if !args.no_compile {
        wync::compile(&base.join(&args.wyn_root), &base.join(&args.out_spv))?;
    }

    let event_loop = EventLoop::new()?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("tiny porto")
            .with_inner_size(LogicalSize::new(args.width, args.height))
            .build(&event_loop)?,
    );
    let gfx = Gfx::new(window.clone())?;
    let mut renderer = Renderer::new(gfx, &app::GRAPH)?;
    let mut input = Input::default();

    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(sz) => renderer.resize(sz.width, sz.height),
                WindowEvent::CursorMoved { position, .. } => {
                    input.mouse_x = position.x as f32;
                    input.mouse_y = position.y as f32;
                }
                WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                    input.held = state == ElementState::Pressed;
                }
                WindowEvent::RedrawRequested => {
                    if let Err(e) = renderer.render(&input) {
                        eprintln!("render error: {e:?}");
                    }
                    if args.frames != 0 && renderer.frame >= args.frames {
                        println!("rendered {} frames; exiting (--frames)", renderer.frame);
                        elwt.exit();
                    }
                }
                _ => {}
            },
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    })?;
    Ok(())
}
