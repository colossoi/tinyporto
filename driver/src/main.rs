//! tinyporto driver — a generic wgpu host that executes a Wyn-compiled SPIR-V
//! frame-graph (`app::graph`). No game concepts live here; see `graph.rs`.

mod app;
mod camera;
mod gfx;
mod graph;
mod wync;

/// Everything build.rs generates from the wyn descriptor: the embedded SPIR-V
/// table (`SHADER_MODULES`), the per-pipeline binding tables (`*_BINDINGS`), and
/// the dispatch/output-size calculations as `const fn`.
mod generated {
    #![allow(dead_code, non_snake_case)]
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use camera::Camera;
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
    /// Screenshot orbit camera: eye distance from the target.
    #[arg(long, default_value_t = 45.0)]
    cam_dist: f32,
    /// Screenshot orbit camera: elevation / pitch in radians (negative looks down).
    #[arg(long, default_value_t = -0.35)]
    cam_elev: f32,
    /// Screenshot orbit camera: azimuth in radians.
    #[arg(long, default_value_t = 0.6)]
    cam_az: f32,
    /// Live modifier mask for the screenshot scenario (bit0 shift, 1 ctrl, 2 alt,
    /// 3 super) — exercises modifier-gated features headless (e.g. Ctrl = AO off).
    #[arg(long, default_value_t = 0)]
    mods: u32,
    /// Screenshot scene time (seconds) fed to `frame.time` — animates time-varying
    /// effects (e.g. the water) for a still. The window path uses real elapsed time.
    #[arg(long, default_value_t = 0.0)]
    time: f32,
    /// After a screenshot render, read these storage buffers back and print their
    /// contents as u32 words (debug aid for compute outputs / compiler scratch).
    #[arg(long, value_delimiter = ',')]
    dump: Vec<String>,
}

// ---- built (concrete GPU) passes ----

// Each physical stage has its own exact descriptor interface. `sets` is indexed
// by frame parity (len 1 if the stage has no ping-pong binding, else 2).
struct BuiltComputeStage {
    pipeline: wgpu::ComputePipeline,
    groups: [u32; 3],
    sets: Vec<Vec<(u32, wgpu::BindGroup)>>,
}

struct BuiltCompute {
    label: &'static str,
    stages: Vec<BuiltComputeStage>,
}

struct BuiltItem {
    pipeline: wgpu::RenderPipeline,
    sets: Vec<Vec<(u32, wgpu::BindGroup)>>,
    draw: Draw,
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
    image_views: HashMap<&'static str, wgpu::TextureView>,
    /// Uniform blocks to fill each frame: (buffer name, members, std140 layout).
    blocks: Vec<(
        &'static str,
        &'static [BlockMember],
        &'static UniformBlockLayout,
    )>,
    depth_view: Option<wgpu::TextureView>,
    passes: Vec<BuiltPass>,
    graph: Graph,
    pingpong: HashMap<&'static str, [wgpu::Buffer; 2]>,
    img_formats: HashMap<&'static str, TexFormat>,
    output_sizes: Vec<(ComputePass, u32, &'static str)>,
    frame: u32,
    start: Instant,
}

/// Borrowed bundle of the physical GPU resources a bind group resolves against —
/// storage/uniform buffers, ping-pong pairs, and image views.
#[derive(Clone, Copy)]
struct Res<'a> {
    buffers: &'a HashMap<&'static str, wgpu::Buffer>,
    pp: &'a HashMap<&'static str, [wgpu::Buffer; 2]>,
    views: &'a HashMap<&'static str, wgpu::TextureView>,
    /// On-GPU pixel format of each image resource, keyed by name — lets a sampled
    /// `texture2d` binding declare the correct `filterable` sample type.
    img_formats: &'a HashMap<&'static str, TexFormat>,
}

