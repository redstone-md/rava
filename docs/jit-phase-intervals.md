# Separate JIT planning from code generation

The eight-worker experiment increased CPU and failed to improve boot wall.
Existing cumulative compile-result timing overlaps between workers and leaves
VM-owned planning unpriced. Add opt-in RAVA_JIT_PHASE_TRACE intervals around
plans, background field resolution, synchronous/background codegen, publication,
and waiting. The environment value is the output file path. The disabled path
does not read clocks or allocate spans.

Each completed span emits thread identity, Code key, wall-clock start and a
monotonic elapsed duration. Clip intervals to the harness launch/Done window,
merge overlapping intervals per thread, and distinguish inclusive phases from
the union of all phases. Never add inclusive nested phase times as if exclusive.
Unfinished spans at process termination are absent; diagnostics can perturb
enclosing spans and scheduling. This is elapsed occupation, not CPU time.
Producers use nonblocking try_send on a bounded 65536-record channel; an isolated
writer formats records and flushes the dedicated file every 50 ms. Drop/error
records invalidate a complete census. The harness allows a 500 ms post-Done
drain window; analysis still clips completed intervals to Done.

The tracer is independent of VM state and cannot change eligibility, counters,
queue policy or compilation outcomes by feedback. Normal runtime overhead is
one cached flag check at these compile boundaries. New module is below 500 lines.
Build diagnostic profile separately, preserving the frozen normal release.
Validate trace records and time clipping, then capture a fresh-world boot.
The full wall <5 s, no-adapt, no-snapshot goal remains unfulfilled.

## Validation and capture

An initial synchronous-stderr implementation made the diagnostic boot take
74.019 s and produced a malformed record rejected by the parser. That capture
is excluded from phase-cost conclusions. The final implementation uses the
separate bounded writer above and was rebuilt before validation.

JitNpeSiteSem with tracing enabled matches Java. No full semantic/product suite
was repeated for this diagnostic-only change. The normal release executable was
not rebuilt or replaced; its SHA-256 remains
62C48966B7A0BC2E1DC2B93FDF9CA94C1A079F6F6269411EA75538D83BCD0954.

Final release-diag executable SHA-256:
C232A4C1E7A0E93DF9B98BAB4B12330E73708083C6DD6215C00C3CACDAF6AAE1.
Fresh-world run under TEMP:
rava-product-boot-51fcf4bb8d0b4621a01c47911aea4c74.
Done wall 27.798 s, CPU 46.421875 s, instrumented. Quiet counters unavailable.
All trace records parsed; no dropped-record or writer-error report was present.
33,046 completed intervals intersect the launch/Done window; 259 lie outside it.
This is not a calibration of the remaining instrumentation overhead or a speedup.

| Phase on VM thread ThreadId(2) | Completed spans | Inclusive ms | Union ms |
| --- | ---: | ---: | ---: |
| plans | 8338 | 309.597 | 309.597 |
| fields | 8168 | 10.953 | 10.953 |
| publish | 8157 | 14.404 | 14.404 |
| sync-codegen | 170 | 166.749 | 166.749 |
| wait | 52 | 295.940 | 295.940 |

The union of all these VM-thread intervals is 796.051 ms. Summing the rows
would count nested intervals twice. No phase repeated a Code key on its thread.
That does not mean there were no queue-full retries: those return before plans.

Four compiler threads completed 8161 captured codegen intervals with unions
5091.933, 5042.944, 4960.647 and 5032.974 ms respectively. Their sum, 20128.498 ms,
overlaps across threads and is neither wall nor CPU time. Attempts may fail;
these are not counts of successfully executable compiled methods. Unfinished
intervals are not represented, and any work outside the instrumented boundaries
is unpriced.

Evidence under TEMP: rava-jit-phases-143dbe9350214eca95b515eb14f67ce8.
The reusable parser is bench/jit_phase_intervals.py; run it with the boot evidence
directory and optional --output directory. It checks malformed/drop/error records
and overlap arithmetic, and writes report.json plus intervals.json. Temporary
binary/run evidence is not guaranteed durable.

## Consequence for the next change

Planning and result publication cannot directly explain the missing roughly
twenty seconds in this capture. Prioritize runtime execution and compiled-to-host
transitions rather than further planning micro-optimizations. Queue request
retention remains relevant to no-adapt, but the measurements do not justify
claiming that such a rewrite itself will deliver the wall target. Effects of
earlier code availability on downstream execution remain separate and unpriced.
