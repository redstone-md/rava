//! Environment-variable feature gates, folded into ONE process-constant word.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! ⭐**NINETEEN PROCESS CONSTANTS, ONE READ (`RAVA_NO_JIT_GATEWORD=1` restores the old shape).**
//!
//! Every gate below used to own a private `OnceLock<bool>`. On the fast path a `OnceLock` read is
//! an ACQUIRE-ordered load of its state word plus a branch plus the payload load — and an acquire
//! load is an OPTIMISATION BARRIER: LLVM may not hoist it out of the caller's loop and may not
//! forward one across the opaque calls that sit between two gate sites. So N gates read on one
//! path meant N independent barriers, N branches and N cache lines, all to learn things that
//! cannot change after the CLI's config apply (the only writer of this process's environment,
//! which completes before the VM exists).
//!
//! WHAT THAT COST, PER OPERATION, ON THE POPULATIONS THAT MATTER:
//!   * `jit::counters::cb` — EVERY compiled→host crossing, ~30 M a product boot and 312,627,349
//!     on a ring-heavy run — read THREE separate `OnceLock`s: `callback_stats_enabled()` here,
//!     `gilfree_cf::stats::fast_lanecb()` inside `note_cb`, and `s09census::enabled()` inside
//!     `note`. Now ONE, read at the top of `cb` and bit-tested three times. **-2 acquire loads
//!     per crossing.**
//!   * `jit::counters::count_direct_miss` — every direct compiled→compiled dispatch MISS — read
//!     `callback_stats_enabled()` and `trace_mega_enabled()`, two acquire loads, to answer one
//!     question. Now one load and one masked test. **-1 acquire load per miss.**
//!   * `jit::direct::fu_admits` (hand-off, not this file) reads `map_intrinsic_enabled` ×3,
//!     `iter_intrinsic_enabled` ×3 and `entry_intrinsic_enabled` ×1 in a single call; a
//!     `fork_plan` build reads `jit_parallel_enabled` ×8. Those seven and eight loads now hit
//!     ONE static instead of three and one instead of eight, which is what lets the compiler
//!     fold them where it can prove nothing intervenes — the caller-side thread-down is recorded
//!     as a hand-off, because those files belong to other agents this round.
//!
//! The word is the `natives::entry::entry_gates()` shape one layer down: every bit is the SAME
//! predicate computed by the SAME expression (`raw_gate`, the single source of truth used by both
//! the word and the opt-out cells), so an armed run behaves exactly as it did.
//!
//! ⚠ POSITION IS NOT LOAD-BEARING FOR ANY BIT HERE. Every `raw_gate` arm is a pure
//! `std::env::var_os` test (or `gilfree_cf::fast_lanecb`, itself a pure env test): none has a
//! side effect and none decides whether a *later* side effect fires, so latching all nineteen at
//! the first `hot_gates()` call cannot reorder anything observable. `hot_gates` is reached only
//! from JIT code, i.e. after the VM exists, i.e. after config apply.

/// Bit ORDINALS in the word (`hg_bit(i)` turns one into its mask). Kept as ordinals rather than masks
/// so `raw_gate` and the opt-out cell array can be indexed by the same value.
pub(crate) const HG_LEGACY: usize = 0;
pub(crate) const HG_STATS: usize = 1;
pub(crate) const HG_S09_AUDIT: usize = 2;
pub(crate) const HG_LANECB_FAST: usize = 3;
pub(crate) const HG_TRACE_MEGA: usize = 4;
const HG_ENV_MEMO: usize = 5;
pub(crate) const HG_BLOCK_CENSUS: usize = 6;
const HG_MAP_INTRINSIC: usize = 7;
const HG_ITER_INTRINSIC: usize = 8;
const HG_ENTRY_INTRINSIC: usize = 9;
const HG_REF_STATIC_CELLS: usize = 10;
const HG_FIELD_VERIFY_OFF: usize = 11;
const HG_COW_UNSWITCH: usize = 12;
const HG_JIT_PARALLEL: usize = 13;
const HG_CALLBUF_WIDE_OFF: usize = 14;
const HG_LAMBDA_IMPL_KEY: usize = 15;
const HG_PRIM_ABI: usize = 16;
const HG_POSITIVE_DIV: usize = 17;
const HG_JIT_BACKEDGE_POLL: usize = 18;
const HG_N: usize = 19;

