# Uniform-sized maps allocate correctly but dispatch only one workgroup

Verified with the installed Wyn compiler on 2026-09-08. This is the remaining
execution-side failure of `uniform_output_size.wyn`; output allocation now has
a valid host expression, but dispatch still defaults to one workgroup.

From the repository root:

```sh
wyn build --graphics --max-warnings 0 repro/uniform_output_size.wyn -o /tmp/uniform_output_size.spv
```

The compute stage reads a rendered image and writes one element per pixel. Its
output length is correctly described as `host_expression(width * height)`, but
its dispatch is:

```json
{
  "workgroup_size": [64, 1, 1],
  "dispatch_size": {
    "kind": "fixed", "x": 1, "y": 1, "z": 1, "explicit": false
  }
}
```

Expected: a dispatch covering the uniform-derived map domain, such as
`[ceil(width * height / 64), 1, 1]`, or equivalent grid-stride shader execution.
The current application behavior demonstrates incomplete coverage.

## Tinyporto impact and runtime confirmation

At 1280 × 800, `tinyporto_frame__compute_3_dispatch_1` should write all 16,000
coarse-depth history cells using 250 workgroups. Its descriptor requests one.
Readback shows depth values in the first cells but zeroes in the middle and tail.
Those zeroes are interpreted as nearer occluders, rejecting all ground pavers.
The AO stage (`dispatch_0`) also receives one workgroup instead of the 16,000
needed to cover 1,024,000 pixels.

A temporary diagnostic override dispatched the two stages using their proper
pixel and tile counts. With the original culling predicate enabled:

- Prop instances increased from 1,718 (walls only) to 5,336: 3,618 pavers restored.
- Middle and tail history cells contained scene depth instead of zeroes.
- Independently bypassing coarse-depth culling also produced 5,336 instances.

The diagnostic overrides were removed. No culling bypass or hardcoded dispatch
workaround is retained in the application. The local-variable cleanup preserves
the earlier screenshot, including this pre-existing missing-paver failure; the
exact introducing commit has not been isolated.
