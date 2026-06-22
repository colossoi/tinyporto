//! The tiny-porto frame-graph, as plain data.
//!
//! IMPORTANT: this file names resources/passes for the app, but it is still just
//! a generic `Graph` value — there are NO domain types here (no Ground/Building).
//! All meaning lives in the Wyn program under `../wyn`. If a concept like
//! "ground" or "building" ever needs to appear in Rust, stop and reconsider:
//! it almost certainly belongs in Wyn instead.
//!
//! This must stay in sync with the descriptor `wyn compile` emits for
//! `wyn/main.wyn` (see `shaders/main.json`). A future `build.rs` enforces that.

use crate::graph::*;

pub const GRAPH: Graph = Graph {
    modules: &[("main", "shaders/main.spv")],

    resources: &[
        Resource::SysUniform { name: "iResolution", kind: SysUniform::Resolution },
        Resource::SysUniform { name: "iTime", kind: SysUniform::Time },
        Resource::SysUniform { name: "iMouse", kind: SysUniform::Mouse },
    ],

    passes: &[Pass::Render(RenderPass {
        label: "scene",
        module: "main",
        vs: "vertex_main",
        fs: "fragment_main",
        // Mirrors shaders/main.json fragment bindings (set 1).
        bindings: &[
            Binding { set: 1, binding: 0, resource: "iResolution", kind: BindingKind::Uniform },
            Binding { set: 1, binding: 1, resource: "iTime", kind: BindingKind::Uniform },
            Binding { set: 1, binding: 2, resource: "iMouse", kind: BindingKind::Uniform },
        ],
        draw: Draw::Fullscreen,
    })],
};
