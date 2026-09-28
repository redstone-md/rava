# Prepared calls before the general dispatcher frame

The experiment was removed after the reverse-order full-boot comparison failed
to demonstrate a repeatable improvement. The evidence below is retained to
avoid repeating this direction. `RAVA_AOT_EARLY_CALL` is no longer implemented.

The symbol-correct profile attributes host self-time to the general compiled
call dispatcher. Successful prepared calls currently enter its large native
stack frame before returning through the direct prepared arm.

An experimental `RAVA_AOT_EARLY_CALL` entry wrapper tries the same prepared
dispatcher before entering that frame. A declined call enters the original
body with only that already-attempted arm skipped. Instrumented and GIL-free
execution retain the original order and guards. No target eligibility,
receiver proof, initialization, root envelope or exception contract changes.
The shared fallback body is kept out of line, and no fallback outcome is cached.

Validate direct dispatch/GC, the Java semantic suite and same-binary full boot
comparisons before promotion. The switch is off by default. The target remains
wall below five seconds without snapshots or adaptive eligibility; the baseline
is the original 27-30 second startup, not the slower prepared experiment.

Release SHA256: `A41D569FB83624CB3803473D63423F3D30ED60538C65CF6E9B27818DA59DB89C`.
The 74 AOT-selected library tests pass, with one ignored. Explicit early-call
dispatch/epoch/GC validation passes at
`%TEMP%/rava-aot-dispatch-b9af8e02899e415db9a7f858fe85e41d`, and asynchronous
progress with worker allocation/GC passes at
`%TEMP%/rava-aot-progress-2a1779c7c4754db683b537943682ee23`.

The matching full archive has 311,894 compiled methods and 37,186 refusals.
All 40 Java differential probes pass with the early-call switch enabled.
Evidence: `%TEMP%/rava-early-call-server-924c358b19f74b4b85167bc95ff7896f`.
Four sequential fresh-world boots used the same executable and prepared archive,
JIT disabled, both optional caches enabled, port 25599 and affinity 0x0FFF.
No profiling or snapshots were enabled; quiet counters were unavailable.

| Order | Early call | Wall s | CPU s | Artifact suffix |
| ---: | --- | ---: | ---: | --- |
| 1 | On | 139.143 | 136.390625 | 8131970669ae42f4a30d057d0f9fbb9e |
| 2 | Off | 142.846 | 140.3125 | b5b9dae535d84ba09a590bd565ce376c |
| 3 | Off | 141.522 | 138.9375 | 95216dbd2837405bb0a804991262e2e7 |
| 4 | On | 142.724 | 139.71875 | 9c83749c6c574a368b9c852fb5305be2 |

Artifacts are `%TEMP%/rava-product-boot-<suffix>`. The first pair's apparent
2.6% wall benefit reverses in the second pair. Neither early-call observation
improves the previous release's 138.584 seconds. This is insufficient evidence
to retain another execution path; source returns to the validated pre-experiment
dispatcher. The 27-30 second original baseline and sub-five-second target remain
far out of reach for this prepared configuration.

## Limited inline-field control

On the same frozen experimental executable, ordinary JIT without prepared
objects completed in 25.166 s with `RAVA_NO_INLINE_FIELDS=1` (CPU 27.09375 s),
then 25.385 s without that flag (CPU 27.578125 s). Artifacts:
`%TEMP%/rava-product-boot-9ae69d3e8a5548a1a7b6b126526d3873` and
`%TEMP%/rava-product-boot-7c260fbcdf9244ea9e010fb5f49bdd5d`.

This flag disables the unresolved arena-slot probe and FieldBlob reference-kind
family, including associated slot-cache publication. It does not remove all
constant-offset field accesses or field promotion. The single pair supports no
large startup benefit attributable to this specific family; it does not bound
the cost of all field operations or prove that all field inlining is irrelevant.
No field code was changed on this evidence.

## Restored runtime

After removing the experiment, `git diff 20e6df51 -- crates bench` is empty:
runtime and probes exactly match the previously validated progress-fix source.
The rebuilt release has SHA256
`6AA554379870637E131AED4C8E1E6AF88A9A611485F4193C35845572300C7B39`.
The prepared asynchronous/GC probe passes again at
`%TEMP%/rava-aot-progress-4df2b40027c047efb17ffe78434fb5a6`.
An ordinary-JIT fresh-world boot completes in 25.148 s (CPU 27.5 s), artifact
`%TEMP%/rava-product-boot-4f556bd0ad624eefb0fdecaed164506c`.

This rebuilt executable has no newly prepared full application archive or
prepared full-boot result; the prior progress-fix prepared boot remains 138.584 s
on its own frozen executable. Do not mix either archive with this executable.
No sub-five-second or full no-adapt completion is established.
