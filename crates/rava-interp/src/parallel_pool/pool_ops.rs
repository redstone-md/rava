//! Pool operations a caller drives: submit, join, and the reductions over a batch.

use crate::parallel_pool::*;

impl RavaPool {
    /// KIT 3c — the data-parallel SCAN (prefix-op) engine. A scan has a cross-iteration
    /// dependency (`out[i] = op(out[i-1], vals[i])`), so it uses the classic 2-pass parallel
    /// algorithm over FIXED chunks: (1) each worker computes the LOCAL scan of its chunk into
    /// `out[]` + records the chunk total; (2) the chunk totals are exclusive-scanned serially
    /// (tiny); (3) each worker folds its chunk's exclusive carry-in prefix into every out[i].
    /// `vals_base`/`out_base` are raw i64 element bases (resolved under the GIL). GIL-free +
    /// alloc-free (primitive arrays don't move in-region); chunk writes are disjoint → no races.
    pub fn parallel_scan(&self, trip: i64, out_base: i64, vals_base: i64, op: ReduceOp) -> Result<(), RegionAbort> {
        self.parallel_scan_w(trip, out_base, vals_base, op, 8)
    }

    /// Width-parametric scan: `elem_bytes` = 8 (long/double bits) or 4 (int). For int, a cell
    /// is read sign-extended, folded in i64, and stored back as the low 32 bits — which equals
    /// Java's wrapping int arithmetic ((int)(a+b) == ((a+b) mod 2^32)), and the same mod-2^32
    /// identity makes the 2-pass carry-in correct.
    pub fn parallel_scan_w(&self, trip: i64, out_base: i64, vals_base: i64, op: ReduceOp, elem_bytes: i64) -> Result<(), RegionAbort> {
        use std::sync::atomic::AtomicI64;
        if trip <= 0 {
            return Ok(());
        }
        let trip = trip as usize;
        let w4 = elem_bytes == 4;
        let n = self.workers.min(trip).max(1);
        let chunk = (trip + n - 1) / n;
        let n_chunks = (trip + chunk - 1) / chunk;

        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();
        parallel::clear_abort();

        // Raw element base addresses shared across threads (each writes a disjoint chunk).
        struct Bases(usize, usize);
        unsafe impl Sync for Bases {}
        let bases = Bases(vals_base as usize, out_base as usize);
        let bases = &bases;
        // width-typed load/store: sign-extend on read, truncate to the low bits on write.
        let load = move |base: usize, i: usize| -> i64 {
            if w4 { unsafe { *(base as *const i32).add(i) as i64 } } else { unsafe { *(base as *const i64).add(i) } }
        };
        let store = move |base: usize, i: usize, v: i64| {
            if w4 { unsafe { *(base as *mut i32).add(i) = v as i32 } } else { unsafe { *(base as *mut i64).add(i) = v } }
        };
        let totals: Vec<AtomicI64> = (0..n_chunks).map(|_| AtomicI64::new(0)).collect();
        let totals = &totals;

        // PASS 1: each chunk's local scan into out[] + its total.
        std::thread::scope(|scope| {
            for c in 0..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let lo = c * chunk;
                    let hi = (lo + chunk).min(trip);
                    let mut acc = op.identity();
                    for i in lo..hi {
                        acc = op.combine(acc, load(bases.0, i));
                        store(bases.1, i, acc);
                    }
                    totals[c].store(acc, Ordering::Relaxed);
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        // SERIAL: exclusive-scan the (tiny) chunk totals → each chunk's carry-in.
        let mut prefix = vec![op.identity(); n_chunks];
        let mut running = op.identity();
        for c in 0..n_chunks {
            prefix[c] = running;
            running = op.combine(running, totals[c].load(Ordering::Relaxed));
        }
        let prefix = &prefix;

        // PASS 2: fold each chunk's carry-in into its out[i] (chunk 0's carry is identity).
        std::thread::scope(|scope| {
            for c in 1..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let p = prefix[c];
                    let lo = c * chunk;
                    let hi = (lo + chunk).min(trip);
                    for i in lo..hi {
                        store(bases.1, i, op.combine(p, load(bases.1, i)));
                    }
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        if parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(())
        }
    }

    /// KIT 3d — the data-parallel FILTER / stream-compaction engine. `out[k++] = in[i]` for
    /// every `i` where `pred(in[i])` holds, preserving order; returns the surviving count `k`.
    /// Order + compaction across chunks is the cross-chunk dependency, so it's 2-pass over
    /// FIXED chunks: (1) each chunk COUNTS its survivors; (2) the counts are exclusive-scanned
    /// to each chunk's disjoint output base; (3) each chunk re-evaluates the predicate and
    /// SCATTERS survivors into its base..base+count slice. The predicate is evaluated twice
    /// (cheaper than materialising a flag array for large N); output slices are disjoint → no
    /// races. `elem_bytes` = 8 (long) or 4 (int). Errs on abort (→ serial fallback).
    pub fn parallel_filter(&self, trip: i64, out_base: i64, in_base: i64, cmp_code: i64, cst: i64, elem_bytes: i64) -> Result<i64, RegionAbort> {
        use std::sync::atomic::AtomicUsize;
        if trip <= 0 {
            return Ok(0);
        }
        let trip = trip as usize;
        let w4 = elem_bytes == 4;
        // INLINE keep test (loop-invariant match, 100%-predicted) — no per-element fn-call.
        let keep = move |v: i64| -> bool {
            match cmp_code {
                0 => v > cst,
                1 => v >= cst,
                2 => v < cst,
                3 => v <= cst,
                4 => v != cst,
                _ => v == cst,
            }
        };
        let n = self.workers.min(trip).max(1);
        let chunk = (trip + n - 1) / n;
        let n_chunks = (trip + chunk - 1) / chunk;

        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();
        parallel::clear_abort();

        struct Bases(usize, usize);
        unsafe impl Sync for Bases {}
        let bases = Bases(in_base as usize, out_base as usize);
        let bases = &bases;
        let load = move |base: usize, i: usize| -> i64 {
            if w4 { unsafe { *(base as *const i32).add(i) as i64 } } else { unsafe { *(base as *const i64).add(i) } }
        };
        let store = move |base: usize, i: usize, v: i64| {
            if w4 { unsafe { *(base as *mut i32).add(i) = v as i32 } } else { unsafe { *(base as *mut i64).add(i) = v } }
        };
        let counts: Vec<AtomicUsize> = (0..n_chunks).map(|_| AtomicUsize::new(0)).collect();
        let counts = &counts;

        // PASS 1: count survivors per chunk.
        std::thread::scope(|scope| {
            for c in 0..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let lo = c * chunk;
                    let hi = (lo + chunk).min(trip);
                    let mut k = 0usize;
                    for i in lo..hi {
                        if keep(load(bases.0, i)) {
                            k += 1;
                        }
                    }
                    counts[c].store(k, Ordering::Relaxed);
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        // SERIAL: exclusive-scan the counts → each chunk's disjoint output base.
        let mut base_of = vec![0usize; n_chunks];
        let mut running = 0usize;
        for c in 0..n_chunks {
            base_of[c] = running;
            running += counts[c].load(Ordering::Relaxed);
        }
        let total = running;
        let base_of = &base_of;

        // PASS 2: re-evaluate the predicate + scatter survivors into the chunk's base slice.
        std::thread::scope(|scope| {
            for c in 0..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let lo = c * chunk;
                    let hi = (lo + chunk).min(trip);
                    let mut pos = base_of[c];
                    for i in lo..hi {
                        let v = load(bases.0, i);
                        if keep(v) {
                            store(bases.1, pos, v);
                            pos += 1;
                        }
                    }
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        if parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(total as i64)
        }
    }

    /// KIT 3e — the data-parallel COUNT: `#{i : in[i] <cmp> cst}`. Embarrassingly parallel.
    /// The comparison is INLINED (a `cmp_code` + constant, not a fn-ptr) so the hot loop has no
    /// per-element call — the `match` is loop-invariant and 100%-predicted. `cmp_code`: 0 `>`,
    /// 1 `>=`, 2 `<`, 3 `<=`, 4 `!=`, 5 `==`. `elem_bytes` = 8 (long) / 4 (int).
    pub fn parallel_count(&self, trip: i64, in_base: i64, cmp_code: i64, cst: i64, elem_bytes: i64) -> Result<i64, RegionAbort> {
        use std::sync::atomic::AtomicI64;
        if trip <= 0 {
            return Ok(0);
        }
        let trip = trip as usize;
        let w4 = elem_bytes == 4;
        let n = self.workers.min(trip).max(1);
        let chunk = (trip + n - 1) / n;
        let n_chunks = (trip + chunk - 1) / chunk;

        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();
        parallel::clear_abort();

        struct B(usize);
        unsafe impl Sync for B {}
        let base = B(in_base as usize);
        let base = &base;
        let load = move |i: usize| -> i64 {
            if w4 { unsafe { *(base.0 as *const i32).add(i) as i64 } } else { unsafe { *(base.0 as *const i64).add(i) } }
        };
        let total = AtomicI64::new(0);
        let total = &total;

        std::thread::scope(|scope| {
            for c in 0..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let lo = c * chunk;
                    let hi = (lo + chunk).min(trip);
                    let mut k = 0i64;
                    for i in lo..hi {
                        let v = load(i);
                        let keep = match cmp_code {
                            0 => v > cst,
                            1 => v >= cst,
                            2 => v < cst,
                            3 => v <= cst,
                            4 => v != cst,
                            _ => v == cst,
                        };
                        k += keep as i64;
                    }
                    total.fetch_add(k, Ordering::Relaxed);
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        if parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(total.load(Ordering::Relaxed))
        }
    }

    /// KIT 3f — the data-parallel ARGMAX: the index of the maximum element (peak-finding),
    /// resolving ties to the LOWEST index (JVM-style `if (a[i] > max)` first-max). Each fixed
    /// chunk finds its local (max, first-index); the small per-chunk results are combined
    /// serially left-to-right with a strict `>` so an earlier chunk wins ties. Returns -1 for
    /// an empty range. `elem_bytes` = 8 (long) / 4 (int).
    pub fn parallel_argmax(&self, trip: i64, in_base: i64, want_max: bool, elem_bytes: i64) -> Result<i64, RegionAbort> {
        use std::sync::atomic::AtomicI64;
        if trip <= 0 {
            return Ok(-1);
        }
        let trip = trip as usize;
        let w4 = elem_bytes == 4;
        // want_max → index of the max; else index of the min. Ties → lowest index either way.
        let better = move |v: i64, cur: i64| -> bool { if want_max { v > cur } else { v < cur } };
        let n = self.workers.min(trip).max(1);
        let chunk = (trip + n - 1) / n;
        let n_chunks = (trip + chunk - 1) / chunk;

        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();
        parallel::clear_abort();

        struct B(usize);
        unsafe impl Sync for B {}
        let base = B(in_base as usize);
        let base = &base;
        let load = move |i: usize| -> i64 {
            if w4 { unsafe { *(base.0 as *const i32).add(i) as i64 } } else { unsafe { *(base.0 as *const i64).add(i) } }
        };
        // Per-chunk (max value, first index of that max).
        let vals: Vec<AtomicI64> = (0..n_chunks).map(|_| AtomicI64::new(i64::MIN)).collect();
        let idxs: Vec<AtomicI64> = (0..n_chunks).map(|_| AtomicI64::new(-1)).collect();
        let vals = &vals;
        let idxs = &idxs;

        std::thread::scope(|scope| {
            for c in 0..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let lo = c * chunk;
                    let hi = (lo + chunk).min(trip);
                    let mut best_v = load(lo);
                    let mut best_i = lo as i64;
                    for i in (lo + 1)..hi {
                        let v = load(i);
                        if better(v, best_v) {
                            best_v = v;
                            best_i = i as i64;
                        }
                    }
                    vals[c].store(best_v, Ordering::Relaxed);
                    idxs[c].store(best_i, Ordering::Relaxed);
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        // Serial combine — strict `better` keeps the earliest (lowest-index) chunk on ties.
        let mut gv = if want_max { i64::MIN } else { i64::MAX };
        let mut gi = -1i64;
        for c in 0..n_chunks {
            let v = vals[c].load(Ordering::Relaxed);
            if better(v, gv) {
                gv = v;
                gi = idxs[c].load(Ordering::Relaxed);
            }
        }
        if parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok(gi)
        }
    }

    /// KIT 3g — a data-parallel DUAL reduction: one fused pass folding TWO independent
    /// associative accumulators (e.g. `s += a[i]; ss += a[i]*a[i]` for mean/variance in a
    /// single traversal). Each fixed chunk's `body` computes both partials into `out[0..2]`;
    /// the tiny per-chunk pairs combine serially under `op1`/`op2`. Returns `(acc1, acc2)`.
    pub fn parallel_reduce2(&self, trip: i64, op1: ReduceOp, op2: ReduceOp, body: ReduceBody2, env: i64) -> Result<(i64, i64), RegionAbort> {
        use std::sync::atomic::AtomicI64;
        if trip <= 0 {
            return Ok((op1.identity(), op2.identity()));
        }
        let trip = trip as usize;
        let n = self.workers.min(trip).max(1);
        let chunk = (trip + n - 1) / n;
        let n_chunks = (trip + chunk - 1) / chunk;

        gil::acquire();
        nextgen::prewarm_for_parallel();
        let nursery = self.worker_nursery_bytes;
        gil::release();
        parallel::clear_abort();

        let p1: Vec<AtomicI64> = (0..n_chunks).map(|_| AtomicI64::new(op1.identity())).collect();
        let p2: Vec<AtomicI64> = (0..n_chunks).map(|_| AtomicI64::new(op2.identity())).collect();
        let p1 = &p1;
        let p2 = &p2;
        let abort = std::sync::atomic::AtomicBool::new(false);
        let abort = &abort;

        std::thread::scope(|scope| {
            for c in 0..n_chunks {
                scope.spawn(move || {
                    parallel::set_gilfree(true);
                    parallel::enter_region();
                    nextgen::worker_tlab_init(nursery);
                    let lo = (c * chunk) as i64;
                    let hi = ((c * chunk + chunk).min(trip)) as i64;
                    let mut ok: i32 = 1;
                    let mut out = [op1.identity(), op2.identity()];
                    body(lo, hi, &mut ok, env as *const i64, out.as_mut_ptr());
                    if ok == 0 {
                        abort.store(true, Ordering::Relaxed);
                    }
                    p1[c].store(out[0], Ordering::Relaxed);
                    p2[c].store(out[1], Ordering::Relaxed);
                    parallel::leave_region();
                    parallel::set_gilfree(false);
                });
            }
        });

        let mut a1 = op1.identity();
        let mut a2 = op2.identity();
        for c in 0..n_chunks {
            a1 = op1.combine(a1, p1[c].load(Ordering::Relaxed));
            a2 = op2.combine(a2, p2[c].load(Ordering::Relaxed));
        }
        if abort.load(Ordering::Relaxed) || parallel::abort_requested() {
            Err(RegionAbort)
        } else {
            Ok((a1, a2))
        }
    }

    /// KIT 4 (SCAFFOLD, INERT) — fork `initCache` over `task.states`, GIL-free. `run_one(state_addr)`
    /// runs one state's initCache (returns `Err(())` on a worker TLAB overflow → region abort). The
    /// signature is frozen for 9.2c; the body cannot yet be constructed because it needs `Vm` access
    /// (see `ParallelInitCacheTask`). While the gate is OFF this returns `RegionAbort` WITHOUT fanning
    /// out, so the driver stays serial and the run is byte-identical.
    #[allow(dead_code)]
    pub fn fork_init_cache<F>(&self, task: &ParallelInitCacheTask, run_one: F) -> Result<(), RegionAbort>
    where
        F: Fn(i64) -> Result<(), ()> + Sync,
    {
        if !m3_core_ready() {
            return Err(RegionAbort); // inert — driver falls back to the serial GIL path
        }
        // 9.2c integration point: once concurrent Vm access is threaded, this fans `run_one` over the
        // states through the existing work-stealing region. Unreachable today (gate OFF).
        let states = &task.states;
        self.parallel_for(0, states.len() as i64, task.chunk, move |i| run_one(states[i as usize]))
    }
}
