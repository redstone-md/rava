# First-entry compilation for flat lambda targets

**Current status (2026-09-27):** This proposal predates the current fixed
first-use implementation. Flat-lambda compilation is default-off; the
history-free first-use request is available only with `RAVA_FLAT_TIER=1`.
The same-binary PGO ABBA measured +5.92 s launch wall with the request enabled
(ledger rows 727–729), so do not treat the plan below as the current default.

`flat_note_hot` uses a separate 512-call threshold before requesting a lambda
body's compilation. The comparator/key-function forwarding path additionally
stops forwarding after 4096 calls. Both remain active inside first-entry JIT,
even though frame-free execution bypasses the normal JIT entry point.

Plan: in explicit first-entry mode, use the existing JIT method state to request
compilation without either counter. Compiled, compiling and failed states remain
authoritative. Ordinary tiered mode retains its previous behavior. Both callers
already root their arguments across resolution/compilation. Reuse force_compile
and its compile-only scope; this request is synchronous for an uncompiled target,
so its startup cost must be measured. Do not execute the body on empty arguments.

The change uses existing owners and adds no cache or global mutable state.
No new file exceeds 500 lines; frontend state/animation rules do not apply.
Validate the full differential suite and compare fresh full boots against the
preserved release. This closes one hotness gate, not whole-runtime no-adapt
compliance. The five-second no-snapshot objective remains unfulfilled.

## Validation failure and rollback

The release build succeeded. The first-entry differential suite completed with
7 failures out of 41: ArraysSem, ClassSem, ExcSem, GuestListSem, IterSem,
MapViewSem and NpeSem. The first six lose expected exceptions in lambda bodies;
NpeSem exits 1 with an uncaught NullPointerException. This is not an accepted
semantic divergence. No boot speed claim or product validation was attempted.

The isolated ClassSem control was rerun on the preserved 858AEA5B executable
under the same first-entry settings and passed all 26 output lines. Its candidate
reports `cast.bad NO THROW` where Java and control report ClassCastException.
Source inspection identifies a concrete defect: `jit/emit/ops/objects.rs` handles
opcode 0xc0 with an empty arm. The interpreter enforces this cast. Other exception
failures need independent tracing; do not assume they all share that cause.

Candidate SHA256:
`5C1E8C0EC2937477A13F8A1B8FB5E4FB82524D3A67380BDEA4C0FAC072475D9C`.
Artifacts including candidate.patch, candidate/control executables, environment,
full semantic outputs and control-class.log:
`%TEMP%/rava-first-entry-flat-lambda-6a762ba888d24f9fb9d717860cf8d686`.

The runtime patch was removed and the normal release executable restored to
858AEA5B921A1E98173681B27F625F1BBB6CB2C95D73C007B56DE37734CF16F4.
Next implement/enforce JIT checkcast and investigate exception propagation, then
reapply the saved first-entry patch and rerun the suite. Retaining the 512-call
gate is temporary rollback, not a solution to the no-adapt requirement.

Retry after 505b0c0e: checked casts and shared list mutation contracts are now
committed and pass 43 differential probes on their own. Reapply the same three
file experiment and repeat the complete suite before considering boot timing.
This remains an experiment until every newly exposed semantic failure is resolved.

Retry result: 39 of 43 probes pass; ExcSem, GuestListSem, IterSem and NpeSem
still fail. ArraysSem, ClassSem and MapViewSem now pass. ExcSem misses a null
String.length exception and uses abbreviated array bounds messages. GuestListSem
and IterSem miss concurrent modification exceptions; NpeSem exits 1. JitListSem
passes, so the remaining iterator execution path needs independent investigation.
No performance or product runs were made with this failing experiment.

Candidate SHA256:
`63DDCEAC124C9ADF4D84F5A400152A13B117DD09D97F5BA52A594F56AE9C5CD1`.
Artifacts: `%TEMP%/rava-flat-lambda-retry-d49dc78606ad4ea1a44438785d7093c9`.
The three-file runtime experiment was removed again and the normal release
restored to the validated 44BF49A8 list-contract binary. Next isolate the null
receiver and iterator paths before retrying first-entry lambda compilation.

Second retry, based on 29197d9d: String length/hash null side exits and iterator
source checks now pass their targeted regressions and all 45 semantic probes
independently. Reapply the same three-file experiment, then run all 45 probes
before performance comparison. Retain the validated control executable and patch
to allow exact rollback. No broadened compilation is certified by the baseline
tests alone, and remaining exception message differences must be inspected.

Second retry result: 43 of 45 probes pass. GuestListSem and IterSem now pass,
as do JitCastSem, JitListSem, JitStringNullSem and JitIteratorSem. ExcSem now
throws the expected exception classes but still differs in the NPE source clause
and array bounds message. NpeSem exits 1 after its monitor line, before throw.null.

The next concrete defect is rava_throw: it unboxes a null cell and stores Null
as the pending exception. Interpreter athrow instead creates an NPE using
npe::throw_null(). The compiled athrow emitter currently documents its callback
as non-allocating and does not spill roots. Fixing the callback therefore also
requires correct allocation/root handling and review of no-allocation analysis.
This is not merely a helpful-message mismatch.

Candidate SHA256:
`36A48C44210AA07069016CAF32B0F80E34B7AEAACE469D29B6CAB7BF4B64C2E5`.
Artifacts: `%TEMP%/rava-flat-lambda-contracts-e60291a3ff93466fae1e7fef0c4fd556`.
The runtime experiment was removed and normal release restored to validated
58854E0D. No boot timing/product run was made with this failing candidate.

Third retry after f800384f: reran only NpeSem and ExcSem. Both processes now
finish normally. NpeSem differs on 28 message lines; its throw.null, explicit
and explicit.empty lines match. ExcSem differs on three messages (NPE source
and positive/negative bounds). No full-suite pass is claimed for this candidate.

An explicitly exploratory boot of this semantically failing candidate reaches
Done in 34.665 s, CPU 56 s, versus the preceding validated single observation
26.382 s. This is not a paired performance verdict or a promotion result.
Quiet-counter sampling was unavailable. No product hash gates were run.

Candidate SHA256:
`1697AC9B2B7452B8533D61B3E4D7A1C15F1C2947D93FB6116EC3E84878E5C427`.
Artifacts: `%TEMP%/rava-flat-lambda-throw-50a49bebc7ff4bb3b93d48b78111a1a6`.

Source investigation changes the next priority: force_compile enters
COMPILE_ONLY, which jit_run deliberately excludes from background submission.
This is appropriate for callers that need a compiled body immediately, but makes
every newly encountered flat lambda synchronously compile on the boot path.
Investigate a separate non-executing background compile request; do not change
force_compile globally or allow empty locals to execute a compiled body.

The existing deopt exit is not a general NPE-message repair: it refuses void
methods and spliced bodies. Fault-site metadata needs a solution that covers
those cases. Separately, rava_array_fault prints only the index while the nearby
array_bounds_pending already formats index and length.
The experiment was removed and normal release restored to validated 7C7221E4.
