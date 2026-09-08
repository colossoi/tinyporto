//! The tiny-porto frame-graph, as plain data.
//!
//! The per-pipeline binding tables and the dispatch/output-size calculations are
//! GENERATED from `wyn/main.wyn`'s descriptor by build.rs (the `descriptor`
//! module). This file authors only what the descriptor can't know: which
//! resources exist, the binding-name -> resource mapping, and the per-frame
//! schedule.

use crate::generated::{GRAPHICS_0_ITEM, GRAPHICS_1_ITEM, GRAPHICS_2_ITEM, GRAPHICS_3_ITEM};
use crate::graph::*;

// Coarse occlusion grid (Hi-Z simple): one texel per OCC_TILE^2 window block,
// rounded up. The unified GTAO compute pipeline's first occ_w*occ_h invocations
// reduce one texel each.
// Must match OCC_TILE / occ_w / occ_h in wyn/hiz.wyn.
const OCC_TILE: u32 = 8;
const fn occ_w(w: u32) -> u32 {
    w.div_ceil(OCC_TILE)
}
const fn occ_h(h: u32) -> u32 {
    h.div_ceil(OCC_TILE)
}
// Wall-brick budget (must match walls.wyn: BRICK_SLOTS + QUOIN_SLOTS + GROUT_SLOTS =
// N_WALL*PER_COURSE*COURSES + 128 + 8 = 8*13*24 + 136 = 2632).
const WALL_BRICKS: u64 = 2632;
// Input event stream: EV_CAP events (must match `step`'s EV_CAP in main.wyn), one
// vec4f32 (16 bytes) each. The host zero-pads unused slots to None each frame.
pub const EV_CAP: usize = 32;
const EVENTS_BYTES: u64 = EV_CAP as u64 * 16;

// Compute pass construction is descriptor-owned: bindings, lowered stages, and
// output-size rules come from generated code. The host contributes only the live
// surface pixel count for image-sized dispatches.
fn compute_entry(module: &'static str, entry: &'static str, w: u32, h: u32) -> ComputePass {
    crate::generated::descriptor_compute_entry(
        module,
        entry,
        u64::from(w) * u64::from(h),
        u64::from(occ_w(w)) * u64::from(occ_h(h)),
    )
    .unwrap_or_else(|| panic!("descriptor has no constructible compute entry {module}:{entry}"))
}

// Draw lists, hoisted out of `graph` because a RenderItem reads the generated
// binding-table statics and so cannot be const-promoted inside a function body.
static SUN_SHADOW_ITEMS: [RenderItem; 1] = [RenderItem { ..GRAPHICS_0_ITEM }];

static SCENE_ITEMS: [RenderItem; 2] = [
    RenderItem { ..GRAPHICS_1_ITEM },
    RenderItem { ..GRAPHICS_2_ITEM },
];

static RESOLVE_ITEMS: [RenderItem; 1] = [RenderItem { ..GRAPHICS_3_ITEM }];

