//! `RAVA_RESIDUE_PROBE` — per-class attribution of the map-key / stringify path.
//!
//! The boot's serial residue is dominated by value-equality map keys, which are
//! derived by running a guest `toString()` over a whole object subtree. Category
//! samplers bill that to `JIT_EXEC` and bridge stats never see it, so this probe
//! bills the OUTERMOST `value_to_string` per receiver class (wall + rendered
//! bytes) and tallies the synthesized record methods that feed it.
//!
//! Fully inert unless the env var is set: one relaxed atomic-bool load per site.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Default, Clone, Copy)]
struct Row {
    calls: u64,
    nanos: u64,
    bytes: u64,
}

static TABLE: OnceLock<Mutex<HashMap<String, Row>>> = OnceLock::new();
static RECORDS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
static NESTED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_RESIDUE_PROBE").is_some())
}

/// RAII billing scope for one `value_to_string`. Only the OUTERMOST frame is
/// billed, so nested subtree renders are charged to the root they belong to.
pub(crate) struct Scope {
    outer: bool,
    start: Option<Instant>,
    class: String,
}

impl Scope {
    pub(crate) fn enter(class: &str) -> Self {
        if !enabled() {
            return Self { outer: false, start: None, class: String::new() };
        }
        let outer = DEPTH.with(|d| {
            let was = d.get();
            d.set(was + 1);
            was == 0
        });
        if !outer {
            NESTED.fetch_add(1, Relaxed);
            return Self { outer: false, start: None, class: String::new() };
        }
        Self { outer: true, start: Some(Instant::now()), class: class.to_owned() }
    }

    pub(crate) fn finish(self, rendered: usize) {
        if let (true, Some(start)) = (self.outer, self.start) {
            let nanos = start.elapsed().as_nanos() as u64;
            let mut table = TABLE.get_or_init(Default::default).lock().unwrap();
            let row = table.entry(self.class.clone()).or_default();
            row.calls += 1;
            row.nanos += nanos;
            row.bytes += rendered as u64;
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if enabled() {
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
}

/// Tally one synthesized record method (`toString` / `hashCode` / `equals`).
pub(crate) fn record_method(kind: &str, class: &str) {
    if !enabled() {
        return;
    }
    let mut table = RECORDS.get_or_init(Default::default).lock().unwrap();
    *table.entry(format!("{kind} {class}")).or_default() += 1;
}

/// Tally one `StringifyMemo` outcome — the memo is what turns the DAG-shaped
/// key derivation from O(flattened text) back into O(distinct nodes).
pub(crate) fn memo_event(outcome: &'static str) {
    if !enabled() {
        return;
    }
    let mut table = MEMO.get_or_init(Default::default).lock().unwrap();
    *table.entry(outcome).or_default() += 1;
}

static MEMO: OnceLock<Mutex<HashMap<&'static str, u64>>> = OnceLock::new();

/// Tally one token-memo probe. A HIT is a DAG edge whose subtree render was already
/// derived — and, on the historic ordering, an edge that had nonetheless just paid a
/// class-name `to_string`, a transitive purity proof and a name-keyed `resolve_method`
/// to discover that. The hit RATE is therefore the exact size of the prologue that
/// `RAVA_TOKEN_PROBE_FIRST` moves behind the lookup.
static TOKEN_HIT: AtomicU64 = AtomicU64::new(0);
static TOKEN_MISS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn token_probe(hit: bool) {
    if !enabled() {
        return;
    }
    if hit { &TOKEN_HIT } else { &TOKEN_MISS }.fetch_add(1, Relaxed);
}

/// Tally one `record_object_method` call: whether it was served by a linked site,
/// and how many components it read. `linked x components` is exactly the number of
/// name-keyed `resolve_field_id` lookups `RAVA_RECORD_LINK` removes, and `linked` is
/// the number of class-name `String` allocations.
static REC_LINKED: AtomicU64 = AtomicU64::new(0);
static REC_UNLINKED: AtomicU64 = AtomicU64::new(0);
static REC_IDS_SAVED: AtomicU64 = AtomicU64::new(0);

static REC_BUILT: AtomicU64 = AtomicU64::new(0);

/// One link resolved from names. A healthy cache builds ONE per (site, class); a
/// count near the linked-call count would mean the key churns and the memo is a loss.
pub(crate) fn record_link_built() {
    if !enabled() {
        return;
    }
    REC_BUILT.fetch_add(1, Relaxed);
}

pub(crate) fn record_call(linked: bool, components: usize) {
    if !enabled() {
        return;
    }
    if linked {
        REC_LINKED.fetch_add(1, Relaxed);
        REC_IDS_SAVED.fetch_add(components as u64, Relaxed);
    } else {
        REC_UNLINKED.fetch_add(1, Relaxed);
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let table = TABLE.get_or_init(Default::default).lock().unwrap();
    let mut rows: Vec<(&String, &Row)> = table.iter().collect();
    rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.nanos));
    let total: u64 = rows.iter().map(|(_, r)| r.nanos).sum();
    eprintln!(
        "[RESIDUE] outermost value_to_string: {:.3}s over {} roots ({} nested renders)",
        total as f64 / 1e9,
        rows.iter().map(|(_, r)| r.calls).sum::<u64>(),
        NESTED.load(Relaxed),
    );
    for (class, row) in rows.iter().take(25) {
        eprintln!(
            "[RESIDUE]   {:>8.3}s  n={:<9} avg={:>9.1}us  bytes={:<12} avg_len={:<8} {}",
            row.nanos as f64 / 1e9,
            row.calls,
            row.nanos as f64 / row.calls.max(1) as f64 / 1e3,
            row.bytes,
            row.bytes / row.calls.max(1),
            class,
        );
    }
    let (hit, miss) = (TOKEN_HIT.load(Relaxed), TOKEN_MISS.load(Relaxed));
    if hit + miss > 0 {
        eprintln!(
            "[RESIDUE] token memo: {hit} hits / {miss} misses ({:.1}% hit) — a hit is one DAG edge \
             whose render was already derived; before RAVA_TOKEN_PROBE_FIRST each one still paid \
             a class-name to_string + purity proof + resolve_method first",
            hit as f64 * 100.0 / (hit + miss) as f64,
        );
    }
    let (linked, unlinked, saved) = (
        REC_LINKED.load(Relaxed),
        REC_UNLINKED.load(Relaxed),
        REC_IDS_SAVED.load(Relaxed),
    );
    if linked + unlinked > 0 {
        eprintln!(
            "[RESIDUE] record calls: {linked} linked / {unlinked} unlinked, {} links BUILT — \
             the linked ones skipped {} class-name String allocations and {} name-keyed \
             resolve_field_id lookups",
            REC_BUILT.load(Relaxed),
            linked.saturating_sub(REC_BUILT.load(Relaxed)),
            saved,
        );
    }
    let memo = MEMO.get_or_init(Default::default).lock().unwrap();
    let mut mrows: Vec<(&&str, &u64)> = memo.iter().collect();
    mrows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!("[RESIDUE] stringify memo");
    for (outcome, n) in mrows {
        eprintln!("[RESIDUE]   {n:<12} {outcome}");
    }
    let records = RECORDS.get_or_init(Default::default).lock().unwrap();
    let mut rrows: Vec<(&String, &u64)> = records.iter().collect();
    rrows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!("[RESIDUE] synthesized record methods (top 20 of {})", rrows.len());
    for (name, n) in rrows.iter().take(20) {
        eprintln!("[RESIDUE]   {n:<12} {name}");
    }
}
