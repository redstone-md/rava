//! **THE PRICE OF A POPULATION, NOT ITS SIZE.**
//!
//! Every counter in this registry answers *how many*. Eight times in one session that was the
//! wrong question, and the eighth is why this file exists:
//!
//! > `[ICMISS] total=9057565` — nine and a half million permanent dispatch verdicts re-derived
//! > per call, each one three `String` allocations and a `resolve_method` walk. Priced by hand at
//! > 200-400 ns that is 2-4 seconds of a 35-second boot. **Measured, after building the fix: 10
//! > ns, and -0.28% CPU.**
//!
//! The count was right. The price was a guess, and a guess is what the whole cycle rested on.
//! The tree's own rule already says *convert a population into a share of the boot BEFORE writing
//! code* — it just had no instrument that could do the converting, so the conversion was always
//! an estimate multiplied by a real number, which reads exactly like a measurement.
//!
//! A meter closes that. Wrap a site, get:
//!
//! ```text
//! seam.pic_resolve   9473938 ev   37007 smp   10.4 ns/ev   est 0.099 s = 0.28% of 35.2s wall
//! ```
//!
//! ## Why sampled, and what that costs
//!
//! An `rdtsc` pair is 20-40 cycles. On a 10 ns event that is not an instrument, it is a tax that
//! doubles what it claims to read. So a meter times **one event in `SAMPLE_N`** and extrapolates,
//! which puts the added work on 0.4% of the population and leaves the other 99.6% paying one
//! relaxed increment and one masked compare.
//!
//! The rdtsc pair's own cost is calibrated (min of 1,000 back-to-back reads) and subtracted, so a
//! cheap event does not read as its own measurement overhead — the failure mode that makes naive
//! micro-timing report a floor instead of a price.
//!
//! ## ⭐Why it reports a MEDIAN, and how that was found out
//!
//! The first version reported the mean, and its first reading was **242.8 ns/event, 3.33% of
//! wall** for the very site whose price an independent six-boot CPU A/B had already fixed at
//! **-0.28%**. A ten-fold disagreement, and the A/B was right.
//!
//! `rdtsc` counts **wall** cycles, not this thread's cycles. A region that costs 10 ns of CPU
//! reads microseconds whenever the sample lands on a preemption, and on a box carrying ~29%
//! background load that happens often enough to dominate a mean. So the mean of a sampled
//! duration is not the price of the work — it is the price of the work plus whatever the OS did
//! in the middle of it.
//!
//! ⇒ The meter keeps a log2 histogram of sample durations and reports the **median**, with the
//! mean printed beside it. When the two agree the population is uniform and either will do; when
//! `mean >> p50` the tail is the machine, not the code.
//!
//! ⚠A median is still a median: a genuinely bimodal population — a fast path plus a rare
//! expensive one — has a real tail that this deliberately discounts. Read both columns. The meter
//! answers "is this worth building"; when the shape itself matters, add a `diag_histogram!`.
//!
//! ## ⭐SELF TIME: the fix for the mistake that was made three times in one day
//!
//! A meter times everything inside its guard, so a region that runs guest code reports the whole
//! subtree. That produced three wrong numbers in a single session — `put_array` at 187 ns when
//! the allocation is 22, and the crossing "decision" at 59 ns when the gates cost 8 — and the
//! same shape is what makes `bridgestat`'s self column misleading.
//!
//! Guards now nest. Each one subtracts the time its metered children consumed, so what a meter
//! reports is **self time**: cost in this region and not in any other metered region beneath it.
//! Meter the callee as well as the caller and the caller's number becomes honest automatically,
//! which is the difference between a stopwatch and a profiler.
//!
//! ⚠Two limits remain, and both are about COVERAGE rather than arithmetic:
//! * only METERED children are subtracted. Guest code with no meter under it is still charged to
//!   whoever wraps it — so the way to trust an outer number is to meter what runs inside it.
//! * sampling and nesting interact: when a sampled region is open, its children must time
//!   themselves even if this is not one of their own sampled events, or the subtraction is
//!   incomplete. Those forced timings are used ONLY for subtraction and are never recorded as the
//!   child's own samples, so no meter's sample selection is biased by its parent's.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::Instant;

