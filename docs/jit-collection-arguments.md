# Bounded native collection arguments

The current primary-thread allocation profile identifies fu_fastpath among
candidate allocation callers. Source confirms fresh argument vectors in the
native Map and Optional arms. fu_map_op admits at most two reference arguments;
the Optional reader list admits at most one.

Move the two bridges into small non-inlined methods with fixed local arrays,
preserving the existing string canonicalization, NativeArgsGuard, operation
dispatch, and handled/unhandled return distinction. Keep the large fu_fastpath
frame from acquiring the array storage. No argument buffer escapes the call and
no new cache, profiling counter or eligibility rule is introduced. New files
are below 500 lines; frontend state/animation requirements do not apply.

Validation plan: existing differential collection/exception coverage and full
semantic suite, then null-first interleaved boots and all five product hashes.
The extra helper call can offset allocation savings; retain only on evidence.
The full wall <5 s, no-adapt, no-snapshot objective remains open.

## Result: not retained

All 46 semantic probes and all five product hashes passed. The uninstrumented
null-first comparison completed successfully:

| Run | Wall, seconds | Process CPU, seconds |
| --- | ---: | ---: |
| Null control 0 | 26.054 | 43.609375 |
| Null control 1 | 26.832 | 44.625 |
| Candidate 0 | 24.874 | 41.6875 |
| Control 0 | 26.322 | 44.78125 |
| Control 1 | 26.384 | 44.0625 |
| Candidate 1 | 26.357 | 44.8125 |

The paired savings were 1.448 s and 0.027 s, against a 0.778 s control-null
spread. This does not establish a repeatable improvement. Runtime changes were
reverted and the frozen control executable restored after product gates ended.
Quiet runtime counters were unavailable; no claim about changed JIT coverage is
supported by these timings. No reload comparison was run for this rejected patch.

Evidence directory (under the Windows temporary directory):
`rava-jit-collection-args-b2fb60e08ed34c4b8fd8b1c24fa394f4`.
It contains `candidate.patch`, frozen binaries, semantic evidence, timing logs,
and product results. Temporary evidence is not a durable repository artifact.

Candidate SHA-256:
`F98FB3AC2B655C3881CF6A390228D4943E04FC7F275DC7B2DC6321673903A983`.
Restored control SHA-256:
`7C7221E480F4E421DACC613DC1DDB6224B201626DD5CC61202324C4CE10DFEAD`.

Product hash checks: ChunkFill, RingScan, RingLane, RingLaneInstance and NbtSave
all exited successfully without timeout. Their separate workload times are not
the server boot wall metric and do not certify the full performance objective.
