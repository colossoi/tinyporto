//! The tiny-porto frame-graph, as plain data.
//!
//! The per-pipeline binding tables and the dispatch/output-size calculations are
//! GENERATED from `wyn/main.wyn`'s descriptor by build.rs (the `descriptor`
//! module). This file authors only what the descriptor can't know: which
//! resources exist, the seed sizes, the binding-name -> resource mapping, and the
//! per-frame schedule.

use crate::generated::{
    cull_out_bytes, cull_stages, gtao_depth_out_bytes, gtao_depth_stages, gtao_main_out_bytes,
    gtao_main_stages, light_out_bytes, light_stages, occ_reduce_out_bytes, occ_reduce_stages,
    step_out_bytes, step_stages, walls_out_bytes, walls_stages, BLIT_VERTEX_BINDINGS,
    BRICK_FRAGMENT_BINDINGS, BRICK_SHADOW_VERTEX_BINDINGS, BRICK_VERTEX_BINDINGS, CULL_BINDINGS,
    CULL_STAGE_COUNT, GTAO_DEPTH_BINDINGS, GTAO_DEPTH_STAGE_COUNT, GTAO_MAIN_BINDINGS,
    GTAO_MAIN_STAGE_COUNT, LIGHT_BINDINGS, LIGHT_STAGE_COUNT, OCC_REDUCE_BINDINGS,
    OCC_REDUCE_STAGE_COUNT, RESOLVE_FRAGMENT_BINDINGS, SCENE_FRAGMENT_BINDINGS,
    SCENE_VERTEX_BINDINGS, SETT_FRAGMENT_BINDINGS, SETT_VERTEX_BINDINGS, SHADOW_FRAGMENT_BINDINGS,
    STEP_BINDINGS, STEP_STAGE_COUNT, WALLS_BINDINGS, WALLS_STAGE_COUNT,
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
const BIDX_BYTES: u64 = BRICK_COUNT * 4;
// Coarse occlusion grid (Hi-Z simple): window (1280x800) / 8. `occ_reduce`
// dispatches one invocation per coarse texel (sized from occ_depth's pixel count).
const OCC_COUNT: u64 = 160 * 100;
// Wall-brick budget (must match walls.wyn: BRICK_SLOTS + QUOIN_SLOTS + GROUT_SLOTS =
// N_WALL*PER_COURSE*COURSES + 128 + 8 = 8*13*24 + 136 = 2632). `wbidx` is the iota
// domain the `walls` generator maps over (one slot per candidate block).
const WALL_BRICKS: u64 = 2632;
const WBIDX_BYTES: u64 = WALL_BRICKS * 4;
// Window pixel count — the dispatch domain for the per-pixel compute passes
// (gtao_depth/main, light), each sized from its window-sized write image.
const PXL_COUNT: u64 = 1280 * 800;
// Input event stream: EV_CAP events (must match `step`'s EV_CAP in main.wyn), one
// vec4f32 (16 bytes) each. The host zero-pads unused slots to None each frame.
pub const EV_CAP: usize = 32;
const EVENTS_BYTES: u64 = EV_CAP as u64 * 16;

// `step`'s output sizes, by binding, from the generated calc (the seed sizes are
// baked in). The driver pairs this with the binding table to size each output
// buffer by name — no output binding numbers appear here.
const fn step_out(binding: u32) -> u64 {
    step_out_bytes(binding, PIDX_BYTES, TIDX_BYTES)
}

// `cull`'s output sizes (compacted sett records + draw args + scratch count),
// derived from the brick-grid seed size.
const fn cull_out(binding: u32) -> u64 {
    cull_out_bytes(binding, BIDX_BYTES)
}

// `occ_reduce` writes only occ_depth (an image, not a buffer) — no sized outputs.
const fn occ_out(binding: u32) -> u64 {
    occ_reduce_out_bytes(binding)
}

// `walls`' output sizes (compacted wall_brick records + draw args + filter scratch),
// derived from the wall-brick iota size.
const fn walls_out(binding: u32) -> u64 {
    walls_out_bytes(binding, WBIDX_BYTES)
}

// `light` / GTAO passes write only images (lit / vdepth / ao_work) — no sized
// buffer outputs, so their out-size calcs take just the binding.
const fn light_out(binding: u32) -> u64 {
    light_out_bytes(binding)
}
const fn gtao_depth_out(binding: u32) -> u64 {
    gtao_depth_out_bytes(binding)
}
const fn gtao_main_out(binding: u32) -> u64 {
    gtao_main_out_bytes(binding)
}

// Ordered compute stages per entry, dispatch sized from the seed counts. The
// stage entry names and per-stage dispatch rules come from the descriptor (via
// the generated `*_stages`); only the seed byte sizes are authored here.
static STEP_STAGES: [ComputeStage; STEP_STAGE_COUNT] =
    step_stages(IIDX_BYTES, PIDX_BYTES, TIDX_BYTES);
static CULL_STAGES: [ComputeStage; CULL_STAGE_COUNT] = cull_stages(BIDX_BYTES);
static OCC_STAGES: [ComputeStage; OCC_REDUCE_STAGE_COUNT] = occ_reduce_stages(OCC_COUNT);
static WALLS_STAGES: [ComputeStage; WALLS_STAGE_COUNT] = walls_stages(WBIDX_BYTES);
static LIGHT_STAGES: [ComputeStage; LIGHT_STAGE_COUNT] = light_stages(PXL_COUNT);
static GTAO_DEPTH_STAGES: [ComputeStage; GTAO_DEPTH_STAGE_COUNT] = gtao_depth_stages(PXL_COUNT);
static GTAO_MAIN_STAGES: [ComputeStage; GTAO_MAIN_STAGE_COUNT] = gtao_main_stages(PXL_COUNT);

pub const GRAPH: Graph = Graph {
    resources: &[
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
        Resource::Buffer(BufferDef {
            name: "bidx",
            size: Some(BIDX_BYTES),
            init: BufInit::Iota,
            indirect: false,
        }),
        // Derived `step` outputs: ground geometry (two parallel (pos,kind)/(nrm,attr)
        // streams) + its draw args; the per-instance sett records + their draw args.
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
            name: "sett_inst",
            size: None,
            init: BufInit::Zeroed,
            indirect: false,
        }),
        Resource::Buffer(BufferDef {
            name: "sett_args",
            size: None,
            init: BufInit::Zeroed,
            indirect: true,
        }),
        Resource::Depth,
        // Nine Phase 2 (Hi-Z): the scene writes window-space depth here as a second
        // MRT target; `occ_reduce` mins it into the coarse occ_depth, which `cull`
        // reads to occlusion-test candidates. `otile` is occ_reduce's iota domain.
        Resource::Image {
            name: "scene_depth",
            format: TexFormat::R32Float,
            size: ImgSize::Window,
            mips: 1,
        },
        Resource::Image {
            name: "occ_depth",
            format: TexFormat::R32Float,
            size: ImgSize::Fixed { w: 160, h: 100 },
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
        // GTAO working images (all window-sized, write+sample like `lit`): viewspace
        // depth, the raw AO+edges term, and the denoised AO the light pass reads.
        Resource::Image {
            name: "vdepth",
            format: TexFormat::R32Float,
            size: ImgSize::Window,
            mips: 1,
        },
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
        // Deferred lighting: `light` writes the final `lit` image (one invocation per
        // window pixel); the resolve fragment copies it to the surface.
        Resource::Image {
            name: "lit",
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
        // Brick buildings: `wbidx` is the candidate-brick iota; `walls` compacts the
        // visible bricks into wall_brick_inst and writes wall_brick_args (its live
        // instance count). Bricks are drawn into the G-buffer as real occluders.
        Resource::Buffer(BufferDef {
            name: "wbidx",
            size: Some(WBIDX_BYTES),
            init: BufInit::Iota,
            indirect: false,
        }),
        Resource::Buffer(BufferDef {
            name: "wall_brick_inst",
            size: None,
            init: BufInit::Zeroed,
            indirect: false,
        }),
        Resource::Buffer(BufferDef {
            name: "wall_brick_args",
            size: None,
            init: BufInit::Zeroed,
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
        ("bidx", "bidx"),
        ("uistate_in", "uistate"),
        ("points_in", "points"),
        ("items_in", "items"),
        ("head_in", "head"),
        ("events", "events"),
        ("frame", "frame"),
        ("step_output_0", "uistate"),
        ("step_output_1", "points"),
        ("step_output_2", "items"),
        ("step_output_3", "head"),
        ("step_output_4", "geom_pos"),
        ("step_output_5", "geom_nrm"),
        ("step_output_6", "draw_args"),
        ("cull_output_0", "sett_inst"),
        ("cull_output_1", "sett_args"),
        ("geom_pos", "geom_pos"),
        ("geom_nrm", "geom_nrm"),
        ("sett_inst", "sett_inst"),
        // Hi-Z occlusion image views (`sd`/`od` are the shader param names).
        ("od", "occ_depth"),
        ("sd", "scene_depth"),
        // G-buffer views read by the deferred resolve fragment.
        ("ga", "g_albedo"),
        ("gn", "g_normal"),
        // Sun shadow map (`shm` in `light`; written as the sun_shadow color target).
        ("shm", "sun_depth"),
        // GTAO views: vdepth (write `vd` / sample `vd`), ao_work (gtao_main writes `aw`,
        // `light` samples `aw` and denoises inline).
        ("vd", "vdepth"),
        ("aw", "ao_work"),
        // Brick generator I/O + the instanced brick draw.
        ("wbidx", "wbidx"),
        ("walls_output_0", "wall_brick_inst"),
        ("walls_output_1", "wall_brick_args"),
        ("wall_brick_inst", "wall_brick_inst"),
        // Deferred lighting output: `light` writes `lit` (view `lt`); the resolve
        // fragment reads it.
        ("lt", "lit"),
    ],

    passes: &[
        // Advance all persistent state + tessellate the ribbon (one kernel).
        Pass::Compute(ComputePass {
            label: "step",
            module: "main",
            bindings: STEP_BINDINGS,
            stages: &STEP_STAGES,
            out_bytes: step_out,
        }),
        // Build one record per brick cell. Culled setts get height=0 and self-cull
        // in the vertex; the candidate budget is small enough to avoid compacting.
        Pass::Compute(ComputePass {
            label: "cull",
            module: "main",
            bindings: CULL_BINDINGS,
            stages: &CULL_STAGES,
            out_bytes: cull_out,
        }),
        // Brick-building generator: lay one record per candidate slot. Dead or
        // off-frustum slots self-cull in the vertex; the candidate budget is small
        // enough that wall bricks do not need a compacting filter stage.
        Pass::Compute(ComputePass {
            label: "walls",
            module: "main",
            bindings: WALLS_BINDINGS,
            stages: &WALLS_STAGES,
            out_bytes: walls_out,
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
            items: &[RenderItem {
                label: "brick_shadow",
                module: "main",
                vs: "brick_shadow_vertex",
                fs: "shadow_fragment",
                vs_bindings: BRICK_SHADOW_VERTEX_BINDINGS,
                fs_bindings: SHADOW_FRAGMENT_BINDINGS,
                draw_args: "shadow_args",
                depth_write: true,
            }],
        }),
        // Scene: the flat ground (materialized ribbon), then the instanced cobble
        // setts. Both depth-tested; the setts protrude and self-occlude.
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
                RenderItem {
                    label: "bricks",
                    module: "main",
                    vs: "brick_vertex",
                    fs: "brick_fragment",
                    vs_bindings: BRICK_VERTEX_BINDINGS,
                    fs_bindings: BRICK_FRAGMENT_BINDINGS,
                    draw_args: "wall_brick_args",
                    depth_write: true,
                },
            ],
        }),
        // Hi-Z reduce: min the scene depth (written by the pass above) into the
        // coarse occ_depth. Runs last so it sees this frame's depth; `cull` reads
        // the result next frame. No visible output — pure occlusion bookkeeping.
        Pass::Compute(ComputePass {
            label: "occ_reduce",
            module: "main",
            bindings: OCC_REDUCE_BINDINGS,
            stages: &OCC_STAGES,
            out_bytes: occ_out,
        }),
        // GTAO: linearize scene depth to viewspace, then integrate horizon AO into
        // ao_work. Runs after the scene pass (needs scene_depth) and before `light`,
        // which reads ao_work and folds the edge-aware denoise into its shading.
        Pass::Compute(ComputePass {
            label: "gtao_depth",
            module: "main",
            bindings: GTAO_DEPTH_BINDINGS,
            stages: &GTAO_DEPTH_STAGES,
            out_bytes: gtao_depth_out,
        }),
        Pass::Compute(ComputePass {
            label: "gtao_main",
            module: "main",
            bindings: GTAO_MAIN_BINDINGS,
            stages: &GTAO_MAIN_STAGES,
            out_bytes: gtao_main_out,
        }),
        // Deferred lighting: one compute invocation per pixel reads the G-buffer,
        // folds in the GTAO term (including the edge-aware denoise of ao_work) and
        // writes the final `lit` image (sun + shadows +
        // AO-attenuated sky, tonemapped).
        Pass::Compute(ComputePass {
            label: "light",
            module: "main",
            bindings: LIGHT_BINDINGS,
            stages: &LIGHT_STAGES,
            out_bytes: light_out,
        }),
        // Deferred resolve: one fullscreen triangle copies the lit image to the
        // surface. No depth (it covers every pixel unconditionally).
        Pass::Render(RenderPass {
            label: "resolve",
            depth: None,
            color: &[ColorTarget {
                target: None,
                format: None,
                clear: [0.74, 0.80, 0.86, 1.0],
            }],
            items: &[RenderItem {
                label: "resolve",
                module: "main",
                vs: "blit_vertex",
                fs: "resolve_fragment",
                vs_bindings: BLIT_VERTEX_BINDINGS,
                fs_bindings: RESOLVE_FRAGMENT_BINDINGS,
                draw_args: "blit_args",
                depth_write: false,
            }],
        }),
    ],
};