#[inline]
pub(crate) const fn hg_bit(i: usize) -> u32 {
    1u32 << i
}

/// THE SINGLE SOURCE OF TRUTH for what each bit means. Both the folded word and the opt-out
/// per-gate cells call this, so the two shapes cannot drift into disagreeing about a gate — the
/// failure mode that makes a kill switch worse than no kill switch.
fn raw_gate(i: usize) -> bool {
    match i {
        // Kill switch for the FOLD ITSELF (default ON, i.e. this bit clear). Falls together with
        // the other emit-more switches, as every gate in this tree does.
        HG_LEGACY => {
            std::env::var_os("RAVA_NO_JIT_GATEWORD").is_some() || crate::flags::emit_more_disabled()
        }
        HG_STATS => {
            std::env::var_os("RAVA_CALLBACK_STATS").is_some()
                || std::env::var_os("RAVA_HEARTBEAT").is_some()
                || std::env::var_os("RAVA_JIT_STATS").is_some()
        }
        // GAP-S09 stage-2 callback census (`jit::counters::s09census`).
        HG_S09_AUDIT => std::env::var_os("RAVA_S09_CB_AUDIT").is_some(),
        // MIRROR OF `gilfree_cf::stats::fast_lanecb`, read here ONCE so `cb` can apply the lane
        // pre-gate without paying that module's own `OnceLock` on all ~30 M crossings. It is the
        // same accessor, called once per process, not a second copy of the predicate — the proof
        // that `parallel::any_parallel()` is a strict superset of "some thread has `in_lane`"
        // lives at `fast_lanecb` and is not restated here.
        HG_LANECB_FAST => crate::gilfree_cf::fast_lanecb(),
        // The megamorphic-site trace consumes the direct-miss ordinal even when the broader
        // callback report is disabled.
        HG_TRACE_MEGA => std::env::var_os("RAVA_TRACE_MEGA").is_some(),
        HG_ENV_MEMO => std::env::var_os("RAVA_NO_ENV_MEMO").is_none(),
        HG_BLOCK_CENSUS => std::env::var_os("RAVA_JIT_BLOCK_CENSUS").is_some(),
        HG_MAP_INTRINSIC => std::env::var_os("RAVA_NO_MAP_INTRINSIC").is_none(),
        HG_ITER_INTRINSIC => std::env::var_os("RAVA_NO_ITER_INTRINSIC").is_none(),
        HG_ENTRY_INTRINSIC => std::env::var_os("RAVA_NO_ENTRY_INTRINSIC").is_none(),
        HG_REF_STATIC_CELLS => std::env::var_os("RAVA_NO_REF_STATIC_CELLS").is_none(),
        HG_FIELD_VERIFY_OFF => std::env::var("RAVA_FIELD_VERIFY").is_err(),
        HG_COW_UNSWITCH => std::env::var_os("RAVA_NO_COW_UNSWITCH").is_none(),
        HG_JIT_PARALLEL => std::env::var_os("RAVA_NO_JIT_PARALLEL").is_none(),
        HG_CALLBUF_WIDE_OFF => std::env::var_os("RAVA_NO_CALLBUF_WIDE").is_some(),
        HG_LAMBDA_IMPL_KEY => {
            std::env::var("RAVA_LAMBDA_IMPL_KEY").map(|v| v != "0").unwrap_or(false)
        }
        HG_PRIM_ABI => std::env::var_os("RAVA_PRIM_ABI").is_some(),
        HG_POSITIVE_DIV => std::env::var("RAVA_JIT_POSITIVE_DIV").ok().as_deref() == Some("1"),
        HG_JIT_BACKEDGE_POLL => std::env::var_os("RAVA_JIT_POLL").is_some(),
        _ => false,
    }
}

