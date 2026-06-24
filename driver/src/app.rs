//! The tiny-porto frame-graph, as plain data.
//!
//! The per-pipeline binding tables and the dispatch/output-size calculations are
//! GENERATED from `wyn/main.wyn`'s descriptor by build.rs (the `descriptor`
//! module). This file authors only what the descriptor can't know: which
//! resources exist, the seed sizes, the binding-name -> resource mapping, and the
//! per-frame schedule.

use crate::generated::{
    step_dispatch, step_out_bytes, SCENE_FRAGMENT_BINDINGS, SCENE_VERTEX_BINDINGS, SETT_FRAGMENT_BINDINGS,
    SETT_VERTEX_BINDINGS, STEP_BINDINGS,
};
use crate::graph::*;

// Seed element counts — the only buffer sizes authored by hand (must match the
// constants in wyn/paint.wyn / wyn/bricks.wyn). Everything downstream is derived
// by the generated `step_out_bytes` from these.
const POINTS_CAP: u64 = 1024;
const ITEMS_CAP: u64 = 128;
const TESS_CAP: u64 = 95238; // ground geom stream: TESS_BASE + ITEMS_CAP * TESS_VPI
const BRICK_COUNT: u64 = 36960; // running-bond grid cells = sett instances (BCOLS*BROWS)
const TIDX_BYTES: u64 = TESS_CAP * 4;
const PIDX_BYTES: u64 = POINTS_CAP * 4;
const IIDX_BYTES: u64 = ITEMS_CAP * 4;
const BIDX_BYTES: u64 = BRICK_COUNT * 4;

// `step`'s output sizes, by binding, from the generated calc (the seed sizes are
// baked in). The driver pairs this with the binding table to size each output
// buffer by name — no output binding numbers appear here.
const fn step_out(binding: u32) -> u64 {
    step_out_bytes(binding, BIDX_BYTES, IIDX_BYTES, PIDX_BYTES, TIDX_BYTES)
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
        Resource::Buffer(BufferDef { name: "bidx", size: Some(BIDX_BYTES), init: BufInit::Iota, indirect: false }),
        // Derived `step` outputs: ground geometry (two parallel (pos,kind)/(nrm,attr)
        // streams) + its draw args; the per-instance sett records + their draw args.
        Resource::Buffer(BufferDef { name: "geom_pos", size: None, init: BufInit::Zeroed, indirect: false }),
        Resource::Buffer(BufferDef { name: "geom_nrm", size: None, init: BufInit::Zeroed, indirect: false }),
        Resource::Buffer(BufferDef { name: "draw_args", size: None, init: BufInit::Zeroed, indirect: true }),
        Resource::Buffer(BufferDef { name: "sett_inst", size: None, init: BufInit::Zeroed, indirect: false }),
        Resource::Buffer(BufferDef { name: "sett_args", size: None, init: BufInit::Zeroed, indirect: true }),
        Resource::Depth,
    ],

    // Shader binding name -> resource name. Roles (prev/next/plain) are derived
    // from the binding kind + whether the resource is ping-pong.
    names: &[
        ("tidx", "tidx"),
        ("pidx", "pidx"),
        ("iidx", "iidx"),
        ("bidx", "bidx"),
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
        ("step_output_4", "geom_pos"),
        ("step_output_5", "geom_nrm"),
        ("step_output_6", "draw_args"),
        ("step_output_7", "sett_inst"),
        ("step_output_8", "sett_args"),
        ("geom_pos", "geom_pos"),
        ("geom_nrm", "geom_nrm"),
        ("sett_inst", "sett_inst"),
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
        // Scene: the flat ground (materialized ribbon), then the instanced cobble
        // setts. Both depth-tested; the setts protrude and self-occlude.
        Pass::Render(RenderPass {
            label: "scene",
            depth: Some("depth"),
            clear: [0.74, 0.80, 0.86, 1.0],
            items: &[
                RenderItem {
                    label: "ground",
                    module: "main",
                    vs: "scene_vertex",
                    fs: "scene_fragment",
                    vs_bindings: SCENE_VERTEX_BINDINGS,
                    fs_bindings: SCENE_FRAGMENT_BINDINGS,
                    draw_args: "draw_args",
                    depth_write: true,
                },
                RenderItem {
                    label: "setts",
                    module: "main",
                    vs: "sett_vertex",
                    fs: "sett_fragment",
                    vs_bindings: SETT_VERTEX_BINDINGS,
                    fs_bindings: SETT_FRAGMENT_BINDINGS,
                    draw_args: "sett_args",
                    depth_write: true,
                },
            ],
        }),
    ],
};
