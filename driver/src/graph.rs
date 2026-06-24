//! Generic GPU frame-graph schema.
//!
//! Domain-agnostic. The per-pipeline binding interface (set/binding/kind/name)
//! and the dispatch/output-size calculations are GENERATED from the wyn `.json`
//! descriptor by build.rs (see the `descriptor` module). This file defines only
//! the driver-side model those generated tables resolve against: what resources
//! exist, how a binding name maps to one, and the per-frame schedule.

/// A driver-managed "system" value, fed automatically each frame.
#[derive(Clone, Copy, Debug)]
pub enum SysUniform {
    /// `vec3f32` = (width, height, width/height).
    Resolution,
    #[allow(dead_code)]
    Time,
    /// `vec4f32` = (x, y, held?1:0, 0).
    Mouse,
    #[allow(dead_code)]
    Frame,
    /// `vec4u32` one-frame key pulses: (x = Tab, y = toggle, …).
    Keys,
    /// `vec4f32` view state: (zoom in [0,1], …).
    Cam,
}

/// How a storage buffer's initial contents are set.
#[derive(Clone, Copy, Debug)]
pub enum BufInit {
    Zeroed,
    /// 0u32, 1u32, 2u32, … — a generic index seed for `map`.
    Iota,
}

/// A storage buffer the graph owns (STORAGE | COPY_DST). `size` is `None` when
/// the buffer is a compute output — the driver derives it from the descriptor
/// calc (see `ComputePass::out_bytes`).
#[derive(Clone, Copy, Debug)]
pub struct BufferDef {
    pub name: &'static str,
    pub size: Option<u64>,
    pub init: BufInit,
    /// Also usable as a `draw_indirect` args buffer (adds INDIRECT usage).
    pub indirect: bool,
}

/// A GPU resource the graph needs.
#[derive(Clone, Copy, Debug)]
pub enum Resource {
    /// Small uniform buffer the driver fills each frame from `kind`.
    SysUniform { name: &'static str, kind: SysUniform },
    /// A storage buffer (compute I/O, derived geometry, …).
    Buffer(BufferDef),
    /// Two storage buffers swapped each frame (persistent state). A binding reads
    /// the prev one (StorageRead) and writes the next (StorageWrite). `size` is
    /// `None` for a buffer that is also a compute output (derived).
    PingPong { name: &'static str, size: Option<u64> },
    /// A window-sized depth texture (recreated on resize). One per graph.
    Depth,
}

/// How a binding resolves a ping-pong resource for the current frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Plain,
    Prev,
    Next,
}

/// How a (set, binding) slot is typed in the shader. Matches the values the
/// generated `*_BINDINGS` tables use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BindingKind {
    Uniform,
    StorageRead,
    StorageWrite,
}

/// One generated binding row: (set, binding, kind, shader-param name). The driver
/// maps `name` to a resource (via `Graph::names`) and derives the role.
pub type BindTable = &'static [(u32, u32, BindingKind, &'static str)];

/// A binding resolved against the resource graph (driver-internal).
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub set: u32,
    pub binding: u32,
    pub resource: &'static str,
    pub kind: BindingKind,
    pub role: Role,
}

/// A compute pass: one entry, its generated binding table, a dispatch count, and
/// the generated size calc for its output bindings (used to derive the byte sizes
/// of buffers this pass writes).
#[derive(Clone, Copy, Debug)]
pub struct ComputePass {
    pub label: &'static str,
    pub module: &'static str,
    pub entry: &'static str,
    pub bindings: BindTable,
    pub groups: u32,
    pub out_bytes: fn(u32) -> u64,
}

/// One pipeline drawn within a render pass. Vertex and fragment binding tables
/// are merged (they share one pipeline layout). The draw is always indirect:
/// `draw_args` names a buffer holding [vertex_count, instance_count, first_vertex,
/// first_instance], written by a compute pass.
#[derive(Clone, Copy, Debug)]
pub struct RenderItem {
    pub label: &'static str,
    pub module: &'static str,
    pub vs: &'static str,
    pub fs: &'static str,
    pub vs_bindings: BindTable,
    pub fs_bindings: BindTable,
    pub draw_args: &'static str,
    /// Write depth + test LessEqual (true): 3D geometry self-occludes while
    /// coplanar fragments fall back to draw order. Ignore depth + keep painter
    /// order (false) for a pure flat overlay.
    pub depth_write: bool,
}

/// A render pass: the surface color (+ optional depth) and items drawn in order.
#[derive(Clone, Copy, Debug)]
pub struct RenderPass {
    #[allow(dead_code)]
    pub label: &'static str,
    pub depth: Option<&'static str>,
    pub clear: [f64; 4],
    pub items: &'static [RenderItem],
}

#[derive(Clone, Copy, Debug)]
pub enum Pass {
    Compute(ComputePass),
    Render(RenderPass),
}

/// The whole application as data. `names` maps each shader binding name (from the
/// generated tables) to a resource name — the only binding info still authored by
/// hand, and purely semantic (no set/binding numbers or types).
#[derive(Clone, Copy, Debug)]
pub struct Graph {
    pub resources: &'static [Resource],
    pub passes: &'static [Pass],
    pub names: &'static [(&'static str, &'static str)],
}
