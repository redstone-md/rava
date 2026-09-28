# Direct-probe instruction attribution

The first-entry profile on d2a176db places 445 of 19551 primary-thread samples
in `Vm::jit_direct_probe`. This is insufficient to choose between receiver
decoding, argument guards and the two nested cache lookups. The existing trace
discards symbol displacement and source lines, so the old samples cannot answer
that question.

Plan: retain optional instruction details in the existing trace, then run an
isolated fresh-fixture first-entry boot with cap 16384. Filter samples against
the recorded launch-to-Done timestamps before ranking source lines/offsets.
Use those locations to choose a runtime experiment; do not infer removable wall
time from sample counts.

`RAVA_PC_TRACE_DETAIL=jit_direct_probe` selects diagnostic leaf names by substring
when `RAVA_PC_TRACE=1` and stack capture is off. It appends RIP, symbol displacement
and PDB source line after the existing leaf delimiter. Selection never controls
guest execution, compilation eligibility or cache admission. Resolution is cached
per address within each dump. This adds no runtime cache or mutable global state.
The existing source-line resolver is reused; no new file exceeds 500 lines.
Frontend state and animation requirements do not apply.

Validation: release-diag build with next-gen-gc and `/OPT:NOICF` passed in
3m37s. No wall-profile feature was used. The isolated boot reached full Done
with instrumented wall 27.244 seconds and CPU 48.671875 seconds. Performance
counters were unavailable, so quiet CPU conditions were not certified.

Artifact: `%TEMP%/rava-product-boot-dec221acef9b4d65bdcfdae9485b782a`.
Executable SHA256:
`C6D7D0BE0E26C09F41523B1FC5671699855229DDF6D8374FCF39F1117A9D6B0D`.
PDB SHA256:
`32A915064080591AF51A5EE80B3A11030F72F359761BF0D2CB4039C25A24191E`.
The normal release binary was not replaced.

Of 18048 primary-thread samples inside the launch-to-Done window, 390 selected
the probe; all 390 carried the expected detail fields and resolved source lines.
Existing timestamp/leaf parsing still works because the leaf field is unchanged.

| Source location in jit_direct_call.rs | Samples | Operation |
| --- | ---: | --- |
| 908 | 182 | Outer call-site map lookup |
| 909 | 89 | Receiver-guard map lookup |
| 910 | 58 | Entry selection closure |

These 329 samples justify examining the nested lookup before receiver-header
micro-optimizations. PDB attribution covers inlined instructions and does not
measure each source expression precisely. The instruction offsets, raw trace,
analysis script and complete rankings are retained under the directory named by
`%TEMP%/rava-direct-probe-detail-current.txt`.

Next experiment: replace the nested direct-entry lookup with one authoritative
lookup keyed by caller, constant-pool index and receiver guard, preserving the
collision list, argument guards, lambda checks, publication discipline and
worker access restrictions. Measure against the existing representation before
promotion. This profile is not a speedup and does not prove that this change
alone can approach five seconds. The full goal remains unmet.
