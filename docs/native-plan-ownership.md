# Native plan ownership experiment

2026-09-06, source baseline `345b8507`. The execution profile identified the
compiled/native boundary as an execution cost. Its thread-local NativePlan table
cloned an Arc per served call. Replacing only the plan's Arc with Rc preserved
reentrant ownership while removing atomic reference count operations. Plan fields
shared with other subsystems remained Arc. No admission policy changed.

The candidate built in ordinary release with next-gen-gc. Existing CollSem
(36 lines) and NativeDeclineSem (one line) matched Java -Xint with no declared
divergences excluded. No new tests or broad suite runs were added.

Evidence: `%TEMP%/rava-native-plan-rc-c16b5e43932d4db18fa5953597cac837`.
The directory contains frozen binaries, candidate.patch, build/semantic logs,
comparison logs and verified-runs.json. Only the two named semantic probes were
executed; the copied shared semantic directory also contains older artifacts.

Control SHA256: `659A37D667531B6B0CE7C4F7C6AFC0E349FBAE8D9AB15DE49991D8E6CEF194C6`.
Candidate SHA256: `6715F609D59A759AC029AF01686B618E291F4CFF91F658D50BB8E690C275A607`.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 26.955 | 44.843750 |
| null1 control | 26.893 | 45.765625 |
| candidate0 | 27.476 | 45.546875 |
| control0 | 26.946 | 45.250000 |
| control1 | 26.139 | 43.843750 |
| candidate1 | 26.023 | 43.812500 |

All six runs reached Done in fresh fixture copies with ordinary first-entry
configuration, cap 16384 and instrumentation off. Executable hashes and
nonoverlapping launch/Done windows were checked. Quiet counters were unavailable.
Null spread was 0.062 s, but subsequent control drift was 0.807 s. Paired candidate
minus control deltas were +0.530 and -0.116 s: no consistent speedup.

The runtime edit was reverted and shared release restored to the control hash.
This result does not price the full native boundary or rule out structural
dispatch changes; it rejects reference counting alone as a demonstrated wall
lever. The below-five-second, no-adapt, no-snapshot objective remains unmet.