/// The whole gate set as one word, computed once per process.
///
/// ⚠ NOT RE-ENTRANT, and it does not need to be: the only non-`env::var_os` arm is
/// `gilfree_cf::fast_lanecb`, which reads its own env var and `flags::emit_more_disabled` and
/// never calls back here. A re-entrant `get_or_init` would deadlock, so any future arm must keep
/// that property.
#[inline]
pub(crate) fn hot_gates() -> u32 {
    static W: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u32;
        for i in 0..HG_N {
            if raw_gate(i) {
                w |= hg_bit(i);
            }
        }
        // In the opt-out arm the LANE pre-gate bit must read FALSE, so `jit::counters::cb` falls
        // through into `gilfree_cf::note_cb` and that function applies its own pre-gate — i.e.
        // byte-for-byte the pre-fold shape, which is the only thing an A/B of this switch is for.
        if w & hg_bit(HG_LEGACY) != 0 {
            w &= !hg_bit(HG_LANECB_FAST);
        }
        w
    })
}

/// One gate, by ordinal. Non-legacy: one word load and one bit test against a constant mask.
#[inline]
fn gate(i: usize) -> bool {
    let w = hot_gates();
    if w & hg_bit(HG_LEGACY) != 0 {
        return legacy_gate(i);
    }
    w & hg_bit(i) != 0
}

/// OPT-OUT SHAPE (`RAVA_NO_JIT_GATEWORD=1`): one `OnceLock<bool>` per gate, populated from the
/// same `raw_gate` expression — exactly what each accessor owned privately before the fold, so
/// the A/B measures the SHAPE and nothing else. Cold and out-of-line: in a default run it is
/// never entered and must not cost the fast path a single byte of I-cache.
#[cold]
#[inline(never)]
fn legacy_gate(i: usize) -> bool {
    static CELLS: [std::sync::OnceLock<bool>; HG_N] =
        [const { std::sync::OnceLock::new() }; HG_N];
    *CELLS[i].get_or_init(|| raw_gate(i))
}

#[inline]
pub(crate) fn callback_stats_enabled() -> bool {
    gate(HG_STATS)
}

/// A process-constant forensic gate, read ONCE instead of on every compiled→host callback
/// DEFAULT-ON since 2026-07-26 (`RAVA_NO_ENV_MEMO` restores the original per-call read).
/// Shipped on a clean-separation A/B: blocks A+B, every ON run below every OFF run,
/// min -7.4% / median -7.8%. Sound because the only env writer is CLI config apply,
/// which completes before the VM exists.
///
/// `std::env::var_os` on Windows is `RtlQueryEnvironmentVariable`: a linear walk of the
/// process environment block, per call. Three of these sit directly on the callback path —
/// the invoke thunk reads two and the indy thunk one, and the compiled getfield path reads a
/// fourth — and RIP sampling puts `RtlQueryEnvironmentVariable` at ~1.1% of blocks A+B with
/// no consumer at all: every one of these gates is a dump that is off in any run not being
/// debugged. The gates name diagnostics, so their value cannot change during a run; the only
/// writer of the environment is the CLI's config apply, which finishes before the VM starts.
#[inline]
pub(crate) fn env_gate(var: &'static str, cell: &'static std::sync::OnceLock<bool>) -> bool {
    if !env_memo_enabled() {
        return std::env::var_os(var).is_some();
    }
    *cell.get_or_init(|| std::env::var_os(var).is_some())
}

#[inline]
pub(crate) fn env_memo_enabled() -> bool {
    gate(HG_ENV_MEMO)
}

/// The megamorphic-site trace consumes the direct-miss ordinal even when the
/// broader callback report is disabled. Cache the process configuration: an
/// environment lookup on every miss is itself substantial hot-path work.
#[inline]
pub(crate) fn trace_mega_enabled() -> bool {
    gate(HG_TRACE_MEGA)
}

