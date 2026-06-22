# tiny porto

A sandbox/diorama game (1950s Venice) in the spirit of *Tiny Glade* — fully
GPU-driven / indirect-draw, structured like Tiny Glade's "Nine" renderer.

**The whole application is written in [Wyn](#the-wyn-program); the Rust crate is
a generic GPU host that knows nothing about the game.**

## Layout

```
wyn/        the application — all shaders AND game/sim logic (Wyn -> SPIR-V)
  main.wyn    root: entry points (vertex/fragment, later compute)
  camera.wyn  orbit camera + ray gen          } library modules
  shade.wyn   sky / lighting / tonemap / cursor }
  math.wyn    shared scalar/vector helpers      }
driver/     generic wgpu host (NO domain types — no Ground/Building/etc.)
  src/graph.rs  generic frame-graph schema (mirrors the wyn descriptor)
  src/app.rs    the tiny-porto graph as plain `const` data
  src/wync.rs   `wyn compile` + load SPIR-V into wgpu
  src/gfx.rs    wgpu context
  src/main.rs   clap args + winit loop + the generic executor
shaders/    build artifacts: *.spv + *.json descriptor (gitignored)
```

## Build & run

Requires the `wyn` compiler on `PATH`.

```sh
cd driver
cargo run                 # compiles wyn/main.wyn -> SPIR-V, then opens a window
cargo run -- --no-compile # use existing shaders/*.spv
cargo run -- --frames 5   # render N frames then exit (headless smoke test)
```

You should see a sand ground under a tilted-diorama orbit camera with a field of
ochre flat-roofed building boxes — the boxes are drawn via `draw_indirect`, with
their instance data and the draw-args (instance count) produced GPU-side by the
`gen` compute pass (no CPU readback).

## Architecture notes

- **Backend: SPIR-V.** `wyn compile` emits `<name>.spv` + a `<name>.json`
  pipeline descriptor in one step. The driver loads the `.spv` via wgpu's
  `ShaderSource::SpirV` (naga frontend → cross-backend). SPIR-V `OpEntryPoint`
  names equal the Wyn source entry names (no mangling).
- **The driver never reads the descriptor at runtime.** The frame-graph is the
  `const GRAPH` in `src/app.rs`, compiled into the binary. A future `build.rs`
  will diff that graph against the emitted `.json` and fail on drift
  (`driver/build.rs` is a stub today).
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

See the project plan for milestones (M0 + M1 done; M2 ground grid + Voronoi +
water painter next).
