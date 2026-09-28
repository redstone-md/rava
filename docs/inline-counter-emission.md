# Inline counter emission experiment

2026-09-06, baseline e3f07bc9 including the ThreadLocal storage fix.
The candidate suppressed collection and iterator inline counter instructions
unless tests, callback statistics, PC profiling or the wall sampler were enabled.
The condition was evaluated during compilation, not on every guest call.

Evidence: `%TEMP%/rava-inline-counters-8052c51e814d420da8b0fabc81eba8eb`.
Frozen executables, candidate.patch, build, semantic and six boot logs are saved.
Control SHA256: `05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.
Candidate SHA256: `9455963AC0E6BE4C6F7631A5BBE5AB37501058AEE11F8CC6BBBFD7E3782CAF6C`.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 27.311 | 44.843750 |
| null1 control | 26.889 | 44.312500 |
| candidate0 | 27.391 | 45.734375 |
| control0 | 28.190 | 46.015625 |
| control1 | 26.742 | 44.359375 |
| candidate1 | 27.272 | 44.546875 |

Fresh worlds, first-entry JIT/cap16384, instrumentation off. All six reached
Done; their actual binary hashes and nonoverlapping launch/Done windows were
verified. Paired candidate minus control: -0.799 and +0.530 seconds.
Null spread: 0.422 seconds; paired controls drifted by 1.448 seconds.
There is no consistent speedup. Timing to Done is not a post-Done health check.

Existing IterSem matched 49 lines with two declared divergences excluded.
Existing JitIteratorSem matched two lines with no exclusions, with JIT statistics
enabled. These outputs do not establish inline hit counts on the boot workload.
No new tests were added. Runtime changes were reverted and the shared release
restored to the ThreadLocal-fixed control; the sub-five-second goal remains unmet.
