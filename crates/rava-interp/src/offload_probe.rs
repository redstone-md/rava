//! `RAVA_OFFLOAD_CACHE_PROBE` — cache-churn census for transitive offload proofs.
//!
//! `Vm::is_offloadable_rec` is an admission proof, never an execution route. Its
//! memo is invalidated when the loader epoch moves because a newly loaded
//! override can change a virtual dispatch closure. This diagnostic establishes
//! whether those conservative invalidations are material before any attempt to
//! split epoch-stable facts from epoch-dependent closure proofs.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

static EPOCH_CLEARS: AtomicU64 = AtomicU64::new(0);
static EVICTED: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
static ANALYZED: AtomicU64 = AtomicU64::new(0);
static STORED: AtomicU64 = AtomicU64::new(0);
static UNSTABLE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_OFFLOAD_CACHE_PROBE").is_some())
}

pub(crate) fn epoch_clear(evicted: usize) {
    if !enabled() {
        return;
    }
    EPOCH_CLEARS.fetch_add(1, Relaxed);
    EVICTED.fetch_add(evicted as u64, Relaxed);
}

pub(crate) fn hit() {
    if enabled() {
        HITS.fetch_add(1, Relaxed);
    }
}

pub(crate) fn analyzed() {
    if enabled() {
        ANALYZED.fetch_add(1, Relaxed);
    }
}

pub(crate) fn stored() {
    if enabled() {
        STORED.fetch_add(1, Relaxed);
    }
}

pub(crate) fn unstable() {
    if enabled() {
        UNSTABLE.fetch_add(1, Relaxed);
    }
}

/// Called only from the periodic diagnostic sampler. The counters remain inert
/// in the production build through `offload_probe_stub`.
pub(crate) fn dump() {
    if !enabled() {
        return;
    }
    let hits = HITS.load(Relaxed);
    let analyzed = ANALYZED.load(Relaxed);
    let total = hits + analyzed;
    let hit_pct = if total == 0 {
        0.0
    } else {
        hits as f64 * 100.0 / total as f64
    };
    eprintln!(
        "[OFFLOAD-CACHE] hits={hits} analyzed={analyzed} ({hit_pct:.1}% hit) stored={} unstable={} epoch_clears={} evicted={}",
        STORED.load(Relaxed),
        UNSTABLE.load(Relaxed),
        EPOCH_CLEARS.load(Relaxed),
        EVICTED.load(Relaxed),
    );
}

/// Reader accessors so the offload memo's health can be read from `RAVA_DIAG` instead of only
/// from the periodic sampler — the counters are what decide whether a verdict is derived once or
/// on every fork gate.
pub(crate) fn hits_n() -> u64 {
    HITS.load(Relaxed) as u64
}
pub(crate) fn analyzed_n() -> u64 {
    ANALYZED.load(Relaxed) as u64
}
pub(crate) fn stored_n() -> u64 {
    STORED.load(Relaxed) as u64
}
pub(crate) fn unstable_n() -> u64 {
    UNSTABLE.load(Relaxed) as u64
}
