//! PILOT-WARM CENSUS (`RAVA_PILOT_CENSUS=1`, default OFF, diagnostic only).
//!
//! Answers two questions about the proposed GILFREE-RINGS "pilot warm" (run the
//! batch's FIRST task serially so its cold metadata lands in the shared caches,
//! then dispatch the rest off-GIL) BEFORE the lever is built, and does it on the
//! ORDINARY SERIAL BOOT — no worker, no off-GIL execution, no soundness risk:
//!
//! 1. **Is cold metadata exhausted by one pilot?** Every shared-cache insert a
//!    task performs is a cache MISS a GIL-free worker would have taken instead:
//!    `class_loader::get_or_load` on a worker aborts the region outright
//!    (class_loader.rs), and `jit_direct_call` is READ-ONLY on a worker, so a
//!    fill/neg insert here marks a site whose worker probe returned `None`.
//!    Per-task deltas of those counters therefore say exactly how much cold
//!    metadata task k needed that tasks 1..k-1 had not already warmed.
//!
//! 2. **What would locked-exec serialize?** `RAVA_M3_WORKER_LOCKED_EXEC` runs the
//!    non-direct `jit_call` tail — the whole general/interpreter callee subtree —
//!    under ONE global re-entrant lock (`jit::resolve_lock_acquire`). The time a
//!    task spends in that tail IS its serialized share, and it is the same code on
//!    main as on a worker. `general_scope` brackets it at depth 0 (nested
//!    `jit_call`s are already inside the outer bracket), so the per-task total is
//!    the Amdahl serial term for the whole lever, measured without running it.
//!
//! Rows print IMMEDIATELY (one stderr line per task): a boot that is stopped at
//! `Done` never reaches heap teardown, so a summary-only report would be lost.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::Instant;

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_PILOT_CENSUS").is_some()
            || std::env::var_os("RAVA_FREEZE_CENSUS").is_some()
    })
}

/// FREEZE-COVERAGE CENSUS (`RAVA_FREEZE_CENSUS=1`, default OFF, implies the task
/// brackets above). Classifies every compiled dispatch MISS by which of the
/// worker's PRE-LOCK arms would have served it — the freeze-resolve table
/// (`RAVA_M3_FREEZE_RESOLVE` must be set so `put` mirrors every verdict) or the
/// frozen direct cache — versus falling through to the ONE global resolve lock.
///
/// Read-only and measured on the ORDINARY SERIAL boot: both probes are pure
/// (`freeze_resolve::get` is an atomic table read, `jit_direct_probe` is `&self`),
/// and main's own dispatch is untouched. Main has warmed both caches by the time a
/// ring task runs, so this is the COVERAGE CEILING of the lock-free arms — the
/// number a pilot-warmed worker could reach at best, without running a worker.
pub(crate) fn freeze_census_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FREEZE_CENSUS").is_some())
}

/// Which pre-lock arm serves a dispatch miss (in the worker's probe order).
#[derive(Copy, Clone)]
pub(crate) enum Arm {
    /// Freeze table holds a positive (compiled) fill → served lock-free.
    Table = 0,
    /// Frozen 16-way direct cache hit → served lock-free.
    Direct = 1,
    /// Freeze table holds −1 (NeverCacheable) and no direct entry → LOCK.
    Neg = 2,
    /// Nothing known for (site, class) → LOCK.
    Unknown = 3,
    /// Receiver has no class key (static / boxed / array) → LOCK.
    NoCk = 4,
}

/// Cold class names kept per task, so the FIRST tasks can be diffed by name and
/// not just by count (the pilot question is "the same classes?", not "how many").
const NAME_CAP: usize = 48;