thread_local! {
    /// Cycles consumed by metered regions that OPENED AND CLOSED inside the currently open one.
    static CHILD: Cell<u64> = const { Cell::new(0) };
    /// How many timing guards are live on this thread. Non-zero means an ancestor is being timed,
    /// so this guard must time itself for its parent's subtraction even if it is not a sample.
    static LIVE: Cell<u32> = const { Cell::new(0) };
}

/// One event in this many is timed. Power of two, so the site tests a remainder the compiler
/// lowers to a mask.
pub(crate) const SAMPLE_N: u64 = 256;

/// Cell layout for a meter entry, shared with `mod.rs`'s renderer.
pub(crate) const C_EVENTS: usize = 0;
pub(crate) const C_SAMPLED: usize = 1;
pub(crate) const C_CYCLES: usize = 2;
/// log2 buckets of sample duration begin here — the median, not the mean, is the price.
pub(crate) const C_BUCKETS: usize = 3;
/// **FOUR BUCKETS PER OCTAVE, NOT ONE.** With plain log2 buckets a p50 can only land on
/// 1.5x2^k, so the first in-situ price map read every population as either 38.5 ns or 153.9 ns
/// with nothing between — a quantisation of 4x that is coarser than most of the differences
/// worth finding. The index is `(exponent << 2) | top two mantissa bits`, which costs one extra
/// shift and mask at record time and brings the step to ~19%.
pub(crate) const N_BUCKETS: usize = 256;

/// Bucket for a duration, and the representative value of a bucket — kept adjacent so the two
/// halves of the encoding can never drift apart.
#[inline(always)]
pub(crate) fn bucket_of(d: u64) -> usize {
    if d < 4 {
        return d as usize;
    }
    let e = 63 - d.leading_zeros() as usize;
    let mant = (d >> (e - 2)) & 3;
    (((e << 2) | mant as usize) + 1).min(N_BUCKETS - 1)
}

fn bucket_value(b: usize) -> f64 {
    if b < 4 {
        return b as f64;
    }
    let b = b - 1;
    let e = b >> 2;
    let mant = (b & 3) as f64;
    // Bucket (e, m) holds [(4+m)/4 * 2^e, (5+m)/4 * 2^e); take its midpoint.
    ((4.0 + mant + 0.5) / 4.0) * (1u64 << e) as f64
}
pub(crate) const N_CELLS: usize = C_BUCKETS + N_BUCKETS;

#[inline(always)]
pub(crate) fn now_cycles() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `_rdtsc` is unconditionally available on x86_64 and only reads a counter.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        process_start().elapsed().as_nanos() as u64
    }
}

/// When the process began, for the "% of wall" column. Initialised from `main`, so a dump's
/// denominator is the run rather than the moment the first meter happened to be registered.
pub fn process_start() -> Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    *T.get_or_init(Instant::now)
}

/// Cycles per nanosecond, and the cost of one `rdtsc` pair — both measured, both lazily, so a run
/// with no meters armed never pays for them. Calibration spins ~2 ms; it happens at DUMP time.
fn calibration() -> (f64, u64) {
    static C: OnceLock<(f64, u64)> = OnceLock::new();
    *C.get_or_init(|| {
        let t0 = Instant::now();
        let c0 = now_cycles();
        while t0.elapsed().as_micros() < 2000 {
            std::hint::spin_loop();
        }
        let cycles = now_cycles().saturating_sub(c0);
        let ns = t0.elapsed().as_nanos().max(1) as f64;
        let per_ns = (cycles as f64 / ns).max(0.001);
        // The pair's own cost: MIN of 1,000 back-to-back reads, not the mean — an interrupt
        // during calibration must not inflate what is subtracted from every sample.
        let mut ovh = u64::MAX;
        for _ in 0..1000 {
            let a = now_cycles();
            let b = now_cycles();
            ovh = ovh.min(b.saturating_sub(a));
        }
        (per_ns, ovh)
    })
}

