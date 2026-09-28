//! Abort sites, their counters, and the reports over them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

pub(crate) fn lane_pending_report() -> (u32, u32, String, &'static str) {
    let (s, t) = crate::gilfree_cf::stats::LANE_PENDING.with(std::cell::Cell::get);
    let class = LANE_PENDING_CLASS
        .with(|c| c.borrow().clone())
        .unwrap_or_else(|| "-".to_string());
    let cb = match crate::gilfree_cf::stats::LANE_ABORT_CB.with(std::cell::Cell::get) {
        Some(i) if i < jit::CB_NAMES.len() => jit::CB_NAMES[i],
        Some(_) => "outside-any-callback",
        None => "-",
    };
    (s, t, class, cb)
}

/// Latch a region abort against THIS lane task. Called from
/// `parallel::request_abort`, so every existing abort raiser (the S09
/// worker-illegal stubs, nursery overflow, the uncompiled-callee guard) feeds it
/// with no new call sites; inert on every non-lane thread.
#[inline]
pub(crate) fn note_lane_abort() {
    if in_lane() {
        crate::gilfree_cf::stats::LANE_ABORT.with(|c| c.set(true));
        // Remember WHICH compiled callback was running. A release-build backtrace
        // through `extern "C"` callbacks misattributes the caller often enough to
        // send an investigation at the wrong function entirely; the callback the
        // thread actually entered is recorded, not inferred.
        crate::gilfree_cf::stats::LANE_ABORT_CB.with(|c| {
            if c.get().is_none() {
                c.set(Some(crate::gilfree_cf::stats::LANE.with(|l| l.get().last_cb)));
            }
        });
    }
}

/// Array allocations refused because the lane's OPEN parallel array region ran out
/// of pre-reserved slots. Distinct from `ABORT_ARRAY_ALLOC` (no region open at all):
/// one says "size the region bigger", the other "the lane never opened one".
pub(crate) static ABORT_ARRAY_REGION_FULL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Per-callback deopt tally for the S09 worker-illegal stubs (indy / putstatic /
/// ldc / map_put / ...), recorded only for THIS lane so the pre-existing forks
/// are unaffected. This is what names the wall when the lane admits but aborts.
pub(crate) static STUB_ABORTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
> = std::sync::OnceLock::new();

pub(crate) fn drain_abort_sites() {
    let Some(map) = ABORT_SITES.get() else { return };
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    if m.is_empty() {
        return;
    }
    let mut rows: Vec<_> = m.drain().map(|((f, l), n)| (n, f, l)).collect();
    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    for (n, f, l) in rows {
        eprintln!("[GILFREE-RINGS] abort-site {f}:{l} x{n}");
    }
}
