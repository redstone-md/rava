//! Spark-style sampling profiler (RAVA_PROF) — a TIME-weighted view of where the guest
//! spends wall-time, built to answer "where are we WASTING time" at a glance:
//!   * TIME SINKS — a flat table of classes by SELF time (where the CPU actually sits).
//!   * ENGINE     — wall-time split into bytecode execution vs NATIVE Rust (natives /
//!                  callbacks / map+string ops = our own overhead) vs GC pauses.
//!   * call TREE  — (RAVA_PROF_TREE) the full call hierarchy with total%/self%.
//!   * heap       — arena tenured / live RSS / GC-cycles over time.
//!
//! It samples at the interpreter safepoint (every CHECK_INTERVAL ops): the elapsed
//! wall-time since the previous safepoint is charged to the current stack, so a
//! slow-but-few-op region (a native call, a GC pause) is correctly weighted. Because each
//! interval is exactly CHECK_INTERVAL ops, a LOW op-rate (large dt) means the engine was
//! off executing bytecode — in native Rust or GC — which is how ENGINE splits the time.

use std::collections::HashMap;
use std::time::Instant;

type ClassId = u32;

struct Node {
    self_ns: u64,
    total_ns: u64,
    children: HashMap<ClassId, Box<Node>>,
}
impl Node {
    fn new() -> Self {
        Node { self_ns: 0, total_ns: 0, children: HashMap::new() }
    }
}

struct MemPoint {
    ms: u64,
    old_kb: u64,
    rss_mb: u64,
    gc: u64,
}

struct State {
    start: Instant,
    last: Instant,
    root: Node,
    names: Vec<String>,
    ids: HashMap<String, ClassId>,
    /// flat self-time by leaf class (where CPU actually sits).
    self_by_class: HashMap<ClassId, u64>,
    total_ns: u64,
    /// per-interval (dt_ns, gc_happened) — the engine split is computed at dump from a
    /// robust low-percentile baseline (pure-bytecode time), not a fragile live threshold.
    intervals: Vec<(u64, bool)>,
    last_gc: u64,
    samples: u64,
    mem: Vec<MemPoint>,
    last_mem_ms: u64,
}
impl State {
    fn intern(&mut self, name: &str) -> ClassId {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as ClassId;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }
}

static STATE: std::sync::Mutex<Option<State>> = std::sync::Mutex::new(None);

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PROF").is_some())
}

/// RAVA_PROF_TREE — sample the full (collapsed, depth-capped) call stack for a call tree.
pub fn tree_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PROF_TREE").is_some())
}

const DEFAULT_DUMP_EVERY_OPS: u64 = 40_000_000;

fn parse_dump_every_ops(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|&ops| ops >= crate::opcodes::CHECK_INTERVAL as u64)
        .unwrap_or(DEFAULT_DUMP_EVERY_OPS)
}

/// Configure diagnostic profile reports without changing execution policy.
pub fn dump_every_ops() -> u64 {
    static OPS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *OPS.get_or_init(|| parse_dump_every_ops(std::env::var("RAVA_PROF_DUMP_OPS").ok().as_deref()))
}

fn focus_class() -> Option<&'static str> {
    static CLASS: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CLASS
        .get_or_init(|| std::env::var("RAVA_PROF_FOCUS_CLASS").ok())
        .as_deref()
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_dump_interval_requires_at_least_one_checkpoint() {
        assert_eq!(super::parse_dump_every_ops(Some("2000000")), 2_000_000);
        assert_eq!(super::parse_dump_every_ops(Some("99999")), 40_000_000);
        assert_eq!(super::parse_dump_every_ops(Some("bad")), 40_000_000);
        assert_eq!(super::parse_dump_every_ops(None), 40_000_000);
    }
}

// ======================================================================
// COMPILED-AWARE WALL-CLOCK SAMPLER (fills the interp-only blind spot).
//
// The flat sampler above only fires at the interpreter op-loop safepoint, so it
// is BLIND to time spent in COMPILED (JIT) code — which runs on the Rust stack
// and never re-enters the op-loop until it returns. In compiled-heavy phases
// (worldgen: 131M compiled→compiled direct calls) the flat TIME SINKS table goes
// EMPTY and all that wall-time lands in the unattributed "native-Rust" ENGINE
// bucket. This background thread samples a GLOBAL breadcrumb — the class jit_run
// is currently executing — every few ms of WALL time, so compiled code finally
// gets attributed by class. Complementary to the flat sampler (interp side).
// ======================================================================

