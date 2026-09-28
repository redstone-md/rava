//! A census of which owners were served natively.
//!/ Serve one native call against a SUPER CHAIN, first owner that handles it wins.
//!/
//!/ The chain walk used to re-enter `try_invoke` per owner, so a 3-deep chain paid
//!/ the bridge prologue THREE times — and the arena-string canonicalization in that
//!/ prologue COPIES every string argument, so a miss was not cheap. It also billed
//!/ the profiler three times for one logical call, which read as three separate hot
//!/ natives (`StringBuilder#write` / `Writer#write` / `Object#write`, and in the
//!/ datapack load `Lifecycle$2#equals` / `Lifecycle#equals` / `Object#equals`, each
//!/ at the identical call count). Prologue once, walk the owners over the inner
//!/ dispatch: the walk ORDER and every arm's semantics are unchanged, only the
//!/ per-miss cost and the billing are.
//!/ Which classes are ACTUALLY served by a native, as opposed to merely looking like it.
//!/
//!/ The birth rule forces a class to a heap handle when `birth_native_backed_alloc` matches
//!/ its name or namespace — a proxy for "its state lives in synthetic `$fields` we manage".
//!/ For `it/unimi/dsi/fastutil/…ArrayMap` the tree's own test asserts there is NO contract for
//!/ `get`, so the proxy may be wrong for a whole family, and 14,540 blocking cells sit behind
//!/ exactly that question for `LinkedTreeMap`. Static analysis of "does every method resolve
//!/ to bytecode" is expensive; this is the same question answered by observation.

use std::sync::{
    atomic::{AtomicU64, Ordering::Relaxed},
    Mutex,
};

pub fn enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SERVED_CENSUS").is_some())
}

fn tally() -> &'static Mutex<std::collections::BTreeMap<String, u64>> {
    pub(crate) static M: std::sync::OnceLock<Mutex<std::collections::BTreeMap<String, u64>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

#[inline]
pub fn note(owner: &str) {
    if !enabled() {
        return;
    }
    static TOTAL: AtomicU64 = AtomicU64::new(0);
    let mut tally = tally().lock().unwrap_or_else(|error| error.into_inner());
    match tally.get_mut(owner) {
        Some(count) => *count += 1,
        None => {
            tally.insert(owner.to_string(), 1);
        }
    }
    drop(tally);
    let total = TOTAL.fetch_add(1, Relaxed) + 1;
    if matches!(total, 100_000 | 500_000 | 1_000_000 | 2_000_000)
        || total % 5_000_000 == 0
    {
        report();
    }
}

/// Was `owner` ever the class that actually answered a native call?
pub fn ever_served(owner: &str) -> bool {
    tally().lock().unwrap_or_else(|e| e.into_inner()).contains_key(owner)
}

pub fn report() {
    if !enabled() {
        return;
    }
    let g = tally().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&String, &u64)> = g.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!("[SERVED] {} distinct classes actually answered a native call", rows.len());
    for (cls, n) in rows.iter().take(20) {
        eprintln!("[SERVED] {n:>10}  {cls}");
    }
}
