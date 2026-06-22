//! tinyporto driver — a generic wgpu host that executes a Wyn-compiled SPIR-V
//! frame-graph (`app::GRAPH`). It knows nothing about the game; all domain logic
//! lives in the Wyn program. See `graph.rs` for the schema.

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
use graph::{Binding, BindingKind, Draw, Graph, Pass, RenderPass, Resource, SysUniform};

/// Repo root = parent of the driver crate dir. Resource paths in the graph are
/// relative to this, so the driver works regardless of the invocation CWD.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("driver crate has a parent dir")
        .to_path_buf()
}

#[derive(Parser, Debug)]
#[command(about = "Generic wgpu host for Wyn SPIR-V pipelines (tiny porto).")]
struct Args {
    /// Window width in logical pixels.
    #[arg(long, default_value_t = 1280)]
    width: u32,
    /// Window height in logical pixels.
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Skip running `wyn compile` at startup (use the existing .spv files).
    #[arg(long)]
    no_compile: bool,
    /// Wyn root file to compile (relative to repo root).
    #[arg(long, default_value = "wyn/main.wyn")]
    wyn_root: String,
    /// SPIR-V output for the root (relative to repo root).
    #[arg(long, default_value = "shaders/main.spv")]
    out_spv: String,
    /// Render this many frames then exit (headless smoke test). 0 = run forever.
    #[arg(long, default_value_t = 0)]
    frames: u32,
}

/// Per-frame input state fed into the system uniforms.
#[derive(Default, Clone, Copy)]
struct Input {
    mouse_x: f32,
    mouse_y: f32,
    held: bool,
}

/// A render pass built into concrete GPU objects.
struct BuiltRender {
    pipeline: wgpu::RenderPipeline,
    /// (set index, bind group) for every non-empty descriptor set.
    sets: Vec<(u32, wgpu::BindGroup)>,
    draw: Draw,
}

struct Renderer {
    gfx: Gfx,
    /// name -> uniform buffer (system uniforms; 16 bytes each).
    buffers: HashMap<&'static str, wgpu::Buffer>,
    /// system uniforms to refresh each frame.
    sys: Vec<(&'static str, SysUniform)>,
    passes: Vec<BuiltRender>,
    start: Instant,
    frame: u32,
}

impl Renderer {
    fn new(gfx: Gfx, graph: &Graph) -> Result<Self> {
        let base = repo_root();
        let device = &gfx.device;

        // Load each compiled module.
        let mut modules: HashMap<&str, wgpu::ShaderModule> = HashMap::new();
        for (key, rel) in graph.modules {
            modules.insert(key, wync::load_module(device, key, &base.join(rel))?);
        }

        // Allocate system-uniform buffers.
        let mut buffers: HashMap<&'static str, wgpu::Buffer> = HashMap::new();
        let mut sys: Vec<(&'static str, SysUniform)> = Vec::new();
        for res in graph.resources {
            match *res {
                Resource::SysUniform { name, kind } => {
                    buffers.insert(name, make_uniform(device, name));
                    sys.push((name, kind));
                }
            }
        }

        // Build passes.
        let mut passes = Vec::new();
        for pass in graph.passes {
            match pass {
                Pass::Render(rp) => {
                    let module = modules
                        .get(rp.module)
                        .unwrap_or_else(|| panic!("unknown module '{}'", rp.module));
                    passes.push(build_render(device, gfx.config.format, module, rp, &buffers));
                }
            }
        }

        Ok(Self { gfx, buffers, sys, passes, start: Instant::now(), frame: 0 })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.gfx.resize(w, h);
    }

    fn update_uniforms(&self, input: &Input) {
        let q = &self.gfx.queue;
        let w = self.gfx.config.width as f32;
        let h = self.gfx.config.height as f32;
        let t = self.start.elapsed().as_secs_f32();
        for (name, kind) in &self.sys {
            let buf = &self.buffers[name];
            let data: [f32; 4] = match kind {
                SysUniform::Resolution => [w, h, if h > 0.0 { w / h } else { 1.0 }, 0.0],
                SysUniform::Time => [t, 0.0, 0.0, 0.0],
                SysUniform::Mouse => {
                    [input.mouse_x, input.mouse_y, if input.held { 1.0 } else { 0.0 }, 0.0]
                }
                SysUniform::Frame => [f32::from_bits(self.frame), 0.0, 0.0, 0.0],
            };
            q.write_buffer(buf, 0, bytemuck::cast_slice(&data));
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
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.02, g: 0.03, b: 0.05, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            for pass in &self.passes {
                rp.set_pipeline(&pass.pipeline);
                for (set, bg) in &pass.sets {
                    rp.set_bind_group(*set, bg, &[]);
                }
                match pass.draw {
                    Draw::Fullscreen => rp.draw(0..3, 0..1),
                }
            }
        }
        self.gfx.queue.submit(Some(enc.finish()));
        frame.present();
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }
}

/// A 16-byte zeroed uniform buffer (covers vec4/vec3/scalar system uniforms).
fn make_uniform(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Build a render pass into a pipeline + per-set bind groups, with bind-group
/// layouts derived from the graph's binding data (no auto-layout guessing).
fn build_render(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    module: &wgpu::ShaderModule,
    rp: &RenderPass,
    buffers: &HashMap<&'static str, wgpu::Buffer>,
) -> BuiltRender {
    let max_set = rp.bindings.iter().map(|b| b.set).max().unwrap_or(0);

    let mut layouts: Vec<wgpu::BindGroupLayout> = Vec::new();
    let mut sets: Vec<(u32, wgpu::BindGroup)> = Vec::new();

    for set in 0..=max_set {
        let in_set: Vec<&Binding> = rp.bindings.iter().filter(|b| b.set == set).collect();

        let entries: Vec<wgpu::BindGroupLayoutEntry> = in_set
            .iter()
            .map(|b| wgpu::BindGroupLayoutEntry {
                binding: b.binding,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: match b.kind {
                    BindingKind::Uniform => wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                },
                count: None,
            })
            .collect();

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(rp.label),
            entries: &entries,
        });

        // Bind a group for EVERY set the pipeline layout declares — including
        // empty ones (e.g. an unused set 0 below a used set 1) — or wgpu rejects
        // the draw with "Incompatible bind group".
        let bg_entries: Vec<wgpu::BindGroupEntry> = in_set
            .iter()
            .map(|b| wgpu::BindGroupEntry {
                binding: b.binding,
                resource: buffers
                    .get(b.resource)
                    .unwrap_or_else(|| panic!("unknown resource '{}'", b.resource))
                    .as_entire_binding(),
            })
            .collect();
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(rp.label),
            layout: &layout,
            entries: &bg_entries,
        });
        sets.push((set, bg));
        layouts.push(layout);
    }

    let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(rp.label),
        bind_group_layouts: &layout_refs,
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(rp.label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: rp.vs,
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: rp.fs,
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    BuiltRender { pipeline, sets, draw: rp.draw }
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
