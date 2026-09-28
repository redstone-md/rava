# Exact-class checkcast experiment

2026-09-06, baseline 7b9838f8. The candidate emitted a successful checkcast for
null or a raw aligned arena cell whose header class exactly matched the target's
published class ID. It emitted this path only when the target ID was already
available at compilation; it did not register classes or select receiver profiles.
Other receivers retained rava_checkcast. A pending fault forced the callback.
The cold arm forwarded live stack references through merge parameters after GC.
This did not implement general hierarchy checks or whole-program no-adapt.

Evidence: `%TEMP%/rava-exact-cast-94268cd765344e638ffc02308b3075a0`.
Frozen binaries, build/semantic/boot logs, candidate.patch and checked_cast.rs
are retained. The separate source file must accompany the patch for reproduction.
Control SHA256: `05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.
Candidate SHA256: `0F17FFBFBD5D70197BBC0A9D441DC4CCD6FC2EB0D3BCD48206F8DB8DDC3C478F`.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 27.469 | 45.265625 |
| null1 control | 27.291 | 44.812500 |
| candidate0 | 27.295 | 45.234375 |
| control0 | 27.469 | 45.609375 |
| control1 | 27.150 | 45.500000 |
| candidate1 | 27.167 | 45.421875 |

Fresh worlds, first-entry JIT/cap16384, instrumentation off. Verified actual
binary hashes, Done completion and nonoverlapping launch/Done intervals for all
six runs. Null spread 0.178 seconds; paired deltas -0.174/+0.017 seconds;
paired control drift 0.319 seconds. No supported wall improvement.

Existing AotExceptionSem matched 13 lines against Java -Xint without exclusions.
No new test was added. This narrow probe is not evidence of fast-arm hit counts
or a complete moving-GC validation. The candidate was rejected before broader
promotion checks. Timing to Done is not proof of post-Done health.

Runtime changes reverted and shared release restored to the ThreadLocal-fixed
control. Repeated micro-optimizations of host boundaries have not reduced the
roughly 27-second baseline; further work should target substantial compiled
execution coverage. The full sub-five-second/no-adapt/no-snapshot goal is unmet.
