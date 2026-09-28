# Map access-order correctness

The native get path removed and reinserted entries on every access-order hit.
This repeated key derivation, replaced the stored key identity, and changed the
version even for the last entry. The JIT handle path omitted the reorder entirely;
getOrDefault also omitted it and the legacy JIT lowering treated mapped null as absent.

Use the existing tail rotation with the resolved MapKey. Share the receiver access
policy between native and JIT paths, and gate access-order JIT reads as mutations on
workers. Keep getOrDefault on the normal invocation path, which preserves presence
independently of the returned value. No new class-name eligibility rules are added.

Validation completed on 2026-09-04:

- MapAccessSem matches java -Xint in all four JIT on/off and arena-map on/off modes.
  The old binary diverged on access order and stored key identity.
- All 25 sem_all probes match their Java reference, with existing declared exceptions.
- The worker regression test passes for both legacy and arena-fallback callbacks.
- The 14 loader, capture-thunk and cache tests repaired in the prerequisite commit pass.
- Product release build and explicit-binary pgo_gates pass: chunkfill
  `8790f04ace726ecf`, ringscan/ringlane/ringlanei `37539c6c375d0c3`,
  nbtsave `ae44a34b42766bd4`; all five processes exit successfully.
- ReloadBench completes with 1470 recipes, 1584 advancements, 625 tags and
  `HASH=c956377e19071d7a`. This was a correctness run, not a paired timing experiment.

No wall-time improvement is claimed without an interleaved measurement.
