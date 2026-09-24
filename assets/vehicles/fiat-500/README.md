# Fiat 500 (1970)

Version 3 adds painted inner fender wells behind all four wheels. Smooth rear
panels with textured slots, round headlights, an empty cabin and opaque tinted
windows are retained.

| | Original | Version 1 | Version 2 | Version 3 |
|---|---:|---:|---:|---:|
| Triangles | 498,649 | 27,250 | 27,050 | 27,394 |
| GLB size | 14,798,552 bytes | 701,096 bytes | 850,416 bytes | 864,048 bytes |

Version 3 has **27,394 triangles**: 344 more than version 2 (+1.27%), and 94.51%
fewer than the original. Each front wheel well uses 92 triangles; each rear
wheel well uses 80. They share the existing body-paint material.

The two rear panels use **370 triangles instead of 1,698** in version 1.
The new liners increase the GLB size by 13,632 bytes over version 2.

## Files

- `fiat-500-simplified-v3.glb`: current version, ready for Sketchfab upload.
- `fiat-500-simplified-v2.glb`: preserved version before adding wheel wells.
- `fiat-500-simplified.glb`: identical current model under the standard filename.
- `fiat-500-sketchfab-upload.zip`: current GLB plus attribution text.
- `fiat-500-simplified.blend`: editable Blender asset; textures are packed.
- `sketchfab-description.txt`: ready-to-paste description and creator credit.
- `preview.jpg`, `preview-rear.jpg`, `preview-side.jpg`, `preview-underside.jpg`: current renders.
- `preview-wheel-wells.jpg`: low front/rear views of the painted liners.
- `comparison.jpg`: original versus current model.
- `revision-comparison.jpg`: version 1 versus version 2, front and rear (historical).
- `versions/fiat-500-v1.glb`: preserved previous simplified model.
- `source/fiat-500-original.glb`: unmodified Sketchfab download.
- `textures/`: two baked color/shadow PNGs, also embedded in the GLB.
- `simplify.py`, `rear_vents.py`, `headlights.py`, `fender_wells.py`: rebuild scripts.
- `simplification-report.json`, `validation-report.json`: geometry statistics and checks.
- `wheel-well-validation.json`: material, tire-clearance and backing checks.

## Changes

- Removed the cabin/floor mesh, both seats, steering wheel/column, underside
  axle/rollbar meshes and concealed bumper supports.
- Reduced the exterior body, doors, roof, hood, lamps, mirrors, bumpers and trim.
- Kept all wheel positions; reduced the rims and rebuilt tires from a measured
  section with 64 circular segments, omitting the fine tread.
- Replaced both rear vent areas with smooth fitted surfaces. Slot shadows are
  baked into color textures, without physical openings or normal maps.
- Rebuilt the main headlight lenses and chrome surrounds with 48 circular
  segments. Added simple reflectors behind the glass; indicator surrounds use 32.
- Moved the rear plate assembly slightly outward to clear the smooth cover.
- Made cabin windows dark blue-gray and opaque, with no transmission.
- Added shallow inner fender wells: fitted arch returns and recessed back
  panels close the see-through wheel openings. Their back walls clear the
  tires by approximately 3.1 cm at the in-game scale. The rest of the underside
  remains open.
- Preserved source scale and world orientation. Parts remain separate meshes.

The underside remains an open exterior shell. The GLB preserves its source
length of approximately 7.73 units. Tiny Porto uses a separate mesh bake,
rescaled to 3 metres long, 1.31 metres wide and 1.35 metres high.

## Default scene

The car is parked beside the right-hand building at `[3.0, 0.06, 2.1]`, with a
0.12-radian yaw along the canal. Placement and ray proxies live in
`wyn/vehicle.wyn`. The 27,394-triangle mesh writes the scene G-buffer and the
water reflection capture; opaque windows and mipmapped rear vent textures are
preserved. Materials use Tiny Porto's existing albedo/roughness lighting.

For off-screen GI rays, one enclosing box rejects misses before testing six
interior boxes (body, cabin, four wheels). The same six boxes cast geometric
sun shadows. These are deliberately approximate; screen-space contact shadows
and GI see the detailed visible mesh. No enclosing box is drawn.

`prepare_scene.py` reads the current simplified GLB and bakes node transforms,
unit normals, UVs, material colors and roughness into `scene/vertices.bin`
(64-byte vertices), `scene/indices.bin` (32-bit triangle indices), and
`scene/mesh.wyn` (draw count). The driver embeds the buffers and the prepared
texture mipmaps. Cargo converts the two PNGs at build time, so the game needs
neither Blender nor a runtime GLB loader or PNG decoder for these assets.
To refresh after editing:

```powershell
python assets/vehicles/fiat-500/prepare_scene.py
cargo run --manifest-path driver/Cargo.toml
```

The converter needs NumPy and verifies index bounds, finite data, normal lengths,
and nondegenerate triangles. `scene/manifest.json` records bounds and the source
hash. Keep the generated buffers and draw count together.

## Rebuild and verification

Built using Blender 4.5.9 LTS:

```powershell
blender --background --factory-startup --python assets/vehicles/fiat-500/simplify.py
```

`simplify.py` rebuilds the standard GLB, Blender file, textures and triangle
report. Versioned copies, the ZIP, previews and upload description are delivery
artifacts and must be refreshed after a rebuild.

The GLB was reimported and rendered from front, rear, side and underside views.
Checks verified valid accessors and indices, finite geometry, unit normals,
zero degenerate triangles, four wheel assemblies, opaque windows, and removal
of interior/undercarriage meshes. Each replacement rear panel has exactly one
outer boundary and no internal vent holes. Both textures are embedded and
decode correctly. Maximum bounding-box change is approximately 0.00102 source
units. The preserved original matches the downloaded file by SHA-256.

## Attribution and license

**Fiat 500 - 1970 Model** by **Parsa Farvadian**
([creator](https://sketchfab.com/parsafarvadian)).

Source: <https://sketchfab.com/3d-models/fiat-500-1970-model-213cd126fcd744809fb7996eb88366f5>

License: [Creative Commons Attribution 4.0 International (CC BY 4.0)](https://creativecommons.org/licenses/by/4.0/).

Downloaded 2026-09-24. Modified for Tiny Porto as described above. Retain the
creator attribution, source and license links, and modification notice with
redistributed assets.
