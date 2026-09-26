# Rendering experiment gates

Each functional group is reviewed separately. The user's performance limit is
**no more than 5% slower than its matched baseline**. Do not begin the next
group until the current appearance and performance gates are accepted.

For each group:

1. Preserve the baseline executable and fixed-camera captures.
2. Implement only the current group and check its relevant invariants.
3. Compare the same resolution, camera, lighting, scene, GI and TAA settings.
   Record complete-frame GPU times and measured rendering throughput separately.
   Startup, shader compilation and PNG writing are outside steady-state timing.
4. Provide wide, close, sun-facing and high-resolution comparisons, plus a live
   candidate for the human appearance check. Review motion as well as stills.
5. Pause for the user's appearance decision. A numerical pass cannot override a
   rejected appearance. Revise and remeasure, or revert the group.

FPS passes when candidate FPS is at least 95% of baseline. GPU frame time has a
separate, conservative limit of at most 105% of baseline. Compare paired repeats;
incomplete or interrupted runs cannot establish a pass.

## Current group 1 status: spectral candidate v3

The current source is the spectral candidate described in the
[September 26 research/status handoff](tidewater-thread-status-20260926.md).
It ports Tidewater's two shortest spectral bands and their dispersive motion,
with explicit canal attenuation and different GPU storage/dispatch choices.

- **Appearance: awaiting human review.** Previous rejections apply to v1/v2;
  they do not constitute a decision on v3.
- **FPS performance: not cleared.** Completed window pairs showed 5.13% and
  2.48% lower FPS in the wide view, and 14.15% lower FPS in one 1080p pair.
  The second 1080p pair was interrupted. Baseline variability also needs control.
- **GPU diagnostics:** mean paired medians increased 7.47%, 7.48%, 7.63%, and
  5.15% for wide, close, sun-facing, and close-1080 views, respectively.
- **Correctness:** ordinary release build, four relevant tests, and a separate
  direct-DFT numerical comparison passed on September 25.
- **Review UI:** local `review.html` still shows v2. Use the explicitly named
  `candidate-v3-*` captures until the page is updated.

The handoff contains exact paired results, source-fidelity details, limitations,
the compiler reproducer, and next actions. Group 2 has not started.

## Historical group 1 record: first and second candidates

Baseline: `32ec4b8aa7765d0cf1e5a293875667a4c4db1497`.
Review files and frozen benchmark sources: `tmp/tidewater-gate-01/`.
Hardware: Radeon RX 580, Vulkan, AMD driver 23.11.1.

Adds shading-only ripple slopes, mip-filtered slope energy and a GGX sun lobe.
This group does not add water transparency. Group 2 (gust/slick variation) has
not started.

- First candidate: **appearance rejected**; user reported periodic diamonds.
  Its sparse crossing-wave texture is retained in the rejected screenshots and
  executable for comparison. Earlier timings apply only to this version.
- Revised candidate: **appearance still fails; needs revision**. The user
  reports that the ripple frequencies still look very regular. Uses a filtered
  random height field in place of the sparse wave sum, doubles the texture
  dimensions and enlarges both repeat areas. Retains the mesh's interpolated
  geometric normals. A flat-normal diagnostic was temporary only.
- Performance gate: **not cleared**. The paired offscreen FPS results include
  regressions beyond 5%, and repeat instability prevents attributing them
  confidently to the shading change. The unchanged 1080p baseline ranged from
  14.66 to 8.62 offscreen FPS while its fixed-time GPU medians were 32.44 and
  31.56 ms. Do not claim this is the game's displayed FPS or claim a pass from
  the GPU timings alone.

The first windowed FPS trial completed; later repetitions were interrupted and
cannot establish a pass. The revised candidate has two alternating-order
offscreen repetitions per view, each with 120 warmup and 600 animated frames.
That loop includes CPU submission and waits for each GPU frame to finish; it
excludes startup, display presentation, timestamp readback and PNG writing.
Compiler processes were active during some measurements. A stable interactive
FPS comparison is still required before accepting this group.

Fixed-time GPU measurements use 60 warmup frames and 120 measured frames per
view. The probe contains the same shader as the corresponding release build;
instrumentation is outside production source. Tests cover texture mip energy,
water mesh/contact repeatability, canal depth, and animated water with GI/TAA.
The final review executable was rebuilt using `WYN_PRECOMPILED_DIR` pointed at
the frozen candidate probe, so its shader matches the measured revision byte
for byte. The ordinary source build succeeded when that snapshot was created;
a later redundant compiler run was cancelled. The environment override was
removed after creating the review executable.

Review artifacts in `tmp/tidewater-gate-01/`:

- `review.html`: slider, full-image toggles and native-resolution inspection.
- `review.ps1`: live `baseline` / `candidate`, with `wide`, `close`, `glint` views.
- `measurements.json`: all revised paired FPS and GPU results, including failures.
- `measure-v2.ps1`, `prepare-probe.mjs`, `add-throughput.mjs`: measurement setup.
- `candidate-v2-*.png`, `baseline-v2-*.png`, `rejected-*.png`: matched captures.

The local review server is `node tmp/tidewater-gate-01/serve-review.mjs`, bound
only to `127.0.0.1:8765`. The HTML also opens directly as a local file.

### Source fidelity correction

The first two candidates were independently designed approximations, not ports
of Tidewater's ripple generation. The user challenged this distinction after
both candidates displayed regular patterns. Do not present them as reproducing
Tidewater's surface statistics or animation.

At the pinned upstream revision, `OceanFFT.js` builds four 256x256 spectral
cascades with JONSWAP/TMA energy, directional spreading, Gaussian random
coefficients, and frequency-dependent dispersion. `WaterSurface.js` combines
their derivatives and re-samples the finest evolving cascade for near-field
ripples, with footprint-dependent fading. `SeaDetail.js` separately modulates
short-wave strength with gusts and slicks. `WaterMaterial.js` estimates unresolved
variance analytically from wind and footprint; our second-moment mip scheme is
also an independent substitution.

The second candidate instead differentiates a single Gaussian-blurred random height
field, samples it at two fixed scales, and translates those samples rigidly.
The fixed blur radius selects a characteristic feature size despite randomized
positions. It does not reproduce the reference spectrum or dispersive motion.
A source-faithful replacement should preserve those mechanisms and explicitly
document canal-scale parameter changes and omitted systems before another
appearance review. Adding more arbitrary noise is not an established fix.

## Subsequent groups

The research and proposed sequence are in
[the Tidewater rendering review](tidewater-rendering-review.md). Each accepted
group becomes the baseline for the next. The remaining sequence can change
based on the user's appearance judgments.
