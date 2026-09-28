//! Callback counts, rejections and the slot budgets a lane reports.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

#[inline]
pub(crate) fn in_lane() -> bool {
    crate::gilfree_cf::stats::LANE.with(|c| c.get().in_lane)
}

/// Something CONSUMED a pending exception inside this lane task
/// (`execution_state::pending_take` returning `Some`). Inert off the lane.
#[inline]
/// How many lane results arrived as a THREAD-LOCAL box-table index and had to be sent
/// back for a serial re-run. Every one of these was a wrong answer before the check
/// existed, so the count is the size of the hole that was being published.
pub(crate) fn note_boxed_result() {
    use std::sync::atomic::Ordering::Relaxed;
    pub(crate) static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, Relaxed) + 1;
    if crate::gilfree_cf::rings() && (n < 8 || n % 64 == 0) {
        eprintln!("[GILFREE-RINGS] boxed-result re-run #{n} (a thread-local box index cannot be published)");
    }
}

pub(crate) fn note_pending_taken() {
    if in_lane() {
        crate::gilfree_cf::stats::LANE_PENDING.with(|c| {
            let (s, t) = c.get();
            c.set((s, t + 1));
        });
    }
}

pub(crate) fn lane_pending_reset() {
    crate::gilfree_cf::stats::LANE_PENDING.with(|c| c.set((0, 0)));
    crate::gilfree_cf::stats::LANE_PENDING_CLASS.with(|c| *c.borrow_mut() = None);
    crate::gilfree_cf::stats::LANE.with(|c| { let mut s = c.get(); s.last_cb = usize::MAX; c.set(s) });
    crate::gilfree_cf::stats::LANE_ABORT_CB.with(|c| c.set(None));
}

/// A compiled-code callback of kind `i` (`jit::CB_NAMES`) was just entered on
/// this thread. Inert unless the rings lane is running here.
#[inline]
pub(crate) fn note_cb(i: usize) {
    // PRE-GATE, BEFORE ANY TLS. Shape before: an UNGATED thread-local read on all ~30 M
    // crossings of a boot — the one link in `jit::counters::cb`'s three-gate chain that was
    // not gate-ordered, while `callback_stats_enabled()` on one side and `s09census::note` on
    // the other both test a read-once `OnceLock` before touching anything per-thread.
    // Shape after: one relaxed load of `IN_REGION` plus one read-once bool, and the TLS chain
    // (`gs:[0x58]` → `_tls_index` → module TLS array → slot, four DEPENDENT loads on
    // windows-msvc) is reached only while some thread is actually inside a parallel region.
    //
    // `parallel::any_parallel()` is a strict superset of "some thread has `in_lane == true`":
    // `LaneWorker::enter` sets `in_lane` and then calls `enter_region()` before the compiled
    // body is ever invoked, and `LaneWorker::drop` clears `in_lane` before `leave_region()`.
    // An undercount affects diagnostics only; see `stats::fast_lanecb`.
    // Kill switch: `RAVA_NO_FAST_LANECB=1`.
    if !crate::parallel::any_parallel() && crate::gilfree_cf::stats::fast_lanecb() {
        return;
    }
    // ONE thread-local crossing for all three facts. This runs on every compiled
    // callback — 312,627,349 a boot — and used to take three.
    let armed = crate::gilfree_cf::stats::LANE.with(|c| {
        let mut s = c.get();
        if !s.in_lane {
            return false;
        }
        s.last_cb = i;
        s.cb_n += 1;
        c.set(s);
        true
    });
    if armed {
        // LANE PROBE: lift the per-task witness into a process-visible per-worker
        // slot. The witness is thread-local, so a task that never returns is
        // invisible from outside; the watchdog reads forward progress live.
        crate::lane_probe::note_cb(i);
    }
}

/// Process-wide compiled-callback total (all kinds). Only moves when
/// `RAVA_CALLBACK_STATS`/`RAVA_JIT_STATS` is armed — the serial yardstick print is a
/// diagnostic, so requiring its own gate is correct.
pub(crate) fn jit_cb_total() -> u64 {
    jit::CB_COUNTS.iter().map(|c| c.load(std::sync::atomic::Ordering::Relaxed)).sum()
}

/// One compiled-callback kind's process-wide total (`jit::CB_NAMES` ordinal).
pub(crate) fn jit_cb_kind(i: usize) -> u64 {
    jit::CB_COUNTS[i].load(std::sync::atomic::Ordering::Relaxed)
}

