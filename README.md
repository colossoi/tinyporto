# tiny porto

A sandbox/diorama game (1950s Venice) in the spirit of *Tiny Glade*, with a
GPU-driven renderer and indirect prop draws.

The simulation and rendering live in `wyn/main.wyn` and its imported modules.
Wyn generates the Rust/WGPU host that executes the complete frame. `driver/`
provides the window, input events, orbit camera, initial state, and render targets.

## Build & run

Requires a current `wyn` compiler on `PATH` and a sibling `../wyn` checkout for
the local packages declared in `wyn.toml` (curves, gfx, GTAO, noise, packing, rng).

```sh
cd driver
cargo run                              # opens a window
cargo run -- --frames 5                # render five window frames, then exit
cargo run -- --fps 144                 # override the default 60 Hz cap
cargo run -- --fps -                   # run uncapped
cargo run -- --screenshot scene.png    # headless scripted water stroke
```

`build.rs` invokes `wyn build --graphics -O --target-double rust-wgpu --target
spirv`, producing `main.rs` and `main.spv` in Cargo's `OUT_DIR`. Rust includes the
compiler's module directly; that module embeds its sibling SPIR-V. Source,
package, and compiler changes trigger rebuilding. There is no runtime compiler
invocation or shader-file loading.

Set `WYN` to select a compiler executable. Alternatively, set
`WYN_PRECOMPILED_DIR` to a directory containing a matching `main.rs` / `main.spv`
pair generated with the command above. Precompiled builds do not require the
compiler or the sibling package checkout.

The title displays wall-clock FPS. Per-pass GPU timing is unavailable because
the generated host currently owns command submission and exposes no timestamp
hooks. A persistent generated `HostContext` creates the shader and compute
pipelines once, caches render pipelines on first use, and reuses scratch buffers.
Returned world buffers remain separate allocations so the previous frame's
inputs survive while the next frame is produced. Frame time still includes
generated command setup and scalar readbacks.

The compiler's runtime dispatch regression is fixed in the version installed
2026-09-22 at 13:01. Static inspection confirms GTAO and coarse-depth workgroup
counts now scale with the caller-provided output buffers: at 1280x800, they
launch 16,000 and 250 workgroups respectively, instead of one each.
[`rust_host_runtime_dispatch.wyn`](repro/rust_host_runtime_dispatch.wyn) reproduces
the original failure and documents the corrected launch. This fix has not been
re-benchmarked here.

Hold the left mouse button to paint. Tab cycles tools; L toggles the overlay.
Right drag orbits, middle drag pans, and the wheel zooms. Resizing preserves the
painted world and recreates screen-sized targets and occlusion history.

## Host integration

`driver/src/app.rs` creates `generated::HostContext` once per renderer and passes
it to `generated::host_tinyporto_frame` once per frame. The context survives
resizing; the generated host replaces scratch allocations when their sizes
change. The single Wyn source entry determines compute/draw ordering and
indirect commands.
There is no JSON descriptor parser, custom Rust code generator, binding-name
alias table, shader loader, or generic frame-graph executor in tinyporto.

The shell packs frame inputs using the generated `RESOURCE_NAMES` and
`BUFFER_FIELDS` metadata. It supplies cleared color/depth targets, initial world
buffers, and the two outputs whose capacities are caller-provided: one `vec4f32`
per pixel for ambient occlusion and one `f32` per 8x8 tile for coarse occlusion.
Ground and props share their depth attachment; shadows use a separate one.

The generated output descriptor's first five buffers are retained as the next
frame's UI, points, items, stroke head, and occlusion inputs. The two caller-owned
occlusion buffers alternate so history never aliases the current output.

The compiler's draw/consumer ordering regression is fixed in the version
verified on 2026-09-22. GPU readback of all 1,000 coarse-depth tiles at 320x200
matches the reduction of the final scene depth, including props. The regression
case [`rust_host_draw_consumer_order.wyn`](repro/rust_host_draw_consumer_order.wyn)
preserves the original failure, expected values, and passing control.

`--screenshot scene.png --dump head,items,occ` reads exposed buffers for debugging.
Available names are `uistate`, `points`, `items`, `head`, `occ`, `events`, `frame`,
and `ao_work`. Compiler-internal scratch and prop buffers are owned by the
generated function and are not exposed by the source result.

## Checks

```sh
cd driver
cargo test
cargo run -- --width 320 --height 200 --screenshot scene.png
```

The GPU integration test exercises generated frame execution, retained state,
event clearing, and resizing to odd viewport dimensions. WGPU's `WGPU_BACKEND`
environment variable can select a backend, for example `vulkan`.

## Wyn module idiom

Local files use bare imports. Shared packages expose named modules such as
`gfx`, `curves`, and `packing`, called through qualified names. Library modules
keep scalar operations qualified (`f32.sin`, `f32.clamp`, etc.); the application
root uses `open f32`. Vector operations such as `normalize`, `dot`, `cross`,
`distance`, `reflect`, `mix`, and `vec.*` are available without that open.
