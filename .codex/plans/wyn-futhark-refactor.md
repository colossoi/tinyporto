# Wyn Futhark-Style Refactor Plan

## Baseline Before Refactoring

Use `driver/target/debug/build/tinyporto-driver-70ef90d265866374/out/main.json` as the current descriptor capability baseline.

Before each structural refactor, capture enough evidence to prove the change is not behavioral:

- Pipeline and stage list from the descriptor.
- Resource lifetimes from `frame_graph`.
- A smoke image from `cargo run -- --frames 5`.

The baseline is the guardrail: each PR should make the Wyn source more Futhark-like while preserving descriptor shape, pass ordering, and rendered output unless a PR explicitly says otherwise.

## PR 1: Image Entry Wrappers

Status: done. Committed as `a85d544 Refactor renderer entries and stabilize instances`.

Target entries in `wyn/main.wyn`:

- `occ_reduce`
- `gtao_depth`
- `gtao_main`
- `light`

Shape:

- Add shared `pixel_xy(gid, width)` helper.
- Move per-pixel or per-tile work into pure `def`s.
- Keep each image-writing entry as a thin wrapper that converts `gid -> xy` and performs the unique image write.

Example:

```wyn
def gtao_depth_pixel(sd: storage_image, x: i32, y: i32) vec4f32 = ...

#[compute]
entry gtao_depth(#[builtin(global_invocation_id)] gid: vec3u32,
                 #[view(vdepth, storage_write)] vd: *storage_image,
                 #[view(scene_depth, storage_read)] sd: storage_image) () =
  let xy = pixel_xy(gid, 1280) in
  vd with [xy] = gtao_depth_pixel(sd, xy.x, xy.y)
```

This keeps the current compiler model, but makes entries read like Futhark-style `tabulate` bodies.

## PR 2: Domain Records And Accessors, No ABI Changes

Introduce domain types and constructors for packed values before changing storage ABI.

Add record types for concepts currently implicit in `[]f32`, `[]vec4f32`, or parallel arrays:

- `ui_state`
- `stroke_head`
- `paint_item`
- `ground_vertex`
- `sett_inst`

First pass constraints:

- Keep the existing buffer ABI.
- Add accessor and constructor functions around packed values.
- Do not change descriptor storage buffer layouts.
- Use `walls.wyn`'s `block` record buffer as evidence that record buffers are viable later, but do not migrate these buffers yet.

Success criteria:

- Call sites read in terms of domain concepts instead of raw swizzles where practical.
- Descriptor storage ABI is unchanged.
- `cargo check` passes.
- Smoke render remains visually equivalent.

## PR 3: Step Decomposition And Geometry Stream Collapse

Refactor `step` in `wyn/main.wyn` into pure phases while keeping a single `entry step` initially.

Target shape:

```wyn
def next_state(frame, events, prev_state) state_update = ...
def update_points(prev_points, update, indices) []vec2f32 = ...
def update_items(prev_items, update, indices) []paint_item = ...
def ground_vertices(items, points, indices) []ground_vertex = ...
def ground_draw_args(geom) [4]u32 = ...
```

Keep `tidx`, `pidx`, and `iidx` seed buffers for now. Later, if the compiler handles fixed ranges as dispatch domains, replace those seed buffers with `iota` or range-generated domains.

Collapse parallel geometry streams:

- Current shape: `geom_pos` and `geom_nrm` are two same-domain maps over `tidx`.
- Target source shape:

```wyn
type ground_vertex = {
  pos: vec3f32,
  kind: f32,
  nrm: vec3f32,
  attr: f32,
}
```

Emit `[]ground_vertex`, or `[](vec4f32, vec4f32)` if the current ABI prefers tuple splitting. Let the compiler and descriptor split or pack the result.

## PR 4: Candidate/Filter Culling Shape

Make culling read as candidate generation plus filtering.

Target helpers:

```wyn
def sett_candidate(frame, items, points, od, i) sett_inst = ...
def sett_visible(c) bool = ...
def visible_setts(...) ?k.[k]sett_inst =
  filter(sett_visible, map(sett_candidate, bidx))
```

Apply the same structure to walls:

- Candidate generation is pure.
- Visibility predicate is separate.
- Draw args are derived from `length live`.
- Entries become thin wrappers returning `(live, draw_args(length live))`.

This restores the more natural Futhark-style `map -> filter -> draw_args` source shape once the blinking/compaction issues are understood and safe to reintroduce.

## PR 5: Split Root By Pass Ownership

