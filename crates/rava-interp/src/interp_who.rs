//! `RAVA_INTERP_WHO` — **which methods run interpreted, and what the JIT thinks of them.**
//!
//! The `exec` counters say 61.5% of guest invocations in `Bootstrap.bootStrap` build an
//! interpreter frame, and that 99.6% of those are in methods the compiler could handle. That is a
//! number without an address: it is equally consistent with a few hot methods that never get
//! compiled (a tiering bug, fixable) and with thousands of methods called a handful of times each
//! (nothing to fix — compiling them would cost more than interpreting them).
//!
//! Those two worlds want opposite work, so the distribution decides. This records one row per
//! method: how many interpreted entries it took, and the `jit_cache` verdict at dump time —
//! `compiled` (machine code EXISTS and the interpreter ran anyway, the worst case), `cold N`
//! (still climbing), `failed`, or `absent`.
//!
//! Diagnostic only, default off, and it takes a global lock per interpreted entry — see the
//! `exec` counters, which cost 2.2x on this phase. **A run with this armed is a count, never a
//! duration.**

use std::sync::Mutex;

pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_INTERP_WHO").is_some())
}

struct Row {
    entries: u64,
    label: String,
}

fn table() -> &'static Mutex<crate::FastMap<usize, Row>> {
    static T: std::sync::OnceLock<Mutex<crate::FastMap<usize, Row>>> = std::sync::OnceLock::new();
    T.get_or_init(|| Mutex::new(crate::FastMap::default()))
}

/// One interpreted entry. `key` is the `Code` pointer, which is what `jit_cache` is keyed by, so
/// the dump can ask the cache about exactly this method.
pub(crate) fn note(key: usize, owner: &str, name: &str, descriptor: &str) {
    let mut g = table().lock().unwrap_or_else(|e| e.into_inner());
    match g.get_mut(&key) {
        Some(r) => r.entries += 1,
        None => {
            g.insert(key, Row { entries: 1, label: format!("{owner}.{name}{descriptor}") });
        }
    }
}

/// Print the heaviest interpreted methods with the compiler's verdict on each.
pub fn dump(vm: &crate::Vm) {
    if !enabled() {
        return;
    }
    let g = table().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&usize, &Row)> = g.iter().collect();
    rows.sort_by(|a, b| b.1.entries.cmp(&a.1.entries));
    let total: u64 = rows.iter().map(|(_, r)| r.entries).sum();
    eprintln!(
        "[INTERP-WHO] {} distinct methods, {} interpreted entries",
        rows.len(),
        total
    );
    // How much of the mass sits in the head? That is the whole question.
    for cut in [10usize, 50, 200, 1000] {
        let head: u64 = rows.iter().take(cut).map(|(_, r)| r.entries).sum();
        if rows.len() > cut {
            eprintln!(
                "[INTERP-WHO]   top {cut:>4}: {head:>10} entries ({:.1}%)",
                100.0 * head as f64 / total.max(1) as f64
            );
        }
    }
    for (key, r) in rows.iter().take(25) {
        let verdict = match vm.jit_cache.get(*key) {
            Some(crate::jit::JitStatus::Compiled(_)) => "COMPILED".to_string(),
            Some(crate::jit::JitStatus::Compiling) => "compiling".to_string(),
            Some(crate::jit::JitStatus::Failed) => "failed".to_string(),
            Some(crate::jit::JitStatus::Cold(n)) => format!("cold {n}"),
            None => "absent".to_string(),
        };
        eprintln!(
            "[INTERP-WHO] {:>10}  {:<10} {}",
            r.entries, verdict, r.label
        );
    }
}
