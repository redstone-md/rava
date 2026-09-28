//! WHICH `return None` in `jit_direct_resolve` fires (`RAVA_CALLBACK_STATS`, default OFF).
//!
//! WHY. Ledger row 163 measured the direct-call miss breakdown and found
//! `dmiss_recv=253,732 · dmiss_mega=0 · dmiss_res=28,969,740` — **99.1% of misses are one
//! function declining to produce an entry, and not one is a megamorphic site.** That refutes
//! the polymorphism framing outright, but it names a 338-line function with two dozen distinct
//! refusals and says nothing about which of them is the 29 M.
//!
//! A count per SITE is the only thing that can say. Each plain `return None;` in that function
//! became `return dresolve::refuse(K);` — mechanical, compiler-checked, and the id is the line's
//! ordinal so it stays readable against the source without a name table to drift out of date.
//!
//! ⚠ The ids are POSITIONAL. Editing `jit_direct_resolve` renumbers everything after the edit
//! point, so a report is only meaningful against the revision that produced it. That is the
//! deliberate trade: a stable name table would be one more thing to forget to update, and this
//! diagnostic exists to be read once and then acted on.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Room for every refusal site plus headroom; over-indexing folds into the last bucket rather
/// than panicking, because a diagnostic must never be the thing that kills a boot.
const N: usize = 48;
static HITS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

/// Bump site `k` and return `None`. `#[inline]` so the default (unarmed) path is one predicted
/// branch on an already-cached `OnceLock` bool — `jit_direct_resolve` is on the dispatch path
/// and must not grow a call for a switched-off diagnostic.
#[inline]
pub(crate) fn refuse<T>(k: usize) -> Option<T> {
    if enabled() {
        HITS[k.min(N - 1)].fetch_add(1, Relaxed);
    }
    None
}

pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // Shares `RAVA_CALLBACK_STATS` with `DIRECT_MISS` deliberately: this table is only readable
    // NEXT TO `dmiss_res`, and arming one without the other produces a share of an unknown total.
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_CALLBACK_STATS").is_some()
            || std::env::var_os("RAVA_JIT_STATS").is_some()
    })
}

/// Non-zero sites, richest first, as one line. Empty when nothing fired, so a caller can print
/// it unconditionally without emitting a blank row every heartbeat.
pub(crate) fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let mut rows: Vec<(usize, u64)> = (0..N)
        .map(|i| (i, HITS[i].load(Relaxed)))
        .filter(|&(_, n)| n > 0)
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u64 = rows.iter().map(|&(_, n)| n).sum();
    let body = rows
        .iter()
        .take(10)
        .map(|&(i, n)| format!("#{i}={n}({:.0}%)", n as f64 * 100.0 / total as f64))
        .collect::<Vec<_>>()
        .join(" ");
    format!("total={total} {body}")
}

/// THE FIVE CLAUSES of refusal site #20, split (`RAVA_CALLBACK_STATS`).
///
/// Row 164: this one site is 87% of all `jit_direct_resolve` refusals and gates 28.9 M calls a
/// boot off the direct path. But "excluded" is not "correctly excluded" — the test is a NAME
/// approximation of a structural property, *is this class natively served?*, and the ledger has
/// already caught that approximation being wrong elsewhere (`owner_bits` is the seven-substring
/// heuristic memoised, not a contract table).
///
/// Two numbers decide whether there is anything here: WHICH clause carries the volume, and
/// WHICH classes it carries. A clause dominated by classes a native genuinely serves is correct
/// and closed; one dominated by classes no native ever touches is an over-approximation with a
/// measurable size.
///
/// ⭐**AND THE LADDER ITSELF IS REPEATED WORK, BECAUSE FOUR OF ITS FIVE VERDICTS ARE
/// PERMANENT.** Site #20 runs, per CALL: `starts_with("java/")` plus a `class_loader::overrides`
/// read-lock probe, `starts_with('$')`, `cache::is_cache_class` (a substring ladder),
/// `fastutil::is_int_set` (another), and `opcodes::force_native_collection` (seven-plus
/// `starts_with`/`contains` scans over the owner). Its inputs are `cls` and `name` — a
/// `to_string()` of the receiver's class name and of the constant-pool method name, both
/// freshly ALLOCATED at the top of `jit_direct_resolve` — and the answer is a function of
/// nothing else. With `dmiss_res = 28,969,740`, that is a name-shaped clause ladder plus two
/// string allocations, ~29 M times, over a (class, method) population that a boot counts in
/// the low hundreds.
///
/// ⛔**NOT MEMOISED HERE, AND THE REASON IS CLAUSE 0.** `class_loader::has_bytecode` is NOT a
/// process constant: `note_bytecode` registers a name the first time a classfile is actually
/// LOADED for it, so `has_bytecode` moves false → true (never back) as the boot proceeds. The
/// four name-only clauses are permanent and memoisable as they stand; clause 0's verdict is
/// only permanent in the "fired" direction, so a naive memo would PIN a refusal for a class
/// that has since gained real bytecode — silently keeping a compilable body off the direct
/// path, which is exactly the over-approximation the comment above was written to hunt.
///
/// ⇒ The memo needs an EPOCH: the override registry's insert count, bumped by `note_bytecode`,
/// stamped into the entry and compared on read (a mismatch re-runs the ladder and restamps).
/// That is a `class_loader` change plus a `jit_direct_resolve` call-site change, neither of
/// which is this file — recorded here rather than half-built, because a memo that cannot prove
/// its precondition must refuse.
pub(crate) const CLAUSES: [&str; 5] =
    ["java/*", "$*", "is_cache_class", "is_int_set", "force_native_collection"];
static CLAUSE_HITS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Per-(clause, class) tally. A plain mutex map: this is a diagnostic behind a default-OFF flag,
/// and the site it sits on is already the slow half of a dispatch miss.
fn names() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    static T: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn note_clause(clause: usize, cls: &str, name: &str) {
    if !enabled() {
        return;
    }
    CLAUSE_HITS[clause.min(4)].fetch_add(1, Relaxed);
    if let Ok(mut m) = names().lock() {
        // Keyed by clause AND class: the same class can only reach one clause, but printing the
        // clause beside it saves cross-referencing two tables to read one row.
        *m.entry(format!("{}|{cls}#{name}", CLAUSES[clause.min(4)])).or_insert(0) += 1;
    }
}

/// Clause split plus the richest (clause, class#method) rows.
pub(crate) fn clause_report(top: usize) -> String {
    if !enabled() {
        return String::new();
    }
    let hits: Vec<u64> = CLAUSE_HITS.iter().map(|a| a.load(Relaxed)).collect();
    let total: u64 = hits.iter().sum();
    if total == 0 {
        return String::new();
    }
    let split = CLAUSES
        .iter()
        .zip(&hits)
        .filter(|&(_, &n)| n > 0)
        .map(|(c, &n)| format!("{c}={n}({:.0}%)", n as f64 * 100.0 / total as f64))
        .collect::<Vec<_>>()
        .join(" ");
    let Ok(m) = names().lock() else { return split };
    let mut rows: Vec<(&String, u64)> = m.iter().map(|(k, &v)| (k, v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let body = rows
        .iter()
        .take(top)
        .map(|(k, n)| format!("{k}={n}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{split} | {body}")
}
