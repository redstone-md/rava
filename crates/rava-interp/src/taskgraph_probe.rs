//! GAP-A01/A05 sizing probe (`RAVA_TASKGRAPH_PROBE`): how much guest work flows
//! through the executor / CompletableFuture task seams, split by routing.
//!
//! Three routings exist today (see `reflect.rs` executor/future natives and
//! `task_runtime.rs`):
//!   * RESIDENT  — enqueued to the host task runtime (proven executor capability),
//!   * DETACHED  — enqueued WITH a proven scalar-kernel plan (may still fall back),
//!   * INLINE    — the legacy serial fallback: the task body runs synchronously
//!                 on the calling thread under the GIL.
//!
//! For INLINE work the probe also accumulates the cumulative wall time of the
//! OUTERMOST inline scope per thread (nested inline tasks — e.g. a supplyAsync
//! body that fires thenApply callbacks — count but do not double-bill time) and
//! a capped per-owner tally (lambda impl-method or task class) of the top
//! inline consumers. Process-wide atomics; inert one-load check when the env
//! flag is unset; printed on `Heap` drop alongside the other probe reports.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::{Value, Vm};

/// Routing counters.
static RESIDENT: AtomicU64 = AtomicU64::new(0);
static DETACHED: AtomicU64 = AtomicU64::new(0);
static DETACHED_FALLBACK: AtomicU64 = AtomicU64::new(0);
/// Cumulative OUTERMOST inline wall time (nanoseconds, summed across threads).
static INLINE_NANOS: AtomicU64 = AtomicU64::new(0);

/// One inline seam per legacy serial execution point (index = `InlineSeam`).
#[derive(Clone, Copy)]
pub(crate) enum InlineSeam {
    ExecutorExecute = 0,
    ExecutorSubmit = 1,
    ExecutorInvokeAll = 2,
    ExecutorSchedule = 3,
    ExecutorScheduleWithResult = 4,
    SupplyAsync = 5,
    RunAsync = 6,
    FutureTaskRun = 7,
    CfCallback = 8,
}

const SEAM_LABELS: [&str; 9] = [
    "executor.execute",
    "executor.submit",
    "executor.invokeAll",
    "executor.schedule",
    "executor.scheduleWithResult",
    "CF.supplyAsync",
    "CF.runAsync",
    "FutureTask.run",
    "CF.callback",
];

static SEAM_COUNTS: [AtomicU64; 9] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Per-owner inline tally: owner -> (count, outermost wall nanos). Capped at
/// `MAX_OWNERS` distinct keys; overflow folds into `<other>`.
const MAX_OWNERS: usize = 64;
static OWNERS: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();

// Nesting depth of armed inline scopes on THIS thread (wall time is billed
// only at depth 0, so nested inline work is not double-counted).
thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// `RAVA_TASKGRAPH_MS=<ms>` — dump the report every N milliseconds instead of only at exit.
///
/// ⚠**A REPORT THAT ONLY PRINTS AT CLEAN EXIT IS UNREADABLE ON THIS BENCHMARK.** A Minecraft server
/// never exits on its own, so every harness here stops it at `Done` — and this probe, the shape
/// census and nearly the flat-stat all produced NOTHING for exactly that reason, each costing a
/// full boot to discover. A periodic dump makes the instrument usable on the only workload the
/// project measures.
pub(crate) fn period_ms() -> u64 {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MS.get_or_init(|| {
        std::env::var("RAVA_TASKGRAPH_MS").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
    })
}

/// Call from a hot-ish path; dumps at most once per `period_ms`.
pub(crate) fn tick() {
    let ms = period_ms();
    if ms == 0 {
        return;
    }
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::{SystemTime, UNIX_EPOCH};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let last = LAST.load(Relaxed);
    if now.saturating_sub(last) < ms {
        return;
    }
    if LAST.compare_exchange(last, now, Relaxed, Relaxed).is_ok() {
        report();
    }
}

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TASKGRAPH_PROBE").is_some())
}

/// A task was accepted by the resident task runtime (`detached` = a proven
/// kernel plan rides along; execution may still fall back to the guest body).
#[inline]
pub(crate) fn count_enqueued(detached: bool) {
    if !enabled() {
        return;
    }
    if detached { &DETACHED } else { &RESIDENT }.fetch_add(1, Relaxed);
}

/// A detached plan failed to materialize on the worker and re-ran as a plain
/// resident guest task.
#[inline]
pub(crate) fn count_detached_fallback() {
    if enabled() {
        DETACHED_FALLBACK.fetch_add(1, Relaxed);
    }
}

/// GILFREE-RINGS stage 3: gilfree-CF lane outcomes (always counted — the lane is
/// itself flag-gated, and the abort-rate is the stage-4 acceptance signal).
pub(crate) static GILFREE_DONE: AtomicU64 = AtomicU64::new(0);
pub(crate) static GILFREE_ABORT: AtomicU64 = AtomicU64::new(0);
#[inline]
pub(crate) fn count_gilfree_done() {
    GILFREE_DONE.fetch_add(1, Relaxed);
}
#[inline]
pub(crate) fn count_gilfree_abort() {
    GILFREE_ABORT.fetch_add(1, Relaxed);
}

/// RAII scope for one INLINE task execution. `None` (zero-cost) when the probe
/// is unarmed. Owner attribution prefers the lambda's shared call-site identity
/// (implementation owner::name) over the (often synthetic) task class name.
pub(crate) struct InlineGuard {
    owner: Option<String>,
    start: Option<Instant>,
}