/// Wall-clock sample period (ms) for the background compiled-code sampler.
const SAMPLE_MS: u64 = 5;

/// The class `jit_run` is currently executing (id+1; 0 = not in compiled code),
/// published cheaply per compiled-call entry and read by the background thread.
static COMPILED_CRUMB: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct Crumb {
    names: Vec<String>,
    ids: HashMap<String, u32>,
    /// wall-sample ticks charged to each class id (parallel to `names`).
    ticks: Vec<u64>,
    /// ticks where the crumb was 0 (interpreter / pure-native, not in compiled code).
    interp_or_native: u64,
    /// WINDOW accumulators (reset each windowed print) — isolate a phase (e.g.
    /// worldgen) from the cumulative aggregate, since the big dump is cumulative
    /// and interp-op-triggered (rare in compiled-heavy phases).
    win_ticks: Vec<u64>,
    win_interp: u64,
}
static CRUMB: std::sync::Mutex<Option<Crumb>> = std::sync::Mutex::new(None);
static SAMPLER_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Spawn the background wall-clock sampler once (idempotent). Only under RAVA_PROF.
fn ensure_sampler() {
    if SAMPLER_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    // Windowed print cadence: every WIN_TICKS wall samples (~15s at 5ms).
    const WIN_TICKS: u64 = 3000;
    std::thread::spawn(|| {
        let mut n: u64 = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(SAMPLE_MS));
            let id = COMPILED_CRUMB.load(std::sync::atomic::Ordering::Relaxed);
            let mut g = CRUMB.lock().unwrap();
            if let Some(c) = g.as_mut() {
                if id == 0 {
                    c.interp_or_native += 1;
                    c.win_interp += 1;
                } else if let Some(t) = c.ticks.get_mut((id - 1) as usize) {
                    *t += 1;
                    if let Some(w) = c.win_ticks.get_mut((id - 1) as usize) {
                        *w += 1;
                    }
                }
                n += 1;
                if n % WIN_TICKS == 0 {
                    dump_window(c);
                }
            }
        }
    });
}

/// Publish the compiled class about to run (returns the prior crumb to restore at
/// exit — nested compiled calls re-home correctly). No-op fast when RAVA_PROF off.
pub fn crumb_enter(class: &str) -> u32 {
    if !enabled() {
        return 0;
    }
    ensure_sampler();
    let id = {
        let mut g = CRUMB.lock().unwrap();
        let c = g.get_or_insert_with(|| Crumb {
            names: Vec::new(),
            ids: HashMap::new(),
            ticks: Vec::new(),
            interp_or_native: 0,
            win_ticks: Vec::new(),
            win_interp: 0,
        });
        match c.ids.get(class) {
            Some(&i) => i,
            None => {
                let i = c.names.len() as u32 + 1; // 1-based; 0 = "not in compiled code"
                c.names.push(class.to_string());
                c.ids.insert(class.to_string(), i);
                c.ticks.push(0);
                c.win_ticks.push(0);
                i
            }
        }
    };
    COMPILED_CRUMB.swap(id, std::sync::atomic::Ordering::Relaxed)
}

/// Restore the prior crumb when a compiled call returns.
pub fn crumb_exit(prev: u32) {
    if !enabled() {
        return;
    }
    COMPILED_CRUMB.store(prev, std::sync::atomic::Ordering::Relaxed);
}

/// Print a WINDOWED compiled top-list (last ~15s of wall samples) and reset the
/// window — so a compiled-heavy phase (worldgen) is isolated from the cumulative
/// aggregate. Called by the background sampler itself, so it fires on WALL time
/// regardless of the interp op-rate (which is near-zero in compiled phases). The
/// caller holds the CRUMB lock.
fn dump_window(c: &mut Crumb) {
    let compiled: u64 = c.win_ticks.iter().sum();
    let total = compiled + c.win_interp;
    if total == 0 {
        return;
    }
    let secs = total * SAMPLE_MS / 1000;
    let mut v: Vec<(usize, u64)> = c.win_ticks.iter().cloned().enumerate().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    let top: Vec<String> = v
        .into_iter()
        .take(8)
        .filter(|(_, t)| *t * 200 >= total) // >=0.5%
        .map(|(i, t)| {
            format!("{:.0}% {}", 100.0 * t as f64 / total as f64, short_name(c.names.get(i).map(|s| s.as_str()).unwrap_or("?")))
        })
        .collect();
    eprintln!(
        "[PROF-WIN {}s] compiled {:.0}% | {}",
        secs,
        100.0 * compiled as f64 / total as f64,
        top.join("  ")
    );
    // reset window
    for w in c.win_ticks.iter_mut() {
        *w = 0;
    }
    c.win_interp = 0;
}