After internal defs are pure, move pass families out of the giant root module.

Target module layout:

- `wyn/entries/state.wyn`: `step`
- `wyn/entries/culling.wyn`: `cull`, `walls`
- `wyn/entries/deferred.wyn`: G-buffer, lighting, resolve
- `wyn/entries/shadow_pass.wyn`: shadow map pass
- `wyn/entries/gtao_pass.wyn`: GTAO passes

`main.wyn` should become mostly:

- Resource declarations.
- Frame/global types.
- Imports.
- Re-exported entries.

This should happen only after the relevant defs have been made pure enough that moving them does not obscure dataflow.

## PR 6: Descriptor-Driven Rust Graph Consumption

Move pass graph meaning out of Rust.

The descriptor now has `frame_graph`, but `driver/src/app.rs` still hand-authors resources, names, sizes, and pass order. Longer term, make the driver consume the descriptor frame graph and leave only host-specific facts in Rust:

- Event buffer initialization.
- Frame uniform sources.
- Persistent ping-pong policy.
- Static draw args that truly come from the host.

This is the step that makes "all meaning lives in Wyn" actually true.

## PR 7: Logical Different-Domain Frame Functions

Move from "thin physical entries" toward Futhark-style source composition.

The goal is not to merge only same-domain dispatches. Idiomatic Futhark source composes many array transformations over many domains inside ordinary functions; the compiler then lowers that source program into as many kernels as needed. Wyn should follow that model: fewer logical source functions, many physical lowered stages when necessary.

Introduce pure logical functions that describe renderer dataflow across domains:

```wyn
def update_world(frame, events, world) world = ...
def build_scene(frame, world) scene_geometry = ...
def visibility(frame, scene_depth, world) visible_geometry = ...
def shade(frame, gbuffer, ao, shadows, world) lit_image = ...
```

These functions may internally use different domains:

- Event fold domain.
- Point/item update domains.
- Tessellation domain.
- Sett and wall candidate domains.
- Occlusion tile domain.
- Window pixel domain.
- Shadow/raster domains where graphics stages remain physical.

Keep existing physical entries as wrappers while introducing these logical functions. The source should start to read as one composed frame computation, even though the backend still emits multiple dispatches and render stages.

Acceptance criteria:

- Logical functions make cross-domain dataflow explicit in Wyn source.
- Existing entry names and descriptor shape can remain stable during the first pass.
- No host-side graph knowledge is added to compensate for the source refactor.

## PR 8: Descriptor Logical Entries Vs Physical Stages

Teach the descriptor to preserve both layers of meaning:

- Logical source operation: `update_world`, `build_scene`, `visibility`, `lighting`, or eventually `frame`.
- Physical lowered stages: generated compute dispatches, vertex stages, fragment stages, and required barriers/resources.

The driver should consume physical lowered stages, but diagnostics and future graph tooling should be able to point back to the logical Wyn source operation.

This is the key compiler/descriptor feature for Futhark-like structure: one source function may lower to many kernels, including kernels over different domains.

Acceptance criteria:

- Descriptor can represent many physical stages belonging to one logical source operation.
- Resource lifetimes and barriers remain explicit at the physical level.
- Driver scheduling does not require manually-authored Rust pass meaning.

## PR 9: Collapse Host-Visible Scheduling Around Logical Frame Graph

Once the descriptor carries logical-vs-physical structure, make the Wyn source own the renderer graph at the logical level.

Target source shape:

```wyn
def frame(frame_globals, events, world, history) (world, image, history) =
  let world' = update_world(frame_globals, events, world)
  let scene = build_scene(frame_globals, world')
  let gbuffer = rasterize_scene(frame_globals, scene)
  let vis = visibility(frame_globals, gbuffer.depth, world')
  let shadows = shadow_pass(frame_globals, world', vis)
  let ao = ambient_occlusion(frame_globals, gbuffer.depth)
  let lit = shade(frame_globals, gbuffer, ao, shadows, world')
  in (world', lit, update_history(history, lit))
```

The compiler may still lower this into the existing physical stages. The important change is that Rust no longer authors the conceptual schedule; it follows the descriptor.

Acceptance criteria:

- Wyn source presents a small number of renderer-level transformations.
- Rust host remains responsible only for host facts: events, frame uniforms, swapchain, persistent allocations, and execution of descriptor stages.
- Physical entry count may stay high if required by GPU stage/domain/resource constraints.

## PR 10+: Physical Entry Consolidation As An Optimization

Only after logical consolidation exists, opportunistically merge physical entries where it is genuinely safe.

