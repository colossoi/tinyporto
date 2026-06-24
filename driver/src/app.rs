//! The tiny-porto frame-graph, as plain data.
//!
//! The per-pipeline binding tables and the dispatch/output-size calculations are
//! GENERATED from `wyn/main.wyn`'s descriptor by build.rs (the `descriptor`
//! module). This file authors only what the descriptor can't know: which
//! resources exist, the seed sizes, the binding-name -> resource mapping, and the
//! per-frame schedule.

use crate::generated::{step_dispatch, step_out_bytes, GROUND_FRAGMENT_BINDINGS, GROUND_VERTEX_BINDINGS, STEP_BINDINGS};
use crate::graph::*;

// Seed element counts — the only buffer sizes authored by hand (must match the
// constants in wyn/paint.wyn). Everything downstream is derived by the generated
// `step_out_bytes` from these.
const POINTS_CAP: u64 = 1024;
const ITEMS_CAP: u64 = 128;
const TESS_CAP: u64 = 95238;
const TIDX_BYTES: u64 = TESS_CAP * 4;
const PIDX_BYTES: u64 = POINTS_CAP * 4;
const IIDX_BYTES: u64 = ITEMS_CAP * 4;

// `step`'s output sizes, by binding, from the generated calc (the seed sizes are
// baked in). The driver pairs this with the binding table to size each output
// buffer by name — no output binding numbers appear here.
const fn step_out(binding: u32) -> u64 {
    step_out_bytes(binding, IIDX_BYTES, PIDX_BYTES, TIDX_BYTES)
}

pub const GRAPH: Graph = Graph {
    resources: &[
        Resource::SysUniform { name: "iResolution", kind: SysUniform::Resolution },
        Resource::SysUniform { name: "iMouse", kind: SysUniform::Mouse },
        Resource::SysUniform { name: "iKeys", kind: SysUniform::Keys },
        Resource::SysUniform { name: "iCam", kind: SysUniform::Cam },
        // Persistent state (ping-pong); sizes derived (they're `step` outputs).
        Resource::PingPong { name: "uistate", size: None },
        Resource::PingPong { name: "points", size: None },
        Resource::PingPong { name: "items", size: None },
        Resource::PingPong { name: "head", size: None },
        // Iota index seeds (the hand-picked design sizes).
        Resource::Buffer(BufferDef { name: "tidx", size: Some(TIDX_BYTES), init: BufInit::Iota, indirect: false }),
        Resource::Buffer(BufferDef { name: "pidx", size: Some(PIDX_BYTES), init: BufInit::Iota, indirect: false }),
        Resource::Buffer(BufferDef { name: "iidx", size: Some(IIDX_BYTES), init: BufInit::Iota, indirect: false }),
        // Derived `step` outputs: the ribbon geometry and its draw_indirect args.
        Resource::Buffer(BufferDef { name: "tess", size: None, init: BufInit::Zeroed, indirect: false }),
        Resource::Buffer(BufferDef { name: "draw_args", size: None, init: BufInit::Zeroed, indirect: true }),
    ],

    // Shader binding name -> resource name. Roles (prev/next/plain) are derived
    // from the binding kind + whether the resource is ping-pong.
    names: &[
        ("tidx", "tidx"),
        ("pidx", "pidx"),
        ("iidx", "iidx"),
        ("uistate_in", "uistate"),
        ("points_in", "points"),
        ("items_in", "items"),
        ("head_in", "head"),
        ("iResolution", "iResolution"),
        ("iMouse", "iMouse"),
        ("iCam", "iCam"),
        ("iKeys", "iKeys"),
        ("step_output_0", "uistate"),
        ("step_output_1", "points"),
        ("step_output_2", "items"),
        ("step_output_3", "head"),
        ("step_output_4", "tess"),
        ("step_output_5", "draw_args"),
        ("tess", "tess"),
    ],

    passes: &[
        // Advance all persistent state + tessellate the ribbon (one kernel).
        Pass::Compute(ComputePass {
            label: "step",
            module: "main",
            entry: "step",
            bindings: STEP_BINDINGS,
            groups: step_dispatch(TIDX_BYTES)[0],
            out_bytes: step_out,
        }),
        // Draw the tessellated ground ribbon.
        Pass::Render(RenderPass {
            label: "scene",
            depth: None,
            clear: [0.74, 0.80, 0.86, 1.0],
            items: &[RenderItem {
                label: "ground",
                module: "main",
                vs: "ground_vertex",
                fs: "ground_fragment",
                vs_bindings: GROUND_VERTEX_BINDINGS,
                fs_bindings: GROUND_FRAGMENT_BINDINGS,
                draw_args: "draw_args",
            }],
        }),
    ],
};
