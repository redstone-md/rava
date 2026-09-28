# Prepared frame buffer reuse

The current diagnostic estimates argument packing at 1.323 seconds of a
30-second bootstrap slice (4.41%). The existing packer allocates a new Vec on
every prepared entry. Reusing inactive frame buffers may remove part of that
cost; this estimate is not a promised whole-boot speedup.

RAVA_AOT_CELL_POOL is experimental and off by default. The per-VM pool retains
at most 64 buffers of at most 1024 cells each (512 KiB cell capacity). Taking
removes ownership from the pool before any nested guest call. Return only after
the frame's roots are popped and its result is decoded. The pool stores no live
GC roots, and its maintenance cannot invoke guest allocation or collection.

Packing clears the vector length, overwrites every argument slot and zeroes
the rest of the used frame, including category-2 padding. Old capacity outside
the new length is never registered as roots or read as locals. Oversized buffers
and excess inactive buffers are released. No execution eligibility changes.

Validate nested ownership, memory bounds and slot clearing in unit tests, then
the real prepared dispatch/GC probe with CellPool enabled. Compare full boots
on/off using one frozen executable and matching prepared objects; keep builds
out of active timed product runs. The five-second goal remains unchanged.

All 67 AOT-selected unit tests pass (one ignored). The release dispatch probe
with both CellPool and ResolvedCache enabled matches Java, confirms direct AOT
entries and cache invalidation, and passes with a 64 KiB nursery and actual GC.
Evidence: `%TEMP%/rava-aot-dispatch-4d4fbf9a5cc740d09c726562a7eb3629`.
Release SHA256:
`F7E35199145BF8CCB924809376B250E1A3BE36CF5E65721E175833601F285326`.
Build/test logs: `%TEMP%/rava-cell-pool-build.log`,
`%TEMP%/rava-cell-pool-tests.log`, `%TEMP%/rava-cell-pool-dispatch.log`.

The full experiment directory is
`%TEMP%/rava-cell-pool-server-77a4227570af45b4a768abe293474df8`.
The pipeline completed: 30,462 classes, 292,541 compiled methods, 56,526 refusals;
all 38 semantic probes match Java with the pool enabled. Resolved-target
memoization is enabled in both timing arms; JIT is disabled and each boot uses
a fresh pristine world without snapshots or diagnostic instrumentation.

First sequential pair, off then on:

| Pool | Wall to Done | CPU | Peak RSS bytes | Boot artifact suffix |
| --- | --- | --- | --- | --- |
| Off | 146.718 s | 144.656250 s | 1060663296 | 6aa631c3fc164ef7af820a9018414104 |
| On | 143.038 s | 140.703125 s | 1076342784 | 610b5c31e24046aeafa386466e643920 |

Artifacts are `%TEMP%/rava-product-boot-<suffix>`. The apparent 2.51% wall
improvement is preliminary. Quiet-box performance counters were unavailable.
The reverse-order pair completed with the same executable and objects:

| Pool | Wall to Done | CPU | Peak RSS bytes | Boot artifact suffix |
| --- | --- | --- | --- | --- |
| On | 146.814 s | 140.750000 s | 1209970688 | fbc098eeb4e44cbc86238b209119a29f |
| Off | 147.193 s | 143.718750 s | 1053446144 | 291b18f194d14650b2379bb4e175130e |

The reverse pair improves wall by only 0.26%, while CPU improves by 2.07%.
The first pair's CPU improvement is 2.73%. Both pairs favor reuse on CPU, but
the wall benefit is small and variable and the reverse-on RSS is higher. This
supports retaining the experiment for further work, not enabling it by default
or treating it as the path to five seconds. The reverse-on guest Done timer
reports 5.377 s; that is not the process-start-to-Done wall above.
