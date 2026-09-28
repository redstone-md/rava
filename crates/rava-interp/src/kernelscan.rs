//! 19.1 M4 KERNEL-SCAN (RAVA_M4_KERNEL_SCAN). Locate the heaviest PURE math kernel in the vanilla noise
//! generator (PerlinNoise/ImprovedNoise/SimplexNoise/CubicSpline family) — the next 15.1-style native
//! port target. For every invoke of a target-class method it bumps a call count and, on first sight,
//! runs the JIT-pass `kernel_profile` bytecode analysis. The candidate = called the most, with allocs==0
//! + non-Math calls==0 + double/float array traffic + a loop (a callback-free primitive-array compute
//! loop a native kernel can absorb without VM re-entry). Read-only ⇒ inert; hashes converge bit-for-bit.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M4_KERNEL_SCAN").is_some())
}
/// The vanilla noise-synth package + the CubicSpline math utility (and its inner classes).
pub fn is_target(class: &str) -> bool {
    class.starts_with("net/minecraft/world/level/levelgen/synth/")
        || class.starts_with("net/minecraft/util/CubicSpline")
}
struct Entry {
    calls: u64,
    prof: crate::jit::KernelProfile,
}
static TABLE: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

/// One invoke of a target-class method: bump its call count; on first sight run `kernel_profile`
/// (lazily via the closure, so the analysis runs once, not per call). Read-only.
pub fn observe<F: FnOnce() -> crate::jit::KernelProfile>(
    class: &str,
    name: &str,
    desc: &str,
    profile: F,
) {
    let key = format!("{class}::{name}{desc}");
    let mut g = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    match m.get_mut(&key) {
        Some(e) => e.calls += 1,
        None => {
            m.insert(key, Entry { calls: 1, prof: profile() });
        }
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let g = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let map = match g.as_ref() {
        Some(m) if !m.is_empty() => m,
        _ => {
            eprintln!("[KERNEL-SCAN] no target noise/spline methods observed (phase not reached?)");
            return;
        }
    };
    let mut rows: Vec<(&String, &Entry)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls));
    eprintln!("[KERNEL-SCAN] target noise/spline methods by call count (allocs/nonMath calls must be 0 for a native kernel):");
    eprintln!(
        "[KERNEL-SCAN]   {:>12}  {:>5} {:>7} {:>4} {:>5} {:>5} {:>5} {:>4}  method",
        "CALLS", "alloc", "nonMath", "Math", "dArr", "iArr", "fpAri", "loop"
    );
    for (k, e) in rows.iter().take(24) {
        let p = &e.prof;
        eprintln!(
            "[KERNEL-SCAN]   {:>12}  {:>5} {:>7} {:>4} {:>5} {:>5} {:>5} {:>4}  {}",
            e.calls, p.allocs, p.nonmath_calls, p.math_calls, p.dfloat_array_ops,
            p.int_array_ops, p.fp_arith, p.has_loop as u8, k
        );
    }
    // Candidate = most-called, callback-free (0 alloc, 0 non-Math call), primitive-array loop.
    let cand = rows.iter().find(|(_, e)| {
        let p = &e.prof;
        p.allocs == 0 && p.nonmath_calls == 0 && p.dfloat_array_ops > 0 && p.has_loop
    });
    match cand {
        Some((k, e)) => {
            let owner_method = k.rsplit_once('(').map(|(m, _)| m).unwrap_or(k);
            eprintln!(
                "[RAVA-M4-KERNEL-TARGET] Found candidate: {}  (calls={}, allocs=0, nonMath=0, dArr={}, loop=1)",
                owner_method, e.calls, e.prof.dfloat_array_ops
            );
        }
        None => {
            // Report the closest: most-called with 0 alloc + 0 non-Math call, even without a
            // double/float array loop, to show the near-miss frontier.
            let near = rows.iter().find(|(_, e)| e.prof.allocs == 0 && e.prof.nonmath_calls == 0);
            match near {
                Some((k, e)) => eprintln!(
                    "[RAVA-M4-KERNEL-TARGET] none strictly qualified; closest callback-free = {} (calls={}, dArr={}, loop={})",
                    k, e.calls, e.prof.dfloat_array_ops, e.prof.has_loop as u8
                ),
                None => eprintln!("[RAVA-M4-KERNEL-TARGET] none: every target method allocates or makes non-Math calls (compute-wall)"),
            }
        }
    }
}
