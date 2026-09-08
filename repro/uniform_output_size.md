# Uniform-sized output has no allocation domain

`wyn build --graphics` succeeds, but Tiny Porto's driver build panics at
`driver/build.rs:342`: `same_as_dispatch needs a buffer-derived dispatch`.

The dependency-free reproducer, `repro/uniform_output_size.wyn`, renders one
triangle and maps its pixels into an array of `width * height` elements, with
both dimensions supplied through a uniform record. It uses the three-index
vertex callback interface.

```sh
wyn build --graphics repro/uniform_output_size.wyn -o /tmp/uniform_output_size.spv
```

Inspect `/tmp/uniform_output_size.json`. The compute output has:

```json
{"kind": "same_as_dispatch", "elem_bytes": 4}
```

Its only producing stage has:

```json
{"kind": "fixed", "x": 1, "y": 1, "z": 1, "explicit": false}
```

There is no domain-derived stage or buffer-derived dispatch. In the driver's
`codegen_pipeline`, `output_domain` therefore returns `None`; its fallback,
`dispatch_input`, finds no input-derived length and panics. The descriptor does
not provide the driver with the uniform-dependent output capacity.

Expected: the host can determine an allocation of `4 * width * height` bytes.
The compiler must expose that logical length, and the driver must support its
representation. A fixed physical dispatch can be valid for serial execution;
it does not imply that the output has one element. Do not fix this by allocating
one element or hardcoding Tiny Porto's viewport dimensions.

Control: replacing the two uniform dimension expressions with `64i32` and
`32i32` allows the compiler to expose a fixed output domain. The reproducer does
not need GTAO, prop culling, state updates, or the earlier tuple workaround.
