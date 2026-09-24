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
cargo run -- --screenshot scene.png    # headless demo scene
cargo run -- --gi off                   # compare with the previous ambient model
cargo run -- --gi indirect              # indirect light only
cargo run -- --gi reference             # slower explicit path-traced comparison
cargo run -- --no-textures              # compare original flat materials
cargo run -- --sun-direction=-0.5,0.52,0.35 # direction toward the sun
cargo run --release -- --sun-demo clear --screenshot sun.png
cargo run --release -- --sun-demo clouded --screenshot sun-clouded.png
cargo run --release -- --sun-demo away --screenshot sky-away.png
```

The sky uses the same sun direction as geometric shadows, contact shadows and
surface lighting. A near-white disk (1.6 degrees across, three times natural size), limb darkening
and two cheap angular glow terms give the sun its appearance; there is no lens
flare, atmospheric ray marching, lookup texture or additional rendering pass.
The sky is paler toward the sun and subtly indigo away. Sky and clouds are linear
HDR and tone mapped once; GI and water sample the directional atmosphere without
the disk because they evaluate direct sunlight separately.

The default sun is 40.4 degrees high, above the interactive camera's view.
`--sun-demo` requires `--screenshot` and places an inspection camera above the
scene, aimed from the actual `--sun-direction` (or the default). `clear` and
`away` disable visible clouds for a matching-exposure comparison; `clouded`
offsets the existing cloud field to a thin edge at the default sun. These cloud
overrides affect only the visible sky in the inspection shot. The ordinary
camera, sun direction and cloud layout are unchanged. At 1280x800 the disk is
about 63 pixels across. See [sun rendering notes](docs/sun.md).

`build.rs` invokes `wyn build --graphics -O --target-double rust-wgpu --target
spirv`, producing `main.rs` and `main.spv` in Cargo's `OUT_DIR`. Rust includes the
compiler's module directly; that module embeds its sibling SPIR-V. Source,
package, and compiler changes trigger rebuilding. There is no runtime compiler
invocation or shader-file loading.

Set `WYN` to select a compiler executable. Alternatively, set
`WYN_PRECOMPILED_DIR` to a directory containing a matching `main.rs` / `main.spv`
pair generated with the command above. Precompiled builds do not require the
compiler or the sibling package checkout.

The title displays wall-clock FPS. Screenshots report median CPU encode/submit
time and, on adapters supporting timestamp queries, total GPU frame time after
five warmup frames. Screenshot wall time includes a GPU wait each frame, so it
does not represent interactive FPS, where CPU and GPU work can overlap.
Per-pass GPU timing is unavailable because the generated passes expose no
timestamp hooks. The driver records clears and the generated frame into one
command encoder, then submits it once. Interactive frames do no timing queries.
A persistent generated `HostContext` creates the shader and compute
pipelines once, caches render pipelines on first use, and reuses scratch buffers.
Windowed startup prepares these pipelines on a worker while the loading window
keeps processing events. Preparation records and discards a frame without
advancing the world or consuming input. Backend selection follows WGPU and
`WGPU_BACKEND`; there is no platform-specific override.
Returned world buffers remain separate allocations so the previous frame's
inputs survive while the next frame is produced. Frame time includes generated
command setup.

The compiler's runtime dispatch regression is fixed in the version installed
2026-09-22 at 13:01. Static inspection confirms GTAO and coarse-depth workgroup
counts now scale with the caller-provided output buffers: at 1280x800, they
launch 16,000 and 250 workgroups respectively, instead of one each.
[`rust_host_runtime_dispatch.wyn`](repro/rust_host_runtime_dispatch.wyn) reproduces
the original failure and documents the corrected launch. This fix has not been
re-benchmarked here.

Hold the left mouse button to paint fence strokes or place building footprints.
Tab switches between those two tools; L toggles the overlay. The starting scene
has a canal through a grid of one-metre terrain cells, each containing land,
water, or one dividing line. Ashlar coping and bank faces meet water centred
0.6 m below the land, with small displaced waves, opaque seawater, coarse scene reflections,
sun glints and wet masonry. Interactive canal drawing is not implemented
yet. See [cell terrain and bank geometry](docs/terrain.md) and
[water rendering](docs/water.md). Sun shadows use a retained geometric grid;
see [shadow casting](docs/shadows.md).
Right drag orbits, middle drag pans, and the wheel zooms. Resizing preserves the
painted world and recreates screen-sized targets and lighting history.

Diffuse GI is on by default. It follows the Tiny Glade talk's screen-space
ray marching with a software BVH fallback, sparse hemisphere sampling, SH
reconstruction, radiance feedback, and an AO-guided recurrent denoiser. Hold Alt
to compare with the previous ambient model; hold Ctrl to suppress final AO.
The separate reference mode uses explicit paths without SH or spatial filtering.
See [GI design, comparison modes, and limitations](docs/gi.md).

Brick, cobble, quoin, and mortar surfaces use CC0 color, normal, and roughness
maps by default. [`assets/textures`](assets/textures/README.md) documents sources,
checksums, mipmaps, and mapping. `driver/src/materials.rs` embeds and uploads the
maps once; `wyn/material.wyn` evaluates them at each rounded-box surface hit.
`--no-textures` restores flat colors/geometric normals for comparison.

## Host integration

`driver/src/app.rs` creates `generated::HostContext` once per renderer and passes
it to `generated::encode_tinyporto_frame` once per frame, using the installed
compiler's command-encoding API. The context survives resizing; the generated
host replaces scratch allocations when their sizes change. The frame entry
determines compute/draw ordering and indirect commands. A separate geometry
entry exports masonry only when the terrain changes; the driver builds and
retains its sun-shadow index. Changing the sun reindexes those cached boxes.
There is no JSON descriptor parser, custom Rust code generator, binding-name
alias table, shader loader, or generic frame-graph executor in tinyporto.

The shell packs frame inputs using the generated `RESOURCE_NAMES` and
`BUFFER_FIELDS` metadata. It supplies cleared color/depth targets, initial world
buffers, and outputs whose capacities are caller-provided: one `vec4f32`
per pixel for ambient occlusion, one `f32` per 8x8 tile for coarse occlusion,
and one 112-byte GI ray sample per 4x4 tile. The compiler owns the GI wall BVH.
Ground, the canal bed, props and the water mesh share their depth attachment.
Sun visibility reads the geometric index. Opaque lighting resolves directly into the final
image, then the water mesh shades over it with opaque body colour, interpolated
wave normals, filtered scene reflections and sun glints. Each surface is tone mapped once.

The G-buffer uses 12 bytes per pixel: `RGBA8Unorm` albedo/roughness, `RG16Float`
octahedral world normals, and `R32Float` window depth. Geometry encodes normals
after interpolation and material evaluation; lighting and GI decode point-sampled
values. Albedo alpha is zero for sky or `0.5 + 0.5 * roughness` for a surface,
preserving the validity test without another attachment. World position
is reconstructed from depth. The separate `Depth32Float` attachment adds another
4 bytes per pixel for hardware depth testing; it is not yet reused for sampling.
Water uses shared height/normal grids (about 2 MiB) and a planar reflection
capture capped at 128 pixels on its longest edge. Its colour and depth cost
120 KiB at the default viewport. There are no additional screen-sized water
targets. The terrain representation is unchanged.

The generated output descriptor's first six buffers are retained as the next
frame's UI, points, items, stroke head, occlusion and GI inputs. The two
caller-owned occlusion buffers alternate. GI receives a distinct output
allocation, so neither history aliases the current output.

The compiler's draw/consumer ordering regression is fixed in the version
verified on 2026-09-22. GPU readback of all 1,000 coarse-depth tiles at 320x200
matches the reduction of the final scene depth, including props. The regression
case [`rust_host_draw_consumer_order.wyn`](repro/rust_host_draw_consumer_order.wyn)
preserves the original failure, expected values, and passing control.

`--screenshot scene.png --dump head,items,occ` reads exposed buffers for debugging.
Available names are `uistate`, `points`, `items`, `head`, `occ`, `gi`, `events`, `frame`,
`terrain`, and `ao_work`. Compiler-internal scratch and prop buffers are owned by the
generated function and are not exposed by the source result.

## Checks

```sh
cd driver
cargo test
cargo run -- --width 320 --height 200 --screenshot scene.png
```

The GPU integration tests exercise generated frame execution, retained state,
event clearing, odd viewport dimensions, GI transport, temporal invalidation,
and agreement with the explicit path reference. WGPU's `WGPU_BACKEND`
environment variable can select a backend, for example `vulkan`.

## Wyn module idiom

Local files use bare imports. Shared packages expose named modules such as
`gfx`, `curves`, and `packing`, called through qualified names. Library modules
keep scalar operations qualified (`f32.sin`, `f32.clamp`, etc.); the application
root uses `open f32`. Vector operations such as `normalize`, `dot`, `cross`,
`distance`, `reflect`, `mix`, and `vec.*` are available without that open.
