# Prepared direct-call overlap with flatcall

Status: measured without a useful gain and removed from the runtime. The
experiment and dedicated route-check script remain available in commit
05e80bb4. AotFlatSem.java remains as a semantic regression probe.

The matched no-JIT pair improved wall but increased CPU. The prepared direct
route precedes the general invocation route, which already runs trivial bodies
without an interpreter frame. Test this overlap before assuming that every
prepared invocation is cheaper.

RAVA_AOT_PREFER_FLAT is experimental and off by default. When enabled, only
bodies recognized by the existing flatcall classifier as a getter, constant,
void return or local-zero return defer to the general route. Require flatcall
enabled and no exception handlers. No observed receiver, method name, hot count
or result selects eligibility. The same executable and prepared code can run
both arms, differing only in this flag.

Validate getter and identity semantics plus actual prepared caller entry, then
measure full boot in both modes. Keep the flag off unless full evidence supports
the change; the unresolved RingLane failure remains a separate correctness issue.

58 AOT unit tests pass. The release AotFlatSem probe matches Java in three modes:
normal prepared routing, prefer-flat, and prefer-flat with flatcall disabled.
The caller enters prepared code in every mode. Getter and identity callees enter
the AOT runner only in the first and third modes, confirming the routing change.
Evidence: `%TEMP%/rava-flat-overlap-3866d5fbdbb74f4e903f0d3a770e8fd7`.

Full preparation and semantic validation completed for release SHA256
`ACC515879C0B0E627E450A39AC317BD6A682FC51B703F5DB598ED11ED4FF3EE8`:
30461 classes, 292540 compiled, 56526 unsupported. All 38 semantic probes match
Java with RAVA_JIT=0 and RAVA_AOT_PREFER_FLAT=1. The two sequential boot arms
use this same executable and prepared directory, fresh world copies and no
profiling. Results are stored under
`%TEMP%/rava-aot-flat-server-c46d77e8c18440b7b72222d7852d9afb`.

Normal routing: 149600 ms wall, CPU 146.9375 s, RSS 1023430656 bytes;
boot directory suffix da0be029b249406f836c584ad0ec14fe.
Prefer-flat: 149950 ms wall, CPU 147.859375 s, RSS 923963392 bytes;
boot directory suffix 1e568d6950694c4796a7e6b3d226f8f0.
Both directories are under `%TEMP%/rava-product-boot-`.
Environment settings match except RAVA_AOT_PREFER_FLAT. Neither run overlaps an
owned heavy process. Idle counters were unavailable. Normal routing logged a
failed Yggdrasil public-key request, so wall can include external I/O variability.
The differences (+0.23% wall, +0.63% CPU) do not establish a precise regression,
but show no gain worth retaining this extra routing switch. The experiment is
removed; the broader call-boundary problem and the <5-second goal remain open.
