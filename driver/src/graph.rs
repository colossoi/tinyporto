//! Generic GPU frame-graph schema.
//!
//! Domain-agnostic. The per-pipeline binding interface (set/binding/kind/name)
//! and the dispatch/output-size calculations are GENERATED from the wyn `.json`
//! descriptor by build.rs (see the `descriptor` module). This file defines only
//! the driver-side model those generated tables resolve against: what resources
//! exist, how a binding name maps to one, and the per-frame schedule.

/// A per-frame value the driver computes and writes into one uniform-block
/// member. Each maps to a member of the `frame_globals` block; the byte offset
/// comes from the descriptor (`UniformBlockLayout`), so this only says *what*
/// to write, never *where*.
#[derive(Clone, Copy, Debug)]
pub enum FrameSource {
    /// `vec3f32` = (width, height, width/height).
    Resolution,
    /// `u32` live modifier mask at frame time (bit0 shift, 1 ctrl, 2 alt, 3 super).
    Mods,
    /// `vec3f32` orbit focal target (world).
    CamTarget,
    /// `f32` orbit azimuth (radians).
    CamAz,
    /// `f32` orbit elevation / pitch (radians).
    CamElev,
    /// `f32` orbit eye distance from the target.
    CamDist,
    /// `f32` seconds since start (drives time-varying effects, e.g. the water).
    Time,
}

/// One member of a uniform block the driver fills: which frame value goes in it.
/// The std140 offset is looked up by `field` name from the generated
/// `UniformBlockLayout`, so member order here need not match the block's.
#[derive(Clone, Copy, Debug)]
pub struct BlockMember {
    pub field: &'static str,
    pub source: FrameSource,
}

/// The std140 layout of one uniform block, GENERATED from the descriptor
/// (`build.rs`). `members` are (field name, byte offset, byte size); the driver
/// packs each `BlockMember` at the offset matching its `field`.
#[derive(Clone, Copy, Debug)]
pub struct UniformBlockLayout {
    pub name: &'static str,
    pub size: u64,
    pub members: &'static [(&'static str, u32, u32)],
}

/// One physical stage from the Wyn descriptor's `frame_graph`.
#[derive(Clone, Copy, Debug)]
pub struct DescriptorPassInfo {
    pub module: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
}

/// One descriptor frame-graph resource and all shader binding names that refer
/// to it. Compiler-created producer/consumer names can differ while still naming
/// one physical buffer; generated descriptor data supplies that aliasing.
#[derive(Clone, Copy, Debug)]
pub struct DescriptorResourceInfo {
    pub name: &'static str,
    pub binding_names: &'static [&'static str],
}

/// How a storage buffer's initial contents are set.
#[derive(Clone, Copy, Debug)]
pub enum BufInit {
    Zeroed,
    /// Fixed u32 contents (e.g. a static draw_indirect args buffer).
    U32s(&'static [u32]),
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
    /// A uniform block the driver fills each frame: one buffer whose members are
    /// packed from `members` at the offsets the descriptor publishes for `name`
    /// (see `UniformBlockLayout`). Replaces N single-value system uniforms.
    UniformBlock {
        name: &'static str,
        members: &'static [BlockMember],
    },
    /// A storage buffer (compute I/O, derived geometry, …).
    Buffer(BufferDef),
    /// Two storage buffers swapped each frame (persistent state). A binding reads
    /// the prev one (StorageRead) and writes the next (StorageWrite). `size` is
    /// `None` for a buffer that is also a compute output (derived).
    PingPong {
        name: &'static str,
        size: Option<u64>,
    },
    /// A window-sized depth texture (recreated on resize). One per graph.
    Depth,
    /// A 2D image backing `storage_image` and/or sampled `texture2d` views. `mips`
    /// > 1 builds a pyramid (Hi-Z). Storage/texture/copy usages are derived from
    /// how the graph's bindings reference it.
    Image {
        name: &'static str,
        format: TexFormat,
        size: ImgSize,
        mips: u32,
    },
}

/// How a binding resolves a ping-pong resource for the current frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Plain,
    Prev,
    Next,
}

/// Pixel format for image / storage-image resources. Mirrors the descriptor's
/// `format` strings (and a `resource`'s `format` field).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TexFormat {
    Rgba8Unorm,
    Rgba16Float,
    R32Float,
}

impl TexFormat {
    /// Whether a sampled view of this format supports linear filtering. 32-bit
    /// float formats are unfilterable on the default feature set, so a sampled
    /// `texture2d` over them must declare `Float { filterable: false }` (they are
    /// read via `texture_load`, not a filtering sampler).
    pub fn filterable(self) -> bool {
        !matches!(self, TexFormat::R32Float)
    }
}

/// How a compute shader touches a `storage_image` view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImgAccess {
    Read,
    Write,
    ReadWrite,
}

/// Extent of an image resource: track the swapchain, or a fixed size.
#[derive(Clone, Copy, Debug)]
pub enum ImgSize {
    /// Recreated on resize to match the surface (Hi-Z, G-buffer).
    Window,
    Fixed {
        w: u32,
        h: u32,
    },
}

/// How a (set, binding) slot is typed in the shader. Matches the values the
/// generated `*_BINDINGS` tables use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BindingKind {
    Uniform,
    StorageRead,
    StorageWrite,
    /// A storage buffer both read and written across a pass's stages —
    /// compiler-internal `intermediate` scratch (e.g. a `filter`'s flag/scan/
    /// gather buffers), auto-allocated at its descriptor-fixed capacity.
    StorageReadWrite,
    /// A sampled `texture2d` (f32, 2D, filterable — Wyn's texture2d is monomorphic).
    Texture,
    /// A `storage_image` view: fixed `vec4f32` texels; `format` is the on-GPU pixel
    /// format and `access` is how the shader reads/writes it.
    StorageImage {
        format: TexFormat,
        access: ImgAccess,
    },
}

