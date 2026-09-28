//! 23.9 MONOMORPHIC DIRECT-CALL for invokestatic/invokespecial (Lever B, rooted). DEFAULT-ON
//! (kill-switch RAVA_NO_JIT_DIRECTMONO=1). A static/special target is monomorphic (known at resolve,
//! receiver-independent) so no class guard is needed: a per-site single-fptr cache + a machine-code
//! direct call. fptr|0 = GC-leaf (plain call), fptr|1 = self_reg non-leaf (spill → call → reload, the
//! callee roots its own frame). MISS (uncached) → rava_direct_miss (resolves+fills+runs). Captures the
//! ~12.8M non-leaf static/special pool (23.7: 100% self_reg-eligible) from the Rust host into machcode.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::Instant;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_JIT_DIRECTMONO").is_none())
}
pub static FILL: AtomicU64 = AtomicU64::new(0);
pub static FILL_NEG: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn note_fill(negative: bool) {
    if crate::picprobe::enabled() {
        (if negative { &FILL_NEG } else { &FILL }).fetch_add(1, Relaxed);
    }
}

pub fn dump() {
    if enabled() && crate::picprobe::enabled() {
        eprintln!(
            "[DIRECTMONO] static/special direct-call armed: cache fills={} never={}",
            FILL.load(Relaxed),
            FILL_NEG.load(Relaxed)
        );
    }
}

// ═══ RAVA_DIRECTMONO_CENSUS — OBSERVATIONAL ONLY, DEFAULT OFF ═══
//
// Sizes the DYNAMIC population of `rava_direct_miss` — above all the repeats the
// permanently negative (`-1`, native-served) cache state produces, which no existing
// counter sees: the static/special plain-invoke population is counted at EMIT time
// (`picprobe::site_invoke`), and the fill census only tallies the cold fills main
// performs, never the calls that keep arriving through an already-fixed `-1`.
//
// Inert unless `RAVA_DIRECTMONO_CENSUS` is set. The counter lives ONLY in the Rust
// miss callback (never emitted into compiled bodies), behind a process-constant
// `LazyLock` gate; it observes only (invoke kind, cache word, thread context) and
// never writes the cache. The cache word is a plain load of the word the fill block
// already acts on. Reported from `diag::dump` (timer/deadline/panic triggers), which
// fires before the harness kills a boot at `[ServerMain/INFO]: Done`.
//
// The census also TIMES each dynamic callback when it is on: wall duration from after
// census/site-attribution bookkeeping through the `vm.jit_call` return inside
// `rava_direct_miss`, aggregated as relaxed ns per (thread × entry cache word) bucket.
// Every counted callback contributes elapsed time for served, thrown and faulted
// results, so counts and ns stay aligned. This is preemption-inclusive host wall
// time, an upper bound per bucket and never CPU time. Invoke kind is count-only;
// when the gate is off, no timing work runs.
static CENSUS_ON: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("RAVA_DIRECTMONO_CENSUS").is_some());
pub fn census_on() -> bool {
    *CENSUS_ON
}

