//! LANE PROBE (`RAVA_LANE_PROBE=<ms>`, default OFF, diagnostic only).
//!
//! The GILFREE-RINGS lane's per-task ledger (`gilfree_cf::report`) fires at heap
//! teardown, and a lane task that never returns never reaches it — so the one
//! measurement that decides between "the pilot warms too little" and "the freeze
//! census over-counted" was unreadable: a boot that grinds for 700 s prints
//! nothing at all.
//!
//! This is the WORKER-SIDE counterpart of `pilot_census`, which classified a ring
//! task's dispatch misses on MAIN with both caches warm. Here the same
//! classification is taken from where it actually matters — inside a lane worker,
//! off-GIL — and a watchdog thread prints it on a fixed period, so a task that
//! never completes is still fully attributed:
//!
//! * per-worker compiled-callback counts (the work witness `gilfree_cf::LaneState::cb_n`
//!   keeps per task, lifted to a process-visible slot so another thread can read
//!   forward progress while the task is still running),
//! * per-worker wall nanos WAITING for and HOLDING the one global resolve lock
//!   (`jit::resolve_lock_acquire`) — the Amdahl serial term, measured rather than
//!   inferred from a serial-boot census,
//! * the last dispatch-miss SITE each worker touched, resolved to a symbol by the
//!   watchdog, which names the code a stuck worker is actually grinding.
//!
//! Everything is behind the env gate and every hot-path hook is one `enabled()`
//! bool read plus one relaxed store into a per-worker, cache-line-padded slot, so
//! an unset boot is byte-neutral.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::OnceLock;

/// Lane worker slots. `worker_cap()` defaults to 6; 16 covers any override a
/// diagnostic run would use, and a thread beyond it simply goes unattributed.
pub(crate) const MAX_W: usize = 16;

/// One worker's counters on their own cache line. Six workers hammering adjacent
/// atomics at millions of callbacks per second would be measuring the probe's own
/// false sharing, not the lane.
#[repr(align(64))]
pub(crate) struct WSlot {
    /// Compiled-code callbacks fired by this worker (all kinds).
    pub cb: AtomicU64,
    /// Wall nanos blocked acquiring the global resolve lock.
    pub wait_ns: AtomicU64,
    /// Wall nanos holding it (the serialized span).
    pub hold_ns: AtomicU64,
    /// Outer (non-reentrant) acquisitions.
    pub locks: AtomicU64,
    /// Last dispatch-miss site: caller `LoadedClass` pointer and constant-pool index.
    pub site_ptr: AtomicUsize,
    pub site_cp: AtomicU64,
    /// Last callback kind ordinal (`jit::CB_NAMES`).
    pub last_cb: AtomicU64,
    /// 1 while this slot's thread is inside a lane task body.
    pub live: AtomicU64,
    /// `rava_arena_safepoint` entries — the compiled ALLOC-RETRY loop's poll. On a
    /// lane worker the young region is a private `NURSERY_SLICE` VIEW that
    /// `arena_collect_jit` cannot reclaim, so once the view is full this callback
    /// is re-entered by a loop that (per its own contract) polls no abort flag.
    /// Counting it separately from the `other` callback bucket is what turns
    /// "the worker is spinning" into a named, counted loop.
    pub safepoint: AtomicU64,
}

impl WSlot {
    const fn new() -> Self {
        WSlot {
            cb: AtomicU64::new(0),
            wait_ns: AtomicU64::new(0),
            hold_ns: AtomicU64::new(0),
            locks: AtomicU64::new(0),
            site_ptr: AtomicUsize::new(0),
            site_cp: AtomicU64::new(0),
            last_cb: AtomicU64::new(u64::MAX),
            live: AtomicU64::new(0),
            safepoint: AtomicU64::new(0),
        }
    }
}

pub(crate) static W: [WSlot; MAX_W] = [const { WSlot::new() }; MAX_W];

