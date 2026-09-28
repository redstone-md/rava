# Handler-bearing direct-call refusal experiment

2026-09-06, baseline 497e0973. The candidate marked direct-resolution misses
permanent for static/special targets with nonempty Code exception tables, since
the current compiler unconditionally refuses those bodies. Already compiled
entries retained priority. Virtual/lambda cases retained transient retry because
their targets can depend on arguments not pinned by the negative-cache key.

Evidence: `%TEMP%/rava-handler-refusal-8368af512ec84136bc754a2d042907d0`.
Frozen binaries, candidate.patch, build, semantic and six boot logs are saved.
Control SHA256: `05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.
Candidate SHA256: `BC4EB06FB26DA607D7A6209CEA042068688ACF7C73C2892D73F224805EFB5B6C`.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 27.386 | 45.937500 |
| null1 control | 27.243 | 45.218750 |
| candidate0 | 27.773 | 45.812500 |
| control0 | 27.610 | 45.984375 |
| control1 | 26.712 | 44.718750 |
| candidate1 | 27.605 | 45.968750 |

First-entry JIT/cap16384, fresh worlds, no instrumentation. Actual executable
hashes, Done completion and nonoverlapping launch/Done windows verified for all
six runs. Null spread 0.143 seconds; paired deltas +0.163/+0.893 seconds;
paired control drift 0.898 seconds. No observed speedup; no claim of statistical
significance. These timings do not establish post-Done health.

Existing AotExceptionSem matched 13 lines against Java -Xint without exclusions.
This was a narrow exception-behavior check, not proof of refusal-path hit counts.
No new test or instrumentation was added. Runtime experiment reverted and shared
release restored to the ThreadLocal-fixed control. This avenue is rejected;
handler execution in JIT and the full sub-five-second/no-adapt objective remain open.
