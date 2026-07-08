//! Orbit camera state + cursor-pivot control math (host side).
//!
//! The driver owns the camera the way it owned `zoom` before: accumulated from
//! mouse input, eased, and written into the `frame` uniform block each frame
//! (target / azimuth / elevation / distance). The rotation and ray conventions
//! MIRROR `wyn/camera.wyn` exactly so the pivot math agrees with the rendered
//! view — keep the two in sync.

const FOV: f32 = 20.0; // matches camera.wyn FOV

// Clamps / feel.
const ELEV_MIN: f32 = -1.45; // near top-down
const ELEV_MAX: f32 = -0.03; // a hair below the horizon (camera stays looking down)
const DIST_MIN: f32 = 6.0;
const DIST_MAX: f32 = 260.0;
const EASE: f32 = 0.18; // per-frame easing of the visible camera toward the input target
const ROT_SPEED: f32 = 0.006; // radians per pixel of right-drag
const ZOOM_STEP: f32 = 0.88; // distance multiplier per wheel notch (in)

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn len(a: V3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn norm(a: V3) -> V3 {
    let l = len(a);
    if l > 0.0 {
        scale(a, 1.0 / l)
    } else {
        a
    }
}
fn lerp3(a: V3, b: V3, t: f32) -> V3 {
    add(a, scale(sub(b, a), t))
}

/// Column-major orbit rotation, mirroring camera.wyn `rotation(angle=(elev,az))`.
fn rotation(elev: f32, az: f32) -> [V3; 3] {
    let (se, ce) = (elev.sin(), elev.cos());
    let (sa, ca) = (az.sin(), az.cos());
    [
        [ca, 0.0, -sa],
        [sa * se, ce, ca * se],
        [sa * ce, -se, ca * ce],
    ]
}

/// `m * v` for a column-major mat3 (columns are m[0], m[1], m[2]).
fn mul(m: &[V3; 3], v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// Pinhole ray in camera space, mirroring camera.wyn `ray_dir`. `pos` is a pixel
/// with y measured bottom-up (as the shader's fragCoord is fed).
fn ray_dir(sw: f32, sh: f32, px: f32, py: f32) -> V3 {
    let x = px - sw * 0.5;
    let y = py - sh * 0.5;
    let cot_half = (90.0 - FOV * 0.5).to_radians().tan();
    let z = sh * 0.5 * cot_half;
    norm([x, y, -z])
}

#[derive(Clone, Copy)]
pub struct Camera {
    // Visible (eased) state, written to the block.
    pub target: V3,
    pub az: f32,
    pub elev: f32,
    pub dist: f32,
    // Input target the visible state eases toward.
    tt: V3,
    t_az: f32,
    t_elev: f32,
    t_dist: f32,
    // Right-drag orbit anchor: (pivot world point, down pixel [bottom-up], eye->pivot distance).
    anchor: Option<(V3, [f32; 2], f32)>,
}

impl Default for Camera {
    fn default() -> Self {
        let (target, az, elev, dist) = ([0.0, 0.0, 0.0], 0.6, -0.35, 45.0);
        Self {
            target,
            az,
            elev,
            dist,
            tt: target,
            t_az: az,
            t_elev: elev,
            t_dist: dist,
            anchor: None,
        }
    }
}

impl Camera {
    /// Eye of the *visible* camera.
    pub fn eye(&self) -> V3 {
        add(
            self.target,
            mul(&rotation(self.elev, self.az), [0.0, 0.0, self.dist]),
        )
    }

    /// World ray for a top-left cursor pixel, using the visible camera.
    fn cursor_ray(&self, sw: f32, sh: f32, mx: f32, my: f32) -> (V3, V3) {
        let py = sh - my; // flip to bottom-up like the shader
        let r = rotation(self.elev, self.az);
        let rd = norm(mul(&r, ray_dir(sw, sh, mx, py)));
        (self.eye(), rd)
    }

    /// Ground-plane (y=0) hit under a top-left cursor pixel, if the ray descends to it.
    fn ground_hit(&self, sw: f32, sh: f32, mx: f32, my: f32) -> Option<V3> {
        let (ro, rd) = self.cursor_ray(sw, sh, mx, my);
        if rd[1].abs() < 1e-6 {
            return None;
        }
        let t = -ro[1] / rd[1];
        if t <= 0.0 {
            return None;
        }
        Some(add(ro, scale(rd, t)))
    }

    /// Begin a right-drag orbit: anchor the pivot to the point under the cursor.
    pub fn begin_orbit(&mut self, sw: f32, sh: f32, mx: f32, my: f32) {
        let pivot = self.ground_hit(sw, sh, mx, my).unwrap_or(self.target);
        let d0 = len(sub(pivot, self.eye())).max(1.0);
        self.anchor = Some((pivot, [mx, sh - my], d0));
    }

    pub fn end_orbit(&mut self) {
        self.anchor = None;
    }

    /// Right-drag: spin azimuth / tilt elevation, keeping the anchored pivot glued
    /// under its original pixel (cursor-based rotation).
    pub fn orbit(&mut self, dx: f32, dy: f32, sw: f32, sh: f32) {
        self.t_az -= dx * ROT_SPEED;
        self.t_elev = (self.t_elev - dy * ROT_SPEED).clamp(ELEV_MIN, ELEV_MAX);
        if let Some((pivot, px, d0)) = self.anchor {
            let r = rotation(self.t_elev, self.t_az);
            let rd = norm(mul(&r, ray_dir(sw, sh, px[0], px[1])));
            let eye = sub(pivot, scale(rd, d0)); // keep pivot at distance d0 along the pixel ray
            self.tt = sub(eye, mul(&r, [0.0, 0.0, self.t_dist]));
        }
    }

    /// Middle-drag: grab the ground and slide it — the point under the cursor follows
    /// the cursor (cursor-based pan). Incremental, using the visible camera.
    pub fn pan(&mut self, sw: f32, sh: f32, from: (f32, f32), to: (f32, f32)) {
        let (a, b) = (
            self.ground_hit(sw, sh, from.0, from.1),
            self.ground_hit(sw, sh, to.0, to.1),
        );
        if let (Some(a), Some(b)) = (a, b) {
            self.tt = add(self.tt, sub(a, b)); // move the world so the grabbed point tracks the cursor
        }
    }

    /// Wheel: zoom toward the point under the cursor (cursor-based zoom). `notches`
    /// is +in / -out. Scales the eye/target toward the cursor-ground point.
    pub fn zoom(&mut self, sw: f32, sh: f32, mx: f32, my: f32, notches: f32) {
        let factor = ZOOM_STEP.powf(notches);
        let new_dist = (self.t_dist * factor).clamp(DIST_MIN, DIST_MAX);
        let f = new_dist / self.t_dist; // effective factor after clamping
        if let Some(pivot) = self.ground_hit(sw, sh, mx, my) {
            self.tt = lerp3(pivot, self.tt, f); // pull the target toward the cursor point
        }
        self.t_dist = new_dist;
    }

    /// Ease the visible camera toward the input target (call once per frame).
    pub fn ease(&mut self) {
        self.target = lerp3(self.target, self.tt, EASE);
        self.az += (self.t_az - self.az) * EASE;
        self.elev += (self.t_elev - self.elev) * EASE;
        self.dist += (self.t_dist - self.dist) * EASE;
    }

    /// Set the camera outright (screenshot scenarios) — snaps input target and visible.
    pub fn set(&mut self, target: V3, az: f32, elev: f32, dist: f32) {
        self.target = target;
        self.az = az;
        self.elev = elev;
        self.dist = dist;
        self.tt = target;
        self.t_az = az;
        self.t_elev = elev;
        self.t_dist = dist;
    }
}