Possible candidates:

- Same-domain image passes when barriers and intermediate lifetimes are unnecessary.
- Depth pyramid stages if they become expressible as one logical reduction pipeline.
- Fixed-output setup stages from `step` if compiler lowering can fuse them safely.

Constraints:

- Do not merge graphics stages that must remain separate vertex/fragment/raster passes.
- Do not merge entries if it hides resource lifetimes, usage transitions, barriers, or temporal feedback.
- Do not turn a clean logical frame function into a manually bundled mega-entry just to reduce physical entry count.

Physical consolidation is secondary. The primary goal is Futhark-like source: one composed array program, many lowered kernels.

## PR 10: Source-Level Frame Composition

PR 10 is the first real consolidation PR. The goal is to make the Wyn source read
like one renderer-level array program, even while the compiler and driver still
lower and execute today's physical entries.

Target source shape:

```wyn
def render_frame(frame, events, world, history, domains) frame_outputs =
  let world' = update_world(frame, events, world, domains.points, domains.items)
  let scene = build_scene(world, domains.ground)
  let visibility = build_visibility(frame, world, history.occ_depth,
                                    domains.setts, domains.walls)
  let shadow = build_shadow(frame, visibility)
  let gbuffer = rasterize_scene(frame, scene, visibility)
  let ao = ambient_occlusion(frame, gbuffer.depth)
  let lit = shade(frame, world, gbuffer, ao, shadow)
  in { world = world', scene = scene, visibility = visibility,
       shadow = shadow, gbuffer = gbuffer, ao = ao, lit = lit }
```

Because current Wyn entries are still the backend boundary, PR 10 should land in
small commits:

1. Add explicit frame-level domain/resource/output records and document which
   fields are logical-only versus physical ABI.
2. Route `step` through a state/scene frame slice, preserving the existing output
   tuple exactly.
3. Route visibility entries through a named frame visibility slice, preserving
   dense non-compacted output until filter compaction is safe again.
4. Route lighting through a named frame lighting slice, preserving the existing
   per-pixel `light` output.
5. If the compiler permits it, add a canonical `render_frame` composition that
   names the whole frame dataflow. If it does not, leave a focused compiler note
   describing the first blocker.

Descriptor/graph cleanup to pair with PR 10:

- A graph edge only exists when producer and consumer use the same resource
  identity. The current driver compensates with binding-name aliases in Rust.
- Compute tuple outputs need stable resource names, the buffer analogue of
  `#[target(name)]`, so `cull` can write `sett_inst` instead of anonymous
  `cull_output_0`.
- Vertex and fragment stages of one draw should be modeled as one logical draw
  pass, or the descriptor should emit an explicit vertex-to-fragment edge.
- Previous-frame reads such as `cull` reading `occ_depth` must be tagged as
  temporal/history reads. Otherwise the correct graph contains the intended
  feedback loop `cull -> scene -> occ_reduce -> cull`.

PR 10 acceptance criteria:

- The source has one visible frame-level composition layer.
- Existing physical entry names and descriptor shape are preserved unless a commit
  explicitly changes them.
- `cargo check` passes after each incremental commit.
- Any compiler blocker is recorded with the smallest failing source shape.

## Documentation Updates

Update docs after each milestone.

`COMPILER-NOTES.md` is partly stale: it still lists serial filter scan as open, while the descriptor shows sharded scan phases. Keep it honest as refactors remove old workarounds.

Documentation updates should happen with the PR that makes the relevant workaround obsolete, not as a disconnected cleanup.

## Recommended Order

1. Image entry wrappers plus shared `pixel_xy` helpers.
2. Domain records/accessors, no ABI changes.
3. `step` decomposition and geometry record/tuple stream.
4. `cull`/`walls` candidate/filter/draw-args extraction.
5. Split entries into pass-owned Wyn files.
6. Descriptor-driven Rust graph consumption.
7. Logical different-domain frame functions.
8. Descriptor logical entries vs physical stages.
9. Collapse host-visible scheduling around the logical frame graph.
10. Physical entry consolidation as an optimization.

## Commit Hygiene

- Keep the stashed water/quay geometry experiment out of these PRs unless explicitly revived.
- Do not stage untracked screenshots, `.spv`, `.spvasm`, `.json`, or `repro/` artifacts.
- Before committing a PR slice, run `git status --short`.
- For implementation PRs, run `cargo check` from `driver/`.
- For renderer-affecting PRs, capture a smoke frame with `cargo run -- --frames 5` when the local environment supports it.
