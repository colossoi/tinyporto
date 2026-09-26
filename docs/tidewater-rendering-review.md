# Tidewater rendering ideas for Tiny Porto

Reviewed September 25, 2026. Tidewater source pinned to
[`4811ba48d795197de5621985f404e765c0b7c0ef`](https://github.com/dgreenheck/tidewater/tree/4811ba48d795197de5621985f404e765c0b7c0ef/src).
Tiny Porto checkout: `32ec4b8aa7765d0cf1e5a293875667a4c4db1497`.
This initial source review is not a same-hardware benchmark. No renderer code
was changed during the review itself. Subsequent implementation, rejected
attempts, measurements, and open gates are recorded in the
[September 26 WIP handoff](tidewater-thread-status-20260926.md).
The inspected upstream files are saved in `tmp/tidewater-review/`, with `REVISION.txt`.

The main opportunity is to borrow Tidewater's separation of visible wave geometry,
fine surface shading, and cached sky lighting. Tiny Porto already avoids evaluating
wave octaves per water pixel and already has a small reflection capture. The most
useful next changes would add detail and lighting quality to those foundations.

## What is actually running

- The current renderer is custom WebGPU/WGSL. Several `src/core` files are compatibility
  exports; the actual renderer lives under `src/engine`.
- `App.js` selects **SkyProClouds** by default. `Clouds.js` is the older implementation,
  selected with `?oldClouds`. Its quality constants should not be used to describe the default.
- Output canvas dimensions default to the browser's CSS viewport dimensions. The engine
  does not multiply them by `window.devicePixelRatio`. A high-DPI screen's physical
  resolution therefore is not necessarily its rendered canvas resolution.
- The separate internal render scale defaults to 1 and can be manually set from 0.5 to 1.
  The README mentions automatic dynamic resolution, but `App.js` explicitly says the
  current implementation does not adjust this automatically. TAAU also operates at scale 1.
- The README's performance statement is a **target** of 60 fps at 2560x1267 on an Apple
  M5 Pro, not a measurement we reproduced. Tiny Porto's historical local profile uses
  an RX 580, so comparing those figures would not isolate renderer efficiency.

Sources: [active cloud selection](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/App.js#L115-L120),
[render-scale control](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/App.js#L704-L712),
[canvas sizing](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/engine/Engine.js#L58-L68),
[README](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/README.md).

## Sky: expensive physics evaluated in small, reusable images

### Atmosphere lookup textures

`Atmosphere.js` implements a Hillaire-style atmosphere with Rayleigh, Mie and ozone
terms. It uses three RGBA16F lookup textures: 256x64 transmittance, 32x32 multiple
scattering, and 192x108 sky view. Together those images occupy about 298 KiB, excluding
other buffers. Looking up sky radiance requires angular mapping and a filtered texture
sample; the atmosphere integration loops run while generating the small textures.

Transmittance and multiple scattering rebuild when marked dirty. Sky view rebuilds
when its packed parameters change. Camera height is quantized to 2 m near sea level,
then roughly 2% increments above 100 m. Camera rotation does not require a rebuild.
Continuously changing the sun can still rebuild sky view every frame.

**Tiny Porto application:** replace or supplement `clouds.clear_sky` with a cached
directional radiance image. The same source should serve the visible sky, reflected
sky and GI misses. Start with a fixed daylight setup; dynamic weather and time of day
are separable additions. This buys a richer atmosphere, not an assured speedup over
our cheap analytic gradient. Match our exposure and art direction: our current sky
deliberately compresses the pale horizon band because the orbit camera sees very
little sky. A literal physical replacement could make ordinary views paler again.

Sources: [LUT definitions](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/Atmosphere.js#L8-L30),
[sampling](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/Atmosphere.js#L190-L216),
[invalidation](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/Atmosphere.js#L471-L500).
Local: [clouds.wyn](../wyn/clouds.wyn), [sky colour rationale](sky-color.md).

### Volumetric clouds sampled sparsely

The default `SkyProClouds.js` uses a baked, mipmapped 64-cubed noise volume, a weather
map and a coarse map of weather bounds. Its volume has sun self-shadowing, approximate
multiple scattering, darkened bases, forward-scattering bright edges and atmospheric
fading with distance. This is a substantial visual difference from our flat noise sheet.

The main view keeps a history at half width and half height. Each frame traces one
position in each 4x4 history block. Consequently, at scale 1 it traces approximately
**1/64 as many rays as there are output pixels**, before edge rounding. At 2560x1440,
that is 320x180 fresh rays, reconstructed into a 1280x720 history. The resolve, panorama,
shadows and final sampling are additional work; 1/64 rays does not mean 1/64 total cost.

Rays skip empty weather cells, use coarse steps in empty space, select noise mip levels
from their footprint, reuse light-march results and terminate at low transmission.
History reprojection includes camera movement and wind drift, with neighborhood
clamping and resets for cuts, coverage changes and large lighting changes.

**Tiny Porto application:** a separate cloud history could make volumetric clouds
affordable, but it is a significant feature with reconstruction and invalidation costs.
For a smaller first experiment, generate a richer lit cloud panorama and reuse it in
the sky, reflections and GI. A baked panorama is also a useful art-direction test
before implementing animation. Our default downward-looking camera benefits mainly
through reflections; cloud quality should also be checked from the allowed horizon views.

Sources: [quality and resources](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/SkyProClouds.js#L5-L55),
[march](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/SkyProClouds.js#L220-L328),
[reprojection](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/SkyProClouds.js#L766-L795),
[allocation](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/SkyProClouds.js#L855-L866).

### Different sky representations for different consumers

Clouds also have a 512x160 directional panorama: only 1/16 is refreshed each ordinary
frame. A 256x256 cloud-shadow map updates a quarter of its rows per frame. Initial
population and some invalidations perform more work.

`Environment.js` captures a 128-pixel cube, builds six roughness levels with GGX
filtering, and integrates nine spherical-harmonic coefficients for diffuse lighting.
After initial population, refresh work is spread over frames; a sun-angle threshold
or a three-second timer starts a refresh. It switches filtered buffers only when done.

The water itself samples atmosphere plus the **cloud panorama**, then adds conditional
screen-space object reflections. It does not sample that GGX environment cube for its
ordinary sky reflection. The cube primarily serves material image-based lighting.
Sun and moon disks are excluded from reflection/environment sky functions because
the direct-light specular calculation handles them separately. Tiny Porto already
makes the same direct-sun separation.

**Tiny Porto application:** reuse a directional sky cache across consumers, but retain
our camera-dependent planar capture for buildings and quay walls. A sky cube cannot
replace nearby scene reflections. Cache the current reflection capture while camera,
scene, sun and relevant animation are unchanged; wave motion changes the lookup, not
the captured mean-plane scene. That is a measurable optimization candidate, not a
reason to update moving-camera captures at a visibly low rate.

Sources: [cloud update scheduling](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/SkyProClouds.js#L999-L1044),
[environment scheduling](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/Environment.js#L252-L307),
[separate sky consumers](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/sky/Sky.js#L174-L196).
Local: [water_reflection.wyn](../wyn/water_reflection.wyn).

## Water: detail beyond the geometry

### Separate displacement from fine normals

Tidewater computes four 256x256 FFT cascades spanning 733, 157, 33.3 and 7.1 m tiles.
Non-integral size ratios reduce obvious repetition. It stores displacement and
derivatives in separate mipmapped RGBA16F array textures. The transform uses two
dispatches, but the normal steady update also has **four mip-generation dispatches**:
six total in one compute pass. The header's two-dispatch claim describes the transform,
not the complete update. Simulation work is independent of screen resolution.

Water geometry uses a distance-dependent quadtree with morphing transitions. Vertex
displacement is filtered to what the local mesh spacing can represent. Fragment
normals sample higher-frequency derivatives with anisotropic filtering. Very close
views add two rotated, rescaled samples of the finest cascade, fading them with the
pixel footprint. The slope vectors are rotated back into world axes before combination.

**Tiny Porto application:** retain our 12.5 cm mesh and shared wave-height grid, then
add one or two small, mipmapped ripple-slope textures to water shading. They can be
precomputed or animated without adopting an FFT. Geometry continues to determine
contact with masonry, while finer normals break up reflections and highlights.
Combine slopes or use reoriented normals; fade/filter subpixel detail rather than
letting it shimmer. Do not increase mesh subdivisions just to add tiny ripples.

Sources: [FFT layout](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/OceanFFT.js#L5-L32),
[complete update](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/OceanFFT.js#L610-L631),
[geometry filtering](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/WaterSurface.js#L122-L138),
[fine ripples](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/WaterSurface.js#L293-L305),
[CDLOD](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/core/CDLOD.js).
Local: [water.wyn](../wyn/water.wyn).

### Roughness and coherent variation

Tidewater turns unresolved wave slopes into additional roughness, then shades direct
sun with GGX. Its water shader also adjusts reflected-sky direction using that slope
spread. Tiny Porto currently uses a fixed `pow(dot(reflection, sun), 120)` highlight.

`SeaDetail.js` adds drifting gust patches, calm slicks and wind-aligned streaks using
a precomputed noise texture. These modulate short-wave slopes and roughness, making
different areas react differently to the same sky. Its hundreds-of-metres defaults
need substantial rescaling for our canals.

**Tiny Porto application:** pair detail normals with footprint-dependent highlight
roughness, then add restrained world-space variation. Treat it as a shading change
rather than directly painting arbitrary light/dark patches on the water. This is my
first recommended experiment: it addresses uniformity and overly smooth reflections
without new screen-sized targets or reflection tracing. More texture samples and
arithmetic still have a cost that should be measured on our GPU.

Sources: [roughness and GGX](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/WaterMaterial.js#L305-L374),
[gusts and slicks](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/SeaDetail.js#L7-L92).

### Body colour, foam and reflections

Water body colour comes from absorption and analytically integrated scattering along
a depth-dependent path. Foam is lit by sun and sky; bubbles and stirred sediment alter
the volume colour. Refraction uses opaque scene data plus an additional half-resolution
underwater capture. Object SSR uses an 11-step march and three refinements on a hit,
and skips directions/Fresnel weights unlikely to contribute. The optional planar
reflection hook is not connected by the current `App.js` water constructor.

**Tiny Porto application:** try a modest depth/lighting-dependent opaque body colour
before adding transparency. Preserve the murky Venetian-water character. The existing
screen depth gap is not automatically the correct refracted water path, especially
at walls, so it should not be blindly substituted into physical absorption equations.
Our small planar capture is a good fit for a canal full of offscreen reflecting
buildings. SSR and clear tropical refraction would add complexity with uncertain
benefit here. Persistent foam and wake fields are useful later if boats become active.

Sources: [volume shading](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/WaterMaterial.js#L482-L537),
[SSR](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/WaterMaterial.js#L680-L738),
[active water setup](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/App.js#L246-L249),
[refraction capture](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/RefractionPass.js).

## Finishing and performance lessons

Tidewater uses temporal reconstruction even at native internal resolution. Its custom
FSR2-derived resolve has detail locks, luminance-instability handling and accumulated
sample weights. Water gets special history handling because its motion vectors only
describe the camera. This is not a drop-in integration of the complete FSR2 pipeline.
Tiny Porto already has jitter, Catmull-Rom history filtering, variance clipping and
reactive water, so temporal reconstruction itself is not a missing technique.

Tidewater also uses bloom, exposure adaptation and an ACES fit with colour-space
matrices. Tiny Porto uses a simpler per-channel ACES-style curve. Identical linear
sky/water colours will therefore not necessarily produce the same displayed result.
A controlled fixed-exposure comparison should precede copying palettes or adding
bloom. The latter adds screen-dependent work and changes the whole game's appearance.

Other useful engineering details include batched compute mip generation, reduced
precision intermediates where tolerated, half-resolution AO and refraction, and
separate water pipeline variants when hull masking needs fragment discard. For Tiny
Porto, investigating its water fragment's explicit depth load/discard/depth output
is a possible early-depth optimization, but its actual effect depends on generated
shader and driver behavior; source inspection alone cannot establish a saving.

Sources: [temporal reconstruction](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/post/TemporalUpscale.js#L11-L40),
[post processing](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/post/PostFX.js),
[water pipeline variants](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/src/ocean/WaterMaterial.js#L128-L149).
Local: [temporal.wyn](../wyn/temporal.wyn), [TAA package](../pkg/taa/src/lib.wyn), [shade.wyn](../wyn/shade.wyn).

## Recommended experiments, in order

| Priority | Experiment | Expected value | Main cost or risk |
|---|---|---|---|
| 1 | Fine ripple slopes + filtered GGX sun highlights | More convincing water at close and grazing views | Fragment sampling; shimmer or trails if filtering is wrong |
| 2 | Subtle gust/slick variation at canal scale | Less uniform water with small implementation scope | Art tuning and additional samples |
| 3 | Rich lit cloud panorama shared by sky, water and GI | Evaluate the sky's visual contribution cheaply | Static capture lacks animated volume/parallax |
| 4 | Cached atmosphere LUT with consistent lighting/exposure | Coherent directional sky and changing sunlight | Integration work; physical horizon may conflict with current styling |
| 5 | Sparse volumetric clouds with their own history | The largest cloud fidelity upgrade | Several resources/passes; temporal artifacts and camera-cut recovery |
| 6 | Optional internal render scale + temporal upscale | Potentially useful at high output resolutions | Fine masonry quality; output-sized resolve still costs work |

If open lagoon/ocean views become a major feature, revisit FFT cascades and CDLOD.
For the present calm canals, their scope is much larger than the first two experiments.

Measure each experiment independently with fixed time, exposure, camera and sun, then
check animation and camera movement. Include the default view, close water, grazing
reflections and a visible-sky view. Compare actual internal pixel dimensions, not only
window size. Use per-pass GPU timings and examine contact stability, distant shimmer,
sun-glint trails and cloud history after camera cuts.

The [historical frame profile](frame-profile.md) measured approximately 0.82 ms for
water, 4.25 ms for GI, and 3.61 ms for prop visibility/compaction, out of a 15.91 ms
generated frame without DoF. It predates subsequent changes and is not a current
benchmark. It nevertheless cautions against expecting a large whole-frame speedup
from replacing our water simulation. Re-profile before assigning performance budgets.

Upstream code is published under the [MIT license](https://github.com/dgreenheck/tidewater/blob/4811ba48d795197de5621985f404e765c0b7c0ef/LICENSE).
Preserve the upstream notice for substantial copied/translated code; separately
check provenance for any imported assets. This review copies no rendering implementation.
