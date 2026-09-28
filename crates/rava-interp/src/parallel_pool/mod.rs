//! Kit 1: GIL-free work-stealing thread pool for data-parallel bytecode regions.
//!
//! Kit 1 — GIL-FREE work-stealing thread pool for data-parallel bytecode regions.
//!
//! The Speed Shield made the single-core hot path callback-free; this is the first
//! frame of the multi-core phase. A `RavaPool` owns N worker threads coordinated by
//! a crossbeam-deque work-stealing scheduler (a global `Injector` + per-worker
//! `Worker`/`Stealer`). `parallel_for` chops a `[start, end)` index range into
//! chunks, injects them, and drains them across the workers — each worker running
//! with the `crate::parallel` region/safepoint protocol and an ISOLATED thread-local
//! TLAB (the `nextgen` nursery is already `thread_local!`), so disjoint iterations
//! allocate with NO shared alloc-pointer contention and WITHOUT holding the GIL.
//!
//! Soundness model (why this is safe even though the collector is stop-the-world):
//!   * Workers never JIT-compile and never drive the green scheduler/GC (they set
//!     `gilfree`); they only run pure, side-effect-disjoint bodies + allocate into
//!     their own nursery.
//!   * A worker whose nursery fills CANNOT collect on its own — Cheney evacuation
//!     needs every root parked. So instead of collecting it calls `abort_region()`:
//!     the coordinator tears the region down and the driver re-runs the loop
//!     SERIALLY under the GIL (where the normal collect-and-retry path applies).
//!     The body reports overflow by returning `Err(())` — modelled here so the
//!     fallback is exercised end-to-end before the real nursery hook lands.
//!
//! This is the framework (каркас): the pool, the coordinator, and the abort
//! fallback, fully tested. Wiring the real `nextgen` TLAB-overflow → `abort_region`
//! and the JIT `analyze_data_parallel_loop` emitter are the next increments; the
//! integration point is documented at `nursery_overflow_hook` below.



use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{gil, nextgen, parallel};

/// Default per-worker nursery budget for a parallel region (1 MiB) — enough for a
/// base workload before a worker must abort + fall back to the serial GIL path.
const DEFAULT_WORKER_NURSERY: usize = 1024 * 1024;

/// One unit of parallel work: a half-open index sub-range `[start, end)`.
#[derive(Clone, Copy, Debug)]
pub struct Chunk {
    pub start: i64,
    pub end: i64,
}

/// The region aborted mid-flight (a worker's TLAB overflowed GIL-free). The driver
/// must re-run the loop serially under the GIL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionAbort;

/// M3 9.2b GATE. When OFF (default — `RAVA_M3_CORE_READY` unset/`0`) the initCache parallel
/// executor is INERT: `fork_init_cache` returns `RegionAbort` immediately, so the shape-cache
/// warmup runs serially under the GIL — byte-identical (HASH=109205b3). Flip to `1` only once the
/// 9.2c Vm-access integration lands (see `fork_init_cache`). Cached one-time env read.
#[allow(dead_code)]
pub fn m3_core_ready() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_M3_CORE_READY").ok().as_deref() == Some("1"))
}

