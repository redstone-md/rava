//! icmiss.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_IC_VTABLE_FALLBACK").is_some())
}
pub static ENTRY: AtomicU64 = AtomicU64::new(0);
pub static NON_ARENA: AtomicU64 = AtomicU64::new(0);
pub static NEG_HIT: AtomicU64 = AtomicU64::new(0);
pub static R_COMPILED: AtomicU64 = AtomicU64::new(0);
pub static R_NEVER: AtomicU64 = AtomicU64::new(0);
pub static R_NOTYET: AtomicU64 = AtomicU64::new(0);

/// WHY a `NeverCacheable` verdict, split by clause — the same question `dresolve::CLAUSES`
/// answers for the OTHER resolver, and for the same reason: that verdict is PERMANENT, so a
/// clause that is an over-approximation costs every later call of every site it touches, for the
/// rest of the boot. The lane probe reports the ring's locked callees as 412,624 `Predicate#test`
/// plus 412,623 `Function#apply` — guest lambdas — and a lambda receiver's class is `$Lambda`,
/// which clause 1 (`$*`) refuses. This table says whether that is where they come from.
/// Clauses 0..5 are `dispatch_resolve_cacheable_cls`'s, in its own order. Clause 6 is the
/// EARLY bail in `vm::pic_fill`, which short-circuits the same `java/*`/`$*` verdict before the
/// resolver runs — so clauses 0 and 1 read 0 on a real boot while 6 carries their whole
/// population. Named separately rather than folded in, so the two are never confused for one.
pub const NEVER_CLAUSES: [&str; 7] = [
    "java/*",
    "$*",
    "is_cache_class",
    "is_int_set",
    "force_native",
    "native_served_deep",
    "jdk-bail",
];
pub static NEVER_WHY: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// Per-CLASS tally for the clause that carries the volume. A clause total says which
/// approximation to attack; only the class names say whether it is an approximation at all —
/// classes a native genuinely serves are correctly excluded, classes no native touches are not.
fn never_names() -> &'static Mutex<Option<std::collections::HashMap<String, u64>>> {
    static T: Mutex<Option<std::collections::HashMap<String, u64>>> = Mutex::new(None);
    &T
}

/// Periodic `[NEVER-WHY]` dump (default 3 s, `RAVA_NEVER_EVERY_MS` overrides).
///
/// Armed by the FIRST clause that fires, i.e. under exactly the gate `dispatch.rs:572`
/// (`freeze_resolve::stats_on() || dresolve::enabled()`) that admitted the counter — so there is
/// no call site to remember and no path that arms a thread whose counters are never written.
/// A server boot never returns from guest `main`, so the exit-chain report in `pc_profile` /
/// `lane_probe` is unreachable and the tally was unreadable on precisely the workloads it exists
/// for. Same shape as `devirt::steady_enabled`'s dumper; see the note there.
fn arm_never_dumper() {
    static ARMED: OnceLock<()> = OnceLock::new();
    let _ = ARMED.get_or_init(|| {
        let every_ms = std::env::var("RAVA_NEVER_EVERY_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|n| *n >= 500)
            .unwrap_or(3000);
        let _ = std::thread::Builder::new()
            .name("rava-never-why".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(every_ms));
                let why = never_why_report();
                if !why.is_empty() {
                    eprintln!("[NEVER-WHY] {why}");
                    let names = never_names_report(12);
                    if !names.is_empty() {
                        eprintln!("[NEVER-NAMES] {names}");
                    }
                }
            });
    });
}

pub fn note_never_class(clause: usize, cls: &str, name: &str) {
    arm_never_dumper();
    if let Ok(mut g) = never_names().lock() {
        *g.get_or_insert_with(std::collections::HashMap::new)
            .entry(format!("{}|{cls}#{name}", NEVER_CLAUSES[clause.min(6)]))
            .or_insert(0) += 1;
    }
}

/// Richest (clause, class) rows.
pub fn never_names_report(top: usize) -> String {
    let Ok(g) = never_names().lock() else { return String::new() };
    let Some(m) = g.as_ref() else { return String::new() };
    let mut rows: Vec<(&String, u64)> = m.iter().map(|(k, &v)| (k, v)).collect();
    if rows.is_empty() {
        return String::new();
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    format!(
        "distinct={} | {}",
        rows.len(),
        rows.iter().take(top).map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(" ")
    )
}

/// Non-zero clauses, richest first. Empty when nothing fired.
pub fn never_why_report() -> String {
    let rows: Vec<(usize, u64)> = (0..6)
        .map(|i| (i, NEVER_WHY[i].load(Relaxed)))
        .filter(|&(_, n)| n > 0)
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let total: u64 = rows.iter().map(|&(_, n)| n).sum();
    let body = rows
        .iter()
        .map(|&(i, n)| format!("{}={n}({:.0}%)", NEVER_CLAUSES[i], n as f64 * 100.0 / total as f64))
        .collect::<Vec<_>>()
        .join(" ");
    format!("total={total} {body}")
}
static PAIRS: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

/// Note a resolved (site,class) pair; returns true the FIRST time (a genuine first resolve),
/// false on a repeat (an evicted-then-re-missed pair = re-resolve thrash).
pub fn seen_pair(cache_ptr: i64, class_id: u32) -> bool {
    let key = ((cache_ptr as u64) << 20) ^ class_id as u64;
    let mut g = PAIRS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashSet::new).insert(key)
}

pub fn dump() {
    let g = PAIRS.lock().unwrap_or_else(|e| e.into_inner());
    let distinct = g.as_ref().map(|s| s.len()).unwrap_or(0);
    let (e, na, neg, rc, rn, rny) = (
        ENTRY.load(Relaxed), NON_ARENA.load(Relaxed), NEG_HIT.load(Relaxed),
        R_COMPILED.load(Relaxed), R_NEVER.load(Relaxed), R_NOTYET.load(Relaxed),
    );
    let resolves = rc + rn + rny;
    let pc = |n: u64| if e > 0 { n * 100 / e } else { 0 };
    eprintln!("[IC-MISS] total dispatch-miss entries={e}");
    eprintln!("[IC-MISS]   non-arena recv (straight to jit_call)      {na:>12} ({}%)", pc(na));
    eprintln!("[IC-MISS]   known-negative fast-path (skip resolve)    {neg:>12} ({}%)", pc(neg));
    eprintln!("[IC-MISS]   ran resolve                                {resolves:>12} ({}%)", pc(resolves));
    eprintln!("[IC-MISS]       -> Compiled(fptr, cacheable)           {rc:>12} ({}%)", pc(rc));
    eprintln!("[IC-MISS]       -> NeverCacheable (java/native)        {rn:>12} ({}%)", pc(rn));
    eprintln!("[IC-MISS]       -> NotYet (not compiled)               {rny:>12} ({}%)", pc(rny));
    eprintln!("[IC-MISS]   distinct (site,class) pairs resolved       {distinct:>12}");
    let rereslv = resolves.saturating_sub(distinct as u64);
    eprintln!("[IC-MISS]   => RE-RESOLVES (resolves - distinct)       {rereslv:>12} ({}% of entries) <- the fixable thrash", pc(rereslv));
}