thread_local! {
    /// Cold `get_or_load` parses (a worker would have aborted the region here).
    static COLD_CLASSES: Cell<u64> = const { Cell::new(0) };
    /// New `(site, receiver-class) -> entry` direct-call cache fills.
    static NEW_FILL: Cell<u64> = const { Cell::new(0) };
    /// New direct-call NEGATIVE entries: sites that resolve to no compiled direct
    /// callee. These are PERMANENT — a pilot cannot warm them, and every later
    /// call at such a site is a worker guard hit.
    static NEW_NEG: Cell<u64> = const { Cell::new(0) };
    /// Methods JIT-compiled (a worker may not compile).
    static NEW_COMPILE: Cell<u64> = const { Cell::new(0) };
    /// Wall nanos inside the non-direct `jit_call` tail, depth-0 bracketed.
    static GEN_NANOS: Cell<u64> = const { Cell::new(0) };
    /// Depth-0 entries into that tail (also the locked-exec lock-acquire count).
    static GEN_CALLS: Cell<u64> = const { Cell::new(0) };
    static GEN_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Depth of armed task scopes (rows are billed at depth 0 only).
    static TASK_DEPTH: Cell<u32> = const { Cell::new(0) };
    static NAMES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Per-CALL-SITE general-path tally for the task currently running:
    /// `(caller class ptr, cp) -> (calls, nanos, symbol)`. Keyed on the raw site
    /// pair so the hot path costs a hash of two words and NO allocation — the
    /// symbol is resolved once, the first time a site is seen. Cleared per task.
    ///
    /// This is what separates "the lock is held on natives a worker fast-path
    /// already serves lock-free" from "the lock is held on general guest work",
    /// which is the difference between a survivable and a fatal serial term.
    static SITES: RefCell<std::collections::HashMap<(usize, u16), (u64, u64, String)>> =
        RefCell::new(std::collections::HashMap::new());
    /// FREEZE-COVERAGE per-site arm tallies for the task currently running:
    /// `(site) -> ([per-Arm counts], symbol)`. Cleared per task, exactly like SITES.
    static ARMS: RefCell<std::collections::HashMap<(usize, u16), ([u64; 5], String)>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Is a bracketed task running on this thread? The coverage probes are only worth
/// paying for inside one (that is the population under study), and the whole-boot
/// miss stream is two orders of magnitude larger.
#[inline]
pub(crate) fn in_task() -> bool {
    TASK_DEPTH.with(Cell::get) > 0
}

/// Record which pre-lock arm would have served one dispatch miss. `sym` is called
/// at most once per site per task.
pub(crate) fn note_arm(site: (usize, u16), arm: Arm, sym: impl FnOnce() -> String) {
    if !in_task() {
        return;
    }
    ARMS.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry(site).or_insert_with(|| ([0u64; 5], sym()));
        e.0[arm as usize] += 1;
    });
}

/// A cold class parse just completed (`class_loader::get_or_load` miss path).
pub(crate) fn note_class_load(name: &str) {
    if !enabled() {
        return;
    }
    COLD_CLASSES.with(|c| c.set(c.get() + 1));
    if TASK_DEPTH.with(Cell::get) > 0 {
        NAMES.with(|n| {
            let mut v = n.borrow_mut();
            if v.len() < NAME_CAP {
                v.push(name.to_string());
            }
        });
    }
}

#[inline]
pub(crate) fn note_direct_fill() {
    if enabled() {
        NEW_FILL.with(|c| c.set(c.get() + 1));
    }
}

#[inline]
pub(crate) fn note_direct_neg() {
    if enabled() {
        NEW_NEG.with(|c| c.set(c.get() + 1));
    }
}

#[inline]
pub(crate) fn note_compile() {
    if enabled() {
        NEW_COMPILE.with(|c| c.set(c.get() + 1));
    }
}

/// Bracket the non-direct `jit_call` tail — the exact span `RAVA_M3_WORKER_
/// LOCKED_EXEC` holds the global resolve lock across. Depth-0 only.
pub(crate) struct GeneralGuard {
    start: Option<Instant>,
    site: (usize, u16),
}

