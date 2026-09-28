//! Callback and compilation counters: the inert tallies every diagnostic in this crate reads, and the reports that render them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use super::{
    HG_LEGACY, HG_STATS, HG_TRACE_MEGA, callback_stats_enabled, hg_bit, hot_gates,
    trace_mega_enabled,
};


/// Per-callback invocation counters (diagnostic): compiled code's remaining VM
/// callbacks are the cost ceiling on jitted heap/dispatch code, so knowing which
/// dominate directs the next optimization. Disabled unless their consumer is
/// enabled; millions of cross-worker relaxed RMWs are not free diagnostics.
pub(crate) static CB_COUNTS: [std::sync::atomic::AtomicU64; 28] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 28];
/// The last 17 entries SPLIT what used to be one `other` bucket: it grew by 7.95 M
/// callbacks under RAVA_ARENA_LAMBDA (5.27 M -> 13.22 M), which is the whole of that
/// flag's remaining +8.5 s in the serial prefix, and one catch-all cannot say which
/// site did it. Named by their ORIGINAL source line so the report points straight at
/// the call — deliberately mechanical, so no guess about meaning enters the census.
pub(crate) const CB_NAMES: [&str; 28] = [
    "invoke", "getstatic", "putstatic", "new", "instanceof", "newarray", "getfield", "putfield",
    "aload", "acmp", "aastore",
    
"o2298", "o2308", "o2319", "o2329", "o2612", "o3708", "o3738", "o3763", "o3767", "o4231", "o4291", "o4298", "o4358", "o4550", "o4568", "o4590", "o4614"
,
];
// Per-thread callback tally (TEST ONLY). The global `CB_COUNTS` is process-wide,
// so a parallel test asserting "zero callbacks" would race other tests' callbacks.
// This thread-local counts only the current thread's callbacks, making the inline
// no-callback assertions race-free. Compiled out of non-test builds (`cb` stays
// byte-identical in production).
#[cfg(test)]
thread_local! {
    // Width MUST track CB_COUNTS/CB_NAMES — `note_cb` indexes all three with the same `i`.
    // When the census was widened 12 -> 28 this array was left at 12, so any callback kind
    // past the old `other` bucket panicked with an out-of-bounds index under `cfg(test)`
    // only: production was unaffected and every gate passed, while `drem_frem_libcall`
    // (which fires the widened `libcall` kind) failed the suite.
    static TL_CB: std::cell::Cell<[u64; CB_NAMES.len()]> =
        const { std::cell::Cell::new([0u64; CB_NAMES.len()]) };
}
#[cfg(test)]
pub(crate) fn tl_cb(i: usize) -> u64 {
    TL_CB.with(|t| t.get()[i])
}