/// Watchdog period in milliseconds; 0 = probe off.
pub(crate) fn period_ms() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_LANE_PROBE")
            .ok()
            .map(|v| v.trim().parse::<u64>().unwrap_or(5000).max(200))
            .unwrap_or(0)
    })
}

#[inline]
pub(crate) fn enabled() -> bool {
    period_ms() != 0
}

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Sticky per-thread slot index. Lane workers are `thread::scope` threads that
    /// die at the join, so a fresh index per REGION would exhaust the table across
    /// 100+ batches; instead the index is assigned once per thread and reused,
    /// which is also what makes the watchdog's per-worker deltas meaningful.
    static SLOT: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

#[inline]
pub(crate) fn slot() -> usize {
    SLOT.with(std::cell::Cell::get)
}

/// A lane worker is entering a task body. Assigns this thread a slot on first use.
pub(crate) fn enter_worker() {
    if !enabled() {
        return;
    }
    let mut s = slot();
    if s == usize::MAX {
        s = NEXT_SLOT.fetch_add(1, Relaxed);
        if s >= MAX_W {
            s = usize::MAX; // unattributed rather than out of bounds
        }
        SLOT.with(|c| c.set(s));
    }
    if s < MAX_W {
        W[s].live.store(1, Relaxed);
    }
}

pub(crate) fn leave_worker() {
    if !enabled() {
        return;
    }
    let s = slot();
    if s < MAX_W {
        W[s].live.store(0, Relaxed);
    }
}

/// One compiled-code callback of kind `i` fired on a lane worker.
#[inline]
pub(crate) fn note_cb(i: usize) {
    if !enabled() {
        return;
    }
    let s = slot();
    if s < MAX_W {
        W[s].cb.fetch_add(1, Relaxed);
        W[s].last_cb.store(i as u64, Relaxed);
    }
}

/// A compiled dispatch miss just entered `rava_dispatch_miss` on this thread.
/// Recording the SITE (not just a count) is what turns "a worker is busy" into
/// "a worker is grinding THIS call".
#[inline]
pub(crate) fn note_site(caller_ptr: usize, cp: u16) {
    if !enabled() {
        return;
    }
    let s = slot();
    if s < MAX_W {
        W[s].site_ptr.store(caller_ptr, Relaxed);
        W[s].site_cp.store(u64::from(cp), Relaxed);
    }
}

/// An OUTER resolve-lock acquisition completed: `wait_ns` blocked, and the hold is
/// billed separately when the guard drops.
#[inline]
pub(crate) fn note_lock_wait(wait_ns: u64) {
    if !enabled() {
        return;
    }
    let s = slot();
    if s < MAX_W {
        W[s].locks.fetch_add(1, Relaxed);
        W[s].wait_ns.fetch_add(wait_ns, Relaxed);
    }
}

/// The compiled alloc-retry loop polled `rava_arena_safepoint` on this thread.
#[inline]
pub(crate) fn note_safepoint() {
    if !enabled() {
        return;
    }
    let s = slot();
    if s < MAX_W {
        W[s].safepoint.fetch_add(1, Relaxed);
    }
}

#[inline]
pub(crate) fn note_lock_hold(hold_ns: u64) {
    if !enabled() {
        return;
    }
    let s = slot();
    if s < MAX_W {
        W[s].hold_ns.fetch_add(hold_ns, Relaxed);
    }
}

/// Resolve a recorded dispatch site to `owner.name+desc`. Read-only: a
/// `LoadedClass` is kept alive by the loader for the process lifetime, and the
/// constant pool is immutable after parse. Wrapped because a slot sampled mid-store
/// can pair a stale pointer with a fresh cp.
fn site_symbol(ptr: usize, cp: u64) -> String {
    if ptr == 0 {
        return "-".to_string();
    }
    std::panic::catch_unwind(|| {
        let caller = unsafe { &*(ptr as *const crate::class_loader::LoadedClass) };
        caller.file.pool.member_ref(cp as u16).map_or_else(
            || format!("{}.?cp{cp}", caller.file.this_class),
            |m| format!("{}.{}{}", m.owner, m.name, m.descriptor),
        )
    })
    .unwrap_or_else(|_| "?".to_string())
}

