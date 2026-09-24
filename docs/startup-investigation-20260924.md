# Startup investigation, September 24, 2026

The measured default backend was Vulkan. Its debug startup delay was dominated
by CPU material preparation, with shader pipeline creation second. DirectX 12
was a separate, much worse case: its temporal GI pipeline was still compiling
when the diagnostic process was stopped after 173 seconds. At the user's
request, DirectX support has now been removed from the application build.

The raw probe, disassemblies, statistics, screenshots and logs linked below are
local investigation artifacts in `tmp/startup-20260924/`; they are not checked
in. This note preserves the measurements and conclusions independently.

## Measurements

The [startup probe](../tmp/startup-20260924/probe.rs) uses the application's
renderer with a copied, instrumented generated host. It prepares a 64×48
headless renderer and records/discards the ordinary frame. It includes
material loading and the shadow-index preparation, but excludes Cargo/Wyn
compilation, window/surface creation, and ordinary rendered frames.

All measurements below precede the Vulkan-only build change. Hardware:
Radeon RX 580; Vulkan driver AMD 23.11.1, DX12 driver 31.0.21905.1001;
WGPU 27.0.1, Naga 27.0.3. No driver cache was cleared. These are individual
process measurements, not controlled cold-cache averages.

| Stage | Optimized Rust / Vulkan | Debug Rust / Vulkan |
|---|---:|---:|
| SPIR-V module ingestion | 0.023 s | 0.186 s |
| All compute pipelines | 2.78 s | 3.18 s |
| GI tracing pipeline | 0.527 s | 0.599 s |
| Temporal/reference GI pipeline | 1.433 s | 1.484 s |
| Complete renderer preparation | 5.094 s | 12.398 s |

GI pipeline rows are subsets of all compute pipelines. Total preparation also
includes render pipelines, assets, device setup, and shadow geometry work.
Logs: [optimized](../tmp/startup-20260924/default-timing.log),
[debug](../tmp/startup-20260924/debug-vulkan-timing.log).

An additional assets-only run isolated `Materials::new` at **7.925 s in debug**
and **0.796 s optimized**. The vehicle took 0.198 s and 0.033 s respectively.
Those separate runs are not additive components of the table's totals.
The material loader decodes PNGs, resamples/processes source maps, and creates
mipmaps during startup. This explains most of the debug/optimized gap.
See [material construction](../driver/src/materials.rs:334),
[texture upload/mipmap generation](../driver/src/materials.rs:178), and
the [debug](../tmp/startup-20260924/debug-assets-timing.log) and
[optimized](../tmp/startup-20260924/release-assets-timing.log) asset logs.

DX12 took 14.054 s for GI tracing and 12.820 s for prop compaction before
reaching temporal GI. Temporal GI did not finish before the process was
stopped at 173.288 s total elapsed. SPIR-V ingestion was still only 0.023 s.
The timed pipeline API includes backend translation, shader compilation, and
native pipeline creation; this does not isolate their individual costs.
See [DX12 log](../tmp/startup-20260924/dx12-timing.log) and
[stop record](../tmp/startup-20260924/dx12-timeout.log).

## Disassembly

The root `main.spv` dates from July and is not the embedded shader. The initial
release snapshot was 728,320 bytes. A newer debug artifact, matching the current
driver interface, was 780,924 bytes and supplied the timing probe.
Both validate with `spirv-val --target-env vulkan1.3`.

The timed [disassembly](../tmp/startup-20260924/latest.spvasm) contains 42,798
instructions, 114 functions, and 37 entry points. Its SHA-256 is
`7C014A0AAB33012DA1CEDC5729EF6F1FA22A32ED88995C1C85E53C8BDE2C8D8D`.
The initial release snapshot has SHA-256
`A9C421120F92EFFE1D933900D3A27F527895F627A9F2AD9CC589BE9482F7CC8C`.
The application was rebuilt again after removing DirectX; these hashes identify
the saved investigation snapshots, not a promise about future Cargo output.

