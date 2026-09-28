//! PER-METHOD INVOCATION CENSUS (`RAVA_CALL_CENSUS=<substr>`, default OFF, diagnostic only).
//!
//! Every profile in this tree measures TIME. Time cannot distinguish "this method is slow"
//! from "this method is called too many times", and ledger row 149 is stuck on exactly that
//! distinction: the `Done` window burns 56.6 cpu-seconds against a JVM window bounded at
//! ≤0.8, which reads as ~70× the WORK — but peak RSS is only 3.65× the JVM's, and the
//! window's named content allocates. One of those two readings is incomplete, and a COUNT
//! is the only thing that separates them.
//!
//! Counts entries at BOTH method-entry points, because either alone is a partial answer: a
//! compiled method never passes through `Vm::invoke`, and an interpreted one never reaches
//! `jit_run`. Keyed by `Class#name`, filtered by substring so the table stays small and the
//! hot path stays a single `enabled()` bool test when the flag is unset.
//!
//! ⚠ **KNOWN BLIND SPOT, AND IT IS THE IMPORTANT ONE: COMPILED → COMPILED CALLS ARE INVISIBLE
//! HERE.** `Vm::invoke` is the interpreter's entry and `jit_run` is the interpreter's entry
//! INTO compiled code; a compiled caller reaching a compiled callee goes through neither.
//! Ledger row 129 measured that population directly: `direct_hit = 195,028,243` served by the
//! worker direct cache alone. **So every number this census prints is a LOWER BOUND, and it
//! under-counts exactly the hot paths it was built to investigate.**
//!
//! First run, `RAVA_CALL_CENSUS=Climate`: `Climate$RTree#build` **1,028** entries while the
//! method table puts it at ~9.8% of a 15 s window. Those cannot both be complete — 1.5 s over
//! 1,028 entries is 1.46 ms each — so the counts are being missed, the time is landing in
//! compiled-to-compiled recursion, or both. **The census cannot currently tell which**, which
//! is precisely what it was built to decide.
//!
//! Closing the gap means counting in the COMPILED PROLOGUE (codegen work, `jit.rs`), not in
//! another host-side hook. Until then: read these as a floor, never as a population.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn filter() -> Option<&'static str> {
    static F: OnceLock<Option<String>> = OnceLock::new();
    F.get_or_init(|| std::env::var("RAVA_CALL_CENSUS").ok().filter(|s| !s.is_empty()))
        .as_deref()
}

#[inline]
pub(crate) fn enabled() -> bool {
    let on = filter().is_some();
    // Started HERE, not on the first FILTER MATCH. The first version armed the watchdog inside
    // `note`, so a run whose matching methods appear late (the `Climate` family lives in the
    // Done window) printed nothing at all before the run ended — the same lazy-arming defect
    // this session fixed in `pure_memo` and `vecgrow`. Idempotent, and off the hot path because
    // the caller gates on this bool.
    if on {
        start_watchdog();
    }
    on
}

fn table() -> &'static Mutex<HashMap<String, u64>> {
    static T: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One entry into `class#name`. Cold by construction: the caller checks `enabled()` first.
pub(crate) fn note(class: &str, name: &str) {
    let Some(want) = filter() else { return };
    if !class.contains(want) {
        return;
    }
    if let Ok(mut t) = table().lock() {
        *t.entry(format!("{class}#{name}")).or_insert(0) += 1;
    }
}

/// Periodic dump — the third diagnostic in this tree to need one. A report wired only to
/// teardown says nothing about a server boot (ledger rows 104, 114, 138).
fn start_watchdog() {
    static W: OnceLock<()> = OnceLock::new();
    W.get_or_init(|| {
        let ms = std::env::var("RAVA_CALL_CENSUS_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(20_000)
            .max(1_000);
        let _ = std::thread::Builder::new()
            .name("rava-callcensus".to_string())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                report();
            });
    });
}

pub(crate) fn report() {
    let Ok(t) = table().lock() else { return };
    if t.is_empty() {
        return;
    }
    let mut rows: Vec<(&String, &u64)> = t.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("[CALL-CENSUS] {} distinct methods matching the filter:", rows.len());
    for (k, n) in rows.iter().take(30) {
        eprintln!("[CALL-CENSUS] {n:>12}  {k}");
    }
}