fn uniform_word(blocks: &HashMap<&str, Vec<u8>>, name: &str, offset: u32) -> Option<u32> {
    let bytes = blocks.get(name)?;
    let start = offset as usize;
    Some(u32::from_le_bytes(
        bytes.get(start..start.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn pack_uniform_block(
    members: &[BlockMember],
    layout: &UniformBlockLayout,
    width: u32,
    height: u32,
    cam: &Camera,
    mods: u32,
    time: f32,
) -> Vec<u8> {
    let (w, h) = (width as f32, height as f32);
    let name = layout.name;
    let mut bytes = vec![0u8; layout.size as usize];
    for m in members.iter() {
        let (_, offset, _) = layout
            .members
            .iter()
            .find(|(f, _, _)| *f == m.field)
            .unwrap_or_else(|| panic!("block '{name}' has no member '{}'", m.field));
        let off = *offset as usize;
        match m.source {
            FrameSource::Resolution => {
                put(
                    &mut bytes,
                    off,
                    bytemuck::cast_slice(&[w, h, if h > 0.0 { w / h } else { 1.0 }]),
                );
            }
            FrameSource::Mods => put(&mut bytes, off, bytemuck::cast_slice(&[mods])),
            FrameSource::CamTarget => put(&mut bytes, off, bytemuck::cast_slice(&cam.target)),
            FrameSource::CamAz => put(&mut bytes, off, bytemuck::cast_slice(&[cam.az])),
            FrameSource::CamElev => put(&mut bytes, off, bytemuck::cast_slice(&[cam.elev])),
            FrameSource::CamDist => put(&mut bytes, off, bytemuck::cast_slice(&[cam.dist])),
            FrameSource::Time => put(&mut bytes, off, bytemuck::cast_slice(&[time])),
        }
    }
    bytes
}

impl Renderer {
    fn new(gfx: Gfx, graph: &Graph, camera: &Camera, mods: u32, time: f32) -> Result<Self> {
        validate_descriptor_graph(graph)?;
        let device = &gfx.device;

        let mut modules: HashMap<&str, wgpu::ShaderModule> = HashMap::new();
        for &(key, bytes) in generated::SHADER_MODULES {
            modules.insert(key, wync::load_module_bytes(device, key, bytes));
        }

        // Byte size of each buffer that is a compute output (resource name ->
        // bytes), from each compute pass's generated size calc applied to its
        // StorageWrite bindings. Buffers with no declared size are sized from here.
        let initial_uniforms: HashMap<_, _> = graph
            .resources
            .iter()
            .filter_map(|resource| {
                let Resource::UniformBlock { name, members } = resource else {
                    return None;
                };
                let layout = generated::UNIFORM_BLOCKS
                    .iter()
                    .find(|l| l.name == *name)
                    .expect("uniform layout");
                Some((
                    *name,
                    pack_uniform_block(
                        members,
                        layout,
                        gfx.config.width,
                        gfx.config.height,
                        camera,
                        mods,
                        time,
                    ),
                ))
            })
            .collect();
        let mut output_sizes = Vec::new();
        let mut derived: HashMap<&'static str, u64> = HashMap::new();
        for pass in &graph.passes {
            if let Pass::Compute(cp) = pass {
                for &(_, binding, kind, usage, name) in cp.bindings {
                    if matches!(usage, BindingUsage::Output | BindingUsage::Intermediate)
                        && matches!(
                            kind,
                            BindingKind::StorageWrite | BindingKind::StorageReadWrite
                        )
                    {
                        let resource = name_to_resource(graph, cp.module, name);
                        let bytes = (cp.out_bytes)(
                            binding,
                            cp.runtime_counts[0],
                            cp.runtime_counts[1],
                            &|uniform, offset| {
                                uniform_word(
                                    &initial_uniforms,
                                    name_to_resource(graph, cp.module, uniform),
                                    offset,
                                )
                            },
                        );
                        derived
                            .entry(resource)
                            .and_modify(|n| *n = (*n).max(bytes))
                            .or_insert(bytes);
                        output_sizes.push((cp.clone(), binding, resource));
                    }
                }
            }
        }
        let size_of = |name: &'static str, declared: Option<u64>| -> u64 {
            declared.unwrap_or_else(|| {
                *derived
                    .get(name)
                    .unwrap_or_else(|| panic!("no size for '{name}'"))
            })
        };

        // Resources.
        let mut buffers: HashMap<&'static str, wgpu::Buffer> = HashMap::new();
        let mut pingpong: HashMap<&'static str, [wgpu::Buffer; 2]> = HashMap::new();
        let mut image_views: HashMap<&'static str, wgpu::TextureView> = HashMap::new();
        let mut img_formats: HashMap<&'static str, TexFormat> = HashMap::new();
        let mut blocks: Vec<(
            &'static str,
            &'static [BlockMember],
            &'static UniformBlockLayout,
        )> = Vec::new();
        let mut has_depth = false;
        let (cw, ch) = (gfx.config.width, gfx.config.height);
        for res in &graph.resources {
            match *res {
                Resource::UniformBlock { name, members } => {
                    // Size the buffer from the descriptor's published std140 layout.
                    let layout = generated::UNIFORM_BLOCKS
                        .iter()
                        .find(|l| l.name == name)
                        .unwrap_or_else(|| {
                            panic!("no descriptor layout for uniform block '{name}'")
                        });
                    buffers.insert(name, make_uniform(device, name, layout.size));
                    blocks.push((name, members, layout));
                }
                Resource::Buffer(def) => {
                    let buf = make_storage(
                        device,
                        &gfx.queue,
                        def.name,
                        size_of(def.name, def.size),
                        def.init,
                        def.indirect,
                    );
                    buffers.insert(def.name, buf);
                }
                Resource::PingPong { name, size } => {
                    let size = size_of(name, size);
                    let a = make_storage_raw(device, &format!("{name}#0"), size, false);
                    let b = make_storage_raw(device, &format!("{name}#1"), size, false);
                    pingpong.insert(name, [a, b]);
                }
                Resource::Image {
                    name,
                    format,
                    size,
                    mips,
                } => {
                    let (w, h) = match size {
                        ImgSize::Window => (cw, ch),
                        ImgSize::Fixed { w, h } => (w, h),
                    };
                    let usage = image_usage(graph, name);
                    let tex = create_image(device, name, format, w, h, mips, usage);
                    image_views.insert(
                        name,
                        tex.create_view(&wgpu::TextureViewDescriptor::default()),
                    );
                    img_formats.insert(name, format);
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

        let res = Res {
            buffers: &buffers,
            pp: &pingpong,
            views: &image_views,
            img_formats: &img_formats,
        };

        // Passes (preserve order).
        let mut passes = Vec::new();
        for pass in &graph.passes {
            match pass {
                Pass::Compute(cp) => {
                    let module = modules.get(cp.module).expect("module");
                    passes.push(BuiltPass::Compute(build_compute(
                        device, module, cp, res, graph,
                    )));
                }
                Pass::Render(rp) => {
                    let mut items = Vec::new();
                    for it in rp.items {
                        let module = modules.get(it.module).expect("module");
                        items.push(build_item(
                            device,
                            gfx.config.format,
                            rp.color,
                            rp.depth.is_some(),
                            module,
                            it,
                            res,
                            graph,
                        ));
                    }
                    passes.push(BuiltPass::Render(BuiltRender {
                        depth: rp.depth,
                        color: rp.color,
                        items,
                    }));
                }
            }
        }

        Ok(Self {
            gfx,
            buffers,
            image_views,
            blocks,
            depth_view,
            passes,
            graph: graph.clone(),
            pingpong,
            img_formats,
            output_sizes,
            frame: 0,
            start: Instant::now(),
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.gfx.resize(w, h);
        if self.depth_view.is_some() {
            self.depth_view = Some(create_depth(
                &self.gfx.device,
                self.gfx.config.width,
                self.gfx.config.height,
            ));
        }
    }

    /// Upload this frame's input events into the `events` buffer, zero-padding
    /// unused slots to None (kind 0) and dropping any past EV_CAP.
    fn upload_events(&self, events: &[[f32; 4]]) {
        let mut buf = vec![[0.0f32; 4]; app::EV_CAP];
        let n = events.len().min(app::EV_CAP);
        buf[..n].copy_from_slice(&events[..n]);
        self.gfx
            .queue
            .write_buffer(&self.buffers["events"], 0, bytemuck::cast_slice(&buf));
    }

    fn update_uniforms(&mut self, cam: &Camera, mods: u32, time: f32) {
        let snapshot: HashMap<_, _> = self
            .blocks
            .iter()
            .map(|(name, members, layout)| {
                (
                    *name,
                    pack_uniform_block(
                        members,
                        layout,
                        self.gfx.config.width,
                        self.gfx.config.height,
                        cam,
                        mods,
                        time,
                    ),
                )
            })
            .collect();
        self.grow_outputs(&snapshot);
        for (name, bytes) in snapshot {
            self.gfx.queue.write_buffer(&self.buffers[name], 0, &bytes);
        }
    }

    /// Grow logical capacities before submitting work using the same uniform snapshot.
    /// Preserve existing contents, including both sides of persistent ping-pong resources.
    fn grow_outputs(&mut self, snapshot: &HashMap<&str, Vec<u8>>) {
        let mut required = HashMap::<&str, u64>::new();
        for (cp, binding, resource) in &self.output_sizes {
            let bytes = (cp.out_bytes)(
                *binding,
                cp.runtime_counts[0],
                cp.runtime_counts[1],
                &|name, offset| {
                    uniform_word(
                        snapshot,
                        name_to_resource(&self.graph, cp.module, name),
                        offset,
                    )
                },
            );
            required
                .entry(resource)
                .and_modify(|n| *n = (*n).max(bytes))
                .or_insert(bytes);
        }
        let device = &self.gfx.device;
        let mut copies = None;
        let mut grow = |name: &str, buffer: &mut wgpu::Buffer, bytes: u64| {
            if bytes <= buffer.size() {
                return;
            }
            let next = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(name),
                size: bytes.max(4),
                usage: buffer.usage(),
                mapped_at_creation: false,
            });
            let encoder = copies.get_or_insert_with(|| {
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("grow logical outputs"),
                })
            });
            encoder.copy_buffer_to_buffer(buffer, 0, &next, 0, buffer.size());
            *buffer = next;
        };
        for (name, bytes) in required {
            if let Some(buffer) = self.buffers.get_mut(name) {
                grow(name, buffer, bytes);
            }
            if let Some(pair) = self.pingpong.get_mut(name) {
                for buffer in pair {
                    grow(name, buffer, bytes);
                }
            }
        }
        let Some(copies) = copies else { return };
        self.gfx.queue.submit([copies.finish()]);
        let res = Res {
            buffers: &self.buffers,
            pp: &self.pingpong,
            views: &self.image_views,
            img_formats: &self.img_formats,
        };
        for (built, definition) in self.passes.iter_mut().zip(&self.graph.passes) {
            match (built, definition) {
                (BuiltPass::Compute(built), Pass::Compute(cp)) => {
                    for (built, stage) in built.stages.iter_mut().zip(&cp.stages) {
                        let binds = resolve_table(cp.module, stage.bindings, &self.graph, res.pp);
                        built.sets = (0..variant_count(&binds))
                            .map(|parity| {
                                build_sets(
                                    device,
                                    stage.entry,
                                    &binds,
                                    |_, _| wgpu::ShaderStages::COMPUTE,
                                    res,
                                    parity,
                                )
                                .1
                            })
                            .collect();
                    }
                }
                (BuiltPass::Render(built), Pass::Render(rp)) => {
                    for (built, item) in built.items.iter_mut().zip(rp.items) {
                        let binds = resolve_table(item.module, item.bindings, &self.graph, res.pp);
                        built.sets = (0..variant_count(&binds))
                            .map(|parity| {
                                build_sets(
                                    device,
                                    item.label,
                                    &binds,
                                    |set, binding| item.binding_visibility(set, binding),
                                    res,
                                    parity,
                                )
                                .1
                            })
                            .collect();
                    }
                }
                _ => unreachable!("built graph preserves pass order"),
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
                        label: Some(c.label),
                        timestamp_writes: None,
                    });
                    for stage in &c.stages {
                        cp.set_pipeline(&stage.pipeline);
                        let set_list = &stage.sets[parity % stage.sets.len()];
                        for (set, bg) in set_list {
                            cp.set_bind_group(*set, bg, &[]);
                        }
                        cp.dispatch_workgroups(stage.groups[0], stage.groups[1], stage.groups[2]);
                    }
                }
                BuiltPass::Render(r) => {
                    let depth_attach = if r.depth.is_some() {
                        self.depth_view
                            .as_ref()
                            .map(|dv| wgpu::RenderPassDepthStencilAttachment {
                                view: dv,
                                depth_ops: Some(wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(1.0),
                                    store: wgpu::StoreOp::Store,
                                }),
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
                                    load: wgpu::LoadOp::Clear(wgpu::Color {
                                        r: c[0],
                                        g: c[1],
                                        b: c[2],
                                        a: c[3],
                                    }),
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
                        match it.draw {
                            Draw::Direct {
                                vertex_count,
                                instance_count,
                                first_vertex,
                                first_instance,
                            } => rp.draw(
                                first_vertex..first_vertex + vertex_count,
                                first_instance..first_instance + instance_count,
                            ),
                            Draw::Indirect { commands, offset } => {
                                rp.draw_indirect(&self.buffers[commands], offset)
                            }
                        }
                    }
                }
            }
        }
    }

    fn render(&mut self, cam: &Camera, mods: u32) -> Result<()> {
        self.update_uniforms(cam, mods, self.start.elapsed().as_secs_f32());
        let surface = self
            .gfx
            .surface
            .as_ref()
            .expect("window mode has a surface");
        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                surface.configure(&self.gfx.device, &self.gfx.config);
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("acquire frame: {e:?}")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.record(&mut enc, &view);
        self.gfx.queue.submit(Some(enc.finish()));
        frame.present();
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    /// Debug: read a storage buffer back and print its contents as u32 words
    /// (head, middle, and tail). Headless-path only — stalls the GPU.
    fn dump_buffer(&self, name: &str) -> Result<()> {
        let buf = self
            .buffers
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no buffer '{name}' to dump"))?;
        let size = buf.size();
        let staging = self.gfx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dump"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dump"),
            });
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, size);
        self.gfx.queue.submit(Some(enc.finish()));
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gfx.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = slice.get_mapped_range();
        let words: &[u32] = bytemuck::cast_slice(&data);
        eprintln!("{name}: {size} bytes / {} words", words.len());
        let show = |label: &str, start: usize, n: usize| {
            let end = (start + n).min(words.len());
            eprintln!("  {label} [{start}..{end}]: {:?}", &words[start..end]);
        };
        show("head", 0, 48);
        if words.len() > 96 {
            show("mid ", words.len() / 2, 24);
            show("tail", words.len() - 24, 24);
        }
        Ok(())
    }

    /// Headless: render a scripted scenario into an offscreen texture and write it
    /// to `path` as a PNG. Used to eyeball the pipeline without a window.
    fn screenshot(
        &mut self,
        path: &std::path::Path,
        cam: &Camera,
        mods: u32,
        time: f32,
    ) -> Result<()> {
        let (w, h) = (self.gfx.config.width, self.gfx.config.height);
        let tex = self.gfx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
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
        // Drive it as the same event stream the live path produces: a MouseDown on
        // the first held frame, a MouseMove each held frame, a MouseUp on release.
        let total = 20u32;
        let mut frame_ms: Vec<f32> = Vec::new();
        let mut prev_held = false;
        for f in 0..total {
            let t = f as f32 / (total - 1).max(1) as f32;
            // A curved sweep (one sine arch) so the ribbon shows the spline curving.
            let mx = (0.25 + 0.50 * t) * w as f32;
            let my = (0.5 - 0.18 * (t * std::f32::consts::PI).sin()) * h as f32;
            let held = f + 4 < total; // release near the end
            let mut events: Vec<[f32; 4]> = Vec::new();
            if held && !prev_held {
                events.push(encode_event(EV_MOUSEDOWN, 0, 0.0, mx, my));
            }
            if held {
                events.push(encode_event(EV_MOUSEMOVE, 0, 0.0, mx, my));
            }
            if !held && prev_held {
                events.push(encode_event(EV_MOUSEUP, 0, 0.0, mx, my));
            }
            prev_held = held;
            self.upload_events(&events);
            self.update_uniforms(cam, mods, time);
            let t0 = Instant::now();
            let mut enc = self
                .gfx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("offscreen"),
                });
            self.record(&mut enc, &view);
            self.gfx.queue.submit(Some(enc.finish()));
            let _ = self.gfx.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
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
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("copy"),
            });
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
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.gfx.queue.submit(Some(enc.finish()));

        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gfx.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = readback.slice(..).get_mapped_range();

        // Drop the row padding into a tight RGBA8 buffer.
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();

        let file =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&pixels)?;
        eprintln!("wrote {} ({w}x{h})", path.display());
        Ok(())
    }
}

