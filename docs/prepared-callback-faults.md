# Prepared runtime callback exception exits

The call-exit fix does not cover array bounds/store failures, cast failures,
allocation failures or class/static resolution failures. These callbacks can
set an exception and return placeholder bits while subsequent bytecode resumes.

Reuse the existing non-allocating pending query immediately after these
callbacks, before reloading roots or consuming their results. Retain early
null exits, direct arithmetic exits, and call-free numeric/object inlining.
No class names, runtime history or expected outputs select the execution path.

The machine-code regression covers all eight array loads, all eight stores,
new/newarray/anewarray, class ldc, getstatic, checkcast and instanceof. Inject
exceptions through the real VM pending slot, require preceding writes to remain,
following writes to be skipped and the exception to remain available. Normal
callbacks must still continue. Run the full AOT suite after the change.

This is compiler-correctness work required before widening native execution.
It does not establish full server correctness or a startup speedup. The prior
release's Defender detection and the full five-second target remain unresolved.

The unmodified machine code failed on the first array-load fault: it returned
the later constant 9. After inserting callback exits, all 23 operation cases
pass for both normal and exceptional returns, with the real VM pending-state
query and preserved exception value. The complete AOT-selected suite passes
(65 passed, one ignored), including existing moving-GC tests. Logs:
`%TEMP%/rava-callback-fault-before.log` and
`%TEMP%/rava-callback-fault-after.log`. Advance the full-method cache namespace
so old prepared code cannot be used for the new exception contract.

The ordinary release build of commit 20a3a08d completed. SHA256:
`B3E74E005794032D665CCE34D097024BC6854849BB5FF888AA38E8D8BAC152CB`.
Its prepared dispatch probe matches Java, passes the 64 KiB nursery/actual-GC
check, and confirms target memo reuse/invalidation (2 resolutions per 1000
entries for each alternating receiver class). Evidence:
`%TEMP%/rava-aot-dispatch-6c654124cf704c80ac500f1608607c7d`.
The new revision ran under existing protection settings; this does not resolve
or invalidate the earlier release's malware classification.

Full preparation, all 38 semantic probes and the 30-second cost diagnostic
completed successfully under
`%TEMP%/rava-callback-exits-server-f753a5ce6c924346b499d0e0a71d5805`.
Preparation covered 30,462 classes, compiling 292,541 methods and refusing
56,526. See prepared-runtime-costs.md for the cost diagnostic. An uninstrumented
boot is now running from that frozen release. No full product hash-suite result
or new wall measurement is established by the semantic probes.

The uninstrumented boot completed with JIT disabled, prepared objects and the
resolved-target memo enabled: process-to-Done 145,565 ms, CPU 143.5 seconds,
peak RSS 1,032,155,136 bytes. Evidence:
`%TEMP%/rava-product-boot-87a55938156347a19179128eee038035`.
Its environment and binary hash match this release. No diagnostic, checkpoint
or restore flags were present. Quiet-box counters were unavailable. The guest
Done interval of 0.712 seconds is not process wall. This is a new observation,
not a paired price of the exception fixes; no five-second result is established.

The earlier ReloadBench timeout is being rechecked with current benchmark
sources, JIT disabled and the same frozen release/objects. Evidence will be in
`validation-current-reload` under this release root. Inspect its live process
or final log before repeating the run; the product hash is not yet certified.

That ReloadBench run exhausted the 600-second harness deadline. The harness
killed only its owned process; no cold or warm digest was emitted. The exception
fixes therefore have not eliminated the observed timeout, and do not certify
this product gate. A follow-up uses the benchmark's existing --trace-phases
option and a separate validation-phase-reload directory to locate the remaining
stage. Its phase durations will be diagnostic, not a startup speed measurement.

The phase trace reached registries.start at epoch 1788621550991 and cold.start
at 1788621566587 (15.596 seconds between those markers). At the 17:23 Budapest
observation it had not emitted cold.end, while its owned process remained live.
The leading thread had consumed almost all observed process CPU. These facts
locate the unresolved delay inside construction/draining of the cold resource
reload, not its later warm reload or digest. They do not distinguish useful
computation from active waiting, and are not a terminal result for this run.

The phase-traced run subsequently hit the same 600-second deadline, still
without cold.end. Its failure is preserved in reload-phases.failure.txt; the
owned process was terminated by the harness. The cell-pool pipeline then moved
on to its own preparation and validation, rather than restarting reload.

The existing marker spans both loadResources/continuation construction and
GameQueue.drain. Add cold.construct.end and warm.construct.end just before the
respective drain calls, gated by the existing --trace-phases option. These
markers do not change resource loading, digest inputs or the nontraced output.
ReloadBench.java remains 497 lines and compiles successfully. Fresh classes and
source/class hashes are preserved at
`%TEMP%/rava-reload-phases-e45d451114c64d66ac521d898eb037aa`.
They have not been executed yet. A future prepared run must include these
classes in its matching preparation; the current pool timing pipeline retains
its existing frozen classpath and must not be silently switched to this fixture.