/// Fraction of the region's young budget held back as the SHARED OVERFLOW POOL
/// rather than pre-carved into per-task slices. A shared reserve is strictly more
/// efficient than padding every slice to the worst case: only the tasks that
/// actually overshoot draw on it. `RAVA_GILFREE_POOL_PCT` overrides.
pub(crate) fn pool_pct() -> usize {
    pub(crate) static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_GILFREE_POOL_PCT")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|p| *p < 100)
            .unwrap_or(45)
    })
}

pub(crate) fn obj_slots() -> usize {
    let n = CLAIMANTS.load(std::sync::atomic::Ordering::Relaxed).max(1);
    (OBJ_SLOTS_TOTAL / n).clamp(OBJ_SLOTS, OBJ_SLOTS_MAX)
}
pub(crate) fn field_box_slots() -> usize {
    let n = CLAIMANTS.load(std::sync::atomic::Ordering::Relaxed).max(1);
    (FIELD_BOX_TOTAL / n).clamp(FIELD_BOX_SLOTS, FIELD_BOX_MAX)
}

pub(crate) fn note_reject(owner: &str, name: &str, why: &'static str) {
    let map = REJECTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    *m.entry((format!("{owner}::{name}"), why)).or_insert(0) += 1;
}

pub(crate) fn reject(owner: &str, name: &str, why: &'static str) -> bool {
    note_reject(owner, name, why);
    false
}

pub(crate) fn reject_none<T>(owner: &str, name: &str, why: &'static str) -> Option<T> {
    note_reject(owner, name, why);
    None
}

/// Admit an off-GIL array store when the target array was allocated inside the
/// region's own pre-reserved slots (DEFAULT ON; `RAVA_NO_GILFREE_ARRAY_OWNED` opts out).
///
/// MEASURED, and the reason it is OFF: the ownership test is sound and it does
/// unblock the ring body — with it the first ring task stops dying a few callbacks
/// after its first array write and goes on executing the real computation. It then
/// converts a fast, safe abort into an UNBOUNDED GRIND, so the lane's usable default
/// is to keep aborting at the barrier (serial retry, `Done` unchanged).
///
/// THE GRIND IS NOT THE DISPATCH LOCK — that reading is superseded. Worker-side
/// measurement (`lane_probe`, `RAVA_LANE_PROBE`) puts the region at **99.3% lock-free
/// dispatch** (1,451,159 of 1,461,351 misses served by the freeze table / frozen
/// direct cache / WS-natives) and **1 ms of total lock contention across 15 outer
/// acquisitions** — better than the 96.6% the serial freeze census predicted, so the
/// census was right and the pilot warms enough. What actually happens is that the
/// task outgrows its 256 KiB `NURSERY_SLICE` mid-body and the compiled inline-alloc
/// RETRY LOOP (`jit::emit_inline_arena_new`) spins on `rava_arena_safepoint` forever:
/// that callback's gil-free arm refills the view from the region's SPILL POOL, the
/// rings lane never opens one (`parallel::spill_setup` has a single caller — the S2
/// chunk-fill fork), so the refill fails, the arm returns with the view still full,
/// and the loop — which polls no abort flag — re-enters immediately. Measured at
/// **7.8 M safepoint calls/s, 176,246,054 of one worker's 181,625,474 callbacks
/// (97.0%)**, one `request_abort_overflow` per iteration, on ONE thread while its
/// five siblings sit at 0.0 s CPU. Fixing this is an ALLOCATION-BUDGET job (open a
/// spill pool for the region, size the slice to the measured footprint) plus a
/// bounded exit on the compiled overflow arm — not a dispatch job.
pub(crate) fn array_owned_stores() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_GILFREE_ARRAY_OWNED").is_none())
}

/// Pre-reserved array slots per admitted task (`RAVA_GILFREE_ARR_PER_TASK`).
/// Exhaustion is SAFE (the worker aborts to the serial retry), so this trades
/// reservation memory — ~65 bytes of placeholder per slot — against abort rate.
///
/// MEASURED: the pilot yardstick puts ONE ring task at 2,738 `newarray`s, so the
/// old 2048 default ran the region's shared reservation dry at task ~80 of 127.
/// Every task past that took `NullPointerException: array` out of a compiled body
/// and the keep-all vote discarded the entire batch — 145 s of real off-GIL work
/// thrown away because the reservation was 25% short.
pub(crate) fn arr_slots_per_task() -> usize {
    pub(crate) static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_GILFREE_ARR_PER_TASK")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(4096)
    })
}

pub(crate) fn note_abort_site(loc: &'static std::panic::Location<'static>) {
    let map = ABORT_SITES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    *m.entry((loc.file(), loc.line())).or_insert(0) += 1;
}
