# Cell terrain and canal banks

Terrain is a 40 by 40 row-major array of one-metre squares. Each cell uses one
16-byte `vec4f32`: `(normal_x, normal_z, offset, kind)`. Kind 0 is water, 1 is
land, and 2 is a square divided by a straight line. For a divided square:

```
land = dot(normal, world_xz - cell_center) <= offset
```

The normal is unit length and points toward water. The offset is in metres.
The grid occupies `[-20, 20]` in X and Z. The complete buffer is 25,600 bytes;
it is geometry data, not a sampled distance field. Line positions and angles
are independent of cell resolution. Two banks, a water stripe, or an island
entirely inside one cell cannot be represented.

Adjacent cells must agree on their shared edge: a crossing has the same
position and the same land side in both cells. An exactly grid-aligned bank
belongs to the land-side cell, represented as kind 2 with its line on the edge;
the neighbour is full water. A line that only touches a corner has no bank
segment. Future editing must preserve these invariants and reject shapes that
need more than one dividing line in a cell.

`driver/src/terrain.rs` supplies a fixed initial canal with oblique banks.
`Renderer::set_terrain_cells` uploads changed cells and invalidates the retained
bank-shadow geometry. Both grids survive ordinary frames and resize. Geometry
export and shadow-index rebuilding happen before the next rendered frame.
Interactive canal editing is not implemented. Fence/building painting and
camera gestures still use the y=0 authoring plane.

## Surfaces

- Land and painted footprints remain at y=0. Their fragments are clipped by
  the containing cell's half-plane, exposing real depth through the canal.
- The bed is a separate opaque surface at y=-1.8.
- Water is an independent height field centred at y=-0.6, displaced by up to
  4.27 cm. A mesh with 12.5 cm vertex spacing is rasterized against opaque scene
  depth, so its contact with masonry moves. Opaque green-blue body colour,
  interpolated wave normals, coarse scene reflections and shadowed sunlight are composed before
  tone mapping. See [water rendering](water.md).
- `banks.wyn` intersects each cell's line with its square and generates broad
  limestone coping slabs and six courses of ashlar face stones down to the bed.
  Backing fills the mortar gaps. The cap spans y=-0.06 to +0.11, extends into the
  land and slightly overhangs the water. Face joints alternate within each run.
  Runs currently stop at cell boundaries; dedicated corner/miter stones are a
  later extension.
- Cobbles whose centres are on land retain their complete shapes. The coping
  overlaps their boundary ends and is higher than the cobbles, hiding partial
  stones with ordinary depth testing. There is no per-cobble clipping.

Banks use the existing rounded-box prop/material renderer and cast sunlight
shadows independently of camera visibility. Opaque AO and GI continue to use
the bed and bank depth. The GI world fallback tests land membership, intersects
the bed/coping, and walks cells to hit bank faces; it no longer treats canals
as solid ground at y=0. Individual stone joints and bevels are screen-space
detail, like the existing building proxies. Water's specular shading does not
enter diffuse GI history.

## Checks

The cell tests check shared-edge continuity, single-boundary coverage, memory
size, and ownership of grid-aligned shores. The GPU canal test checks that the
G-buffer exposes a submerged bed and raised coping, that no ground or cobbles
float across the canal, and that animated water changes only pixels whose
opaque surface is below or close to the waterline (including the wet band).
Water integration tests also check fixed-input repeatability, displaced contact,
mesh accuracy and odd-sized viewports. Existing GI/reference tests retain
their original all-land fixture.
