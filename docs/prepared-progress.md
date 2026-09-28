# Prepared cooperative progress

An isolated CompletableFuture polling loop returns 42 in the interpreter but
times out after ten seconds in prepared execution. Both main and await have
confirmed prepared entry; the asynchronous body never enters.

Add a per-invocation loop budget to full compiled methods. Every 1024 visits to
backedge targets, spill references to registered cells and offer a GIL handoff with the
same native-depth, pump and trampoline restrictions as the interpreter.
Reload references after the handoff. Eligibility does not use method names or
execution history. Append the runtime symbol and invalidate full code keys.

This first implementation prioritizes an executable diagnosis. Validate the
probe, reference survival, existing AOT tests and application workloads before
claiming a fix or performance gain. The original baseline is 27-30 seconds;
the target remains below five seconds without snapshots or adaptation.

## Validation

Release SHA256: `F4BBB61F743638FB55E14452E4E7A7F794EEC79B24A00E4EF6306FD893AE549B`.
All 74 AOT-selected library tests pass, with one ignored. A generated loop
retains an operand-stack reference across simulated forwarding and calls the
progress callback twice over 2050 iterations; straight-line methods retain
their callback-free relocation contract.

`bench/aot_progress_sem.ps1` passes Java/interpreter/prepared comparison with
actual asynchronous work, 20,000 worker allocations, observed arena GC and a
retained main-thread object. The prepared await entry is required explicitly.
Evidence: `%TEMP%/rava-aot-progress-ac860baa43ee4023b280a1e34d433cbb`.
The existing literal/class identity, initialization, GC and configuration
rejection probe also passes at
`%TEMP%/rava-aot-literals-6c2512b3f96146728443e85bf0089d6f`.

The matching full archive contains 30,466 classes, 311,894 compiled methods
and 37,186 refusals. All 40 Java differential probes pass. Evidence:
`%TEMP%/rava-progress-server-eee5f67c17854d29856587a8248af81c`.

The previously timing-out prepared ReloadBench now finishes with exit 0 in
174,460 ms. Both cold and warm results contain 1470 recipes, 1584 advancements,
625 tags and hash `c956377e19071d7a`. Cold/warm reload durations are 18,110 and
17,376 ms. This fixes application forward progress; it is still substantially
slower than the prior ordinary-JIT full ReloadBench reference of 47,681 ms.
Exact environment, phase trace and result are in the archive root's `reload/`.

Sequential fresh-world, uninstrumented server boots on the same executable:

| Mode | Wall s | CPU s | Artifact suffix |
| --- | ---: | ---: | --- |
| Prepared, JIT off, both caches on | 138.584 | 134.796875 | 326c3e11f2744ce480043fb701545e8f |
| Ordinary JIT, no prepared directory | 24.967 | 27.46875 | 28695e6385c84a48a53e9fc537333fe2 |

Artifacts are `%TEMP%/rava-product-boot-<suffix>`. No snapshots or restore;
benchmark port 25599, affinity 0x0FFF. Quiet counters were unavailable.
These single observations do not show a meaningful startup improvement over
the previous release's 139.206/24.842 seconds. The prepared result remains a
severe regression against the original 27-30 second baseline. This change
fixes forward progress, not the sub-five-second objective. Other product
workloads are not newly certified by the reload and boot results.
