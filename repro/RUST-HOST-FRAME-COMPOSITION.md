# Rust host composition loses a multi-compute graphics root

**Retest 2026-09-22: fixed.** The updated compiler emits one `host_reproduce`
containing both map dispatches, then the draw, and returns both buffers followed
by the texture for both SPIR-V and WGSL. Tinyporto's unchanged source now also
emits one `host_tinyporto_frame` with its compute work and five persistent buffer
results. The original failure is preserved below as a regression description.

`rust_host_frame_composition.wyn` is one source entry that computes two arrays,
uses the first in a draw, and returns both arrays and the image.

Reproduce with the current installed Wyn compiler:

```powershell
wyn build --graphics -O --target-double rust-wgpu --target spirv repro/rust_host_frame_composition.wyn -o tmp/rust-port/composition.spv
```

Expected: one `host_reproduce` call dispatches both computations, draws using the
computed array, and returns both buffers and the texture in source order.

Previously observed: `host_reproduce` only draws and returns the texture. Dispatches and
buffer results are stranded in `host_reproduce__compute_0` and
`host_reproduce__compute_1`. Removing the second computation makes the dispatch
and draw appear in the same function.

Confirmed with both `--target spirv` and `--target wgsl` on 2026-09-22.
The emitted `composition.rs` contains the evidence; no GPU run is required.

The same problem occurs with tinyporto's unchanged `tinyporto_frame`:

```powershell
wyn build --graphics -O --target-double rust-wgpu --target spirv wyn/main.wyn -o tmp/rust-port/main.spv
```

Its Rust host performs the four draws but omits world updates, visibility, and
postprocessing. It does not return the five persistent state buffers declared
by the source tuple. Calling the synthesized functions separately cannot restore
the authored compute/draw interleaving through the public root function.

Relevant compiler paths:

- `wyn-core/src/tlc/stage_extract.rs`: multiple compute operations receive
  synthesized entry names (`__compute_N`); a single compute uses the root name.
- `wyn-core/src/egglog/publish.rs`: compute stage owners and result ownership use
  those synthesized names, while graphics stages use the original root.
- `wyn-host/src/program.rs`: `Program::new` groups operations by published owner;
  `prepare_entry` collects results by that same name.

The repair must preserve source-root ownership and source result identity through
stage extraction, including outputs that are internal to the frame versus those
actually returned. It should not rely on parsing generated names in tinyporto.
