# Geometric sun shadows

The normal renderer uses the existing building and quay box geometry for sun
visibility. `wyn/shadow.wyn` performs analytic ray/box intersections.
`driver/src/shadow.rs` builds a conservative spatial index over those boxes.

## Data and lookup

All sun rays are parallel. A plane perpendicular to the sun direction is divided
into 512x512 cells. Projecting a receiver onto that plane selects one cell;
its list contains every box whose projected outline overlaps it. Each candidate
is tested until one blocks the ray. Cell lookup is O(1); intersection work is
O(k) for k candidates, with early exit. Lists have no fixed cap.

The GPU stores one packed `vec4f32` buffer:

| Records | Contents |
| --- | --- |
| 0 | Reference-array base, box-array base, grid side, inverse cell width |
| 1–2 | Projection basis vectors and lower bounds |
| 3 | Normalized sun direction |
| 4 onward | One offset/count record per cell |
| Reference array | Four box IDs per record |
| Box array | Four records per box: centre and three prepared slab equations |

Indices use exactly representable integer-valued floats. Construction rejects a
buffer exceeding that range. The default projection spans 60 m; bounds expand
to include outlying casters. Projected convex hulls are conservatively binned
with a small margin for floating-point rounding. The CPU uses the box's dual
basis to account for rounding in its orientation axes. Parallel slabs use
containment checks; vertical and horizontal sun directions are supported.

The buffer is about 4.43 MiB for the demo, additional to the visible scene data.
Cell records account for 4 MiB; an 8-byte cell layout would halve that part.
Grid dimensions affect candidate counts and memory; box intersections determine
the geometric boundary.

## Rebuilding

`shadow_geometry` in `wyn/main.wyn` exports the same `masonry_prop` placement
used for rendering. At startup or after `Renderer::set_terrain_cells`, the host
reads that export, builds the index on the CPU, and uploads a new retained
buffer. Changing the sun through `set_sun_direction` reuses the cached boxes and
rebuilds their projection and slab coefficients. Both edits invalidate lighting
history. Building definitions are currently static and regenerate on app reload.

Camera movement, window resizing, material changes and water animation reuse
the grid. The geometry export/readback and CPU construction run only on edits,
before frame timing starts. The console reports each rebuild's duration and
storage size. There is no interactive terrain or sun editing UI yet; a startup
sun direction can be supplied with `--sun-direction=-0.5,0.52,0.35`.

## Coverage and other lighting

Opaque and water shading average four visibility queries across the pixel's
local surface plane. This estimates pixel-area coverage in quarter steps.
Low-resolution reflection lighting and on-screen GI sunlight use one point
query. The existing screen-space contact shadows still provide local cobble
detail; off-screen GI sunlight retains its existing world-geometry trace.

Visible masonry has rounded edges while the caster boxes have sharp edges.
A 6 mm ray-start tolerance accommodates the small bevels. Exact pixel-area
coverage and physically soft sunlight are separate, unimplemented features.

## Checks

`cargo test --manifest-path driver/Cargo.toml -- --test-threads=1` exercises:

- GPU visibility against f64 all-caster ray clipping for 16,000 queries across
  four sun directions, including parallel slab cases.
- Shadows at both canal ends, grid retention across camera/time/material/resize
  changes, and invalidation after sun and terrain edits.
- Coping shadows on cobbles, avoiding self-shadow on the cap, and the existing
  water, materials, input, terrain and GI regressions.
- Empty grids, invalid sun vectors, expanded bounds, and 160 overlapping casters
  in a cell without truncation.

## Integration measurements

Measured 2026-09-24 on a Radeon RX 580 using Vulkan, the default debug Cargo
build, 1280x800, fixed time 1, azimuth 0.6 and elevation -0.35. Each screenshot
renders 40 frames and excludes the first five from timing. These are whole-frame
GPU medians, including masonry, water, materials, AO and GI:

| Camera distance | GPU frame time |
| --- | --- |
| 45 m | 14.9 ms |
| 15 m | 20.6 ms |

The demo index contains 3,068 boxes and 63,238 references, with a maximum of
33 candidates per cell and 4.43 MiB of GPU storage. Initial geometry export,
readback, CPU construction and upload took 132–134 ms in these runs. That work
is excluded from ordinary frame timing and occurs only on initialization/edits.

The close shadow-edge inspection (distance 6, elevation -1.1, azimuth 0, GI and
AO off) measured 13.6 ms. A vertical-sun capture also rendered successfully;
its index has a maximum of 80 candidates per cell. Captures and logs are under
`tmp/shadow-integration/`. The full 16-test suite passed; the subsequent CLI
argument regression passed separately, giving 17 passing tests in total.
