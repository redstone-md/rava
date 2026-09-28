# Register entry compilation cost

2026-09-06, baseline `427e02ee`. The preceding primitive non-leaf ABI experiment
showed no consistent wall gain. A possible explanation was duplicate compilation:
register entries have a separate cache and compile a separate body. This run
measured that explanation rather than assuming its cost.

Temporarily restored the primitive deep-entry change and added one existing
phase-tracer span around compile_reg_entry (including its planning/refusals).
No new profiler, tests or admission policy were introduced. Built release-diag
with next-gen-gc, without wall-profile. Enabled PRIM_ABI and buffered phase trace,
ordinary first-entry/cap16384 configuration, fresh fixture, no PC sampling.

Evidence: `%TEMP%/rava-register-phases-cb70546bafb14636b4fcc0398727b7cd`.
Run: `%TEMP%/rava-product-boot-bd371412903a426ab692ffbfc0fa4c41`.
Executable SHA256: `56FACD482CF800974B3536C618458E73433141BCB08607CCCBAFB93F6936D646`.
Includes binary, diagnostic.patch, build/boot logs, report.json, intervals.json
and overlap.json. Existing analyzer validated no trace drops/errors and clipped
intervals to launch/Done. The harness drained the writer for 500 ms after Done.

Register-entry preparation: 16 attempts, 16 unique method keys, **12.771 ms** total
on the main thread. This includes refusals, not just successful compilation.
Normal main-thread plans: 318.164 ms; fields: 9.894 ms; publication: 13.166 ms;
synchronous codegen: 156.078 ms; waits: 229.284 ms. Union of all measured
main-thread phases, including register entries: **737.842 ms**.

Four compiler threads completed 8074 background attempts in total; their elapsed
interval sums were 4691.387 / 4731.181 / 4918.209 / 4773.984 ms. They overlap and
must not be added to startup wall. Instrumented wall was 26.807 s, which is not a
new uninstrumented speed result. Quiet counters were unavailable.

The measured cost rules out register-body preparation as a seconds-scale direct
cost in this run. It does not establish which execution costs cancel any ABI
savings, or price other ABI configurations. Do not pursue shared-body wrappers
on the claim that this duplicate compilation accounts for the missing seconds.

Temporary runtime instrumentation and experiment were reverted. Normal release
remained the validated control `659A37D6...`; no production runtime edit retained.
The full performance/no-adapt/no-snapshot objective is still unmet.
