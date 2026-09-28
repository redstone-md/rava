//! Warming a lane before it runs, and the nursery slice it gets.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

/// Per-task nursery slice (bump view over main's young).
///
/// It is NOT a perf knob — that reading is what produced the wall. A ring task
/// allocates ~480 KB, so a 256 KiB slice is exhausted MID-BODY, and the compiled
/// inline-alloc retry loop the overflow lands in polls nothing: without a spill pool
/// to refill from it spun 1.44 billion callbacks on one thread. The slice is now the
/// task's STARTING view; the region's shared overflow pool (`region_spill`) absorbs
/// the variance, so the slice can stay small and 127 of them still fit main's young.
/// `RAVA_GILFREE_SLICE_KB` overrides.
pub(crate) fn nursery_slice() -> usize {
    pub(crate) static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_GILFREE_SLICE_KB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|kb| *kb > 0)
            .map(|kb| kb * 1024)
            .unwrap_or(192 * 1024)
    })
}

/// Record one general-path callee reached by the pilot.
pub(crate) fn warm_note(class: &str, name: &str, desc: &str) {
    // ⛔**THIS ALLOCATED THREE `String`s TO ASK A QUESTION, AND THREE MORE TO ANSWER IT.** The
    // membership test built `(class.to_string(), name.to_string(), desc.to_string())` — three
    // heap allocations, on every call, under a global mutex — purely so `contains` had something
    // to compare, and then built them AGAIN for the insert. A PC profile of the product boot put
    // this cluster (`RandomState` hashing + `HashMap::insert` + the allocator) at ~3.7%.
    //
    // The set is a WARM-UP HINT LIST: it says which callees to pre-compile, and it is drained
    // once. So the membership test is now a hash of the three borrowed strs — no allocation, no
    // `String` — and only a genuinely new target pays for the strings it stores.
    use std::hash::{Hash, Hasher};
    let mut h = crate::FxHasher::default();
    class.hash(&mut h);
    name.hash(&mut h);
    desc.hash(&mut h);
    let key = h.finish();
    let seen = WARM_SEEN.get_or_init(|| std::sync::Mutex::new(crate::FastSet::default()));
    {
        let mut k = seen.lock().unwrap_or_else(|e| e.into_inner());
        if k.len() >= 4096 || !k.insert(key) {
            return;
        }
    }
    let set = WARM_TARGETS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let mut s = set.lock().unwrap_or_else(|e| e.into_inner());
    s.insert((class.to_string(), name.to_string(), desc.to_string()));
}

/// Hashes of the targets already recorded — the allocation-free half of `warm_note`.
static WARM_SEEN: std::sync::OnceLock<std::sync::Mutex<crate::FastSet<u64>>> =
    std::sync::OnceLock::new();
