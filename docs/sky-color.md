# Deep blue sky: colour reference and gameplay camera

The old away sample in the elevated inspection image was sRGB `#5160B1`
(`81,96,177`), from scene-linear RGB approximately `(0.075,0.095,0.30)`.
Its small green/red separation gave it a violet cast. This is distinct from the
ordinary orbit view looking pale: that view sees at most about 8.3 degrees above
the horizon, while the inspection camera points 40.43 degrees upward. The former
gradient kept most of the visible sky near the bright horizon colour.

## Reference calculation

Evaluated the authors' [Hošek-Wilkie 1.4a reference data and equations](https://cgg.mff.cuni.cz/projects/SkylightModelling/)
offline, using the CIE XYZ variant: sun elevation 40.431 degrees (the actual
default light), relative azimuth 180 degrees, ground albedo 0.1, turbidity 2 and 3.
XYZ was converted to linear sRGB with the D65 matrix in
[CSS Color 4](https://www.w3.org/TR/css-color-4/#color-conversion-code).

A single exposure scale of `0.03163519356` puts the blue channel of the
turbidity-2, 40.431-degree away sample at `0.30`. That exposure is an artistic
normalization to this renderer, **not a measured camera exposure**. Every sample
then uses the same existing per-channel ACES-style fit and sRGB output transfer,
without white balancing or a separate exposure for each row.

| Turbidity | View elevation | Exposed linear sRGB | Display sRGB |
| --- | --- | --- | --- |
| 2 | 2° | `(0.5050, 0.6987, 0.8042)` | `#CEDCE1` |
| 2 | 8° | `(0.3195, 0.5193, 0.7443)` | `#B5D0DE` |
| 2 | 40.431° | `(0.0681, 0.1460, 0.3000)` | `#4C7EB1` |
| 2 | 90° | `(0.0553, 0.1034, 0.2258)` | `#40669D` |
| 3 | 40.431° | `(0.0766, 0.1529, 0.3030)` | `#5281B1` |
| 3 | 90° | `(0.0660, 0.1156, 0.2331)` | `#4A6DA0` |

These are model-derived examples under stated conditions, not universal hex
codes for sky blue. Air clarity, direction, elevation, ground reflectance and
the display transform affect the result. The deep samples have noticeably more
green relative to red than the previous palette. The low sky is naturally pale.

[Bruneton's physically based atmosphere API](https://ebruneton.github.io/precomputed_atmospheric_scattering/atmosphere/model.h.html)
also makes an important distinction: a vector of radiances sampled at three
wavelengths is not automatically linear sRGB. Its photometric colour conversion
integrates spectra with colour-matching functions (or approximates that step).
Scattering coefficients should not be pasted directly into an sRGB palette.

The radiative-transfer study
[“Revisiting the question ‘Why is the sky blue?’”](https://acp.copernicus.org/articles/23/14829/2023/)
likewise computes CIE chromaticity from spectra. It finds that ozone's role grows
toward low sun elevations; “Rayleigh blue” alone is not a complete universal sky
colour prescription.

## Chosen lightweight integration

`wyn/clouds.wyn::clear_sky` uses `(0.0681,0.1460,0.30)` for the away endpoint.
The existing sunward endpoint, shared light direction, sun disk, cloud noise and
display transform stay in the same pipeline. The reference model is not shipped
or evaluated at runtime.

The horizon transition now uses `height / (height + 0.004)`, where
`height=max(ray.y,0)`. This intentionally compresses the physical height range
into the narrow band visible to the orbit camera. It leaves a pale horizon seam
but lets deep blue appear during normal play. This vertical shaping is artistic;
the final gameplay image is not presented as a physical atmosphere simulation.
No additional passes, texture lookups, loops or scattering model are introduced.

The sky GPU test now also checks a normal orbit pitch of `-0.03`, comparing the
upper sky facing toward and away from the light and rejecting a violet away hue.
To inspect the same view with the usual release build and clouds enabled:

```powershell
cargo run --manifest-path .\driver\Cargo.toml --release -- --screenshot sky-away.png --cam-elev=-0.03 --cam-az=-0.9600704
```

In the interactive run, right-drag to the highest permitted pitch, then orbit
away from the light. The default starting camera looks downward and shows no
above-horizon sky; its pose and orbit limits are unchanged.
