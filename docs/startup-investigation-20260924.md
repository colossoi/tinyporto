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

Remaining startup work, if desired: bake processed material maps and mipmaps
ahead of launch, then address repeated shader expressions and separate the
reference-lighting pipeline. Removing DirectX does not eliminate Vulkan's
material-preparation cost, since Vulkan was already the default in these runs.