static WATCHDOG: OnceLock<()> = OnceLock::new();

/// Start the periodic dump. Idempotent — the lane drains once per batch and the
/// watchdog must survive across batches, not be respawned per region.
pub(crate) fn start_watchdog() {
    if !enabled() {
        return;
    }
    WATCHDOG.get_or_init(|| {
        let period = std::time::Duration::from_millis(period_ms());
        std::thread::Builder::new()
            .name("lane-probe".to_string())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let mut prev = [0u64; MAX_W];
                let mut prev_locks = [0u64; MAX_W];
                loop {
                    std::thread::sleep(period);
                    let secs = t0.elapsed().as_secs_f64();
                    let mut any = false;
                    let mut rows = Vec::new();
                    let mut tot_cb = 0u64;
                    let mut tot_wait = 0u64;
                    let mut tot_hold = 0u64;
                    let mut tot_locks = 0u64;
                    for i in 0..MAX_W {
                        let cb = W[i].cb.load(Relaxed);
                        if cb == 0 {
                            continue;
                        }
                        any = true;
                        let locks = W[i].locks.load(Relaxed);
                        let wait = W[i].wait_ns.load(Relaxed);
                        let hold = W[i].hold_ns.load(Relaxed);
                        let live = W[i].live.load(Relaxed);
                        let kind = W[i].last_cb.load(Relaxed) as usize;
                        let kind = crate::jit::CB_NAMES.get(kind).copied().unwrap_or("-");
                        let sym =
                            site_symbol(W[i].site_ptr.load(Relaxed), W[i].site_cp.load(Relaxed));
                        rows.push(format!(
                            "w{i}[live={live} cb={cb} d_cb={} safepoint={} locks={locks} d_locks={} wait_ms={} hold_ms={} last={kind} site={sym}]",
                            cb - prev[i],
                            W[i].safepoint.load(Relaxed),
                            locks - prev_locks[i],
                            wait / 1_000_000,
                            hold / 1_000_000,
                        ));
                        prev[i] = cb;
                        prev_locks[i] = locks;
                        tot_cb += cb;
                        tot_wait += wait;
                        tot_hold += hold;
                        tot_locks += locks;
                    }
                    if !any {
                        continue;
                    }
                    eprintln!(
                        "[LANE-PROBE] t={secs:.1}s cb={tot_cb} cb/s={:.0} locks={tot_locks} wait_ms={} hold_ms={} | {}",
                        tot_cb as f64 / secs.max(0.001),
                        tot_wait / 1_000_000,
                        tot_hold / 1_000_000,
                        rows.join(" ")
                    );
                    #[cfg(feature = "next-gen-gc")]
                    eprintln!(
                        // `wide_refused` is printed beside `stored` because the two together are
                        // the only way to tell an EMPTY table from a REFUSING one. With hit=0 the
                        // first reading is always "the lookup is broken"; if publication is
                        // declining the ring's hot callees, the lookup was never the question.
                        "[LANE-PROBE]   freeze: table_hit={} direct_hit={} wsnative_hit={} miss={} locked_calls={} dmiss_locks={} stored={} wide_refused={} neg_hit={}",
                        crate::jit::freeze_resolve::WORKER_HIT.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_DIRECT_HIT.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_MISS.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_LOCKED_CALLS.load(Relaxed),
                        crate::gil::worker_miss_count(),
                        crate::jit::freeze_resolve::STORED.load(Relaxed),
                        crate::jit::freeze_resolve::WIDE_REFUSED.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_NEG_HIT.load(Relaxed),
                    );
                    #[cfg(feature = "next-gen-gc")]
                    eprintln!(
                        "[LANE-PROBE]   aborts: shared_field={} array_store={} array_alloc={} array_region_full={} requested={} | cbkinds: {}",
                        crate::gilfree_cf::ABORT_SHARED_FIELD.load(Relaxed),
                        crate::gilfree_cf::ABORT_ARRAY_STORE.load(Relaxed),
                        crate::gilfree_cf::ABORT_ARRAY_ALLOC.load(Relaxed),
                        crate::gilfree_cf::ABORT_ARRAY_REGION_FULL.load(Relaxed),
                        crate::parallel::abort_requested(),
                        cb_kind_line(),
                    );
                    #[cfg(feature = "next-gen-gc")]
                    {
                        let ac = crate::gilfree_cf::abort_census::report();
                        if !ac.is_empty() {
                            eprintln!("{ac}");
                        }
                    }
                    // WHICH frozen shared-VM sites the workers reached (RAVA_FREEZE_TALLY).
                    // Reported HERE and not only at teardown: the teardown report is exactly
                    // what a region that never finishes never reaches - the same trap that
                    // made the lane's own classification unreadable during the grind.
                    #[cfg(feature = "next-gen-gc")]
                    crate::freeze_tally::report();
                    // WHICH callees still take the lock. The composition, not the count:
                    // two estimates of it from the per-site coverage census were wrong by
                    // an order of magnitude, so this is measured at the acquisition itself.
                    // Needs RAVA_FORK_TRACE (that is what populates the tally).
                    #[cfg(feature = "next-gen-gc")]
                    {
                        let top = crate::jit::freeze_resolve::locked_tally_top(12);
                        if !top.is_empty() {
                            eprintln!("[LANE-PROBE]   locked callees: {top}");
                        }
                    }
                    // WHY the direct-call path refused, which is the question the locked
                    // population raises and cannot answer. `jit::callback_stats()` already
                    // carries `dmiss_recv`/`dmiss_mega`/`dmiss_res`, but its only reachable
                    // print is the interpreter heartbeat (`n % 20_000_000` inside
                    // `opcodes::run`) and a mostly-COMPILED boot never gets there — measured:
                    // zero `[CB]` lines in a full boot with `RAVA_HEARTBEAT` set. That is the
                    // same "a report you cannot reach is a fix you cannot verify" trap the
                    // ledger has now hit three times, so the counters are re-emitted HERE,
                    // beside the population they explain, on a timer that a boot does reach.
                    // Needs `RAVA_CALLBACK_STATS` — the increments are gated on it too.
                    {
                        // THE WHOLE table, not just the direct-miss tail. 354 M host callbacks
                        // a boot is the largest single structural fact about this engine (the JVM
                        // makes none), and the per-KIND split is the only thing that says which
                        // one to attack. Slicing from `dmiss_recv=` threw that away.
                        let cbs = crate::jit::callback_stats();
                        if !cbs.is_empty() {
                            eprintln!("[LANE-PROBE]   callbacks: {cbs}");
                        }
                        // ...and WHICH refusal inside `jit_direct_resolve` produced them.
                        // The served-owner census used to print only from the CELL census, which
                        // costs ~90 s of boot to arm — so the one table that names WHICH family
                        // gained native calls was gated behind an instrument that changes the run
                        // it is measuring. It prints from here too now.
                        crate::natives::served_census::report();
                        // Per-bridge time AND count. `bridgestat::dump` is called from the CLI's
                        // clean teardown, which a boot benchmark never reaches — it stops at
                        // `Done` and kills the process — so the one instrument that prices a
                        // native call published nothing. Same trap as the collector's line.
                        crate::natives::bridgestat::dump();
                        {
                            let sc = crate::shape_census::report();
                            if !sc.is_empty() {
                                eprintln!("{sc}");
                            }
                        }
                        #[cfg(feature = "next-gen-gc")]
                        {
                            let lp = crate::vm::pic_fill::lpic_refusal_line();
                            if !lp.is_empty() {
                                eprintln!("{lp}");
                            }
                            let dl = crate::jit::dispatch_table::learn_refusal_line();
                            if !dl.is_empty() {
                                eprintln!("{dl}");
                            }
                        }
                        // The collector's own line prints from `arena_collect_jit` only every 32nd
                        // collection, so a boot with fewer than that publishes NOTHING and reads
                        // like a boot that never collected. It prints from here too now.
                        #[cfg(feature = "next-gen-gc")]
                        if crate::nextgen::gc_stats_enabled() {
                            eprintln!("[LANE-PROBE]   {}", crate::nextgen::gc_stats_line());
                        }
                        {
                            let sr = crate::opcodes::arena_cells::stale_stat_report();
                            if !sr.is_empty() {
                                eprintln!("[LANE-PROBE]   {sr}");
                            }
                        }
                        let r = crate::dresolve::report();
                        if !r.is_empty() {
                            eprintln!("[LANE-PROBE]   dresolve-refusals: {r}");
                        }
                        {
                            use std::sync::atomic::Ordering::Relaxed;
                            let (h, m) = (
                                crate::natives::ARM_HIT.load(Relaxed),
                                crate::natives::ARM_MISS.load(Relaxed),
                            );
                            if h + m > 0 {
                                eprintln!(
                                    "[LANE-PROBE]   last-arm: hit={h} miss={m} ({:.1}%) | cascade fell through unserved={} served_by_tail={}",
                                    h as f64 * 100.0 / (h + m) as f64,
                                    crate::natives::cascade::CASCADE_NONE.load(Relaxed),
                                    crate::natives::cascade::CASCADE_SERVED_TAIL.load(Relaxed),
                                );
                                let c: Vec<usize> =
                                    crate::COLD_LEVEL.iter().map(|a| a.load(Relaxed)).collect();
                                {
                                    let e = crate::EXC_MADE.load(Relaxed);
                                    if e > 0 {
                                        eprintln!("[LANE-PROBE]   exceptions_made: {e}");
                                    }
                                }
                                let fw: Vec<usize> =
                                    crate::FU_WHY.iter().map(|a| a.load(Relaxed)).collect();
                                if fw.iter().sum::<usize>() > 0 {
                                    eprintln!(
                                        "[LANE-PROBE]   fu-why: ref_class_nomatch={} arena_class_nomatch={} other_recv={}",
                                        fw[0], fw[1], fw[2]
                                    );
                                }
                                let g: Vec<usize> =
                                    crate::JIT_GATE.iter().map(|a| a.load(Relaxed)).collect();
                                eprintln!(
                                    "[LANE-PROBE]   cold-levels: >=64:{} >=128:{} >=256:{} >=512:{} | worker_visits={} worker_hot_dropped={} passed_gate={}",
                                    c[0], c[1], c[2], c[3], g[5], g[6], g[7]
                                );
                            }
                        }
                        {
                            {
                                let nw = crate::icmiss::never_why_report();
                                if !nw.is_empty() {
                                    eprintln!("[LANE-PROBE]   never-cacheable why: {nw}");
                                    let nn = crate::icmiss::never_names_report(10);
                                    if !nn.is_empty() {
                                        eprintln!("[LANE-PROBE]   never-cacheable classes: {nn}");
                                    }
                                }
                                let fu = crate::backedge::report_prefix("FU:", 14);
                                if !fu.is_empty() {
                                    eprintln!("[LANE-PROBE]   fu-refused-classes: {fu}");
                                }
                                let be = crate::backedge::report(8);
                                if !be.is_empty() {
                                    eprintln!("[LANE-PROBE]   backedges: {be}");
                                }
                            }
                            let n = crate::COMPILED_TOTAL.load(Relaxed);
                            let t = crate::COMPILE_TRIED.load(Relaxed);
                            let r = crate::COMPILE_REFUSED.load(Relaxed);
                            let g: Vec<usize> =
                                crate::JIT_GATE.iter().map(|a| a.load(Relaxed)).collect();
                            if n + t > 0 {
                                eprintln!(
                                    "[LANE-PROBE]   jit: compiled={n} tried={t} refused={r} | gate_deny={} gate_native_served={} spline_fallback={} | entries={} cold_bumps={}",
                                    g[0], g[1], g[2], g[3], g[4]
                                );
                            }
                        }
                        let c = crate::dresolve::clause_report(12);
                        if !c.is_empty() {
                            eprintln!("[LANE-PROBE]   excl-clauses: {c}");
                        }
                    }
                    // WHICH raiser set the process-wide flag. The four lane tallies
                    // above cover the lane's own barriers; `overflow_raised` is the
                    // nursery-view exhaustion path (`request_abort_overflow`), which
                    // is otherwise indistinguishable from them in the ledger.
                    let (raised, overflow, teardown, teardown_ovf) = crate::parallel::abort_stats();
                    eprintln!(
                        "[LANE-PROBE]   abort_raisers: total={raised} overflow={overflow} teardowns={teardown} teardowns_by_overflow={teardown_ovf}"
                    );
                    // WHY a call was refused a direct entry (RAVA_JIT_PIC_PROBE). The probe's own
                    // dump fires from the CLI's clean exit, which a boot measured to `Done` and
                    // then killed never reaches — so the one readout that distinguishes "not
                    // compiled yet" from "native-served" from "receiver shape" was unavailable
                    // for exactly the phase under investigation. Same fix as the abort tally.
                    crate::picprobe::dump();
                    // WHY the gil-free lane refused a CompletableFuture task. Same
                    // reason picprobe::dump moved here: the rejection census prints
                    // from the CLI's clean exit, which a boot measured to `Done` and
                    // then killed never reaches — so the one readout that names which
                    // admission predicate is blocking a phase was unavailable for
                    // exactly the phase under investigation. The boot's first 33
                    // seconds run at 1.05 cores of 16, and this list is what says
                    // which predicate to widen.
                    crate::gilfree_cf::dump_rejects();
                    // WHERE the dispatch misses actually go (RAVA_IC_VTABLE_FALLBACK). Same
                    // reason picprobe::dump() moved here: icmiss::dump() is wired to the CLI's
                    // clean exit, which a boot measured to Done and then killed never reaches —
                    // so the one readout that separates "known-negative, skip the resolve"
                    // from "resolved to a compiled fptr" was unavailable for exactly the phase
                    // under investigation.
                    if crate::icmiss::on() {
                        crate::icmiss::dump();
                    }
                    // Direct-mapped dispatch table census (RAVA_DISPATCH_TABLE). Sits next to
                    // the PIC hit/miss split on purpose: the split is what motivates the table,
                    // so the two must be readable in one place and from one run.
                    #[cfg(feature = "next-gen-gc")]
                    {
                        let r = crate::jit::dispatch_table::report();
                        if !r.is_empty() {
                            eprintln!("[DISPATCH-TABLE] {r}");
                        }
                    }
                    // Which callees ANCHOR the serialized spans (RAVA_LANE_OUTER).
                    #[cfg(feature = "next-gen-gc")]
                    if crate::jit::outer_attrib::enabled() {
                        let r = crate::jit::outer_attrib::report(15);
                        if !r.is_empty() {
                            eprintln!("[LANE-OUTER]   {r}");
                        }
                    }
                }
            })
            .ok();
    });
}

/// Process-wide callback totals by kind (moves only under `RAVA_CALLBACK_STATS` /
/// `RAVA_JIT_STATS`, the same gate the serial pilot yardstick is printed under, so
/// the two are directly comparable).
fn cb_kind_line() -> String {
    crate::jit::CB_NAMES
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let v = crate::jit::CB_COUNTS[i].load(Relaxed);
            (v != 0).then(|| format!("{n}={v}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}
