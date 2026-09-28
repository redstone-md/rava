# Structural inline target selection

Initial inline planning no longer consumes `sitemono::mono_class`,
`sitemono::mono_ref`, or `lambda_site_mono`. Removed the profiled lambda chain,
heap-reference target branch, and preference for observed arena receiver types.

Direct/final resolution, capped loaded-hierarchy resolution, and static lambda
provenance remain. Existing emitted receiver guards and representation, descriptor
and GC checks remain mandatory. Unknown receivers use ordinary dispatch.

Remaining profile readers were inspected: `lambda_prov` uses a receiver-profile
query only for diagnostic counts/logging; the compiled SAM learning hook compares
before/after profile states for counters. Profile writers still run and are a
separate cleanup target. This change alone does not establish full no-adapt
compliance: invocation thresholds and other runtime policies still need review.

Candidate SHA-256:
`06F0A29A48DEC9171177F5D55ADB6B39E10A7B2F3E8FD24DA821C18398E157F2`.
Release build and `cargo check` pass. All 28 default semantic probes, five product
hashes and certified cold/warm ReloadBench outputs pass in
`%TEMP%/rava-structural-inline-20260905`. Source comments were clarified after the build;
the executable behavior under test is unchanged.

One exploratory full-boot pair, both in normal JIT mode, reached Done:

| Binary | Wall to Done | CPU | Evidence directory suffix |
| --- | ---: | ---: | --- |
| Structural selection | 33,206 ms | 34.781 s | `dc94f0cbd2cb4592892e190e9aaa92af` |
| Previous early-IR binary | 32,862 ms | 34.422 s | `ce1a5cc16ba948bf964a24cafd24150b` |

Evidence directories are `%TEMP%/rava-product-boot-<suffix>`. Both use identical
standing settings, copied pristine fixtures and affinity. Host quiet counters
were unavailable. This approximately one-percent difference in one pair does not
establish a speed effect. The change removes history-selected inlining; it does
not establish startup below five seconds.
