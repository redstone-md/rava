# First-entry fallback invocation census

After rejecting the flat direct-call cache, inspect the remaining general
invocation path rather than another map-layout variation. Existing
`RAVA_JIT_CB_CENSUS` is recorded in `jit_call_body` after direct/native fast
returns and before general argument marshalling. Despite its historical header,
it does not count every JIT-to-host transition.

Run: `%TEMP%/rava-product-boot-2634780d39704777b2849a434d56c4e6`.
Diagnostic executable C6D7D0BE (full hash in
`jit-direct-probe-instructions.md`), first-entry JIT, cap 16384, default early
reservation, fresh fixture, no prepared archive or snapshots. PC trace and
fallback census were enabled; no wall-profile feature. Full Done reached in
27.684 seconds instrumented, CPU 49.515625 seconds. These are diagnostic run
timings, not a performance comparison. Quiet counters were unavailable.

The last printed cumulative snapshot contains 6615267 recorded events:
1610121 static, 698566 virtual, 377030 special, 3929550 interface.
The sampled names merge overloads and call kinds:

| Declared owner and method | Share of sampled fallback names |
| --- | ---: |
| java/util/function/Function.apply | 20.18% |
| net/minecraft/world/phys/shapes/IndexMerger.size | 12.94% |
| java/lang/String.valueOf | 7.03% |
| it/unimi/dsi/fastutil/objects/Reference2ObjectMap.size | 6.98% |
| java/util/Objects.equals | 5.47% |
| java/util/Objects.requireNonNull | 4.51% |
| com/google/common/cache/LoadingCache.getUnchecked | 3.72% |

Sampling is deterministic every 256th event, so periodic correlations can bias
the name distribution. These shares are not CPU time. The owner is from the
caller's constant pool, not necessarily the resolved receiver's implementation.

The report itself has no timestamp. An attempted association with subsequent
PC trace rows produced nonmonotonic block maxima (27893 ms then 26349 ms from
launch). Therefore that association cannot certify an exact pre-Done census.
Use the table as a cumulative diagnostic population from this isolated run,
not an exact startup count. The script's `selected` field in census-window.json
does not certify a pre-Done interval and must not be used for that purpose.
Artifacts are under the directory named by
`%TEMP%/rava-first-entry-crossings-current.txt`.

Next: identify structurally why interface/lambda targets remain on the general
path (resolved body unavailable, representation constraint, unsupported adapter,
or compilation status). The source already contains lambda PICs and chained
getter shapes, so their existence is not a new optimization. Do not route by
the names in this table or equate a declared owner with the actual target.
The full five-second, no-adapt, no-snapshot objective remains unfulfilled.
