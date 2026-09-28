//! G1.5 FORK TARGET HUNT (RAVA_FORK_TARGET_SCAN, inert). Aggregates every for-each iterator loop the
//! runtime sees into a ledger keyed by `owner.method+desc`, recording the loop's scale (n) and the
//! SOUNDNESS-relevant properties: tree-compiled (all_compiled ⇒ no callback ⇒ no garbage-continuation,
//! the G1.4 lesson), object/array allocation, and structural fork-safety. The top-n-coverage entries
//! that are all_compiled + fork_safe are the safe high-ROI 15-core targets. Purely observational.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FORK_TARGET_SCAN").is_some())
}

#[derive(Default)]
pub struct Entry {
    pub count: u64,      // times this loop executed
    pub total_n: u64,    // Σ n (total per-element work = coverage)
    pub max_n: u64,      // largest single-loop element count
    pub props_set: bool, // static properties computed (once per key)
    pub all_compiled: bool,
    pub has_obj: bool,
    pub has_arr: bool,
    pub fork_safe: bool,
}

static REGISTRY: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);
static CALLS: AtomicU64 = AtomicU64::new(0);

/// True the FIRST time `key` is seen (caller then computes + sets the static props). Marks it seen.
pub fn is_new(key: &str) -> bool {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.contains_key(key) {
        false
    } else {
        m.insert(key.to_string(), Entry::default());
        true
    }
}

pub fn set_props(key: &str, all_compiled: bool, has_obj: bool, has_arr: bool, fork_safe: bool) {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = g.get_or_insert_with(HashMap::new).get_mut(key) {
        e.props_set = true;
        e.all_compiled = all_compiled;
        e.has_obj = has_obj;
        e.has_arr = has_arr;
        e.fork_safe = fork_safe;
    }
}

pub fn record_n(key: &str, n: u64) {
    {
        let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.get_or_insert_with(HashMap::new).get_mut(key) {
            e.count += 1;
            e.total_n += n;
            e.max_n = e.max_n.max(n);
        }
    }
    let c = CALLS.fetch_add(1, Relaxed) + 1;
    // Low milestones + frequent periodic: the boot is HARD-killed after Done (no clean exit dump),
    // and forkable-loop detections are only in the hundreds, so dump early + often.
    if c == 50 || c == 500 || c == 2_000 || c % 5_000 == 0 {
        dump();
    }
}

/// The ledger: every loop, sorted by total per-element coverage (Σn), with soundness flags.
pub fn dump() {
    if !enabled() {
        return;
    }
    let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else {
        eprintln!("[FORK-SCAN] no iterator loops observed");
        return;
    };
    let mut rows: Vec<(&String, &Entry)> = m.iter().collect();
    rows.sort_by(|a, b| b.1.total_n.cmp(&a.1.total_n));
    eprintln!(
        "[FORK-SCAN] {} distinct for-each loops | {} total loop executions. Sorted by Σn (coverage):",
        m.len(),
        CALLS.load(Relaxed)
    );
    eprintln!("[FORK-SCAN]  {:>10} {:>7} {:>7} | compiled alloc fork-safe  method", "Σn", "maxN", "calls");
    for (k, e) in rows.iter().take(30) {
        let alloc = if e.has_obj { "obj" } else if e.has_arr { "arr" } else { "none" };
        let safe_target = e.all_compiled && e.fork_safe && e.max_n >= 100;
        eprintln!(
            "[FORK-SCAN] {}{:>10} {:>7} {:>7} |  {:>5}   {:>4}  {:>6}    {}",
            if safe_target { "★ " } else { "  " },
            e.total_n, e.max_n, e.count,
            if e.all_compiled { "YES" } else { "no" },
            alloc,
            if e.fork_safe { "YES" } else { "no" },
            k
        );
    }
    // The verdict list: SOUND high-ROI targets (all_compiled → no garbage-continuation, per G1.4).
    let mut targets: Vec<(&String, &Entry)> = m
        .iter()
        .filter(|(_, e)| e.all_compiled && e.fork_safe && e.max_n >= 100)
        .collect();
    targets.sort_by(|a, b| b.1.total_n.cmp(&a.1.total_n));
    eprintln!("[FORK-SCAN] === SOUND high-ROI targets (all_compiled + fork_safe + maxN≥100) ===");
    if targets.is_empty() {
        eprintln!("[FORK-SCAN]   (none — no large loop has a fully-compiled fork-safe tree)");
    }
    for (k, e) in targets.iter().take(5) {
        eprintln!("[FORK-SCAN]   Σn={} maxN={} calls={}  {}", e.total_n, e.max_n, e.count, k);
    }
}