/// Call-buffer slot cap for the machine-code dispatch IC: a cached callee's
/// `max_locals` (leaf) or `max_locals + max_stack` (deep) must fit, and the
/// per-site call buffer is this many i64 slots. 16 by default; the opt-in DEEP
/// arm widens to 32 (worldgen compute bodies) — widening unconditionally
/// bloated EVERY PIC site's stack frame by 128B and alone cost a measurable
/// slowdown across the compiled population.
#[cfg(feature = "next-gen-gc")]
/// `RAVA_NO_CALLBUF_WIDE`, now a bit of `hot_gates()`. Read once per emitted PIC site (~3000 per
/// boot) rather than being genuinely hot, but it shares the word with everything else so it costs
/// no static of its own.
#[inline]
pub(crate) fn callbuf_wide_disabled() -> bool {
    gate(HG_CALLBUF_WIDE_OFF)
}

/// λ-IMPL KEY (RAVA_LAMBDA_IMPL_KEY, default OFF): key the per-site direct-call
/// cache on a lambda's IMPLEMENTATION identity — the pinned `Arc<LambdaSite>`
/// address (`Heap::lambda_site_key`) — instead of the receiver object.
///
/// A CAPTURING lambda's receiver is a fresh object per instantiation, so a
/// receiver key never repeats: `BitSetDiscreteVoxelShape.join` mints three nested
/// closures per `joinUnoptimized` and `forMergedIndexes` calls `IndexConsumer.merge`
/// on a fresh closure per merged index. The implementation those closures target is
/// the same every time. Keying on it makes such a site MONOMORPHIC, and replaces the
/// incumbent `Heap::lambda_fp` — a 64-bit FNV fold over owner+name+descriptor
/// (~120 bytes, recomputed TWICE per dispatch: once to build the probe key, once for
/// the invoke-time re-check) — with a single pointer load. It is also strictly
/// SOUNDER: `lam_fp` is a hash, so two distinct implementations could in principle
/// collide onto one cached target; an address cannot.
#[inline]
pub(crate) fn lambda_impl_key_enabled() -> bool {
    gate(HG_LAMBDA_IMPL_KEY)
}

// FAST FRAME (was `RAVA_DISPATCH_FASTFRAME`, retired 2026-07-27): take the per-thread
// JIT context ONCE per warm direct compiled→compiled call and do the whole synthetic
// frame's bookkeeping through that handle. The gate is gone and the shape is
// unconditional — see `Vm::jit_direct_invoke` and `frames::ctx_cells_push` for the
// mechanism and for the crossing census that retired the OFF arm.

/// Extend the getstatic value cells to REFERENCE statics. DEFAULT-ON since
/// 2026-07-26 (`RAVA_NO_REF_STATIC_CELLS` disables) — the largest single win of
/// the host-bound arc, measured on the Done window it targets: five interleaved
/// full boots in both orderings, EVERY ON below EVERY OFF with ~25s of daylight
/// (OFF Done 90.6/90.4/88.3s vs ON 63.3/60.6s, −30%; process wall −17-20%).
/// Mechanism: the ring's ~20% getstatic host cluster (RIP-profiled) collapses —
/// RingScan getstatic callbacks 17.39M → 6.02M, ChunkFill compiled→interp
/// fallbacks 129.7M → 26.6M, and one INLINED site (`SimplexNoise.GRADIENT`) that
/// alone made 101.3M callbacks now reads an inline gen-guarded cell.
///
/// The historical exclusion ("a cached ref handle could dangle past
/// a GC") is already answered by the gen guard: `static_gen` bumps on every
/// putstatic AND on every arena collection (`arena_safepoint_gc`/`arena_collect_jit`
/// both call `bump_static_gen`), so a post-GC read always misses the cell and
/// refills through the real interpreter handler — the exact invariant the interp's
/// own `tlcache` getstatic cache (which caches ref Values) has always relied on.
/// The one UNSTABLE packed form — a `BOX_TAG` side-table index (`Str`, sentinel,
/// boxed primitive), whose table unwinds with the compiled frame — is skipped at
/// REFILL time (`rava_getstatic_cached` leaves the cell sentinel), decided
/// dynamically over the value like the key-intern purity check. Ref statics are
/// additionally kept OUT of the static-LICM readonly arm (it reads the cell with
/// NO gen check, which is only sound for immovable primitives).
#[inline]
pub(crate) fn ref_static_cells_enabled() -> bool {
    gate(HG_REF_STATIC_CELLS)
}

