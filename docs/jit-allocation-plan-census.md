# Allocation plans under first-entry JIT

The objective remains wall below five seconds without adaptive admission or
snapshots. First-entry JIT currently takes about 39 seconds versus ordinary
JIT's approximately 27 seconds. Completed compiler scopes end well before Done.

Tracer experiment: `RAVA_TRACE_NEW_PLANS` reports each resolved `new` plan using
its constant-pool site and declared method identity. Routes distinguish arena,
permanent handle, unstable, and the existing opt-in partial-layout case. The
observer does not query or initialize any additional classes and does not alter
route caching. Names are diagnostic identifiers only. Logging is per planning
attempt, so duplicate sites must be reported and distinguished from unique sites.

Compare ordinary and first-entry runs on the same release, fresh fixtures and
standing settings. Compare common sites separately from the expanded first-entry
population. A difference in planning eligibility is not a dynamic allocation
count or evidence of saved wall time. Unstable includes uninitialized classes,
load failure and incomplete hierarchy; do not equate all unstable routes with
class initialization. These diagnostic runs are not production timings.

## Current comparison

Release SHA-256:
`1A7F9A5CABB96290A841DC1A28AB27048B9F41B9BFD83BF51E9B2983C37F2C32`.
Release build passed. Both instrumented full boots reached Done:

- Ordinary: `%TEMP%/rava-product-boot-d632891d77944657bf08700ad7c0537d`,
  27.703 s wall, 29.609375 s CPU.
- First entry: `%TEMP%/rava-product-boot-57ef2ce902b3464f92aeb29b526d73af`,
  42.449 s wall, 48.609375 s CPU.

These instrumented times are not a speedup comparison. Quiet counters were
unavailable. Current uninstrumented first-entry reference remains 38.986 s.

Parser, raw comparison and complete common-site CSV:
`%TEMP%/rava-new-plan-census-cf00476c1f9848f786c48c96674e35e8`.
Ordinary has 473 records / 416 unique method+CP sites; first entry has 3238
records / 1811 unique sites. A site's route set preserves repeated attempts
rather than silently choosing the final record.

| Ordinary route set | First-entry route set | Common sites |
| --- | --- | ---: |
| arena | arena | 32 |
| arena | unstable | 73 |
| arena | unstable, arena | 3 |
| handle | handle | 14 |
| unstable | unstable | 86 |

There are 208 common sites. Thus the early-plan loss is observed at 73 sites,
with another three changing during repeated planning. It is not yet known which
plans reach installed machine code or how often these sites execute. The records
cover plan construction, including later compiler refusals. They cannot prove
that these losses account for the entire roughly 12-second runtime difference.

Next tracer: a resolved allocation slot whose fast path becomes available after
class initialization and complete-layout publication, while keeping the existing
slow allocation path before then. This must preserve representation, exception
and GC contracts. Eligibility must derive from resolved class structure, without
hotness, name-based selection or retry-after-outcome admission. First inspect
existing allocation callback caches to avoid duplicating their mechanism.

## Existing late allocation and structural publication experiment

The proposed late-bound machine-code slot already exists in `objects.rs` and
`emit_inline_arena_new_late`. Therefore 73 missing compile-time plans do NOT
prove 73 permanent callback paths. The current late publisher instead inspects
the first returned object's header, nursery location and zero cells, then makes
that site's verdict permanent. This observed-result decision is unsuitable as
the basis of the requested no-adapt policy.

The opt-in `RAVA_STRUCTURAL_NEW_SITE` experiment reuses the existing slot and
machine-code emitter. It derives publication from the authoritative
`alloc_route_cache` entry produced by `new_object_value`, without inspecting the
returned object's shape. Only `NewRoute::Arena` with `pinned: false`, complete
class initialization (`clinit_done`), valid nonzero class ID and bounded field
count publishes a shape. Handle, map and pinned routes retain the callback.
Missing routes and unfinished initialization remain unresolved. No additional
class loading or initialization occurs after allocation, preserving the unrooted
result's no-GC interval. The old publisher remains the unset control.

This does not certify the entire runtime's no-adapt compliance: existing birth
routing and other admission policies retain their own rules. It specifically
removes observed allocation contents from the experimental late-new publication.
The goal and full performance acceptance criteria are unchanged.

Structural publisher build passed, SHA-256
`078DE86F2C7E04D93B50EB1AE0EA980494602631B30DC1795F7878CDF9452D20`.
All 40 differential semantic probes match Java -Xint with first-entry JIT and
`RAVA_STRUCTURAL_NEW_SITE=1`, subject to the suite's declared divergences.
`RAVA_INLINE_NEW_TRACE=1` confirms 85 publications in CompSem. This establishes
that the modified branch ran; it is not machine-code allocation coverage.
Evidence root is recorded in `%TEMP%/rava-structural-new-current.txt` and contains
`binary.json`, `sem.log` and the preserved `CompSem.rava.err`.

Uninstrumented same-executable first-entry controls, sequential candidate then
legacy, both full Done with pristine fixtures and no prepared archive:

| Publication | Wall | CPU | Product-boot suffix |
| --- | ---: | ---: | --- |
| Structural | 37.333 s | 45.296875 s | 382254c9b4b94530a12f6920d26b077e |
| Legacy | 38.636 s | 46.53125 s | 0d10bee8c3444d2bbae9e5d92497d118 |

These directories are under `%TEMP%/rava-product-boot-<suffix>`. Quiet counters
were unavailable. This is one pair, not repeated interleaved evidence of a
speedup. The structural mode stays opt-in pending broader validation and repeat
measurements. It removes one observed-result eligibility decision but does not
establish whole-runtime no-adapt compliance or startup below five seconds.

## Reverse-order follow-up and late-new semantics

On the same 078DE86F executable, the reverse-order pair reached full Done:

| Publication | Wall | CPU | Product-boot suffix |
| --- | ---: | ---: | --- |
| Legacy | 38.852 s | 46.1875 s | 1421179555fd4fd09047d9c96afd9f5b |
| Structural | 39.030 s | 47.109375 s | 39d8ccfcccff4a528fe9be0eca18d36b |

Combined order is structural/legacy/legacy/structural. The effect changes sign:
this does not establish a performance improvement. Do not promote the flag on
speed grounds. Its narrower purpose remains eliminating object-observation-based
publication; broader no-adapt compliance and the wall goal remain unmet.

`bench/LateNewSem.java` adds a behavioral regression probe: recursive allocation
during class initialization, inherited state, and a kept reference passed as the
first argument while the second argument allocates. Synchronous first-entry JIT,
structural publication, a 64 KiB nursery and arena tracing match all three Java
-Xint reference lines without declared divergences:

```
init=1:1:2:7
sum=804420000
kept=73:19:2
```

The preserved `LateNewSem.rava.err` includes publication for the Item class and
actual minor collections. Artifacts remain in
`%TEMP%/rava-structural-new-8cef6eae38e84760a5932661700d69ff`.
This behavioral check extends the previous 40-probe run; the complete suite was
not rerun merely for adding the independently executed 41st probe.