| Entry | Reachable instructions | Reachable loops |
|---|---:|---:|
| `tinyporto_frame_compute_5_compute_4` — temporal/reference GI | 12,245 | 21 |
| `tinyporto_frame_compute_5_compute_3` — GI tracing | 7,700 | 11 |
| `tinyporto_frame_fragment_7` — opaque lighting | 4,656 | 6 |

Counts include each transitively called function once, including labels,
parameters and terminators. They are SPIR-V counts, not GPU machine instructions
or dynamic execution counts. [Complete statistics](../tmp/startup-20260924/latest.spvasm.stats.json).

The earlier integer-clamp expansion was substantially reduced, but redundant
calculations remain. GI tracing contains 52 `Sqrt` instructions with the same
SSA operand; temporal GI contains another 52 with its own common operand.
Opaque lighting repeats one identical `Normalize` 97 times, including five
copies in a single basic block. This is direct evidence of missed reuse, though
later backend compilers may eliminate some repetitions.
[Exact operands and lines](../tmp/startup-20260924/latest.spvasm.duplicates.json).

The slower `--gi reference` comparison mode traces eight samples with up to
three light bounces. It is a runtime branch inside the normal temporal GI
shader, so its code is compiled even when reference mode is off.
See [the branch](../wyn/gi.wyn:237) and [bounce loop](../wyn/gi.wyn:32).
Moving that mode to a separately created pipeline is a plausible reduction
in normal startup compilation; no speedup from that change was measured here.

Generic `spirv-opt -O --preserve-bindings --preserve-spec-constants` was not a
size fix for this snapshot: it increased the binary to 826,880 bytes and 45,169
instructions, largely changing function structure. It validated but was not
installed or performance-tested.

## Applied change and verification

The driver disables WGPU's default features and enables only `std`,
`parking_lot`, `vulkan`, and `spirv`. Its shared instance factory explicitly
selects Vulkan in both windowed and headless modes, after reading other WGPU
environment options. `WGPU_BACKEND=dx12` cannot re-enable DirectX.
The README and Cargo lockfile were updated accordingly.

Both `cargo build --offline` and `cargo build --offline --release` passed.
The [resolved feature graph](../tmp/startup-20260924/vulkan-only-features.txt)
contains Vulkan and no DX12 feature. The rebuilt release executable rendered
40 headless frames successfully with `WGPU_BACKEND=dx12` set, producing a
[scene screenshot](../tmp/startup-20260924/vulkan-only.png) that was visually
checked. [Smoke-test log](../tmp/startup-20260924/vulkan-only-smoke.log).

The next investigation targeted preparing material maps and mipmaps ahead of
launch (implemented below). Repeated shader expressions and the reference-lighting
pipeline remain candidates for further work. Removing DirectX does not eliminate
material-preparation cost, since Vulkan was already the default in these runs.

## Material preparation follow-up

A second, Vulkan-only probe times a copy of `materials.rs` at function/stage
boundaries. It adds no per-pixel timers and leaves production loading unchanged.
The temporary crate and logs live in `tmp/startup-materials-20260924/`; its
generator is `tmp/startup-20260924/prepare-material-probe.mjs`.

| CPU stage | Debug repeat | Optimized repeat |
|---|---:|---:|
| Decode nine PNGs | 5.685 s | 0.532 s |
| Resample color, normal, roughness maps | 2.108 s | 0.179 s |
| Normalize color palette | 0.151 s | 0.008 s |
| Convert/quantize and pack mip bytes | 1.126 s | 0.547 s |
| Generate lower-resolution mips | 0.612 s | 0.065 s |
| Create textures and queue writes | 0.020 s | 0.037 s |
| Complete material initialization | 9.717 s | 1.385 s |
| Separate queue submission/GPU completion wait | 0.006 s | 0.007 s |

Total includes small unclassified allocation/cleanup overhead. Individual runs
varied: the first detailed debug run took 17.219 s and optimized took 1.743 s.
A repeat of the original, coarse probe immediately before these repeats took
9.868 s debug and 1.127 s optimized. Thus the broad result is robust, but these
single-run differences should not be interpreted as precise regressions or
instrumentation overhead. The earlier 7.925/0.796 s asset measurements remain
valid records of their respective runs, not fixed startup durations.