/// AUTO-OPT (RAVA_ITER_INTRINSIC, default OFF pending MC verification): route
/// ArrayList.iterator() to a native $elems/$pos iterator and java/util/Iterator
/// next/hasNext to the fu_fastpath serve — the highest-count uncovered native
/// dispatch in MC boot (Iterator#next/hasNext ~5M calls). Non-native iterators
/// fall back to the general path (fu_fastpath returns None), so it's safe by
/// construction for the for-each case; iterator.remove()/CME divergence is why
/// it stays gated until an MC-boot A/B + broad correctness run confirms parity.
/// `java/util/Map$Entry#getKey/getValue` served from machine code off `$key`/`$val`
/// (`fu_fastpath` FuKind::Entry). DEFAULT ON; `RAVA_NO_ENTRY_INTRINSIC` restores the
/// dispatch-miss route. The cascade's `entry_fastpath` serves the same two fields but only
/// after the miss has been paid — 5,082,022 of them a boot.
#[inline]
pub(crate) fn entry_intrinsic_enabled() -> bool {
    gate(HG_ENTRY_INTRINSIC)
}

/// ⛔**STAYS OPT-IN, AND THE WIDENING THAT WAS SUPPOSED TO EARN IT THE DEFAULT HANGS THE BOOT.**
/// The admission covered only `java/util/Iterator`, which reaches the loop body of an
/// enhanced-`for` but never the `iterator()` that CREATES it (that call's owner is the
/// collection's static type) nor fastutil's `ObjectSet`/`ObjectIterator` — 4.94% of the boot's
/// crossings. Widening it to those owners, exactly as the map admission next door was widened
/// to `java/util/Map`, measured:
///   RAVA_NO_ITER_INTRINSIC=1 (off)  wall 38.3 s, Done 0.448, reached
///   intrinsic on                    wall 700 s, `Done` NEVER REACHED  (x2, fresh binary)
/// ⭐**And the 4 bit-exact gates and all 21 differential probes passed on that build.** A
/// fast path that hands back a plausible-but-wrong iterator does not corrupt an answer the
/// gates hash — it fails to TERMINATE, which is a shape no correctness gate in this tree
/// looks for. The narrow gate itself is sound (38.3/37.3/37.8 on vs 38.3/38.3/38.3 off) but
/// that is -1.3% median, under the boot floor, so it does not earn a default either.
/// ✅**DEFAULT ON as of 2026-08-28, and the number that flipped it came from a PROBE, not the
/// boot.** `bench/StreamApplyCost.java` — a four-element `ArrayList` iterated in a loop, which
/// is the boot's dominant collection shape — costs 2,861-2,965 ns per element without this and
/// 2,337-2,533 with it, **-17%**, in a run that takes ten seconds instead of thirty-five.
/// `bridgestat` says why: `List#iterator` is 36.9% of that probe at 2,064 ns per call, and the
/// cost is the CROSSING, not the work — copying the backing array directly measured exactly
/// zero over 780,000 verified hits.
///
/// The boot then agreed, where the same flag had read -1.3% (under the floor) when it was last
/// measured: eight rounds, P-cores, median wall **36.8 s against 38.25** = -3.8%, with the ON
/// arm reading 36.8 three times out of four.
///
/// ⛔The WIDENING of this admission is a separate thing and it HANGS THE BOOT — see the
/// recorded measurement above. Only the narrow form ships.
#[inline]
pub(crate) fn iter_intrinsic_enabled() -> bool {
    gate(HG_ITER_INTRINSIC)
}

