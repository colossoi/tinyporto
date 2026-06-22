//! Generic GPU frame-graph schema.
//!
//! This module is deliberately domain-agnostic: it has no notion of "ground",
//! "building", "canal", or anything game-specific. It mirrors the structure of
//! the pipeline descriptor that `wyn compile` emits (`particles.json`-style):
//! resources, bindings (set/binding → resource), and an ordered list of passes.
//!
//! The actual graph for tiny porto is declared as a `const` in `app.rs` — plain
//! data compiled into the binary. The driver never reads the JSON descriptor at
//! runtime; a future `build.rs` validates the two against each other.
//!
//! M0 implements only the bits the first milestone exercises (a render pass with
//! system-uniform bindings + a fullscreen draw). Compute passes, storage
//! buffers/images, and indirect draws are represented in the schema where it is
//! cheap, and filled in as later milestones need them.

/// A driver-managed "system" value, fed automatically every frame (the
/// Shadertoy/mountains.wyn convention).
#[derive(Clone, Copy, Debug)]
pub enum SysUniform {
    /// `vec3f32` = (width, height, width/height).
    Resolution,
    /// `f32` seconds since start.
    Time,
    /// `vec4f32` = (x, y, held?1:0, 0). y is top-down window coords.
    Mouse,
    /// `u32` frame counter.
    #[allow(dead_code)] // wired in a later milestone
    Frame,
}

/// A GPU resource the graph needs. (M0: only system uniforms.)
#[derive(Clone, Copy, Debug)]
pub enum Resource {
    /// A small uniform buffer the driver fills each frame from `kind`.
    SysUniform { name: &'static str, kind: SysUniform },
}

impl Resource {
    #[allow(dead_code)] // used by the build-time descriptor validator (future)
    pub fn name(&self) -> &'static str {
        match self {
            Resource::SysUniform { name, .. } => name,
        }
    }
}

/// How a single (set, binding) slot is typed in the shader.
#[derive(Clone, Copy, Debug)]
pub enum BindingKind {
    Uniform,
    // Forward-looking (M1+): StorageRead, StorageReadWrite, Texture, Sampler,
    // StorageImage. Added when a milestone first needs them.
}

/// Wires one shader binding slot to a named resource.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub set: u32,
    pub binding: u32,
    pub resource: &'static str,
    pub kind: BindingKind,
}

/// What geometry a render pass draws.
#[derive(Clone, Copy, Debug)]
pub enum Draw {
    /// A single full-screen triangle (3 verts, 1 instance, no vertex buffers).
    Fullscreen,
    // Forward-looking (M1): IndexedIndirect { args/index/vertex buffers }.
}

/// A graphics pass: one vertex + one fragment entry from the same SPIR-V module.
#[derive(Clone, Copy, Debug)]
pub struct RenderPass {
    pub label: &'static str,
    /// Key into `Graph::modules`.
    pub module: &'static str,
    /// SPIR-V `OpEntryPoint` names (== Wyn source entry names).
    pub vs: &'static str,
    pub fs: &'static str,
    pub bindings: &'static [Binding],
    pub draw: Draw,
}

/// An ordered step in the per-frame schedule.
#[derive(Clone, Copy, Debug)]
pub enum Pass {
    Render(RenderPass),
    // Forward-looking (M1): Compute(ComputePass).
}

/// The whole application as data. `app.rs` builds one `const GRAPH`.
#[derive(Clone, Copy, Debug)]
pub struct Graph {
    /// (key, path-to-.spv) — each compiled Wyn module.
    pub modules: &'static [(&'static str, &'static str)],
    pub resources: &'static [Resource],
    pub passes: &'static [Pass],
}
