# GAP-S09 closure plan: typed `WorkerContext`

> **STATUS: COMPLETE (2026-07-21) — historical design record.** All five
> stages landed: stage 1 `a55bb3f`, stage 2 `b026887`, stage 3 `65eae35`,
> stage 4 `f843567`, stage 5 (interp-arm quarantine + `RAVA_FORK_COLUMN`
> retirement + register closure) in the closing commit. GAP-S09 is CLOSED in
> `runtime-gap-register.md`; residual typed-interpreter hygiene is re-filed
> under GAP-A02. Line numbers below are as-audited at `0328d01` and have
> drifted.

Derived 2026-07-21 from a full worker-surface audit (anchors verified at
commit `0328d01`). Authoritative acceptance list lives in
`runtime-gap-register.md` (GAP-S09). Every stage keeps the GIL-held/serial
path byte-identical, is hash-gated at `8790f04ace726ecf`, and is individually
revertible.

## Worker surfaces carrying `*mut Vm` today

| kind | payload | worker body | notes |
|---|---|---|---|
| A. apply-leaf fork | `Sh.vmptr` lib.rs:12788 | `set_current_vm` + `jit::call_raw` only (lib.rs:12812/12838) | all Vm access via CURRENT_VM callbacks |
| B. S2 chunk fill | lib.rs:13237 | compiled arm 13320-13426 (`install_worker_jit_ctx`, `jit_direct_call`, locked `jit_call`); interp arm 13427+ | gated RAVA_M3_* |
| C. counted loop | lib.rs:13740 | full interp on proven bodies (13787) | gated |
| D. column fork | lib.rs:13948 | full interp (13971) | gated, superseded by S2 — candidate for retirement |
| E. parallel_pool | — | already Vm-free extern "C" bodies | end-state model |
| F. GIL-held handles | task_runtime.rs:122, lib.rs:5578/6123, jit_run 8422/9119 | typed in stage 1 as `GilVmToken`; not gil-free | GAP-A02 hygiene |

## LIVE HOLES found by the audit

1. `rava_putstatic` (jit.rs:1717) on a gil-free worker is only COUNTED
   (jit.rs:1727), not aborted — statics are not COW-shadowed. Stage 3 makes it
   an abort stub.
2. Kind-A workers never install `WorkerJitCtxGuard`, so `set_jit_fault`
   writes the SHARED `JitCtx.fault` latch (heap.rs:658). Stage 2 installs the
   guard (mirrors lib.rs:13316).

## Target shape

`parallel::WorkerContext` (upgrade the dormant struct at lib.rs:1051):
FROZEN `FrozenVm(*const Vm)` (loader/jit_cache/heap-registry READS) +
`WorkerJitCtxGuard` (private fault latch + COW bloom + published bases) +
per-worker TLAB/arena-range RAII + the abort channel. Callback triage:
- re-pointable to `&Vm`+TLS: field/array/acmp/div/set_pending/str/safepoint/
  base-address callbacks, dispatch fast paths (freeze table, frozen direct cache);
- already abort-before-publication: getstatic worker miss, newarray region
  exhaustion, uncompiled-callee guard, ClinitPending;
- must become worker abort stubs: putstatic, indy, cold ldc, fu_invoke,
  map/amap mutators, enum/bitset receiver-mut helpers, anewarray outside the
  reserved region (honor `worker_locked_exec_enabled()` exactly like
  jit.rs:1650 so S2 locked-exec keeps working).

## Stages (all DONE)

1. **`WorkerVmRef` newtype** (mechanical, zero behavior): replace the four
   fork payload pointers; today's surface = `install_current()` + one loud
   `unsafe fn vm_mut_unsound_interp()` at the three interp sites
   (13312/13772/13971) — every `&mut` use becomes grep-enumerable. Type the
   GIL-held handles as `GilVmToken` (task_runtime.rs:122, lib.rs:6123).
   Tests: full release gate + canonical hash + S2 four-worker acceptance
   telemetry (92,160 shadow puts / zero shared puts).
2. **`jit::vm_shared()` split + guards + census**: migrate the re-pointable
   callback group to `&Vm`+TLS; install `WorkerJitCtxGuard` in kind-A workers
   (closes hole 2; gate + A/B traced runs); extend `debug_assert_not_gilfree`
   to every frozen-table mutation site in gil-free-vm-layout.md (cnp/aot/
   jit_direct_cache/neg, pure_value_override, new_site_cache, class_layout,
   numeric_probe_seen, indy_singletons, bool_cache, write_static_cell,
   site_cache); add `RAVA_S09_CB_AUDIT` census of callbacks firing under
   `gilfree()`.
3. **Abort stubs** for the worker-illegal callbacks (closes hole 1), gated on
   the stage-2 census showing zero hits on acceptance runs; extend the
   `s09_workers_do_zero_resolution_or_compilation` counters; negative tests
   (synthetic worker putstatic/indy aborts without publication).
4. **Real `WorkerContext`**: kind-A + S2-compiled workers consume it;
   `jit_direct_call` gets a `&self` worker arm (the gilfree path already
   mutates nothing, lib.rs:9611-9614); `vm_ref()` becomes mode-aware (worker
   mode → request abort + None, debug panic naming the callback);
   `set_current_vm` carries a checked mode tag.
5. **Interp-arm quarantine + closure**: B-interp/C/D constructible only via
   `vm_mut_unsound_interp` inside their env gates, documented outside the S09
   claim (or retire RAVA_FORK_COLUMN); final acceptance per the register;
   GAP-S09 → CLOSED, residuals → GAP-A02.
   *As landed:* B-interp quarantined behind the new `RAVA_M3_CHUNK_FILL_INTERP`
   gate, C behind its existing `RAVA_FORK_NUMERIC` gate (QUARANTINE doc blocks
   at both arms name GAP-S09 and the register); D (`RAVA_FORK_COLUMN`) was
   RETIRED — `try_fork_column`, its opcode hook, the `fork_column`/
   `fork_column_min_n` gates and both CLI aliases deleted (superseded by S2;
   the S2 engagement floor keeps the inherited 512-element do-no-harm
   constant).

Order strict 1→4; 5 after 3.