/// `Map.get` served from machine code (`fu_fastpath`). Default ON with a kill switch, like the
/// entry intrinsic: it routes to `map_method`, the same function the general path ends at, and
/// admits only receivers carrying a native `$map`.
#[inline]
pub(crate) fn map_intrinsic_enabled() -> bool {
    gate(HG_MAP_INTRINSIC)
}

/// `RAVA_PRIM_ABI` gate for the register-ABI arc (docs/register-abi-arc.md). Default OFF:
/// every increment stays byte-neutral (ChunkFill 8790) with the flag unset until I4.
#[allow(dead_code)]
#[inline]
pub(crate) fn prim_abi_enabled() -> bool {
    gate(HG_PRIM_ABI)
}

/// Drop the `ids[slot]==fid` verify in the const-slot getfield, trusting
/// UNIVERSAL pre-population (every class's layout is cached at init via
/// `ensure_init` → the alloc choke-point fills the fixed slots of EVERY object,
/// `new` or native, so the class-constant slot is always valid). The fast path
/// becomes STRAIGHT-LINE (null → callback-sets-NPE + early return, a side exit
/// with NO merge-phi), so the readonly cells load is loop-invariant and the
/// egraph hoists it (full field-LICM): Fields 2.3x, DispatchMono 2.7x.
/// DEFAULT ON; opt-out `RAVA_FIELD_VERIFY` restores the guarded scan (a fallback
/// if a never-initialised class is ever native-allocated then field-read).
#[inline]
pub(crate) fn field_verify_disabled() -> bool {
    gate(HG_FIELD_VERIFY_OFF)
}

/// Data-parallel fork codegen (multi-core): DEFAULT ON (opt-out RAVA_NO_JIT_PARALLEL).
/// A recognised step-1 associative-accumulator loop compiles a worker body + a
/// `parallel_reduce` fork with a serial fallback. MEASURED on this branch (2026-07-02,
/// [DBG-PAR] fold=false analyzed=Some step_ok=true): bench/ArrReduce HOTNS 8ms vs the
/// JVM's 77ms — ~9.6x FASTER THAN THE JVM, SUM bit-exact. (An earlier 'recognition
/// regression' note here was a measurement artifact: truncated timeouts on a loaded
/// host killed the run before the interpreter warmup crossed the compile threshold —
/// at a warmed threshold the fork engages and wins.)
///
/// ⭐Read EIGHT TIMES in one `runtime::fork_plan` build; all eight now hit the one word.
#[inline]
pub(crate) fn jit_parallel_enabled() -> bool {
    gate(HG_JIT_PARALLEL)
}

/// COW method-entry UNSWITCH opt-out (`RAVA_NO_COW_UNSWITCH`, default ON).
/// Set it to fall back to the previous shape — one hoisted `[ctx+12]` test
/// reused by a per-site armed/inline DIAMOND at every getfield/putfield. The
/// unswitch instead branches ONCE in the entry block and emits a DISARMED body
/// with no COW code at all next to an ARMED body with today's diamonds; the
/// disarmed body is what the main thread runs for the whole boot.
#[inline]
pub(crate) fn cow_unswitch_enabled() -> bool {
    gate(HG_COW_UNSWITCH)
}

#[inline]
pub(crate) fn positive_div_enabled() -> bool {
    gate(HG_POSITIVE_DIV)
}

/// Opt-in stop-only safepoints before emitted backward branches. Never invokes GC.
#[inline]
pub(crate) fn jit_backedge_poll_enabled() -> bool {
    #[cfg(test)]
    if TEST_JIT_BACKEDGE_POLL.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    gate(HG_JIT_BACKEDGE_POLL)
}

#[cfg(test)]
static TEST_JIT_BACKEDGE_POLL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn force_jit_backedge_poll_for_test(enabled: bool) {
    TEST_JIT_BACKEDGE_POLL.store(enabled, std::sync::atomic::Ordering::Relaxed);
}
