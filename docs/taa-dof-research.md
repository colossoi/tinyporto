# TAA shimmer and DoF transitions: research

**Follow-up:** the [implemented correction and validation](image-finishing.md)
supersede the recommendations below. TAA remains enabled by default, as requested.
DoF was subsequently removed to simplify the renderer and reduce rendering work.
The following records the source audit before that correction.

Reviewed 2026-09-24 after reports of constant TAA shimmer and visible DoF band
edges. This is a source audit and comparison, not a verified runtime diagnosis
or an implemented fix.

## Findings in our implementation

### TAA

In `wyn/temporal.wyn`, reconstructed color history is on the unjittered pixel
grid, but its alpha stores the raw jittered center depth (`center.w`). History
validation reads these depths as though they occupy the color history grid.
There is another correspondence concern at silhouettes: the nearest sample in
the current 3x3 neighborhood supplies both motion and expected previous depth,
while history is fetched at the center pixel displaced by that motion. The
neighbor's depth need not describe the center's surface.

These are concrete sample-correspondence inconsistencies. Repeated rejection
is a plausible consequence, but its contribution to the reported shimmer needs
measurement with a history-acceptance visualization.

In `pkg/taa/src/lib.wyn`, `clip_history` performs componentwise clamping to
intersected neighborhood and variance bounds. The bounds change as jitter
changes which details are visible. The brightness-change heuristic then raises
the current-frame contribution from 8% to as much as 54% for ordinary pixels.
That can favor unstable fresh samples precisely where accumulation is needed.
This is a second candidate mechanism, not a measured attribution.

The GPU test in `driver/src/app.rs` compares resolved frames against raw
*jittered* frames. Reducing their variation by 57% does not establish that TAA
improves on the original unjittered renderer, nor that the remaining variation
is imperceptible. The test also averages away localized problems.

### DoF

The circle-of-confusion (CoC, or signed blur radius) formula in
`pkg/dof/src/lib.wyn` is continuous. The image formation is not: foreground
samples switch on below -0.5 pixels, and background gathering switches on when
the center exceeds +0.5 pixels. The existing smooth disc-edge coverage does not
smooth those gates.

`wyn/finishing.wyn` rounds 32 disc offsets to whole pixels and point-loads them.
The offsets are fixed for a given resolution; changing CoC changes weights,
not sample positions. Integer sampling nevertheless discards subpixel coverage
and can expose the sampling pattern. The resolve has no filtered foreground
coverage or explicit smooth transition between the original sharp image and
small-radius blur.

Reducing aperture only moves these transitions. The radius cap is continuous
and is not, by itself, evidence of an abrupt band boundary.

## What other implementations do

### TAA references

[Playdead's production TAA](https://github.com/playdeadgames/temporal) makes
jitter handling explicit and combines closest-depth motion selection with
history rectification and feedback control. It is a useful compact reference
for auditing the basic resolve.

[Alex Tardif's implementation walkthrough](https://alextardif.com/TAA.html)
recommends establishing reprojection correctness before adding jitter. It
demonstrates current-image reconstruction, filtered history, variance clipping,
and luminance-weighted blending to reduce flicker. These are useful isolated
steps for debugging our implementation.

[Unity HDRP's TAA shader](https://raw.githubusercontent.com/Unity-Technologies/Graphics/master/Packages/com.unity.render-pipelines.high-definition/Runtime/PostProcessing/Shaders/TemporalAntialiasing.hlsl)
includes contrast- and motion-dependent anti-flicker options. It can relax
history bounds when spatial and temporal contrast are high, especially when
stationary, and reduce the fresh-frame blend contribution. This explicitly
addresses the conflict between aggressive history rejection and stability;
turning every brightness change into reactivity is insufficient.

### DoF references

[Unity URP's Bokeh DoF shader](https://raw.githubusercontent.com/Unity-Technologies/Graphics/master/Packages/com.unity.render-pipelines.universal/Shaders/PostProcessing/BokehDepthOfField.shader)
has separate CoC, prefilter, blur, postfilter, and composite passes. The gather
tracks near and far contributions, uses fractional filtered samples, and
postfilters the result. The final composite smoothly blends the sharp image
with blur using CoC and foreground coverage. Internal classifications can
still be discrete; the reconstructed output must handle their transitions.

[AMD FidelityFX DoF 1.1](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/depth-of-field/)
uses near/far processing, prefiltered mip levels, tile classification, and
background hole filling. Its final combination smooths foreground opacity and
adds a small 3x3 blur for CoC between half a pixel and two pixels, bridging sharp
input and the larger half-resolution blur. Layers are blended by CoC and
coverage. AMD also recommends placing this DoF after TAA because unstable input
can become more visible after blur. This is a more substantial implementation
than our current gather.

[Infinity Ward's GPU Gems chapter](https://developer.nvidia.com/gpugems/gpugems3/part-iv-image-effects/chapter-28-practical-post-process-depth-field)
explains discontinuities in naive variable-radius gathers, foreground boundary
problems, and smoothly combining differently blurred images. It supports
treating coverage and final composition as part of the effect, rather than
assuming a continuous radius formula guarantees a continuous image.

Our [Tiny Glade notes](../rendering-tiny-glade.md#6-depth-of-field) describe a
more ambitious aperture-ray-marching approach, with radial data reuse,
background reconstruction, prefiltering and adaptive resolution. Our single
pass is not an implementation of that algorithm. Its complexity should not be
hidden behind a promise to make a small parameter adjustment.

## Recommended direction

1. Keep TAA disabled during ordinary visual evaluation until a corrected resolve
   demonstrates an improvement. `--no-taa` already disables both it and jitter.
   This research has not changed the default.
2. Audit TAA sampling and depth correspondence first. Capture motion,
   history acceptance, clipping amount, and effective blend weight with DoF off.
   Test reconstruction and history separately before tuning anti-flicker.
3. Validate fixed-camera/fixed-time scenes against TAA off and a supersampled
   reference, then test slow camera movement, fine brick edges, silhouettes,
   water, and disocclusion. Check local worst regions as well as averages;
   smoothing everything is not an acceptable way to pass a stability test.
4. Replace the DoF single-pass resolve with a modest URP-style staged effect:
   CoC/prefilter, filtered near/far gather, postfilter, and smooth composition.
   Explicitly cover small blur radii, drawing on AMD's transition treatment.
   Validate with a textured continuous depth ramp and foreground silhouettes.
5. Keep both effects in the existing local packages. Application adapters own
   camera conventions, images and pass scheduling; numeric kernels remain
   generic. Additional DoF passes require intermediate images, not additional
   frame history. Keep the full Tiny Glade ray marcher as a separate, larger
   quality step if this simpler approach proves insufficient.

Only documentation changed during this research; rendering behavior and
defaults are unchanged. No runtime verification of a fix has been performed.