#[inline]
pub(crate) fn count_direct_hit() {
    if callback_stats_enabled() {
        DIRECT_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Return the one-based diagnostic ordinal, or zero when no consumer is live.
///
/// ⭐**TWO ACQUIRE LOADS PER DIRECT-DISPATCH MISS BECAME ONE.** The question is "does anyone want
/// the ordinal", and it used to be asked of two separate `OnceLock`s — `callback_stats_enabled()`
/// and `trace_mega_enabled()` — each an acquire load LLVM may not hoist out of the caller's
/// dispatch loop and may not forward across the calls between them. Both are bits of
/// `hot_gates()`, so it is now one word load and one masked test against a constant.
/// Population: every direct compiled→compiled dispatch MISS of a boot.
#[inline]
pub(crate) fn count_direct_miss(reason: usize) -> u64 {
    let w = hot_gates();
    let wanted = if w & hg_bit(HG_LEGACY) != 0 {
        // Opt-out arm (`RAVA_NO_JIT_GATEWORD=1`): the pre-fold shape, two separate cells.
        callback_stats_enabled() || trace_mega_enabled()
    } else {
        w & (hg_bit(HG_STATS) | hg_bit(HG_TRACE_MEGA)) != 0
    };
    if wanted {
        DIRECT_MISS[reason].fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    } else {
        0
    }
}

/// ⭐**THREE ACQUIRE LOADS PER COMPILED→HOST CROSSING BECAME ONE. This is the largest population
/// in the engine: ~30 M crossings a product boot, and 312,627,349 on a ring-heavy run.**
///
/// Before, this six-line function asked three DIFFERENT process constants, each behind its own
/// `OnceLock` in its own module and therefore its own acquire load, its own branch and its own
/// cache line:
///   1. `callback_stats_enabled()`            — is the callback census armed?
///   2. `gilfree_cf::stats::fast_lanecb()`    — inside `note_cb`, reached unconditionally
///   3. `s09census::enabled()`                — inside `note`, reached unconditionally
/// None of the three can change after the CLI's config apply, and an acquire load is an
/// optimisation barrier, so those were three barriers per crossing to learn three constants.
///
/// Now: ONE `hot_gates()` load at the top, bit-tested three times against constant masks. The
/// lane pre-gate is applied HERE from that word, so the common (main-thread, not-parallel) case
/// never enters `note_cb` at all and never touches its `OnceLock`; `note_cb` keeps its own
/// identical pre-gate, so this is a short-circuit of a duplicate test, not a second copy of the
/// invariant — the proof that `parallel::any_parallel()` is a strict superset of "some thread has
/// `in_lane == true`" lives at `gilfree_cf::stats::fast_lanecb` and is unchanged.
///
/// **Net: -2 acquire loads per crossing on ~30 M crossings.** Kill switch `RAVA_NO_JIT_GATEWORD=1`
/// (also `RAVA_NO_EMIT_MORE`), which routes to `cb_legacy` below — the exact pre-fold body.
#[inline]
pub(crate) fn cb(i: usize) {
    let w = hot_gates();
    if w & hg_bit(HG_LEGACY) != 0 {
        return cb_legacy(i);
    }
    if w & hg_bit(HG_STATS) != 0 {
        CB_COUNTS[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    // GILFREE-RINGS: remember the callback kind so a lane abort can name where it fired. The
    // pre-gate that used to live at the top of `note_cb` (and cost an acquire load per crossing
    // to evaluate) is hoisted into the word: with it set, the thread-local chain is reached only
    // while some thread is actually inside a parallel region. Clearing the bit — which the
    // opt-out arm does — falls straight through to `note_cb`'s own copy.
    #[cfg(feature = "next-gen-gc")]
    if w & hg_bit(super::HG_LANECB_FAST) == 0 || crate::parallel::any_parallel() {
        crate::gilfree_cf::note_cb(i);
    }
    // GAP-S09 stage-2 census: tally this callback kind iff the audit gate is on AND this thread is
    // inside a gil-free region. The gate is a bit of the word already in hand, so the armed case
    // costs one TLS read and the (universal) unarmed case costs a bit test.
    // ⚠ Test builds keep calling `note`, because its `FORCED` override is how the acceptance
    // tests arm a census whose read-once env gate has already latched OFF.
    #[cfg(not(test))]
    if w & hg_bit(super::HG_S09_AUDIT) != 0 && crate::parallel::gilfree() {
        s09census::bump(i);
    }
    #[cfg(test)]
    s09census::note(i);
    #[cfg(test)]
    TL_CB.with(|t| {
        let mut a = t.get();
        a[i] += 1;
        t.set(a);
    });
}

/// Pre-fold body of `cb`, reached only under `RAVA_NO_JIT_GATEWORD=1`: three independent gate
/// reads, one per consumer, each in its own module. Kept verbatim so an A/B of the switch
/// measures the SHAPE and nothing else. Cold and out-of-line — a default run never enters it.
#[cold]
#[inline(never)]
fn cb_legacy(i: usize) {
    if callback_stats_enabled() {
        CB_COUNTS[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(feature = "next-gen-gc")]
    crate::gilfree_cf::note_cb(i);
    s09census::note(i);
    #[cfg(test)]
    TL_CB.with(|t| {
        let mut a = t.get();
        a[i] += 1;
        t.set(a);
    });
}

/// GAP-S09 stage-2 callback census (`RAVA_S09_CB_AUDIT`, read-once gate): which
/// callback kinds fire while `parallel::gilfree()` is true. The 12 base ordinals
/// mirror `CB_NAMES`; three extra kinds split out the worker-illegal cold
/// callbacks the stage-3 abort stubs target, which the base tally cannot
/// distinguish (indy counts as `invoke`, ldc as `other`). Counts are process-wide
/// atomics filtered per-thread by the gilfree TLS flag; the report is one
/// `[S09-CB]` line at `Heap` drop. Inert (zero writes) when the env var is unset.
pub(crate) mod s09census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub(crate) const N: usize = 20;
    /// Extra ordinals past the 12 `CB_NAMES` kinds.
    pub(crate) const IDX_INDY: usize = 12;
    pub(crate) const IDX_LDC: usize = 13;
    pub(crate) const IDX_FU_INVOKE: usize = 14;
    /// Stage-3 splits: the map/amap MUTATORS (put/remove) and the BitSet
    /// RECEIVER-MUTATORS (set/clear) tally as the base `invoke` kind, which the
    /// worker-illegal census cannot distinguish — give each family an ordinal.
    pub(crate) const IDX_MAP_MUT: usize = 15;
    pub(crate) const IDX_BITSET_MUT: usize = 16;
    /// Stage-4: a PLAIN `vm_ref()` (untyped `&mut Vm` escape) reached on a
    /// gil-free worker. `note` tallies every such reach (audit gate); the
    /// ABORTS row counts the DENIALS (no locked-exec envelope → abort + None).
    pub(crate) const IDX_VM_REF_WORKER: usize = 17;
    pub(crate) const IDX_THROW_NULL: usize = 18;
    pub(crate) const IDX_NPE_SITE: usize = 19;
    const EXTRA_NAMES: [&str; 8] = ["indy", "ldc", "fu_invoke", "map_mut", "bitset_mut", "vm_ref_worker", "throw_null", "npe_site"];

    static COUNTS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    /// GAP-S09 stage-3 abort-stub tally: how many times each worker-illegal
    /// callback ABORTED a gil-free region (fired its stub). ALWAYS counted — the
    /// stub path is already a region teardown, so one relaxed add is noise — which
    /// lets acceptance tests assert "the admitted kernel never fires a stub"
    /// without arming the env-gated kind census above.
    static ABORTS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    #[inline]
    pub(crate) fn note_abort(kind: usize) {
        ABORTS[kind].fetch_add(1, Relaxed);
    }

    #[allow(dead_code)]
    pub(crate) fn abort_count(kind: usize) -> u64 {
        ABORTS[kind].load(Relaxed)
    }

    /// Test-only override: the read-once env gate may already have latched OFF
    /// before a test runs (any earlier gil-free test fires `note`), so tests arm
    /// the census through this flag instead. Compiled out of production — the
    /// release `enabled()` is exactly the one-OnceLock-load read-once gate.
    #[cfg(test)]
    static FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// `RAVA_S09_CB_AUDIT`, now bit `HG_S09_AUDIT` of the JIT gate word rather than a private
    /// `OnceLock`. Same predicate, same read-once semantics — but it is the SAME static that
    /// `cb`'s stats bit and the lane pre-gate come from, so a callback that calls `cb(0)` and then
    /// `note(..)` (every map mutator, every indy, every ldc) touches one gate location instead of
    /// two, and `cb` itself needs no call here at all.
    fn enabled() -> bool {
        #[cfg(test)]
        if FORCED.load(Relaxed) {
            return true;
        }
        crate::jit::hot_gates() & crate::jit::hg_bit(crate::jit::HG_S09_AUDIT) != 0
    }

    /// Tally, GATE ALREADY DECIDED BY THE CALLER. `jit::counters::cb` holds the gate word and the
    /// gil-free flag in hand; making it call `note` would re-ask both. Never call this without
    /// having tested `enabled()` (or its bit) and `parallel::gilfree()` first.
    #[allow(dead_code)] // `cb` reaches it only in non-test builds; tests go through `note`.
    #[inline]
    pub(crate) fn bump(kind: usize) {
        COUNTS[kind].fetch_add(1, Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn force_enable_for_test() {
        FORCED.store(true, Relaxed);
    }

    #[inline]
    pub(crate) fn note(kind: usize) {
        if enabled() && crate::parallel::gilfree() {
            COUNTS[kind].fetch_add(1, Relaxed);
        }
    }

    #[cfg(test)]
    pub(crate) fn count(kind: usize) -> u64 {
        COUNTS[kind].load(Relaxed)
    }

    /// One-line census dump (called from `Heap::drop`, the same hook the
    /// GAP-P01 interner tracer uses). Skips zero kinds; silent when the gate is
    /// off or nothing fired gil-free.
    pub(crate) fn report() {
        if !enabled() {
            return;
        }
        let mut out = String::new();
        for (i, c) in COUNTS.iter().enumerate() {
            let n = c.load(Relaxed);
            if n > 0 {
                let name = if i < super::CB_NAMES.len() {
                    super::CB_NAMES[i]
                } else {
                    EXTRA_NAMES[i - super::CB_NAMES.len()]
                };
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&format!("{name}={n}"));
            }
        }
        if !out.is_empty() {
            eprintln!("[S09-CB] gil-free callback census: {out}");
        }
        let mut ab = String::new();
        for (i, c) in ABORTS.iter().enumerate() {
            let n = c.load(Relaxed);
            if n > 0 {
                let name = if i < super::CB_NAMES.len() {
                    super::CB_NAMES[i]
                } else {
                    EXTRA_NAMES[i - super::CB_NAMES.len()]
                };
                if !ab.is_empty() {
                    ab.push(' ');
                }
                ab.push_str(&format!("{name}={n}"));
            }
        }
        if !ab.is_empty() {
            eprintln!("[S09-CB] worker-illegal ABORT stubs fired: {ab}");
        }
    }
}

/// Direct compiled→compiled call HITS (the invoke callbacks that skipped the
/// synthetic frame + marshal): `invoke − direct` = the full-gauntlet residue, i.e.
/// the actual per-call-tax population the next dispatch optimization should target.
pub(crate) static DIRECT_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Direct-call MISS reasons (heartbeat diagnostic): [0]=receiver shape undecodable,
/// [1]=megamorphic site, [2]=resolve refused (exclusion / callee not compiled /
/// non-bytecode). Which dominates decides the next dispatch lever.
pub(crate) static DIRECT_MISS: [std::sync::atomic::AtomicU64; 3] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// Cranelift CODE-SIZE / COMPILE-TIME census. Method-entry unswitches (PROMO,
/// FLAT-GETFIELD, COW) buy straight-line bodies by emitting the body more than
/// once, so their trade has to be measurable and not guessed: these three
/// counters are the deterministic half of it (the wall clock on this machine
/// swings ~2x run to run, `bench/bench_run.ps1`). Reported by `dump_jit_stats`
/// under `RAVA_JIT_STATS`.
pub(crate) static CODE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static COMPILE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static COMPILED_FNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `bodies=<n> code_bytes=<n> compile_ms=<n>` — the emitted machine-code volume
/// and the time Cranelift spent producing it.
pub fn code_size_stats() -> String {
    format!(
        "bodies={} code_bytes={} compile_ms={}",
        COMPILED_FNS.load(std::sync::atomic::Ordering::Relaxed),
        CODE_BYTES.load(std::sync::atomic::Ordering::Relaxed),
        COMPILE_NS.load(std::sync::atomic::Ordering::Relaxed) / 1_000_000,
    )
}

/// Render the callback counters (heartbeat diagnostic), skipping zeros.
pub fn callback_stats() -> String {
    let mut out = String::new();
    for (i, name) in CB_NAMES.iter().enumerate() {
        let n = CB_COUNTS[i].load(std::sync::atomic::Ordering::Relaxed);
        if n > 0 {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&format!("{name}={n}"));
        }
    }
    let d = DIRECT_HITS.load(std::sync::atomic::Ordering::Relaxed);
    if d > 0 {
        out.push_str(&format!(" direct={d}"));
    }
    let (mr, mm, mres) = (
        DIRECT_MISS[0].load(std::sync::atomic::Ordering::Relaxed),
        DIRECT_MISS[1].load(std::sync::atomic::Ordering::Relaxed),
        DIRECT_MISS[2].load(std::sync::atomic::Ordering::Relaxed),
    );
    if mr + mm + mres > 0 {
        out.push_str(&format!(" dmiss_recv={mr} dmiss_mega={mm} dmiss_res={mres}"));
    }
    #[cfg(feature = "next-gen-gc")]
    {
        let field_repr = crate::jit::field_repr_census_report();
        if !field_repr.is_empty() {
            out.push(' ');
            out.push_str(&field_repr);
        }
        let field_probe = crate::jit::field_probe_census_report();
        if !field_probe.is_empty() {
            out.push(' ');
            out.push_str(&field_probe);
        }
    }
    out
}
