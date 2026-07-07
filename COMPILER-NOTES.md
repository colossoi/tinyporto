# Wyn compiler — workarounds & known issues

Running log of where tiny porto bends around the Wyn compiler: **standing
workarounds** currently in the code, **open issues** worth fixing, and a history of
what's already been fixed (with minimal reproducers under `repro/`). Since we own
the compiler, the "open" items are really a fix-list.

The diagnostic loop we use: `wyn compile` a minimal `repro/*.wyn`, disassemble with
`spirv-dis`, and (because `wyn compile` succeeding is **not** the same as the SPIR-V
passing wgpu/naga) run it through the driver to catch validation errors.

---

## Standing workarounds (currently in the code)

1. **Image-writing compute entries need a dummy buffer output + iota domain.**
   `image_store` is only per-invocation inside a `map`, and a `map` needs an array
   to range over, so an image-only entry can't just return `()`. We feed an iota
   input (`otile`/`pxl`) and return a throwaway `[]u32` (`… in 0u32`) purely to
   carry the dispatch domain. See `occ_reduce`, `walls`, `light` in `wyn/main.wyn`.
   *Ideal:* a compute entry that dispatches over an explicit grid and writes only
   images, no carrier buffer.

2. **A `loop` inside a `map` must not reference values bound outside the `map`.**
   Doing so panics the compiler: *"FuncParam/BlockParam NodeId(..) should have been
   pre-populated in elaborated map."* Workaround: compute those values **inside** the
   map body. In `light` we compute `w`/`h` from `iResolution` inside the lambda
   instead of once above it.

3. **A `def` may take a `storage_image`/`texture2d` parameter.** The compiler inlines
   the helper or specializes it per call-site, binding its image ops to the concrete
   resource global (the image never travels as a value parameter), so the logical
   model's "image ops reference a global" holds. Image-touching helpers are ordinary
   defs — `contact_shadow` / `sun_shadow_pcf` in `shadow.wyn`, `gtao.denoise` (a
   `texture2d`). The occlusion test still inlined in both `cull` and `walls` could
   likewise collapse to one shared def in `hiz.wyn`.

4. **Read-write storage textures are r32-only** (WebGPU limit, not a compiler bug,
   but it shapes the design). An image written in one entry and read in another via
   *storage* becomes a read-write module global — fine for `occ_depth` (`r32float`),
   impossible for `rgba*`. So `lit` is **write + sample** (`storage_write` +
   `sampled`, read back with `texture_load`) rather than read-write storage.

---

## Open compiler issues (fix-list)

- **`filter`'s scan stage is serial** — emitted as `dispatch_size: fixed 1×1×1`, a
  single-thread prefix sum over the whole input. It dominates frame time (~85%).
  A workgroup-parallel / work-efficient scan is the biggest perf lever. Repro:
  `repro/r5_filter_count.wyn` (inspect the `*_filter_scan` stage).
- **Loop-in-map capturing an outer binding panics** — see workaround #2. The
  elaborator should handle a `loop` closing over the map's enclosing scope.

---

## Fixed this session (kept as regression repros)

- **`filter` was serial, now shards** into flags/scan/scatter. `repro/r4_filter.wyn`,
  `repro/r5_filter_count.wyn`.
- **`filter` output element size** was taken from the input element, wrong when a
  `map` widens the element before the `filter` (e.g. `u32` → `vec4f32`). Now uses
  the output element size. `repro/r6_map_filter_len.wyn`, `repro/r7_filter_widens_elem.wyn`.
- **`image_store`/`image_load` inside a `map`** were emitted against an
  `OpFunctionParameter` instead of the global (naga: *"Not a global variable"*).
  Fixed by inlining the map body. `repro/t5_image_store_in_map.wyn` (straight-line),
  `repro/t6_image_in_map_with_loop.wyn` (with a `loop`).
- **Image shared read+write across entries** was over-decorated `NonWritable` from
  the first (reader) entry, breaking the writer (naga: *"used incorrectly as
  WRITE"*). Fixed to union the access. `repro/t7_rw_image_nonwritable.wyn`.

---

## Rendering limitations (not compiler bugs — deferred work)

- **Sun shadow map casts from the camera-culled brick list.** The `sun_shadow`
  pass rasterizes `wall_brick_inst`, which `walls` already frustum + occlusion
  culled to the *camera*. So a building fully off-screen (or Hi-Z-culled) won't
  cast into the view. Fine while all casters are on-screen; the correct fix is a
  separate light-frustum caster cull feeding its own instance list. The shadow map
  is also window-sized (reuses the shared depth buffer) rather than a dedicated
  square hi-res target — a driver upgrade (fixed-size render target + its own depth
  attachment) would sharpen it.

## Driver-side accommodations (not compiler bugs)

- **The window is a fixed, non-resizable physical size** (`main.rs`, `resumed`).
  The graph bakes window-sized compute grids (`pxl`/`otile`/occ) as compile-time
  constants and the window-sized images/bind-groups aren't rebuilt on resize, so
  the surface must equal the graph size. We request a **physical** size (not
  logical — that inflates the surface on HiDPI, leaving the bottom rows unlit) and
  disable resizing. *Deferred:* true dynamic resize (recompute window-relative
  buffer sizes + rebuild the affected bind groups on every `Resized`).
- **Storage-texture limit raised to the adapter max** (`gfx.rs`): the deferred
  `light` pass binds 5 storage textures (G-buffer albedo/normal, scene depth, sun
  shadow map, lit output), past WebGPU's default limit of 4.
- **Storage-image layout access is widened to the resource's graph-wide union**
  (`image_union_access` in `driver/src/main.rs`): the compiler emits one module
  global per image with the union access, so every pipeline's layout must match.
- **Per-view storage-image binding dedup** in `driver/build.rs` codegen: the
  descriptor lists a storage-image resource once per view kind, so the same
  `(set, binding)` can appear twice.
