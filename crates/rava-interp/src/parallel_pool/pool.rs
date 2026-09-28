//! The worker pool itself.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::parallel_pool::*;

/// A GIL-free work-stealing pool. Cheap to construct; `parallel_for` spawns scoped
/// workers for the duration of one region (no persistent parked threads yet — the
/// skeleton favours a clear lifecycle over a resident pool; a resident variant is a
/// later optimisation once a real workload drives it).
pub struct RavaPool {
    pub(crate) workers: usize,
    /// Fixed nursery size each worker reserves for a region (pre-sized phase).
    pub(crate) worker_nursery_bytes: usize,
}

impl RavaPool {
    /// A pool with `workers` lanes (clamped to ≥1) and the default per-worker nursery.
    pub fn new(workers: usize) -> Self {
        RavaPool { workers: workers.max(1), worker_nursery_bytes: DEFAULT_WORKER_NURSERY }
    }

    /// A pool with an explicit per-worker nursery budget (bytes) — used to force the
    /// overflow → `RegionAbort` path in tests, and to size regions by workload.
    pub fn with_nursery(workers: usize, worker_nursery_bytes: usize) -> Self {
        RavaPool { workers: workers.max(1), worker_nursery_bytes: worker_nursery_bytes.max(64) }
    }

    /// The process-wide pool, sized to `available_parallelism - 1` (leave a core for
    /// the GIL-holding driver). Lazily initialised.
    pub fn global() -> &'static RavaPool {
        static POOL: std::sync::OnceLock<RavaPool> = std::sync::OnceLock::new();
        POOL.get_or_init(|| {
            let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            RavaPool::new(n.saturating_sub(1).max(1))
        })
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Run `body(idx)` for every `idx` in `[start, end)` across the pool, GIL-FREE.
    /// The range is split into `chunk`-sized tasks (work-stealing balances stragglers).
    /// `body` returns `Err(())` to signal its TLAB overflowed — the whole region then
    /// aborts and this returns `Err(RegionAbort)` (the driver retries serially).
    ///
    /// `body: Fn(i64) -> Result<(), ()> + Sync` — must be pure w.r.t. shared state
    /// (disjoint writes into a pre-reserved arena only); the purity gate
    /// (`crate::offload::is_offloadable`) is the compile-time guard that decides
    /// whether a loop is eligible to reach here.
    pub fn parallel_for<F>(
        &self,
        start: i64,
        end: i64,
        chunk: i64,
        body: F,
    ) -> Result<(), RegionAbort>
    where
        F: Fn(i64) -> Result<(), ()> + Sync,
    {
        if start >= end {
            return Ok(());
        }
        let chunk = chunk.max(1);

        // ── Pre-sized nursery phase (under the GIL). Serialised against the main
        //    engine so no worker races to first-initialise shared lazy state, and the
        //    per-worker nursery budget is fixed before any lane fans out. The actual
        //    per-thread TLAB is reserved by each worker on entry (a thread_local can
        //    only be initialised on its own thread) — this phase pins the size + warms
        //    the shared registrations the workers will touch.
        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();

        // Open a fresh region: clear the abort flag, build the work-stealing deques.
        parallel::clear_abort();
        let injector: Injector<Chunk> = Injector::new();
        let mut c = start;
        while c < end {
            let e = (c + chunk).min(end);
            injector.push(Chunk { start: c, end: e });
            c = e;
        }

        let n = self.workers.min(((end - start + chunk - 1) / chunk) as usize).max(1);
        let locals: Vec<Worker<Chunk>> = (0..n).map(|_| Worker::new_fifo()).collect();
        let stealers: Vec<Stealer<Chunk>> = locals.iter().map(|w| w.stealer()).collect();
        let abort = AtomicBool::new(false);

        // Borrow shared state for the scope.
        let injector = &injector;
        let stealers = &stealers;
        let abort = &abort;
        let body = &body;

        std::thread::scope(|scope| {
            for local in locals.into_iter() {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    // Reserve THIS worker's clean, fixed-size isolated nursery.
                    nextgen::worker_tlab_init(nursery);
                    run_worker(local, injector, stealers, abort, body);
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        if abort.load(Ordering::Acquire) || parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(())
        }
    }

    /// KIT 3 — the data-parallel FORK + REDUCE engine. Fold the half-open trip-index
    /// range `[0, trip)` into a single integer accumulator across the pool, GIL-free.
    /// Each worker takes whole chunks, calls `body(lo, hi, &ok)` to fold its sub-range
    /// into a partial, and the partials combine via the associative `op` (so the
    /// result is order-independent — exactly what makes the fork sound). The JIT emits
    /// `body` as a specialised machine-code function matching `ReduceBody`; the tests
    /// drive it with `extern "C"` Rust bodies over the SAME ABI.
    ///
    /// `Err(RegionAbort)` if any chunk set `*ok = 0` (a GIL-free TLAB overflow — the
    /// body already raised `request_abort`); the driver then re-runs the loop serially
    /// under the GIL. `Ok(acc)` is the fully reduced accumulator.
    pub fn parallel_reduce(
        &self,
        trip: i64,
        chunk: i64,
        op: ReduceOp,
        body: ReduceBody,
        env: i64,
    ) -> Result<i64, RegionAbort> {
        if trip <= 0 {
            return Ok(op.identity());
        }
        let chunk = chunk.max(1);

        // Pre-sized nursery phase under the GIL (see `parallel_for`).
        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();

        parallel::clear_abort();
        let injector: Injector<Chunk> = Injector::new();
        let mut c = 0;
        while c < trip {
            let e = (c + chunk).min(trip);
            injector.push(Chunk { start: c, end: e });
            c = e;
        }

        let n = self.workers.min(((trip + chunk - 1) / chunk) as usize).max(1);
        let locals: Vec<Worker<Chunk>> = (0..n).map(|_| Worker::new_fifo()).collect();
        let stealers: Vec<Stealer<Chunk>> = locals.iter().map(|w| w.stealer()).collect();
        let abort = AtomicBool::new(false);
        // The reduced accumulator: each worker folds its partial in once, on exit.
        let acc = Mutex::new(op.identity());

        let injector = &injector;
        let stealers = &stealers;
        let abort = &abort;
        let acc = &acc;

        std::thread::scope(|scope| {
            for local in locals.into_iter() {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);

                    let mut partial = op.identity();
                    while !abort.load(Ordering::Acquire) {
                        let Some(task) = find_task(&local, injector, stealers) else {
                            break;
                        };
                        let mut ok: i32 = 1;
                        let p = body(task.start, task.end, &mut ok as *mut i32, env as *const i64);
                        if ok == 0 {
                            // GIL-free TLAB overflow → tear the region down.
                            abort_region(abort);
                            break;
                        }
                        partial = op.combine(partial, p);
                        parallel::safepoint();
                    }
                    // Fold this worker's partial into the shared accumulator (once).
                    if !abort.load(Ordering::Acquire) {
                        let mut g = acc.lock().unwrap_or_else(|e| e.into_inner());
                        *g = op.combine(*g, partial);
                    }

                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        if abort.load(Ordering::Acquire) || parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(*acc.lock().unwrap_or_else(|e| e.into_inner()))
        }
    }

    /// KIT 3b — the data-parallel FORK + MAP engine. Same partition/steal/region model as
    /// `parallel_reduce`, but the loop has NO accumulator: each worker's `body(lo, hi, &ok,
    /// env)` writes only `out[i]` for `i in [lo, hi)` — provably disjoint (index == counter),
    /// so no combine and no ordering matters. The JIT emits `body` as machine code matching
    /// `MapBody`. `Err(RegionAbort)` on a GIL-free TLAB overflow → the driver re-runs serially.
    pub fn parallel_map(
        &self,
        trip: i64,
        chunk: i64,
        body: MapBody,
        env: i64,
    ) -> Result<(), RegionAbort> {
        if trip <= 0 {
            return Ok(());
        }
        let chunk = chunk.max(1);

        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();

        parallel::clear_abort();
        let injector: Injector<Chunk> = Injector::new();
        let mut c = 0;
        while c < trip {
            let e = (c + chunk).min(trip);
            injector.push(Chunk { start: c, end: e });
            c = e;
        }

        let n = self.workers.min(((trip + chunk - 1) / chunk) as usize).max(1);
        let locals: Vec<Worker<Chunk>> = (0..n).map(|_| Worker::new_fifo()).collect();
        let stealers: Vec<Stealer<Chunk>> = locals.iter().map(|w| w.stealer()).collect();
        let abort = AtomicBool::new(false);

        let injector = &injector;
        let stealers = &stealers;
        let abort = &abort;

        std::thread::scope(|scope| {
            for local in locals.into_iter() {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);

                    while !abort.load(Ordering::Acquire) {
                        let Some(task) = find_task(&local, injector, stealers) else {
                            break;
                        };
                        let mut ok: i32 = 1;
                        body(task.start, task.end, &mut ok as *mut i32, env as *const i64);
                        if ok == 0 {
                            abort_region(abort);
                            break;
                        }
                        parallel::safepoint();
                    }

                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        if abort.load(Ordering::Acquire) || parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(())
        }
    }

}

/// The associative integer reduction a data-parallel loop may fold with. Associativity
/// (+ commutativity) is what lets per-worker partials combine in ANY order and still
/// equal the serial fold — the soundness condition for the fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReduceOp {
    Add,
    Mul,
    Xor,
    And,
    Or,
    /// `Math.max` fold. Identity `i64::MIN` works for BOTH int and long: an int value
    /// sign-extends into `(i64::MIN, i64::MAX)`, so the identity is below every element
    /// (never wins) and `i64::max` on sign-extended i32s is the correct i32 max (the result
    /// is a valid sign-extended element, exact after the final i32 truncation).
    Max,
    /// `Math.min` fold — identity `i64::MAX`, symmetric to `Max`.
    Min,
}

impl ReduceOp {
    /// The identity element each partial accumulator (and the global one) seeds with.
    /// AND's identity is all-1-bits (-1), which for a 32-bit int accumulator is
    /// 0xFFFF_FFFF — `x & -1 == x` at every bit, so int truncation stays exact.
    pub fn identity(self) -> i64 {
        match self {
            ReduceOp::Add | ReduceOp::Xor | ReduceOp::Or => 0,
            ReduceOp::Mul => 1,
            ReduceOp::And => -1,
            ReduceOp::Max => i64::MIN,
            ReduceOp::Min => i64::MAX,
        }
    }
    /// Combine two (partial) accumulators. Wrapping, to match JVM `int`/`long` fold.
    /// Bitwise ops are per-bit exact, so an int partial's low 32 bits fold correctly
    /// regardless of the high sign-extension bits (truncated in the final result).
    #[inline]
    pub fn combine(self, a: i64, b: i64) -> i64 {
        match self {
            ReduceOp::Add => a.wrapping_add(b),
            ReduceOp::Mul => a.wrapping_mul(b),
            ReduceOp::Xor => a ^ b,
            ReduceOp::And => a & b,
            ReduceOp::Or => a | b,
            ReduceOp::Max => a.max(b),
            ReduceOp::Min => a.min(b),
        }
    }
    /// The JVM fold opcode → its `ReduceOp`, or `None` if not associative-reducible.
    /// +/*/^ AND & (iand/land) AND | (ior/lor) are all associative + commutative, so
    /// per-worker partials combine in any order and equal the serial fold. Used by
    /// `analyze_data_parallel_loop`. (Subtraction, division, float/double folds are
    /// NOT associative/commutative and are excluded — the fork would diverge.)
    pub fn from_fold_opcode(op: u8) -> Option<ReduceOp> {
        match op {
            0x60 | 0x61 => Some(ReduceOp::Add), // iadd / ladd
            0x68 | 0x69 => Some(ReduceOp::Mul), // imul / lmul
            0x82 | 0x83 => Some(ReduceOp::Xor), // ixor / lxor
            0x7e | 0x7f => Some(ReduceOp::And), // iand / land
            0x80 | 0x81 => Some(ReduceOp::Or),  // ior / lor
            _ => None,
        }
    }
}

/// One worker's drain loop: pull a chunk (local LIFO → global → steal), run each
/// index through `body`, polling the region safepoint + abort flag between items.
pub(crate) fn run_worker<F>(
    local: Worker<Chunk>,
    injector: &Injector<Chunk>,
    stealers: &[Stealer<Chunk>],
    abort: &AtomicBool,
    body: &F,
) where
    F: Fn(i64) -> Result<(), ()> + Sync,
{
    while !abort.load(Ordering::Acquire) {
        let Some(task) = find_task(&local, injector, stealers) else {
            break; // no work left anywhere
        };
        for idx in task.start..task.end {
            if abort.load(Ordering::Relaxed) {
                return;
            }
            // Park here if the driver runs a stop-the-world (e.g. to scan roots).
            parallel::safepoint();
            if body(idx).is_err() {
                // TLAB overflow (or any unrecoverable per-item failure) → abort the
                // whole region; the driver falls back to a serial GIL run.
                abort_region(abort);
                return;
            }
        }
    }
}
