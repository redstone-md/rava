# Ordinary JIT and prepared execution reference

The historical 27-30 second ordinary-JIT boot remains the comparison baseline;
improvements relative to a 145-second prepared run do not establish progress
beyond that baseline. On 2026-09-05 a fresh ordinary-JIT boot of the current
frozen release reached Done in 26.529 seconds.

Both measurements use executable SHA256
`1488DF5DB129FA01CB4CE6053DFB1E4EB3EF450C07C5862FE46E11DDF37D45E9`,
the same server configuration and pristine-world input hashes, port 25599,
affinity 0x0FFF, and process-start-to-Done measurement. No snapshots or diagnostic
instrumentation. Shared RAVA flags are JIT_TIER0=1, GSON_NATIVE=1,
M3_BITSTORAGE=1, XMX_MB=4096, PROC_CAP_MB=4096, HARD_MB=3584.

| Mode | Wall seconds | CPU seconds | Peak RSS bytes | Artifact suffix |
| --- | --- | --- | --- | --- |
| Ordinary JIT=1, no prepared directory | 26.529 | 28.625000 | 1167572992 | c196b8aa183b45349dcb9f67f254176c |
| JIT=0, prepared, resolved cache and cell pool on | 145.761 | 141.968750 | 1096568832 | 5978b9edead74bceb2629de7e5566270 |

Artifacts: `%TEMP%/rava-product-boot-<suffix>`. Each contains result.json,
env.json, binary.json and fixtures.json. Quiet-box counters were unavailable;
this is one observation per mode, not a repeated estimate of small effects.
The prepared mode is approximately 5.49 times slower in this comparison.

The ordinary-JIT route is still fast relative to the historical baseline, but
is not no-adapt certification. The prepared compiler/runtime strategy failed
to preserve its performance. Prioritize explaining and removing this gap;
small prepared-only improvements must not replace that comparison. The new
numeric-chain inliner is not in either frozen executable measurement.

The actual acceptance target remains wall below five seconds, no adaptation,
no snapshots, and passing full product semantics. Neither measured mode meets
all these conditions.

## Tree-inlining control

With the same ordinary-JIT binary and setup, `RAVA_INLINE_BUDGET=0` reached
Done in 28.326 s wall, 29.718750 s CPU, peak RSS 1153712128 bytes.
Artifact: `%TEMP%/rava-product-boot-b6ddd59af74d47bb9ebdeff9ed8d2c0e`.
This disables budgeted inter-method tree expansion; it does not disable every
intrinsic or all local code-generation optimizations. The roughly 1.8-second
difference from the prior 26.529-second observation is not a repeated estimate,
but clearly does not account for the roughly 119-second prepared-mode gap.
Small pure numeric-chain inlining is therefore not the principal strategy for
closing that gap. Next isolate prepared execution with ordinary JIT fallback.

## Prepared execution with JIT available

The same frozen binary with prepared objects, both optional AOT caches and
`RAVA_JIT=1` reached Done in 53.338 s wall, 52.734375 s CPU, peak RSS
1495818240 bytes. Artifact:
`%TEMP%/rava-product-boot-af929ccbd14b4fc1b731d881eea80251`.

This is substantially faster than prepared/JIT-off, yet approximately twice
the ordinary-JIT observation. It supports investigating both uncovered-method
interpretation and prepared execution cost. It does not isolate compilation
alone: the global JIT switch also gates the GIL-free task lane in
gilfree_cf/rings.rs, among other runtime paths. Do not attribute the entire
92-second difference to a single cause without further evidence.
