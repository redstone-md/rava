//! M3 9.2a SHAPE-CACHE CROSS-STATE ISOLATION ANALYZER (RAVA_M3_SHAPE_PROBE). Proves whether the
//! `initCache` warmup is read-only across BlockState objects — the precondition for forking it.
//! While an `initCache` runs for state X, every field WRITE is classified: to X itself (safe), to a
//! DIFFERENT state Y (a CROSS-STATE WRITE — the fork hazard), or to a non-state object (its own new
//! Cache — safe). Ref-writes are caught in every tier (interp putfield, rava_putfield, rava_card_mark
//! inline barrier); compiled-inline PRIMITIVE writes are the only residual gap (noted — the neighbour
//! graph is reference-based, so ref-write coverage is what matters). Read-only, gated; no executor/GIL.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M3_SHAPE_PROBE").is_some())
}

thread_local! {
    static INIT_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}
static STATE_CID: AtomicU32 = AtomicU32::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static SELF_WRITES: AtomicU64 = AtomicU64::new(0); // write to the state whose initCache is running
static CROSS_WRITES: AtomicU64 = AtomicU64::new(0); // write to a DIFFERENT state — the fork hazard
static CACHE_WRITES: AtomicU64 = AtomicU64::new(0); // write to a non-state object (own Cache) — safe
static CROSS_READS: AtomicU64 = AtomicU64::new(0); // read of a different state (neighbour) — safe iff no cross-write

/// Enter an initCache for state `recv_addr` (class id `cid`). The state class is learned once.
pub fn enter_initcache(recv_addr: u64, cid: u32) {
    let _ = STATE_CID.compare_exchange(0, cid, Relaxed, Relaxed);
    CALLS.fetch_add(1, Relaxed);
    INIT_STACK.with(|s| s.borrow_mut().push(recv_addr));
}
pub fn exit_initcache() {
    INIT_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

#[inline]
fn current() -> Option<u64> {
    INIT_STACK.with(|s| s.borrow().last().copied())
}

/// Classify a field WRITE to `(target_addr, target_cid)` occurring during initCache.
pub fn note_write(target_addr: u64, target_cid: u32) {
    let Some(cur) = current() else { return };
    let scid = STATE_CID.load(Relaxed);
    if scid != 0 && target_cid == scid {
        if target_addr == cur {
            SELF_WRITES.fetch_add(1, Relaxed);
        } else {
            CROSS_WRITES.fetch_add(1, Relaxed);
        }
    } else {
        CACHE_WRITES.fetch_add(1, Relaxed);
    }
}

/// Classify a field READ of `(target_addr, target_cid)` occurring during initCache.
pub fn note_read(target_addr: u64, target_cid: u32) {
    let Some(cur) = current() else { return };
    let scid = STATE_CID.load(Relaxed);
    if scid != 0 && target_cid == scid && target_addr != cur {
        CROSS_READS.fetch_add(1, Relaxed);
    }
}

pub fn dump() {
    let (calls, sw, cw, cache, cr) = (
        CALLS.load(Relaxed),
        SELF_WRITES.load(Relaxed),
        CROSS_WRITES.load(Relaxed),
        CACHE_WRITES.load(Relaxed),
        CROSS_READS.load(Relaxed),
    );
    eprintln!("[M3-SHAPE] initCache calls={calls} state_cid={}", STATE_CID.load(Relaxed));
    eprintln!(
        "[M3-SHAPE]   writes during initCache: self-state={sw}  CROSS-STATE={cw}  non-state(own Cache/etc)={cache}"
    );
    eprintln!(
        "[M3-SHAPE]   cross-state READS during initCache={cr} (neighbour reads — safe iff CROSS-STATE writes == 0)"
    );
    if cw == 0 {
        eprintln!(
            "[M3-SHAPE] VERDICT: 0 cross-state writes → neighbour graph is READ-ONLY by construction → initCache is SAFE TO FORK (ref-write coverage; compiled-inline primitive writes not intercepted)"
        );
    } else {
        eprintln!(
            "[M3-SHAPE] VERDICT: {cw} CROSS-STATE WRITE(S) → NOT trivially forkable; a state is mutated during another's initCache"
        );
    }
}