/// `sym` is invoked at most ONCE per call site (the first time it is seen in the
/// current task), so naming the serialized callees costs no per-call allocation.
pub(crate) fn general_scope(site: (usize, u16), sym: impl FnOnce() -> String) -> Option<GeneralGuard> {
    if !enabled() {
        return None;
    }
    let depth = GEN_DEPTH.with(|d| {
        let v = d.get();
        d.set(v + 1);
        v
    });
    GEN_CALLS.with(|c| c.set(c.get() + 1));
    if depth == 0 {
        SITES.with(|m| {
            m.borrow_mut().entry(site).or_insert_with(|| (0, 0, sym()));
        });
    }
    Some(GeneralGuard { start: (depth == 0).then(Instant::now), site })
}

impl Drop for GeneralGuard {
    fn drop(&mut self) {
        GEN_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if let Some(start) = self.start {
            let n = start.elapsed().as_nanos() as u64;
            GEN_NANOS.with(|c| c.set(c.get() + n));
            SITES.with(|m| {
                if let Some(e) = m.borrow_mut().get_mut(&self.site) {
                    e.0 += 1;
                    e.1 += n;
                }
            });
        }
    }
}

/// Per-owner task sequence numbers, so a row says "the 7th task of this impl".
static SEQ: OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> = OnceLock::new();

fn next_seq(owner: &str) -> u64 {
    let map = SEQ.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    let n = m.entry(owner.to_string()).or_insert(0);
    *n += 1;
    *n
}

struct Snapshot {
    classes: u64,
    fill: u64,
    neg: u64,
    compile: u64,
    gen_nanos: u64,
    gen_calls: u64,
    /// Arena collections so far, and the nursery bump pointer. Together these size
    /// the task's ALLOCATION footprint, which the lane must fit in its private
    /// per-task nursery slice with no collection available off-GIL.
    gc_epoch: u64,
    young_top: usize,
    young_cap: usize,
}

#[cfg(feature = "next-gen-gc")]
fn young_now() -> (u64, usize, usize) {
    let (_, top, cap) = crate::nextgen::young_extent();
    (crate::nextgen::gc_epoch(), top, cap)
}

#[cfg(not(feature = "next-gen-gc"))]
fn young_now() -> (u64, usize, usize) {
    (0, 0, 0)
}

fn snapshot() -> Snapshot {
    let (gc_epoch, young_top, young_cap) = young_now();
    Snapshot {
        classes: COLD_CLASSES.with(Cell::get),
        fill: NEW_FILL.with(Cell::get),
        neg: NEW_NEG.with(Cell::get),
        compile: NEW_COMPILE.with(Cell::get),
        gen_nanos: GEN_NANOS.with(Cell::get),
        gen_calls: GEN_CALLS.with(Cell::get),
        gc_epoch,
        young_top,
        young_cap,
    }
}

/// Bracket ONE serial task body (an inline `supplyAsync` supplier, or the lane's
/// serial re-run). Prints the row on drop. Nested tasks are billed to the
/// outermost scope only.
pub(crate) struct TaskGuard {
    owner: String,
    seq: u64,
    start: Instant,
    base: Snapshot,
    outermost: bool,
    /// The general-bracket depth this task was ENTERED at, restored on drop.
    ///
    /// A task body is usually reached THROUGH a non-direct `jit_call` (compiled
    /// guest code → runtime → the `supplyAsync` native → the supplier), so the
    /// depth is already ≥1 when the task starts and every general scope inside it
    /// would be billed as "nested" — the task would report general=0us against a
    /// million general calls. The task is its own top-level unit of work here, so
    /// its bracket depth is rebased to 0 and put back afterwards; the enclosing
    /// guard's own elapsed time still lands in `GEN_NANOS` when it drops, which is
    /// strictly after this row is printed and so cannot contaminate it.
    saved_depth: u32,
}

