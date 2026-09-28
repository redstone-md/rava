//! bootdisp.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PROFILE_BOOT_DISPATCH").is_some())
}
// key "class.name descriptor" -> [interpreted, jit]
static TALLY: Mutex<Option<HashMap<String, [u64; 2]>>> = Mutex::new(None);
static MISSES: AtomicU64 = AtomicU64::new(0);

/// Record one method entry. `jit` = ran via a compiled tier; else interpreted.
/// Caller builds `key` only when `on()` (so the format! is off the hot path when gated off).
pub fn record(key: &str, jit: bool) {
    let mut g = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(e) = m.get_mut(key) {
        e[jit as usize] += 1;
    } else {
        let mut a = [0u64, 0];
        a[jit as usize] = 1;
        m.insert(key.to_string(), a);
    }
}

/// One compiled-code dispatch-miss (rava_dispatch_miss) — context only.
#[inline]
pub fn miss() {
    MISSES.fetch_add(1, Relaxed);
}

/// Crossings whose callee is a `java/*` class — the population the never-cacheable rule refuses,
/// which no amount of compiling guest bytecode can reach because those classes HAVE no bytecode.
pub static MISS_JAVA: AtomicU64 = AtomicU64::new(0);
/// Crossings whose callee is ordinary guest code — the population dependency-order compilation
/// could actually convert into a direct call.
pub static MISS_GUEST: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn miss_owner(owner: &str) {
    if owner.starts_with("java/")
        || owner.starts_with("javax/")
        || owner.starts_with("jdk/")
        || owner.starts_with("sun/")
    {
        MISS_JAVA.fetch_add(1, Relaxed);
    } else {
        MISS_GUEST.fetch_add(1, Relaxed);
    }
}

/// The headline totals only — safe to call from the periodic profile dump.
pub fn dump_totals() {
    let g = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.as_ref().cloned().unwrap_or_default();
    let (mut tot_i, mut tot_j) = (0u64, 0u64);
    for v in map.values() {
        tot_i += v[0];
        tot_j += v[1];
    }
    let misses = MISSES.load(Relaxed);
    let mj = MISS_JAVA.load(Relaxed);
    let mg = MISS_GUEST.load(Relaxed);
    eprintln!(
        "[DISPATCH-TOTALS] interpreted-entries={tot_i} jit-entries={tot_j} host-crossings={misses}          crossings-per-jit-entry={:.2} | callee java/*={mj} guest={mg} ({:.1}% java)",
        if tot_j == 0 { 0.0 } else { misses as f64 / tot_j as f64 },
        if mj + mg == 0 { 0.0 } else { mj as f64 * 100.0 / (mj + mg) as f64 }
    );
}

/// The un-tiered hotspots only — safe to call from the periodic dump.
pub fn dump_stuck(top: usize) {
    let g = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.as_ref().cloned().unwrap_or_default();
    let mut stuck: Vec<(&String, &[u64; 2])> =
        map.iter().filter(|(_, v)| v[0] > 10_000 && v[1] == 0).collect();
    stuck.sort_by(|a, b| b.1[0].cmp(&a.1[0]));
    eprintln!("[STUCK] hot in the interpreter, never compiled — top {top}:");
    for (k, v) in stuck.iter().take(top) {
        eprintln!("[STUCK]   {:>12} interp  {k}", v[0]);
    }
    let total: u64 = stuck.iter().map(|(_, v)| v[0]).sum();
    eprintln!("[STUCK]   {} methods, {} interpreted entries total", stuck.len(), total);
}

pub fn dump() {
    let g = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.as_ref().cloned().unwrap_or_default();
    let (mut tot_i, mut tot_j) = (0u64, 0u64);
    for v in map.values() {
        tot_i += v[0];
        tot_j += v[1];
    }
    eprintln!(
        "[BOOT-DISPATCH] {} distinct methods | interpreted entries={} jit entries={} | compiled dispatch-misses={}",
        map.len(), tot_i, tot_j, MISSES.load(Relaxed)
    );
    // The target: hot in the interpreter, never compiled.
    let mut stuck: Vec<(&String, &[u64; 2])> = map
        .iter()
        .filter(|(_, v)| v[0] > 10_000 && v[1] == 0)
        .collect();
    stuck.sort_by(|a, b| b.1[0].cmp(&a.1[0]));
    eprintln!(
        "[BOOT-DISPATCH] TOP-20 STUCK methods (INTERPRETED > 10000, JIT == 0) — the un-tiered hotspots:"
    );
    eprintln!("[BOOT-DISPATCH]   {:>12}  {:>10}  {}", "INTERP", "JIT", "class.method descriptor");
    for (k, v) in stuck.iter().take(20) {
        eprintln!("[BOOT-DISPATCH]   {:>12}  {:>10}  {}", v[0], v[1], k);
    }
    if stuck.is_empty() {
        eprintln!("[BOOT-DISPATCH]   (none — every >10k-entry method reached a compiled tier)");
    }
}
