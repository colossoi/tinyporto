# Diffuse GI

The design follows sections 4.1-4.3 of
[the local rendering talk notes](../rendering-tiny-glade.md), checked against
the transcript at 22:00-23:50, 28:30-31:00, and 58:24-59:03. The talk explicitly
separates an explicit path-traced reference from its shipping final-gather GI.

## Pipeline

1. Build a software BVH from the existing wall footprints. Land and the canal bed
   are intersected analytically using the terrain cells; a grid walk intersects
   bank faces, and raised coping has a separate surface test. These are
   secondary-ray proxies for the rendered scene; they do
   not create or alter visible geometry. They remain available off screen.
2. Produce current linear screen radiance from current materials and shadowed
   sunlight, plus reprojected previous indirect lighting. This is the feedback
   that propagates additional diffuse bounces across frames.
3. Trace one hemisphere ray per 4x4 pixel tile. March the full-resolution depth
   buffer first, with point and bilinear depth, refinement, and a thickness test.
   On failure, traverse the world-space BVH. Both paths sample screen radiance
   when the hit is visible and its position/normal agree with the G-buffer.
   Misses receive the procedural sky. Primary sample positions cycle through
   the sixteen pixels of the tile.
4. Project incoming radiance into four SH coefficients per color channel
   (SH2: the constant and linear bands). Reconstruct at half resolution using
   eight neighboring trace samples, guided by surface geometry and GTAO.
5. Reproject and validate history against world position, normal and material.
   Use a fresh ray only when both spatial reconstruction and history fail.
   Accumulate up to 32 frames, then apply a small recurrent SH blur. Young
   histories use a wider kernel; stable histories use a tighter one. Retain the
   **filtered** result, so this is a recurrent denoiser, not a final RGB blur.
   Realtime history and spatial filters measure separation in the local projected
   footprint, with stretch capped at 20x and separate surface checks retained.
6. Cross-bilaterally reconstruct at full resolution and evaluate using
   hallucinated ZH3. Apply subtle additional AO (0.2), add direct light, then
   compose water and tone map. Tonemapped pixels never enter the feedback path.