pub(crate) fn task_scope(owner: String) -> Option<TaskGuard> {
    if !enabled() {
        return None;
    }
    let depth = TASK_DEPTH.with(|d| {
        let v = d.get();
        d.set(v + 1);
        v
    });
    let outermost = depth == 0;
    if outermost {
        NAMES.with(|n| n.borrow_mut().clear());
        SITES.with(|m| m.borrow_mut().clear());
        ARMS.with(|m| m.borrow_mut().clear());
    }
    let saved_depth = GEN_DEPTH.with(|d| {
        let v = d.get();
        if outermost {
            d.set(0);
        }
        v
    });
    Some(TaskGuard {
        seq: if outermost { next_seq(&owner) } else { 0 },
        owner,
        start: Instant::now(),
        base: snapshot(),
        outermost,
        saved_depth,
    })
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        TASK_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if !self.outermost {
            return;
        }
        GEN_DEPTH.with(|d| d.set(self.saved_depth));
        let now = snapshot();
        let dur_us = self.start.elapsed().as_micros() as u64;
        let gen_us = (now.gen_nanos - self.base.gen_nanos) / 1000;
        let pct = if dur_us == 0 { 0.0 } else { gen_us as f64 * 100.0 / dur_us as f64 };
        // Nursery bytes consumed. Each collection in the window recycled a full
        // nursery, so `gcs * cap` is the floor for what a task that cannot collect
        // — every off-GIL lane task — would have to hold privately.
        let gcs = now.gc_epoch - self.base.gc_epoch;
        let alloc_kb = (gcs as usize * now.young_cap
            + now.young_top.saturating_sub(if gcs == 0 { self.base.young_top } else { 0 }))
            / 1024;
        eprintln!(
            "[PILOT] task#{} owner={} dur={}us general={}us ({:.1}%) gen_calls={} cold_classes={} new_fill={} new_neg={} new_compile={} gcs={gcs} alloc_kb={alloc_kb} young_cap_kb={}",
            self.seq,
            self.owner,
            dur_us,
            gen_us,
            pct,
            now.gen_calls - self.base.gen_calls,
            now.classes - self.base.classes,
            now.fill - self.base.fill,
            now.neg - self.base.neg,
            now.compile - self.base.compile,
            now.young_cap / 1024,
        );
        if now.classes > self.base.classes {
            let names = NAMES.with(|n| n.borrow().join(" "));
            eprintln!("[PILOT]   task#{} cold-classes: {names}", self.seq);
        }
        // FREEZE COVERAGE: what fraction of this task's dispatch misses a
        // lock-free arm would serve, and which sites remain locked. Printed for
        // EVERY task (the answer must not rest on one sampled task).
        if freeze_census_on() {
            let rows: Vec<([u64; 5], String)> =
                ARMS.with(|m| m.borrow().values().cloned().collect());
            let mut tot = [0u64; 5];
            for (c, _) in &rows {
                for i in 0..5 {
                    tot[i] += c[i];
                }
            }
            let all: u64 = tot.iter().sum();
            let served = tot[0] + tot[1];
            let pct = if all == 0 { 0.0 } else { served as f64 * 100.0 / all as f64 };
            eprintln!(
                "[FREEZE-COV] task#{} misses={all} served={served} ({:.1}%) table={} direct={} | locked: neg={} unknown={} nock={}",
                self.seq, pct, tot[0], tot[1], tot[2], tot[3], tot[4]
            );
            if matches!(self.seq, 1 | 2 | 64 | 128) {
                let mut v = rows;
                v.sort_by_key(|(c, _)| std::cmp::Reverse(c[2] + c[3] + c[4]));
                for (c, sym) in v.into_iter().take(8) {
                    eprintln!(
                        "[FREEZE-COV]   task#{} site {sym} table={} direct={} neg={} unknown={} nock={}",
                        self.seq, c[0], c[1], c[2], c[3], c[4]
                    );
                }
            }
        }
        // Composition of the serialized span, for a first task and a steady-state
        // one. WHICH callees hold the lock decides whether a lock-free fast path
        // (the existing WS-native arm) could recover any of it.
        if matches!(self.seq, 1 | 2 | 64) {
            let mut rows: Vec<(u64, u64, String)> =
                SITES.with(|m| m.borrow().values().cloned().collect());
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            for (calls, nanos, sym) in rows.into_iter().take(12) {
                eprintln!(
                    "[PILOT]   task#{} locked-site {sym} calls={calls} time={}us",
                    self.seq,
                    nanos / 1000
                );
            }
        }
    }
}
