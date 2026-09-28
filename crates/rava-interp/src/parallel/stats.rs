//! Counters over COW, aborts and the shadow, and the clears that reset them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::parallel::*;

/// (abort_raised, overflow_raised, regions_torn_down, regions_torn_down_by_overflow)
pub(crate) fn abort_stats() -> (usize, usize, usize, usize) {
    (
        ABORT_RAISED.load(Ordering::Relaxed),
        OVERFLOW_RAISED.load(Ordering::Relaxed),
        REGION_TEARDOWN.load(Ordering::Relaxed),
        REGION_TEARDOWN_OVERFLOW.load(Ordering::Relaxed),
    )
}

/// Reset the abort flags — the coordinator calls this when opening a new region.
#[allow(dead_code)]
pub(crate) fn clear_abort() {
    // `swap` (not `store`) so the TEARDOWN tally is exact: this is called both to OPEN a
    // region (flags already false → not a teardown) and to tear one down (flags set →
    // counted once, regardless of how many workers raised).
    if REGION_ABORT.swap(false, Ordering::AcqRel) {
        REGION_TEARDOWN.fetch_add(1, Ordering::Relaxed);
    }
    if REGION_OVERFLOW.swap(false, Ordering::AcqRel) {
        REGION_TEARDOWN_OVERFLOW.fetch_add(1, Ordering::Relaxed);
    }
}

thread_local! {
    /// True while THIS thread is running offloaded bytecode GIL-FREE (a parallel
    /// worldgen worker). Distinct from `gil::is_worker` (the GIL-HOLDING daemon
    /// threads). A gil-free thread's op-loop checkpoint polls the safepoint and
    /// does NOT drive GC/network-pump/GIL-handoff (those are the GIL holder's job)
    /// and must NOT touch shared `Vm` state outside its arena + immutable reads.
    pub(crate) static GILFREE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(feature = "next-gen-gc")]
pub(crate) fn field_box_clear() {
    FIELD_BOX.with(|f| f.set((0, 0)));
}

// ── M3 COW mutation isolation (RAVA_FORK_COW) ──────────────────────────────────
// A gil-free worker's writes go to a THREAD-LOCAL shadow instead of the shared
// object, so an impure fork body (e.g. worldgen memoisation caches) can run in
// parallel without racing shared state. Reads fall through shadow→shared (shared is
// read-only during phase B). Sound iff the isolated writes were semantically inert
// (pure memo) — HASH-arbitrated (see the M3 COW concept). Default-off = byte-identical.
thread_local! {
    /// (object-identity, field-id) → shadowed value. Object identity: a heap handle
    /// (small int) or an arena address (huge) — disjoint domains, no collision.
    pub(crate) static SHADOW_FIELDS: std::cell::RefCell<crate::cow_shadow::ShadowFields> =
        std::cell::RefCell::new(crate::cow_shadow::ShadowFields::new());
}
pub(crate) fn owned_clear() {
    OWNED_RANGES.with(|r| r.borrow_mut().clear());
}
#[inline]
pub(crate) fn count_hot_cow(global: &std::sync::atomic::AtomicU64, slot: usize) -> u64 {
    // Trace event ordinals remain process-wide. The normal acceptance path
    // keeps every increment worker-local until its scoped region exit.
    let in_region = REGION_DEPTH.with(|depth| depth.get() != 0);
    if !in_region || fork_trace_enabled() {
        return global.fetch_add(1, Ordering::Relaxed);
    }
    let counter = match slot {
        COW_LOCAL_SHADOW_PUT => &COW_HOT_SHADOW_PUT,
        COW_LOCAL_SHARED_PUT => &COW_HOT_SHARED_PUT,
        COW_LOCAL_GET_HIT => &COW_HOT_GET_HIT,
        _ => unreachable!("unknown local COW counter"),
    };
    counter.with(|value| {
        let previous = value.get();
        value.set(previous.wrapping_add(1));
        previous
    })
}

pub(crate) fn flush_hot_cow_stats() {
    let values = [
        COW_HOT_SHADOW_PUT.with(|value| value.replace(0)),
        COW_HOT_SHARED_PUT.with(|value| value.replace(0)),
        COW_HOT_GET_HIT.with(|value| value.replace(0)),
    ];
    for (value, global) in [
        (values[COW_LOCAL_SHADOW_PUT], &CNT_SHADOW_PUT),
        (values[COW_LOCAL_SHARED_PUT], &CNT_SHARED_PUT),
        (values[COW_LOCAL_GET_HIT], &CNT_SHADOW_GET_HIT),
    ] {
        if value != 0 {
            global.fetch_add(value, Ordering::Relaxed);
        }
    }
    let arena_allocs = HOT_ARENA_ALLOCS.with(|value| value.replace(0));
    if arena_allocs != 0 {
        crate::nextgen::ARENA_ALLOCS.fetch_add(arena_allocs, Ordering::Relaxed);
    }
}

/// Drop this worker's shadow — called per fork element so writes never leak across
/// elements (each element is an independent pure computation). Under OWNED-UNDO the
/// direct owned-writes are rolled back here too (same discard point).
pub(crate) fn shadow_clear() {
    #[cfg(feature = "next-gen-gc")]
    if cow_owned_undo() {
        undo_rollback();
    }
    SHADOW_FIELDS.with(|s| {
        let mut shadow = s.borrow_mut();
        if cow_profile_enabled() {
            let len = shadow.len() as u64;
            CNT_SHADOW_CLEAR.fetch_add(1, Ordering::Relaxed);
            CNT_SHADOW_CLEAR_ENTRIES.fetch_add(len, Ordering::Relaxed);
            CNT_SHADOW_MAX_ENTRIES.fetch_max(len, Ordering::Relaxed);
        }
        shadow.clear();
    });
}
