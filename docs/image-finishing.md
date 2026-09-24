# Image finishing

The frame now composites opaque lighting and water in linear HDR, performs
final-image TAA, then applies the existing
ACES fit before the sRGB display attachment. Tony McMapface is deferred.

## Boundaries

- `pkg/taa`: stateless reconstruction, depth agreement, history clipping and
  blending. It receives sample values and explicit jitter/reactivity.
- `driver/src/temporal.rs`: two retained HDR/depth images, projection jitter,
  camera-cut detection, validity and buffer swapping. `prepare` does not
  advance this state; only a submitted frame does.
- `wyn/temporal.wyn`: application texture reads and camera/depth reprojection.
- `wyn/main.wyn`: final full-screen ACES display pass, reading TAA's result.

The TAA package lives in this repository. Existing `wyn/gfx` camera helpers are
reused; no changes to the Wyn package repository are required.

## Temporal reconstruction

TAA is enabled by default; `--no-taa` disables it and projection jitter. A
16-position Halton sequence displaces samples by less than half a pixel.
Geometry projection, AO view reconstruction and depth-derived rays use the same jitter. GI uses the
current and previous jitter with its own independent history. Reference GI
automatically disables TAA/jitter to preserve its fixed-pixel comparison.
Mouse picking and planar reflection capture remain unjittered.

The resolve reconstructs the current 3x3 neighborhood with a Mitchell filter.
Depth is also reconstructed onto the stable grid using a local reciprocal-depth
plane and the less discontinuous one-sided derivatives. The same center surface
supplies motion and expected previous depth. Catmull-Rom history reconstruction
avoids the cumulative softening from repeated bilinear sampling during motion.

For moving pixels, the bilinear history footprint is checked for bounds,
sky/opaque/water class and depth agreement (0.04 world units plus 0.3% of
expected depth). Sub-0.05-pixel motion retains history across jitter-induced
silhouette coverage changes; color clipping still responds to scene changes.
YCoCg ray-box clipping uses wider variance bounds when still and tighter bounds
in motion. Current-color weight runs from 2.5% to 12% with motion, with 30%
reactivity for water. Luminance-compressed blend weights prevent isolated bright
samples from dominating. Brightness changes alone no longer increase reactivity.

History is invalid after resize, camera cuts, scene/sun rebuilds, GI mode or
material changes, modifier changes and sky-setting changes. Ordinary camera
motion reprojects history. Paint edits use neighborhood rejection as their
colors change; they do not need a CPU readback or reset GI's separate history.

The compositor and two history images use RGBA32Float: linear RGB plus positive
view depth for opaque surfaces, negative depth for reactive water, zero for
sky. This convention belongs to the adapter. Three images cost 46.875 MiB at
1280x800. They are allocated on initialization/resize, not per frame.

## Display

The display pass reads the resolved HDR color, applies the existing ACES fit,
and writes opaque color to the sRGB attachment. Depth of field has been removed
to simplify the renderer and reduce rendering work: no lens package, prefilter,
gather, blur composite, intermediate images, or lens controls remain.
This removes two passes and three half-resolution RGBA32F images (11.72 MiB at
1280x800). The earlier [research](taa-dof-research.md) and
[profile](frame-profile.md) remain as historical references.

## Checks

The [2026-09-24 research](taa-dof-research.md) documents the original regressions.
The correction keeps TAA enabled by default. Quality checks now compare with
TAA disabled and an unjittered 4x-per-axis reference, not just jittered input.

- At 320x224, after warmup, stationary opaque display variation averages
  0.140/255; the 99th percentile is 1/255.
- Linear HDR mean squared error against the 16-sample-per-pixel reference is
  0.001949 with TAA versus 0.008240 disabled when stationary, and 0.006330 versus
  0.008245 during the tested slow orbit. This measures detail as well as stability.
- With realtime GI and animated water, stationary opaque display variation
  averages 0.258/255. The test also verifies water continues to animate.
- Existing tests cover history lifetime, camera cuts, picking and resize.
  Geometry/lighting tests disable TAA
  for their exact-sample assertions.

Run GPU checks with `cargo test --manifest-path driver/Cargo.toml --release`.
Set `TINYPORTO_QA_DIR` to save the quality tests' still/moving comparison images.
All 22 release tests passed on Vulkan after DoF removal, along with SPIR-V
validation and Rust formatting checks. The generated frame has 23 passes.
These measurements cover the repository's test scenes; they are not a claim
that all temporal artifacts are eliminated.
