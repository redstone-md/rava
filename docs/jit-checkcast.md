# Enforce compiled checkcast

Early lambda compilation exposed a missing JIT operation: opcode 0xc0 was an
empty arm, while the interpreter enforces casts. Implement a callback using the
interpreter's `checkcast_reject`, keep all live references rooted, and branch
immediately through the JIT epilogue on failure. A failed cast must not execute
later stores. Null casts succeed; unknown compatibility retains interpreter
behavior rather than reusing the distinct instanceof cache.

The cast is GC-capable and counted as a callback. Exclude it from GC-leaf,
field-invariance and forwarding-tail proofs. Reuse normal import registration;
append the archive symbol without renumbering existing symbols. The new callback
is encapsulated in one small module, with no new mutable global state. New files
stay below 500 lines; frontend rules do not apply.

Validation plan: compare JitCastSem against Java and the preserved broken
baseline, including rejected-cast side effects, identity, null, interface and
array cases. Then repeat the existing differential suite and the early-lambda
experiment. Other early-lambda exception failures require separate diagnosis;
this patch does not claim to fix them. Full startup below five seconds, no-adapt
and no snapshots remains the objective.

Validation completed on release executable SHA256
`E5727F429C5B476ED4193E4B606E41CC2FF3402A089DDC6A2BB00F8486692CBF`:

- All 42 differential probes match Java with existing declared divergences.
- JitCastSem rejects all 4096 invalid object casts and all 4096 primitive-array
  casts; only successful object casts increment the observable counter.
  The preserved 858AEA5B baseline fails these checks once its unchecked JIT body
  takes over (rejected=313, after=7879, bad-array=144 in the recorded run).
- After extending the new probe with repeated fresh receiver allocation, it
  passes again with a 64 KiB nursery and an observed minor GC. This is evidence
  of collection during the test, not proof of every possible safepoint interleaving.
- All five product hashes pass: ChunkFill 8790f04ace726ecf;
  RingScan/RingLane/RingLaneInstance 37539c6c375d0c3;
  NbtSave ae44a34b42766bd4.
- Both cold/warm Reload runs retain hash c956377e19071d7a, 1470 recipes,
  1584 advancements and 625 tags. This was a same-binary null pair, not an A/B
  speed result. The deliberate stale-source override covered later comments
  and Java test edits; the frozen executable hash is recorded in the evidence.
- Fresh full server boot reached Done in 26.663 seconds, CPU 45.453125 seconds,
  artifact 233e044f0dd148dc84b328b56133abb1. Quiet counters were unavailable.

The test explicitly records the interpreter's preexisting leniency for an
Object[] source cast to a reference array as `erased-array=`. That line remains
a declared Java divergence, and this patch does not repair missing native
component descriptors. An exploratory Integer[] case also reached Unknown;
the independent primitive-array rejection tests a decidable incompatible type.
Do not describe this as complete JVM cast compatibility.

Artifacts, test logs, preserved binaries, product results and Reload evidence:
`%TEMP%/rava-jit-checkcast-4ad02ed0e1f14d2eb45c54fc8668e9d6`.
The 512/4096 lambda-threshold experiment remains rolled back. Reapply its saved
patch only for the next validation; other lost-exception failures remain open.
This is a correctness prerequisite for expanding JIT coverage, not a startup
speedup or completion of the five-second goal.