The source of the depth test is the talk author's
[published marcher](https://gist.github.com/h3r2tic/9c8356bdaefbe80b1a22ae0aaee192db).
It tests against the farther of point/bilinear depths and measures penetration
against the nearer depth. That conservative test differs from the average-depth
wording in the local notes. The implementation here uses metric view depth and
our existing forward-Z projection. Rays are transformed to homogeneous clip
coordinates once and interpolated during marching and refinement. Point depth
reuses the nearest of the four samples already loaded for bilinear depth.

Directional reconstruction uses the shared-luminance-axis, curve-fit model from
[ZH3: Quadratic Zonal Harmonics](https://www.ppsloan.org/publications/ZH3.pdf),
section 3.4, equation 21. Filtering follows the recurrent-blur structure described
in the Tiny Glade talk, not the complete NVIDIA ReBLUR implementation.

## Source layout

Library files use explicit named modules, imported with bare `import "gi"`
and called as `gi.resolve`, `ray.box`, etc. This works with the installed
compiler without repeatedly prefixing definitions inside each namespace.

| File / namespace | Responsibility |
| --- | --- |
| `gi.wyn` / `gi` | Radiance feedback, final gather, reference integration, history and filtering |
| `scene_trace.wyn` / `scene_trace` | Wall BVH, ground/material policy and screen/world fallback |
| `screen_trace.wyn` / `screen_trace` | G-buffer depth sampling, ray marching, refinement and thickness tests |
| `ray.wyn` / `ray` | Plane, box and rounded-box intersections |
| `hemisphere.wyn` / `hemisphere` | Uniform pairs and hemisphere direction mapping |
| `sh.wyn` / `sh` | SH projection weights and ZH3 evaluation |
| `camera.wyn` | Camera rays, projection and depth reconstruction |

The primary prop renderer shares `ray.rounded_box`; ground picking and scene
tracing share `ray.plane_y`. Rounded-box tracing
keeps its exterior-only distance contract and caller-supplied step budget.
Contact shadows retain their existing march and acceptance rules in
`shadow.wyn`; consolidating them with the screen tracer would change behavior.
The Rust host's CPU camera picking remains separate from the GPU Wyn helpers.

## Comparison modes

From `driver/`:

```sh
cargo run -- --gi on
cargo run -- --gi off
cargo run -- --gi indirect
cargo run -- --gi reference --width 640 --height 480 --screenshot reference.png
```

GI is enabled by default. Hold Alt for the previous ambient-lighting model;
hold Ctrl to suppress the final AO contribution. `indirect` hides direct sun.
`reference` traces eight cosine-weighted paths per full-resolution pixel per
frame, with three segments, without SH, feedback, spatial filtering, or extra
AO. It progressively averages up to 4,096 frames. Screenshots warm up for 40
frames, giving the reference 320 paths per pixel. This is a comparison
integrator over the hybrid scene representation, **not ground truth**.

## Integration and current limits

All passes, their dependencies, tracing and filtering live in Wyn. The Rust
shell supplies camera history, frame index, capacities, mode and invalidation.
G-buffer normals use octahedral encoding in an `RG16Float` attachment. All GI
surface reads decode them to unit world normals before tracing, lighting, or
history validation. GI history still stores Cartesian normals plus mean albedo;
its layout and filtering are unchanged. GTAO reconstructs normals from depth.
Textured props write palette-relative color variation and shading normals into
the same G-buffer. Roughness shares albedo alpha while preserving the existing
validity threshold. Direct GGX highlights are added only in final lighting;
they do not enter diffuse radiance feedback. Off-screen proxies keep their
coarse palette colors and geometric normals. `--no-textures` restores the
original material evaluation and invalidates GI history when toggled.
Opaque lighting resolves to the final image before the water mesh is drawn.
Water shading and the animated wet band remain outside diffuse radiance
feedback; see [water rendering](water.md). The screen tracer and world BVH remain
part of GI; water no longer uses them.
`build_gi` groups the array dependencies into one generated compute graph;
separate top-level maps currently cause Wyn to expose disconnected producer
outputs as new inputs. Reconstructing loaded records at branch joins also
avoids a current Wyn record/tuple phi mismatch. Neither workaround changes the
lighting algorithm or patches generated code.

The first six frame outputs retain UI, points, items, stroke head, coarse
occlusion and GI. Later outputs expose BVH nodes, linear radiance, sparse rays
and temporal data for diagnosis. GI uses seven float4s (112 bytes) per
half-resolution pixel; reference history is full resolution. The ray buffer
uses the same layout at quarter resolution. Current outputs never overwrite
history inputs. Resizing and material-mode changes invalidate history;
reference-mode changes recreate it. Paint input no longer resets realtime GI.
Committed paint points/items start a 16-frame tracking window in the retained
stroke head (slot 10 of the existing 12-float buffer). GI reads that head with
the rendered world, so the edit marker arrives one frame after capture, exactly
when the edit becomes visible. No CPU readback is needed. Hover, motion that emits
no control point, and building drags do not restart the window.

Realtime GI retains its normal 32-frame cap during painting. Existing surface
position, normal and material checks reject mismatched history locally. A
global edit no longer increases fresh-sample weight or widens the recurrent
filter on untouched surfaces. Indirect lighting affected by nearby paint can
take longer to converge with the normal cap, and the revised visual result
still needs validation. Reference mode discards history on the first frame
rendering each edit, then resumes its normal progressive average.

The remaining differences from Tiny Glade are explicit:

- Its wide collision-mesh BVH is represented here by a binary BVH over the
  current axis-aligned wall generators, plus separate cell-terrain intersections.
  Individual bricks
  come from screen-space visibility. No collision meshes, roofs, or dynamic
  3D building generator exist in this scene yet.
- An off-screen proxy hit gets its material and direct sun; indirect feedback
  is available for visible hits. Consequently, hidden multi-bounce transport
  remains approximate. The reference shares the visibility representation but
  explicitly traces subsequent bounces.
- Failed gap filling traces a fresh sample instead of reproducing the talk's
  deliberate read/write race. There is no same-dispatch history aliasing.
- Sparse sampling and SH projection still lose some fine lighting detail and
  can leave noise during motion. The denoiser is an implementation of the
  described structure, not a claim of reproducing proprietary filter tuning.

## Validation

The Vulkan GPU integration test checks screen hits, BVH hits and sky misses;
warm color bounce and sky occlusion inside the brick rooms; history age and
non-aliasing; camera reprojection; mode switches; and odd-size resizing. Added
paint regressions cover mature history on untouched surfaces, edit timing,
no-op pointer motion, and reference-mode invalidation. These pass in the release
test suite. The existing comparison checks
realtime indirect lighting after 40 frames against
512-path-per-pixel reference lighting at the same primary pixels. The measured
relative RGB RMS error in that 80x60 scene is about 0.159 with surface textures;
this is one regression
scene, not a general accuracy estimate.

The generated SPIR-V is validated with `spirv-val`. Realtime, indirect-only and
reference screenshots were inspected at 640x480. Validation used Vulkan; the
water integration also checks responsive window startup and screenshot rendering.

The installed release compiler on `PATH` now provides `encode_tinyporto_frame`.
The combined changes pass with `WYN` and `WYN_PRECOMPILED_DIR` unset, using these
commands from `driver/`:

```powershell
$env:WGPU_BACKEND = 'vulkan'
cargo test --offline
cargo run --offline
```
