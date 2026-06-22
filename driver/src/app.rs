//! The tiny-porto frame-graph, as plain data.
//!
//! There are NO domain types here (no Ground/Building); this is a generic
//! `Graph` value. All meaning lives in the Wyn program (`../wyn`). Must stay in
//! sync with the descriptor `wyn compile` emits for `wyn/main.wyn`
//! (`shaders/main.json`); a future `build.rs` enforces that.

use crate::graph::*;

// GRID*GRID candidate building sites (must match GRID in wyn/main.wyn: 24).
const N: u64 = 24 * 24;

pub const GRAPH: Graph = Graph {
    resources: &[
        Resource::SysUniform { name: "iResolution", kind: SysUniform::Resolution },
        // Compute I/O. `seed` is an iota index buffer; `gen` maps it to
        // `instances` (vec4 each) and writes `args` (the draw_indirect record).
        Resource::Buffer(BufferDef { name: "seed", size: N * 4, indirect: false, init: BufInit::Iota }),
        Resource::Buffer(BufferDef { name: "instances", size: N * 16, indirect: false, init: BufInit::Zeroed }),
        Resource::Buffer(BufferDef { name: "args", size: 16, indirect: true, init: BufInit::Zeroed }),
        Resource::Depth { name: "depth" },
    ],

    passes: &[
        // GPU-driven: generate instances + indirect args.
        Pass::Compute(ComputePass {
            label: "gen",
            module: "main",
            entry: "gen",
            bindings: &[
                Binding { set: 0, binding: 0, resource: "seed", kind: BindingKind::StorageRead },
                Binding { set: 0, binding: 1, resource: "instances", kind: BindingKind::StorageWrite },
                Binding { set: 0, binding: 2, resource: "args", kind: BindingKind::StorageWrite },
            ],
            dispatch: Dispatch::FromBufferElems { buffer: "seed", elem_bytes: 4, workgroup: 64 },
        }),
        // Scene: ground quad (direct) + buildings (indirect), depth-tested.
        Pass::Render(RenderPass {
            label: "scene",
            depth: Some("depth"),
            clear: [0.74, 0.80, 0.86, 1.0], // horizon sky
            items: &[
                RenderItem {
                    label: "ground",
                    module: "main",
                    vs: "ground_vertex",
                    fs: "ground_fragment",
                    bindings: &[Binding { set: 1, binding: 0, resource: "iResolution", kind: BindingKind::Uniform }],
                    draw: Draw::Direct { vertices: 6, instances: 1 },
                    depth_write: true,
                },
                RenderItem {
                    label: "buildings",
                    module: "main",
                    vs: "box_vertex",
                    fs: "box_fragment",
                    bindings: &[
                        Binding { set: 1, binding: 0, resource: "instances", kind: BindingKind::StorageRead },
                        Binding { set: 1, binding: 1, resource: "iResolution", kind: BindingKind::Uniform },
                    ],
                    draw: Draw::Indirect { args: "args" },
                    depth_write: true,
                },
            ],
        }),
    ],
};
