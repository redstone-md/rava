# Bound repeated inline expansion before code generation

An opt-in first-entry scheduling experiment exposed a compiler memory failure in
CompSem. The experiment bypassed the main and worker invocation thresholds while
retaining ordinary planning and safety checks. A one-entry probe at threshold
1,000,000,000 confirmed `cold=1, compile_ok=0` with the flag absent and
`cold=0, compile_ok=1, run=1` with it present, with identical Java output.

The experimental binary (SHA-256
`00AD1540E445694583620E583CA60E42E8B4AB31CF3D8ED07574B9309FBB3FC5`)
passed 26 semantic probes but CompSem aborted on allocation failure. Synchronous
compilation reproduced the failure: sampled private memory reached 4,294,885,376
bytes under the 4096 MiB process cap. The same binary without the flag passed
CompSem and matched Java. First-entry synchronous compilation with
`RAVA_INLINE_BUDGET=0` also passed and matched Java.

The last traced compilation before another failure was Blocks' 40,095-byte
initializer. That name locates diagnostic evidence only; it does not select the
fix. The bytecode contains hundreds of calls sharing individual constant-pool
indices. Inline maps are keyed by those indices. The old planning budget could
refuse later repeats while leaving an earlier map entry present, allowing that
entry to expand at every corresponding call site. Nested multiplicities were
likewise not an upper bound on the final expanded bytecode tree.

## Change

Keep the existing planning budget, then constrain the completed root target map
by call-site multiplicity and recursively expanded callee bytecode size. Admit
whole targets in first-bytecode-occurrence order; remove targets that do not fit.
Do not prune inside a target: some nested callees require full collapse.
Overflow and malformed bytecode fail closed. The policy uses bytecode and the
existing configured budget, not names, profiles, timings or previous failures.

This bounds inline bytecode represented by these maps. It is not an exact IR
memory bound and does not account for every independent emitter transformation.
Other history-based eligibility remains. The failed first-entry scheduling
experiment was removed from runtime source; its patch and binaries are archived,
not a production no-adapt certification or a speed result.

Evidence: `%TEMP%/rava-first-entry-20260905` includes the probe, Java and VM
outputs, per-run environments, allocation failures, memory samples, controls and
`Blocks.javap`.

## Corrected-binary experiment

Four expansion-budget tests pass. The corrected candidate is SHA-256
`703F911DE42232858AD840B32877BFBEEB2BF0D7A35C166CC2657E7712A95039`;
evidence is `%TEMP%/rava-inline-expansion-20260905`. A subsequent comment-only
edit explains the two budget stages; executable code matches the candidate.

The corrected budget alone does **not** fix first-entry memory exhaustion.
Synchronous CompSem still fails under the default 6000-byte budget, again after
the Blocks initializer compilation starts. At budget 600 it exits 0; at budget
0 the earlier candidate already passed. Budget 600's 48 normalized output lines
match the preserved control previously verified against Java. No default budget
was changed.

The inline emitter declares fresh local variables for every splice. Expanded
bytecode bytes are consequently an incomplete model of SSA/IR resource cost.
This is a source-backed next investigation boundary, not a proven attribution
of every allocated byte. A bounded compiler-work policy remains necessary before
first-entry scheduling can become a production replacement for counters.

The initial follow-up inspected SSA variable declarations in `jit/emit/inline.rs`:
each splice allocates `max(max_locals, argument_slots)` fresh variables. Scoped
reuse across non-overlapping splices is a candidate to measure, not an implemented
fix. Any reuse must preserve nested live locals, branch definitions, return SSA
values and stack-map rooting; no class-name exclusion can replace those proofs.
That pool prototype was subsequently tested and removed: allocation still fails
after frontend finalization. See `compile-memory-phases.md` for the measured
boundary and next structural IR-work-budget investigation.

The corrected candidate with first-entry mode unset passes all 27 semantic
probes and all five product hashes (ChunkFill `8790f04ace726ecf`, the three ring
probes `37539c6c375d0c3`, NbtSave `ae44a34b42766bd4`). Cold and warm ReloadBench
both certify `c956377e19071d7a` with counts 1470/1584/625. Cold was 7510 ms, warm
6659 ms, and process wall 42826 ms. These are single correctness-run timings,
not a paired price. Standing environment and affinity `0x0FFF` were used.

There is no timed ReloadBench speed claim for first-entry scheduling: its default
configuration failed the prerequisite semantic gate.

The final source removes the first-entry switch entirely. The resulting release
binary, SHA-256 `824D9324A309A3814E27D64466CB3837CCB2A3FA703A9C647F2136C82CE68085`,
also exits 0 and certifies cold/warm `c956377e19071d7a` with counts 1470/1584/625.
Cold was 7961 ms and warm 6970 ms. The preserved `final.env`, `final-result.txt`
and final reload outputs record this run. The earlier full suite exercised the
same budget implementation with the experimental switch unset; the final build
only removes that dormant switch. No snapshot/restore input was used.
