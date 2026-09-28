# First-entry compiler pool: eight threads rejected

Runtime/source 8e0bb26c; no runtime or default configuration edits in this
experiment. Both arms use the identical frozen release executable, SHA-256
62C48966B7A0BC2E1DC2B93FDF9CA94C1A079F6F6269411EA75538D83BCD0954.
The full wall <5 s, no-adapt, no-snapshot objective remains unfulfilled.

Historical thread-count measurements used a smaller compilation population.
The current first-entry diagnostic reaches thousands more results, motivating
one new controlled comparison, rather than assuming the historical optimum.

## Uninstrumented comparison

Standard fresh-world boot, first-entry, cap 16384, affinity 0x0FFF, process cap
4096 MiB. Null-first interleaving, no concurrent builds/tests/source changes.
Four and eight compiler threads are explicit environment values. Queue capacity
is max(depth, threads * 4), so this compares the complete pool configurations
(4 threads/16 slots versus 8 threads/32 slots), not thread count in isolation.

| Run | Threads | Wall seconds | CPU seconds |
| --- | ---: | ---: | ---: |
| null0 | 4 | 24.999 | 42.015625 |
| null1 | 4 | 25.578 | 42.875 |
| candidate0 | 8 | 25.858 | 53.578125 |
| control0 | 4 | 25.368 | 42.890625 |
| control1 | 4 | 25.529 | 42.96875 |
| candidate1 | 8 | 28.277 | 57.15625 |

All six reached Done. Saved per-run environments confirm the intended thread
counts. Null spread is 0.579 s; candidate is slower by 0.490 and 2.748 s, with
substantially more CPU. No speed gain supports promotion. Default remains four.
No semantic/product suite was run for the rejected eight-thread configuration.
Quiet performance counters were unavailable; no precision beyond these samples
or attribution of the slowdown to a single mechanism is claimed.

Evidence under TEMP: rava-jit-pool-eight-19c95b4673b74c549f77fd791d1bd7ab.

## Diagnostic captures and limits

Same binary with PROFILE_PC, PC_TRACE, PROFILE_PC_MS=3000 and COMPILE_TIME.
No PDB was copied: this capture reads counters, not resolved host call stacks.
Four-thread run rava-product-boot-203d5b512ce249febb8a0f95f96f3e2f reached Done
in instrumented 28.127 s, CPU 50.875 s, then allowed the standard 3.5 s dump tail.
The log's last COMPILE-TIME milestone is 8000 results, 20.00 s cumulative
background compile elapsed time, 0.14 s cumulative finalize_definitions.
The latest periodic JIT-COVERAGE is 7387, cap 16384.

Compile-result counters include failures and elapsed time overlaps across
workers; neither number is successful compiled-method count or process CPU.
Counter lines lack timestamps, so these figures describe the entire capture,
including the post-Done tail, not an exact pre-Done census. No HR-WAIT report
appeared; the reporter suppresses output when its recorded parks and deferrals
are zero. This does not bound all possible VM/compiler synchronization.

Eight-thread run rava-product-boot-3adf533d1dce41c4ae7d13b9f4bb4ac9 failed
before Done, exit -1073740791. nextgen/mapping.rs:98 reports failure to commit
8388608 bytes, followed by a non-unwinding panic. The exact reason for VirtualAlloc
failure was not captured; process peak RSS is not commit usage. Its wall is
excluded from performance comparisons. The partial log reaches 9000 background
results, 29.30 s cumulative compile elapsed, periodic coverage 8561. Comparing
these differently terminated captures does not establish a matched-window rate.

Diagnostic evidence under TEMP: rava-jit-queue-profile-edc454fd9de045e8a69aac7e234b9dbc.
Temporary evidence is not a durable repository artifact.

## Next causal target

Adding workers did not improve wall. Measure VM-owned plan generation separately
from worker codegen before further pool tuning. The early-permit failure arm in
jit_run returns Cold(n2), leaving the compile request dependent on a future
invocation; queue availability can therefore change which requests get planned.
Investigate retaining pending request identity independently of queue capacity.
Do not prebuild heap-owning plans into an unbounded backlog: static-slot rollback
depends on contiguous top-of-table reservations and nested compilation ordering.
No new eligibility or pending-request policy is implemented by this report.

