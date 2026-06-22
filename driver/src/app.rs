//! The tiny-porto frame-graph, as plain data.
//!
//! NO domain types here (no Ground/Building); it's a generic `Graph` value. All
//! meaning lives in the Wyn program (`../wyn`). Must stay in sync with the
//! descriptor `wyn compile` emits for `wyn/main.wyn`; a future `build.rs`
//! enforces that.

use crate::graph::*;

// Building sites (BGRID^2 in wyn/main.wyn = 24^2).
const BN: u64 = 24 * 24;
// Ground control points (GW*GH*CP_PER_CELL in wyn/grid.wyn = 40*40*4).
const CPN: u64 = 40 * 40 * 4;

// Shorthand for a plain binding.
const fn b(set: u32, binding: u32, resource: &'static str, kind: BindingKind) -> Binding {
    Binding { set, binding, resource, kind, role: Role::Plain }
}

pub const GRAPH: Graph = Graph {
    resources: &[
        Resource::SysUniform { name: "iResolution", kind: SysUniform::Resolution },
        Resource::SysUniform { name: "iMouse", kind: SysUniform::Mouse },
        // Building generation I/O.
        Resource::Buffer(BufferDef { name: "seed_b", size: BN * 4, indirect: false, init: BufInit::Iota }),
        Resource::Buffer(BufferDef { name: "instances", size: BN * 16, indirect: false, init: BufInit::Zeroed }),
        Resource::Buffer(BufferDef { name: "args", size: 16, indirect: true, init: BufInit::Zeroed }),
        // Ground grid: per-control-point iota + ping-pong material (0=sand).
        Resource::Buffer(BufferDef { name: "seed_g", size: CPN * 4, indirect: false, init: BufInit::Iota }),
        Resource::PingPong { name: "material", size: CPN * 4 },
        Resource::Depth { name: "depth" },
    ],

    passes: &[
        // Buildings: generate instances + indirect args.
        Pass::Compute(ComputePass {
            label: "gen",
            module: "main",
            entry: "gen",
            // Set-0 bindings are compiler-allocated; gen's outputs land at b3/b4
            // (b1/b2 belong to `edit` — the compiler keeps them distinct across
            // entries in one module). seed shares b0 (per-pipeline bind group).
            bindings: &[
                b(0, 0, "seed_b", BindingKind::StorageRead),
                b(0, 3, "instances", BindingKind::StorageWrite),
                b(0, 4, "args", BindingKind::StorageWrite),
            ],
            dispatch: Dispatch::FromBufferElems { buffer: "seed_b", elem_bytes: 4, workgroup: 64 },
        }),
        // Water painting: prev material -> next material (ping-pong).
        Pass::Compute(ComputePass {
            label: "edit",
            module: "main",
            entry: "edit",
            bindings: &[
                b(0, 0, "seed_g", BindingKind::StorageRead),
                Binding { set: 0, binding: 1, resource: "material", kind: BindingKind::StorageRead, role: Role::Prev },
                Binding { set: 0, binding: 2, resource: "material", kind: BindingKind::StorageWrite, role: Role::Next },
                b(1, 0, "iResolution", BindingKind::Uniform),
                b(1, 1, "iMouse", BindingKind::Uniform),
            ],
            dispatch: Dispatch::FromBufferElems { buffer: "seed_g", elem_bytes: 4, workgroup: 64 },
        }),
        // Scene: Voronoi ground (this frame's material) + buildings.
        Pass::Render(RenderPass {
            label: "scene",
            depth: Some("depth"),
            clear: [0.74, 0.80, 0.86, 1.0],
            items: &[
                RenderItem {
                    label: "ground",
                    module: "main",
                    vs: "ground_vertex",
                    fs: "ground_fragment",
                    bindings: &[
                        b(1, 0, "iResolution", BindingKind::Uniform),
                        b(1, 1, "iMouse", BindingKind::Uniform),
                        Binding { set: 1, binding: 2, resource: "material", kind: BindingKind::StorageRead, role: Role::Next },
                    ],
                    draw: Draw::Direct { vertices: 6, instances: 1 },
                    depth_write: true,
                },
                RenderItem {
                    label: "buildings",
                    module: "main",
                    vs: "box_vertex",
                    fs: "box_fragment",
                    bindings: &[
                        b(1, 0, "instances", BindingKind::StorageRead),
                        b(1, 1, "iResolution", BindingKind::Uniform),
                    ],
                    draw: Draw::Indirect { args: "args" },
                    depth_write: true,
                },
            ],
        }),
    ],
};
