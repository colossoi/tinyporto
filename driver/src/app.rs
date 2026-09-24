//! Application inputs, retained frame results, and caller-owned render targets.
//! All shader loading, pipeline creation, dispatches, and draws belong to Wyn.

use crate::temporal::{HistoryKey, TemporalFrame, TemporalState};
use crate::{camera::Camera, generated, gfx::Gfx, materials::Materials, shadow, terrain};
use anyhow::{bail, Context, Result};
use generated::output::{OutputDescriptor, OutputResource};
use std::time::Instant;

// Input ABI from input.wyn and paint.wyn; output sizes are compiler-owned.
const EV_CAP: usize = 32;

struct World {
    ui: wgpu::Buffer,
    points: wgpu::Buffer,
    items: wgpu::Buffer,
    head: wgpu::Buffer,
    occ: wgpu::Buffer,
    gi: wgpu::Buffer,
}

impl World {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        Self {
            ui: storage(device, "uistate", 2 * 4),
            points: storage(device, "points", 1024 * 8),
            items: storage(device, "items", 128 * 16),
            head: storage(device, "head", 12 * 4),
            occ: storage(device, "previous occlusion", occlusion_bytes(width, height)),
            gi: storage(device, "previous GI", gi_bytes(width, height, false)),
        }
    }

    fn from_output(output: &OutputDescriptor) -> Result<Self> {
        // The first six results are persistent state; later results expose GI
        // diagnostics and render targets without being retained as next inputs.
        // Retain the actual returned allocations for the next call.
        let buffer = |index: usize| match output.values.get(index).map(|value| &value.resource) {
            Some(OutputResource::Buffer { buffer, .. }) => Ok(buffer.clone()),
            _ => bail!(
                "Wyn entry '{}' did not return persistent buffer {index}",
                output.entry
            ),
        };
        Ok(Self {
            ui: buffer(0)?,
            points: buffer(1)?,
            items: buffer(2)?,
            head: buffer(3)?,
            occ: buffer(4)?,
            gi: buffer(5)?,
        })
    }

    fn buffer(&self, name: &str) -> Option<&wgpu::Buffer> {
        match name {
            "uistate" => Some(&self.ui),
            "points" => Some(&self.points),
            "items" => Some(&self.items),
            "head" => Some(&self.head),
            "occ" => Some(&self.occ),
            "gi" => Some(&self.gi),
            _ => None,
        }
    }
}

fn occlusion_bytes(width: u32, height: u32) -> u64 {
    // Must match OCC_TILE in hiz.wyn.
    u64::from(width.div_ceil(8)) * u64::from(height.div_ceil(8)) * 4
}

fn gi_bytes(width: u32, height: u32, reference: bool) -> u64 {
    // gi.sample in gi.wyn: seven vec4s. Reference has full-resolution history.
    let divisor = if reference { 1 } else { 2 };
    u64::from(width.div_ceil(divisor)) * u64::from(height.div_ceil(divisor)) * 112
}

struct FrameHistory {
    camera: Camera,
    frame_index: u32,
    valid: u32,
    gi_mode: u32,
    textures_enabled: u32,
    temporal: TemporalFrame,
}

fn storage(device: &wgpu::Device, name: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(name),
        size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Pack application values using the compiler's published field offsets.
fn frame_bytes(
    width: u32,
    height: u32,
    cam: &Camera,
    mods: u32,
    time: f32,
    sky_clouds: &[f32; 3],
    history: &FrameHistory,
) -> Result<Vec<u8>> {
    let id = generated::RESOURCE_NAMES
        .iter()
        .find(|(_, name)| *name == "frame")
        .map(|(id, _)| *id)
        .context("Wyn frame input is missing")?;
    let fields: Vec<_> = generated::BUFFER_FIELDS
        .iter()
        .filter(|(resource, ..)| *resource == id)
        .collect();
    let size = fields
        .iter()
        .map(|(_, _, offset, size)| offset + size)
        .max()
        .context("Wyn frame layout is missing")?;
    let mut bytes = vec![0; size.div_ceil(16) as usize * 16];
    let resolution = [
        width as f32,
        height as f32,
        width as f32 / height.max(1) as f32,
    ];
    for (name, value) in [
        ("resolution", bytemuck::cast_slice(&resolution)),
        ("mods", bytemuck::bytes_of(&mods)),
        ("cam_target", bytemuck::cast_slice(&cam.target)),
        ("cam_az", bytemuck::bytes_of(&cam.az)),
        ("cam_elev", bytemuck::bytes_of(&cam.elev)),
        ("cam_dist", bytemuck::bytes_of(&cam.dist)),
        ("time", bytemuck::bytes_of(&time)),
        ("sky_clouds", bytemuck::cast_slice(sky_clouds)),
        (
            "previous_target",
            bytemuck::cast_slice(&history.camera.target),
        ),
        ("previous_az", bytemuck::bytes_of(&history.camera.az)),
        ("previous_elev", bytemuck::bytes_of(&history.camera.elev)),
        ("previous_dist", bytemuck::bytes_of(&history.camera.dist)),
        ("frame_index", bytemuck::bytes_of(&history.frame_index)),
        ("history_valid", bytemuck::bytes_of(&history.valid)),
        ("gi_mode", bytemuck::bytes_of(&history.gi_mode)),
        ("jitter", bytemuck::cast_slice(&history.temporal.jitter)),
        (
            "previous_jitter",
            bytemuck::cast_slice(&history.temporal.previous_jitter),
        ),
        ("taa_enabled", bytemuck::bytes_of(&history.temporal.enabled)),
        ("taa_valid", bytemuck::bytes_of(&history.temporal.valid)),
        (
            "textures_enabled",
            bytemuck::bytes_of(&history.textures_enabled),
        ),
    ] {
        let (_, _, offset, size) = fields
            .iter()
            .find(|(_, field, ..)| *field == name)
            .with_context(|| format!("Wyn frame has no field '{name}'"))?;
        anyhow::ensure!(
            *size as usize == value.len(),
            "Wyn frame field '{name}' changed size"
        );
        bytes[*offset as usize..*offset as usize + value.len()].copy_from_slice(value);
    }
    Ok(bytes)
}

struct Targets {
    hdr: wgpu::Texture,
    albedo: wgpu::Texture,
    normal: wgpu::Texture,
    depth: wgpu::Texture,
    scene_z: wgpu::Texture,
    reflection: wgpu::Texture,
    reflection_z: wgpu::Texture,
    ao: wgpu::Buffer,
    next_occ: wgpu::Buffer,
    gi_rays: wgpu::Buffer,
}

fn reflection_extent(width: u32, height: u32) -> (u32, u32) {
    let divisor = width.max(height).div_ceil(128).max(1);
    (width.div_ceil(divisor), height.div_ceil(divisor))
}

impl Targets {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let image = |name, format, width, height| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(name),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let (rw, rh) = reflection_extent(width, height);
        Self {
            hdr: image(
                "composited HDR/depth",
                wgpu::TextureFormat::Rgba32Float,
                width,
                height,
            ),
            albedo: image(
                "scene albedo",
                wgpu::TextureFormat::Rgba8Unorm,
                width,
                height,
            ),
            normal: image(
                "scene octahedral normal",
                wgpu::TextureFormat::Rg16Float,
                width,
                height,
            ),
            depth: image(
                "scene window depth",
                wgpu::TextureFormat::R32Float,
                width,
                height,
            ),
            scene_z: image(
                "scene depth attachment",
                wgpu::TextureFormat::Depth32Float,
                width,
                height,
            ),
            reflection: image("water reflection", wgpu::TextureFormat::Rgba16Float, rw, rh),
            reflection_z: image(
                "reflection depth attachment",
                wgpu::TextureFormat::Depth32Float,
                rw,
                rh,
            ),
            ao: storage(device, "ao_work", u64::from(width) * u64::from(height) * 16),
            next_occ: storage(device, "next occlusion", occlusion_bytes(width, height)),
            gi_rays: storage(
                device,
                "GI quarter-resolution rays",
                u64::from(width.div_ceil(4)) * u64::from(height.div_ceil(4)) * 112,
            ),
        }
    }

    fn record_clears(&self, encoder: &mut wgpu::CommandEncoder) {
        // Generated draws load their attachments. Ground, props and water share
        // scene_z. Water runs after props; the separate opaque depth image stays
        // unchanged for AO, GI and tracing.
        for (texture, color) in [
            (
                &self.albedo,
                wgpu::Color {
                    r: 0.74,
                    g: 0.80,
                    b: 0.86,
                    a: 0.0,
                },
            ),
            (&self.normal, wgpu::Color::TRANSPARENT),
            (&self.depth, wgpu::Color::WHITE),
        ] {
            let view = texture.create_view(&Default::default());
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear color"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        for texture in [&self.scene_z, &self.reflection_z] {
            let view = texture.create_view(&Default::default());
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear depth"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
        }
    }
}

struct SunShadows {
    direction: [f32; 3],
    casters: Vec<shadow::Caster>,
    grid: Option<wgpu::Buffer>,
    geometry_dirty: bool,
    grid_dirty: bool,
    generation: u64,
    geometry_exports: u64,
}

pub struct Renderer {
    pub gfx: Gfx,
    pub frame: u32,
    pub gi_mode: u32,
    pub textures_enabled: bool,
    pub taa_enabled: bool,
    /// Screenshot-only cloud controls: offset xy, coverage strength z.
    pub sky_clouds: [f32; 3],
    // Retain Wyn's pipelines and scratch buffers across frames and resizes.
    host: generated::HostContext,
    world: World,
    targets: Targets,
    temporal: TemporalState,
    materials: Materials,
    vehicle: crate::vehicle::Vehicle,
    reflection_sampler: wgpu::Sampler,
    terrain: wgpu::Buffer,
    terrain_cells: Vec<[f32; 4]>,
    shadows: SunShadows,
    events: wgpu::Buffer,
    globals: wgpu::Buffer,
    start: Instant,
    previous_camera: Camera,
    gi_reset_frames: u32,
    previous_textures_enabled: bool,
    // Allocated only for screenshot measurements; interactive frames do no queries.
    timing: Option<(wgpu::QuerySet, wgpu::Buffer)>,
}

