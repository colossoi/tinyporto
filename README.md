# tiny porto

A sandbox/diorama game (1950s Venice) in the spirit of *Tiny Glade* — fully
GPU-driven / indirect-draw, structured like Tiny Glade's "Nine" renderer.

**The whole application is written in [Wyn](#the-wyn-program); the Rust crate is
a generic GPU host that knows nothing about the game.**

## Layout

- **`wyn/`** — the application: all shaders *and* all game/sim logic, compiled to
  SPIR-V. This is where the work happens.
- **`driver/`** — a generic wgpu host that runs the compiled pipelines. No game
  concepts live here (no Ground/Building/etc.); all meaning is in `wyn/`.

## Build & run

Requires the `wyn` compiler on `PATH`.

```sh
cd driver
cargo run                 # build.rs compiles+embeds the Wyn shaders, then opens a window
cargo run -- --frames 5   # render N frames then exit (headless smoke test)
```

Shaders are compiled at **build time** (`build.rs` → `wyn compile`) and embedded
into the binary via `include_bytes!`, so the driver does no shader I/O at runtime
and never shells out to `wyn`. Editing any `wyn/*.wyn` triggers a rebuild.

You should see a sand ground (a 1 m Voronoi grid) with ochre flat-roofed
buildings under a tilted-diorama orbit camera, and a cyan brush ring tracking the
mouse. **Hold the left mouse button** to paint water — canals follow the Voronoi
cell boundaries and persist. Buildings are drawn via `draw_indirect`; the painted
grid state lives in a ping-pong storage buffer updated each frame by a compute
pass (no CPU readback).

## Architecture notes

- **Backend: SPIR-V.** `wyn compile` emits `<name>.spv` + a `<name>.json`
  pipeline descriptor in one step. `build.rs` runs it and embeds the `.spv`; the
  driver loads it via wgpu's `ShaderSource::SpirV` (naga frontend → cross-backend).
  SPIR-V `OpEntryPoint` names equal the Wyn source entry names (no mangling).
- **The driver never reads the descriptor at runtime.** The frame-graph is the
  `const GRAPH` in `src/app.rs`, compiled into the binary. The `.json` descriptor
  is a build-time artifact only; a future `build.rs` step will diff `GRAPH`
  against it and fail on drift (that validation is the remaining stub).
- **Generic host.** `graph.rs`/the executor have no game concepts; `app.rs`
  names resources but is still just generic graph data. All meaning lives in
  `wyn/`.

## Wyn module idiom (current)

Until the compiler supports qualified imports (`module m = import "x"`):

- **Library modules** (`math`/`camera`/`shade`): no `open`; qualify scalar math
  as `f32.sin`, `f32.clamp`, … (an `open f32` re-exports and currently collides
  with the importer's `open f32`).
- **Root** (`main.wyn`): `open f32` + bare `import "math"` / `"camera"` /
  `"shade"`; call exports unqualified. Export names are globally unique.
- Globals needing no `open`: `normalize`, `dot`, `cross`, `distance`, `reflect`,
  `mix` (scalar+vec), the `**` operator, and the `vec.*` module.

See the project plan for milestones (M0–M2 done; M3 building plunker next).
