# First-entry product validation

Configuration: first-entry JIT, cap 16384, early queue reservation, standard
register allocation, standing memory/feature flags, no prepared objects or
snapshots. Executable SHA-256:
`078DE86F2C7E04D93B50EB1AE0EA980494602631B30DC1795F7878CDF9452D20`.
The goal remains full process-to-Done wall below five seconds and no adaptive
eligibility. Product validation does not replace that performance requirement.

Evidence:
`%TEMP%/rava-first-entry-products-772743cc54444ba99ab96213fd5bf435`.
The binary is the verified frozen release; intervening source changes are
comments/documents and the independent LateNewSem Java probe. The reload
harness's `AllowStale` was used deliberately for this exact executable.

## Reload null pair

Both fresh processes exited zero without timeout. Each cold and warm resource
set had 1470 recipes, 1584 advancements, 625 tags, and hash
`c956377e19071d7a`. First/second cold reloads were 5901/5853 ms, warm reloads
5618/5581 ms. Total process wall was about 34.93/34.96 seconds, including
bootstrap and both reloads; these are not full-server startup times.

The two arms differ only by an unused null flag. With one result per arm,
the harness's printed DISJOINT labels have no statistical meaning; differences
below one percent here are null variation, not a speedup. Raw results are in
`reload/run_1_A.log`, `reload/run_2_B.log`, and `reload/runs.jsonl`.

Five additional product gates are run sequentially from freshly compiled Java
sources in an isolated temporary directory. Their copied rava.toml and explicit
environment match the fixture configuration. Every result requires exit zero,
no timeout and the expected content hash; individual 600-second timeouts are
failures, not timings. The gate runner preserves stdout/stderr and source/binary
hashes. Their terminal results remain pending until inspected.

## Completed product gates

All five processes exited zero, without timeout, and matched their required
hashes:

| Gate | Arguments | Hash |
| --- | --- | --- |
| ChunkFill | 1 64 | 8790f04ace726ecf |
| RingScan | 8 | 37539c6c375d0c3 |
| RingLane | 8 | 37539c6c375d0c3 |
| RingLaneInstance | 8 | 37539c6c375d0c3 |
| NbtSave | 64 24 | ae44a34b42766bd4 |

This replaces the pending gate status above. The raw `gate-results.json`,
per-gate `.out`/`.err`, fresh sources' hashes and executable hash are preserved.
These runs establish results for the exercised inputs; they do not independently
prove that every parallel lane ran, nor whole-runtime no-adapt compliance.
Together with the 41 semantic probes and repeated full boots, they support
using early queue reservation by default inside explicit first-entry mode.
The explicit environment override keeps both on/off behavior available.

## First-entry default integration

Early reservation now defaults on when `RAVA_FIRST_ENTRY_JIT` is present;
explicit `RAVA_BG_JIT_EARLY_RESERVE=0` disables it. Ordinary threshold mode
retains its prior default. This selects the same branch exercised by the
41-probe and product checks above; it changes no permit or queue algorithm.

Release build passed. New executable SHA-256:
`858AEA5B921A1E98173681B27F625F1BBB6CB2C95D73C007B56DE37734CF16F4`.
Full uninstrumented first-entry boot with cap 16384 and early-reserve unset
reached Done in 25.837 seconds, CPU 43.734375 seconds:
`%TEMP%/rava-product-boot-dbaa67a270434fffa9c4d7b14dfed715`.
This is above the preceding executable's best 24.5-24.8-second runs; do not
claim a new measured improvement from the integration run. The product and
41-probe certification above was on 078DE86F with the equivalent branch
explicitly enabled, not repeated wholesale on this new executable. The new
build's full boot is the direct verification of the default selection.
