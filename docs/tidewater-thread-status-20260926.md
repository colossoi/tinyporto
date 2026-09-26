# Tidewater rendering research and water experiment: WIP handoff

Written September 26, 2026; experiments and measurements below were performed
September 25. This note records the state of the Tidewater research task before
saving it on `codex/tidewater-water-wip`.

**Current status: group 1 is experimental and unapproved. The appearance gate
is pending for the spectral candidate, and the performance gate is not cleared.
No subsequent experiment group has started.** Saving or pushing this WIP is
not acceptance of its appearance or performance.

## Objective and user decisions

The user asked us to inspect Tidewater's impressive sky/water rendering and
high-resolution performance for techniques usable in Tiny Porto. The discussion
then focused on how its nearly ray-traced-looking water and transparency work.
The user decided transparent water has no current application here and asked
us to proceed with other experiments.

Every functional group must have a human appearance gate and a performance
gate. The user's limit is **no more than 5% lower FPS than the matched baseline**,
meaning candidate FPS must be at least 95% of baseline. We also track a
conservative GPU-time check of candidate time at most 105% of baseline; that is
an additional diagnostic, not a substitute for measured FPS. A successful test
or numerical comparison cannot approve appearance on the user's behalf.

The original Tiny Porto baseline is
`32ec4b8aa7765d0cf1e5a293875667a4c4db1497`.
Tidewater research and translated code are pinned to
[`4811ba48d795197de5621985f404e765c0b7c0ef`](https://github.com/dgreenheck/tidewater/tree/4811ba48d795197de5621985f404e765c0b7c0ef/src).
The detailed source review is [tidewater-rendering-review.md](tidewater-rendering-review.md).

## Research findings

### Why Tidewater can look expensive without tracing everything

- Its active renderer is custom WebGPU/WGSL. `SkyProClouds` is the default cloud
  implementation; older cloud code and compatibility exports are not the active
  path. Rendered canvas dimensions, internal render scale, and display pixel
  density must be distinguished when comparing resolution or FPS.
- Atmosphere integration produces small reusable transmittance, scattering,
  and sky-view lookup textures. Visible sky, reflected sky, and diffuse
  illumination can share representations rather than each repeating the
  expensive integration.
- Default volumetric clouds trace sparse fresh samples into a temporal history.
  At native internal scale, their sampling pattern traces approximately 1/64
  as many fresh rays as output pixels. Reconstruction, shadows, panorama updates,
  and final composition still cost work.
- A directional cloud panorama, cloud-shadow map, and filtered environment
  lighting are updated incrementally. The water's ordinary reflected sky uses
  atmosphere plus the cloud panorama; it does not simply sample the material
  GGX environment cube.
- Water separates mesh displacement from fine shading derivatives. Four
  256x256 FFT cascades cover 733, 157, 33.3, and 7.1 m repeat areas. Different
  frequencies evolve at different speeds. Mip/anisotropic filtering and extra
  close-range samples handle detail below mesh resolution.
- Tidewater uses two workgroup-memory FFT dispatches plus four mip-generation
  dispatches for its normal displacement/derivative update. The header's
  two-dispatch claim describes the transform, not the complete update.
- Filtered/unresolved wave slopes broaden GGX sun highlights. Gusts and slicks
  modulate short-wave strength over the world. Temporal reconstruction and
  tone mapping contribute materially to the final appearance.

These are source findings, not a reproduced same-hardware Tidewater benchmark.
Its published performance target and Tiny Porto's RX 580 measurements cannot
establish a direct speed comparison.

### Transparency discussion

The almost ray-traced appearance is a combination of techniques: Fresnel
reflection, reflected atmosphere/clouds, conditional screen-space object
reflections, scene-depth-dependent water absorption and scattering, and
refraction using opaque scene data plus a separate half-resolution underwater
capture. The source review does not establish hardware ray tracing as the
mechanism behind this appearance. Tiny Porto's current candidate retains an
opaque water body and its existing planar scene reflection.

### Proposed functional groups

1. Fine water slopes and filtered GGX sun highlights (the current WIP).
2. Restrained gust/slick variation scaled to the canals.
3. A richer lit cloud panorama shared by sky, water, and GI.
4. Cached atmosphere lookup textures with consistent exposure and lighting.
5. Sparse volumetric clouds with their own temporal history.
6. Optional internal render scaling and temporal upscaling.

Only group 1 has been implemented. The later priorities may change after user
review. New sky physics would improve appearance at a cost; it is not assumed
to be faster than Tiny Porto's existing analytic sky.

## Experiment history and correction

| Candidate | Mechanism | Appearance decision |
| --- | --- | --- |
| First | Sparse crossing waves baked into a slope texture | Rejected: periodic diamond shapes |
| Second (v2) | Slopes from a Gaussian-filtered random height field, sampled at rotated scales and translated over time | Rejected: ripple frequency still looked too regular |
| Current (v3) | Tidewater's spectral initialization and dispersive evolution for its two shortest bands | Awaiting human review |

The user challenged whether the first two attempts actually reproduced
Tidewater. They did not: they were independent approximations. That distinction
was acknowledged. Increasing texture size or adding randomized positions did
not make the fixed blur scale or rigidly translated animation equivalent to
Tidewater's wave spectrum. The current code replaces those approximations.

Earlier candidate timings and screenshots must not be presented as measurements
or approval of the current spectral implementation.

## Current implementation and source fidelity

The implementation ports the wave mathematics while retaining Tiny Porto's
existing mesh and rendering architecture. It does not reproduce all of
Tidewater's water renderer or its GPU scheduling strategy.

| Area | Current behavior |
| --- | --- |
| Initialization | `driver/src/water_spectrum.rs` builds JONSWAP/TMA energy, directional spreading, seeded Gaussian coefficients, conjugate partners, wave frequencies, and FFT twiddles once |
| Bands | Original 33.3 m and 7.1 m bands, both 256x256; original seed 1337 and original cascade indices 2/3 |
| Parameters | Upstream default local wind/swell systems and 500 m dispersion depth; this depth preserves the reference model and is not a canal bathymetry model |
| CPU precision | Initialization evaluates the translated formulas in f64 and uploads f32; it is not claimed to be bit-identical to upstream WGSL initialization |
| Evolution | `wyn/water_spectrum.wyn` evolves complex coefficients and computes two packed complex inverse transforms for surface slopes and horizontal derivatives |
| Derivatives | Original sign correction, 0.9 choppiness, and slope correction by horizontal compression |
| Canal attenuation | Explicit 0.35 amplitude applied to retained derivatives and extra close detail; unresolved variance is multiplied by 0.35 squared |
| Close detail | Finest band re-sampled at 7.3x and 3.1x, rotated 0.63 and 2.14 radians, weighted 0.55 and 0.35, with footprint fade between 0.01 and 0.04 m |
| Normals and light | Reoriented fine normal over the original interpolated mesh normal; GGX sun, minimum roughness 0.035, and analytic wind/footprint unresolved variance |
| Filtering | Nine box-filtered mip levels packed into a 2.67 MiB f32 buffer; manual repeat, bilinear, and trilinear filtering using an approximate projected pixel footprint |
| Geometry and body | Existing shared height grid, moving masonry contact, opaque body color, planar capture, shadow filtering, and reactive TAA remain |

Important deviations and omissions:

- The two longest ocean bands are omitted. FFT displacement is not applied to
  the mesh, whose original waves still determine heights and waterline contact.
- Upstream foam history, wakes, shoreline simulation, gust/slick modulation,
  refraction, transmission, and screen-space reflections are not added.
- The original implementation uses texture arrays, reduced-precision storage,
  and hardware filtering (including anisotropy for ordinary band samples).
  The WIP uses f32 storage buffers and explicit interpolation.
- Tiny Porto retains its existing reflection/body lighting and a sun-highlight
  clamp of 5; this is not a complete WaterMaterial port.
- The CPU owns initial input data only. Per-frame evolution, FFT, mip creation,
  and shader composition remain authored in Wyn. No direct WGSL implementation
  or custom Rust dispatch scheduler was added.

The upstream MIT notice is in [licenses/tidewater-MIT.txt](../licenses/tidewater-MIT.txt).
See [water.md](water.md) for the renderer's current design.

## Validation completed

The ordinary optimized release build succeeded with the installed Wyn compiler
on September 25. Four relevant tests passed:

- `water_spectrum::tests::bands_are_hermitian_and_use_dispersive_frequencies`
- `app::tests::water_mesh_is_repeatable_and_moves_its_contact`
- `app::tests::taa_stays_stable_with_realtime_gi_and_animated_water`
- `app::tests::canal_has_real_bed_and_ashlar_and_water_only_animates_below_land`

A separate local GPU harness compared the FFT result with a direct f64 DFT of
the uploaded f32 inputs at times 0 and 1.37. All 131,072 base-level texels were
finite at both times. Eighty selected component comparisons had a maximum
absolute error of **0.000000138**, below the 0.00002 tolerance. This validates
the transform arithmetic against those inputs; it does not prove that the
entire shaded image matches Tidewater or that appearance is acceptable.

Re-run the production checks with:

```powershell
cargo build --offline --release --manifest-path driver/Cargo.toml
cargo test --offline --release --manifest-path driver/Cargo.toml water -- --nocapture --test-threads=1
```

The GPU tests require access to the local Vulkan adapter. The DFT harness and
its generated shader are local evidence under `tmp/tidewater-gate-01/spectral/`,
not a checked-in automated test suite.

## Performance evidence

Hardware: Radeon RX 580, Vulkan, AMD driver 23.11.1. Baseline source at the
commit above was rebuilt with the same installed compiler used for v3.
Compiler SHA-256:
`49936AB8401AE00BC97724928F6CFC611B886A475D769B4BB76A35D734D9C0EE`.
The measured candidate shader matched the ordinary production shader:
`1994C778E060A9EF09CBFA3938EA44DC64D5C28A93BC9ACF49266FB501074E1B` (SHA-256).

### Complete-frame GPU time

Each run used a fixed scene time of 1, 60 warmup frames, and 120 measured frames.
Startup, PNG writing, and timestamp readback are outside the GPU interval.
Baseline/candidate order was reversed for repeat 2. These are whole-frame GPU
medians, not water-only timings and not displayed FPS.

| View | Baseline run 1 / 2 (ms) | Candidate run 1 / 2 (ms) | Mean of baseline medians | Mean of candidate medians | Increase |
| --- | --- | --- | ---: | ---: | ---: |
| Wide, 1280x800 | 15.2683 / 15.1984 | 16.3776 / 16.3637 | 15.2334 | 16.3707 | 7.47% |
| Close, 1280x800 | 18.1448 / 18.2334 | 19.6259 / 19.4733 | 18.1891 | 19.5496 | 7.48% |
| Sun-facing, 1280x800 | 17.6584 / 17.4864 | 18.9538 / 18.8717 | 17.5724 | 18.9128 | 7.63% |
| Close, 1920x1080 | 31.3056 / 30.7403 | 32.5608 / 32.6781 | 31.0230 | 32.6195 | 5.15% |

Camera parameters: wide uses the default camera; close uses distance 19 and
elevation -0.3; sun-facing uses distance 19, elevation -0.7, and azimuth 2.1815.
The 1080p view uses the close camera.

### Actual uncapped window FPS

The temporary probe uses `AutoNoVsync`, 120 warmup frames, and 360 measured
frames. It fixes the camera and advances animation at the same frame-indexed
times in both variants. Runs request 480 total frames and close automatically.
Reported physical client dimensions matched the requested sizes.

| View / repeat | Baseline FPS | Candidate FPS | Candidate decrease |
| --- | ---: | ---: | ---: |
| Wide, repeat 1 | 61.72343 | 58.55578 | 5.13% |
| Wide, repeat 2 | 55.70044 | 54.31973 | 2.48% |
| Close 1080p, repeat 1 | 28.94791 | 24.85207 | 14.15% |
| Close 1080p, repeat 2 | Incomplete | Incomplete | No paired result |

The second 1080p pair was interrupted. The baseline's own wide-view FPS varied
substantially between repetitions, so the 14.15% result must not be attributed
entirely to the FFT without further measurement. The evidence does not clear
the user's 5% limit. Do not cherry-pick the passing wide repeat or infer an
FPS pass from GPU timing alone.

Earlier v2 offscreen-throughput measurements were also unstable and included
CPU submission plus a wait after every frame; they were not displayed FPS.
They are historical artifacts, not the acceptance data above.

## Slowdown: facts versus hypotheses

Measured generated-code changes: complete-frame compute passes increased from
**15 to 43**, and buffer-clear commands from **11 to 39**. The wave update adds
28 compute passes: coefficient evolution/reordering, 16 butterfly stages,
column reordering, sign/choppiness correction, eight mip-generation stages,
and packing. The generated implementation materializes intermediate results
in ordinary GPU buffers.

The leading hypothesis is extra global-memory traffic, dispatch overhead, and
synchronization compared with Tidewater's two FFT dispatches using workgroup
memory. Each butterfly stage reads and writes large intermediate arrays.
Additional scratch clears appear avoidable where a subsequent kernel fully
overwrites its output, but this needs checking before removal. Manual fragment
filtering adds buffer loads and arithmetic that upstream delegates to texture
sampling hardware. CPU command encoding, submission, and presentation may
also contribute to the gap between the GPU-time and window-FPS results.

The fairly fixed 1.1-1.6 ms added GPU cost across the views is consistent with
screen-resolution-independent wave computation. This is supporting evidence,
not per-pass attribution: **no isolated per-pass GPU timings have been taken
for the current spectral implementation**. CPU spectrum initialization occurs
once and is outside the measured steady-state interval.

## Compiler issue and workaround

Passing the original record of computed mip arrays through nested sampling
helpers into the fragment shader failed in Wyn's egglog-to-SSA lowering. The
current implementation packs the levels into one buffer to avoid that failure.
The issue is more specific than merely capturing several arrays in a record.

The standalone [reproducer](../repro/water_mip_fragment_capture.wyn) needs no
imports or FFT code. It failed both with and without `-O`:

```text
TODO: first-class value Global(SymbolId(270))
```

Both controls compiled with the same installed compiler:

- [Inline control](../repro/water_mip_fragment_capture.inline-control.wyn):
  substitute one sampling helper's body at its two call sites.
- [Compute control](../repro/water_mip_fragment_capture.compute-control.wyn):
  use the identical helpers from a compute map instead of a fragment shader.

[Diagnostic and commands](../repro/water_mip_fragment_capture.md) include the
compiler fingerprint. The compiler's internal cause is not established.
Helper inlining is a promising smaller workaround, but has not been tested in
the full renderer. A compiler update must be checked against both the reduced
case and production shaders before removing the packing workaround.

## Local artifacts and reproducibility limits

The WIP commit includes the rendering sources, MIT notice, research, gate/status
notes, and standalone compiler reproducer with controls. Large temporary
executables, generated shaders, captured images, downloaded upstream sources,
and debug experiments remain local. A fresh clone will not contain those
`tmp/` artifacts; the numerical tables in this note preserve their findings.

Useful local paths under `tmp/tidewater-gate-01/`:

- `candidate-v3-*.png` and `baseline-v3-*.png`: four matched static views.
- `*-v3-gpu-*.log` and `*-v3-fps-*.log`: raw measurements above.
- `candidate-probe/`: frozen v3 shader and instrumented driver.
- `baseline-v3-probe/`: baseline rebuilt with the matching compiler.
- `spectral/proof-run.log`, `spectral/tests.log`, and `spectral/driver-build.log`:
  numerical proof, regression results, and ordinary release build.
- `spectral/measure.ps1`, `spectral/instrument-window.mjs`, and
  `prepare-probe.mjs`: temporary measurement setup.
- `review.ps1`: launches the current candidate or matching baseline for live
  inspection. `review.html` and its metrics still describe **v2**, so the page
  at `http://127.0.0.1:8765/` must be updated before claiming it reviews v3.
- `spectral/rejected-v2.exe`, old images, and old measurement files: historical
  rejected-candidate evidence.

The downloaded upstream reference is in `tmp/tidewater-review/` locally; the
pinned GitHub links in the source review remain the portable reference.

## Next actions

1. Profile coefficient evolution/FFT, mip generation/packing, clears, and water
   shading separately, as well as CPU encoding/submission. Measure rather than
   assuming which suspected overhead dominates.
2. Recheck the compiler reproducer after any compiler update. Test helper
   inlining in the full renderer as a possible way to remove packing without
   changing the wave spectrum or quality.
3. Investigate fully overwritten scratch clears, FFT stage fusion/workgroup
   memory, and hardware-filterable derivative storage while preserving the
   project's Wyn-owned computation and dispatch architecture.
4. Repeat matched, alternating-order GPU and actual-window FPS measurements
   after changes, with interference controlled and all interrupted runs marked.
5. Update the review page to v3/current captures and measurements, then obtain
   the user's explicit appearance judgment, including motion, periodicity,
   distant shimmer, sun trails, and water/masonry contact.
6. Clear both gates before starting gusts/slicks or any sky experiment.

Do not call this WIP a faithful port of Tidewater's complete renderer, a
performance success, or an appearance-approved replacement.
