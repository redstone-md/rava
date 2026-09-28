# Static register ABI experiment

The primitive register-ABI emitter was tried after direct-mono, which already
accepts ordinary static descriptors. Move the existing opt-in primitive route
before direct-mono, matching the reference route's ordering. The default stays off.

Preserve direct-mono inside the register route's rejected-entry branch. Rejection
must not turn a normal cached compiled call into permanent generic invocation.
The fallback uses its own compile-time operand-stack copy and passes forwarded
references into the outer CFG merge. Register eligibility still uses descriptors,
resolved bytecode, initialization state and the existing GC proof.

Validation: mixed-width/reference differential probe with inlining suppressed,
SET/UNSET correctness gates, then interleaved reload A/B after a null test.
Do not enable the default without a measured benefit.

## Executable route validation (2026-09-04 continuation)

Run `bench/static_abi_sem.ps1 -Exe <absolute-rava.exe>` and repeat with
`-NurseryKb 64`. The harness clears inherited RAVA variables, restores them on
exit, saves the executable SHA-256, environment, stdout, stderr and exit codes
under a unique temporary evidence directory. It checks ordered output against
Java and checks actual resolver diagnostics; interpreter-only success fails.

The original one-shot main loop produced no REFABI diagnostics. The probe now
invokes a batch repeatedly, and the diagnostic harness uses synchronous JIT,
threshold 16 and inline budget 0 to retain the call sites. These settings are
coverage controls, not production eligibility or performance evidence.

All five modes (unset, primitive, reference, both, both without direct-mono)
passed with the built WIP executable SHA-256
`1AB5B1B2D303EDA559FEC3F0C6BFD85A55C7AD95EF1E69460DDF62552923BEDF`.
The diagnostics confirm an accepted mixed-width primitive leaf, a rejected
primitive nonleaf, and an accepted deep reference entry. The probe retains two
references on the caller operand stack across allocating fallback calls.

An initial small-nursery pass did not collect (`minor=0`); it is not rooting
evidence. The final object-allocation probe requires nonzero minor collections
and enables GC poison. All five modes passed with 169 minor collections each.
Final evidence directories in `%TEMP%`:

- `rava-static-abi-574e5dbf7ef444eb8b12214936eb037a` (64 KiB, poison, GC counters).
- `rava-static-abi-9c170a9e3ccb48bda2aa177fb0d72989` (normal nursery).

The general semantic harness now rejects nonzero exit codes and enforces its
timeout on each child process. A deliberate one-second CompSem run failed with
both children reported TIMEOUT, rather than treating partial output as PASS.
The UNSET and PRIM+REF semantic suites both passed 26/26 with exit checks.
The SET suite used the enforced 300-second per-process timeout. All five product
hash gates passed with exit 0 in UNSET and PRIM+REF configurations (10 processes):
ChunkFill `8790f04ace726ecf`, RingScan/RingLane/RingLaneInstance
`37539c6c375d0c3`, NbtSave `ae44a34b42766bd4`. Sources and arguments are those of
`pgo_gates.ps1`; they ran in a separate temporary directory without its unused
world-directory mirror. Raw results: `%TEMP%/rava-abi-product-gates-20260904`.

The rebuilt interpreter unit test
`access_order_get_callbacks_abort_before_reordering_on_workers` passed
(1 passed, 552 filtered out). This checks the inherited map worker contract;
register route coverage is supplied by the differential probe above.

No new timing claim is made. Default ABI gates remain off.

## Repeated null (same continuation)

Eight ABBA runs of unused `RAVA_NULL_ABI_VERIFY`, with both ABI options absent,
all exited 0 and returned counts 1470/1584/625 and hash `c956377e19071d7a`.
No compiler build ran concurrently. The old crash did not reproduce in these
eight runs; its cause remains unresolved.

| Metric (ms) | A, unset | B, unused flag set |
| --- | --- | --- |
| Cold reload | 7582 / 7912 / 7785 / 9004 | 7972 / 7836 / 7953 / 8550 |
| Warm reload | 6928 / 6527 / 6503 / 7513 | 6941 / 6779 / 6725 / 7390 |
| Window | 13174 / 12864 / 12870 / 14205 | 13517 / 12860 / 12818 / 14331 |

A cold spread was 18.8%, warm spread 15.5%, CPU spread 9.6%. Null median B/A
was +1.45% cold and +1.97% warm. Process wall ranged 39.485-43.517 seconds.
Arms overlap and all runs are valid, but the spread does not support pricing a
small ABI effect. No ABI A/B or default flip follows from this series.

Raw stdout/stderr, process exits, cycles, wall, environments and the worktree
configuration are preserved in `%TEMP%/rava-abi-null-repeat-20260904`;
the summary is `%TEMP%/rava-abi-null-repeat.log`. The reload harness now saves
every run and rejects an incomplete batch instead of re-pairing survivors.
An intentional one-second timeout test returned exit 1 with both children
marked TIMEOUT and no aggregate timing claim.

