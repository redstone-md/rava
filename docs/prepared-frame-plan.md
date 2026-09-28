# Immutable prepared frame plan

The symbol-correct profile locates repeated descriptor work at the prepared
execution boundary. Cache the local-slot plan and return offset alongside each
successfully linked function, keyed by its immutable Code identity. This is
metadata memoization, not execution-history eligibility.

Keep one parser and packing implementation for raw compiled arguments and
interpreter Value locals. Preserve reference forwarding, category-2 padding,
zeroed inactive slots, frame roots and exception exits. A failed link must still
remain retryable. Native-backed and class-initialization guards remain live.

Validate mixed descriptors, more than 64 arguments, pooled buffer clearing,
the AOT library suite and prepared release dispatch/GC probes. Then prepare
fresh objects for the exact release executable and compare full application
wall to the original 27-30 second baseline. No performance gain or sub-five-
second completion is claimed until measured; full no-adapt acceptance remains.

The implementation passes all 72 AOT-selected library tests (one ignored),
including a reusable plan with 80 integer arguments, wide integer/NaN payloads,
arrays of wide primitives, an instance receiver and dirty pooled storage.
Existing raw-versus-Value local-slot tests still pass. Argument boxing was
checked to use only the host-side box table, so the immutable cache borrow ends
before any guest execution can rehash the cache.

Release SHA256:
`965A2F52445562E1525EB083E8CFDB8DDA59FB0373E373F2BE8A89319763EFD4`.
Direct dispatch/epoch/GC validation passes at
`%TEMP%/rava-aot-dispatch-da51d5077bec4152b6d5c92174de2165`.
Literal identity, initialization, GC and configuration rejection pass at
`%TEMP%/rava-aot-literals-9ccc7e8af5934a609c2f6e62fc5ee522`.
The full 39-probe Java differential suite passes with expanded prepared
constants, JIT disabled and both optional caches enabled. Its matching archive
and frozen executable are in
`%TEMP%/rava-frame-plan-server-6f636cbf54264ad3829a469c8938dd6b`.
Sequential fresh-world boots are complete:

| Mode | Wall s | CPU s | Peak RSS bytes | Artifact suffix |
| --- | ---: | ---: | ---: | --- |
| Prepared, JIT off, both caches on | 139.206 | 136.4375 | 1093124096 | 1fff2d91037d4d489d5475da208d04a4 |
| Ordinary JIT, no prepared directory | 24.842 | 27.03125 | 1155420160 | fb558e07eb3d4740a0d0a0999355afc6 |

Artifacts are `%TEMP%/rava-product-boot-<suffix>`. No diagnostic instrumentation
or snapshots; quiet performance counters were unavailable. The prior executable
measured 140.774 s prepared and 24.429 s ordinary JIT. These individual runs do
not demonstrate a meaningful speedup from frame-plan caching. Prepared execution
remains severely slower than the original 27-30 s baseline and the sub-five-
second objective remains unmet. Do not make further descriptor micro-tuning the
principal strategy; eliminating full runtime transitions between compiled
methods is still necessary. Full product workloads beyond boot/semantics remain
unverified, including the previously timing-out ReloadBench.
