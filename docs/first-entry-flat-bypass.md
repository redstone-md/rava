# First-entry flatcall bypass experiment

**Current status (2026-09-27):** The source no longer uses `flat_note_hot`'s
512-call threshold. Flat-lambda first-use compilation is default-off and
explicitly enabled with `RAVA_FLAT_TIER=1`; the old threshold description below
is historical. A same-binary no-snapshot PGO ABBA measured a 5.92 s wall
regression when the fixed compile request was enabled (ledger rows 727–729).

The source reveals a JIT coverage boundary: `invoke_idx` calls `flatcall::try_run`
before `jit_run`. A successful frame-free return therefore bypasses compilation.
`flat_note_hot`, called for lambda targets, has a separate 512-entry threshold.
The first-entry configuration alone does not eliminate that threshold.

The retired `jit/trivial_getter.rs` cache is permanently disabled because its
first receiver selected the optimized class. Do not re-enable it as a fix.
Inspection with javap confirms that `IndirectMerger.size` is a five-byte field
getter, while `DiscreteCubeMerger.size` reads a field and invokes another getter.
Declared interface names alone cannot identify which body ran.

Experiment: use the existing `RAVA_NO_FLATCALL=1` switch with first-entry JIT,
cap 16384 and default early queue reservation. Same normal binary for both arms,
SHA256 858AEA5B921A1E98173681B27F625F1BBB6CB2C95D73C007B56DE37734CF16F4.
Fresh fixtures, standard allocator/memory flags, no prepared archive, snapshots
or instrumentation in the timing series. Quiet counters were unavailable.

| Order | Flatcall | Wall seconds | CPU seconds | Boot artifact suffix |
| --- | --- | ---: | ---: | --- |
| B0 | disabled | 24.567 | 42.015625 | 6e0c6bd663db408eaa170eb65b8d3cb1 |
| A0 | enabled | 24.998 | 41.8125 | b3ad16db8e09486eaaa5d1c2f4f68cb1 |
| A1 | enabled | 24.612 | 42.234375 | f69cb81204c2476bb65186ee0debdd23 |
| B1 | disabled | 25.075 | 41.640625 | 5fb6652e31dd4c519551d7da86bf6858 |

All reached Done. Paired candidate-minus-control differences reverse sign:
-431 ms then +463 ms. No stable wall improvement was demonstrated; the global
bypass is not promoted. No semantic/product certification of this rejected
configuration is claimed.

A separate diagnostic boot with the same bypass and the existing C6D7D0BE
diagnostic binary reached Done: artifact d63e723f97e74a30b8c1a9c9e55a0ee0,
instrumented wall 26.523 seconds, CPU 47.484375 seconds. Its last cumulative
fallback census contains 4999697 events versus 6615267 in the earlier enabled
diagnostic. Interface counts are 2450864 versus 3929550. `IndexMerger.size`
disappears from the top rows; Function.apply remains first (5277 sampled events,
27.02% of sampled names, versus 5216 and 20.18% earlier).

These are cumulative diagnostics with the window and sampling limitations in
`first-entry-fallback-census.md`, not exact paired startup counts or time saved.
They support changed invocation routing, while the normal timing series does
not support a startup speedup. A smaller callback population is not sufficient
evidence for promotion.

Artifacts and scripts: directory named by
`%TEMP%/rava-first-entry-flat-bypass-current.txt`. Runtime source and production
defaults are unchanged. Next investigate lambda target/adapter restrictions and
the separate flat-target threshold; preserve structural eligibility instead of
reviving first-receiver selection. Full wall below five seconds, no-adapt and
no-snapshot remains the objective and is not achieved.