fn validate_descriptor_graph(graph: &Graph) -> Result<()> {
    let mut expected: Vec<(&'static str, &'static str, &'static str)> = Vec::new();
    for pass in &graph.passes {
        match pass {
            Pass::Compute(cp) => {
                for stage in &cp.stages {
                    expected.push((cp.module, stage.entry, "compute"));
                }
            }
            Pass::Render(rp) => {
                for item in rp.items {
                    expected.push((item.module, item.vs, "vertex"));
                    expected.push((item.module, item.fs, "fragment"));
                }
            }
        }
    }

    for &(module, name, kind) in &expected {
        if !generated::DESCRIPTOR_PASSES
            .iter()
            .any(|p| p.module == module && p.name == name && p.kind == kind)
        {
            anyhow::bail!(
                "graph references {kind} stage '{name}' in module '{module}', but it is not in the Wyn descriptor frame_graph"
            );
        }
    }

    for pass in generated::DESCRIPTOR_PASSES {
        if !expected.iter().any(|(module, name, kind)| {
            *module == pass.module && *name == pass.name && *kind == pass.kind
        }) {
            anyhow::bail!(
                "Wyn descriptor frame_graph contains {} stage '{}' in module '{}', but the driver graph does not schedule it",
                pass.kind,
                pass.name,
                pass.module
            );
        }
    }

    Ok(())
}

// ---- resource creation ----

/// Copy `src` into `dst` starting at `off` (one uniform-block member).
fn put(dst: &mut [u8], off: usize, src: &[u8]) {
    dst[off..off + src.len()].copy_from_slice(src);
}

// Input event kinds (must match `step`'s constants in main.wyn).
const EV_KEYDOWN: u32 = 1;
const EV_KEYUP: u32 = 2;
const EV_MOUSEDOWN: u32 = 3;
const EV_MOUSEUP: u32 = 4;
const EV_MOUSEMOVE: u32 = 5;

/// Encode one input event as a `vec4f32`: `.x = kind*256 + mods` (mods frozen at
/// the event), `.y = code` (keycode / button index), `.z,.w = cursor pixels).
fn encode_event(kind: u32, mods: u32, code: f32, x: f32, y: f32) -> [f32; 4] {
    [(kind * 256 + mods) as f32, code, x, y]
}

/// Map a physical key to the shared keycode table `step` reads (None = ignored).
fn keycode(pk: winit::keyboard::PhysicalKey) -> Option<f32> {
    use winit::keyboard::{KeyCode, PhysicalKey};
    match pk {
        PhysicalKey::Code(KeyCode::Tab) => Some(1.0), // KEY_TAB
        PhysicalKey::Code(KeyCode::KeyL) => Some(2.0), // KEY_L
        _ => None,
    }
}

fn make_uniform(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn make_storage_raw(device: &wgpu::Device, label: &str, size: u64, indirect: bool) -> wgpu::Buffer {
    // COPY_SRC lets `--dump` read any storage buffer back for inspection.
    let mut usage =
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
    if indirect {
        usage |= wgpu::BufferUsages::INDIRECT;
    }
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        // A zero logical length still needs a legal, nonempty storage binding.
        size: size.max(4),
        usage,
        mapped_at_creation: false,
    })
}

