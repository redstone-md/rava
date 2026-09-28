//! BACK-EDGE CENSUS (`RAVA_BACKEDGE_CENSUS`, default OFF, inert otherwise).
//!
//! WHY. Ledger row 190: rava compiles **42 methods** during a boot where HotSpot's C1 compiles
//! **6,675**, and the decomposition (`tried=22 refused=1`) says almost nothing is refused —
//! almost nothing arrives. The mechanism is in HotSpot's own globals:
//! `Tier3InvocationThreshold = 200` **and `Tier3BackEdgeThreshold = 60000`**, so a method
//! compiles when its LOOP is hot, not only when it is called often, and OSR then transfers the
//! running loop into the compiled body. rava counts invocations only and cannot OSR.
//!
//! ⚠ THE POINT OF THIS FILE IS TO PRICE THAT ARC BEFORE BUILDING IT. OSR is a large feature, and
//! row 172 already shows the cost of guessing the trigger: compiling every method that merely
//! CONTAINS a back edge measured **+7.1% wall**. So the question this answers is narrow and
//! decisive: **how much of the boot's looping happens inside methods that are entered ONCE?**
//! If the top back-edge methods are entered many times, a back-edge counter alone pays and no
//! OSR is needed. If they are entered once, OSR is the whole prize and nothing short of it will
//! do.
//!
//! Keyed by the `Code` pointer (stable for a loaded class), counted per method, and reported
//! against the method's own entry count so both halves of the question are on one line.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_BACKEDGE_CENSUS").is_some())
}

struct Row {
    name: String,
    edges: u64,
    entries: u64,
}

fn table() -> &'static Mutex<std::collections::HashMap<usize, Row>> {
    static T: OnceLock<Mutex<std::collections::HashMap<usize, Row>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Total back edges taken, so the top rows can be read as a share rather than a raw count.
pub(crate) static TOTAL: AtomicU64 = AtomicU64::new(0);

/// One backward branch taken in `code`. The name is resolved lazily by the caller only on the
/// first sighting, so the hot path is a pointer key and a counter bump.
pub(crate) fn note(code_ptr: usize, name: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    TOTAL.fetch_add(1, Relaxed);
    if let Ok(mut t) = table().lock() {
        t.entry(code_ptr)
            .or_insert_with(|| Row { name: name(), edges: 0, entries: 0 })
            .edges += 1;
    }
}

/// One ENTRY into `code` — the invocation count a back-edge trigger has to be judged against.
pub(crate) fn note_entry(code_ptr: usize, name: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    if let Ok(mut t) = table().lock() {
        t.entry(code_ptr)
            .or_insert_with(|| Row { name: name(), edges: 0, entries: 0 })
            .entries += 1;
    }
}

/// Top methods by back edges, each with its entry count. `entries=1` is the OSR case.
pub(crate) fn report(top: usize) -> String {
    if !enabled() {
        return String::new();
    }
    let Ok(t) = table().lock() else { return String::new() };
    let total = TOTAL.load(Relaxed);
    if total == 0 {
        return String::new();
    }
    let mut rows: Vec<(&String, u64, u64)> =
        t.values().map(|r| (&r.name, r.edges, r.entries)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    // How much of ALL looping sits in methods entered once — the number that decides the arc.
    let once: u64 = rows.iter().filter(|r| r.2 <= 1).map(|r| r.1).sum();
    let body = rows
        .iter()
        .take(top)
        .map(|(n, e, en)| format!("{n}[edges={e} entries={en}]"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "total_edges={total} methods={} once_entered_share={:.1}% | {body}",
        rows.len(),
        once as f64 * 100.0 / total as f64
    )
}

/// Rows whose name starts with `prefix`, ranked by ENTRIES rather than edges.
///
/// The main report ranks by back edges, which is right for loops and useless for the
/// `fu_fastpath` refusal census bolted onto the same table: those rows have zero edges and only
/// entry counts, so they sort to the bottom and never print. 7.9 M refusals a boot deserve their
/// own ranking (ledger row 198).
pub(crate) fn report_prefix(prefix: &str, top: usize) -> String {
    if !enabled() {
        return String::new();
    }
    let Ok(t) = table().lock() else { return String::new() };
    let mut rows: Vec<(&String, u64)> = t
        .values()
        .filter(|r| r.name.starts_with(prefix))
        .map(|r| (&r.name, r.entries))
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u64 = rows.iter().map(|r| r.1).sum();
    let body = rows
        .iter()
        .take(top)
        .map(|(n, e)| format!("{}={e}", n.trim_start_matches(prefix)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("total={total} distinct={} | {body}", rows.len())
}
