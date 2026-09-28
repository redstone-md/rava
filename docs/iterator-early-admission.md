# Iterator early admission experiment

2026-09-06, baseline `dd51c11a`. Iterator inline admission requires a class word
published by an earlier fu_fastpath execution. This makes compilation depend on
execution order. The candidate initialized the three bare synthetic field IDs
at VM creation and removed the class-word prerequisite from emit-time readiness.
Generated code retained the live class-word check and zero-word fallback.

This addressed one admission dependency, not full no-adapt execution: the class
word was still published from a live receiver and other adaptive paths remained.
The candidate was built and exercised, then reverted because it did not show
consistent wall improvement. The current runtime still has this admission issue.

Evidence: `%TEMP%/rava-iterator-admission-757015956e8e4515a00a6d99b465bc7b`.
Includes frozen binaries, candidate.patch, build/semantic logs, compare.log and
verified-runs.json with binary hashes, environments and timing windows.

Control SHA256: `659A37D667531B6B0CE7C4F7C6AFC0E349FBAE8D9AB15DE49991D8E6CEF194C6`.
Candidate SHA256: `77E534149FDB175886AA3ED0262330ECE126F2CA5CBC1D3BA7051A08975964F2`.

Existing JitIteratorSem matched Java -Xint (two lines, no exclusions). IterSem
matched 49 lines with its two declared divergences excluded. No new tests were
added. These probes do not prove that every newly admitted site executed inline;
no machine-code path census was collected for this candidate.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 28.765 | 46.796875 |
| null1 control | 26.918 | 44.812500 |
| candidate0 | 27.243 | 45.234375 |
| control0 | 27.809 | 46.125000 |
| control1 | 27.427 | 45.093750 |
| candidate1 | 28.565 | 47.468750 |

All runs reached Done without instrumentation in fresh fixture copies, ordinary
first-entry settings and cap 16384. Actual executable hashes and nonoverlapping
launch/Done windows were checked; quiet counters were unavailable. Null spread
was 1.847 s. Paired candidate-minus-control deltas were -0.566 and +1.138 s.
No consistent speedup; source reverted and shared release restored to control.

Do not repeat early publication alone as a demonstrated wall lever. Removing
history-based admission still belongs to the goal, but this experiment gives no
evidence that the existing broadly admitted inline path reduces startup time.
