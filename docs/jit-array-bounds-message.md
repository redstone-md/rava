# JIT array bounds messages

The synchronous first-entry lambda experiment exposed positive and negative
array bounds messages containing only an index. The native emitted fault bridge
used idx.to_string(), while the callback load/store paths already used the
authoritative index-and-length formatter.

Resolve length from the existing tagged descriptor or arena primitive array
decoder, then use array_bounds_pending. Preserve the pending-exception early
return and the null/invalid-shape arm. This changes only the cold fault callback,
not JIT eligibility or compilation timing. It is a correctness prerequisite for
expanded compilation, not evidence of a boot speed improvement.

JitBoundsSem repeatedly exercises valid and invalid primitive loads, primitive
stores and reference loads, checking messages and absence of post-fault effects.
Validate against the frozen control and Java, then run the full semantic suite.
The full wall <5 s, no-adapt, no-snapshot objective remains unfulfilled.

## Validation

Release build with next-gen-gc succeeded. Frozen control returned only 42 of
12288 expected messages in JitBoundsSem; candidate returned all 12288. Both
reported 12288 successful post-access increments and the expected stored value.
All 47 semantic probes matched java -Xint with their existing declared
divergences. No new divergence was declared.

A separate RAVA_JIT_STATS run also passed: read and write each executed through
JIT 8022 times, reference 7989 times; each compiled once without compile failure.
The main method remained interpreted. This directly exercises the compiled
fault bridge, but does not establish opt-in arena-array or off-thread fault
coverage. Existing worker allocation/root behavior is unchanged by this patch.

ChunkFill, RingScan, RingLane, RingLaneInstance and NbtSave each exited 0 without
timeout and passed their product hashes. A fresh-world uninstrumented boot
reached Done in 26.957 s, process CPU 43.953125 s. This is a single observation,
not an A/B speed claim; quiet performance counters were unavailable.
Boot directory: `rava-product-boot-9e85794843cc41efbfbb1ea6fb3fc2c2` under TEMP.

Evidence directory under TEMP:
`rava-jit-bounds-08e3320d442445ca9a5760697e21e2e2`.
Contains build, control-probe, full-suite, JIT-stat, product and boot logs plus
frozen binaries. Temporary evidence may be removed outside repository control.

Candidate SHA-256:
`27DB068A205EF50F1C4242CC40B53195B8720235961B1B26B2A3D0C402A9E22C`.
Control SHA-256:
`7C7221E480F4E421DACC613DC1DDB6224B201626DD5CC61202324C4CE10DFEAD`.

FIRST_ENTRY_JIT remains narrower than whole-runtime no-adapt. Flat-served
lambda bodies have no compile request by default; `RAVA_FLAT_TIER=1` enables a
fixed first-use request, and workers still leave compilation to the owner.
This correction removes a known message defect from expanded JIT execution;
it does not remove those broader policy dependencies.
