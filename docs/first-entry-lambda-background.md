# Background requests for first-entry flat lambdas

The synchronous experiment reached Done in 34.665 s and still had exception
message differences. force_compile intentionally bypasses background submission
because its existing callers require an executable body before proceeding.

Introduce an encapsulated, non-executing background request using the existing
jit_run planning/queue path. Preserve COMPILE_ONLY's execution guard and scope
both modes together, including nested synchronous force_compile. The request
uses the resolved class/code and does not prime a hotness counter. Only the
explicit first-entry flat path calls it. Ordinary structural eager execution
keeps its synchronous policy outside this request.

The flat path polls completed results while its method is Compiling, so a loop
does not depend on an unrelated JIT entry to install the emitted body. This drain
does not wait for an unfinished compiler job. Existing worker-note draining in
jit_run remains a separate possible source of synchronous work.

Queue capacity/retry and other existing runtime history remain limitations of
the broader no-adapt objective; this experiment does not certify them. No cache
snapshot or prepared execution state is introduced. New files are under 500
lines; frontend state/animation requirements do not apply.

Validate exact lambda invocation counts, then the differential suite and an
explicitly exploratory boot. Existing early-JIT message failures must not be
reclassified as accepted divergences to promote the candidate.

Result: rejected for retention. Both intermediate and final builds pass all 47
differential probes with existing divergences. A separate JIT-stats run of
JitRequestSem confirms 6968 compiled lambda executions, compile_ok=1, and the
correct total 8192 guest calls/sum. All five product hashes pass. Short NPE probes
passing asynchronously do not resolve the known compiled message differences.

Final uninstrumented candidate boot: 25.701 s. Subsequent null-first interleaving:

| Run | Binary | Wall seconds | CPU seconds |
| --- | --- | ---: | ---: |
| null0 | control | 26.655 | 44.546875 |
| null1 | control | 25.823 | 43.4375 |
| candidate0 | candidate | 25.396 | 43.328125 |
| control0 | control | 25.980 | 43.046875 |
| control1 | control | 26.024 | 43.8125 |
| candidate1 | candidate | 28.934 | 48.1875 |

The first pair favours candidate by 0.584 s, less than the 0.832 s control
null spread; the second favours control by 2.910 s. There is no stable gain.
All boots reached Done, but quiet-counter sampling was unavailable. No source
changes, builds, semantic tests or hash gates ran concurrently with these boots.
The cause of the candidate's wider variation is unproven; investigate plan work,
code installation and compile scheduling before adding more background coverage.

Candidate SHA256:
`DA3E87394349BA4C780052C7358D9CD1A06F0D3F43DD6C32DDF42D2C4A104133`.
Control SHA256:
`7C7221E480F4E421DACC613DC1DDB6224B201626DD5CC61202324C4CE10DFEAD`.
Artifacts, including reversible runtime/test patch:
`%TEMP%/rava-lambda-background-4d115b5a135841469ee75e51f4e61064`.
The runtime patch and new probe were removed; normal release was restored to
the control. The complete wall <5 s, no-adapt, no-snapshot objective remains unmet.