/// Descriptor role of a binding within its pipeline. Unlike storage access, this
/// says whether a ping-pong resource is the prior-frame input or current-frame
/// output even when the shader must declare the slot `read_write`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BindingUsage {
    Input,
    Output,
    Intermediate,
    Other,
}

/// One generated binding row: (set, binding, kind, usage, shader-param name).
/// The driver maps `name` to a resource and uses `usage` to derive ping-pong role.
pub type BindTable = &'static [(u32, u32, BindingKind, BindingUsage, &'static str)];

/// A binding resolved against the resource graph (driver-internal).
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub set: u32,
    pub binding: u32,
    pub resource: &'static str,
    pub kind: BindingKind,
    pub role: Role,
}

/// One ordered stage of a compute pass: an entry point and its workgroup dispatch
/// dims. A Wyn compute entry lowers to one stage per output domain (e.g. `step` →
/// six: one fixed-grid kernel per fixed output, one input-sized kernel per `map`),
/// which the descriptor names in order. The stages of a pass share its binding
/// interface and run sequentially.
#[derive(Clone, Copy, Debug)]
pub struct ComputeStage {
    pub entry: &'static str,
    pub groups: [u32; 3],
}

/// A compute pass: its generated binding table, the ordered stages it lowers to
/// (each with its own dispatch), and the generated size calc for its output
/// bindings (used to derive the byte sizes of buffers this pass writes).
///
/// `stages` is owned because image-sized dispatches depend on the window size,
/// which is only known once the surface exists (see `app::graph`).
#[derive(Clone, Debug)]
pub struct ComputePass {
    pub label: &'static str,
    pub module: &'static str,
    pub bindings: BindTable,
    pub stages: Vec<ComputeStage>,
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

/// One color attachment of a render pass, in shader `location` order. `target:
/// None` is the swapchain surface (or screenshot view); `Some(name)` is a
/// graph-owned `Image` resource (e.g. an MRT linear-depth target the Hi-Z reduce
/// reads). `format` is ignored for the surface (it uses the surface format).
#[derive(Clone, Copy, Debug)]
pub struct ColorTarget {
    pub target: Option<&'static str>,
    pub format: Option<TexFormat>,
    pub clear: [f64; 4],
}

/// A render pass: its color attachments (location 0 first; every item's fragment
/// writes all of them), an optional depth attachment, and items drawn in order.
#[derive(Clone, Copy, Debug)]
pub struct RenderPass {
    #[allow(dead_code)]
    pub label: &'static str,
    pub depth: Option<&'static str>,
    pub color: &'static [ColorTarget],
    pub items: &'static [RenderItem],
}

#[derive(Clone, Debug)]
pub enum Pass {
    Compute(ComputePass),
    Render(RenderPass),
}

/// The whole application as data. `names` maps each shader binding name (from the
/// generated tables) to a resource name — the only binding info still authored by
/// hand, and purely semantic (no set/binding numbers or types).
///
/// Built once per run against the actual surface size (see `app::graph`): image
/// extents and image-sized compute dispatches both derive from it.
#[derive(Clone, Debug)]
pub struct Graph {
    pub resources: Vec<Resource>,
    pub passes: Vec<Pass>,
    pub names: &'static [(&'static str, &'static str)],
}

impl Graph {
    /// Expand compiler-declared compute prerequisites immediately before the
    /// authored pass that needs them. The callbacks are generated entirely from
    /// the descriptor: handwritten graph code continues to name only Wyn source
    /// entries, never physical materialization stages or pipeline indices.
    pub fn insert_compute_prerequisites(
        &mut self,
        pipeline_index: fn(&str, &str) -> Option<usize>,
        prerequisite_pipelines: fn(&str, &str) -> &'static [usize],
        compute_pass: fn(&str, usize) -> Option<ComputePass>,
    ) {
        let mut authored = std::collections::HashSet::new();
        for pass in &self.passes {
            match pass {
                Pass::Compute(compute) => {
                    for stage in &compute.stages {
                        if let Some(index) = pipeline_index(compute.module, stage.entry) {
                            authored.insert((compute.module, index));
                        }
                    }
                }
                Pass::Render(render) => {
                    for item in render.items {
                        for entry in [item.vs, item.fs] {
                            if let Some(index) = pipeline_index(item.module, entry) {
                                authored.insert((item.module, index));
                            }
                        }
                    }
                }
            }
        }

        let mut inserted = std::collections::HashSet::new();
        let mut expanded = Vec::new();
        for pass in std::mem::take(&mut self.passes) {
            let mut required = Vec::new();
            let mut require = |module: &'static str, entry: &'static str| {
                for &index in prerequisite_pipelines(module, entry) {
                    if !authored.contains(&(module, index)) && inserted.insert((module, index)) {
                        required.push((module, index));
                    }
                }
            };
            match &pass {
                Pass::Compute(compute) => {
                    for stage in &compute.stages {
                        require(compute.module, stage.entry);
                    }
                }
                Pass::Render(render) => {
                    for item in render.items {
                        require(item.module, item.vs);
                        require(item.module, item.fs);
                    }
                }
            }
            for (module, index) in required {
                let prerequisite = compute_pass(module, index).unwrap_or_else(|| {
                    panic!(
                        "descriptor compute prerequisite pipeline {index} in module {module:?} \
                         requires runtime sizing and cannot be inserted automatically"
                    )
                });
                expanded.push(Pass::Compute(prerequisite));
            }
            expanded.push(pass);
        }
        self.passes = expanded;
    }
}
