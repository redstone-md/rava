//! Which FROZEN shared-VM sites a gil-free worker actually reaches (RAVA_FREEZE_TALLY).
//!
//! Which FROZEN shared-VM sites a gil-free worker actually reaches (`RAVA_FREEZE_TALLY=1`).
//!
//! # Why this exists
//!
//! The gil-free lane's ceiling is not cache coldness — measured, the worker's lock-free arms
//! serve **99.21%** of calls (84.8M freeze-table + 198.6M direct-cache + 6.3M WS-native hits
//! against 2.29M misses). It is the LIFETIME of one guard: `worker_locked_exec` takes the global
//! resolve lock and holds it across the callee's entire EXECUTION, so the 0.79% of calls that
//! miss serialise ~82% of the region (`hold_ms=57582` of a 70s window).
//!
//! That guard exists because the general path *might* mutate shared `Vm` state. But the frozen
//! set is bounded and already enumerated — twenty named sites carrying
//! `Vm::debug_assert_not_gilfree`. Crucially they are not all alike:
//!
//! * Most are **cache fills** (`offload_cache`, `dispatch_closure_cache`, `class_layout_cache`,
//!   `jit_direct_cache`, `jit_cache`, …). Skipping the insert is semantically NEUTRAL — the
//!   result is recomputed instead of memoised. A worker can run these frozen.
//! * A few are **real state** (`Heap::write_static_cell`, `indy_singletons`). Skipping those is
//!   not neutral, so they must lock or abort.
//!
//! So the question the lane turns on is empirical: **which of the twenty does the ring reach?**
//! If only fillable ones, the global lock can be replaced by "execute frozen" and the hold
//! collapses. If a non-fillable one, this tally names it, and that site is the arc's real work.
//!
//! Counts, not timings — contention cannot distort them, and this box's wall figures spread ±30%.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FREEZE_TALLY").is_some())
}

static TOTAL: AtomicU64 = AtomicU64::new(0);

fn table() -> &'static Mutex<HashMap<&'static str, u64>> {
    static T: OnceLock<Mutex<HashMap<&'static str, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record one frozen-site touch by a gil-free worker.
///
/// The site labels are string literals at every call site, so leaking one into `&'static str` on
/// first sight keeps the map key cheap and the tally allocation-free after warm-up. The mutex is
/// taken only on a touch — and the whole point of the measurement is that touches should be RARE
/// relative to the 292M calls the region makes, so it cannot distort what it measures. If it ever
/// does become hot, that is itself the answer.
pub(crate) fn note(what: &str) {
    TOTAL.fetch_add(1, Ordering::Relaxed);
    let mut g = table().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = g.get_mut(what) {
        *c += 1;
        return;
    }
    let leaked: &'static str = Box::leak(what.to_owned().into_boxed_str());
    g.insert(leaked, 1);
}

/// Ranked report. Called from the same teardown path as the other censuses.
pub(crate) fn report() {
    if !enabled() {
        return;
    }
    let total = TOTAL.load(Ordering::Relaxed);
    if total == 0 {
        eprintln!(
            "[FREEZE-TALLY] ZERO frozen-site touches by any gil-free worker. \
             Every shared-VM mutation the lane would have made is one the lock never needed to \
             serialise: the guard can be narrowed to the resolve itself."
        );
        return;
    }
    let g = table().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&&'static str, &u64)> = g.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("[FREEZE-TALLY] {total} frozen-site touches by gil-free workers:");
    for (what, n) in rows {
        // Fillable = skipping the insert only costs a recomputation; the rest are real state.
        let kind = match *what {
            w if w.starts_with("Heap::write_static_cell") || w.starts_with("indy_singletons") => {
                "REAL-STATE (cannot simply skip)"
            }
            _ => "cache fill (skippable)",
        };
        eprintln!("[FREEZE-TALLY]   {n:>12}  {what}  — {kind}");
    }
}
