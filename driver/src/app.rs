//! The tiny-porto frame-graph, as plain data.
//!
//! The per-pipeline binding tables and the dispatch/output-size calculations are
//! GENERATED from `wyn/main.wyn`'s descriptor by build.rs (the `descriptor`
//! module). This file authors only what the descriptor can't know: which
//! resources exist, the seed sizes, the binding-name -> resource mapping, and the
//! per-frame schedule.

use crate::generated::{
    frame_prepare_out_bytes, frame_prepare_stages, gtao_main_out_bytes, gtao_main_stages,
    BLIT_VERTEX_BINDINGS, BRICK_SHADOW_VERTEX_BINDINGS, FRAME_PREPARE_BINDINGS,
    FRAME_PREPARE_STAGE_COUNT, GTAO_MAIN_BINDINGS, GTAO_MAIN_STAGE_COUNT, PROP_FRAGMENT_BINDINGS,
    PROP_VERTEX_BINDINGS, RESOLVE_FRAGMENT_BINDINGS, SCENE_FRAGMENT_BINDINGS,
    SCENE_VERTEX_BINDINGS, SHADOW_FRAGMENT_BINDINGS,
};
use crate::graph::*;

// Seed element counts — the only buffer sizes authored by hand (must match the
// constants in wyn/paint.wyn / wyn/bricks.wyn). Everything downstream is derived
// by the generated `step_out_bytes` from these.
const POINTS_CAP: u64 = 1024;
const ITEMS_CAP: u64 = 128;
const TESS_CAP: u64 = 95244; // ground geom stream: TESS_BASE(12) + ITEMS_CAP * TESS_VPI
const BRICK_COUNT: u64 = 36960; // running-bond grid cells = sett instances (BCOLS*BROWS)
const TIDX_BYTES: u64 = TESS_CAP * 4;
const PIDX_BYTES: u64 = POINTS_CAP * 4;
const IIDX_BYTES: u64 = ITEMS_CAP * 4;
// Coarse occlusion grid (Hi-Z simple): one texel per OCC_TILE^2 window block,
// rounded up. `gtao_main`'s first occ_w*occ_h invocations reduce one texel each.
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
// The prop domain: cobble setts first, then the wall blocks (must match main.wyn's
// PROP_SETTS / PROP_WALLS split). `propidx` is the one iota the prop generator maps
// over, and the instanced prop draw runs over the same slot count.
const PROP_COUNT: u64 = BRICK_COUNT + WALL_BRICKS;
const PROPIDX_BYTES: u64 = PROP_COUNT * 4;
// Input event stream: EV_CAP events (must match `step`'s EV_CAP in main.wyn), one
// vec4f32 (16 bytes) each. The host zero-pads unused slots to None each frame.
pub const EV_CAP: usize = 32;
const EVENTS_BYTES: u64 = EV_CAP as u64 * 16;

// Logical frame preparation advances state, builds ground geometry, and emits one
// visibility record per prop (cobble setts and wall blocks alike).
const fn frame_prepare_out(binding: u32) -> u64 {
    frame_prepare_out_bytes(binding, IIDX_BYTES, PIDX_BYTES, PROPIDX_BYTES, TIDX_BYTES)
}

// The GTAO pass writes only images (ao_work / occ_depth) — no sized buffer
// outputs, so its out-size calc takes just the binding.
const fn gtao_main_out(binding: u32) -> u64 {
    gtao_main_out_bytes(binding)
}

// Ordered compute stages per entry, dispatch sized from the seed counts. The stage
// entry names and per-stage dispatch rules come from the descriptor (via the
// generated `*_stages`); only the seed sizes are authored here. The image-sized ones
// take the live window, so nothing bakes a resolution.
fn frame_prepare_stage_list(w: u32, h: u32) -> [ComputeStage; FRAME_PREPARE_STAGE_COUNT] {
    let occ_pixels = (occ_w(w) as u64) * (occ_h(h) as u64);
    frame_prepare_stages(IIDX_BYTES, occ_pixels, PIDX_BYTES, PROPIDX_BYTES, TIDX_BYTES)
}
fn gtao_main_stage_list(w: u32, h: u32) -> [ComputeStage; GTAO_MAIN_STAGE_COUNT] {
    gtao_main_stages((w as u64) * (h as u64))
}

// Draw lists, hoisted out of `graph` because a RenderItem reads the generated
// binding-table statics and so cannot be const-promoted inside a function body.
static SUN_SHADOW_ITEMS: [RenderItem; 1] = [RenderItem {
    label: "brick_shadow",
    module: "main",
    vs: "brick_shadow_vertex",
    fs: "shadow_fragment",
    vs_bindings: BRICK_SHADOW_VERTEX_BINDINGS,
    fs_bindings: SHADOW_FRAGMENT_BINDINGS,
    draw_args: "shadow_args",
    depth_write: true,
}];