/// The frame graph for a `w` x `h` surface. Image extents and image-sized compute
/// dispatches derive from it; no resolution is hardcoded here or in the shaders.
pub fn graph(w: u32, h: u32) -> Graph {
    let mut graph = Graph {
        resources: vec![
            // Per-frame globals, one std140 uniform block (see `frame_globals` in
            // main.wyn). The driver fills each member by name at the descriptor's
            // offset; member order here is free.
            Resource::UniformBlock {
                name: "frame",
                members: &[
                    BlockMember {
                        field: "resolution",
                        source: FrameSource::Resolution,
                    },
                    BlockMember {
                        field: "mods",
                        source: FrameSource::Mods,
                    },
                    BlockMember {
                        field: "cam_target",
                        source: FrameSource::CamTarget,
                    },
                    BlockMember {
                        field: "cam_az",
                        source: FrameSource::CamAz,
                    },
                    BlockMember {
                        field: "cam_elev",
                        source: FrameSource::CamElev,
                    },
                    BlockMember {
                        field: "cam_dist",
                        source: FrameSource::CamDist,
                    },
                    BlockMember {
                        field: "time",
                        source: FrameSource::Time,
                    },
                ],
            },
            // Input event stream: the host appends one vec4f32 per raw event and
            // zero-pads to EV_CAP; `step` folds it. Written fresh each frame.
            Resource::Buffer(BufferDef {
                name: "events",
                size: Some(EVENTS_BYTES),
                init: BufInit::Zeroed,
                indirect: false,
            }),
            // Persistent state (ping-pong); sizes derived (they're `step` outputs).
            Resource::PingPong {
                name: "uistate",
                size: None,
            },
            Resource::PingPong {
                name: "points",
                size: None,
            },
            Resource::PingPong {
                name: "items",
                size: None,
            },
            Resource::PingPong {
                name: "head",
                size: None,
            },
            Resource::PingPong {
                name: "occ",
                size: None,
            },
            // Derived `step` outputs: ground geometry (two parallel (pos,kind)/(nrm,attr)
            // streams) + its draw args; the per-instance prop records + their draw args.
            Resource::Buffer(BufferDef {
                name: "geom_pos",
                size: None,
                init: BufInit::Zeroed,
                indirect: false,
            }),
            Resource::Buffer(BufferDef {
                name: "geom_nrm",
                size: None,
                init: BufInit::Zeroed,
                indirect: false,
            }),
            Resource::Buffer(BufferDef {
                name: "draw_args",
                size: None,
                init: BufInit::Zeroed,
                indirect: true,
            }),
            Resource::Buffer(BufferDef {
                name: "prop_inst",
                size: None,
                init: BufInit::Zeroed,
                indirect: false,
            }),
            Resource::Buffer(BufferDef {
                name: "prop_args",
                size: None,
                init: BufInit::Zeroed,
                indirect: true,
            }),
            Resource::Depth,
            // Nine Phase 2 (Hi-Z): the scene writes window-space depth here as a second
            // MRT target; the unified GTAO compute pipeline mins it into the coarse
            // occ_depth, which `cull` reads to occlusion-test candidates.
            Resource::Image {
                name: "scene_depth",
                format: TexFormat::R32Float,
                size: ImgSize::Window,
                mips: 1,
            },
            // Sun shadow map: the `sun_shadow` pass writes light-space depth here (R32Float
            // color target, nearest kept by the shared depth buffer), and `light` samples
            // it for directional cast shadows. Window-sized, mirroring scene_depth.
            Resource::Image {
                name: "sun_depth",
                format: TexFormat::R32Float,
                size: ImgSize::Window,
                mips: 1,
            },
            // GTAO working array: raw AO+edges term, consumed by the resolve fragment
            // for the edge-aware denoise.
            Resource::Buffer(BufferDef {
                name: "ao_work",
                size: None,
                init: BufInit::Zeroed,
                indirect: false,
            }),
            // Nine Phase 3 (deferred): the scene writes a thin G-buffer here (albedo +
            // world normal); `resolve_fragment` reads it back and lights it. `blit_args`
            // is the fullscreen-triangle draw (3 verts, 1 instance).
            Resource::Image {
                name: "g_albedo",
                format: TexFormat::Rgba8Unorm,
                size: ImgSize::Window,
                mips: 1,
            },
            Resource::Image {
                name: "g_normal",
                format: TexFormat::Rgba32Float,
                size: ImgSize::Window,
                mips: 1,
            },
            Resource::Buffer(BufferDef {
                name: "blit_args",
                size: Some(16),
                init: BufInit::U32s(&[3, 1, 0, 0]),
                indirect: true,
            }),
            // Shadow caster draws every candidate slot (brick_shadow_vertex regenerates each
            // from its index, camera-independent); dead slots self-cull. Static draw args:
            // 36 verts x WALL_BRICKS instances.
            Resource::Buffer(BufferDef {
                name: "shadow_args",
                size: Some(16),
                init: BufInit::U32s(&[36, WALL_BRICKS as u32, 0, 0]),
                indirect: true,
            }),
        ],

        // Shader binding name -> resource name. Roles (prev/next/plain) are derived
        // from the binding kind + whether the resource is ping-pong.
        names: &[
            ("uistate_in", "uistate"),
            ("points_in", "points"),
            ("items_in", "items"),
            ("head_in", "head"),
            ("occ_in", "occ"),
            ("events", "events"),
            ("frame", "frame"),
            ("tinyporto_frame__compute_0_output_0", "uistate"),
            ("tinyporto_frame__compute_0_output_1", "points"),
            ("tinyporto_frame__compute_0_output_2", "items"),
            ("tinyporto_frame__compute_0_output_3", "head"),
            ("tinyporto_frame__compute_1_output_0", "geom_pos"),
            ("tinyporto_frame__compute_1_output_1", "geom_nrm"),
            ("tinyporto_frame__compute_1_output_2", "draw_args"),
            ("tinyporto_frame__compute_2_output_0", "prop_inst"),
            ("tinyporto_frame__compute_2_output_1", "prop_args"),
            ("tinyporto_frame__compute_3_output_0", "ao_work"),
            ("tinyporto_frame__compute_3_output_1", "occ"),
            ("geom_pos", "geom_pos"),
            ("geom_nrm", "geom_nrm"),
            // The one instanced prop stream, read by both stages of the prop draw.
            ("prop_inst", "prop_inst"),
            // G-buffer views read by the deferred resolve fragment.
            ("scene_albedo", "g_albedo"),
            ("scene_normal", "g_normal"),
            // Sun shadow map (`shm` in `resolve_fragment`; written as the sun_shadow
            // color target).
            ("sun", "sun_depth"),
        ],

        passes: vec![
            // Advance persistent state. Geometry and visibility compute passes are
            // inserted from descriptor dependencies before their consuming draws.
            Pass::Compute(compute_entry("main", "tinyporto_frame__compute_0", w, h)),
            // Sun shadow map: rasterize the wall bricks through the sun's ortho light
            // camera, storing nearest light-space depth into sun_depth. Reuses the shared
            // window depth buffer (cleared here, then re-cleared by the scene pass). Runs
            // before `light`, which samples it.
            Pass::Render(RenderPass {
                label: "sun_shadow",
                depth: Some("depth"),
                color: &[ColorTarget {
                    target: Some("sun_depth"),
                    format: Some(TexFormat::R32Float),
                    clear: [1.0, 1.0, 1.0, 1.0],
                }],
                items: &SUN_SHADOW_ITEMS,
            }),
            // Scene: the flat ground (materialized ribbon), then one instanced draw over
            // every prop — cobble setts and wall blocks. Both depth-tested; the props
            // protrude and self-occlude, and the wall blocks occlude the setts.
            Pass::Render(RenderPass {
                label: "scene",
                depth: Some("depth"),
                // Deferred G-buffer (no surface write here): albedo @0 (a=0 sky mask, so
                // the clear is the sky color at a=0), world normal @1, window depth @2
                // (also the Hi-Z source, cleared to the far plane).
                color: &[
                    ColorTarget {
                        target: Some("g_albedo"),
                        format: Some(TexFormat::Rgba8Unorm),
                        clear: [0.74, 0.80, 0.86, 0.0],
                    },
                    ColorTarget {
                        target: Some("g_normal"),
                        format: Some(TexFormat::Rgba32Float),
                        clear: [0.0, 0.0, 0.0, 0.0],
                    },
                    ColorTarget {
                        target: Some("scene_depth"),
                        format: Some(TexFormat::R32Float),
                        clear: [1.0, 1.0, 1.0, 1.0],
                    },
                ],
                items: &SCENE_ITEMS,
            }),
            // GTAO + Hi-Z reduce: one pass over the scene depth written above. Every
            // invocation integrates horizon AO into ao_work; the first occ_w*occ_h also min
            // their coarse occ_depth tile, which `cull` reads next frame. Runs before the
            // resolve, which reads ao_work and folds the edge-aware denoise into shading.
            Pass::Compute(compute_entry("main", "tinyporto_frame__compute_3", w, h)),
            // Deferred resolve: one fullscreen triangle whose fragment reads the G-buffer,
            // folds in the GTAO term (including the edge-aware denoise of ao_work) and
            // writes the final colour (sun + shadows + AO-attenuated sky, tonemapped)
            // straight to the surface. No depth (it covers every pixel unconditionally).
            Pass::Render(RenderPass {
                label: "resolve",
                depth: None,
                color: &[ColorTarget {
                    target: None,
                    format: None,
                    clear: [0.74, 0.80, 0.86, 1.0],
                }],
                items: &RESOLVE_ITEMS,
            }),
        ],
    };
    crate::generated::insert_descriptor_prerequisites(
        &mut graph, u64::from(w) * u64::from(h),
        u64::from(occ_w(w)) * u64::from(occ_h(h)),
    );
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_prerequisites_accept_live_viewport_sizes() {
        let (width, height) = (1279, 799);
        let graph = graph(width, height);
        let expected = [
            u64::from(width) * u64::from(height),
            u64::from(occ_w(width)) * u64::from(occ_h(height)),
        ];
        let compute: Vec<_> = graph.passes.iter().filter_map(|pass| match pass {
            Pass::Compute(pass) => Some(pass),
            _ => None,
        }).collect();
        // More than the two authored passes must be generated successfully,
        // including prerequisites whose dispatches have nontrivial domains.
        assert!(compute.len() > 2);
        assert!(compute.iter().all(|pass| pass.runtime_counts == expected));
        assert!(compute.iter().any(|pass| pass.stages.iter().any(|stage| stage.groups[0] > 1)));
    }
}
