# Structural JIT admission experiment

Goal remains process-to-Done under five seconds, no adaptation, no snapshots.
Raising the normal JIT threshold to two billion on ED79742D...AFEC took
62.001 seconds (CPU 60.328) with existing primitive-loop eager classification.
Evidence: `%TEMP%/rava-product-boot-d649233399b945488adde70a2c9b89b0`.
This is a bounded experiment, not proof the threshold can never be reached.

Rejected trial: opt-in RAVA_STRUCTURAL_JIT_ONLY admitted bounded valid loop methods on first
entry, including reference/call instructions, through existing codegen checks.
Other standalone methods do not become eligible through invocation counts.
Inlining remains subject to the existing compiler's checks. No default change.
The final validation and rejection are recorded below.
This scheduling change alone does not certify all downstream no-adapt policies.

Initial validation matched 29/31 probes: GuestListSem and IterSem lost CME on
mutation during compiled iteration. Each individual iterator kill switch still
failed; all three together restored IterSem. Therefore the experimental mode
disables JIT iterator shortcut, iterator intrinsic and emitted hasNext/next
specialization until those paths carry mutation/fail-fast proofs. This is a
process-wide compiler capability restriction, not a workload or method-name
exception. Ordinary mode retains its existing policy. Evidence:
`%TEMP%/rava-structural-only-e759d5f7a5b14cdaa77fa92be8085cbb/sem.log` and
`%TEMP%/rava-structural-all-iter-control.log`.

After disabling all three iterator paths, all 31 semantic probes matched Java
-Xint with the suite's existing declared divergences. The uninstrumented full
boot reached Done in 103.221 seconds, CPU 100.90625 seconds, peak RSS
1,225,707,520 bytes. Quiet-box counters were unavailable. This is a failed
performance experiment, not a replacement for the ordinary 27-28 second mode.
Evidence: `%TEMP%/rava-product-boot-5bfae0d72a2d4a3cb646a2af1d498428/result.json`.
Executable SHA256:
`E8BA1E06EB382C151ECFB72713980E3AD4989041C575B9518F45C0C950939CBA`.

The five-file experimental source diff was saved as
`rejected-structural-jit.patch` beside the saved executable, sem.log and boot.log
in the directory referenced by `%TEMP%/rava-structural-safe-current.txt`.
That exact diff was reverse-applied after git apply --reverse --check passed.
No experimental source changes remain; the shared target executable still
contains the rejected experiment until rebuilt. Do not benchmark it as HEAD.

Next architecture investigation: general prepared invoke emission still spills
roots, marshals arguments and calls rava_invoke; only the proven numeric static
leaf bypasses this boundary. A direct-call region needs explicit GC root and
exception propagation contracts, class-initialization handling and resolved
dispatch proofs. Changing the intermediate language to Rust alone removes none
of these costs. Require full-boot evidence before expanding a region compiler.