/// A meter handle. `Copy`, like every other handle here, so it lives behind a `OnceLock`.
///
/// ⭐**THE CELLS ARE AN ARRAY, NOT A SLICE.** Every write below is at an index this file fixes at
/// compile time — `C_EVENTS`, `C_SAMPLED`, `C_CYCLES`, and `C_BUCKETS + bucket_of(..)` which
/// `bucket_of` already clamps to `N_BUCKETS - 1`. Through a `&[AtomicU64]` each of those carried a
/// length load and a bounds check, paid by the ARMED path on every event and on every sample: the
/// instrument taxing exactly the population it was built to price. `&[AtomicU64; N_CELLS]` proves
/// all of them in range statically. The registry keeps `Entry.cells` as a slice, because the
/// renderer walks all three kinds through one field and runs once per dump.
#[derive(Copy, Clone)]
pub struct Meter {
    pub(crate) cells: &'static [AtomicU64; N_CELLS],
    pub(crate) armed: bool,
}

impl Meter {
    /// Begin timing this event. The guard stops the clock when it drops, so a site with early
    /// returns needs no bookkeeping. Costs one increment and one compare when the event is not a
    /// sample, and nothing at all when the meter is not armed.
    ///
    /// ⚠"nothing at all when not armed" is true of the CODE and was false of the CALL, and the
    /// difference is where five rounds of this tree kept finding defects. The disarmed arm here is
    /// already the cheap shape — one branch, and a guard whose `timing: false` lets SROA delete
    /// both it and its `Drop`. What was never cheap was the `OnceLock` the caller went through to
    /// obtain `self`, because `armed` arriving as runtime data means the branch below cannot fold
    /// and the guard must be materialised. `diag::none_armed` is what makes `armed` a literal.
    #[inline(always)]
    pub fn start(&self) -> MeterGuard {
        if !self.armed {
            return MeterGuard { cells: None, t0: 0, parent_child: 0, timing: false };
        }
        let n = self.cells[C_EVENTS].fetch_add(1, Relaxed);
        let sampled = n % SAMPLE_N == 0;
        // Time this region if it is our own sample OR an ancestor is timing and needs to subtract
        // us. In the second case we time but do not RECORD, so our sample selection stays ours.
        if !sampled && LIVE.with(|l| l.get()) == 0 {
            return MeterGuard { cells: None, t0: 0, parent_child: 0, timing: false };
        }
        let parent_child = CHILD.with(|c| c.replace(0));
        LIVE.with(|l| l.set(l.get() + 1));
        MeterGuard {
            cells: if sampled { Some(self.cells) } else { None },
            t0: now_cycles(),
            parent_child,
            timing: true,
        }
    }

    /// Record an event whose duration is already known in cycles — for a site whose end is in a
    /// different function and so cannot hold a guard.
    #[inline]
    pub fn record_cycles(&self, cycles: u64) {
        if !self.armed {
            return;
        }
        self.cells[C_EVENTS].fetch_add(1, Relaxed);
        self.cells[C_SAMPLED].fetch_add(1, Relaxed);
        self.cells[C_CYCLES].fetch_add(cycles, Relaxed);
        self.cells[C_BUCKETS + bucket_of(cycles)].fetch_add(1, Relaxed);
    }

    #[inline]
    pub fn armed(&self) -> bool {
        self.armed
    }
}