impl Renderer {
    pub fn new(gfx: Gfx, cam: &Camera, mods: u32, time: f32) -> Result<Self> {
        let (w, h) = (gfx.config.width, gfx.config.height);
        let bytes = frame_bytes(
            w,
            h,
            cam,
            mods,
            time,
            &[0.0, 0.0, 1.0],
            &FrameHistory {
                camera: *cam,
                frame_index: 0,
                valid: 0,
                gi_mode: 0,
                textures_enabled: 1,
                temporal: TemporalFrame::default(),
            },
        )?;
        let globals = gfx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        gfx.queue.write_buffer(&globals, 0, &bytes);
        let cells = terrain::demo();
        let terrain = storage(&gfx.device, "terrain cells", (cells.len() * 16) as u64);
        let mut renderer = Self {
            host: generated::HostContext::new(&gfx.device).context("initialize Wyn host")?,
            world: World::new(&gfx.device, w, h),
            targets: Targets::new(&gfx.device, w, h),
            temporal: TemporalState::new(&gfx.device, w, h),
            materials: Materials::new(&gfx.device, &gfx.queue),
            vehicle: crate::vehicle::Vehicle::new(&gfx.device, &gfx.queue).context("load Fiat")?,
            reflection_sampler: gfx.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("filtered water reflection"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            terrain,
            terrain_cells: Vec::new(),
            shadows: SunShadows {
                direction: shadow::sun_direction(shadow::DEFAULT_SUN)?,
                casters: Vec::new(),
                grid: None,
                geometry_dirty: true,
                grid_dirty: true,
                generation: 0,
                geometry_exports: 0,
            },
            events: storage(&gfx.device, "events", EV_CAP as u64 * 16),
            gfx,
            globals,
            frame: 0,
            gi_mode: 0,
            textures_enabled: true,
            taa_enabled: true,
            sky_clouds: [0.0, 0.0, 1.0],
            start: Instant::now(),
            previous_camera: *cam,
            gi_reset_frames: 1,
            previous_textures_enabled: true,
            timing: None,
        };
        renderer.set_terrain_cells(&cells)?;
        Ok(renderer)
    }

    /// Route terrain edits through here so bank geometry and its shadows agree.
    pub fn set_terrain_cells(&mut self, cells: &[[f32; 4]]) -> Result<()> {
        anyhow::ensure!(
            cells.len() == terrain::SIDE * terrain::SIDE,
            "wrong terrain cell count"
        );
        anyhow::ensure!(
            cells.iter().flatten().all(|v| v.is_finite()),
            "nonfinite terrain cell"
        );
        if self.terrain_cells == cells {
            return Ok(());
        }
        self.gfx
            .queue
            .write_buffer(&self.terrain, 0, bytemuck::cast_slice(cells));
        self.terrain_cells = cells.to_vec();
        self.shadows.geometry_dirty = true;
        self.shadows.grid_dirty = true;
        self.gi_reset_frames = 1;
        Ok(())
    }

    pub fn set_sun_direction(&mut self, direction: [f32; 3]) -> Result<()> {
        let direction = shadow::sun_direction(direction)?;
        if self.shadows.direction != direction {
            self.shadows.direction = direction;
            self.shadows.grid_dirty = true;
            self.gi_reset_frames = 1;
        }
        Ok(())
    }

    /// Export Wyn's actual placement geometry on edits, then retain the index.
    /// A sun change reuses the cached boxes. Ordinary frames do neither readback
    /// nor grid construction. Building definitions are regenerated on app reload.
    fn rebuild_sun_shadows(&mut self) -> Result<()> {
        if !self.shadows.grid_dirty {
            return Ok(());
        }
        let start = Instant::now();
        if self.shadows.geometry_dirty {
            let mut encoder = self.gfx.device.create_command_encoder(&Default::default());
            let output =
                generated::encode_shadow_geometry(&mut self.host, &mut encoder, &self.terrain)
                    .context("export shadow geometry")?;
            self.gfx.queue.submit(Some(encoder.finish()));
            let Some(OutputResource::Buffer { buffer, .. }) =
                output.values.first().map(|v| &v.resource)
            else {
                bail!("shadow geometry entry did not return a buffer");
            };
            self.shadows.casters = shadow::casters(&read_buffer(&self.gfx, buffer)?)?;
            self.shadows.geometry_exports += 1;
            self.shadows.geometry_dirty = false;
        }
        let grid = shadow::build(&self.shadows.casters, self.shadows.direction)?;
        let bytes = bytemuck::cast_slice(&grid.records);
        let buffer = storage(
            &self.gfx.device,
            "geometric sun shadows",
            bytes.len() as u64,
        );
        self.gfx.queue.write_buffer(&buffer, 0, bytes);
        self.shadows.grid = Some(buffer);
        self.shadows.grid_dirty = false;
        self.shadows.generation += 1;
        self.gi_reset_frames = 1;
        eprintln!(
            "sun grid: {} boxes, {} references, max {} per cell, {:.2} MiB; rebuild {:.1} ms",
            self.shadows.casters.len(),
            grid.references,
            grid.max_candidates,
            bytes.len() as f64 / 1048576.0,
            start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0
            || height == 0
            || (width, height) == (self.gfx.config.width, self.gfx.config.height)
        {
            return;
        }
        self.gfx.resize(width, height);
        self.targets = Targets::new(&self.gfx.device, width, height);
        self.temporal = TemporalState::new(&self.gfx.device, width, height);
        // The painting survives; screen-space occlusion history must be reset.
        self.world.occ = storage(
            &self.gfx.device,
            "previous occlusion",
            occlusion_bytes(width, height),
        );
        self.world.gi = storage(
            &self.gfx.device,
            "previous GI",
            gi_bytes(width, height, self.gi_mode == 3),
        );
        self.gi_reset_frames = 1;
    }

    pub fn upload_events(&mut self, events: &[[f32; 4]]) {
        let mut padded = [[0f32; 4]; EV_CAP];
        let count = events.len().min(EV_CAP);
        padded[..count].copy_from_slice(&events[..count]);
        // Wyn tracks committed paint edits alongside the retained world and
        // invalidates reference GI when they are rendered. Raw input never resets GI.
        self.gfx
            .queue
            .write_buffer(&self.events, 0, bytemuck::cast_slice(&padded));
    }

    fn temporal_key(&self, mods: u32) -> HistoryKey {
        HistoryKey {
            enabled: self.taa_enabled && self.gi_mode != 3,
            gi_mode: self.gi_mode,
            textures: self.textures_enabled,
            mods,
            sky: self.sky_clouds,
        }
    }

    fn encode_frame(
        &mut self,
        target: &wgpu::Texture,
        cam: &Camera,
        mods: u32,
        time: f32,
    ) -> Result<(wgpu::CommandBuffer, generated::output::OutputDescriptor)> {
        self.rebuild_sun_shadows()?;
        if self.textures_enabled != self.previous_textures_enabled {
            self.gi_reset_frames = 1;
        }
        let history_bytes = gi_bytes(
            self.gfx.config.width,
            self.gfx.config.height,
            self.gi_mode == 3,
        );
        if self.world.gi.size() != history_bytes {
            self.world.gi = storage(&self.gfx.device, "previous GI", history_bytes);
            self.gi_reset_frames = 1;
        }
        let bytes = frame_bytes(
            self.gfx.config.width,
            self.gfx.config.height,
            cam,
            mods,
            time,
            &self.sky_clouds,
            &FrameHistory {
                camera: self.previous_camera,
                frame_index: self.frame,
                valid: u32::from(self.gi_reset_frames == 0),
                gi_mode: self.gi_mode,
                textures_enabled: u32::from(self.textures_enabled),
                temporal: self.temporal.frame(
                    cam,
                    self.temporal_key(mods),
                    self.gi_reset_frames != 0,
                ),
            },
        )?;
        self.gfx.queue.write_buffer(&self.globals, 0, &bytes);
        let mut encoder = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tinyporto frame"),
            });
        if let Some((queries, _)) = &self.timing {
            encoder.write_timestamp(queries, 0);
        }
        self.targets.record_clears(&mut encoder);
        let output = generated::encode_tinyporto_frame(
            &mut self.host,
            &mut encoder,
            &self.world.head,
            &self.world.ui,
            &self.events,
            &self.globals,
            &self.world.points,
            &self.world.items,
            &self.terrain,
            &self.world.occ,
            &self.world.gi,
            &self.materials.brick.color,
            &self.materials.brick.normal,
            &self.materials.stone.color,
            &self.materials.stone.normal,
            &self.materials.mortar.color,
            &self.materials.mortar.normal,
            &self.vehicle.vertices,
            &self.vehicle.indices,
            &self.vehicle.vent,
            &self.vehicle.engine,
            &self.vehicle.sampler,
            &self.materials.sampler,
            &self.reflection_sampler,
            self.shadows.grid.as_ref().context("sun grid is missing")?,
            &self.targets.albedo,
            &self.targets.normal,
            &self.targets.depth,
            &self.targets.ao,
            &self.targets.next_occ,
            &self.targets.gi_rays,
            &self.targets.reflection,
            self.temporal.input(),
            &self.targets.hdr,
            self.temporal.output(),
            target,
            &self.targets.reflection_z,
            &self.targets.reflection_z,
            &self.targets.scene_z,
            &self.targets.scene_z,
            &self.targets.scene_z,
            &self.targets.scene_z,
        )
        .context("execute Wyn frame")?;
        if let Some((queries, result)) = &self.timing {
            encoder.write_timestamp(queries, 1);
            encoder.resolve_query_set(queries, 0..2, result, 0);
        }
        Ok((encoder.finish(), output))
    }

