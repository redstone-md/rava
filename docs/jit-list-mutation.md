# Share collection contracts for JIT list mutations

The compiled fu_fastpath List arm directly used array_push/remove/set, bypassing
coll_op's mutability contract, view handling and modification-count maintenance.
A 4096-call lambda on an unmodifiable list reproduced the defect on E5727F42:
RAVA caught 511 exceptions and grew the list to 3586 elements; Java caught 4096
and retained one element. The transition follows lambda compilation, so this is
an execution-path defect rather than merely a lost exception at return.

Plan: keep the native list receiver classification and direct read paths, but
route add(Object), remove(int) and set(int,Object) through the authoritative
collection operation. Decode into a small fixed argument array, root it across
the operation, and preserve the distinction between unhandled and a handled null
return. This reuses contracts and mutation bookkeeping without recreating them
in the JIT fast path. The adapter is encapsulated in a small module; new files
are under 500 lines. Frontend state and animation requirements do not apply.

Validation: reproduce against the preserved control, run JitListSem and the
full differential suite, then repeat early-lambda checks and product hashes.
Do not report a speed gain from a path that mutated a supposedly immutable list.
The wall-under-five-seconds, no-adapt, no-snapshot goal remains unmet.

Validation result: all 43 differential probes match Java with the existing
declared divergences. JitListSem checks rejected immutable writes, permitted
fixed-size set and iterator modification detection. The preserved control fails
this probe; the candidate passes. All five product hash gates pass (ChunkFill,
RingScan, RingLane, RingLaneInstance and NbtSave).

An isolated first-entry boot reached Done in 26.792 s (44.875 CPU seconds).
Performance-counter sampling was unavailable, so the harness skipped its quiet
check. This single run is functional evidence, not a measured speed improvement.
Candidate SHA256:
`44BF49A8D93179015283941E3740628A939F0E5B2226DE1BA2713150866DFDBD`.
Artifacts: `%TEMP%/rava-lambda-exceptions-89a9e82326184fbaa9eb2fbab013b874`.
Earlier compilation of flat lambdas remains a separate, unvalidated experiment;
this fix does not establish that all its exception failures are resolved.