/// Stops the clock on drop. Bind it to a named `_t`, never to a bare `_`: `let _ = m.start()`
/// drops the guard immediately and times nothing, which is the one way to use this wrong.
///
/// The `cells` field is an ARRAY reference, so `Option` is one niche-optimised word rather than a
/// fat pointer's two: the guard a site must keep live across its whole region is 32 bytes instead
/// of 40. It matters only when the guard survives SROA — a disarmed meter's guard is deleted
/// outright, and with `diag::none_armed` that is now every site in a product boot.
pub struct MeterGuard {
    cells: Option<&'static [AtomicU64; N_CELLS]>,
    t0: u64,
    /// The enclosing region's child-accumulator, saved so this guard can restore it and add its
    /// own elapsed time to it.
    parent_child: u64,
    timing: bool,
}

impl Drop for MeterGuard {
    #[inline(always)]
    fn drop(&mut self) {
        if !self.timing {
            return;
        }
        // ⚠`calibration()` here is a `OnceLock` acquire load on every TIMED drop — and under a
        // sampled parent every nested guard times itself, so this is not the 1-in-256 path it
        // looks like. LEFT ALONE DELIBERATELY: it is reached only when a meter is armed, which no
        // product boot does, and it sits beside an `rdtsc` pair that costs 20-40 cycles by itself,
        // so caching it would move a number that the pair's own noise already dominates. The
        // instrument's self-tax that DID matter was the per-event bounds check on `cells`, and
        // that is gone — see `Meter`.
        let elapsed = now_cycles().saturating_sub(self.t0).saturating_sub(calibration().1);
        // Whatever metered regions ran inside us is not OUR cost.
        let child = CHILD.with(|c| c.get());
        let self_cycles = elapsed.saturating_sub(child);
        // Restore the parent's accumulator and charge it our whole elapsed span: from its point
        // of view we are one child, and its own subtraction removes all of us at once.
        CHILD.with(|c| c.set(self.parent_child.saturating_add(elapsed)));
        LIVE.with(|l| l.set(l.get().saturating_sub(1)));
        if let Some(cells) = self.cells {
            cells[C_SAMPLED].fetch_add(1, Relaxed);
            cells[C_CYCLES].fetch_add(self_cycles, Relaxed);
            cells[C_BUCKETS + bucket_of(self_cycles)].fetch_add(1, Relaxed);
        }
    }
}

/// The rendered price of one meter: events, samples, ns per event, estimated total, and the share
/// of wall that total represents — which is the number the "convert before you build" rule wants.
/// Median sample duration in cycles, read off the buckets.
fn median_cycles(cells: &[AtomicU64], sampled: u64) -> f64 {
    let half = sampled / 2;
    let mut seen = 0u64;
    for b in 0..N_BUCKETS {
        seen += cells[C_BUCKETS + b].load(Relaxed);
        if seen > half {
            return bucket_value(b);
        }
    }
    0.0
}

/// Per-window view for `RAVA_DIAG_DELTA=1`. The buckets are cumulative and cannot be subtracted,
/// so the window's `p50` is the run's `p50` — what the delta genuinely adds is **where in the run
/// the population happens**, which is the question a flat profile most needs answered.
///
/// ⭐The first series this printed settled a 10x disagreement between two instruments. Cumulative
/// events read 651,157 at 36 s and 5,587,944 at 45 s on a boot that finishes near 33 — **86% of
/// the population was server ticking, not booting.** A count taken over a capped window and a
/// wall A/B taken to `Done` are not measuring the same program, and nothing but a time series
/// says so.
pub(crate) fn render_delta(cells: &[AtomicU64], prev_ev: u64, prev_cy: u64, prev_smp: u64) -> String {
    let events = cells[C_EVENTS].load(Relaxed).saturating_sub(prev_ev);
    let sampled = cells[C_SAMPLED].load(Relaxed).saturating_sub(prev_smp);
    let cycles = cells[C_CYCLES].load(Relaxed).saturating_sub(prev_cy);
    let (per_ns, _) = calibration();
    let p50_ns = median_cycles(cells, cells[C_SAMPLED].load(Relaxed).max(1));
    let mean_ns = if sampled == 0 { 0.0 } else { (cycles as f64 / sampled as f64) / per_ns };
    let est_s = (p50_ns / per_ns) * events as f64 / 1e9;
    format!(
        "{events:>12} ev {sampled:>8} smp  p50 {:>7.1} ns* mean {mean_ns:>8.1} ns  est {est_s:>7.3}s this window",
        p50_ns / per_ns
    )
}

