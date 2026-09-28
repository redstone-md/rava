# Null operands in compiled athrow

rava_throw previously put Value::Null in the pending exception slot. The
interpreter instead creates an NPE. Reuse its exception class and throw_null
message, preserve the original object for non-null throws, and retain the
first pending fault. Spill live references before the callback and review both
no-allocation analyses because constructing an exception may collect.

Validation: repeat null and non-null throws across JIT compilation, verify
exception identity and message, then run the complete differential suite and
product hashes before keeping the change. This enables further first-entry
lambda validation; it is not a speed gain or proof of the full performance goal.
New files are under 500 lines; frontend state and animation rules do not apply.

The null arm uses worker_illegal_gate: serial execution proceeds, locked worker
execution holds the existing resolve lock, and an unlocked worker aborts before
exception construction. Non-null throws retain the existing private fault path.
Both no-allocation analyses and field invariance now reject athrow. Tests below
do not establish that the null worker branch was exercised.

Final validation: all 46 differential probes pass with existing declared
divergences. JitThrowNullSem has none: null=4096, same=4096 and the literal-null
message matches Java. Preserved control exits 1 on this test. The initial build
also passed, but final evidence was rerun after the invariant and worker changes.
Messages identifying a null parameter instead of a literal remain outside this
focused correction; the existing helper uses the literal-null wording.

All five product hashes pass. Isolated first-entry boot reaches Done in 26.382 s,
CPU 44.09375 s. Quiet-counter sampling was unavailable; this single observation
is not an A/B performance result. Early flat-lambda compilation remains removed.
The complete wall <5 s, no-adapt, no-snapshot objective remains unmet.

Candidate SHA256:
`7C7221E480F4E421DACC613DC1DDB6224B201626DD5CC61202324C4CE10DFEAD`.
Control SHA256:
`58854E0D5CCAC3641722A0990943C64F03F043BC1218CA9428CDDBA847BC3F9B`.
Artifacts: `%TEMP%/rava-jit-throw-null-4db19bc15108418c92502db707e65b0a`.
