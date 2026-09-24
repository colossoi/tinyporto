# tinyporto/taa

Local, stateless Wyn kernels for temporal image reconstruction. Import with
`import "pkg:taa"`. No renderer, camera, material, texture, or history allocation
is owned by this package.

1. Build a `neighborhood` with `empty` and nine `add` calls. Supply linear HDR
   RGB, integer pixel offsets in a 3x3 square, and the projection displacement
   in pixels using the same axis directions. Reconstruction uses a separable
   Mitchell filter; its distances remove that projection displacement.
2. Reproject/sample the previous resolved image in the caller. Validate bounds,
   surface identity, and depth. `depth_agrees` accepts positive view depths in
   the same coordinate frame, with explicit absolute and relative tolerances.
3. Call `resolve` with the sampled history, validity, current-frame weight,
   reactivity, and variance gamma. Invalid history returns the reconstructed
   current sample. Reactivity 1 uses only the current sample; 0 permits the
   configured history weight. YCoCg ray-box clipping rectifies history without
   independently clamping channels. Luminance-compressed weights reduce HDR
   flicker; brightness differences do not automatically increase reactivity.

`history_filter` supplies a Catmull-Rom weight for the caller's history samples,
avoiding repeated bilinear softening during motion. The application chooses
motion-dependent weights and variance bounds. Stored depth and reconstructed
RGB must describe the same pixel grid.

Weights/reactivity must be in [0,1], gamma nonnegative, and colors finite.
Callers own motion conventions, history reset policy and ping-pong resources.
There is one recursively accumulated previous image, not a queue of frames.

Tiny Porto's adapter is `wyn/temporal.wyn`; its resource lifetime component is
`driver/src/temporal.rs`. The adapter reconstructs static-world camera motion
from depth, treats sky as rotation-only, and supplies water reactivity. Those
choices are application policy, not part of these kernels.

The filtering and clipping follow the techniques described in
[Alex Tardif's TAA implementation notes](https://alextardif.com/TAA.html).
