# First-entry memory failure is after frontend finalization

An opt-in inline-local pool was implemented with function-local stack leases,
disjoint nested ranges, unchanged initialization and explicit reference-result
rooting. Two lease tests and three existing stack-map contract tests passed.
InlinePoolSem also matched Java with pooling off and on under a 64 KiB nursery
and GC poisoning. The on arm recorded 88 minor collections, nested/branching
inline plans and 185 compiled executions of `exercise`.

That did not resolve first-entry CompSem's allocation failure at a 4096 MiB
process cap. The pool and first-entry switches were removed. Their source and
binary are preserved under `%TEMP%/rava-inline-pool-20260905`; no new optimization
was enabled by default. InlinePoolSem remains as reference-result GC coverage.

## Measured compiler boundary

`RAVA_TRACE_COMPILE_PHASES` now emits opt-in diagnostic markers after body
construction, after `FunctionBuilder::finalize`, and after `finish_compile`.
The diagnostic never selects compilation or execution. `finish_compile` clears
its context, so the last marker's zero counts are not the emitted method's size.

The same diagnostic binary was run sequentially with the pool off/on, with
first-entry scheduling and synchronous compilation enabled. Both exited
-1073740791 following allocation failure. Both reached frontend finalization for
the 40,095-byte initializer identified by the compile trace as Blocks.<clinit>:

| Pool | Body SSA values | Finalized SSA values | Blocks | Outcome |
| --- | ---: | ---: | ---: | --- |
| Off | 1,331,566 | 1,508,830 | 296,233 | Allocation failure before finish_compile returns |
| On | 1,331,566 | 1,509,264 | 296,233 | Same failure boundary |

The class name locates evidence only. This does not isolate a particular
optimizer or register allocator pass inside `finish_compile`. It does rule out
failure before frontend finalization for these two runs and does not support
shipping the local pool as a fix for this failure.

Evidence includes `phase-off/on.err`, environments, sampled process memory,
`phases-result.log`, and diagnostic binary SHA-256
`6418A0150FC8568FA887D6C4B9145D78B2FEAEA901B51961AB4102AD02262F9F`.
The earlier pool candidate was
`C441DBE227A3EF00C6C7A2DF1558898E03837DDF4D8A863E4AB73C9F31B7CD9A`.
Compiler-library references were checked through Context7 and installed
Cranelift 0.132.1 source; no dependency was patched.

## Next work

Design and test a structural IR-work budget before expensive final compilation,
using CFG/value counts rather than names, invocation history or previous OOMs.
Account for growth during frontend finalization and preserve ordinary interpreter
fallback on refusal. Then re-test first-entry correctness within the existing
memory cap before measuring its performance. The full wall-under-five-seconds,
no-adapt, no-snapshot goal remains unachieved.

The final diagnostics-only release builds successfully, SHA-256
`40D2B7E504E97F8DF568C631F8BF7FC0209189782827E810D5022B1EA479C992`.
InlinePoolSem matches Java with phase tracing enabled, 64 KiB nursery and GC
poisoning; it records 88 minor GCs. All three phase markers are observed. See
`final.env`, `final-gc.log`, `final-gc.err` and the preserved Java/VM output.
No full product speed result is claimed for this diagnostics-only change.
