//! Generic GPU frame-graph schema.
//!
//! Domain-agnostic: no "ground"/"building"/"canal" here. Mirrors the structure
//! of the pipeline descriptor `wyn compile` emits — resources, bindings
//! (set/binding -> resource), compute passes, and render passes. The concrete
//! graph for tiny porto is the `const GRAPH` in `app.rs`; the driver never reads
//! the JSON descriptor at runtime (a future `build.rs` validates the two).

/// A driver-managed "system" value, fed automatically each frame.
#[derive(Clone, Copy, Debug)]
pub enum SysUniform {
    /// `vec3f32` = (width, height, width/height).
    Resolution,
    /// `f32` seconds since start.
    #[allow(dead_code)] // wired in M2
    Time,
    /// `vec4f32` = (x, y, held?1:0, 0).
    #[allow(dead_code)] // wired in M2
    Mouse,
    /// `u32` frame counter.
    #[allow(dead_code)]
    Frame,
    /// `vec4u32` one-frame key pulses: (x = Tab pressed, y = toggle pressed, …).
    Keys,
}

/// How a storage buffer's initial contents are set.
#[derive(Clone, Copy, Debug)]
pub enum BufInit {
    /// All zero (wgpu zero-initializes by default).
    Zeroed,
    /// 0u32, 1u32, 2u32, … (count = size / 4). A generic index buffer; lets a
    /// compute `map` recover a per-element index without a domain concept.
    Iota,
}

/// A storage buffer the graph owns. Always gets STORAGE | COPY_DST; `indirect`
/// additionally adds INDIRECT (so it can back a draw_indirect call).
#[derive(Clone, Copy, Debug)]
pub struct BufferDef {
    pub name: &'static str,
    pub size: u64,
    pub indirect: bool,
    pub init: BufInit,
}

/// A GPU resource the graph needs.
#[derive(Clone, Copy, Debug)]
pub enum Resource {
    /// Small uniform buffer the driver fills each frame from `kind`.
    SysUniform { name: &'static str, kind: SysUniform },
    /// A storage buffer (compute I/O, instance data, indirect args, …).
    Buffer(BufferDef),
    /// Two storage buffers swapped each frame (persistent state). Referenced by
    /// `Binding`s with `Role::Prev` (last frame) / `Role::Next` (this frame).
    /// Zero-initialized.
    PingPong { name: &'static str, size: u64 },
    /// A window-sized depth texture (recreated on resize).
    Depth { name: &'static str },
}

/// How a binding resolves a ping-pong resource for the current frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    /// A plain (non-ping-pong) resource, looked up by name.
    Plain,
    /// The ping-pong buffer written last frame (read source for `edit`).
    Prev,
    /// The ping-pong buffer written this frame (edit's output; the render's read).
    Next,
}

/// How a (set, binding) slot is typed in the shader.
#[derive(Clone, Copy, Debug)]
pub enum BindingKind {
    Uniform,
    StorageRead,
    StorageWrite,
    #[allow(dead_code)]
    StorageReadWrite,
}

/// Wires one shader binding slot to a named resource. For ping-pong resources,
/// `resource` is the pair name and `role` selects which physical buffer.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub set: u32,
    pub binding: u32,
    pub resource: &'static str,
    pub kind: BindingKind,
    pub role: Role,
}

/// How a compute pass's dispatch size is computed.
#[derive(Clone, Copy, Debug)]
pub enum Dispatch {
    /// ceil(buffer_size / elem_bytes / workgroup) workgroups in x.
    FromBufferElems { buffer: &'static str, elem_bytes: u32, workgroup: u32 },
    /// A fixed number of workgroups in x (e.g. a 1-thread state pass).
    Fixed { x: u32 },
}

/// A compute pass: one entry, its set-0 bindings, and a dispatch rule.
#[derive(Clone, Copy, Debug)]
pub struct ComputePass {
    pub label: &'static str,
    pub module: &'static str,
    pub entry: &'static str,
    pub bindings: &'static [Binding],
    pub dispatch: Dispatch,
}

/// What a render item draws.
#[derive(Clone, Copy, Debug)]
pub enum Draw {
    /// Non-indexed direct draw (vertices, instances).
    Direct { vertices: u32, instances: u32 },
    /// Non-indexed indirect draw; args buffer holds
    /// [vertex_count, instance_count, first_vertex, first_instance].
    Indirect { args: &'static str },
}

/// One pipeline drawn within a render pass (shares the pass's attachments).
#[derive(Clone, Copy, Debug)]
pub struct RenderItem {
    pub label: &'static str,
    pub module: &'static str,
    pub vs: &'static str,
    pub fs: &'static str,
    pub bindings: &'static [Binding],
    pub draw: Draw,
    pub depth_write: bool,
}

/// A render pass: shared color (the surface) + optional depth, and a list of
/// items drawn in order.
#[derive(Clone, Copy, Debug)]
pub struct RenderPass {
    #[allow(dead_code)] // used by the build-time descriptor validator (future)
    pub label: &'static str,
    pub depth: Option<&'static str>,
    pub clear: [f64; 4],
    pub items: &'static [RenderItem],
}

/// An ordered step in the per-frame schedule.
#[derive(Clone, Copy, Debug)]
pub enum Pass {
    Compute(ComputePass),
    Render(RenderPass),
}

/// The whole application as data. Shader modules are compiled+embedded by
/// `build.rs` (see the generated `SHADER_MODULES`); passes reference them by the
/// same key, so the graph itself carries no module paths.
#[derive(Clone, Copy, Debug)]
pub struct Graph {
    pub resources: &'static [Resource],
    pub passes: &'static [Pass],
}
