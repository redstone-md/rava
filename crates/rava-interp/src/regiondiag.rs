//! 18.1 M4 VOXEL-BOOT INSPECTOR (RAVA_DIAG_VOXEL_BOOT). Read-only landscape probe for the
//! shape-precompute phase (`Block.getShapeForEachState` + its per-state lambdas), the M4 fork
//! Target #1. A thread-local region depth stays active across the (interpreted) per-state shape
//! body so every nested `net/minecraft/world/phys/shapes/Shapes.*` factory/interning call and
//! every getstatic/putstatic executed inside the traversal is ATTRIBUTED to it. Answers the fork
//! feasibility questions: (1) region compute-density (body-entry count), (2) which interning-layer
//! factory methods / static fields the body dereferences, (3) whether it hits UNISOLATED static
//! writes (putstatic) or clinit-triggering getstatic MISSES — the exact worker-abort hazards the
//! column-fork must isolate (getstatic cache HITS are read-only ⇒ fork-safe per 17.1). Absolutely
//! inert: counts only, never touches a value or control-flow path ⇒ hashes converge bit-for-bit.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

// Registered regions. Each carries its own env gate, body-entry matcher, inner-call filter and
// counter set — so ONE inspector maps any boot phase's fork landscape (18.1 voxel + 18.3 json).
pub const VOXEL: usize = 0;
pub const JSON: usize = 1;
const NREG: usize = 2;
const ENV: [&str; NREG] = ["RAVA_DIAG_VOXEL_BOOT", "RAVA_DIAG_JSON_BOOT"];
const LABEL: [&str; NREG] = ["VOXEL-DIAG", "JSON-DIAG"];

struct Stat {
    calls: AtomicU64,     // region-body entries (compute-density / parse-pass index)
    inner: AtomicU64,     // tracked nested lib invokes inside the region
    native: AtomicU64,    // (JSON) parses served natively by gson_native.rs (bypass the guest frame)
    gs_hit: AtomicU64,    // getstatic served from tlcache (read-only, fork-safe)
    gs_miss: AtomicU64,   // getstatic resolve/<clinit> path (abort hazard)
    ps: AtomicU64,        // putstatic inside region (shared write = hazard)
    inner_m: Mutex<Option<HashMap<String, u64>>>,
    gs_sites: Mutex<Option<HashMap<String, u64>>>,
    gs_miss_sites: Mutex<Option<HashMap<String, u64>>>,
    ps_sites: Mutex<Option<HashMap<String, u64>>>,
}
impl Stat {
    const fn new() -> Self {
        Stat {
            calls: AtomicU64::new(0), inner: AtomicU64::new(0), native: AtomicU64::new(0),
            gs_hit: AtomicU64::new(0), gs_miss: AtomicU64::new(0), ps: AtomicU64::new(0),
            inner_m: Mutex::new(None), gs_sites: Mutex::new(None),
            gs_miss_sites: Mutex::new(None), ps_sites: Mutex::new(None),
        }
    }
}
static STATS: [Stat; NREG] = [Stat::new(), Stat::new()];

fn gates() -> &'static [bool; NREG] {
    static ON: OnceLock<[bool; NREG]> = OnceLock::new();
    ON.get_or_init(|| {
        let mut g = [false; NREG];
        for (i, e) in ENV.iter().enumerate() {
            g[i] = std::env::var_os(e).is_some();
        }
        g
    })
}
#[inline]
pub fn any_enabled() -> bool {
    static ANY: OnceLock<bool> = OnceLock::new();
    *ANY.get_or_init(|| gates().iter().any(|&b| b))
}
#[inline]
pub fn json_on() -> bool {
    gates()[JSON]
}

thread_local! {
    static DEPTH: [Cell<u32>; NREG] = [const { Cell::new(0) }, const { Cell::new(0) }];
}
#[inline]
fn active(r: usize) -> bool {
    DEPTH.with(|d| d[r].get() > 0)
}

/// Region-body entry matcher — the frame we keep active across its whole (nested) execution.
fn is_region(r: usize, class: &str, name: &str) -> bool {
    match r {
        VOXEL => {
            class == "net/minecraft/world/level/block/Block"
                && (name == "getShapeForEachState"
                    || name.starts_with("lambda$getShapeForEachState$"))
        }
        JSON => {
            (class == "net/minecraft/util/StrictJsonParser" && name == "parse")
                || (class == "com/google/gson/JsonParser" && name == "parseReader")
        }
        _ => false,
    }
}
/// Nested callee classes to record as inner calls (the interning / heavy-lib layer the body drives).
fn track_inner(r: usize, class: &str) -> bool {
    match r {
        VOXEL => class == "net/minecraft/world/phys/shapes/Shapes",
        JSON => {
            class.starts_with("com/google/gson/")
                || class.starts_with("com/mojang/")
                || class == "net/minecraft/util/StrictJsonParser"
        }
        _ => false,
    }
}

fn bump(m: &Mutex<Option<HashMap<String, u64>>>, key: String) {
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(key).or_insert(0) += 1;
}

