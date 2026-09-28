# Prepared binding failures and execution eligibility

The per-VM AOT cache previously stored `None` for both unsupported bytecode
and failed runtime linking. `field_declaring_id` can fail because a field owner
is not yet loadable. A later synthetic class definition invalidates loader
metadata, but could not repair this permanent negative AOT entry.

Compilation/linking now distinguishes immutable refusal (`Ok(None)`) from
binding failure (`Err(())`). Only immutable refusal and successfully linked code
enter the cache. Explicit prepared-object read errors other than missing keys,
and malformed prepared objects, also remain retryable; preparation is still
read-only and no runtime compiler is invoked in prepared mode. Missing methods
in the fixed prepared set retain negative caching.

The regression exercises actual VM method admission and native execution:
an initial call cannot bind a missing field owner, then a class definition
provides that field and the identical method executes successfully. It also
checks that the failed call did not populate the method cache. This establishes
recovery, not whole-program no-adapt compliance or a performance improvement.

Validation: the regression passes, and all 68 AOT-selected library tests pass
(one ignored). Logs: `%TEMP%/rava-link-retry-test.log` and
`%TEMP%/rava-link-retry-aot-tests.log`. The regression uses the in-process compiler
with first-call admission to exercise the shared method cache; it does not
inject prepared archive I/O errors. Release build succeeded in 1m02s and the
prepared dispatch probe passes with both optional caches enabled, including
actual GC in a 64 KiB nursery, direct entries and loader-epoch invalidation.
Evidence: `%TEMP%/rava-aot-dispatch-167073bdebb54b4ba603e8316962b160`,
with executable identity in `binary.json`; logs are
`%TEMP%/rava-link-retry-build.log` and `%TEMP%/rava-link-retry-dispatch.log`.
Full preparation and all 38 Java differential probes pass in
`%TEMP%/rava-link-retry-server-c13b87e9a9044722a17ee849f591cf01`.
The uninstrumented JIT-disabled server boot with both optional caches enabled
reached Done in 145.761 s wall, 141.968750 s CPU, peak RSS 1096568832 bytes.
Artifact: `%TEMP%/rava-product-boot-5978b9edead74bceb2629de7e5566270`.
Quiet-box counters were unavailable. This is not a paired speedup measurement.

The frozen ReloadBench fixture includes the construction/draining markers.
Cold construction began at epoch ms 1788625103707 and ended at 1788625103713:
6 ms. Subsequent work is inside GameQueue.drain, which includes future
observation, polling and executing queued tasks. This does not yet distinguish
useful computation from spinning or a stalled future. Reload validation is
terminal: the owned process reached the 600-second deadline without cold.end
or either reload hash and was stopped by the harness. This remains a failed
product gate, not successful reload certification. The full wall target remains
below five seconds.
