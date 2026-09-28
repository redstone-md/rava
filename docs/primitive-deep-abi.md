# Primitive non-leaf register ABI experiment

2026-09-06, source baseline `848108f6`. The primitive ABI rejected all non-leaf
methods, although reference signatures already used a self-registering deep
prologue. The candidate removed the primitive-specific refusals and retained the
same deep frame-size limit, mandatory self-registration and caller spill/reload.
No new ABI signatures or runtime flags were introduced. Existing flags stayed
default-off. This did not remove initialization/cache-dependent admission.

Evidence: `%TEMP%/rava-primitive-deep-a6129a73d1554bf096c125c10d0b26a9`.
It contains candidate.patch, frozen executables, build/semantic logs, copied
comparison harness and verified-runs.json. The boot harness enables PRIM_ABI only
for candidate.exe. REF_ABI is absent in every boot. Thus comparison prices the
expanded primitive ABI versus ordinary memory ABI, not just the two removed
refusals versus the old primitive flag.

Control SHA256: `659A37D667531B6B0CE7C4F7C6AFC0E349FBAE8D9AB15DE49991D8E6CEF194C6`.
Candidate SHA256: `14369FAD71EF52DA411C3481AC30C7FC1178A22A83E5A6D4BB07972625E4055B`.

The existing five-mode static ABI harness passed with nursery 64 KiB and GC
poison. Its candidate expectation required accepted non-leaf primitive resolution
instead of historical rejection. Evidence:
`%TEMP%/rava-static-abi-a4f8a50de4024b20ab3c86902eb8b6d9`.
Primitive-mode diagnostics show StaticAbiSem.nonleaf(IJFD)D accepted with the deep
pointer bit and 169 minor collections; all output matches Java -Xint. These are
route/GC checks under diagnostic threshold/inlining settings, not boot tuning.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 26.523 | 43.843750 |
| null1 control | 27.160 | 44.921875 |
| candidate0 | 28.178 | 47.328125 |
| control0 | 28.693 | 48.031250 |
| control1 | 27.581 | 46.578125 |
| candidate1 | 28.464 | 46.640625 |

Every run reached Done in a fresh fixture copy without instrumentation. Standard
first-entry/cap16384 settings were used; no build overlapped timing. Actual hashes,
flag selection and nonoverlapping launch/Done windows were verified. Quiet
counters were unavailable. Null spread: 0.637 s; subsequent control drift: 1.112 s.
Paired deltas: -0.515 and +0.883 s. There is no consistent wall benefit.

Runtime and harness changes were reverted, shared release restored to control.
No product hash suite was run after the performance rejection. This rejects this
expanded static-ABI configuration as a demonstrated startup optimization; it does
not establish the cost or benefit of a different virtual-call ABI implementation.
The below-five-second/no-adapt/no-snapshot goal remains unmet.