PNG headers confirm eight of nine inputs are 16-bit; the brick normal map is
8-bit. Their compressed source bytes total 61.5 MiB. The loader decodes all of
that, strips to 8-bit, crops/resamples, applies palette normalization, generates
mips, and packs final 8-bit GPU bytes every launch. The full-size brick sources
are decoded even though only eight small face regions are retained.

For a diagnostic control, the instrumented upload function saved the exact byte
slice passed to `queue.write_texture` for every mip of all six final textures.
A subsequent run read those prepared files, recreated the same texture formats,
dimensions and mip counts, and submitted the recorded bytes. This is not a
production asset-cache implementation and does not change the application.

| Prepared-byte control | Debug | Optimized |
|---|---:|---:|
| Read files, create textures, queue bytes | 0.041 s | 0.024 s |
| Separate submit/GPU wait | 0.005 s | 0.004 s |

The six prepared files total 23,768,008 bytes (22.67 MiB), including 376 bytes of
probe metadata. This avoids CPU image conversion without reducing texture
resolution, mip counts, or changing the bytes submitted to the GPU. File reads
were shortly after writing, so filesystem cache was warm. These measurements
are for material initialization alone; they do not claim a 24–41 ms full launch.

This control motivated preparing these deterministic material maps and
mips during the build/asset pipeline, then embed/upload the prepared bytes at
runtime. That should remove most of the observed debug material delay and about
one second from these optimized runs. Shader pipeline creation remains a
separate startup cost. `--no-textures` currently changes shading after
`Renderer::new` has already loaded the maps, so it does not avoid this work.

## Build-time material preparation implemented

`driver/build/materials.rs` now performs the existing PNG decoding, cropping,
resampling, palette normalization, color conversion, and mip generation during
the Cargo build. It emits eight RGBA payloads and their dimensions, formats, and
mip lengths into `OUT_DIR`: the six surface maps plus the two Fiat textures.
The application embeds those prepared bytes and only creates GPU textures and
queues uploads at startup. No runtime asset files or cache are needed. Cargo
tracks all eleven source PNGs and the preparation code; this also runs when
`WYN_PRECOMPILED_DIR` supplies the shader. Build scripts are optimized in both
debug and release profiles so image processing does not use the slow debug path.

A direct comparison of the preserved old probe and a probe using the new
production loader, run sequentially on the same RX 580:

| Initialization | Debug before | Debug after | Release before | Release after |
|---|---:|---:|---:|---:|
| Six surface maps and sampler | 7.526 s | 0.025 s | 0.784 s | 0.020 s |
| Vehicle mesh and two textures | 0.190 s | 0.009 s | 0.031 s | 0.002 s |

These timers include texture creation and queue writes, but exclude device
creation and a separate GPU completion wait, equally for before and after.
They are individual warm-system runs, not cold-boot averages. Logs and the
temporary probe source live in `tmp/startup-bake-20260924/`.

The full new headless startup probe, including device creation, renderer
construction, and `Renderer::prepare`, took 4.622 s debug and 3.804 s release.
It uses a 64x48 target and does not measure window creation or presentation.
Release shader ingestion took 0.023 s; the two heaviest compute pipeline
creations still took 1.605 s (temporal GI) and 0.517 s (GI tracing). Pipeline
creation now accounts for most of the measured startup time. The earlier full
startup timings used a different run/cache state, so the paired asset timings
above are the more direct measure of this change.

Validation:

- All eight prepared payloads, dimensions, formats, and every mip length match
  the old loader exactly. The prepared surface bytes total 23,767,632 bytes;
  the two vehicle payloads add 1,747,620 bytes.
- A 320x200, 40-frame release screenshot is byte-for-byte identical before and
  after (SHA-256 `7BFB5240B9DD44B627CCC29325654D548313FC8F2F092FA9E3F8B134C2C175CA`).
- Debug and release builds pass. The CPU color/roughness mip test and atlas
  isolation test pass, as do the Vulkan material/GI-reset and Fiat texture,
  opaque-window, and shadow integration tests.
- The release executable shrank from 74,550,272 to 35,354,624 bytes because it
  embeds the smaller prepared textures instead of the original source PNGs.