/// Mean of every sample BELOW the 95th percentile.
///
/// ⭐**The estimator the total actually needs, and neither of the first two was it.** The median
/// answers "what does a typical event cost" and is immune to the preemption spikes that poison a
/// mean on a loaded box — but a population whose tail is REAL work is then understated, and the
/// first in-situ price map showed exactly that shape: array births read p50 28.8 ns against a
/// mean of 932, and an allocation that triggers a collection genuinely costs microseconds.
///
/// Trimming at p95 keeps the ordinary tail and drops the outliers, so `count x trimmed` is a
/// total that neither the median nor the mean can give. All three are printed, because their
/// disagreement is itself the diagnosis: `p50 ~ trimmed ~ mean` is a uniform population,
/// `mean >> trimmed` is the machine, `trimmed >> p50` is a real tail worth splitting out.
fn trimmed_mean_cycles(cells: &[AtomicU64], sampled: u64) -> f64 {
    let cut = (sampled as f64 * 0.95) as u64;
    let (mut seen, mut n, mut acc) = (0u64, 0u64, 0f64);
    for b in 0..N_BUCKETS {
        let c = cells[C_BUCKETS + b].load(Relaxed);
        if c == 0 {
            continue;
        }
        if seen >= cut {
            break;
        }
        let take = c.min(cut - seen);
        acc += bucket_value(b) * take as f64;
        n += take;
        seen += c;
    }
    if n == 0 {
        0.0
    } else {
        acc / n as f64
    }
}

pub(crate) fn render_row(cells: &[AtomicU64]) -> String {
    let events = cells[C_EVENTS].load(Relaxed);
    let sampled = cells[C_SAMPLED].load(Relaxed);
    let cycles = cells[C_CYCLES].load(Relaxed);
    if sampled == 0 {
        return format!("{events:>12} ev        (no samples yet)");
    }
    let (per_ns, _) = calibration();
    let mean_ns = (cycles as f64 / sampled as f64) / per_ns;
    let p50_ns = median_cycles(cells, sampled) / per_ns;
    let trim_ns = trimmed_mean_cycles(cells, sampled) / per_ns;
    let est_s = trim_ns * events as f64 / 1e9;
    let wall_s = process_start().elapsed().as_secs_f64().max(1e-9);
    format!(
        "{events:>12} ev {sampled:>8} smp  p50 {p50_ns:>7.1}  trim {trim_ns:>8.1}  mean {mean_ns:>9.1} ns  est {est_s:>7.3}s = {:>5.2}% of {wall_s:.1}s wall",
        100.0 * est_s / wall_s
    )
}

/// JSON form, for a tool that diffs two dumps.
pub(crate) fn render_json(cells: &[AtomicU64]) -> String {
    let events = cells[C_EVENTS].load(Relaxed);
    let sampled = cells[C_SAMPLED].load(Relaxed);
    let cycles = cells[C_CYCLES].load(Relaxed);
    let (per_ns, _) = calibration();
    let mean = if sampled == 0 { 0.0 } else { (cycles as f64 / sampled as f64) / per_ns };
    let ns = if sampled == 0 { 0.0 } else { median_cycles(cells, sampled) / per_ns };
    format!(
        "\"kind\":\"meter\",\"events\":{events},\"sampled\":{sampled},\"p50_ns\":{ns:.1},\"mean_ns\":{mean:.1},\"est_seconds\":{:.4},\"wall_seconds\":{:.3}",
        ns * events as f64 / 1e9,
        process_start().elapsed().as_secs_f64()
    )
}