/// RAII guard: decrements the depth of whatever region it entered, on every return path.
pub struct Guard(Option<usize>);
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(r) = self.0 {
            DEPTH.with(|d| d[r].set(d[r].get().saturating_sub(1)));
        }
    }
}
pub fn inert() -> Guard {
    Guard(None)
}
/// Enter the region body iff (class,name) matches an ENABLED region; records the body-entry and
/// returns a guard that stays active across the frame's whole (interpreted) nested execution.
pub fn maybe_enter(class: &str, name: &str) -> Guard {
    let g = gates();
    for r in 0..NREG {
        if g[r] && is_region(r, class, name) {
            STATS[r].calls.fetch_add(1, Relaxed);
            DEPTH.with(|d| d[r].set(d[r].get() + 1));
            return Guard(Some(r));
        }
    }
    Guard(None)
}

/// A nested invoke — attribute to every active region whose inner-filter matches the callee.
pub fn note_invoke(class: &str, name: &str) {
    for r in 0..NREG {
        if active(r) && track_inner(r, class) {
            STATS[r].inner.fetch_add(1, Relaxed);
            bump(&STATS[r].inner_m, format!("{class}.{name}"));
        }
    }
}
/// A getstatic while a region is active. `miss` = went through `getstatic_resolve` (may run
/// `<clinit>` = the shared-init abort hazard); else a read-only tlcache hit (fork-safe).
pub fn note_getstatic(owner: &str, name: &str, miss: bool) {
    for r in 0..NREG {
        if active(r) {
            let site = format!("{owner}.{name}");
            bump(&STATS[r].gs_sites, site.clone());
            if miss {
                STATS[r].gs_miss.fetch_add(1, Relaxed);
                bump(&STATS[r].gs_miss_sites, site);
            } else {
                STATS[r].gs_hit.fetch_add(1, Relaxed);
            }
        }
    }
}
pub fn note_putstatic(owner: &str, name: &str) {
    for r in 0..NREG {
        if active(r) {
            STATS[r].ps.fetch_add(1, Relaxed);
            bump(&STATS[r].ps_sites, format!("{owner}.{name}"));
        }
    }
}
/// A JSON parse served by the native gson fast-path (gson_native.rs), intercepted at the opcode
/// level BEFORE the guest frame — so it never enters `maybe_enter`. Counted separately to prove
/// the native path handles the files while producing ZERO guest re-entry / static mutation.
pub fn note_json_native_parse() {
    if json_on() {
        STATS[JSON].native.fetch_add(1, Relaxed);
    }
}

fn top(m: &Mutex<Option<HashMap<String, u64>>>, n: usize) -> Vec<(String, u64)> {
    let g = m.lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<(String, u64)> = g
        .as_ref()
        .map(|h| h.iter().map(|(k, c)| (k.clone(), *c)).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}

pub fn dump_all() {
    for r in 0..NREG {
        if gates()[r] {
            dump(r);
        }
    }
}
fn dump(r: usize) {
    let l = LABEL[r];
    let s = &STATS[r];
    let (rc, ic, nat, gh, gm, pw) = (
        s.calls.load(Relaxed),
        s.inner.load(Relaxed),
        s.native.load(Relaxed),
        s.gs_hit.load(Relaxed),
        s.gs_miss.load(Relaxed),
        s.ps.load(Relaxed),
    );
    eprintln!("[{l}] region-body entries (guest frames) = {rc}");
    if r == JSON {
        eprintln!("[{l}]   parses served NATIVELY by gson_native.rs (isolated, no guest frame) = {nat}");
    }
    eprintln!("[{l}]   tracked nested lib invokes inside region = {ic}");
    for (k, c) in top(&s.inner_m, 20) {
        eprintln!("[{l}]     call {k}  {c}");
    }
    eprintln!(
        "[{l}]   getstatic inside region: cache-HIT(read-only/fork-safe)={gh}  RESOLVE-miss(<clinit> hazard)={gm}"
    );
    for (k, c) in top(&s.gs_sites, 20) {
        eprintln!("[{l}]     getstatic {k}  {c}");
    }
    if gm > 0 {
        eprintln!("[{l}]   ^ getstatic MISS sites (the clinit-abort hazard subset):");
        for (k, c) in top(&s.gs_miss_sites, 20) {
            eprintln!("[{l}]       MISS {k}  {c}");
        }
    }
    eprintln!("[{l}]   putstatic inside region (shared-write ABORT hazard) = {pw}");
    for (k, c) in top(&s.ps_sites, 20) {
        eprintln!("[{l}]     putstatic {k}  {c}");
    }
    if pw == 0 && gm == 0 {
        eprintln!(
            "[{l}]   => VERDICT: NO unisolated static writes / clinit-miss getstatic in region — read-only during traversal (fork-favourable)."
        );
    } else {
        eprintln!(
            "[{l}]   => VERDICT: region touches {pw} putstatic + {gm} clinit-miss getstatic — these sites must be worker-isolated before the fork can be deterministic."
        );
    }
}
