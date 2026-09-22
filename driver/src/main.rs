//! Window and input shell for the Wyn-generated Rust/WGPU application.
mod app;
mod camera;
mod gfx;
mod materials;
mod terrain;
include!(concat!(env!("OUT_DIR"), "/module.rs"));

use anyhow::Result;
use app::Renderer;
use camera::Camera;
use clap::Parser;
use gfx::Gfx;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameRate(Option<u32>);

impl FrameRate {
    fn interval(self) -> Option<Duration> {
        self.0
            .map(|fps| Duration::from_secs_f64(1.0 / f64::from(fps)))
    }
}

impl FromStr for FrameRate {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "-" {
            return Ok(Self(None));
        }

        match value.parse::<u32>() {
            Ok(0) => Err("frame rate must be a positive integer or '-' for uncapped".into()),
            Ok(fps) => Ok(Self(Some(fps))),
            Err(_) => Err("frame rate must be a positive integer or '-' for uncapped".into()),
        }
    }
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum GiMode {
    On = 0,
    Off = 1,
    Indirect = 2,
    Reference = 3,
}

#[derive(Parser, Debug)]
#[command(about = "Tiny Porto — Wyn-generated Rust/WGPU application.")]
struct Args {
    #[arg(long, default_value_t = 1280)]
    width: u32,
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Render N window frames then exit. 0 = run forever.
    #[arg(long, default_value_t = 0)]
    frames: u32,
    /// Maximum interactive frame rate in Hz. Use `-` for uncapped.
    #[arg(long, default_value = "60", allow_hyphen_values = true)]
    fps: FrameRate,
    /// Lighting: realtime GI, old ambient, indirect only, or an explicit path reference.
    #[arg(long, value_enum, default_value_t = GiMode::On)]
    gi: GiMode,
    /// Compare with the original flat surface colors and geometric normals.
    #[arg(long)]
    no_textures: bool,
    /// Render the demo scene offscreen to this PNG and exit (no window).
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
    /// Screenshot scene time (seconds) fed to `frame.time`, reserved for animated
    /// effects. The window path uses real elapsed time.
    #[arg(long, default_value_t = 0.0)]
    time: f32,
    /// After a screenshot render, read these exposed buffers back and print their
    /// contents as u32 words (debug aid for persistent state / caller-owned buffers).
    #[arg(long, value_delimiter = ',')]
    dump: Vec<String>,
}

// Input event kinds (must match input.wyn).
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

/// Map a physical key to the keycode table in input.wyn (None = ignored).
fn keycode(pk: winit::keyboard::PhysicalKey) -> Option<f32> {
    use winit::keyboard::{KeyCode, PhysicalKey};
    match pk {
        PhysicalKey::Code(KeyCode::Tab) => Some(1.0), // KEY_TAB
        PhysicalKey::Code(KeyCode::KeyL) => Some(2.0), // KEY_L
        _ => None,
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
    /// Explicit redraw pacing. `None` preserves the uncapped polling mode.
    frame_interval: Option<Duration>,
    next_frame: Instant,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        // Use physical pixels so the initial target size is independent of DPI.
        let attrs = WindowAttributes::default()
            .with_title("tiny porto")
            .with_inner_size(PhysicalSize::new(self.args.width, self.args.height))
            .with_resizable(true);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("create_window: {e}");
                event_loop.exit();
                return;
            }
        };
        let renderer =
            Gfx::new(window.clone()).and_then(|gfx| Renderer::new(gfx, &self.cam, self.mods, 0.0));
        match renderer {
            Ok(mut r) => {
                r.gi_mode = self.args.gi as u32;
                r.textures_enabled = !self.args.no_textures;
                self.window = Some(window);
                self.renderer = Some(r);
                let now = Instant::now();
                self.fps_start = now;
                self.fps_frames = 0;
                self.next_frame = now;
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
                // them meaning — the Wyn input handler does.
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
                    event_loop.exit();
                    return;
                }
                if renderer.frame != previous_frame {
                    self.fps_frames += 1;
                }
                let now = Instant::now();
                let elapsed = now.duration_since(self.fps_start).as_secs_f64();
                if elapsed >= 0.5 && self.fps_frames > 0 {
                    let fps = f64::from(self.fps_frames) / elapsed;
                    if let Some(window) = &self.window {
                        window.set_title(&format!("tiny porto — {fps:.0} FPS"));
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = &self.window else {
            return;
        };

        match self.frame_interval {
            Some(interval) => {
                let now = Instant::now();
                if now >= self.next_frame {
                    window.request_redraw();
                    // Schedule from `now` instead of the previous deadline so a slow
                    // frame never causes a burst of catch-up redraws.
                    self.next_frame = now + interval;
                }
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
            }
            None => {
                window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Poll);
            }
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Headless: render the demo scene offscreen to a PNG and exit.
    if let Some(path) = args.screenshot.clone() {
        let gfx = Gfx::new_headless(args.width, args.height)?;
        let mut cam = Camera::default();
        cam.set([0.0, 0.0, 0.0], args.cam_az, args.cam_elev, args.cam_dist);
        let mut renderer = Renderer::new(gfx, &cam, args.mods, args.time)?;
        renderer.gi_mode = args.gi as u32;
        renderer.textures_enabled = !args.no_textures;
        renderer.screenshot(&path, &cam, args.mods, args.time)?;
        for name in &args.dump {
            renderer.dump_buffer(name)?;
        }
        return Ok(());
    }

    let frame_interval = args.fps.interval();
    let next_frame = Instant::now();
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(match frame_interval {
        Some(_) => ControlFlow::WaitUntil(next_frame),
        None => ControlFlow::Poll,
    });
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
        frame_interval,
        next_frame,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_defaults_to_60_hz() {
        let args = Args::try_parse_from(["tinyporto"]).unwrap();
        assert_eq!(args.fps, FrameRate(Some(60)));
    }

    #[test]
    fn dash_frame_rate_means_uncapped() {
        let args = Args::try_parse_from(["tinyporto", "--fps", "-"]).unwrap();
        assert_eq!(args.fps, FrameRate(None));
    }

    #[test]
    fn zero_frame_rate_is_rejected() {
        assert!(Args::try_parse_from(["tinyporto", "--fps", "0"]).is_err());
    }
}
