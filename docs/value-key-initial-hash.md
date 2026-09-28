# Value-key initial hash experiment

2026-09-06, baseline b311c721. The candidate stored the FNV offset-basis hash
alongside each memo span, calculated when its bytes were inserted. A fold with
that exact seed returned the cached value; other seeds retained the byte loop.
Overwrite, epoch reset and entry-cap reset kept bytes and hash together. This
doubled Span from 8 to 16 bytes and also charged hashing to shared render-memo
inserts. The existing GC-epoch memo policy was preserved, not newly validated
as a general mutable-object caching policy.

Evidence: `%TEMP%/rava-key-initial-hash-b25e587dc2ff441b939dd67183cfc422`.
Frozen executables, initial/final patches, build, semantic, product and timing
logs retained. Control SHA256:
`05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.
Candidate SHA256:
`934662D117724F9E86117B70BC9F69771AECE21A3EAE5DFF32AF8F2A04715B30`.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 26.932 | 44.687500 |
| null1 control | 27.399 | 45.625000 |
| candidate0 | 27.190 | 45.765625 |
| control0 | 27.983 | 46.171875 |
| control1 | 27.457 | 45.796875 |
| candidate1 | 26.487 | 44.968750 |
| confirmation control | 27.122 | 44.421875 |
| confirmation candidate | 27.215 | 45.640625 |

Initial paired deltas -0.793/-0.970 seconds prompted further validation.
Null spread was 0.467 seconds; initial paired controls drifted 0.526 seconds.
The confirming pair reversed sign (+0.093 seconds), with candidate CPU higher
by 1.21875 seconds. This does not establish a consistent wall reduction.
All runs used fresh worlds and first-entry JIT/cap16384 without instrumentation.
Actual hashes and completion were verified. Initial six launch/Done intervals
were checked for nonoverlap; confirmation runs executed sequentially. Both
confirmation servers were alive without the fatal-server log marker ten seconds
after Done, before their owned processes were terminated.

Validation: existing MapAccessSem matched 11 lines without exclusions; it does
not prove the new cached-fold path. Four existing memo tests passed, with the
fold test changed to call actual put/fold across changed bytes, epochs, empty
strings and four seeds. No new test was added. All five existing product hashes
passed: ChunkFill, RingScan, RingLane, RingLaneInstance, NbtSave. The test-only
edit followed the frozen release build and did not change its runtime code.

The experiment, including its test edit, was reverted after confirmation.
Shared release restored to the control. Do not repeat this enlarged-span eager
hash design as an established win; the profile names work, not guaranteed savings.
The full wall<5s/no-adapt/no-snapshot goal remains unmet.