    // Build lazy render pipelines on the startup worker. Discard the recorded
    // commands so this cannot consume input or advance simulation/GI history.
    pub fn prepare(&mut self, cam: &Camera, mods: u32) -> Result<()> {
        let target = self.gfx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pipeline preparation"),
            size: wgpu::Extent3d {
                width: self.gfx.config.width,
                height: self.gfx.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.gfx.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let _ = self.encode_frame(&target, cam, mods, 0.0)?;
        self.start = Instant::now();
        Ok(())
    }

    fn execute(
        &mut self,
        target: &wgpu::Texture,
        cam: &Camera,
        mods: u32,
        time: f32,
    ) -> Result<()> {
        let (commands, output) = self.encode_frame(target, cam, mods, time)?;
        self.gfx.queue.submit(Some(commands));
        let next = World::from_output(&output)?;
        let previous = std::mem::replace(&mut self.world, next);
        // The caller-provided occlusion output cannot alias next frame's input.
        self.targets.next_occ = previous.occ;
        self.temporal.commit(*cam, self.temporal_key(mods));
        self.frame = self.frame.wrapping_add(1);
        self.previous_camera = *cam;
        self.previous_textures_enabled = self.textures_enabled;
        self.gi_reset_frames = self.gi_reset_frames.saturating_sub(1);
        Ok(())
    }

    pub fn render(&mut self, cam: &Camera, mods: u32) -> Result<()> {
        let surface = self
            .gfx
            .surface
            .as_ref()
            .context("window mode needs a surface")?;
        let frame = match surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                surface.configure(&self.gfx.device, &self.gfx.config);
                return Ok(());
            }
            Err(error) => bail!("acquire frame: {error}"),
        };
        self.execute(
            &frame.texture,
            cam,
            mods,
            self.start.elapsed().as_secs_f32(),
        )?;
        frame.present();
        Ok(())
    }

    fn buffer(&self, name: &str) -> Option<&wgpu::Buffer> {
        self.world.buffer(name).or_else(|| match name {
            "events" => Some(&self.events),
            "frame" => Some(&self.globals),
            "ao_work" => Some(&self.targets.ao),
            "terrain" => Some(&self.terrain),
            _ => None,
        })
    }

    pub fn dump_buffer(&self, name: &str) -> Result<()> {
        let buffer = self.buffer(name).with_context(|| format!(
            "no exposed buffer '{name}'; available: uistate, points, items, head, occ, gi, events, frame, terrain, ao_work"
        ))?;
        let bytes = read_buffer(&self.gfx, buffer)?;
        let words: Vec<_> = bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        eprintln!("{name}: {} bytes / {} words", bytes.len(), words.len());
        let show = |label, start: usize, count: usize| {
            let end = (start + count).min(words.len());
            eprintln!("  {label} [{start}..{end}]: {:?}", &words[start..end]);
        };
        show("head", 0, 48);
        if words.len() > 96 {
            show("mid", words.len() / 2, 24);
            show("tail", words.len() - 24, 24);
        }
        Ok(())
    }
    /// Headless: render the demo scene into an offscreen texture and write it
    /// to `path` as a PNG. Used to eyeball the pipeline without a window.
    pub fn screenshot(
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

        // Warm the renderer and occlusion history without painting into the scene.
        let total = 40u32;
        let mut frame_ms: Vec<f32> = Vec::new();
        let mut submit_ms = Vec::new();
        let mut gpu_ms = Vec::new();
        if self
            .gfx
            .device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
        {
            self.timing = Some((
                self.gfx.device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("frame timing"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                self.gfx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("frame timestamps"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
            ));
        }
        self.upload_events(&[]);
        for _ in 0..total {
            let t0 = Instant::now();
            self.execute(&tex, cam, mods, time)?;
            submit_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
            self.gfx.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })?;
            frame_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
            if let Some((_, result)) = &self.timing {
                let bytes = read_buffer(&self.gfx, result)?;
                let begin = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let end = u64::from_le_bytes(bytes[8..].try_into().unwrap());
                gpu_ms.push(
                    end.wrapping_sub(begin) as f32 * self.gfx.queue.get_timestamp_period()
                        / 1_000_000.0,
                );
            }
        }
        self.timing = None;
        let mut steady: Vec<f32> = frame_ms.split_off(5);
        steady.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!("frame time: median {:.1} ms", steady[steady.len() / 2]);
        let mut submit = submit_ms.split_off(5);
        submit.sort_by(f32::total_cmp);
        eprintln!(
            "CPU encode/submit: median {:.1} ms",
            submit[submit.len() / 2]
        );
        if gpu_ms.len() > 5 {
            let mut gpu = gpu_ms.split_off(5);
            gpu.sort_by(f32::total_cmp);
            eprintln!("GPU frame: median {:.1} ms", gpu[gpu.len() / 2]);
        }

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

        let data = mapped_bytes(&self.gfx.device, &readback)?;

        // Drop the row padding into a tight RGBA8 buffer.
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);

        let file =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        // The offscreen sRGB attachment already encoded these display bytes.
        enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        enc.write_header()?.write_image_data(&pixels)?;
        eprintln!("wrote {} ({w}x{h})", path.display());
        Ok(())
    }
}

