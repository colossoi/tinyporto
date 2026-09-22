//! Application inputs, retained frame results, and caller-owned render targets.
//! All shader loading, pipeline creation, dispatches, and draws belong to Wyn.

use crate::{camera::Camera, generated, gfx::Gfx};
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
    sun: wgpu::Texture,
    albedo: wgpu::Texture,
    normal: wgpu::Texture,
    depth: wgpu::Texture,
    shadow_z: wgpu::Texture,
    scene_z: wgpu::Texture,
    ao: wgpu::Buffer,
    next_occ: wgpu::Buffer,
    gi_rays: wgpu::Buffer,
}

impl Targets {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let image = |name, format| {
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
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        Self {
            sun: image("sun", wgpu::TextureFormat::R32Float),
            albedo: image("scene albedo", wgpu::TextureFormat::Rgba8Unorm),
            normal: image("scene normal", wgpu::TextureFormat::Rgba32Float),
            depth: image("scene window depth", wgpu::TextureFormat::R32Float),
            shadow_z: image("shadow depth attachment", wgpu::TextureFormat::Depth32Float),
            scene_z: image("scene depth attachment", wgpu::TextureFormat::Depth32Float),
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
        // Generated draws load their attachments. Both scene draws share scene_z,
        // so the ground depth survives into the prop draw. Shadows have their own.
        for (texture, color) in [
            (&self.sun, wgpu::Color::WHITE),
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
        for texture in [&self.shadow_z, &self.scene_z] {
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

pub struct Renderer {
    pub gfx: Gfx,
    pub frame: u32,
    pub gi_mode: u32,
    // Retain Wyn's pipelines and scratch buffers across frames and resizes.
    host: generated::HostContext,
    world: World,
    targets: Targets,
    events: wgpu::Buffer,
    globals: wgpu::Buffer,
    start: Instant,
    previous_camera: Camera,
    gi_reset_frames: u32,
    paint_held: bool,
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
            &FrameHistory {
                camera: *cam,
                frame_index: 0,
                valid: 0,
                gi_mode: 0,
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
        Ok(Self {
            host: generated::HostContext::new(&gfx.device).context("initialize Wyn host")?,
            world: World::new(&gfx.device, w, h),
            targets: Targets::new(&gfx.device, w, h),
            events: storage(&gfx.device, "events", EV_CAP as u64 * 16),
            gfx,
            globals,
            frame: 0,
            gi_mode: 0,
            start: Instant::now(),
            previous_camera: *cam,
            gi_reset_frames: 1,
            paint_held: false,
        })
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
        for event in &events[..count] {
            let kind = event[0] as u32 / 256;
            if kind == crate::EV_MOUSEDOWN && event[1] == 0.0 {
                self.paint_held = true;
                self.gi_reset_frames = 2;
            } else if kind == crate::EV_MOUSEUP && event[1] == 0.0 {
                self.paint_held = false;
                self.gi_reset_frames = 2;
            } else if kind == crate::EV_MOUSEMOVE && self.paint_held {
                // The frame renders the previous world, so edits invalidate two frames.
                self.gi_reset_frames = 2;
            }
        }
        self.gfx
            .queue
            .write_buffer(&self.events, 0, bytemuck::cast_slice(&padded));
    }

    fn execute(
        &mut self,
        target: &wgpu::Texture,
        cam: &Camera,
        mods: u32,
        time: f32,
    ) -> Result<()> {
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
            &FrameHistory {
                camera: self.previous_camera,
                frame_index: self.frame,
                valid: u32::from(self.gi_reset_frames == 0),
                gi_mode: self.gi_mode,
            },
        )?;
        self.gfx.queue.write_buffer(&self.globals, 0, &bytes);
        let mut encoder = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tinyporto frame"),
            });
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
            &self.world.occ,
            &self.world.gi,
            &self.targets.albedo,
            &self.targets.normal,
            &self.targets.depth,
            &self.targets.ao,
            &self.targets.next_occ,
            &self.targets.sun,
            &self.targets.gi_rays,
            target,
            &self.targets.shadow_z,
            &self.targets.scene_z,
            &self.targets.scene_z,
        )
        .context("execute Wyn frame")?;
        self.gfx.queue.submit(Some(encoder.finish()));
        let next = World::from_output(&output)?;
        let previous = std::mem::replace(&mut self.world, next);
        // The caller-provided occlusion output cannot alias next frame's input.
        self.targets.next_occ = previous.occ;
        self.frame = self.frame.wrapping_add(1);
        self.previous_camera = *cam;
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
            _ => None,
        })
    }

    pub fn dump_buffer(&self, name: &str) -> Result<()> {
        let buffer = self.buffer(name).with_context(|| format!(
            "no exposed buffer '{name}'; available: uistate, points, items, head, occ, gi, events, frame, ao_work"
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
        self.upload_events(&[]);
        for _ in 0..total {
            let t0 = Instant::now();
            self.execute(&tex, cam, mods, time)?;
            self.gfx.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })?;
            frame_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
    }

    #[test]
    fn generated_frames_retain_input_state_and_resize_occlusion_history() {
        let gfx = Gfx::new_headless(64, 48).expect("GPU device");
        let cam = Camera::default();
        let mut renderer = Renderer::new(gfx, &cam, 0, 0.0).unwrap();
        let target = offscreen(&renderer.gfx);
        // Tab selects building for the next frame; this frame still paints fence.
        renderer.upload_events(&[
            encode_event(crate::EV_KEYDOWN, 0, 1.0, 0.0, 0.0),
            encode_event(EV_MOUSEDOWN, 0, 0.0, 32.0, 24.0),
        ]);
        renderer.execute(&target, &cam, 0, 0.0).unwrap();
        let ui = read_buffer(&renderer.gfx, &renderer.world.ui).unwrap();
        assert_eq!(f32::from_le_bytes(ui[..4].try_into().unwrap()), 1.0);
        let head = read_buffer(&renderer.gfx, &renderer.world.head).unwrap();
        assert_eq!(f32::from_le_bytes(head[..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(head[4..8].try_into().unwrap()), 1.0);
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
            read_buffer(&renderer.gfx, &renderer.world.head).unwrap(),
            head
        );
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
            read_buffer(&renderer.gfx, &renderer.world.head).unwrap(),
            head
        );
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

    #[test]
    fn gi_accumulates_reprojects_and_invalidates_history() {
        let gfx = Gfx::new_headless(80, 60).expect("GPU device");
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], 0.6, -1.1, 25.0);
        let mut renderer = Renderer::new(gfx, &cam, 0, 0.0).unwrap();
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

        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        renderer.upload_events(&[encode_event(EV_MOUSEDOWN, 0, 0.0, 40.0, 27.0)]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));
        renderer.upload_events(&[]);
        renderer.execute(&resized, &cam, 0, 0.0).unwrap();
        assert!(gi_samples(&renderer).iter().all(|p| p[3] <= 1.0));
    }
}