fn make_storage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    name: &str,
    size: u64,
    init: BufInit,
    indirect: bool,
) -> wgpu::Buffer {
    let buf = make_storage_raw(device, name, size, indirect);
    match init {
        BufInit::U32s(vals) => queue.write_buffer(&buf, 0, bytemuck::cast_slice(vals)),
        BufInit::Zeroed => {}
    }
    buf
}

fn create_depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
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
    let touch = |module: &'static str, t: BindTable, u: &mut wgpu::TextureUsages| {
        for &(_, _, kind, _, bname) in t {
            if name_to_resource(graph, module, bname) == name {
                match kind {
                    BindingKind::StorageImage { .. } => *u |= wgpu::TextureUsages::STORAGE_BINDING,
                    BindingKind::Texture => *u |= wgpu::TextureUsages::TEXTURE_BINDING,
                    _ => {}
                }
            }
        }
    };
    for pass in &graph.passes {
        match pass {
            Pass::Compute(cp) => touch(cp.module, cp.bindings, &mut u),
            Pass::Render(rp) => {
                if rp.color.iter().any(|ct| ct.target == Some(name)) {
                    u |= wgpu::TextureUsages::RENDER_ATTACHMENT;
                }
                for it in rp.items {
                    touch(it.module, it.bindings, &mut u);
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
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: mips.max(1),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu_format(format),
        usage,
        view_formats: &[],
    })
}

// ---- bind groups (shared by compute + render) ----

/// Map a shader binding name to its physical graph resource. Authored aliases
/// win; otherwise the descriptor groups compiler-created producer/consumer names
/// that refer to one resource. Unclaimed compiler scratch is auto-allocated under
/// the descriptor resource's opaque identity.
fn name_to_resource(
    graph: &Graph,
    module: &'static str,
    binding_name: &'static str,
) -> &'static str {
    let authored = |name: &str| {
        graph
            .names
            .iter()
            .find(|(binding, _)| *binding == name)
            .map(|(_, resource)| *resource)
    };
    if let Some(resource) = authored(binding_name) {
        return resource;
    }
    let Some(descriptor) = generated::descriptor_resource(module, binding_name) else {
        return binding_name;
    };
    descriptor
        .binding_names
        .iter()
        .find_map(|alias| authored(alias))
        .unwrap_or(descriptor.name)
}

