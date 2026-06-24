//! The tiny-porto frame-graph, as plain data.
//!
//! NO domain types here (no Ground/Building); it's a generic `Graph` value. All
//! meaning lives in the Wyn program (`../wyn`). Must stay in sync with the
//! descriptor `wyn compile` emits for `wyn/main.wyn`; a future `build.rs`
//! enforces that.

use crate::graph::*;

// Capture-buffer capacities — must match the constants in wyn/paint.wyn.
const POINTS_CAP: u64 = 1024; // control points (vec2f32)
const ITEMS_CAP: u64 = 128; // items (vec4f32)
const HEAD_LEN: u64 = 12; // capture-state floats

// Shorthand for a plain binding.
const fn b(set: u32, binding: u32, resource: &'static str, kind: BindingKind) -> Binding {
    Binding { set, binding, resource, kind, role: Role::Plain }
}
// Shorthand for a ping-pong binding (Prev = last frame, Next = this frame).
const fn pp(set: u32, binding: u32, resource: &'static str, kind: BindingKind, role: Role) -> Binding {
    Binding { set, binding, resource, kind, role }
}

pub const GRAPH: Graph = Graph {
    resources: &[
        Resource::SysUniform { name: "iResolution", kind: SysUniform::Resolution },
        Resource::SysUniform { name: "iMouse", kind: SysUniform::Mouse },
        Resource::SysUniform { name: "iKeys", kind: SysUniform::Keys },
        Resource::SysUniform { name: "iCam", kind: SysUniform::Cam },
        // UI state [tool, overlay_on], ping-ponged; advanced by the `ui` pass.
        Resource::PingPong { name: "uistate", size: 8 },
        // Paint state (all ping-pong): control points, items, capture head.
        Resource::PingPong { name: "points", size: POINTS_CAP * 8 },
        Resource::PingPong { name: "items", size: ITEMS_CAP * 16 },
        Resource::PingPong { name: "head", size: HEAD_LEN * 4 },
        // Per-buffer iota index seeds (so each `map` recovers its element index).
        // paint_head emits a fixed array literal, so it needs no seed.
        Resource::Buffer(BufferDef { name: "pidx", size: POINTS_CAP * 4, indirect: false, init: BufInit::Iota }),
        Resource::Buffer(BufferDef { name: "iidx", size: ITEMS_CAP * 4, indirect: false, init: BufInit::Iota }),
    ],

    passes: &[
        // UI state machine: advance [tool, overlay_on] from key pulses (Wyn-owned).
        // One compute entry advances all persistent state: reads the previous
        // ping-pong buffers (Prev) + iota seeds, writes the next ones (Next). The
        // compiler scheduled the whole thing into a single kernel dispatched over
        // pidx; set-0 bindings are compiler-allocated (see shaders/main.json).
        Pass::Compute(ComputePass {
            label: "step",
            module: "main",
            entry: "step",
            bindings: &[
                b(0, 0, "pidx", BindingKind::StorageRead),
                b(0, 1, "iidx", BindingKind::StorageRead),
                pp(0, 2, "uistate", BindingKind::StorageRead, Role::Prev),
                pp(0, 3, "points", BindingKind::StorageRead, Role::Prev),
                pp(0, 4, "items", BindingKind::StorageRead, Role::Prev),
                pp(0, 5, "head", BindingKind::StorageRead, Role::Prev),
                pp(0, 6, "uistate", BindingKind::StorageWrite, Role::Next),
                pp(0, 7, "points", BindingKind::StorageWrite, Role::Next),
                pp(0, 8, "items", BindingKind::StorageWrite, Role::Next),
                pp(0, 9, "head", BindingKind::StorageWrite, Role::Next),
                b(1, 0, "iResolution", BindingKind::Uniform),
                b(1, 1, "iMouse", BindingKind::Uniform),
                b(1, 3, "iCam", BindingKind::Uniform),
                b(1, 7, "iKeys", BindingKind::Uniform),
            ],
            dispatch: Dispatch::FromBufferElems { buffer: "pidx", elem_bytes: 4, workgroup: 64 },
        }),
        // Scene: the whole ground as one quad; items composited per fragment.
        Pass::Render(RenderPass {
            label: "scene",
            depth: None,
            clear: [0.74, 0.80, 0.86, 1.0],
            items: &[RenderItem {
                label: "ground",
                module: "main",
                vs: "ground_vertex",
                fs: "ground_fragment",
                bindings: &[
                    b(1, 0, "iResolution", BindingKind::Uniform),
                    b(1, 1, "iMouse", BindingKind::Uniform),
                    pp(1, 2, "uistate", BindingKind::StorageRead, Role::Next),
                    b(1, 3, "iCam", BindingKind::Uniform),
                    pp(1, 4, "items", BindingKind::StorageRead, Role::Next),
                    pp(1, 5, "points", BindingKind::StorageRead, Role::Next),
                    pp(1, 6, "head", BindingKind::StorageRead, Role::Next),
                ],
                draw: Draw::Direct { vertices: 6, instances: 1 },
                depth_write: false,
            }],
        }),
    ],
};