/// Last path segment of a class name (compact windowed line).
fn short_name(c: &str) -> &str {
    c.rsplit('/').next().unwrap_or(c)
}

/// Print the compiled-code wall-clock breakdown (called from `dump`).
fn dump_compiled() {
    let g = CRUMB.lock().unwrap();
    let Some(c) = g.as_ref() else { return };
    let compiled: u64 = c.ticks.iter().sum();
    let total = compiled + c.interp_or_native;
    if total == 0 {
        return;
    }
    eprintln!(
        "[PROF] COMPILED wall-clock ({}ms bg sampler — attributes compiled code the flat sampler can't):",
        SAMPLE_MS
    );
    eprintln!(
        "[PROF]   in-compiled {:.1}%   interp/native {:.1}%   ({} samples)",
        100.0 * compiled as f64 / total as f64,
        100.0 * c.interp_or_native as f64 / total as f64,
        total
    );
    let mut v: Vec<(usize, u64)> = c.ticks.iter().cloned().enumerate().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (i, t) in v.into_iter().take(20) {
        let p = 100.0 * t as f64 / total as f64;
        if p < 0.5 {
            break;
        }
        eprintln!("[PROF]   {:5.1}%  {}", p, c.names.get(i).map(|s| s.as_str()).unwrap_or("?"));
    }
}

/// Record a sample. `stack` = bottom→top class names; `ops` = ops executed in this
/// interval (CHECK_INTERVAL); `mem` = (old_kb, rss_mb, gc_cycles).
pub fn sample(stack: &[&str], ops: u64, mem: (u64, u64, u64)) {
    if !enabled() || stack.is_empty() {
        return;
    }
    let mut b = STATE.lock().unwrap();
    let st = b.get_or_insert_with(|| {
        let now = Instant::now();
        State {
            start: now,
            last: now,
            root: Node::new(),
            names: Vec::new(),
            ids: HashMap::new(),
            self_by_class: HashMap::new(),
            total_ns: 0,
            intervals: Vec::new(),
            last_gc: mem.2,
            samples: 0,
            mem: Vec::new(),
            last_mem_ms: 0,
        }
    });
    let now = Instant::now();
    let dt = now.duration_since(st.last).as_nanos() as u64;
    st.last = now;
    st.total_ns += dt;
    st.samples += 1;

    // Record the interval for the ENGINE split (computed robustly at dump).
    let (old_kb, rss_mb, gc) = mem;
    let _ = ops;
    st.intervals.push((dt, gc > st.last_gc));
    st.last_gc = gc;

    // flat self-time to the leaf class.
    let leaf = st.intern(stack[stack.len() - 1]);
    *st.self_by_class.entry(leaf).or_insert(0) += dt;

    // call tree.
    let ids: Vec<ClassId> = stack.iter().map(|c| st.intern(c)).collect();
    let mut node = &mut st.root as *mut Node;
    unsafe {
        (*node).total_ns += dt;
        for (i, &id) in ids.iter().enumerate() {
            let child = (*node).children.entry(id).or_insert_with(|| Box::new(Node::new()));
            child.total_ns += dt;
            if i == ids.len() - 1 {
                child.self_ns += dt;
            }
            node = child.as_mut() as *mut Node;
        }
    }

    let ms = now.duration_since(st.start).as_millis() as u64;
    if ms >= st.last_mem_ms + 250 {
        st.last_mem_ms = ms;
        st.mem.push(MemPoint { ms, old_kb, rss_mb, gc });
    }
}

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.1}s", ns as f64 / 1e9)
    } else {
        format!("{}ms", ns / 1_000_000)
    }
}

