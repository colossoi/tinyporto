# Sun and directional sky

`driver/src/shadow.rs::DEFAULT_SUN` (or `--sun-direction`) is normalized by the
shadow builder and stored in the geometric grid. `shadow_sun` reads that vector
for the visible disk, directional atmosphere, geometric/contact shadows, surface
lighting, GI and water. The sky has no independent sun position.

`wyn/clouds.wyn` evaluates a horizon/zenith palette with a horizontal dot product
for the forward/away colour change. That term fades at the zenith. The away hue
uses an offline physical sky reference; the pale horizon band is deliberately
compressed to make the blue visible in the orbit camera's low elevation range.
See [sky colour research and reproduction](sky-color.md) for the samples and
the distinction between model-derived hue and artistic vertical shaping. Two
reciprocal lobes in squared angular distance approximate atmospheric forward
haze and the small aureole around the sun. This is art-directed daylight, not a
physical atmosphere simulation. There are no new textures or passes, scattering
integrals, bloom or lens flares. The existing cloud noise is evaluated once and
also supplies disk transmission.

The disk is deliberately three times natural angular size: a 0.8-degree radius,
a pixel-width antialiased edge and an inexpensive limb profile. `SUN_SIZE`
controls the disk and its compact aureole together; it changes neither the
lighting direction nor shadow softness. Squared chord distance avoids acos and
preserves precision around the small disk. The square root for the limb is only needed
inside the disk. Brightness is tuned to the existing scene exposure and shared
sun colour; it is intentionally not solar radiance in physical units.

`main.wyn` tone maps sky and surfaces at final output. GI's two sky-miss paths
and `water_reflection.wyn` receive the same atmosphere without the compact disk,
so direct solar light is not counted twice or sparsely sampled into GI noise.
The low-resolution reflection capture and its cheap clear-sky fallback both
receive the current sun vector. Water retains its existing shadowed glint.

The working colour space is linear sRGB. Colour maps are decoded by sRGB texture
sampling; normals and roughness are raw data. The existing ACES-style fitted
curve maps HDR values into display range, followed by the output attachment's
sRGB encoding. This is not an ACES working-space/colour-transform pipeline.
Screenshots contain already-encoded sRGB bytes and explicitly declare sRGB.

## Reproducible inspection

From `driver/`, run:

```sh
cargo run --release -- --sun-demo clear --screenshot sun.png
cargo run --release -- --sun-demo clouded --screenshot sun-clouded.png
cargo run --release -- --sun-demo away --screenshot sky-away.png
```

The inspection camera uses a raised target and looks along the shared sun
direction; the away view changes azimuth by 180 degrees at equal elevation.
All images use the normal 20-degree FOV and exposure. The sun is approximately
63 pixels across at 800 pixels high. Clear/away disable clouds; the clouded
preset offsets the actual procedural cloud field to partially veil the default
sun. The overrides affect only the inspection sky, not ordinary frame lighting.

`solar_disk_tracks_shadow_direction_and_sky_is_directional` renders an odd-sized
GPU image, checks the disk extent, compares forward/away colour, checks dense
cloud occlusion and moves the lighting direction while leaving the camera fixed.
Existing shadow tests cover geometric visibility when that direction changes.

At 1280x800, local release captures on 2026-09-24 measured median GPU frame time
of 15.2 ms before and 14.9 ms after for the normal scene; a full sky view with
procedural clouds measured 5.5 ms before and after. Each capture runs 40 frames
and discards the first five. These whole-frame measurements show no apparent
regression in this run; the small difference is not evidence of a speedup.

## References

- [NASA Sun Fact Sheet](https://nssdc.gsfc.nasa.gov/planetary/factsheet/sunfact.html):
  1919 arcseconds apparent diameter at 1 AU.
- [Hosek and Wilkie's sky/solar model](https://cgg.mff.cuni.cz/projects/SkylightModelling/):
  near-white high-elevation sun and limb darkening.
- [Bruneton's atmosphere model](https://ebruneton.github.io/precomputed_atmospheric_scattering/atmosphere/functions.glsl.html):
  directional scattering and separation of sky radiance from the solar disk.
  Used as visual/physical guidance, not ported as a full scattering simulation.
