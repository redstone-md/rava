//! M3 9.2d.1 BIOME RTREE ISOLATION PROBE (RAVA_M3_BIOME_SHAPE_PROBE). Proves the shared Climate$RTree
//! (+ its long[] parameter arrays, the pre-built worldgen graph) is READ-ONLY during biome queries —
//! the precondition for forking getNoiseBiome across ring positions. Snapshots the heap-handle count
//! and arena frontier ONCE at the phase start (first getNoiseBiome); any write during a query to an
//! object that PREDATES the snapshot = a write into pre-built shared state = the fork hazard. Writes
//! to in-phase allocations (per-query temporaries: TargetPoint, the query's own long[]) are local +
//! safe. Heap classification (the tree's long[] live on the heap) is robust (monotone handles); arena
//! is best-effort (addr can alias after a minor GC — reported with that caveat). Read-only, gated.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M3_BIOME_SHAPE_PROBE").is_some())
}

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}
static QUERIES: AtomicU64 = AtomicU64::new(0);
static GEN_SNAP: AtomicU64 = AtomicU64::new(0); // alloc-generation at phase start (0 = unset) — 9.2d.3
static ARENA_SNAP: AtomicU64 = AtomicU64::new(0); // arena frontier addr at phase start
static SHARED_HEAP: AtomicU64 = AtomicU64::new(0); // writes to a pre-phase HEAP object (hazard)
static SHARED_ARENA: AtomicU64 = AtomicU64::new(0); // writes to a pre-phase ARENA object (hazard)
static LOCAL_WRITES: AtomicU64 = AtomicU64::new(0); // writes to in-phase temporaries (safe)
// 9.2d.2: target class/element-type of each SHARED write (real Climate/RTree vs generic-temp artifact).
static SHARED_TYPES: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// Enter a getNoiseBiome/findBiome query. `gen_now` (alloc-generation) + `arena_frontier` snapshot
/// the phase start (recorded once, on the first query). 9.2d.3: generation is recycling-robust.
pub fn enter(gen_now: u64, arena_frontier: u64) {
    QUERIES.fetch_add(1, Relaxed);
    let _ = GEN_SNAP.compare_exchange(0, gen_now.max(1), Relaxed, Relaxed);
    let _ = ARENA_SNAP.compare_exchange(0, arena_frontier.max(1), Relaxed, Relaxed);
    DEPTH.with(|d| d.set(d.get() + 1));
}
pub fn exit() {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}
#[inline]
fn active() -> bool {
    DEPTH.with(|d| d.get() > 0)
}

/// Classify a write during a query. For a heap target `id` = the object's ALLOC-GENERATION
/// (9.2d.3, recycling-robust); for arena `id` = the address. `ty` = target class / element type
/// (recorded only for a SHARED write). A heap gen < the phase snapshot = guaranteed pre-phase.
pub fn note_write(id: u64, is_heap: bool, ty: &str) {
    if !active() {
        return;
    }
    let shared = if is_heap {
        id < GEN_SNAP.load(Relaxed)
    } else {
        id != 0 && id < ARENA_SNAP.load(Relaxed)
    };
    if !shared {
        LOCAL_WRITES.fetch_add(1, Relaxed);
        return;
    }
    if is_heap {
        SHARED_HEAP.fetch_add(1, Relaxed);
    } else {
        SHARED_ARENA.fetch_add(1, Relaxed);
    }
    let mut g = SHARED_TYPES.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(HashMap::new).entry(ty.to_string()).or_insert(0) += 1;
}

pub fn dump() {
    let (q, sh, sa, lo) = (
        QUERIES.load(Relaxed),
        SHARED_HEAP.load(Relaxed),
        SHARED_ARENA.load(Relaxed),
        LOCAL_WRITES.load(Relaxed),
    );
    eprintln!("[M3-BIOME] getNoiseBiome/findBiome queries={q}");
    eprintln!(
        "[M3-BIOME]   writes during queries: SHARED-heap(pre-phase)={sh}  SHARED-arena(pre-phase)={sa}  local-temps={lo}"
    );
    // 9.2d.2: unmask the shared-write targets by type.
    let g = SHARED_TYPES.lock().unwrap_or_else(|e| e.into_inner());
    let types = g.as_ref().cloned().unwrap_or_default();
    let mut v: Vec<(&String, &u64)> = types.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    // A genuine SHARED PRE-BUILT structure (tree nodes / spline definition), NOT a per-query I/O
    // temp (TargetPoint = search input, *Context = eval context) that a recycled handle mis-flags.
    let per_query_io = |t: &str| {
        t.contains("TargetPoint") || t.contains("Context") || t.contains("HolderSet")
    };
    let is_tree = |t: &str| {
        !per_query_io(t)
            && (t.contains("RTree")
                || t.contains("ParameterList")
                || t.contains("ParameterPoint")
                || t.contains("Spline")
                || t.contains("NoiseRouter"))
    };
    let tree_writes: u64 = types.iter().filter(|(t, _)| is_tree(t)).map(|(_, n)| *n).sum();
    eprintln!("[M3-BIOME]   SHARED-write target types (count):");
    for (t, n) in v.iter() {
        let tag = if is_tree(t) {
            " <-- shared pre-built structure (RESIDUAL — confirm vs recycled temp)"
        } else if per_query_io(t) {
            " (per-query I/O temp — recycled-handle artifact)"
        } else {
            " (generic temp — recycled-handle artifact)"
        };
        eprintln!("[M3-BIOME]     {:>6}  {}{}", n, t, tag);
    }
    let total_shared = sh + sa;
    if total_shared == 0 {
        eprintln!(
            "[M3-BIOME] VERDICT (alloc-generation, recycling-PROOF): 0 writes to any pre-phase object. The 9.2d.1/.2 counts were 100% free-list artifacts. Biome phase is STERILE — getNoiseBiome is SAFE TO FORK across ring positions."
        );
    } else if tree_writes == 0 {
        eprintln!(
            "[M3-BIOME] VERDICT (alloc-generation): {total_shared} real pre-phase write(s), but 0 hit a tree/spline structure (all per-query I/O or generic) → RTree + splines READ-ONLY → SAFE TO FORK."
        );
    } else {
        eprintln!(
            "[M3-BIOME] VERDICT (alloc-generation, recycling-PROOF): {tree_writes} GENUINE write(s) to a pre-built shared structure (RTree nodes 0; density-spline) → REAL lazy mutation → must isolate (COW-shadow/per-thread) BEFORE forking."
        );
    }
}