/// Dynamic callbacks by JVM invoke kind: static (0) / special (2); anything else —
/// which no current emitter produces — lands in CB_OTHER rather than being dropped.
pub static CB_STATIC: AtomicU64 = AtomicU64::new(0);
pub static CB_SPECIAL: AtomicU64 = AtomicU64::new(0);
pub static CB_OTHER: AtomicU64 = AtomicU64::new(0);
/// Thread context of the callback, using `gil::is_worker` to separate background
/// worker threads from main. The independent `parallel::gilfree` flag controls
/// whether this callback may resolve and publish the site cache.
pub static MAIN_CB: AtomicU64 = AtomicU64::new(0);
pub static WORKER_CB: AtomicU64 = AtomicU64::new(0);
/// Entry cache word buckets (-1 permanent negative vs 0/positive), by thread. Any
/// negative word means permanently unservable from machine code. A gil-free worker
/// never performs the cold fill; its nonnegative bucket can include a still-cold
/// site or one already filled by main.
pub static MAIN_NEG: AtomicU64 = AtomicU64::new(0);
pub static MAIN_NONNEG: AtomicU64 = AtomicU64::new(0);
pub static WORKER_NEG: AtomicU64 = AtomicU64::new(0);
pub static WORKER_NONNEG: AtomicU64 = AtomicU64::new(0);
/// Aggregate wall nanoseconds of dynamic callbacks, from after census/site-attribution
/// bookkeeping through the `vm.jit_call` return in `rava_direct_miss` — host time
/// including preemption, an upper bound per bucket, never CPU time. Bucketed
/// exactly like the counts above (cache word × thread), so `ns / count` is the
/// per-call mean for that bucket.
pub static MAIN_NEG_NS: AtomicU64 = AtomicU64::new(0);
pub static MAIN_NONNEG_NS: AtomicU64 = AtomicU64::new(0);
pub static WORKER_NEG_NS: AtomicU64 = AtomicU64::new(0);
pub static WORKER_NONNEG_NS: AtomicU64 = AtomicU64::new(0);
/// Main cold fills whose resolve itself returned a PERMANENT negative (`f < 0`).
/// Distinct-site-shaped, not per-call: the fill arm runs once per site (`== 0`
/// guard), so this approximates how many site descriptors the future fast route
/// would have to allocate for its bounded pool. Separate from the dynamic entry
/// counts/timing above — it sizes a structure, not a population.
pub static FILL_NEG_RES: AtomicU64 = AtomicU64::new(0);
/// Main cold fills by ACTUAL `direct_mono_resolve` outcome: `None` (NotYet — no
/// interpreter target yet, cache stays 0 and the site refills on a later call),
/// `Some(f > 0)` a compiled fptr, `Some(f < 0)` a permanent negative. NONE is
/// dynamic (it can repeat for the same site — the `== 0` guard does not fire when
/// the resolver declines); FPTR/NEG are once-per-cache-site. Never derive distinct
/// sites from the NONE count.
pub static RES_NONE: AtomicU64 = AtomicU64::new(0);
pub static RES_FPTR: AtomicU64 = AtomicU64::new(0);
pub static RES_NEG: AtomicU64 = AtomicU64::new(0);
/// Resolver wall nanoseconds by the same outcome classification, plus a total.
/// Measured around the single `direct_mono_resolve` call in the main fill arm —
/// an upper bound (host wall time), never CPU time.
pub static RES_NS: AtomicU64 = AtomicU64::new(0);
pub static RES_NONE_NS: AtomicU64 = AtomicU64::new(0);
pub static RES_FPTR_NS: AtomicU64 = AtomicU64::new(0);
pub static RES_NEG_NS: AtomicU64 = AtomicU64::new(0);

/// One dynamic direct-mono callback. Pure counter classification; the caller gates on
/// `census_on()` so the disabled arm costs nothing. Test-only reset below.
#[inline]
pub fn note_miss(kind: i64, cache_word: i64, worker: bool) {
    match kind {
        0 => &CB_STATIC,
        2 => &CB_SPECIAL,
        _ => &CB_OTHER,
    }
    .fetch_add(1, Relaxed);
    let (total, neg, nonneg) = if worker {
        (&WORKER_CB, &WORKER_NEG, &WORKER_NONNEG)
    } else {
        (&MAIN_CB, &MAIN_NEG, &MAIN_NONNEG)
    };
    total.fetch_add(1, Relaxed);
    if cache_word < 0 {
        neg.fetch_add(1, Relaxed);
    } else {
        nonneg.fetch_add(1, Relaxed);
    }
}

/// Bucket select for the timing aggregates — same (worker × cache word) split the
/// counts use, as an atomic pair so the hot path stays branch-cheap.
fn ns_bucket(worker: bool, negative: bool) -> &'static AtomicU64 {
    match (worker, negative) {
        (true, true) => &WORKER_NEG_NS,
        (true, false) => &WORKER_NONNEG_NS,
        (false, true) => &MAIN_NEG_NS,
        (false, false) => &MAIN_NONNEG_NS,
    }
}

