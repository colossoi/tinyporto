# Mapped array indexed in a loop emits invalid SPIR-V

**Status: fixed in the installed compiler.** Rechecked on 2026-09-08: both
`wyn build` and `spirv-val` exit successfully. The original failure is recorded
below, and the source remains as a regression reproducer.

`mapped_array_loop.wyn` is a standalone four-line reduction of the nonconstant
struct-index validation error encountered in the stroke-capture regression fixture.
It has no imports, graphics operations, or host inputs. Expected output: `[1]`.

From the repository root:

```sh
wyn build --graphics --max-warnings 0 repro/mapped_array_loop.wyn -o /tmp/mapped_array_loop.spv
spirv-val /tmp/mapped_array_loop.spv
```

Before the fix, the compiler reported a successful build (exit 0), but validation failed
(exit 1):

```text
error: line 154: The <id> passed to OpAccessChain to index '89[%89]' into a structure must be an OpConstant.
  %98 = OpAccessChain %_ptr_Function_int %96 %89
```

Replacing `map(|i| i, iota(2))` with `[0, 1]` produces SPIR-V that passes validation.
This isolates the failure to the mapped-array case; it does not establish the
compiler's root cause. No GPU or application driver is needed to reproduce it.

Original failure verified with the then-installed `/Users/gmiller/.cargo/bin/wyn` and
`/usr/local/bin/spirv-val` on 2026-09-08. This reduction reproduces the fixture's
nonconstant struct-index error, not its earlier undefined-ID diagnostic.