/// Union of the storage-image accesses of `resource` across the whole graph. The
/// compiler emits one module global per image and gives it the union access (a
/// resource written by one entry and read by another is read-write), so every
/// pipeline's layout for that binding must use the union too — not the per-view
/// access the descriptor records.
fn image_union_access(graph: &Graph, resource: &str) -> ImgAccess {
    let (mut r, mut w) = (false, false);
    let mut scan = |module: &'static str, t: BindTable| {
        for &(_, _, kind, _, bname) in t {
            if let BindingKind::StorageImage { access, .. } = kind {
                if name_to_resource(graph, module, bname) == resource {
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
    for pass in &graph.passes {
        match pass {
            Pass::Compute(cp) => scan(cp.module, cp.bindings),
            Pass::Render(rp) => {
                for it in rp.items {
                    scan(it.module, it.bindings);
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
    module: &'static str,
    table: BindTable,
    graph: &Graph,
    pp: &HashMap<&'static str, [wgpu::Buffer; 2]>,
) -> Vec<Binding> {
    table
        .iter()
        .map(|&(set, binding, kind, usage, name)| {
            let resource = name_to_resource(graph, module, name);
            let kind = match kind {
                BindingKind::StorageImage { format, .. } => BindingKind::StorageImage {
                    format,
                    access: image_union_access(graph, resource),
                },
                other => other,
            };
            let is_pp = pp.contains_key(resource);
            let role = match (is_pp, usage, kind) {
                (true, BindingUsage::Output, _) => Role::Next,
                (true, BindingUsage::Input, _) => Role::Prev,
                // Compatibility for descriptors predating explicit buffer usage.
                (true, _, BindingKind::StorageWrite) => Role::Next,
                (true, _, BindingKind::StorageRead) => Role::Prev,
                _ => Role::Plain,
            };
            Binding {
                set,
                binding,
                resource,
                kind,
                role,
            }
        })
        .collect()
}

/// Whether a resolved binding set needs both parities (has a ping-pong binding).
fn variant_count(bindings: &[Binding]) -> usize {
    if bindings.iter().any(|b| b.role != Role::Plain) {
        2
    } else {
        1
    }
}

/// Resolve a buffer binding to its physical buffer for `parity`. Ping-pong: this
/// frame's buffer is index `parity` (Next); last frame's is `1 - parity` (Prev).
/// Only called for buffer kinds; image kinds resolve via `res.views`.
fn resolve<'a>(b: &Binding, parity: usize, res: Res<'a>) -> &'a wgpu::Buffer {
    match b.role {
        Role::Plain => res
            .buffers
            .get(b.resource)
            .unwrap_or_else(|| panic!("no resource '{}'", b.resource)),
        Role::Next => &res
            .pp
            .get(b.resource)
            .unwrap_or_else(|| panic!("no ping-pong '{}'", b.resource))[parity],
        Role::Prev => &res
            .pp
            .get(b.resource)
            .unwrap_or_else(|| panic!("no ping-pong '{}'", b.resource))[1 - parity],
    }
}

/// The wgpu layout binding type for one resolved binding. `filterable` applies
/// only to sampled `texture2d` bindings — it must be false for unfilterable
/// formats (R32Float depth), which are read via `texture_load`.
fn layout_type(kind: BindingKind, filterable: bool) -> wgpu::BindingType {
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
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
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
    visibility: impl Fn(u32, u32) -> wgpu::ShaderStages,
    res: Res,
    parity: usize,
) -> (Vec<wgpu::BindGroupLayout>, Vec<(u32, wgpu::BindGroup)>) {
    if bindings.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let max_set = bindings.iter().map(|b| b.set).max().unwrap_or(0);
    let mut layouts = Vec::new();
    let mut sets = Vec::new();

    for set in 0..=max_set {
        let in_set: Vec<&Binding> = bindings.iter().filter(|b| b.set == set).collect();
        let entries: Vec<wgpu::BindGroupLayoutEntry> = in_set
            .iter()
            .map(|b| wgpu::BindGroupLayoutEntry {
                binding: b.binding,
                visibility: visibility(b.set, b.binding),
                // Sampled textures over unfilterable formats (R32Float) must declare
                // a non-filterable sample type; look up the bound resource's format.
                ty: layout_type(
                    b.kind,
                    res.img_formats
                        .get(b.resource)
                        .map_or(true, |f| f.filterable()),
                ),
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
                    BindingKind::Texture | BindingKind::StorageImage { .. } => {
                        wgpu::BindingResource::TextureView(
                            res.views
                                .get(b.resource)
                                .unwrap_or_else(|| panic!("no image view '{}'", b.resource)),
                        )
                    }
                    _ => resolve(b, parity, res).as_entire_binding(),
                };
                wgpu::BindGroupEntry {
                    binding: b.binding,
                    resource,
                }
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
    let stages = cp
        .stages
        .iter()
        .map(|st| {
            let binds = resolve_table(cp.module, st.bindings, graph, res.pp);
            let (layouts, sets0) = build_sets(
                device,
                st.entry,
                &binds,
                |_, _| wgpu::ShaderStages::COMPUTE,
                res,
                0,
            );
            let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(st.entry),
                bind_group_layouts: &layout_refs,
                push_constant_ranges: &[],
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(st.entry),
                layout: Some(&layout),
                module,
                entry_point: Some(st.entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
            let mut sets = vec![sets0];
            for parity in 1..variant_count(&binds) {
                sets.push(
                    build_sets(
                        device,
                        st.entry,
                        &binds,
                        |_, _| wgpu::ShaderStages::COMPUTE,
                        res,
                        parity,
                    )
                    .1,
                );
            }
            BuiltComputeStage {
                pipeline,
                groups: st.groups,
                sets,
            }
        })
        .collect();
    BuiltCompute {
        label: cp.label,
        stages,
    }
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
    let binds = resolve_table(it.module, it.bindings, graph, res.pp);
    let (layouts, sets0) = build_sets(
        device,
        it.label,
        &binds,
        |set, binding| it.binding_visibility(set, binding),
        res,
        0,
    );
    let layout_refs: Vec<&wgpu::BindGroupLayout> = layouts.iter().collect();
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(it.label),
        bind_group_layouts: &layout_refs,
        push_constant_ranges: &[],
    });
    let depth_stencil = if has_depth && it.depth_test != DepthTest::Disabled {
        // Depth-writers test LessEqual: protruding geometry self-occludes, while
        // coplanar fragments at equal depth let the later draw win, preserving
        // painter order within the geometry stream. Non-writers test Always.
        Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: it.depth_write,
            depth_compare: wgpu::CompareFunction::LessEqual,
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
        sets.push(
            build_sets(
                device,
                it.label,
                &binds,
                |set, binding| it.binding_visibility(set, binding),
                res,
                p,
            )
            .1,
        );
    }
    BuiltItem {
        pipeline,
        sets,
        draw: match it.draw {
            Draw::Indirect { commands, offset } => Draw::Indirect {
                commands: name_to_resource(graph, it.module, commands),
                offset,
            },
            direct => direct,
        },
    }
}

// winit 0.30 drives the app through `ApplicationHandler`: the window (and thus
// the GPU surface) is created in `resumed`, events arrive in `window_event`, and
// `about_to_wait` keeps redraws flowing.
struct App {
    args: Args,
    /// Orbit camera, driven by RMB (rotate), MMB (pan), wheel (zoom); eased itself.
    cam: Camera,
    /// Live modifier mask (bit0 shift, 1 ctrl, 2 alt, 3 super).
    mods: u32,
    /// Right / middle mouse held — right drives orbit, middle drives pan.
    rmb: bool,
    mmb: bool,
    /// Raw input events since the last redraw; uploaded and cleared each frame.
    events: Vec<[f32; 4]>,
    /// Last cursor position (pixels), stamped onto button events / drag deltas.
    mouse: (f32, f32),
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// Wall-clock presentation rate, including event-loop and vsync waits.
    fps_start: Instant,
    fps_frames: u32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        // The graph bakes fixed window-sized compute grids (pxl/otile/occ) and the
        // window-sized images are not rebuilt on resize, so the surface must be
        // exactly the graph size. Request a PHYSICAL size (DPI-independent, unlike
        // LogicalSize which inflates the surface on HiDPI and leaves the bottom rows
        // unlit) and disable resizing (the resize path can't rebuild the grids).
        let attrs = WindowAttributes::default()
            .with_title("tiny porto")
            .with_inner_size(PhysicalSize::new(self.args.width, self.args.height))
            .with_resizable(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("create_window: {e}");
                event_loop.exit();
                return;
            }
        };
        let renderer = Gfx::new(window.clone()).and_then(|gfx| {
            let (w, h) = (gfx.config.width, gfx.config.height);
            Renderer::new(gfx, &app::graph(w, h), &self.cam, self.mods, 0.0)
        });
        match renderer {
            Ok(r) => {
                self.window = Some(window);
                self.renderer = Some(r);
                self.fps_start = Instant::now();
                self.fps_frames = 0;
            }
            Err(e) => {
                eprintln!("gpu init: {e:?}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let (sw, sh) = (
            renderer.gfx.config.width as f32,
            renderer.gfx.config.height as f32,
        );
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(sz) => renderer.resize(sz.width, sz.height),
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                self.mods = (s.shift_key() as u32)
                    | ((s.control_key() as u32) << 1)
                    | ((s.alt_key() as u32) << 2)
                    | ((s.super_key() as u32) << 3);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let prev = self.mouse;
                self.mouse = (position.x as f32, position.y as f32);
                let (dx, dy) = (self.mouse.0 - prev.0, self.mouse.1 - prev.1);
                // Right drag orbits (cursor-pivot), middle drag pans (grab the ground).
                if self.rmb {
                    self.cam.orbit(dx, dy, sw, sh);
                } else if self.mmb {
                    self.cam.pan(sw, sh, prev, self.mouse);
                } else {
                    // Forward motion to Wyn's paint stream only when not manipulating the
                    // camera, so a right/middle drag never grows a stroke.
                    self.events.push(encode_event(
                        EV_MOUSEMOVE,
                        self.mods,
                        0.0,
                        self.mouse.0,
                        self.mouse.1,
                    ));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                match button {
                    // Left = build/paint: goes to Wyn as an event, but not while a
                    // camera button is held (no painting during orbit/pan).
                    MouseButton::Left if !(self.rmb || self.mmb) => {
                        let kind = if down { EV_MOUSEDOWN } else { EV_MOUSEUP };
                        self.events.push(encode_event(
                            kind,
                            self.mods,
                            0.0,
                            self.mouse.0,
                            self.mouse.1,
                        ));
                    }
                    MouseButton::Left => {}
                    // Right = orbit (driver-owned camera): anchor the cursor pivot on press.
                    MouseButton::Right => {
                        self.rmb = down;
                        if down {
                            self.cam.begin_orbit(sw, sh, self.mouse.0, self.mouse.1);
                        } else {
                            self.cam.end_orbit();
                        }
                    }
                    // Middle = pan.
                    MouseButton::Middle => self.mmb = down,
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                self.cam.zoom(sw, sh, self.mouse.0, self.mouse.1, notches);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                // Forward keydown/up edges (ignore auto-repeat) as events carrying the
                // live mods. The driver maps physical keys to codes but never assigns
                // them meaning — `step` does.
                if !key_event.repeat {
                    if let Some(code) = keycode(key_event.physical_key) {
                        let kind = if key_event.state == ElementState::Pressed {
                            EV_KEYDOWN
                        } else {
                            EV_KEYUP
                        };
                        self.events
                            .push(encode_event(kind, self.mods, code, 0.0, 0.0));
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.cam.ease(); // glide the visible camera toward the input target
                renderer.upload_events(&self.events);
                self.events.clear();
                let previous_frame = renderer.frame;
                if let Err(e) = renderer.render(&self.cam, self.mods) {
                    eprintln!("render error: {e:?}");
                }
                if renderer.frame != previous_frame {
                    self.fps_frames += 1;
                }
                let now = Instant::now();
                let elapsed = now.duration_since(self.fps_start).as_secs_f64();
                if elapsed >= 0.5 && self.fps_frames > 0 {
                    let fps = f64::from(self.fps_frames) / elapsed;
                    let frame_ms = elapsed * 1000.0 / f64::from(self.fps_frames);
                    if let Some(window) = &self.window {
                        window.set_title(&format!("tiny porto — {fps:.0} FPS · {frame_ms:.1} ms"));
                    }
                    self.fps_start = now;
                    self.fps_frames = 0;
                }
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
        let (gw, gh) = (gfx.config.width, gfx.config.height);
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], args.cam_az, args.cam_elev, args.cam_dist);
        let mut renderer = Renderer::new(gfx, &app::graph(gw, gh), &cam, args.mods, args.time)?;
        renderer.screenshot(&path, &cam, args.mods, args.time)?;
        for name in &args.dump {
            renderer.dump_buffer(name)?;
        }
        return Ok(());
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        args,
        cam: Camera::default(),
        mods: 0,
        rmb: false,
        mmb: false,
        events: Vec::new(),
        mouse: (0.0, 0.0),
        window: None,
        renderer: None,
        fps_start: Instant::now(),
        fps_frames: 0,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod allocation_tests {
    use super::*;

    fn uniform_output_bytes(_: u32, _: u64, _: u64, words: &UniformWords<'_>) -> u64 {
        static LENGTH: std::sync::LazyLock<wyn_pipeline_descriptor::BufferLen> =
            std::sync::LazyLock::new(|| {
                let descriptor: serde_json::Value = serde_json::from_str(include_str!(
                    "../tests/fixtures/uniform_output_size.json"
                ))
                .unwrap();
                let output = descriptor["pipelines"][1]["bindings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|b| b["usage"] == "output")
                    .unwrap();
                serde_json::from_value(output["length"].clone()).unwrap()
            });
        LENGTH
            .resolve_host_bytes(&|set, binding, offset| {
                if (set, binding) == (0, 0) {
                    words("frame", offset)
                } else {
                    None
                }
            })
            .unwrap()
    }

    #[test]
    fn uniform_capacity_growth_preserves_storage_and_pingpong_contents() {
        let gfx = Gfx::new_headless(64, 32).expect("headless GPU");
        let buffer = |name, value| {
            make_storage(
                &gfx.device,
                &gfx.queue,
                name,
                4,
                BufInit::U32s(value),
                false,
            )
        };
        let buffers = HashMap::from([("pixels", buffer("pixels", &[42]))]);
        let pingpong = HashMap::from([(
            "history",
            [buffer("history0", &[17]), buffer("history1", &[29])],
        )]);
        const BINDS: BindTable = &[(
            0,
            0,
            BindingKind::StorageReadWrite,
            BindingUsage::Output,
            "pixels",
        )];
        let cp = ComputePass {
            label: "test",
            module: "test",
            bindings: BINDS,
            stages: vec![ComputeStage {
                entry: "update",
                groups: [1, 1, 1],
                bindings: BINDS,
            }],
            out_bytes: uniform_output_bytes,
            runtime_counts: [0, 0],
        };
        let graph = Graph {
            resources: vec![],
            passes: vec![Pass::Compute(cp.clone())],
            names: &[],
        };
        let image_views = HashMap::new();
        let img_formats = HashMap::new();
        let module = gfx.device.create_shader_module(wgpu::ShaderModuleDescriptor { label:Some("binding growth test"),source:wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var<storage,read_write> data: array<u32>; @compute @workgroup_size(1) fn update() { data[0] = data[0] + 1u; }".into()) });
        let built = build_compute(
            &gfx.device,
            &module,
            &cp,
            Res {
                buffers: &buffers,
                pp: &pingpong,
                views: &image_views,
                img_formats: &img_formats,
            },
            &graph,
        );
        let mut renderer = Renderer {
            gfx,
            buffers,
            pingpong,
            image_views,
            img_formats,
            blocks: vec![],
            depth_view: None,
            passes: vec![BuiltPass::Compute(built)],
            graph,
            output_sizes: vec![(cp.clone(), 2, "pixels"), (cp, 2, "history")],
            frame: 0,
            start: Instant::now(),
        };
        let snapshot = |w: f32, h: f32| {
            HashMap::from([("frame", [w.to_le_bytes(), h.to_le_bytes()].concat())])
        };
        renderer.grow_outputs(&snapshot(64.0, 32.0));
        assert_eq!(renderer.buffers["pixels"].size(), 8192);
        assert!(renderer.pingpong["history"]
            .iter()
            .all(|b| b.size() == 8192));
        renderer.grow_outputs(&snapshot(128.0, 65.0));
        assert_eq!(renderer.buffers["pixels"].size(), 4 * 128 * 65);
        renderer.grow_outputs(&snapshot(0.0, 32.0));
        assert_eq!(
            renderer.buffers["pixels"].size(),
            4 * 128 * 65,
            "zero logical size need not shrink capacity"
        );

        let readback = renderer.gfx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("growth check"),
            size: 12,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = renderer
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        // Execute through the refreshed bind group. A stale binding would write
        // the old buffer and leave the newly allocated output unchanged.
        {
            let BuiltPass::Compute(built) = &renderer.passes[0] else {
                unreachable!()
            };
            let stage = &built.stages[0];
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&stage.pipeline);
            for (set, group) in &stage.sets[0] {
                pass.set_bind_group(*set, group, &[]);
            }
            pass.dispatch_workgroups(1, 1, 1);
        }
        for (i, buffer) in [
            &renderer.buffers["pixels"],
            &renderer.pingpong["history"][0],
            &renderer.pingpong["history"][1],
        ]
        .into_iter()
        .enumerate()
        {
            encoder.copy_buffer_to_buffer(buffer, 0, &readback, i as u64 * 4, 4);
        }
        renderer.gfx.queue.submit([encoder.finish()]);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |result| result.unwrap());
        renderer
            .gfx
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        let bytes = readback.slice(..).get_mapped_range();
        let values: Vec<_> = bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(values, [43, 17, 29]);
    }
}
