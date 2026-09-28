# Preserve source checks in compiled iterator reads

Source inspection corrects the prior partial finding: List.iterator already
stamps Source and ExpSize. The missing checks are in fu_fastpath's next arm and
the inline element/advance path. Reuse iterator_source_changed before callback
advancement; route source-backed inline receivers through that callback before
any element read or cursor write. Bare iterators keep their native inline path.

The field identity joins existing iterator metadata; this does not certify the
existing readiness/publication system as globally no-adapt. The shared witness
currently compares sizes, so equal-size structural changes remain a separate
limitation. No snapshot execution mechanism is introduced.

Validate repeated compiled reads after source mutation against the control,
then all semantic probes and product hashes. Do not claim the goal achieved or
a performance win from this correctness change. New files are under 500 lines;
frontend state and animation rules do not apply.

Validation result: all 45 differential probes pass with existing declared
divergences. JitIteratorSem has none; candidate reports caught=4096, after=4096.
The preserved control reports caught=120, after=8072. All five product hash
checks pass. An isolated first-entry boot reaches Done in 27.976 s, CPU
46.703125 s. This exceeds the prior single 26.764 s observation; it is not a
paired speed verdict. Quiet-counter sampling was unavailable. The extra source
check may cost runtime and must be evaluated with the expanded JIT candidate.

Candidate SHA256:
`58854E0D5CCAC3641722A0990943C64F03F043BC1218CA9428CDDBA847BC3F9B`.
Control SHA256:
`76A2E8CABC43A3E866E30070D9972CCAFF6D60D220498134B0BF583C3ADC74E3`.
Artifacts: `%TEMP%/rava-jit-iterator-da0ab87796084acf8c6a558292c22b54`.
The early-lambda experiment remains removed. Next rerun it with the String-null
and iterator repairs; do not infer that its remaining exception/message failures
are fixed from this standalone suite. The full performance objective remains open.
