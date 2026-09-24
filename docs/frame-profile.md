# Frame profile — 2026-09-24

**Historical:** this profile predates DoF removal. The current renderer has
no DoF prefilter or gather passes, and its display pass only tone maps.
The timings below describe the earlier implementation.

Measured after the TAA/DoF correction and the deeper-focus lens tuning
(`aperture_pixels = 8 * aperture * height / 800`).

## Configuration and method

- Radeon RX 580, AMD Vulkan driver 23.11.1; GPU timestamp period 40 ns.
- 1280x800, default demo camera (distance 45, elevation -0.35, azimuth 0.6),
  fixed scene time 1, realtime GI, textures and TAA enabled, no painting.
- Compare DoF disabled with tilt-shift at aperture 1 and automatic focus.
- Temporary timestamps bracketed all 25 generated compute/render passes.
  Shader bytecode was unchanged by instrumentation. A separate pair brackets
  the generated encoder's full GPU workload, including its clears/transitions.
- Each configuration ran twice for 40 frames. The profiler reads the previous
  frame on the next call, yielding frames 0–38. Discarding frames 0–9 leaves
  **58 samples per configuration**. The table reports medians of per-frame
  category sums, rounded to 0.01 ms. GI below combines its five stages.
- Instrumentation and adapter logging were temporary. The ordinary release
  executable was rebuilt without them, and its total time was checked separately.

## GPU breakdown

| Work | DoF off (ms) | Tilt-shift on (ms) |
|---|---:|---:|
| Prop visibility and draw compaction | 3.61 | 3.59 |
| Global illumination | ~4.25 | ~4.25 |
| Opaque lighting and composition | 2.46 | 2.46 |
| Final-image TAA | 1.23 | 1.23 |
| Ground and masonry G-buffer | 1.17 | 1.17 |
| Ambient occlusion and coarse depth | 1.09 | 1.09 |
| Water simulation, reflections and shading | 0.82 | 0.82 |
| World state and geometry preparation | 0.09 | 0.09 |
| DoF prefilter | 0.02 | 0.44 |
| DoF bokeh gather | 0.04 | 1.93 |
| Final display, including DoF composition when enabled | **0.09** | **1.27** |
| Other GPU commands/transitions between measured passes | ~1.01 | ~0.93 |
| **Total generated frame** | **15.91** | **19.29** |

Individual medians need not sum exactly to the median frame total. The residual
is measured as total minus the sum of pass durations in each frame; it is not
attributed to a specific shader. Driver attachment clears outside the generated
encoder are included only in the separate full-frame measurement below.

The final display pass with DoF off measured **0.09056 ms median**, with a
0.08832–0.09728 ms range. It tone-maps resolved HDR and writes the sRGB display
attachment. TAA is separate. Two bypassed DoF passes still execute and write
their intermediate targets, costing another **0.059 ms combined**; they do not
run the blur loops. Thus the three finishing passes total about 0.15 ms off,
versus 3.64 ms with tilt-shift. Their incremental DoF cost is about **3.49 ms**.

### GI stages with tilt-shift enabled

| Stage | GPU ms |
|---|---:|
| Trace geometry/BVH preparation | 0.009 |
| Screen-space radiance preparation | 1.445 |
| Quarter-resolution ray tracing | 0.501 |
| Temporal reconstruction | 0.755 |
| Recurrent spatial filter | 1.542 |

GI filtering/preparation costs substantially more than ray tracing in this
view. Prop visibility/compaction is the largest individual pass and is a clear
candidate for a separate optimization investigation.

## Uninstrumented release checks

Normal release runs outside the execution sandbox, with only the existing
whole-frame timestamps, measured:

| Metric | DoF off | Tilt-shift on |
|---|---:|---:|
| GPU frame median | 15.5 ms | 19.2 ms |
| CPU encode/submit median | 1.8 ms | 3.1 ms |
| Synchronous headless frame median | 18.2 ms | 23.6 ms |

CPU and GPU work can overlap in windowed rendering, so their medians should not
be added to estimate interactive FPS. Headless timings include waiting for each
frame. CPU submission and residual GPU times vary more than individual shader
timings. Initial pipeline creation, PNG encoding and the roughly 19 ms initial
sun-grid rebuild are excluded from the steady-frame figures.

These are measurements of this camera, scene, resolution and GPU, not fixed
costs for arbitrary scenes or a prediction for another machine.

## Local artifacts

The reproducible one-off instrumenter, its pass names, raw logs, screenshots
and CSV data are under `tmp/frame-profile/`:

- `instrument.ps1` patches a copy of the generated Rust host, pairing it with
  the original SPIR-V; it does not patch renderer source or the Wyn compiler.
- `tinyporto-profile.exe` is the separate instrumented executable. It is for
  the synchronous screenshot path, not normal windowed use or tests.
- `dof-{on,off}-{1,2}.log` contains raw per-pass ticks.
- `summarize.ps1` uses this adapter's measured 40 ns timestamp period.
- `pass-times.csv`, `group-times.csv`, and `summary.csv` retain the measurements.
- `native-{on,off}.log` records the uninstrumented release checks.

The regular executable under `driver/target/release/` has no per-pass profiling
or per-frame profiling readback. No persistent profiling feature was added.
