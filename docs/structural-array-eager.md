# Primitive-array eager admission: rejected for zero reload coverage

The current scalar eager policy excludes primitive array accesses inside natural
loops. Outside the parallel-map proof, such methods wait for invocation counts
even when a single invocation contains substantial iteration.

The removed `RAVA_STRUCTURAL_ARRAY_EAGER=1` prototype extended the bounded, immutable class-load
proof. It permits primitive array loads/stores, reference-local movement,
arraylength and null branches inside the same reducible natural-loop CFG.
Calls, allocation, fields, monitors and reference-array access remain excluded
inside loops. Existing frame, exception-table, bytecode-size and block limits
remained authoritative. Method names, past calls, results and timing were absent
from admission. The flag and all runtime changes have been removed.

Execution used the existing sequential compiler with its normal handles, array
bounds/null checks and exception propagation. No raw-array representation or
new parallel execution was introduced. This local policy did not establish
whole-runtime no-adapt compliance: other compilation routes still use history.

The prototype's `RAVA_JIT_STATS` diagnostic recorded `[STRUCTURAL-ARRAY] early-decision`
when the new admission bypasses ordinary warmup, and `compiled-entry` when a newly
admitted body reaches the VM-to-JIT entry seam. Direct JIT calls bypass the latter;
zero entry-seam events alone cannot prove zero scheduling effect. Its set is
diagnostic only and cannot affect production eligibility. Runs with JIT statistics
are coverage evidence, not timing evidence.

Eight admission tests pass, including opt-in/name independence, forbidden loop
effects, malformed control flow, exception handlers and irreducible loops.
`ArrayLoopSem` covers all eight primitive types, integer narrowing/overflow,
floating-point bit patterns, nulls, bounds failures and partial stores, with
two calls per method so a high ordinary JIT threshold cannot fake eager coverage.
All 28 differential probes pass on the first release candidate. The focused probe
passes with the flag off, on, and on with a 64 KiB nursery/GC poison. With normal
threshold 1000000 and background compilation disabled, off records zero new entry
events and each on mode records all eight. No minor GC occurred in this small
probe, so it is not moving-GC evidence.

The first ReloadBench diagnostic run exited 0 with cold and warm hash
`c956377e19071d7a` but no new entry-seam events. Early-decision coverage is still
not inferable from that seam alone.
Evidence directory: `%TEMP%/rava-array-eager-20260905`. First candidate SHA-256:
`42DA7CA3C1F5A711C7B8DCBADE6476265EA5FFA301DAE2A1392EB20801C55E77`.

The second diagnostic version adds the early-decision event exactly where an
eager body bypasses the ordinary warmup condition. Its positive control records
eight early decisions and eight compiled entries with matching Java output.
ReloadBench then exits 0 with both hashes correct, **zero early decisions and
zero compiled-entry events**. This population does not justify a timing series
or retaining the scheduling extension. Second executable SHA-256:
`999445B5F2B9F45B708E469ECDE2E3FF057F6119BF0D331D9556821D4C74C9B9`.
Raw `coverage2.*`, `sem-decision.*`, both executables, and the complete modified
source files under `source/` are retained in that evidence directory. The probe
and added tests are archived there too; none of the prototype remains in runtime.

The diagnostic population was exported to `jit-population.csv` using the last
record for each method. High cached-failure counts include LinkedTreeMap's
`findByObject` (261750) and Identifier's `read` (80371). These identify a possible
next profiling population, not time costs or permission for name-based admission.
Before another implementation, attribute wall time and inspect the actual
compiler refusal reasons; do not rank optimization value by frequency alone.

## Certified null floor

The four-pair same-binary null series completed with all eight cold and warm
hashes correct. Evidence: `%TEMP%/rava-certified-null-20260904-235610` and sibling
`.log`. Warm A values: 7697/7610/7828/7085 ms; B: 7764/7630/8618/7270 ms.
A spread is 10.5%; all reported metrics overlap. Median warm delta is +0.57%,
which is not a performance effect. Small subsequent deltas remain unresolved.

The full wall-under-five-seconds goal remains unachieved.
