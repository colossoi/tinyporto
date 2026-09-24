# Surface textures

CC0 source maps for realistically weathered bricks, cobbles, and mortar.
The renderer uses these maps by default. Run with `--no-textures` to compare
with the original flat colors and geometric normals.

## Sources and license

All nine PNGs are unchanged downloads from [Poly Haven](https://polyhaven.com).
They are released under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/),
as stated on each asset page and in [Poly Haven's asset license](https://polyhaven.com/license).

| Asset | Author | Maps | Source width | Intended use |
| --- | --- | --- | --- | --- |
| [Red Brick](https://polyhaven.com/a/red_brick) | Rob Tuytel | 2048 x 2048 | 1.4 m | Individual weathered brick faces |
| [Rock Surface](https://polyhaven.com/a/rock_surface) | Amal Kumar | 1024 x 1024 | 2 m | Grain and wear on cobbles |
| [Rough Concrete](https://polyhaven.com/a/rough_concrete) | Dimitrios Savva | 1024 x 1024 | 1.2 m | Granular mortar candidate |

Each material includes diffuse/color (`diff`), tangent-space normal (`nor_gl`),
and roughness (`rough`). Source width describes the entire texture tile.
The original files total 64,481,089 bytes (about 61.5 MiB).

[sources.json](sources.json) records download URLs, authors, licensing,
dimensions, bit depths, source MD5 checksums, and local SHA-256 checksums.
All downloads matched the source MD5 checksums and successfully decoded at
their expected dimensions. Color-map previews were visually inspected.

## Build-time preparation

[`driver/build/materials.rs`](../../driver/build/materials.rs) decodes the
original 8-bit and 16-bit PNGs and prepares the final GPU bytes during the Cargo
build. It writes the six maps, their mip chains, and Rust metadata into
`OUT_DIR`. Cargo tracks the source PNGs and preparation code for rebuilding.
The source files on disk remain unchanged.

[`driver/src/materials.rs`](../../driver/src/materials.rs) embeds those prepared
bytes. Startup only creates the textures and uploads each mip: it performs no
PNG decoding, resampling, palette conversion, or mip generation. There is no
runtime asset directory or writable cache to locate. The two vehicle color
textures use the same build-time preparation and upload path. This preparation
also runs when `WYN_PRECOMPILED_DIR` supplies the shader and generated host.

- Eight brick-face interiors are resampled into 256x64 tiles in a 4x2 atlas.
  Color, normal, and roughness use exactly the same source rectangles. Every
  tile gets independent mips, stopping before tiles merge. Sampling stays inset
  by half a texel at the coarser mip, preventing joint or neighbor bleed.
- Stone and mortar retain 1024x1024 tiles, with complete mip chains. Normal
  channels are averaged as linear data; color is averaged in linear light;
  roughness mips average squared perceptual roughness, then take the square root.
- Relative RGB variation is normalized around the existing per-piece palette.
  This preserves the broad colors used by off-screen GI proxies while adding
  scanned surface variation. Bright variations are bounded for 8-bit upload.
- GPU color uses `RGBA8UnormSrgb`, with linear roughness packed in alpha. Normals
  use `RGBA8Unorm`. Hardware sRGB decoding applies to RGB only. The six maps,
  including mipmaps, occupy approximately 22.7 MiB on the GPU.

## Surface evaluation and lighting

[`wyn/material.wyn`](../../wyn/material.wyn) projects each rounded-box hit onto
its dominant local face. Stable position hashes select brick faces and offsets
for stone/mortar, independently of the compacted draw index. Stone and mortar
start with the source physical tile widths. Quoins use the stone maps with
reduced contrast and normal strength to retain the pale limestone palette.

The OpenGL tangent normal (+Y) is transformed using a right-handed face basis,
then applied to the rounded geometric normal. Image V increases downward, while
the normal's positive Y follows the upward bitangent. This also handles negative
box faces. Normal strength is deliberately moderate to preserve rounded edges
and GI history. Dominant-face projection can still expose subtle seams at bevels.

Wyn's sampler takes explicit LOD. We estimate a conservative footprint from
camera distance, viewport height, field of view, and viewing angle, then use
trilinear mip filtering. This is isotropic filtering, not anisotropic sampling.

The G-buffer remains 12 bytes/pixel. Its albedo alpha contains zero for sky or
`0.5 + 0.5 * roughness` for surfaces, retaining the existing validity threshold.
World-space shading normals use the existing octahedral attachment. Deferred
lighting adds dielectric GGX sunlight using roughness; diffuse GI reads textured
albedo and normals without feeding view-dependent specular into its history.
Changing the texture toggle invalidates GI history.

The flat ground retains its existing sand/paint colors. Rough concrete supplies
the wall mortar detail; plaster, wood, and location-dependent moss or dampness
are outside this initial set. Textures change shading, not geometry or depth.

## Validation

The Vulkan integration tests check material changes in the G-buffer, unchanged
surface depth, stable mapping across frames, roughness packing, comparison-mode
restoration, and GI invalidation. The existing GI transport/reference test passes
with textured surfaces (relative RMS error 0.157 in its 80x60 regression scene).
The CPU mip test checks linear-light color filtering and roughness filtering.