Configuration check: the isolated worktree and the initial product-gate temp
directory resolve 16 threads and an 8192 MiB soft heap; main's file uses automatic
threads and a 4096 MiB soft heap. Both set OS/allocator caps to 4096/3584 via the
test environment. The null describes the isolated configuration, not main.
Product gates were then repeated with main's copied configuration and explicit
4096 MiB soft heap: all five gates passed in UNSET and PRIM+REF modes, all ten
processes exited 0 with the same expected hashes. Evidence (including copied
configuration, environment and executable hash):
`%TEMP%/rava-abi-product-gates-4096-20260904`.

## Remaining work

### 2026-09-05 full startup recheck: no default enablement

On current runtime commit `2f767fba`, binary SHA-256
`18985DD93BF325683874C8084A59E0FEE378A1B95799DD30DB65B562E3F4BD1A`,
the five-mode ABI differential probe passes again with nursery 64 KiB and GC
poison. Both-mode diagnostics report 169 minor collections. Evidence:
`%TEMP%/rava-static-abi-5fdd03387469461b8b4b68ac237d0ab8`.

Full fresh-process boot used the isolated world-copy harness, standing flags,
port 25599 and affinity 0x0FFF. The only experimental flags were `RAVA_PRIM_ABI=1`
and `RAVA_REF_ABI=1` together. Threshold and inline-budget coverage controls from
the semantic probe were absent. All four runs reached the full Done line.

| Order | ABI | Wall to Done | CPU | Evidence suffix |
| --- | --- | ---: | ---: | --- |
| 1 | both on | 31,681 ms | 33.859375 s | `b7395b3c7f9f46c0bce7dab69a6cc65b` |
| 2 | unset | 30,735 ms | 32.968750 s | `51d4a04c812a499c8abd2a18157e0de5` |
| 3 | unset | 31,689 ms | 33.359375 s | `2e5a4c31b4664aefb5e37633e7202209` |
| 4 | both on | 32,915 ms | 35.218750 s | `7cf1fb7851fa4a3c9fae6fdb28f35f83` |

Directories are `%TEMP%/rava-product-boot-<suffix>`. Host quiet counters remain
unavailable, arms overlap, and this small series is not a precise regression
estimate. Neither opposite-order pair shows a win; default enablement is rejected.
This is actual process launch to Done, not a ReloadBench measurement.

A separate full boot with `RAVA_DBG_REFABI=1` confirms the route is active under
ordinary threshold/inlining settings: 149 accepted resolver outcomes (145 nonleaf,
4 leaf) and 13 refused nonleaf outcomes. Evidence:
`%TEMP%/rava-product-boot-38606fb672a8435bb075b2d08348219a`, including parsed
`abi-coverage.json`. Its instrumented timing is excluded from the comparison.
These counts establish reached routes, not call frequency, time saved, or the
reason for the timing differences. No runtime default changed.

The static ABI route correction is committed and correctness-checked; the current
full-startup experiment does not justify enabling it. Wall under five seconds
remains unachieved. The
baseline crash is still open despite eight successful reload repetitions.
Next establish a focused reproducer for the deferred-initialization/nursery
ordering. Any future claim of a small ABI benefit needs repeatably stable null
measurements as well as a materially different implementation or hypothesis.
Do not treat the diagnostic threshold/inline settings as production tuning, or
merge the experiment into main on the strength of correctness checks alone.

### Baseline crash investigation lead (not established as the cause)

Source inspection found that `gilfree_cf/batch.rs` drains deferred class
initialization after workers join but before `young_set_top` retreats the
nursery. Guest initialization can allocate or collect. This ordering deserves a
focused reproducer: allocations published by initialization must not be
invalidated by the later region retreat. The old crash log alone does not prove
this path initialized a class or caused that crash. No fix is included here.

## Session null test (2026-09-04)

Baseline executable: map-access-order fix, before ABI experiment compilation.
Four ABBA pairs with unused `RAVA_NULL_ABI_20260904`:

- RELOAD_MS A: 7866, 7971, 7911; B: 7985, 7908, 7633, 8589.
- RELOAD2_MS A: 6713, 6953, 6818; B: 8851, 8067, 7160, 7664.
- CPU A spread: 24.2%; harness marked the batch not level.
- Final A crashed (access violation, compiled stack JsonObject.add then
  ReloadableServerRegistries.lambda$scheduleRegistryLoad$3). It is not a timing
  data point; the complete stderr is in bench/reloadab_discard_8_A.log.err.

The null produced disjoint warm arms with no semantic flag difference. Therefore
disjointness alone is not sufficient evidence on this session's machine. No
performance conclusion is permitted from this batch. Investigate the crash and
re-establish measurement stability before pricing or enabling the ABI default.