pub(crate) fn inline_scope(vm: &Vm, seam: InlineSeam, task: &Value) -> Option<InlineGuard> {
    if !enabled() {
        return None;
    }
    SEAM_COUNTS[seam as usize].fetch_add(1, Relaxed);
    let depth = DEPTH.with(|d| {
        let v = d.get();
        d.set(v + 1);
        v
    });
    Some(InlineGuard {
        owner: Some(owner_of(vm, task)),
        start: (depth == 0).then(Instant::now),
    })
}

impl Drop for InlineGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        let nanos = self
            .start
            .map(|s| s.elapsed().as_nanos() as u64)
            .unwrap_or(0);
        if nanos > 0 {
            INLINE_NANOS.fetch_add(nanos, Relaxed);
        }
        if let Some(owner) = self.owner.take() {
            record_owner(owner, nanos);
        }
    }
}

/// Attribution key for a task body: the lambda's shared call-site identity
/// (`owner::name desc`) when it has one, else the task object's class. Shared with
/// `pilot_census` so both probes bill a task to exactly the same owner.
///
/// ⭐**Arena lambdas have no heap handle, so their site lives in the NextGen
/// table, not in `heap.lambda_site_of`.** The previous probe therefore billed
/// every arena continuation as `[arena-lambda:4_4_4]` — its CLASS, not its
/// shared call SITE (the BSM that created it). That is why the whole
/// `inline_wall=4.1s` hill appeared to belong to one anonymous shape.
pub(crate) fn owner_of(vm: &Vm, task: &Value) -> String {
    if let Value::Ref(handle) = task {
        if let Some(site) = vm.heap.lambda_site_of(*handle) {
            return format!("{}::{} {}", site.owner, site.name, site.descriptor);
        }
    }
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = task {
        if crate::nextgen::is_arena_lambda(*p) {
            if let Some(site) = crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(*p)) {
                return format!("{}::{} {}", site.owner, site.name, site.descriptor);
            }
        }
    }
    let class = vm.value_class_name(task);
    if class.is_empty() {
        "<non-object>".to_string()
    } else {
        class
    }
}

/// GILFREE-RINGS stage 1: per-owner offload-admission verdicts for inline CF
/// task bodies — owner -> (admitted, rejected). The full rejection CHAIN prints
/// via `Vm::offload_drill_print` (deduped per root) at the seam; this tally is
/// the summary the periodic report surfaces.
static OFFLOAD: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();

pub(crate) fn note_offload(owner: &str, admitted: bool) {
    let map = OFFLOAD.get_or_init(|| Mutex::new(HashMap::new()));
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    let e = m.entry(owner.to_string()).or_insert((0, 0));
    if admitted {
        e.0 += 1;
    } else {
        e.1 += 1;
    }
}

fn record_owner(owner: String, nanos: u64) {
    let owners = OWNERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = owners.lock().unwrap_or_else(|e| e.into_inner());
    let key = if map.len() >= MAX_OWNERS && !map.contains_key(&owner) {
        "<other>".to_string()
    } else {
        owner
    };
    let entry = map.entry(key).or_insert((0, 0));
    entry.0 += 1;
    entry.1 += nanos;
}

pub(crate) fn report() {
    if !enabled() {
        return;
    }
    let inline_total: u64 = SEAM_COUNTS.iter().map(|c| c.load(Relaxed)).sum();
    eprintln!(
        "[TASKGRAPH] routed: resident={} detached={} (detached_fallback={}) inline={} inline_wall={:.3}s",
        RESIDENT.load(Relaxed),
        DETACHED.load(Relaxed),
        DETACHED_FALLBACK.load(Relaxed),
        inline_total,
        INLINE_NANOS.load(Relaxed) as f64 / 1e9,
    );
    let seams: Vec<String> = SEAM_LABELS
        .iter()
        .zip(SEAM_COUNTS.iter())
        .map(|(label, count)| format!("{label}={}", count.load(Relaxed)))
        .collect();
    eprintln!("[TASKGRAPH] inline seams: {}", seams.join(" "));
    eprintln!(
        "[TASKGRAPH] gilfree lane: done={} abort={}",
        GILFREE_DONE.load(Relaxed),
        GILFREE_ABORT.load(Relaxed)
    );
    if let Some(owners) = OWNERS.get() {
        let map = owners.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &(u64, u64))> = map.iter().collect();
        rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(b.1 .0.cmp(&a.1 .0)));
        eprintln!("[TASKGRAPH] top inline owners (count, outermost wall):");
        for (owner, (count, nanos)) in rows.into_iter().take(20) {
            eprintln!(
                "[TASKGRAPH]   {owner} count={count} wall={:.3}s",
                *nanos as f64 / 1e9
            );
        }
    }
    if let Some(off) = OFFLOAD.get() {
        let m = off.lock().unwrap_or_else(|e| e.into_inner());
        if !m.is_empty() {
            eprintln!("[TASKGRAPH] offload admission (admitted/rejected) per inline owner:");
            let mut rows: Vec<(&String, &(u64, u64))> = m.iter().collect();
            rows.sort_by(|a, b| (b.1 .0 + b.1 .1).cmp(&(a.1 .0 + a.1 .1)));
            for (owner, (adm, rej)) in rows.into_iter().take(20) {
                eprintln!("[TASKGRAPH]   {owner} admitted={adm} rejected={rej}");
            }
        }
    }
}
