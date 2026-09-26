# Nested helper calls with a fragment-captured array record

Reduced from Tiny Porto's spectral-water mip sampling. The standalone failing
file has no imports, FFT, textures, or external assets. It only needs the Wyn
compiler; the failure occurs during compilation, before GPU execution.

From the Tiny Porto repository root:

```powershell
wyn build --graphics -O --target-double rust-wgpu --target spirv repro/water_mip_fragment_capture.wyn -o tmp/water_mip_fragment_capture.spv
```

Expected: successful compilation. For runtime use, `values` must contain at
least 16 elements. The degenerate triangle is intentional: rasterization is
only present to exercise fragment compilation.

Observed: exit 1, with the installed compiler on September 25, 2026:

```text
error: egglog output: egglog to SSA: _w_stage_reproduce__fragment (BlockId(16)): egglog output: egglog to SSA: TODO: first-class value Global(SymbolId(270))
```

The same error occurs without `-O`. Numeric symbol/block IDs may change with
compiler versions or edits to the source.

## Trigger and controls

`make_levels` computes two arrays and returns a record. The fragment captures
that record and calls `slopes -> sample -> sample_level`. Merely capturing a
record of two arrays does **not** establish this failure: the additional helper
boundary matters in this reduction.

Both controls were verified to compile successfully with the same flags:

```powershell
wyn build --graphics -O --target-double rust-wgpu --target spirv repro/water_mip_fragment_capture.inline-control.wyn -o tmp/water_mip_fragment_capture.inline-control.spv
wyn build --graphics -O --target-double rust-wgpu --target spirv repro/water_mip_fragment_capture.compute-control.wyn -o tmp/water_mip_fragment_capture.compute-control.spv
```

- **Inline control:** substitutes the body of `sample` at its two call sites
  in `slopes`; the array record and fragment capture remain. Exit 0.
- **Compute control:** keeps the original helpers and calls them from `map`
  instead of a fragment shader. Exit 0.

These narrow the failure to how the captured record passes through nested
helpers in the graphics path. The internal compiler cause has not been proven.
The full renderer was worked around by packing its nine mip arrays into one
buffer; this reduction also identifies helper inlining as a potential smaller
workaround, which has not yet been tested in the full renderer.

## Compiler identity and evidence

Installed executable: `C:/Users/gmiller_amilarcap/.cargo/bin/wyn.exe`.
File timestamp: September 25, 2026, 13:13:50 local time. Size: 23,741,440 bytes.
SHA-256:

```text
49936AB8401AE00BC97724928F6CFC611B886A475D769B4BB76A35D734D9C0EE
```

Logs are in `tmp/water_mip_fragment_capture.log`,
`tmp/water_mip_fragment_capture.noopt.log`, and the corresponding
`inline-control.log` / `compute-control.log` files.