/// M3 9.2b SCAFFOLD (INERT) — one parallel shape-cache warmup: fork `BlockStateBase#initCache` over
/// an independent, cross-state-read-only set of block states (proven fork-safe by 9.2a — 0
/// cross-state writes over 29671 calls). Each state's Cache is built into the worker's OWN isolated
/// TLAB; the work-stealing partition + abort→serial fallback are the existing `RavaPool` machinery.
///
/// INTEGRATION GAP (9.2c, why this is inert today): the worker body must EXECUTE guest bytecode
/// (`initCache`), which needs `Vm` engine access — but the pool's live bodies are pure `extern "C"`
/// machine code over an `env` pointer (no `Vm`), and `Vm::invoke`/`jit_run` are `&mut self`. Running
/// initCache on 16 threads requires concurrent SHARED-IMMUTABLE `Vm` reads (loaded classes, method
/// code, field layouts) while ISOLATING engine mutation (jit_cache compile — already blocked on
/// workers via `gil::is_worker`; tlcache field cache; heap handle allocation; field-hint tables).
/// That threading of `Vm` is the unsolved step; until it lands the gate stays OFF.
#[allow(dead_code)]
pub struct ParallelInitCacheTask {
    /// The block-state receivers to warm (one independent task per state; arena addresses).
    pub states: Vec<i64>,
    /// Work-stealing partition chunk size (states per task).
    pub chunk: i64,
    /// Phase barrier — workers rendezvous here before the driver reads the warmed caches. (For the
    /// future resident-pool variant; the current scoped-spawn model uses thread-join as the barrier.)
    pub barrier: std::sync::Arc<std::sync::Barrier>,
}

/// A fused dual-accumulator reduction body: folds `[lo, hi)` into `out[0]` (acc1) and `out[1]`
/// (acc2) in one pass. `ok` → 0 on a region overflow (abort → serial fallback).
pub type ReduceBody2 = extern "C" fn(lo: i64, hi: i64, ok: *mut i32, env: *const i64, out: *mut i64);

/// The ABI a JIT-forked loop body targets (and the tests exercise with `extern "C"`
/// Rust bodies): fold the half-open trip-index sub-range `[lo, hi)` into a partial
/// accumulator and return it. `*ok` is set to 0 iff the body aborted mid-range (a
/// GIL-free TLAB overflow, which also raised `parallel::request_abort`); non-zero
/// means the partial is complete and valid.
/// `env` points to the marshalled invariant captures (the loop's external params
/// read inside the body), or null when the body captures nothing.
pub type ReduceBody = extern "C" fn(lo: i64, hi: i64, ok: *mut i32, env: *const i64) -> i64;

/// The machine-code body of a data-parallel MAP: for each `i in [lo, hi)` it writes the
/// element `out[i] = f(in[i], ...)` (disjoint indices → no combine, no return). Same `ok` /
/// `env` contract as `ReduceBody`. `env` carries the marshalled captures + the input AND
/// output array bases (resolved under the GIL before the fork).
pub type MapBody = extern "C" fn(lo: i64, hi: i64, ok: *mut i32, env: *const i64);

/// Tear the current region down: flip the local + global abort flags so every other
/// worker stops at its next safepoint/poll and the coordinator returns `RegionAbort`.
pub fn abort_region(abort: &AtomicBool) {
    abort.store(true, Ordering::Release);
    parallel::request_abort();
}

/// INTEGRATION POINT (next increment): the `nextgen` nursery-bump path calls this
/// when `parallel::gilfree()` is set and the TLAB is full, instead of running the
/// stop-the-world collector inline (which a GIL-free worker must never do). It flips
/// the process-wide region-abort flag; the active worker observes it on its next
/// `body` return / safepoint and unwinds the region. Exposed now so the alloc path
/// has a stable, single call site to wire.
#[inline]
pub fn nursery_overflow_hook() {
    parallel::request_abort();
}

/// Canonical crossbeam-deque work-stealing pick: local queue first (LIFO, cache-hot),
/// then a batch from the global injector, then a single steal from a sibling worker.
fn find_task<T>(local: &Worker<T>, global: &Injector<T>, stealers: &[Stealer<T>]) -> Option<T> {
    local.pop().or_else(|| {
        std::iter::repeat_with(|| {
            global
                .steal_batch_and_pop(local)
                .or_else(|| stealers.iter().map(|s| s.steal()).collect::<Steal<T>>())
        })
        .find(|s| !s.is_retry())
        .and_then(|s| s.success())
    })
}
mod pool_ops;
mod pool;
pub(crate) use pool::*;
#[cfg(test)]
mod tests;