/// Print the report (process exit or periodic heartbeat).
pub fn dump() {
    if !enabled() {
        return;
    }
    let b = STATE.lock().unwrap();
    let Some(st) = b.as_ref() else { return };
    if st.total_ns == 0 {
        return;
    }
    let total = st.total_ns as f64;
    let pct = |ns: u64| 100.0 * ns as f64 / total;

    // ---- ENGINE split — where the wall-time goes at the engine level ----
    // Every interval ran exactly CHECK_INTERVAL bytecode ops; the FASTEST intervals (low
    // percentile) are ~pure bytecode, so p10 of the non-GC interval times is the
    // per-interval bytecode cost. Excess over that baseline is native (non-GC) or GC.
    let mut nongc: Vec<u64> = st.intervals.iter().filter(|(_, g)| !*g).map(|(dt, _)| *dt).collect();
    let gc_sum: u64 = st.intervals.iter().filter(|(_, g)| *g).map(|(dt, _)| *dt).sum();
    let n_gc = st.intervals.iter().filter(|(_, g)| *g).count() as u64;
    nongc.sort_unstable();
    let baseline = if nongc.is_empty() { 0 } else { nongc[nongc.len() / 10] };
    let n_all = st.intervals.len() as u64;
    let n_nongc = nongc.len() as u64;
    let nongc_sum: u64 = nongc.iter().sum();
    let bytecode_ns = (baseline * n_all).min(st.total_ns);
    let native_ns = nongc_sum.saturating_sub(baseline * n_nongc);
    let gc_ns = gc_sum.saturating_sub(baseline * n_gc);

    eprintln!("\n[PROF] ================ where time goes ({} wall) ================", fmt_ns(st.total_ns));
    eprintln!(
        "[PROF] ENGINE:  bytecode {:.1}%   native-Rust {:.1}%   GC {:.1}%",
        pct(bytecode_ns),
        pct(native_ns),
        pct(gc_ns)
    );
    eprintln!(
        "[PROF]          (native = time in natives/callbacks/map+string ops — OUR overhead; GC = {} pauses)",
        st.mem.last().map(|m| m.gc).unwrap_or(0)
    );

    // ---- TIME SINKS — flat self-time by class (where the CPU actually sits) ----
    eprintln!("[PROF] TIME SINKS (self-time — where the CPU actually sits):");
    let mut sinks: Vec<(&ClassId, &u64)> = st.self_by_class.iter().collect();
    sinks.sort_by(|a, b| b.1.cmp(a.1));
    for (id, ns) in sinks.into_iter().take(20) {
        let p = pct(*ns);
        if p < 0.5 {
            break;
        }
        let name = st.names.get(*id as usize).map(|s| s.as_str()).unwrap_or("?");
        eprintln!("[PROF]   {:5.1}%  {}", p, name);
    }
    if let Some(class) = focus_class() {
        let ns = st
            .ids
            .get(class)
            .and_then(|id| st.self_by_class.get(id))
            .copied()
            .unwrap_or(0);
        eprintln!("[PROF] FOCUS {class} {:.2}% ({})", pct(ns), fmt_ns(ns));
    }

    // ---- compiled-code wall-clock breakdown (background sampler) ----
    dump_compiled();

    // ---- call tree ----
    if tree_enabled() {
        eprintln!("[PROF] CALL TREE (total% [self%]):");
        walk(&st.root, &st.names, st.total_ns, 0);
    }

    // ---- heap timeline ----
    if !st.mem.is_empty() {
        eprintln!("[PROF] HEAP (arena tenured / live RSS / GC-cycles):");
        let n = st.mem.len();
        let step = (n / 16).max(1);
        for (i, m) in st.mem.iter().enumerate() {
            if i % step == 0 || i == n - 1 {
                eprintln!(
                    "[PROF]   {:>5}s  old {:>5}MB  rss {:>5}MB  gc {}",
                    m.ms / 1000,
                    m.old_kb / 1024,
                    m.rss_mb,
                    m.gc
                );
            }
        }
        eprintln!(
            "[PROF]   peak rss {}MB | old {}→{}MB | {} GC cycles",
            st.mem.iter().map(|m| m.rss_mb).max().unwrap_or(0),
            st.mem.first().map(|m| m.old_kb / 1024).unwrap_or(0),
            st.mem.last().map(|m| m.old_kb / 1024).unwrap_or(0),
            st.mem.last().map(|m| m.gc).unwrap_or(0),
        );
    }
    eprintln!("[PROF] ===========================================================\n");
}

fn walk(node: &Node, names: &[String], grand_total: u64, depth: usize) {
    let mut kids: Vec<(&ClassId, &Box<Node>)> = node.children.iter().collect();
    kids.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns));
    for (id, child) in kids {
        let pct = 100.0 * child.total_ns as f64 / grand_total as f64;
        if pct < 1.0 {
            continue;
        }
        let self_pct = 100.0 * child.self_ns as f64 / grand_total as f64;
        let name = names.get(*id as usize).map(|s| s.as_str()).unwrap_or("?");
        eprintln!("[PROF] {:indent$}{:5.1}%  {name}  [self {:.1}%]", "", pct, self_pct, indent = depth * 2);
        if depth < 18 {
            walk(child, names, grand_total, depth + 1);
        }
    }
}
