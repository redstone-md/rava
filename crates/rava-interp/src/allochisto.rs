//! Step-2 measurement: a global heap-allocation histogram, armed/dumped via System.getProperty
//! sentinels so a bench can bracket exactly one phase (e.g. Codec.parse decode) and read the
//! composition — how many gson tree nodes (JsonObject/JsonArray/JsonPrimitive/LinkedTreeMap$*) vs
//! DFU monad objects (DataResult$*/Pair/Optional) vs final domain objects were allocated. Generic:
//! keyed purely on the class string; gated RAVA_ALLOC_HISTO (zero cost when off).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ALLOC_HISTO").is_some())
}
static ARMED: AtomicBool = AtomicBool::new(false);
static TYPES: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[inline]
pub fn record(ty: &str) {
    if !ARMED.load(Relaxed) {
        return;
    }
    TOTAL.fetch_add(1, Relaxed);
    let mut g = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(ty.to_string()).or_insert(0) += 1;
}

pub fn reset() {
    ARMED.store(true, Relaxed);
    TOTAL.store(0, Relaxed);
    *TYPES.lock().unwrap_or_else(|e| e.into_inner()) = Some(HashMap::new());
}

/// `RAVA_ALLOC_HISTO_FIND=<substr>[,<substr>…]` — print the count for every recorded type
/// matching a substring, REGARDLESS of rank. `dump` ranks by count and shows the head, which
/// is useless for a question like "is PushReaction ever heap-allocated": an enum has a handful
/// of constants, so it sits at the very bottom of a list headed by millions of gson nodes.
/// The blocking-cell census attributes 30,837 cells to that class while it appears at NEITHER
/// `new` birth site, so the only remaining question is whether it reaches `Heap::alloc` at all
/// — and `Heap::alloc` ALWAYS mints a handle, so it is the choke point every remaining
/// candidate path (ctor-reference receivers, Enum.valueOf, the enum natives) must pass.
pub fn find() {
    let Ok(pats) = std::env::var("RAVA_ALLOC_HISTO_FIND") else { return };
    let g = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    let types = g.as_ref().cloned().unwrap_or_default();
    for pat in pats.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let mut any = false;
        for (ty, n) in types.iter() {
            if ty.contains(pat) {
                eprintln!("[ALLOC-FIND] {pat}: {n:>9}  {ty}");
                any = true;
            }
        }
        if !any {
            eprintln!("[ALLOC-FIND] {pat}: NEVER heap-allocated (0 records)");
        }
    }
}

pub fn dump() {
    ARMED.store(false, Relaxed);
    let total = TOTAL.load(Relaxed);
    let g = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    let types = g.as_ref().cloned().unwrap_or_default();
    // Bucket into the three storms so the A-vs-C decision is explicit.
    let (mut gson, mut monad, mut other) = (0u64, 0u64, 0u64);
    for (ty, n) in &types {
        if ty.starts_with("com/google/gson/") || ty.contains("LinkedTreeMap") {
            gson += n;
        } else if ty.starts_with("com/mojang/serialization/") || ty.contains("datafixers/util/Pair") || ty == "java/util/Optional" {
            monad += n;
        } else {
            other += n;
        }
    }
    eprintln!("[ALLOC-HISTO] total={total} objects across {} distinct types", types.len());
    let pc = |n: u64| if total > 0 { n * 100 / total } else { 0 };
    eprintln!("[ALLOC-HISTO] BUCKETS: gson-tree={gson} ({}%)  dfu-monad={monad} ({}%)  other/domain={other} ({}%)",
        pc(gson), pc(monad), pc(other));
    let mut v: Vec<(&String, &u64)> = types.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("[ALLOC-HISTO] top allocated types (count | %):");
    for (ty, n) in v.iter().take(30) {
        eprintln!("[ALLOC-HISTO]   {:>10} ({:>2}%)  {}", n, pc(**n), ty);
    }
}