fn mapped_bytes(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>> {
    let (send, receive) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;
    receive.recv().context("readback callback")??;
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    Ok(bytes)
}

fn read_buffer(gfx: &Gfx, buffer: &wgpu::Buffer) -> Result<Vec<u8>> {
    let staging = gfx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("buffer readback"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gfx.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
    gfx.queue.submit(Some(encoder.finish()));
    mapped_bytes(&gfx.device, &staging)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode_event, EV_MOUSEDOWN};

    fn offscreen(gfx: &Gfx) -> wgpu::Texture {
        gfx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test frame"),
            size: wgpu::Extent3d {
                width: gfx.config.width,
                height: gfx.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: gfx.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    #[test]
    fn generated_frames_retain_input_state_and_resize_occlusion_history() {
        let gfx = Gfx::new_headless(64, 48).expect("GPU device");
        let cam = Camera::default();
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        let target = offscreen(&renderer.gfx);
        // Tab selects building for the next frame; this frame still paints fence.
        renderer.upload_events(&[
            encode_event(crate::EV_KEYDOWN, 0, 1.0, 0.0, 0.0),
            encode_event(EV_MOUSEDOWN, 0, 0.0, 32.0, 24.0),
        ]);
        let initial_ui = read_buffer(&renderer.gfx, &renderer.world.ui).unwrap();
        renderer.prepare(&cam, 0).unwrap();
        assert_eq!(renderer.frame, 0);
        assert_eq!(
            read_buffer(&renderer.gfx, &renderer.world.ui).unwrap(),
            initial_ui
        );
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let ui = read_buffer(&renderer.gfx, &renderer.world.ui).unwrap();
        assert_eq!(f32::from_le_bytes(ui[..4].try_into().unwrap()), 1.0);
        let head = read_buffer(&renderer.gfx, &renderer.world.head).unwrap();
        assert_eq!(f32::from_le_bytes(head[..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(head[4..8].try_into().unwrap()), 1.0);
        assert_eq!(paint_edit_frames(&renderer), 16.0);
        let items = read_buffer(&renderer.gfx, &renderer.world.items).unwrap();
        assert_eq!(f32::from_le_bytes(items[..4].try_into().unwrap()), 2.0);
        let points = read_buffer(&renderer.gfx, &renderer.world.points).unwrap();
        let occlusion = read_buffer(&renderer.gfx, &renderer.world.occ).unwrap();
        assert!(occlusion
            .chunks_exact(4)
            .any(|word| f32::from_le_bytes(word.try_into().unwrap()) > 0.0));
        // An empty batch must replace the old key/button events, not replay them.
        renderer.upload_events(&[]);
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        assert_eq!(read_buffer(&renderer.gfx, &renderer.world.ui).unwrap(), ui);
        assert_eq!(
            &read_buffer(&renderer.gfx, &renderer.world.head).unwrap()[..40],
            &head[..40]
        );
        assert_eq!(paint_edit_frames(&renderer), 15.0);
        assert_eq!(
            read_buffer(&renderer.gfx, &renderer.world.points).unwrap(),
            points
        );
        // Odd dimensions cover ceil-divided tiles and replace both history buffers.
        renderer.resize(81, 55);
        assert_eq!(
            (renderer.gfx.config.width, renderer.gfx.config.height),
            (81, 55)
        );
        assert_eq!(renderer.world.occ.size(), 11 * 7 * 4);
        assert_eq!(renderer.targets.next_occ.size(), 11 * 7 * 4);
        assert_eq!(renderer.targets.ao.size(), 81 * 55 * 16);
        assert!(read_buffer(&renderer.gfx, &renderer.world.occ)
            .unwrap()
            .iter()
            .all(|&b| b == 0));
        let resized = offscreen(&renderer.gfx);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(
            &read_buffer(&renderer.gfx, &renderer.world.head).unwrap()[..40],
            &head[..40]
        );
        assert_eq!(paint_edit_frames(&renderer), 14.0);
        assert_eq!(
            read_buffer(&renderer.gfx, &renderer.world.points).unwrap(),
            points
        );
        // Cycling again wraps to fence; the current building tool paints kind 3.
        renderer.upload_events(&[
            encode_event(crate::EV_KEYDOWN, 0, 1.0, 0.0, 0.0),
            encode_event(EV_MOUSEDOWN, 0, 0.0, 40.0, 27.0),
        ]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        let ui = read_buffer(&renderer.gfx, &renderer.world.ui).unwrap();
        assert_eq!(f32::from_le_bytes(ui[..4].try_into().unwrap()), 0.0);
        let items = read_buffer(&renderer.gfx, &renderer.world.items).unwrap();
        assert_eq!(f32::from_le_bytes(items[16..20].try_into().unwrap()), 3.0);
        assert_eq!(renderer.frame, 4);
    }

    fn paint_edit_frames(renderer: &Renderer) -> f32 {
        let head = read_buffer(&renderer.gfx, &renderer.world.head).unwrap();
        f32::from_le_bytes(head[40..44].try_into().unwrap())
    }

    fn gi_samples(renderer: &Renderer) -> Vec<[f32; 28]> {
        read_buffer(&renderer.gfx, &renderer.world.gi)
            .unwrap()
            .chunks_exact(112)
            .map(|pixel| {
                std::array::from_fn(|i| {
                    f32::from_le_bytes(pixel[i * 4..i * 4 + 4].try_into().unwrap())
                })
            })
            .collect()
    }

    fn image_bytes(gfx: &Gfx, texture: &wgpu::Texture) -> Vec<u8> {
        let bytes_per_pixel = texture.format().block_copy_size(None).unwrap();
        let row_bytes = texture.width() * bytes_per_pixel;
        let stride = row_bytes.div_ceil(256) * 256;
        let buffer = gfx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test image readback"),
            size: u64::from(stride) * u64::from(texture.height()),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gfx.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(texture.height()),
                },
            },
            texture.size(),
        );
        gfx.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).unwrap();
        });
        gfx.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();
        let mapped = slice.get_mapped_range();
        let result = mapped
            .chunks_exact(stride as usize)
            .flat_map(|row| row[..row_bytes as usize].iter().copied())
            .collect();
        drop(mapped);
        buffer.unmap();
        result
    }

    #[test]
    fn solar_disk_tracks_shadow_direction_and_sky_is_directional() {
        let (w, h) = (321u32, 201u32);
        let gfx = Gfx::new_headless(w, h).expect("GPU device");
        let cam = crate::sun_demo_camera(shadow::DEFAULT_SUN, crate::SunDemo::Clear).unwrap();
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        renderer.gi_mode = 1;
        renderer.sky_clouds = [0.0, 0.0, 0.0];
        let target = offscreen(&renderer.gfx);
        let pixel = |image: &[u8], x: usize, y: usize| -> [u8; 3] {
            image[(y * w as usize + x) * 4..][..3].try_into().unwrap()
        };
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let clear = image_bytes(&renderer.gfx, &target);
        assert!(pixel(&clear, 160, 100).iter().all(|&c| c >= 247));
        // The art-directed 1.6-degree disk spans about 15.9 pixels at this FOV.
        // Check the actual rendered extent, and reject any bright flare ghosts.
        let bright: Vec<_> = clear
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, p)| p[..3].iter().all(|&c| c >= 247))
            .map(|(i, _)| ((i % w as usize) as i32, (i / w as usize) as i32))
            .collect();
        assert!(
            (100..=240).contains(&bright.len()),
            "solar footprint: {}",
            bright.len()
        );
        assert!(bright
            .iter()
            .all(|&(x, y)| (x - 160).abs() <= 9 && (y - 100).abs() <= 9));

        let away = crate::sun_demo_camera(shadow::DEFAULT_SUN, crate::SunDemo::Away).unwrap();
        renderer.execute(&target, &away, 0, 0.0).unwrap();
        let back = image_bytes(&renderer.gfx, &target);
        // Compare away from the compact emitter at matching elevations.
        let front_color = pixel(&clear, 30, 100);
        let back_color = pixel(&back, 30, 100);
        assert!(back_color[0] + 8 < front_color[0] && back_color[1] + 8 < front_color[1]);
        assert!(
            back_color[2] > back_color[1] + 20,
            "away sky must retain its deep blue"
        );
        assert!(
            back_color[1] > back_color[0] + 20,
            "away hue must not turn violet"
        );

        // Check the actual orbit camera range, not only the raised sun demo.
        // At the highest legal pitch the upper edge sees about 8 degrees of sky.
        let mut low = Camera::default();
        low.set([0.0, 0.0, 0.0], away.az, -0.03, 45.0);
        renderer.execute(&target, &low, 0, 0.0).unwrap();
        let low_back = pixel(&image_bytes(&renderer.gfx, &target), 160, 2);
        low.set([0.0, 0.0, 0.0], cam.az, -0.03, 45.0);
        renderer.execute(&target, &low, 0, 0.0).unwrap();
        let low_front = pixel(&image_bytes(&renderer.gfx, &target), 160, 2);
        assert!(low_back[0] < 140 && low_back[1] > low_back[0] + 20);
        assert!(low_back[2] > low_back[1] + 25);
        assert!(
            low_front[0] > low_back[0] + 20 && low_front[1] > low_back[1] + 20,
            "directional blue must be visible at the gameplay camera angle"
        );

        renderer.sky_clouds = [0.0, 0.0, 1.0]; // known dense cloud at the default sun
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let clouded = image_bytes(&renderer.gfx, &target);
        assert!(pixel(&clouded, 160, 100)[0] + 8 < pixel(&clear, 160, 100)[0]);

        renderer.sky_clouds = [0.0, 0.0, 0.0];
        let (_, shifted_sun) = cam.cursor_ray(w as f32, h as f32, 210.5, 100.5);
        renderer.set_sun_direction(shifted_sun).unwrap();
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let shifted = image_bytes(&renderer.gfx, &target);
        assert!(pixel(&shifted, 210, 100).iter().all(|&c| c >= 247));
        assert!(
            pixel(&shifted, 160, 100)[0] < 240,
            "disk must leave the old lighting direction"
        );
        assert_eq!(renderer.shadows.generation, 2);
        assert_eq!(renderer.shadows.geometry_exports, 1);
    }

    #[test]
    fn textures_change_materials_preserve_geometry_and_reset_gi() {
        let gfx = Gfx::new_headless(160, 120).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -0.7, 18.0);
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        let target = offscreen(&renderer.gfx);
        renderer.textures_enabled = false;
        for _ in 0..3 {
            renderer.execute(&target, &cam, 0, 0.0).unwrap();
        }
        let color = image_bytes(&renderer.gfx, &renderer.targets.albedo);
        let normal = image_bytes(&renderer.gfx, &renderer.targets.normal);
        let depth = image_bytes(&renderer.gfx, &renderer.targets.depth);
        renderer.textures_enabled = true;
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let textured = image_bytes(&renderer.gfx, &renderer.targets.albedo);
        let normals = image_bytes(&renderer.gfx, &renderer.targets.normal);
        assert_eq!(
            depth,
            image_bytes(&renderer.gfx, &renderer.targets.depth),
            "material maps must not move surfaces"
        );
        assert!(
            color
                .chunks_exact(4)
                .zip(textured.chunks_exact(4))
                .filter(|(a, b)| a[..3] != b[..3])
                .count()
                > 1000,
            "albedo maps must affect visible surfaces"
        );
        assert!(
            normal
                .chunks_exact(4)
                .zip(normals.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
                > 1000,
            "normal maps must reach the G-buffer"
        );
        assert!(
            textured
                .chunks_exact(4)
                .filter(|p| (185..254).contains(&p[3]))
                .count()
                > 100,
            "roughness must be packed into surface alpha"
        );
        // Vehicle paint/glass can be smoother than masonry. The G-buffer's
        // surface tag is 0.5 + 0.5 * roughness, including roughness near zero.
        assert!(textured.chunks_exact(4).all(|p| p[3] == 0 || p[3] >= 128));
        assert!(
            gi_samples(&renderer).iter().all(|p| p[3] <= 1.0),
            "material changes must invalidate lighting history"
        );
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        assert_eq!(
            textured,
            image_bytes(&renderer.gfx, &renderer.targets.albedo),
            "texture mapping must be stable across frames"
        );
        renderer.textures_enabled = false;
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        assert_eq!(color, image_bytes(&renderer.gfx, &renderer.targets.albedo));
        assert_eq!(normal, image_bytes(&renderer.gfx, &renderer.targets.normal));
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));
    }

    #[test]
    fn fiat_mesh_has_opaque_windows_textured_rear_panels_and_sun_shadow() {
        let (width, height) = (640, 480);
        let gfx = Gfx::new_headless(width, height).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([3.0, 0.65, 2.1], 0.6, -0.4, 9.0);
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        renderer.gi_mode = 1;
        let target = offscreen(&renderer.gfx);
        for (name, azimuth) in [("fiat-front", 0.6), ("fiat-rear", -2.15)] {
            cam.set([3.0, 0.65, 2.1], azimuth, -0.4, 9.0);
            renderer.textures_enabled = false;
            renderer.execute(&target, &cam, 0, 0.0).unwrap();
            let flat = image_bytes(&renderer.gfx, &renderer.targets.albedo);
            renderer.textures_enabled = true;
            for _ in 0..3 {
                renderer.execute(&target, &cam, 0, 0.0).unwrap();
            }
            let albedo = image_bytes(&renderer.gfx, &renderer.targets.albedo);
            let depths = image_depths(&renderer.gfx, &renderer.targets.depth);
            let eye = cam.eye();
            let mut forward: [f32; 3] = std::array::from_fn(|i| cam.target[i] - eye[i]);
            let length = forward.iter().map(|x| x * x).sum::<f32>().sqrt();
            forward.iter_mut().for_each(|x| *x /= length);
            let (mut paint, mut windows, mut vent_texels) = (0, 0, 0);
            for (i, rgba) in albedo.chunks_exact(4).enumerate() {
                let (_, ray) = cam.cursor_ray(
                    width as f32,
                    height as f32,
                    (i % width as usize) as f32 + 0.5,
                    (i / width as usize) as f32 + 0.5,
                );
                let cosine = ray.iter().zip(forward).map(|(a, b)| a * b).sum::<f32>();
                let view_depth = 100.0 / (1000.0 - depths[i] * 999.9);
                let p: [f32; 3] =
                    std::array::from_fn(|axis| eye[axis] + ray[axis] * view_depth / cosine);
                let dx = p[0] - 3.0;
                let dz = p[2] - 2.1;
                let x = dx * 0.12f32.cos() - dz * 0.12f32.sin();
                let z = dz * 0.12f32.cos() + dx * 0.12f32.sin();
                let on_car = x.abs() < 0.66 && z.abs() < 1.51 && p[1] > 0.25 && p[1] < 1.42;
                if rgba[0] > 100 && rgba[1] < 15 && rgba[2] < 15 && on_car {
                    paint += 1;
                }
                if rgba[..3] == [14, 19, 24] {
                    assert!(on_car && p[1] > 0.90, "window must write opaque car depth");
                    windows += 1;
                }
                if on_car
                    && z < -1.02
                    && x.abs() < 0.38
                    && (0.47..1.02).contains(&p[1])
                    && rgba[..3] != flat[i * 4..i * 4 + 3]
                {
                    vent_texels += 1;
                }
            }
            assert!(paint > 1000, "{name}: missing red mesh ({paint})");
            assert!(windows > 100, "{name}: missing opaque glass ({windows})");
            if name == "fiat-rear" {
                assert!(
                    vent_texels > 100,
                    "rear panel texture missing ({vent_texels})"
                );
            }
            save_quality_frame(name, &renderer.gfx, &target);
        }
        renderer.set_sun_direction([0.0, 1.0, 0.0]).unwrap();
        assert_eq!(
            shadow_queries(
                &mut renderer,
                &[[3.0, 0.06, 2.1, 0.0], [4.4, 0.06, 2.1, 0.0]]
            ),
            [1.0, 0.0],
            "car must cast a geometric shadow onto the cobbles"
        );
    }

    #[test]
    fn gi_accumulates_reprojects_and_adapts_to_paint() {
        let gfx = Gfx::new_headless(80, 60).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -1.1, 25.0);
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        // Preserve the original flat-ground lighting fixture. Canal geometry has
        // its own coverage below, including the exposed bed and raised coping.
        renderer
            .set_terrain_cells(&vec![terrain::LAND; terrain::SIDE * terrain::SIDE])
            .unwrap();
        let target = offscreen(&renderer.gfx);
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let first_buffer = renderer.world.gi.clone();
        let first_bytes = read_buffer(&renderer.gfx, &first_buffer).unwrap();
        let first = gi_samples(&renderer);
        assert_eq!(first.len(), 40 * 30);
        assert!(first.iter().filter(|p| p[3] == 1.0).count() > 500);
        for _ in 0..39 {
            renderer.execute(&target, &cam, 0, 0.0).unwrap();
        }
        // Retained input must never be overwritten by the generated output.
        assert_eq!(
            read_buffer(&renderer.gfx, &first_buffer).unwrap(),
            first_bytes
        );
        let settled = gi_samples(&renderer);
        assert!(settled.iter().filter(|p| p[3] >= 10.0).count() > 500);
        assert!(settled.iter().all(|p| p[3] <= 32.0));
        assert!(settled.iter().flatten().all(|v| v.is_finite()));
        assert!(settled
            .iter()
            .all(|p| p[..3].iter().all(|&v| (0.0..8.0).contains(&v))));

        // Open ground receives blue sky; the open-topped brick rooms receive
        // occluded sky and warmer bounced light. Check actual traced illumination,
        // not display mapping or a uniform ambient multiplier.
        let mean = |inside: bool| {
            let samples: Vec<_> = settled
                .iter()
                .filter(|p| {
                    let in_room = (p[4] - 2.6).abs() < 0.9 && (p[6] + 1.0).abs() < 0.5;
                    let open = p[4].abs() < 1.0 && p[6].abs() < 1.0;
                    p[7] > 0.5 && p[5] < 0.08 && if inside { in_room } else { open }
                })
                .collect();
            assert!(
                samples.len() >= 3,
                "missing GI test surfaces: {}",
                samples.len()
            );
            let mut rgb = [0.0f32; 3];
            for pixel in &samples {
                for i in 0..3 {
                    rgb[i] += pixel[i] / samples.len() as f32;
                }
            }
            rgb
        };
        let room = mean(true);
        let open = mean(false);
        eprintln!("GI mean: room {room:?}, open ground {open:?}");
        assert!(
            room[0] / room[2].max(0.001) > open[0] / open[2].max(0.001) + 0.1,
            "brick room must receive colored bounce light"
        );
        assert!(room[2] < open[2] * 0.8, "walls must occlude blue sky light");

        let rays = read_buffer(&renderer.gfx, &renderer.targets.gi_rays).unwrap();
        assert_eq!(rays.len(), 20 * 15 * 112);
        let ray_guides: Vec<_> = rays
            .chunks_exact(112)
            .map(|p| {
                (
                    f32::from_le_bytes(p[104..108].try_into().unwrap()),
                    f32::from_le_bytes(p[108..112].try_into().unwrap()),
                    f32::from_le_bytes(p[12..16].try_into().unwrap()),
                )
            })
            .collect();
        assert!(
            ray_guides.iter().any(|p| p.0 == 1.0),
            "screen-space hits must contribute"
        );
        assert!(
            ray_guides
                .iter()
                .any(|p| p.0 == 0.0 && p.1 == 0.0 && p.2 == 1.0),
            "world-space fallback must hit geometry"
        );
        assert!(
            ray_guides.iter().any(|p| p.1 == 1.0),
            "sky misses must contribute"
        );

        // Compare against independent, full-resolution, 512-sample explicit paths.
        // The reference bypasses SH, feedback and spatial filtering, and switches
        // history dimensions. Compare the exact primary pixels traced by realtime.
        renderer.gi_mode = 3;
        for _ in 0..64 {
            renderer.execute(&target, &cam, 0, 0.0).unwrap();
        }
        let reference = gi_samples(&renderer);
        assert_eq!(reference.len(), 80 * 60);
        assert!(reference.iter().flatten().all(|v| v.is_finite()));
        assert!(reference.iter().any(|p| p[3] > 32.0));
        assert!(reference
            .iter()
            .all(|p| p[12..24].iter().all(|&v| v == 0.0)));
        let mut squared_error = 0.0;
        let mut energy = 0.0;
        let mut count = 0;
        for (i, sample) in settled.iter().enumerate() {
            let exact = &reference[(i / 40 * 2 + 1) * 80 + i % 40 * 2 + 1];
            if sample[7] > 0.5 && exact[7] > 0.5 {
                for c in 0..3 {
                    squared_error += (sample[c] - exact[c]).powi(2);
                    energy += exact[c].powi(2);
                    count += 1;
                }
            }
        }
        assert!(count > 1500);
        let relative_rmse = (squared_error / energy.max(0.001)).sqrt();
        eprintln!("GI relative RMS error against reference: {relative_rmse:.3}");
        assert!(
            relative_rmse < 0.4,
            "realtime GI departed from reference lighting"
        );
        renderer.gi_mode = 0;
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        assert_eq!(gi_samples(&renderer).len(), 40 * 30);
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));
        renderer.execute(&target, &cam, 0, 0.0).unwrap();

        // A modest orbit keeps some history and rejects newly exposed surfaces.
        cam.az += 0.18;
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let moved = gi_samples(&renderer);
        assert!(moved.iter().any(|p| p[3] > 1.0));
        assert!(moved.iter().any(|p| p[3] == 1.0));

        renderer.gi_mode = 1;
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        assert!(gi_samples(&renderer).iter().flatten().all(|&v| v == 0.0));
        renderer.gi_mode = 0;
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));

        renderer.resize(81, 55);
        assert_eq!(renderer.world.gi.size(), 41 * 28 * 112);
        assert!(gi_samples(&renderer).iter().flatten().all(|&v| v == 0.0));
        let resized = offscreen(&renderer.gfx);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));

        for _ in 0..32 {
            renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        }
        // Hover cannot disturb mature lighting history.
        renderer.upload_events(&[encode_event(crate::EV_MOUSEMOVE, 0, 0.0, 30.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 0.0);
        assert!(gi_samples(&renderer).iter().any(|p| p[3] == 32.0));

        // Capturing paint still renders the previous world. Once it is visible,
        // untouched surfaces must retain mature history and the narrow filter.
        renderer.upload_events(&[encode_event(EV_MOUSEDOWN, 0, 0.0, 30.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 16.0);
        assert!(gi_samples(&renderer).iter().any(|p| p[3] == 32.0));
        renderer.upload_events(&[encode_event(crate::EV_MOUSEMOVE, 0, 0.0, 30.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 15.0);
        let edited = gi_samples(&renderer);
        assert!(edited.iter().all(|p| p[3] <= 32.0));
        assert!(edited.iter().any(|p| p[3] == 32.0));

        // The first moved sample establishes a direction; the second commits a
        // span. Both this extension and the final release point mark an edit.
        renderer.upload_events(&[
            encode_event(crate::EV_MOUSEMOVE, 0, 0.0, 44.0, 27.0),
            encode_event(crate::EV_MOUSEMOVE, 0, 0.0, 65.0, 27.0),
        ]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 16.0);
        assert!(gi_samples(&renderer).iter().any(|p| p[3] > 1.0));
        renderer.upload_events(&[encode_event(crate::EV_MOUSEUP, 0, 0.0, 65.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 16.0);

        // Continued pointer motion with no new paint must let the window expire.
        renderer.upload_events(&[encode_event(crate::EV_MOUSEMOVE, 0, 0.0, 65.0, 27.0)]);
        for remaining in (0..16).rev() {
            renderer.execute(&resized, &cam, 0, 0.0).unwrap();
            assert_eq!(paint_edit_frames(&renderer), remaining as f32);
            let samples = gi_samples(&renderer);
            assert!(samples.iter().all(|p| p[3] <= 32.0));
            assert!(samples.iter().any(|p| p[3] == 32.0));
            assert!(samples.iter().flatten().all(|v| v.is_finite()));
        }
        for _ in 0..32 {
            renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        }
        assert!(gi_samples(&renderer).iter().any(|p| p[3] == 32.0));

        // Reference mode resets only when a committed edit is first rendered.
        // Building drags and release emit no extra paint and must not reset it.
        renderer.gi_mode = 3;
        renderer.upload_events(&[encode_event(crate::EV_KEYDOWN, 0, 1.0, 0.0, 0.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        renderer.upload_events(&[]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        renderer.upload_events(&[encode_event(EV_MOUSEDOWN, 0, 0.0, 40.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert!(gi_samples(&renderer).iter().any(|p| p[3] > 1.0));
        renderer.upload_events(&[encode_event(crate::EV_MOUSEMOVE, 0, 0.0, 50.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 15.0);
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 14.0);
        assert!(gi_samples(&renderer).iter().any(|p| p[3] > 1.0));
        renderer.upload_events(&[encode_event(crate::EV_MOUSEUP, 0, 0.0, 50.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert_eq!(paint_edit_frames(&renderer), 13.0);
        assert!(gi_samples(&renderer).iter().any(|p| p[3] > 2.0));
    }

    #[test]
    fn canal_has_real_bed_and_ashlar_and_water_only_animates_below_land() {
        let gfx = Gfx::new_headless(160, 120).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.0, -1.35, 22.0);
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        renderer.gi_mode = 1;
        let target = offscreen(&renderer.gfx);
        for _ in 0..3 {
            renderer.execute(&target, &cam, 0, 0.0).unwrap();
        }
        let depth = image_bytes(&renderer.gfx, &renderer.targets.depth);
        let before = image_bytes(&renderer.gfx, &target);
        let eye = cam.eye();
        let mut forward = std::array::from_fn::<_, 3, _>(|i| cam.target[i] - eye[i]);
        let length = forward.iter().map(|x| x * x).sum::<f32>().sqrt();
        forward.iter_mut().for_each(|x| *x /= length);
        let positions: Vec<[f32; 3]> = depth
            .chunks_exact(4)
            .enumerate()
            .map(|(i, bytes)| {
                let d = f32::from_le_bytes(bytes.try_into().unwrap());
                let view_depth = 1000.0 * 0.1 / (1000.0 - d * (1000.0 - 0.1));
                let (origin, direction) =
                    cam.cursor_ray(160.0, 120.0, (i % 160) as f32 + 0.5, (i / 160) as f32 + 0.5);
                let cosine = direction
                    .iter()
                    .zip(forward)
                    .map(|(a, b)| a * b)
                    .sum::<f32>();
                std::array::from_fn(|axis| origin[axis] + direction[axis] * view_depth / cosine)
            })
            .collect();
        let bed = positions
            .iter()
            .filter(|p| (p[1] + 1.8).abs() < 0.02)
            .count();
        let coping = positions
            .iter()
            .filter(|p| (p[1] - 0.11).abs() < 0.012)
            .count();
        assert!(
            bed > 500,
            "the canal must expose actual geometry below water: {bed}"
        );
        assert!(
            coping > 150,
            "broad ashlar caps must have their own raised depth: {coping}"
        );
        for p in &positions {
            let channel_distance = ((p[0] - 0.12 * p[2]) / 1.0f32.hypot(0.12)).abs();
            if channel_distance < 0.7 && p[2].abs() < 3.0 {
                assert!(
                    p[1] < -0.65,
                    "ground or cobbles float over the canal: {p:?}"
                );
            }
        }
        renderer.execute(&target, &cam, 0, 2.0).unwrap();
        assert_eq!(
            depth,
            image_bytes(&renderer.gfx, &renderer.targets.depth),
            "water waves must preserve opaque bed/bank depth"
        );
        let after = image_bytes(&renderer.gfx, &target);
        let mut water_changes = 0;
        for (i, (a, b)) in before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .enumerate()
        {
            if a != b {
                assert!(
                    positions[i][1] < -0.45,
                    "animated water covered an above-water surface: {:?}",
                    positions[i]
                );
                water_changes += 1;
            }
        }
        assert!(
            water_changes > 100,
            "the visible water surface must animate"
        );
    }

    fn image_depths(gfx: &Gfx, texture: &wgpu::Texture) -> Vec<f32> {
        image_bytes(gfx, texture)
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn raised_coping_shadows_the_cobble_tops_beside_it() {
        let (width, height) = (400, 300);
        let gfx = Gfx::new_headless(width, height).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([2.3, 0.0, 4.5], 0.0, -1.35, 6.2);
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        renderer.gi_mode = 1;
        let target = offscreen(&renderer.gfx);
        // Disable AO to isolate sunlight. Exercise both geometric and textured
        // normals with the normal scene geometry and shared shadow mechanisms.
        for textures in [false, true] {
            renderer.textures_enabled = textures;
            for _ in 0..3 {
                renderer.execute(&target, &cam, 2, 1.0).unwrap();
            }
            let depths = image_depths(&renderer.gfx, &renderer.targets.depth);
            let albedo = image_bytes(&renderer.gfx, &renderer.targets.albedo);
            let image = image_bytes(&renderer.gfx, &target);
            let eye = cam.eye();
            let mut forward = std::array::from_fn::<_, 3, _>(|i| cam.target[i] - eye[i]);
            let length = forward.iter().map(|x| x * x).sum::<f32>().sqrt();
            forward.iter_mut().for_each(|x| *x /= length);
            let tone = |x: f32| (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14);
            let srgb = |x: f32| 1.055 * x.powf(1.0 / 2.4) - 0.055;
            let mut sampled = 0;
            let mut shadowed = 0;
            let mut exposed = 0;
            let mut lit = 0;
            let mut cap_samples = 0;
            let mut cap_lit = 0;
            for (i, depth) in depths.into_iter().enumerate() {
                let (_, ray) = cam.cursor_ray(
                    width as f32,
                    height as f32,
                    (i % width as usize) as f32 + 0.5,
                    (i / width as usize) as f32 + 0.5,
                );
                let cosine = ray.iter().zip(forward).map(|(a, b)| a * b).sum::<f32>();
                let view_depth = 100.0 / (1000.0 - depth * 999.9);
                let p: [f32; 3] =
                    std::array::from_fn(|axis| eye[axis] + ray[axis] * view_depth / cosine);
                let across = (p[0] - 0.12 * p[2]) / 1.0f32.hypot(0.12);
                // Independent geometric expectation: the cap ends 1.58 m from the
                // canal centre and its top is y=.11. Trace the sun direction to
                // that side, allowing for the 6 mm bevel, joints, and map footprint.
                let edge_height = p[1] + (across - 1.58) * (0.52 * 1.0f32.hypot(0.12) / 0.542);
                let joint_distance = (p[2] - p[2].round()).abs();
                // This generous upper bound includes the GI-off path's 15%
                // sunlight floor and partial edge coverage; sunlit tops exceed it.
                let maximum = srgb(tone(albedo[i * 4] as f32 / 255.0 * 0.72));
                let dark = image[i * 4] as f32 / 255.0 <= maximum + 2.0 / 255.0;
                if p[1] > 0.025
                    && p[1] < 0.07
                    && across > 1.588
                    && edge_height < 0.085
                    && joint_distance > 0.06
                {
                    sampled += 1;
                    if dark {
                        shadowed += 1;
                    }
                }
                if p[1] > 0.045 && p[1] < 0.06 && across > 1.75 && across < 1.85 {
                    exposed += 1;
                    lit += usize::from(!dark);
                }
                if (p[1] - 0.11).abs() < 0.001
                    && across > 1.2
                    && across < 1.5
                    && joint_distance > 0.06
                {
                    cap_samples += 1;
                    cap_lit += usize::from(!dark);
                }
            }
            assert!(
                sampled > 100,
                "fixture must expose cobble tops behind the coping: {sampled}"
            );
            assert!(shadowed * 100 >= sampled * 95,
            "coping must shade at least 95% of the interior cobble samples (textures={textures}): {shadowed}/{sampled}");
            assert!(
                exposed > 100 && lit * 100 >= exposed * 95,
                "cobbles beyond the shadow must stay lit (textures={textures}): {lit}/{exposed}"
            );
            assert!(cap_samples > 100 && cap_lit * 100 >= cap_samples * 99,
            "the cap must not shadow its own flat top (textures={textures}): {cap_lit}/{cap_samples}");
        }
    }

    fn shadow_queries(renderer: &mut Renderer, points: &[[f32; 4]]) -> Vec<f32> {
        renderer.rebuild_sun_shadows().unwrap();
        let buffer = storage(
            &renderer.gfx.device,
            "shadow query positions",
            (points.len() * 16) as u64,
        );
        renderer
            .gfx
            .queue
            .write_buffer(&buffer, 0, bytemuck::cast_slice(points));
        let mut encoder = renderer
            .gfx
            .device
            .create_command_encoder(&Default::default());
        let output = generated::encode_shadow_visibility(
            &mut renderer.host,
            &mut encoder,
            renderer.shadows.grid.as_ref().unwrap(),
            &buffer,
        )
        .unwrap();
        renderer.gfx.queue.submit(Some(encoder.finish()));
        let OutputResource::Buffer { buffer, .. } = &output.values[0].resource else {
            panic!("visibility entry must return a buffer");
        };
        read_buffer(&renderer.gfx, buffer)
            .unwrap()
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn geometric_shadows_rebuild_on_terrain_and_sun_edits() {
        let gfx = Gfx::new_headless(160, 120).expect("GPU device");
        let mut cam = Camera::default();
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        renderer.gi_mode = 1;
        let mut points = Vec::new();
        for z in [-18.0, -12.0, 12.0, 18.0] {
            for y in [-0.64, -0.60, -0.56] {
                points.push([-0.70 + 0.12 * z, y, z, 0.0]);
                points.push([0.60 + 0.12 * z, y, z, 0.0]);
            }
        }
        let visibility = shadow_queries(&mut renderer, &points);
        for (i, &occluded) in visibility.iter().enumerate() {
            assert_eq!(
                occluded,
                if i % 2 == 0 { 1.0 } else { 0.0 },
                "at {:?}",
                points[i]
            );
        }
        assert_eq!(renderer.shadows.geometry_exports, 1);
        assert_eq!(renderer.shadows.generation, 1);

        // Camera, time, material and viewport changes keep the same index.
        let target = offscreen(&renderer.gfx);
        renderer.execute(&target, &cam, 0, 1.0).unwrap();
        cam.set([0.0, 0.0, 0.0], 1.3, -0.7, 15.0);
        renderer.textures_enabled = false;
        renderer.execute(&target, &cam, 0, 2.0).unwrap();
        renderer.resize(81, 55);
        let resized = offscreen(&renderer.gfx);
        renderer.execute(&resized, &cam, 0, 3.0).unwrap();
        renderer.set_terrain_cells(&terrain::demo()).unwrap();
        renderer.set_sun_direction(shadow::DEFAULT_SUN).unwrap();
        assert_eq!(shadow_queries(&mut renderer, &points), visibility);
        assert_eq!(renderer.shadows.generation, 1);

        renderer.set_sun_direction([0.0, 1.0, 0.0]).unwrap();
        assert_eq!(renderer.gi_reset_frames, 1);
        assert!(shadow_queries(&mut renderer, &points)
            .iter()
            .all(|s| *s == 0.0));
        assert_eq!(renderer.shadows.generation, 2);
        assert_eq!(renderer.shadows.geometry_exports, 1);

        renderer.set_sun_direction(shadow::DEFAULT_SUN).unwrap();
        assert_eq!(shadow_queries(&mut renderer, &points), visibility);
        assert_eq!(renderer.shadows.generation, 3);
        renderer
            .set_terrain_cells(&vec![terrain::LAND; terrain::SIDE * terrain::SIDE])
            .unwrap();
        assert!(shadow_queries(&mut renderer, &points)
            .iter()
            .all(|s| *s == 0.0));
        assert_eq!(renderer.shadows.generation, 4);
        assert_eq!(renderer.shadows.geometry_exports, 2);
        renderer.set_terrain_cells(&terrain::demo()).unwrap();
        assert_eq!(shadow_queries(&mut renderer, &points), visibility);
        assert_eq!(renderer.shadows.generation, 5);
        assert_eq!(renderer.shadows.geometry_exports, 3);
    }

    #[test]
    fn geometric_gpu_queries_match_all_caster_reference() {
        let gfx = Gfx::new_headless(64, 48).expect("GPU device");
        let mut renderer = fixed_sample_renderer(gfx, &Camera::default());
        let mut seed = 731u32;
        let mut random = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / 16777216.0
        };
        let points: Vec<_> = (0..4000)
            .map(|_| {
                [
                    random() * 40.0 - 20.0,
                    random() * 5.0 - 0.7,
                    random() * 40.0 - 20.0,
                    0.0,
                ]
            })
            .collect();
        for sun in [
            shadow::DEFAULT_SUN,
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.3, 0.8, -0.6],
        ] {
            renderer.set_sun_direction(sun).unwrap();
            let actual = shadow_queries(&mut renderer, &points);
            for (p, actual) in points.iter().zip(actual) {
                let expected = shadow::reference_occluded(
                    &renderer.shadows.casters,
                    renderer.shadows.direction,
                    [p[0], p[1], p[2]],
                );
                assert_eq!(
                    actual,
                    if expected { 1.0 } else { 0.0 },
                    "point {p:?}, sun {sun:?}"
                );
            }
        }
        assert_eq!(renderer.shadows.geometry_exports, 1);
    }

    #[test]
    fn water_mesh_is_repeatable_and_moves_its_contact() {
        assert_eq!(reflection_extent(1280, 800), (128, 80));
        let gfx = Gfx::new_headless(240, 180).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -0.4, 21.0);
        let mut renderer = fixed_sample_renderer(gfx, &cam);
        renderer.gi_mode = 1;
        let target = offscreen(&renderer.gfx);
        for _ in 0..3 {
            renderer.execute(&target, &cam, 0, 1.0).unwrap();
        }
        let (commands, output) = renderer.encode_frame(&target, &cam, 0, 1.0).unwrap();
        renderer.gfx.queue.submit(Some(commands));
        let OutputResource::Buffer { buffer, .. } = &output
            .values
            .iter()
            .find(|value| value.name == "result_13")
            .unwrap()
            .resource
        else {
            panic!("water grid output missing");
        };
        let grid_bytes = read_buffer(&renderer.gfx, buffer).unwrap();
        let grid: Vec<[f32; 4]> = grid_bytes
            .chunks_exact(16)
            .map(|sample| {
                std::array::from_fn(|axis| {
                    f32::from_le_bytes(sample[axis * 4..axis * 4 + 4].try_into().unwrap())
                })
            })
            .collect();
        assert_eq!(grid.len(), 321 * 321);
        assert!(grid.iter().all(|v| v.iter().all(|x| x.is_finite())
            && (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] - 1.0).abs() < 0.001
            && v[1] > 0.0));
        let full = image_bytes(&renderer.gfx, &target);
        let capture = image_bytes(&renderer.gfx, &renderer.targets.reflection);
        let reflected_pixels: Vec<_> = capture
            .chunks_exact(8)
            .filter(|pixel| u16::from_le_bytes([pixel[6], pixel[7]]) == 0x3c00)
            .collect();
        assert!(
            reflected_pixels.len() > 50,
            "reflection must contain masonry"
        );
        assert!(
            reflected_pixels.len() < capture.len() / 8,
            "reflection must retain sky"
        );
        // Positive half floats preserve ordering: 0x3400 is 0.25. Shadowed
        // masonry must cover the sky just as lit faces do.
        assert!(
            reflected_pixels.iter().any(|pixel| (0..3)
                .all(|c| u16::from_le_bytes([pixel[c * 2], pixel[c * 2 + 1]]) < 0x3400)),
            "dark masonry must remain in the reflection"
        );
        let opaque_depth = image_depths(&renderer.gfx, &renderer.targets.depth);
        let surface = image_depths(&renderer.gfx, &renderer.targets.scene_z);
        let eye = cam.eye();
        let forward: [f32; 3] = std::array::from_fn(|i| (cam.target[i] - eye[i]) / cam.dist);
        let wet: Vec<_> = surface
            .iter()
            .zip(&opaque_depth)
            .map(|(a, b)| a < b)
            .collect();

        // Compare actual depth with the shared GPU height grid, including cell
        // seams. This checks grid addressing and rasterization independently of
        // the procedural wave formula.
        let mut wet_pixels = 0;
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for (i, &depth) in surface.iter().enumerate() {
            assert!(depth.is_finite() && (0.0..=1.0).contains(&depth));
            if !wet[i] {
                continue;
            }
            wet_pixels += 1;
            let (origin, direction) =
                cam.cursor_ray(240.0, 180.0, (i % 240) as f32 + 0.5, (i / 240) as f32 + 0.5);
            let cosine = direction
                .iter()
                .zip(forward)
                .map(|(a, b)| a * b)
                .sum::<f32>();
            // Invert the rounded f32 coefficients actually used by the GPU
            // projection. Rearranging near/far algebra loses millimetres here.
            let near_far = 1.0f32 / (0.1 - 1000.0);
            let a = 1000.0 * near_far;
            let b = 1000.0 * 0.1 * near_far;
            let distance = b / (a + depth) / cosine;
            let [x, y, z] = std::array::from_fn(|axis| origin[axis] + direction[axis] * distance);
            lo = lo.min(y);
            hi = hi.max(y);
            let gx = ((x + 20.0) * 8.0).clamp(0.0, 319.999);
            let gz = ((z + 20.0) * 8.0).clamp(0.0, 319.999);
            let ix = gx as usize;
            let iz = gz as usize;
            let u = gx.fract();
            let v = gz.fract();
            let a = grid[iz * 321 + ix][3];
            let b = grid[(iz + 1) * 321 + ix][3];
            let c = grid[iz * 321 + ix + 1][3];
            let d = grid[(iz + 1) * 321 + ix + 1][3];
            let wave = if u + v <= 1.0 {
                a + (c - a) * u + (b - a) * v
            } else {
                d + (b - d) * (1.0 - u) + (c - d) * (1.0 - v)
            };
            assert!(
                (y - wave).abs() < 0.001,
                "mesh wave error at {x}, {z}: {}",
                y - wave
            );
        }
        assert!(wet_pixels > 1000, "water mesh must cover the canal");
        assert!(
            lo >= -0.646 && hi <= -0.554 && hi - lo > 0.025,
            "actual water hits must follow bounded waves: {lo}..{hi}"
        );

        renderer.execute(&target, &cam, 0, 2.0).unwrap();
        assert_ne!(full, image_bytes(&renderer.gfx, &target));
        assert_eq!(
            capture,
            image_bytes(&renderer.gfx, &renderer.targets.reflection),
            "waves distort the lookup; the static reflected scene must not flicker"
        );
        assert_eq!(
            opaque_depth,
            image_depths(&renderer.gfx, &renderer.targets.depth)
        );
        let later = image_depths(&renderer.gfx, &renderer.targets.scene_z);
        let contacts = wet
            .iter()
            .zip(later.iter().zip(&opaque_depth))
            .filter(|(before, (a, b))| **before != (a < b))
            .count();
        assert!(
            contacts > 0,
            "waves must move the water/wall intersection over time, not just the normals"
        );

        // With ambient lighting and fixed inputs, returning to a previous time
        // must reproduce its image exactly, independent of intervening frames.
        renderer.execute(&target, &cam, 0, 1.0).unwrap();
        assert_eq!(full, image_bytes(&renderer.gfx, &target));

        cam.az += 0.5;
        renderer.execute(&target, &cam, 0, 1.0).unwrap();
        assert_ne!(
            capture,
            image_bytes(&renderer.gfx, &renderer.targets.reflection),
            "reflection must follow the camera"
        );

        // Odd edge pixels and the shared depth attachment survive resizing.
        renderer.resize(81, 55);
        assert_eq!(
            (
                renderer.targets.reflection.width(),
                renderer.targets.reflection.height()
            ),
            (81, 55)
        );
        let resized = offscreen(&renderer.gfx);
        renderer.execute(&resized, &cam, 0, 1.0).unwrap();
        let resized_depth = image_depths(&renderer.gfx, &renderer.targets.scene_z);
        assert_eq!(resized_depth.len(), 81 * 55);
        assert!(resized_depth.iter().all(|v| v.is_finite()));

        // Turning every cell into land must leave only opaque geometry depth.
        renderer
            .set_terrain_cells(&vec![terrain::LAND; terrain::SIDE * terrain::SIDE])
            .unwrap();
        renderer.execute(&resized, &cam, 0, 1.0).unwrap();
        assert_eq!(
            image_depths(&renderer.gfx, &renderer.targets.scene_z),
            image_depths(&renderer.gfx, &renderer.targets.depth)
        );
    }
    // Geometry/lighting tests compare exact samples. Exercise jitter separately
    // below rather than changing their assertions to tolerate moving samples.
    fn fixed_sample_renderer(gfx: Gfx, cam: &Camera) -> Renderer {
        let mut renderer = Renderer::new(gfx, cam, 0, 0.0).unwrap();
        renderer.taa_enabled = false;
        renderer
    }

    fn hdr_pixels(gfx: &Gfx, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
        image_bytes(gfx, texture)
            .chunks_exact(16)
            .map(|pixel| {
                std::array::from_fn(|i| {
                    f32::from_le_bytes(pixel[i * 4..i * 4 + 4].try_into().unwrap())
                })
            })
            .collect()
    }

    fn save_quality_frame(name: &str, gfx: &Gfx, target: &wgpu::Texture) {
        if let Some(dir) = std::env::var_os("TINYPORTO_QA_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
            let mut png = png::Encoder::new(file, target.width(), target.height());
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            png.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            png.write_header()
                .unwrap()
                .write_image_data(&image_bytes(gfx, target))
                .unwrap();
        }
    }

    #[test]
    fn taa_quality_static_and_motion_against_supersampled_reference() {
        let (w, h, scale) = (320usize, 224usize, 4usize);
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -0.6, 24.0);
        let mut renderer =
            Renderer::new(Gfx::new_headless(w as u32, h as u32).unwrap(), &cam, 0, 0.0).unwrap();
        renderer.gi_mode = 1;
        let target = offscreen(&renderer.gfx);
        let mut reference = fixed_sample_renderer(
            Gfx::new_headless((w * scale) as u32, (h * scale) as u32).unwrap(),
            &cam,
        );
        reference.gi_mode = 1;
        let reference_target = offscreen(&reference.gfx);
        let mut fixed = fixed_sample_renderer(Gfx::new_headless(w as u32, h as u32).unwrap(), &cam);
        fixed.gi_mode = 1;
        let fixed_target = offscreen(&fixed.gfx);
        let mut prior = Vec::<u8>::new();
        let mut deltas = Vec::<u8>::new();
        for frame in 0..96 {
            renderer.execute(&target, &cam, 0, 1.0).unwrap();
            if frame >= 63 {
                let image = image_bytes(&renderer.gfx, &target);
                if !prior.is_empty() {
                    let depth = hdr_pixels(&renderer.gfx, renderer.temporal.input());
                    for i in 0..w * h {
                        if depth[i][3] > 0.0 {
                            for c in 0..3 {
                                deltas.push(image[i * 4 + c].abs_diff(prior[i * 4 + c]));
                            }
                        }
                    }
                }
                prior = image;
            }
            if frame == 94 || frame == 95 {
                save_quality_frame(&format!("taa-static-{frame}"), &renderer.gfx, &target);
            }
        }
        deltas.sort_unstable();
        let mean = deltas.iter().map(|x| f64::from(*x)).sum::<f64>() / deltas.len() as f64;
        let p99 = deltas[deltas.len() * 99 / 100];
        eprintln!("TAA settled display variation: mean {mean:.4}/255, p99 {p99}/255");
        assert!(
            mean < 0.35 && p99 <= 3,
            "stationary TAA must not visibly shimmer"
        );

        for moving in [false, true] {
            if moving {
                for _ in 0..24 {
                    cam.az += 0.003;
                    renderer.execute(&target, &cam, 0, 1.0).unwrap();
                }
            }
            for _ in 0..3 {
                fixed.execute(&fixed_target, &cam, 0, 1.0).unwrap();
                reference.execute(&reference_target, &cam, 0, 1.0).unwrap();
            }
            let truth = hdr_pixels(&reference.gfx, &reference.targets.hdr);
            let taa = hdr_pixels(&renderer.gfx, renderer.temporal.input());
            let baseline = hdr_pixels(&fixed.gfx, &fixed.targets.hdr);
            let (mut taa_error, mut baseline_error, mut count) = (0.0f64, 0.0f64, 0usize);
            for y in 2..h - 2 {
                for x in 2..w - 2 {
                    let i = y * w + x;
                    if baseline[i][3] <= 0.0 || taa[i][3] <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        let mut expected = 0.0;
                        for dy in 0..scale {
                            for dx in 0..scale {
                                expected += truth[(y * scale + dy) * w * scale + x * scale + dx][c]
                                    / (scale * scale) as f32;
                            }
                        }
                        taa_error += f64::from((taa[i][c] - expected).powi(2));
                        baseline_error += f64::from((baseline[i][c] - expected).powi(2));
                        count += 1;
                    }
                }
            }
            eprintln!(
                "TAA reference moving={moving}: MSE {:.6}, disabled {:.6}, ratio {:.3}",
                taa_error / count as f64,
                baseline_error / count as f64,
                taa_error / baseline_error
            );
            let label = if moving { "moving" } else { "static" };
            save_quality_frame(&format!("taa-{label}"), &renderer.gfx, &target);
            save_quality_frame(&format!("no-taa-{label}"), &fixed.gfx, &fixed_target);
            save_quality_frame(
                &format!("reference-{label}"),
                &reference.gfx,
                &reference_target,
            );
            assert!(
                taa_error < baseline_error * 0.9,
                "TAA must improve on disabled AA, not just reduce jitter"
            );
        }
    }

    #[test]
    fn taa_stabilizes_jitter_keeps_picking_stable_and_invalidates_history() {
        let gfx = Gfx::new_headless(192, 128).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -0.6, 24.0);
        let mut renderer = Renderer::new(gfx, &cam, 0, 0.0).unwrap();
        renderer.gi_mode = 1;
        let target = offscreen(&renderer.gfx);
        let key = renderer.temporal_key(0);
        let first = renderer.temporal.frame(&cam, key, false);
        assert_eq!(first.valid, 0);
        renderer.prepare(&cam, 0).unwrap();
        assert_eq!(
            renderer.temporal.frame(&cam, key, false).jitter,
            first.jitter
        );
        assert_eq!(renderer.temporal.frame(&cam, key, false).valid, 0);

        let mut previous = None;
        let (mut raw_delta, mut resolved_delta, mut count) = (0.0f64, 0.0f64, 0usize);
        for frame in 0..48 {
            renderer.execute(&target, &cam, 0, 1.0).unwrap();
            let raw = hdr_pixels(&renderer.gfx, &renderer.targets.hdr);
            let resolved = hdr_pixels(&renderer.gfx, renderer.temporal.input());
            assert!(resolved.iter().flatten().all(|v| v.is_finite()));
            if let Some((old_raw, old_resolved)) = previous.as_ref() {
                let old_raw: &Vec<[f32; 4]> = old_raw;
                let old_resolved: &Vec<[f32; 4]> = old_resolved;
                if frame >= 16 {
                    for i in 0..raw.len() {
                        if raw[i][3] <= 0.0 || old_raw[i][3] <= 0.0 {
                            continue;
                        }
                        for c in 0..3 {
                            raw_delta += f64::from((raw[i][c] - old_raw[i][c]).abs());
                            resolved_delta +=
                                f64::from((resolved[i][c] - old_resolved[i][c]).abs());
                            count += 1;
                        }
                    }
                }
            }
            previous = Some((raw, resolved));
        }
        eprintln!(
            "TAA static variation: raw {}, resolved {}, ratio {}",
            raw_delta / count as f64,
            resolved_delta / count as f64,
            resolved_delta / raw_delta
        );
        assert!(count > 10000 && raw_delta > 1.0);
        assert!(
            resolved_delta < raw_delta * 0.65,
            "history must reduce subpixel flicker"
        );
        assert_eq!(renderer.temporal.frame(&cam, key, false).valid, 1);
        assert_eq!(renderer.temporal.frame(&cam, key, true).valid, 0);
        let mut cut = cam;
        cut.az += 0.5;
        assert_eq!(renderer.temporal.frame(&cut, key, false).valid, 0);
        renderer.textures_enabled = false;
        assert_eq!(
            renderer
                .temporal
                .frame(&cam, renderer.temporal_key(0), false)
                .valid,
            0
        );
        renderer.textures_enabled = true;

        // Identical cursor positions must paint identical world points, despite
        // different projection jitter on their two press frames.
        renderer.upload_events(&[encode_event(EV_MOUSEDOWN, 0, 0.0, 96.0, 64.0)]);
        renderer.execute(&target, &cam, 0, 1.0).unwrap();
        renderer.upload_events(&[encode_event(crate::EV_MOUSEUP, 0, 0.0, 96.0, 64.0)]);
        renderer.execute(&target, &cam, 0, 1.0).unwrap();
        renderer.upload_events(&[encode_event(EV_MOUSEDOWN, 0, 0.0, 96.0, 64.0)]);
        renderer.execute(&target, &cam, 0, 1.0).unwrap();
        let points = read_buffer(&renderer.gfx, &renderer.world.points).unwrap();
        assert_eq!(&points[..8], &points[8..16]);
        renderer.upload_events(&[]);
        renderer.resize(193, 129);
        assert_eq!(renderer.temporal.frame(&cam, key, false).valid, 0);
        let resized = offscreen(&renderer.gfx);
        renderer.execute(&resized, &cam, 0, 1.0).unwrap();
        assert_eq!(renderer.temporal.input().width(), 193);
        renderer.gi_mode = 3;
        let reference = renderer
            .temporal
            .frame(&cam, renderer.temporal_key(0), false);
        assert_eq!(reference.enabled, 0);
        assert_eq!(reference.jitter, [0.0; 2]);
    }

    #[test]
    fn taa_stays_stable_with_realtime_gi_and_animated_water() {
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -0.6, 24.0);
        let mut renderer =
            Renderer::new(Gfx::new_headless(384, 256).unwrap(), &cam, 0, 0.0).unwrap();
        let target = offscreen(&renderer.gfx);
        let mut previous = Vec::<u8>::new();
        let (mut delta, mut count) = (0u64, 0usize);
        for i in 0..96 {
            renderer
                .execute(&target, &cam, 0, 1.0 + i as f32 / 60.0)
                .unwrap();
            if i >= 63 {
                let frame = image_bytes(&renderer.gfx, &target);
                if !previous.is_empty() {
                    let hdr = hdr_pixels(&renderer.gfx, renderer.temporal.input());
                    for (pixel, depth) in hdr.iter().enumerate() {
                        if depth[3] > 0.0 {
                            for c in 0..3 {
                                delta += u64::from(
                                    frame[pixel * 4 + c].abs_diff(previous[pixel * 4 + c]),
                                );
                                count += 1;
                            }
                        }
                    }
                }
                previous = frame;
            }
        }
        let mean = delta as f64 / count as f64;
        eprintln!("TAA with GI and water animation: opaque mean step {mean:.4}/255");
        assert!(mean < 0.5, "GI must not restore full-scene jitter");
        let before = hdr_pixels(&renderer.gfx, renderer.temporal.input());
        for i in 0..12 {
            renderer
                .execute(&target, &cam, 0, 3.0 + i as f32 / 30.0)
                .unwrap();
        }
        let after = hdr_pixels(&renderer.gfx, renderer.temporal.input());
        let changed_water = before
            .iter()
            .zip(&after)
            .filter(|(a, b)| {
                a[3] < 0.0
                    && b[3] < 0.0
                    && (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() > 0.01
            })
            .count();
        assert!(
            changed_water > 100,
            "stability must not freeze animated water"
        );
        save_quality_frame("taa-live-gi", &renderer.gfx, &target);
    }
}
