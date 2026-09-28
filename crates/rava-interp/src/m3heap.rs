//! M3 9.2c.2a HEAP-ALLOCATION CLASSIFIER (RAVA_M3_HEAP_CLASSIFY_PROBE). Anatomises the ~1.87M heap
//! allocations 9.2c.1 found during the post-warm initCache phase: WHICH object types (by Java class /
//! array element / map), through which alloc path, and how concentrated. Every `Heap::alloc*` is a
//! fresh `Vec<Box<Object>>` push (a new instance — zero alloc-layer dedup), so the per-type COUNT is
//! the composition that decides the fork's heap-isolation strategy: a handful of geometry types
//! (VoxelShape/DiscreteVoxelShape/BitSet/arrays) ⇒ arena-migrate them (Variant B); a long tail of
//! arbitrary types ⇒ wake slot-partitioning (Variant A). Also records which methods JIT-COMPILE and
//! which classes LOAD during the phase (the pre-warm scope — does one-state-per-block-class close the
//! 148 lazy compiles?). Read-only, gated. Only counts calls #2.. (call #1 = serial pre-warm).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// Shape-cache mode (bracket initCache).
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M3_HEAP_CLASSIFY_PROBE").is_some())
}
/// 9.2c.2b worldgen-biome mode (bracket getNoiseBiome/findBiome) — the Variant C comparison.
pub fn worldgen_probe() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M3_WORLDGEN_ERR_PROBE").is_some())
}
/// Either phase probe is on — the heap/compile hooks guard on this (zero cost when both off).
#[inline]
pub fn probe_on() -> bool {
    enabled() || worldgen_probe()
}
/// The method name whose invoke brackets the measured phase, or None (no probe).
pub fn phase_label() -> &'static str {
    if worldgen_probe() { "worldgen-biome (getNoiseBiome/findBiome)" } else { "shape-cache (initCache)" }
}

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static MEASURE: Cell<bool> = const { Cell::new(false) };
}
static CALLS: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static TYPES: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static COMPILES: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// Enter an initCache call: the OUTERMOST frame decides whether this call is measured (#>=2).
pub fn enter() {
    let idx = CALLS.fetch_add(1, Relaxed) + 1;
    DEPTH.with(|d| {
        if d.get() == 0 {
            MEASURE.with(|m| m.set(idx >= 2));
        }
        d.set(d.get() + 1);
    });
}
pub fn exit() {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}
#[inline]
fn active() -> bool {
    DEPTH.with(|d| d.get() > 0) && MEASURE.with(|m| m.get())
}

/// Record a heap allocation of `ty` (Java class / `[element` / `map`) during a measured initCache.
pub fn record_alloc(ty: &str) {
    if !active() {
        return;
    }
    TOTAL.fetch_add(1, Relaxed);
    let mut g = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(ty.to_string()).or_insert(0) += 1;
}

/// Record a method JIT-compiling during a measured initCache (the pre-warm scope).
pub fn record_compile(class_method: &str) {
    if !active() {
        return;
    }
    let mut g = COMPILES.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(class_method.to_string()).or_insert(0) += 1;
}

pub fn dump() {
    let (calls, total) = (CALLS.load(Relaxed), TOTAL.load(Relaxed));
    let g = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    let types = g.as_ref().cloned().unwrap_or_default();
    let distinct = types.len();
    eprintln!(
        "[M3-HEAP] phase={} | post-warm heap allocs: {total} across {distinct} distinct type(s) (each alloc = a fresh Vec<Box<Object>> instance → 0 dedup at the alloc layer)",
        phase_label()
    );
    let mut v: Vec<(&String, &u64)> = types.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("[M3-HEAP] top allocated types (count | %):");
    for (ty, n) in v.iter().take(20) {
        let pct = if total > 0 { *n * 100 / total } else { 0 };
        eprintln!("[M3-HEAP]   {:>10} ({:>2}%)  {}", n, pct, ty);
    }
    // Pre-warm scope: distinct methods compiled during the phase (class-load count: see 9.2c.1's
    // classes delta +1134 — not re-measured here).
    let cg = COMPILES.lock().unwrap_or_else(|e| e.into_inner());
    let compiles = cg.as_ref().cloned().unwrap_or_default();
    eprintln!(
        "[M3-HEAP] pre-warm scope: {} distinct method(s) JIT-compiled DURING the post-warm phase (a one-state-per-block-class pre-warm must force these serially first — workers can't compile)",
        compiles.len()
    );
    let mut cv: Vec<(&String, &u64)> = compiles.iter().collect();
    cv.sort_by(|a, b| b.1.cmp(a.1));
    for (m, n) in cv.iter().take(12) {
        eprintln!("[M3-HEAP]   compiled x{:<5} {}", n, m);
    }
}