static SCENE_ITEMS: [RenderItem; 2] = [
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
        label: "props",
        module: "main",
        vs: "prop_vertex",
        fs: "prop_fragment",
        vs_bindings: PROP_VERTEX_BINDINGS,
        fs_bindings: PROP_FRAGMENT_BINDINGS,
        draw_args: "prop_args",
        depth_write: true,
    },
];

static RESOLVE_ITEMS: [RenderItem; 1] = [RenderItem {
    label: "resolve",
    module: "main",
    vs: "blit_vertex",
    fs: "resolve_fragment",
    vs_bindings: BLIT_VERTEX_BINDINGS,
    fs_bindings: RESOLVE_FRAGMENT_BINDINGS,
    draw_args: "blit_args",
    depth_write: false,
}];

/// The frame graph for a `w` x `h` surface. Image extents and image-sized compute
/// dispatches derive from it; no resolution is hardcoded here or in the shaders.
pub fn graph(w: u32, h: u32) -> Graph {
    Graph {
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
        // Iota index seeds (the hand-picked design sizes).
        Resource::Buffer(BufferDef {
            name: "tidx",
            size: Some(TIDX_BYTES),
            init: BufInit::Iota,
            indirect: false,
        }),
        Resource::Buffer(BufferDef {
            name: "pidx",
            size: Some(PIDX_BYTES),
            init: BufInit::Iota,
            indirect: false,
        }),
        Resource::Buffer(BufferDef {
            name: "iidx",
            size: Some(IIDX_BYTES),
            init: BufInit::Iota,
            indirect: false,
        }),
        // The prop iota: one slot per cobble sett, then one per wall block.
        Resource::Buffer(BufferDef {
            name: "propidx",
            size: Some(PROPIDX_BYTES),
            init: BufInit::Iota,
            indirect: false,
        }),
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
        // MRT target; `gtao_main` mins it into the coarse occ_depth, which `cull`
        // reads to occlusion-test candidates.
        Resource::Image {
            name: "scene_depth",
            format: TexFormat::R32Float,
            size: ImgSize::Window,
            mips: 1,
        },
        Resource::Image {
            name: "occ_depth",
            format: TexFormat::R32Float,
            size: ImgSize::Fixed {
                w: occ_w(w),
                h: occ_h(h),
            },
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
        // GTAO working image: raw AO+edges term, sampled by the light pass for the
        // edge-aware denoise.
        Resource::Image {
            name: "ao_work",
            format: TexFormat::Rgba16Float,
            size: ImgSize::Window,
            mips: 1,
        },
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
            format: TexFormat::Rgba16Float,
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
        ("tidx", "tidx"),
        ("pidx", "pidx"),
        ("iidx", "iidx"),
        ("propidx", "propidx"),
        ("uistate_in", "uistate"),
        ("points_in", "points"),
        ("items_in", "items"),
        ("head_in", "head"),
        ("events", "events"),
        ("frame", "frame"),
        ("frame_prepare_output_0", "uistate"),
        ("frame_prepare_output_1", "points"),
        ("frame_prepare_output_2", "items"),
        ("frame_prepare_output_3", "head"),
        ("frame_prepare_output_4", "geom_pos"),
        ("frame_prepare_output_5", "geom_nrm"),
        ("frame_prepare_output_6", "draw_args"),
        ("frame_prepare_output_7", "prop_inst"),
        ("frame_prepare_output_8", "prop_args"),
        ("geom_pos", "geom_pos"),
        ("geom_nrm", "geom_nrm"),
        // The one instanced prop stream, read by both stages of the prop draw.
        ("prop_inst", "prop_inst"),
        // Hi-Z occlusion image views (`sd`/`od` are the shader param names).
        ("od", "occ_depth"),
        ("sd", "scene_depth"),
        // G-buffer views read by the deferred resolve fragment.
        ("ga", "g_albedo"),
        ("gn", "g_normal"),
        // Sun shadow map (`shm` in `resolve_fragment`; written as the sun_shadow
        // color target).
        ("shm", "sun_depth"),
        // GTAO view: ao_work (gtao_main writes `aw`, `resolve_fragment` samples `aw`
        // and denoises inline).
        ("aw", "ao_work"),
    ],

    passes: vec![
        // Logical frame preparation: advance persistent state, tessellate the
        // ground ribbon, and build visibility records for cobble/wall instances.
        Pass::Compute(ComputePass {
            label: "frame_prepare",
            module: "main",
            bindings: FRAME_PREPARE_BINDINGS,
            stages: frame_prepare_stage_list(w, h).to_vec(),
            out_bytes: frame_prepare_out,
        }),
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
                    format: Some(TexFormat::Rgba16Float),
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
        Pass::Compute(ComputePass {
            label: "gtao_main",
            module: "main",
            bindings: GTAO_MAIN_BINDINGS,
            stages: gtao_main_stage_list(w, h).to_vec(),
            out_bytes: gtao_main_out,
        }),
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
    }
}
