//! M3 9.2c.1 ENGINE-MUTATION PROBE (RAVA_M3_ENGINE_MUTATION_PROBE). Proves whether, after a SERIAL
//! PRE-WARM (initCache call #1 on the main thread — forces JIT compile, tlcache fill, field-hints),
//! the subsequent initCache calls (#2..29671, the would-be-parallel tasks) still MUTATE shared Vm
//! engine structures. Any growth in jit_cache / jit_direct_cache / cnp+aot caches / heap-handle
//! allocations / loaded-class table between two consecutive initCache calls is a shared-`Vec`/map
//! write = a data-race hazard for a `*const Vm` shared read. If post-warm mutations decay to ZERO,
//! a raw shared-read pointer is safe. Read-only, gated. (Snapshots at each call ENTER and diffs vs
//! the previous enter, attributing the window to the prior call — includes minor inter-call loop
//! work, so it OVER-reports: a zero result is a hard proof of stability.)

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M3_ENGINE_MUTATION_PROBE").is_some())
}

/// Snapshot of the 5 growth-prone shared engine structures.
#[derive(Clone, Copy, Default)]
pub struct EngineSnap {
    pub jit: usize,     // jit_cache (compiled methods)
    pub direct: usize,  // jit_direct_cache (dispatch PIC sites)
    pub cnp: usize,     // cnp_cache + aot_clif_cache
    pub heap: usize,    // heap handle allocs (objects+arrays+maps)
    pub classes: usize, // loaded-class table
}

const N: usize = 5;
const LABELS: [&str; N] = ["jit_cache", "jit_direct_cache", "cnp+aot_cache", "heap_allocs", "classes"];

thread_local! {
    static PREV: Cell<Option<(u64, EngineSnap)>> = const { Cell::new(None) };
}
static CALLS: AtomicU64 = AtomicU64::new(0);
static MEASURED: AtomicU64 = AtomicU64::new(0);
static MUT_CALLS: [AtomicU64; N] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];
static TOTAL_DELTA: [AtomicU64; N] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];
static LAST_MUT_IDX: [AtomicU64; N] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];
static PREWARM_DELTA: [AtomicU64; N] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

/// Called at each initCache ENTER with the current engine snapshot.
pub fn observe(cur: EngineSnap) {
    let idx = CALLS.fetch_add(1, Relaxed) + 1; // 1-based
    let prev = PREV.with(|p| p.replace(Some((idx, cur))));
    if let Some((prev_idx, prev)) = prev {
        let d = [
            cur.jit.saturating_sub(prev.jit),
            cur.direct.saturating_sub(prev.direct),
            cur.cnp.saturating_sub(prev.cnp),
            cur.heap.saturating_sub(prev.heap),
            cur.classes.saturating_sub(prev.classes),
        ];
        if prev_idx == 1 {
            // Window closing call #1 = the SERIAL PRE-WARM. Record its delta separately.
            for i in 0..N {
                PREWARM_DELTA[i].fetch_add(d[i] as u64, Relaxed);
            }
        } else {
            // prev_idx >= 2: a would-be-parallel task. Any growth = a post-warm engine mutation.
            MEASURED.fetch_add(1, Relaxed);
            for i in 0..N {
                if d[i] > 0 {
                    MUT_CALLS[i].fetch_add(1, Relaxed);
                    TOTAL_DELTA[i].fetch_add(d[i] as u64, Relaxed);
                    LAST_MUT_IDX[i].store(prev_idx, Relaxed);
                }
            }
        }
    }
}

pub fn dump() {
    let (calls, measured) = (CALLS.load(Relaxed), MEASURED.load(Relaxed));
    eprintln!(
        "[M3-ENGINE] initCache calls={calls}; pre-warm=call#1; measured post-warm windows (#2..)={measured}"
    );
    let pw: Vec<u64> = (0..N).map(|i| PREWARM_DELTA[i].load(Relaxed)).collect();
    eprintln!("[M3-ENGINE]   pre-warm(call#1) growth: {}", fmt_row(&pw));
    let mut total_mut = 0u64;
    for i in 0..N {
        let mc = MUT_CALLS[i].load(Relaxed);
        total_mut += mc;
        eprintln!(
            "[M3-ENGINE]   {:<16} post-warm: mutated in {} call(s), +{} total, last at call#{}",
            LABELS[i], mc, TOTAL_DELTA[i].load(Relaxed), LAST_MUT_IDX[i].load(Relaxed)
        );
    }
    if total_mut == 0 {
        eprintln!(
            "[M3-ENGINE] VERDICT: 0 engine mutations after pre-warm → Vm engine STABLE → *const Vm shared-read is SAFE for the fork"
        );
    } else {
        eprintln!(
            "[M3-ENGINE] VERDICT: {total_mut} post-warm mutating window(s) → engine NOT yet write-stable; inspect the last-mutating call# (warmup tail vs steady) before arming the raw-ptr bridge"
        );
    }
}

fn fmt_row(v: &[u64]) -> String {
    LABELS.iter().zip(v).map(|(l, n)| format!("{l}=+{n}")).collect::<Vec<_>>().join(" ")
}
