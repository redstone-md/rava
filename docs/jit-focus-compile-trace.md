# JIT compiler attribution after the focus change

The target remains full wall below five seconds, no adaptive eligibility and no
snapshots. Ordinary JIT at 25.148 seconds is the reference, not completion.

A fresh ordinary-JIT compile trace on release 6AA55437 completed full boot in
30.495 seconds (instrumented). There were 1735 completed driver scopes before
Done on five threads, with 7.066385 seconds of interval union and 15.501168
seconds of summed elapsed durations. These are elapsed scopes, not CPU time or
removable startup time. Evidence:
`%TEMP%/rava-product-boot-67d9ac13e7f04711b96b483b40b92e22`.

Several concurrent 9-12 byte methods showed roughly 200 ms durations. Trace
ordering exposed an observer problem: each phase synchronously writes stderr.
For example id 1255 records codegen.done at 10278 us, but that line is written
after another thread's end at roughly 203 ms; its own end is 206765 us.
This is consistent with logging contention or scheduling delays and does not
establish a 200 ms code-generation cost.

Buffer phase records per driver invocation and capture its end before any
formatting or stderr locking. Emit the complete record batch afterwards.
Disabled tracing still allocates nothing. Validate a fresh full trace with the
existing parser before using its intervals to select a compiler optimization.
The final write can still delay the next compilation and perturb scheduling;
this change does not turn the trace into an uninstrumented CPU profile.

## Validation

Release build passed with `next-gen-gc`. Executable SHA-256:
`CB4DFE4BDECEB5B668C58B2D87D34EBEFB2DE1E3CD2E8AEF8545E3AF8870CB8A`.
The buffered trace completed full boot in 27.226 seconds:
`%TEMP%/rava-product-boot-c1847fbebfa44ad8bd410c988a2ace41`.
The existing compile-trace parser found 1760 completed scopes before Done,
zero missing ends and zero outside-window scopes. Interval union was
2.487424 seconds; summed elapsed was 3.880480 seconds, including 3.238279
seconds attributed to codegen and 0.410141 seconds to finalize. The roughly
200 ms clusters disappeared. This validates the observer correction; it does
not establish a production speedup or exact compiler CPU consumption.
Both trace runs are instrumented despite the copied harness result field
`instrumented: false`: that field only checks its PC-profile switch. Their
recorded environments explicitly enable `RAVA_TRACE_COMPILE_PHASES`.

Sequential uninstrumented controls, with no prepared archive or snapshot:

| Executable | Wall to Done | CPU | Artifact suffix |
| --- | ---: | ---: | --- |
| New CB4DFE4B | 27.538 s | 30.046875 s | 1f44e74fa88e4055bd6d80b992fdb91a |
| Old 6AA55437 | 26.991 s | 28.640625 s | 74e66476f89b4fdd9611a6fa6fc67102 |

Each suffix belongs to `%TEMP%/rava-product-boot-<suffix>`. Quiet counters were
unavailable. This single sequential pair is not a repeated interleaved A/B
and establishes neither a speedup nor a causal regression. The original
27-30 second baseline remains the comparison target; below five seconds has
not been achieved. Ordinary JIT reference runs do not certify no-adapt eligibility.

Inspection of the JIT native bridge confirms that argument vectors are already
pooled, superclass chains and descriptor kinds already cached, and arena-string
canonicalization already avoids copying when no conversion is required. Prior
owner-dispatch-mask experiments were null. Do not repeat those optimizations
without new evidence. Remaining work is to locate avoidable runtime transitions
from compiled code while retaining GC rooting and representation guards.