/// Start timing after counter and optional site-tally bookkeeping. Elapsed is recorded
/// by `note_duration` after `vm.jit_call` returns. Only the gate arm calls this.
#[inline]
pub fn start_miss() -> Instant {
    Instant::now()
}

/// Account one callback's elapsed wall time into the (thread × entry cache word)
/// bucket the count already landed in. Relaxed adds — totals only.
#[inline]
pub fn note_duration(started: Instant, cache_word: i64, worker: bool) {
    ns_bucket(worker, cache_word < 0).fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
}

/// One main cold fill whose `direct_mono_resolve` returned a permanent negative
/// (`f < 0`). Census-gated by the caller — one tick per site, ever.
#[inline]
pub fn note_fill_neg_res() {
    FILL_NEG_RES.fetch_add(1, Relaxed);
}

/// One main cold fill's `direct_mono_resolve` outcome, classified from the SAME
/// `Option<i64>` the fill block already holds (no extra resolve call). `started` is
/// the `Instant` taken immediately before the resolve; the disabled path never calls
/// this, so it costs no timer reads and no counter writes. Test-only reset below.
#[inline]
pub fn note_fill_resolve(started: Instant, res: Option<i64>) {
    let ns = started.elapsed().as_nanos() as u64;
    RES_NS.fetch_add(ns, Relaxed);
    match res {
        None => {
            RES_NONE.fetch_add(1, Relaxed);
            RES_NONE_NS.fetch_add(ns, Relaxed);
        }
        Some(f) if f > 0 => {
            RES_FPTR.fetch_add(1, Relaxed);
            RES_FPTR_NS.fetch_add(ns, Relaxed);
        }
        Some(_) => {
            RES_NEG.fetch_add(1, Relaxed);
            RES_NEG_NS.fetch_add(ns, Relaxed);
        }
    }
}

