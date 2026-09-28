# Null receivers in compiled String reads

The length/hash intrinsic bypasses invocation's null receiver check. Preserve
the native non-null path, but send null to the existing invocation callback and
exit through the compiled epilogue before subsequent effects. Reuse exception
construction and root handling instead of implementing another String contract.
The guard is local to these two intrinsic shapes; no counters or history select it.

Validation plan: JitStringNullSem repeats both valid and null calls, checking
exception counts and the absence of writes after a fault. Compare the preserved
control and candidate, then run the full differential suite. Helpful NPE message
provenance and the remaining early-lambda iterator failures remain separate work.
New files are below 500 lines; no frontend state or animation code is involved.

Validation: all 44 differential probes pass with their existing declared
divergences. The new probe has no declared divergences: candidate produces 4096
exceptions for each method and after=8192. Preserved control produces 186 for
each and after=16012, demonstrating execution past the fault after compilation.
All five product hashes pass. An isolated first-entry boot reaches Done in
26.764 s, CPU 45.1875 s. Quiet-counter sampling was unavailable; this single run
is functional validation and establishes no speed gain.

Candidate SHA256:
`76A2E8CABC43A3E866E30070D9972CCAFF6D60D220498134B0BF583C3ADC74E3`.
Control SHA256:
`44BF49A8D93179015283941E3740628A939F0E5B2226DE1BA2713150866DFDBD`.
Artifacts: `%TEMP%/rava-jit-string-null-11823db2227a41c8894f470a48358bfb`.
The early-lambda experiment is still rolled back. This focused correction does
not certify helpful NPE messages or other String intrinsics. The full wall <5 s,
no-adapt, no-snapshot objective remains unmet.

Follow-up source correction: List.iterator also stamps Source and ExpSize after
Elems/Pos. Its Iter.next arm and the inline next path advance without checking
that witness. The missing validation is in consumption, not iterator creation;
see jit-iterator-source.md for the focused repair and its remaining limits.
