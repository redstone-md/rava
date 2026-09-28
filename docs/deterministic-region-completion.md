# Deterministic worker-region completion

The previous lane vote compared each worker's callback count to one eighth of
the serial pilot's count. This is not a correctness condition: independent
tasks may do different amounts of work. Moreover, pilot counts only advance
with callback instrumentation, so enabling diagnostics changed production
retention/replay decisions.

The coordinator now captures the process-wide abort flag after every scoped
worker joins and before clearing it. `JoinedRegion` retains results only when
the region did not abort and every claimed task returned without an abort.
Missing plans, missing outcomes and caught panics all discard the entire
region. Callback counts remain diagnostic and never select execution.

The old coordinator also initialized classes recorded by worker refusals before
retreating the nursery. Such initialization can allocate, publish statics or
collect, violating the reservation/retirement boundary. The refusal-history
queue is removed. The ordinary serial retry executes lazy initialization after
retirement, in guest execution order. This removes the unsafe ordering; it does
not establish that the previously recorded JsonObject.add crash had this cause.

## Direct reproducer

`bench/LaneWorkSem.java` runs a heavy first task followed by seven short tasks,
all returning the same captured object through read-only map lookups.

Diagnostic setup: JIT/tier0 on, threshold 16, synchronous compilation. Both
binaries returned Java's `identical=8` and exited 0 in all four runs:

| Binary | Callback census | Worker outcome |
| --- | --- | --- |
| Previous | Off | 7/7 retained, floor 0 |
| Previous | On | 0/7 retained, short_work 7, floor 12500 |
| Candidate | Off | 7/7 retained, aborted false |
| Candidate | On | 7/7 retained, aborted false |

The workers made 1-7 callbacks (sum 28); the instrumented pilot made 100001.
The old seven serial replays were selected by this difference alone. The new
decision eliminates those replays. These are execution-path witnesses, not a
boot timing result; elapsed times of these small runs are not speedup evidence.

## Validation and provenance

- Four completion tests pass: heterogeneous work, late region abort, missing/
  aborted outcomes, and worker initialization refusal followed by retirement
  and ordinary serial initialization.
- Eight worker/safepoint tests pass.
- All 26 existing semantic probes pass; the newly added LaneWorkSem passes
  separately under the standing environment (27 total probes checked).
- The five-mode static ABI probe passes with GC poison and a 64 KiB nursery,
  including accepted/rejected resolver diagnostics and actual minor collections.
- All five product hash gates pass with exit 0: ChunkFill `8790f04ace726ecf`,
  RingScan/RingLane/RingLaneInstance `37539c6c375d0c3`, NbtSave `ae44a34b42766bd4`.
- Two candidate ReloadBench processes exit 0 with counts 1470/1584/625 and
  hash `c956377e19071d7a`, including worker abort followed by serial replay.

Candidate reload process walls were 72.243 and 78.700 seconds. A subsequent
baseline control in the same directory/configuration exited 0 with the same
hash in 75.622 seconds. Candidate cold/warm reloads were 13402/12349 and
15387/13780 ms; baseline was 14432/12209 ms. This small sequential check does
not price the change or establish an improvement. It does not reproduce the
historical crash, and cannot establish that crash's cause was removed.

Harness limitation found during review: ReloadBench's former final hash folds
`stem.dataPackResources()` (the first resource set); it does not fold the warm
reload's newly returned resource set. Thus the hash certifies the first result,
not every timed warm result. The benchmark now folds every returned warm set
outside its timed interval and requires identical hashes and counts. The harness
requires all indexed warm-result lines and rejects incomplete certification.

Java and the saved candidate both pass with two warm reloads, all three resource
sets producing `c956377e19071d7a` and counts 1470/1584/625. Candidate process wall
was 101955 ms; cold/warm intervals were 14881/13349/12930 ms. This is a correctness
check, not an A/B price. The parser accepts the actual Java output and rejects
missing warm results, a wrong hash, a wrong count and a duplicate index.
Evidence: `%TEMP%/rava-warm-certified-20260904`.

A one-pair null run through the complete harness also passes (both processes
exit 0 with the required cold and warm digests). Warm intervals are 8263/8213 ms;
one pair cannot establish the null floor or a performance verdict. Raw evidence
is `%TEMP%/rava-warm-certified-ab-20260904` and its sibling `.log`.
The preserved candidate was intentionally used with `AllowStale`: the only Rust
edit after its build was a comment in `gilfree_cf/warmup.rs`.

For multiple warm reloads, validation between iterations can alter later cache
and JIT state. Re-establish the null floor with the new benchmark before pricing
changes. Default `WarmReloads=1` certifies only after the sole warm timed interval;
CPU and process wall include the added validation work in either mode.

Evidence directory: `%TEMP%/rava-region-completion-20260904`.
It contains a copy of main's runtime configuration, with the product tests using
soft heap 4096 MiB, OS cap 4096 MiB and hard cap 3584 MiB. No checkpoint/restore
environment is inherited by these tests. The earlier isolated 8192 MiB setting
is not used for these product gates.
The small lane routing comparison uses the worktree's configuration (copied as
`lane-diagnostic-config.toml`), with the diagnostic threshold/background settings
stated above. Its 8192 MiB soft heap is not a product-timing configuration.

Baseline executable SHA-256:
`1AB5B1B2D303EDA559FEC3F0C6BFD85A55C7AD95EF1E69460DDF62552923BEDF`.
Candidate executable SHA-256:
`529FB86F00B7F77098451C0CEFE25DA84F8FFFE6BC2070281C8961A9C44C3204`.

The full goal remains wall under five seconds with no adaptive eligibility and
no VM snapshot speed path. This fixes two concrete history-dependent decisions;
it does not certify the rest of the runtime as no-adapt or establish a wall gain.