/// One `[DMMISS-CENSUS]` line for `diag::dump`; empty until the first callback fires,
/// so an armed run with no misses prints nothing.
pub fn report_line() -> String {
    let (s, sp, o) = (
        CB_STATIC.load(Relaxed),
        CB_SPECIAL.load(Relaxed),
        CB_OTHER.load(Relaxed),
    );
    let tot = s + sp + o;
    let (r_none, r_fptr, r_neg) = (
        RES_NONE.load(Relaxed),
        RES_FPTR.load(Relaxed),
        RES_NEG.load(Relaxed),
    );
    // Empty until the first callback fires OR the first main fill outcome is
    // recorded — the fill counters are data on their own, so a run whose misses
    // are all cached-away still prints its fill line. (The timing tests rely on
    // this too.)
    if tot == 0 && FILL_NEG_RES.load(Relaxed) == 0 && r_none + r_fptr + r_neg == 0 {
        return String::new();
    }
    let (mn_n, mw_n, mn_p, mw_p) = (
        MAIN_NEG_NS.load(Relaxed),
        WORKER_NEG_NS.load(Relaxed),
        MAIN_NONNEG_NS.load(Relaxed),
        WORKER_NONNEG_NS.load(Relaxed),
    );
    format!(
        "[DMMISS-CENSUS] dynamic direct-mono misses: static={} special={} other={} (total={}) | \
entry cache -1 (permanent negative): main={} worker={} | \
entry cache 0/+: main={} worker={} | threads: main={} worker={} | \
wall ns (preemption-inclusive, upper bound): main -1={} worker -1={} main 0/+={} worker 0/+={} | \
fills resolved permanent-negative (distinct-site approx): {} | \
main fill resolve outcomes: none(NotYet)={} ns={} fptr={} ns={} neg={} ns={} | \
resolve wall ns total={}",
        s,
        sp,
        o,
        tot,
        MAIN_NEG.load(Relaxed),
        WORKER_NEG.load(Relaxed),
        MAIN_NONNEG.load(Relaxed),
        WORKER_NONNEG.load(Relaxed),
        MAIN_CB.load(Relaxed),
        WORKER_CB.load(Relaxed),
        mn_n,
        mw_n,
        mn_p,
        mw_p,
        FILL_NEG_RES.load(Relaxed),
        r_none,
        RES_NONE_NS.load(Relaxed),
        r_fptr,
        RES_FPTR_NS.load(Relaxed),
        r_neg,
        RES_NEG_NS.load(Relaxed),
        RES_NS.load(Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The census counters are shared process statics and the harness runs tests
    /// in parallel: one lock serialises every reset/assert window so interleaved
    /// resets from a sibling test cannot erase or double-count this test's
    /// traffic. Lock only, never poisoned-meaningful data.
    fn census_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        L.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Zeroes every census counter so assertions see only their own traffic.
    fn reset_for_test() {
        for c in [
            &CB_STATIC,
            &CB_SPECIAL,
            &CB_OTHER,
            &MAIN_CB,
            &WORKER_CB,
            &MAIN_NEG,
            &MAIN_NONNEG,
            &WORKER_NEG,
            &WORKER_NONNEG,
            &MAIN_NEG_NS,
            &MAIN_NONNEG_NS,
            &WORKER_NEG_NS,
            &WORKER_NONNEG_NS,
            &FILL_NEG_RES,
            &RES_NONE,
            &RES_FPTR,
            &RES_NEG,
            &RES_NS,
            &RES_NONE_NS,
            &RES_FPTR_NS,
            &RES_NEG_NS,
        ] {
            c.store(0, Relaxed);
        }
    }

    /// Pure classification and reset of the counter set. Never touches the environment:
    /// `note_miss` is called directly, so the gate (`census_on`, a process-global
    /// `LazyLock`) stays out of the test.
    #[test]
    fn classifies_by_kind_cache_and_thread_then_resets() {
        let _g = census_lock();
        reset_for_test();
        note_miss(0, -1, false); // main, static, permanent negative
        note_miss(2, -1, true); // worker, special, permanent negative
        note_miss(0, 0, false); // main, static, cold
        note_miss(2, 7, true); // worker, special, positive
        note_miss(3, 0, true); // unknown kind must not be dropped
        note_miss(2, -1, true); // the repeat population this census exists for
        assert_eq!(CB_STATIC.load(Relaxed), 2);
        assert_eq!(CB_SPECIAL.load(Relaxed), 3);
        assert_eq!(CB_OTHER.load(Relaxed), 1);
        assert_eq!(MAIN_CB.load(Relaxed), 2);
        assert_eq!(WORKER_CB.load(Relaxed), 4);
        assert_eq!(MAIN_NEG.load(Relaxed), 1);
        assert_eq!(MAIN_NONNEG.load(Relaxed), 1);
        assert_eq!(WORKER_NEG.load(Relaxed), 2);
        assert_eq!(WORKER_NONNEG.load(Relaxed), 2);
        let line = report_line();
        assert!(line.starts_with("[DMMISS-CENSUS]"), "{line}");
        assert!(line.contains("total=6"), "{line}");
        assert!(line.contains("wall ns"), "{line}");
        reset_for_test();
        assert!(report_line().is_empty(), "report must be empty after reset");
    }

    /// Timing aggregation: `start_miss`/`note_duration` land elapsed ns in the same
    /// (thread × cache word) bucket the count did, so counts and ns stay aligned.
    /// No wall-clock bounds and no `elapsed == 0` assumption — elapsed ns is
    /// monotonic, so only `> 0` after real accrual and clean reset are asserted.
    #[test]
    fn durations_land_in_their_count_bucket_and_reset() {
        let _g = census_lock();
        reset_for_test();
        // Duration-only traffic first: the ns buckets must accrue in isolation,
        // proving they are fed by `note_duration` and not by the counts.
        let t = start_miss();
        note_duration(t, -1, false); // main, negative
        let t2 = start_miss();
        note_duration(t2, 7, true); // worker, nonnegative
        assert!(
            MAIN_NEG_NS.load(Relaxed) > 0,
            "main negative ns accrued, {}",
            MAIN_NEG_NS.load(Relaxed)
        );
        assert!(
            WORKER_NONNEG_NS.load(Relaxed) > 0,
            "worker nonnegative ns accrued"
        );
        assert_eq!(
            WORKER_NEG_NS.load(Relaxed),
            0,
            "untouched bucket stays zero"
        );
        assert_eq!(
            MAIN_NONNEG_NS.load(Relaxed),
            0,
            "untouched bucket stays zero"
        );
        // Matching counts, as the live caller always produces them: counts and ns
        // stay interpretable together (ns / count = per-call mean).
        note_miss(0, -1, false);
        note_miss(2, 7, true);
        assert_eq!(MAIN_NEG.load(Relaxed), 1);
        assert_eq!(WORKER_NONNEG.load(Relaxed), 1);
        let line = report_line();
        assert!(line.contains("wall ns"), "{line}");
        assert!(line.contains("permanent-negative"), "{line}");
        reset_for_test();
        assert_eq!(MAIN_NEG_NS.load(Relaxed), 0);
        assert_eq!(WORKER_NONNEG_NS.load(Relaxed), 0);
        assert_eq!(MAIN_NEG.load(Relaxed), 0);
        assert!(report_line().is_empty(), "report must be empty after reset");
    }

    /// The fill-result counter ticks per call, but the fill arm runs once per site
    /// (`== 0` guard), so its total is a distinct-site approximation. Pure count,
    /// no environment touch — same contract as the classification test.
    #[test]
    fn fill_neg_res_counts_and_resets() {
        let _g = census_lock();
        reset_for_test();
        assert_eq!(FILL_NEG_RES.load(Relaxed), 0);
        crate::directmono::note_fill_neg_res();
        crate::directmono::note_fill_neg_res();
        assert_eq!(FILL_NEG_RES.load(Relaxed), 2);
        let line = report_line();
        assert!(line.contains("permanent-negative"), "{line}");
        reset_for_test();
        assert_eq!(FILL_NEG_RES.load(Relaxed), 0);
    }

    /// Main fill-resolve outcome classification: the caller passes the SAME
    /// `Option<i64>` the fill block already holds, plus an `Instant` taken just
    /// before the resolve. NONE repeats (dynamic — a site can resolve to None many
    /// times), FPTR/NEG are once-per-site in production but count freely here.
    /// Pure count + timing, no environment touch.
    #[test]
    fn fill_resolve_outcomes_classify_time_and_reset() {
        let _g = census_lock();
        reset_for_test();
        assert_eq!(RES_NONE.load(Relaxed), 0);
        let t = start_miss();
        note_fill_resolve(t, None);
        let t2 = start_miss();
        note_fill_resolve(t2, Some(1234)); // compiled fptr
        let t3 = start_miss();
        note_fill_resolve(t3, Some(-1)); // permanent negative
        let t4 = start_miss();
        note_fill_resolve(t4, None); // NotYet may repeat for the same site
        assert_eq!(RES_NONE.load(Relaxed), 2);
        assert_eq!(RES_FPTR.load(Relaxed), 1);
        assert_eq!(RES_NEG.load(Relaxed), 1);
        assert!(RES_NONE_NS.load(Relaxed) > 0, "none ns accrued");
        assert!(RES_FPTR_NS.load(Relaxed) > 0, "fptr ns accrued");
        assert!(RES_NEG_NS.load(Relaxed) > 0, "neg ns accrued");
        assert!(RES_NS.load(Relaxed) > 0, "total ns accrued");
        // Per-outcome ns must partition the total exactly.
        assert_eq!(
            RES_NONE_NS.load(Relaxed) + RES_FPTR_NS.load(Relaxed) + RES_NEG_NS.load(Relaxed),
            RES_NS.load(Relaxed)
        );
        let line = report_line();
        assert!(line.contains("none(NotYet)=2"), "{line}");
        assert!(line.contains("fptr=1"), "{line}");
        assert!(line.contains("neg=1"), "{line}");
        assert!(line.contains("resolve wall ns total="), "{line}");
        reset_for_test();
        assert_eq!(RES_NONE.load(Relaxed), 0);
        assert_eq!(RES_NS.load(Relaxed), 0);
        assert!(report_line().is_empty(), "report must be empty after reset");
    }
}
