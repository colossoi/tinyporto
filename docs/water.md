# Harbour water

`wyn/water.wyn` owns the water mesh and shading. `wyn/main.wyn` declares its
draw after opaque lighting. Water sits 0.6 m below land on average.

## Surface and contact

Two Seascape-style wave octaves displace mesh vertices by at most 4.27 cm. Each
octave combines two counter-drifting, noise-warped wave fields. The octave
transform increases spatial frequency by 3.8x and reduces amplitude to 0.22x;
finer octaves are omitted because the mesh cannot resolve them.

Each wet terrain cell contains an 8x8 grid of quads, with vertices 12.5 cm apart.
A shared GPU height grid covers the terrain with a one-sample apron. Height
evaluation skips dry areas outside the shoreline apron. A second compute phase
derives vertex normals from neighbouring heights. Triangles read shared samples,
so adjacent cells meet exactly; interpolated normals give smooth pixel shading.
The GPU compacts wet cells touching the camera frustum into an indirect draw.
Rasterization and depth testing produce the moving contact with masonry.

The water mesh shades into the composited HDR image and reuses the hardware
scene depth attachment after opaque geometry. The original opaque depth image
remains intact for AO and GI. It uses no surface, reflection or refraction ray marches, no world
tracing fallback, and no temporal reflection history.

Masonry darkens in a narrow wet band sampling the same height grid, using the
mesh's triangle interpolation so the band follows the actual geometry. A subtle,
broken contact highlight uses the gap between water and the existing opaque
depth at the same screen pixel. This needs one depth sample and no march.

## Shading

Each visible water fragment normalizes its interpolated vertex normal and
samples a filtered planar reflection. No wave octaves are evaluated per pixel.
Fresnel blends the reflection with the opaque green-blue body colour.
The geometric sun-shadow grid controls the body lighting and sun glints.
Four analytic box-visibility queries estimate shadow coverage across each
pixel's surface footprint. Direct sun glints vanish in full shadow; reflected
sky and sunlit masonry may remain bright. Body colour, reflection, glints and
the contact highlight are combined before final-image TAA and
tone mapping. The TAA adapter marks water as reactive to limit trails from
animated shading; there is no separate temporal reflection history.

The grid covers all building and bank casters and is retained across frames.
Terrain edits regenerate its geometry; sun changes rebuild its projection and
intersection coefficients. Its buffer uses about 4.43 MiB in the demo. See
[shadow casting](shadows.md) for the layout, lookup cost, rebuild lifecycle and
coverage limits.

Fixed camera, scene and time give repeatable water shading. Close views shade
more water fragments; shadow work also depends on the number of candidate
casters at each point. The rest of the renderer retains its GI tracing and
temporal accumulation.

## Coarse reflections

`wyn/water_reflection.wyn` projects the existing building and bank boxes through
the camera mirrored across the mean water level. These boxes use the same
placement definitions as the retained shadow geometry. The reflection pass
clips surfaces below the mean water level, keeps lit and shaded faces, and
applies simple ambient/sun lighting with geometric shadow visibility. Empty
instance slots collapse to empty triangles. Rounded-box tracing, material
textures, AO and GI are omitted from this coarse capture.

The capture is capped at 128 pixels on its longest edge (128x80 for 1280x800).
It contains linear HDR colour in RGBA16Float and has a separate Depth32Float
attachment: 120 KiB at the default viewport size. The sky and its procedural
clouds are rendered into this same small capture, instead of evaluating clouds
again for every water pixel. The capture is updated every frame with the camera.

Full-resolution water shading samples the capture with bilinear filtering.
The reflected wave direction projects a point two metres above the local water
surface into the capture, approximating ripple distortion without tracing.
An enlarged capture frustum gives that distortion room; lookups outside it fade
to the sky gradient. This is approximate planar reflection, not an exact
reflection from the displaced surface. Fine masonry details are deliberately
blurred. Only the building and quay boxes appear; cobbles and painted ground
are omitted. The wave grids use about 2 MiB independently of screen resolution.

## Running and checks

```sh
cargo run --manifest-path driver/Cargo.toml
cargo run --manifest-path driver/Cargo.toml -- --width 800 --height 600 --cam-dist 19 --cam-elev=-0.3 --time 1 --screenshot water.png
```

`--time` fixes animation time for screenshots. Screenshot timings measure the
whole frame, including opaque geometry and GI, rather than water alone.

The GPU water test reconstructs the rendered mesh from hardware depth and
checks its heights against the shared height grid within a millimetre. It
verifies moving water/wall contact while opaque depth stays unchanged, and
byte-identical output when returning to a fixed time with GI and TAA disabled. It also
checks odd-sized resizing and that all-land terrain leaves no water depth.
Reflection checks require masonry, dark faces and sky, a stable capture while
only waves animate, and an updated capture when the camera moves.
Sunlight tests query the GPU grid at lit and bank-shaded water positions near
both canal ends. They check grid reuse and edit invalidation, and compare 16,000
queries against independent double-precision intersections with all casters.

### Water measurements before geometric sun shadows

Local Vulkan measurements at 1280x800, fixed time 1, September 23, 2026.
Temporary per-pass timestamp instrumentation measured medians over 25 frames
after warmup; it was removed from the application after measurement.

| Water-specific GPU work | Wide (45 m) | Close (19 m) |
| --- | ---: | ---: |
| Heights and vertex normals | 0.03 ms | 0.03 ms |
| Water-cell culling | 0.04 ms | 0.04 ms |
| Reflection sky and masonry capture | 0.43 ms | 0.46 ms |
| Water rasterization and shading | 0.09 ms | 0.12 ms |
| Total (rounded) | 0.60 ms | 0.65 ms |

These exclude command submission, target clears, the shared sunlight shadow
pass, and opaque rendering/GI. Shared masonry placement adds about 0.03 ms and
also serves the sunlight pass. Most of the wide-to-close frame-time increase
in these scenes comes from opaque prop rendering (about 2.08 to 5.24 ms), with
additional AO and GI work. These water timings are not whole-frame FPS.

The sequential whole-frame screenshot comparison (40 frames, five warmup)
measured 14.1/18.7 ms wide/close before this change, 14.0/18.3 ms with the shared
wave mesh but procedural sky only, and 14.9/19.0 ms with coarse scene reflections.
The final version restores reflections at a small overall cost; it is not a
whole-frame performance improvement over the previous sky-only renderer.

## Appearance and limits

The water reflects the sky, buildings and bank faces at very low resolution.
Sun shadows and wet-bank shading
still anchor the water to the quay. The body is opaque, with no underwater
view, refraction, transmission absorption or underwater caustics. Light shimmer
cast onto above-water walls is not implemented; the separate diffuse GI can
exhibit temporal noise in shaded areas.

This is a simpler lighting model than the hybrid water described in
[Tomasz Stachowiak's 2024 Tiny Glade talk](https://www.youtube.com/watch?v=jusWW2pPnA0&t=1970s).
The moving geometric waterline is an addition to the flat water described
there. Waves do not simulate flow, collisions, wakes, breaking or overtopping.
