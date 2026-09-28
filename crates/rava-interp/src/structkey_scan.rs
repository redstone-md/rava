//! M5.1 STRUCTURAL-KEY SCAN (RAVA_STRUCTKEY_SCAN, inert). Two histograms: (1) object allocations by
//! class_id (lock-free atomic array, hooked at the arena bump), (2) how many times each class is used
//! as a Map KEY (hooked at value_map_key/map_key — the value→key choke). A class that is both
//! alloc-heavy AND map-key-heavy is the structural-key churn (a fresh composite key built per lookup,
//! e.g. populateNeighbours' property-map key). The field-kind summary sizes a future 64-bit packer:
//! an all-int/long key is packable into a u64, erasing every one of its heap allocations.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_STRUCTKEY_SCAN").is_some())
}

const N: usize = 1 << 16; // class-id slots
static ALLOC: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static ALLOC_TOTAL: AtomicU64 = AtomicU64::new(0);
static MAPKEY_TOTAL: AtomicU64 = AtomicU64::new(0);
static MAPKEY: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None); // times class used as a Map key

/// Per successful arena allocation (lock-free, Rust-path only — the JIT inline bump is separate).
pub fn note_alloc(class_id: u32) {
    if (class_id as usize) < N {
        ALLOC[class_id as usize].fetch_add(1, Relaxed);
    }
    ALLOC_TOTAL.fetch_add(1, Relaxed);
}

/// A guest object value used as a Map key (put/get). Keyed by class name (unifies Ref + Obj). This
/// is the COMPLETE churn metric (every key routes through here) and drives the periodic dump, since
/// the boot is hard-killed after Done (no clean exit dump) and map-key ops are the number of interest.
pub fn note_mapkey(name: &str) {
    {
        let mut g = MAPKEY.lock().unwrap_or_else(|e| e.into_inner());
        *g.get_or_insert_with(HashMap::new).entry(name.to_string()).or_insert(0) += 1;
    }
    let t = MAPKEY_TOTAL.fetch_add(1, Relaxed) + 1;
    if t == 100_000 || t == 1_000_000 || t % 5_000_000 == 0 {
        dump();
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let mk = MAPKEY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let mut rows: Vec<(u32, u64)> = (0..N as u32)
        .filter_map(|cid| {
            let a = ALLOC[cid as usize].load(Relaxed);
            if a > 0 { Some((cid, a)) } else { None }
        })
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!(
        "[STRUCTKEY] Rust-path allocs = {} | total map-key ops = {} | {} distinct alloc classes. Top by alloc:",
        ALLOC_TOTAL.load(Relaxed),
        MAPKEY_TOTAL.load(Relaxed),
        rows.len()
    );
    eprintln!("[STRUCTKEY]  {:>13} {:>11}  fields(i=int J=long fp=float/dbl r=ref)   class", "allocs", "map-key");
    for (cid, a) in rows.iter().take(25) {
        let (name, layout) = match crate::nextgen::class_layout(*cid) {
            Some(x) => x,
            None => continue,
        };
        let mkc = mk.get(&*name).copied().unwrap_or(0);
        let (mut i, mut j, mut fp, mut r) = (0u32, 0u32, 0u32, 0u32);
        for &(_, k) in layout.iter() {
            match k { 0 => i += 1, 1 => j += 1, 2 | 3 => fp += 1, 6 => r += 1, _ => {} }
        }
        eprintln!(
            "[STRUCTKEY] {:>13} {:>11}  {}i {}J {}fp {}r ({} tot)   {}",
            a, mkc, i, j, fp, r, layout.len(), name
        );
    }
    // VERDICT: the structural-key churn — classes that are BOTH map keys AND allocated a lot.
    // packable64 ⇒ all fields int/long ⇒ a native u64/u128 key erases every one of its allocations.
    // name → class_id from the alloc rows (so churn entries can show alloc count + field layout).
    let name_to_cid: HashMap<String, u32> = rows
        .iter()
        .filter_map(|(cid, _)| crate::nextgen::class_name(*cid).map(|n| (n.to_string(), *cid)))
        .collect();
    let mut churn: Vec<(Option<u32>, String, u64)> = mk
        .iter()
        .map(|(name, &mkc)| (name_to_cid.get(name).copied(), name.clone(), mkc))
        .collect();
    churn.sort_by(|a, b| b.2.cmp(&a.2)); // by map-key frequency (the churn magnitude)
    eprintln!("[STRUCTKEY] === STRUCTURAL-KEY CHURN (used as Map key), sorted by key-op count ===");
    let mut erasable = 0u64;
    for (cid, name, mkc) in churn.iter().take(15) {
        let (allocs, layout) = match cid.and_then(crate::nextgen::class_layout) {
            Some((_, l)) => (cid.map(|c| ALLOC[c as usize].load(Relaxed)).unwrap_or(0), l),
            None => (0, std::sync::Arc::from(Vec::new())),
        };
        let packable = !layout.is_empty() && layout.iter().all(|&(_, k)| matches!(k, 0 | 1));
        if packable {
            erasable += mkc;
        }
        eprintln!(
            "[STRUCTKEY]   map-key={:>10} allocs={:>10} packable64={} nfields={}  {}",
            mkc, allocs, packable, layout.len(), name
        );
    }
    eprintln!(
        "[STRUCTKEY] ⇒ ~{} key-ops on PACKABLE (all int/long) classes — allocations a native u64 packer could erase.",
        erasable
    );
}
