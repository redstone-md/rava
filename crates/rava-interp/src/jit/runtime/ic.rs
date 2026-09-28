//! The inline-cache substrate: global slots, per-thread tables, and the gates over them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// The interned `java/util/BitSet` class KEY (first word of the `Arc<str>` class
/// name = pointer-to-ArcInner, exactly what the machine-code heap-Ref reads at
/// `[box_ptr + Object::class_offset]`). Populated once by `jit_bitset_recv`; read
/// at RUNTIME by the inline `BitSet.get` fast arm to prove the receiver is EXACTLY
/// `java.util.BitSet` before touching `$bits` directly (a subclass override must
/// take the slow arm — matching `jit_bitset_recv`'s exact-class guard). 0 until
/// populated → the guard fails → the slow arm serves (always correct), so a late
/// populate only costs speed, never correctness. Not baked into machine code (the
/// codegen loads it from this stable static address), so a run before/after the
/// populate produces identical results.
#[cfg(feature = "next-gen-gc")]
/// The BARE interned id of `$elems` PLUS ONE, published by `Vm::publish_arraylist_word`.
///
/// ⛔**The `+1` is not decoration.** Field ids are `field_names.len()` at first sighting, so the
/// first name any VM interns gets id **0** — and publishing at construction makes `$elems`
/// exactly that first name. A raw id here would be indistinguishable from "nothing published
/// yet", which is how the arm silently never emitted: `[ALIST] publish elems_fid=0`.
///
/// ⛔**IT MUST BE THE BARE ID, AND IT MUST NOT BE RESOLVED AT COMPILE TIME.** The obvious route
/// — the driver's `intern(owner, field)` closure — is owner-QUALIFIED, so it answers an id the
/// natives never store under (they write `$elems` through `field_id("$elems")`), and merely
/// asking perturbs how `$elems` resolves afterwards: measured at **3,357 -> 43,000 ns** on
/// `bench/OpCost2.java`'s index loop with the inline arm still disabled and all four gates
/// bit-exact. Publishing the bare id from the native side and loading it here costs one relaxed
/// read and has no side effect at all.
pub(crate) static ELEMS_FID: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

/// The interned `java/util/ArrayList` class KEY, published once by `Vm::publish_arraylist_word`
/// and read by the inline `ArrayList.get` arm's exact-class guard. Zero until published, and the
/// guard treats zero as "take the slow arm" — which is what publishes it, so the fast path
/// bootstraps itself without any ordering requirement.
pub(crate) static ARRAYLIST_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

/// The interned `it/unimi/dsi/fastutil/doubles/DoubleArrayList` class KEY. Published beside the
/// ArrayList word, for the same guard and the same reason — except this one covers the single
/// largest (class, method) pair in the engine: **`DoubleArrayList#getDouble`, 10,528,512 calls a
/// boot**, found by counting the `fu_fastpath` population by RECEIVER rather than by name.
/// The `java/util/Iterator` class key and its two field ids, published from a LIVE receiver the
/// first time `fu_fastpath` classifies one — the lesson of the `getDouble` arm, which published
/// from `intern_class` at construction and compared two unequal words 16.9 M times.
///
/// The population: `Iterator#hasNext` **4,023,040** calls a boot and `#next` **3,173,120**, on
/// ONE fabricated class whose backing (`$elems`, `$pos`) IS a guest field — which is exactly the
/// condition the `getDouble` attempt failed and the `ArrayList.get` arm satisfies.
pub(crate) static ITER_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
/// `$elems` id + 1, and `$pos` id + 1. Zero = not yet published, arm not emitted.
pub(crate) static ITER_ELEMS_FID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
pub(crate) static ITER_POS_FID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
pub(crate) static ITER_SOURCE_FID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// `net/minecraft/world/phys/shapes/CubePointRange` and its `parts` field id + 1. **6,504,704
/// `getDouble` calls a boot** — the second largest (class, method) pair in the engine — and the
/// native answers each with `i / parts`, one guest int field and a division. Published from a
/// live receiver, never from an interned name.
pub(crate) static CUBE_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
pub(crate) static CUBE_PARTS_FID: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

pub(crate) static DALIST_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
/// ⭐**`DoubleArrayList`'s BACKING ARRAY IS THE FIELD `a`, NOT `$elems`.**
///
/// The inline `getDouble` arm read `env.elems_fid` — the synthetic field the NATIVE ITERATOR
/// carries — on a receiver that has no such field, so its guard reached the field test every
/// time and passed it never (`dg_elem` hit, `dg_fast` zero) and the route was written off as
/// closed. Meanwhile `fu_double_view` has been reading the real thing all along, by resolving
/// `a` and `size` on the owner and caching the ids.
///
/// Published from that same resolution, +1 so that 0 means "not yet known" (the arm is not
/// emitted until both are), exactly like `ITER_ELEMS_FID`.
///
/// ⚠**`size`, not the array's length.** A `DoubleArrayList` over-allocates, so the logical
/// length is the `size` field and the physical array is longer; bounds-checking against the
/// array would hand the guest elements past the end of the list.
pub(crate) static DALIST_A_FID: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
pub(crate) static DALIST_SIZE_FID: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

/// Gate observations: how often the `getDouble` arm's emission test was reached with the ids
/// already published, versus reached too early. Distinguishes a compile-ORDER problem from a
/// refusal further upstream — the question that two wrong conclusions today came from skipping.
pub(crate) static DALIST_GATE_READY: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static DALIST_GATE_COLD: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// True once the class word and both field ids are known, i.e. a `DoubleArrayList` has been
/// seen. Until then the inline arm is not emitted at all.
pub(crate) fn dalist_ready() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    // The CLASS WORD is NOT required at emit time: it can only be taken from a live receiver, and
    // the emitted guard already loads it from this atomic per call and refuses while it is zero.
    // Demanding it here made the gate cold on all ten sites that reach it, because those sites
    // compile before the first `DoubleArrayList` exists.
    DALIST_A_FID.load(Relaxed) != 0 && DALIST_SIZE_FID.load(Relaxed) != 0
}

/// ⭐**`Map$Entry.getKey`/`getValue` — 464,896 crossings a boot**, #5 and #7 of the receiver
/// census, and each is ONE field read of a fabricated two-field object. The native arm
/// (`FuKind::Entry`) already answers them from `$key`/`$val`; reaching it still leaves machine
/// code, which is the whole cost.
///
/// Published from a LIVE receiver, like every other guard word here: interning the name
/// separately yields a different `Arc<str>` and the guard then never matches.
pub(crate) static ENTRY_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
pub(crate) static ENTRY_KEY_FID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
pub(crate) static ENTRY_VAL_FID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// True once both field ids are known. The class WORD is deliberately NOT required — it can only
/// come from a live receiver, and the emitted guard loads it from the atomic per call and refuses
/// on zero. Requiring it here is the trap that kept the `getDouble` arm at zero for months.
pub(crate) fn entry_ready() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    ENTRY_KEY_FID.load(Relaxed) != 0 && ENTRY_VAL_FID.load(Relaxed) != 0
}


pub(crate) static BITSET_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

// ⭐**NINE PROCESS CONSTANTS AND A SLOT COUNT, ONE READ** (`RAVA_NO_FAST_DRIVERENV=1` restores
// the old shape).
//
// SHAPE BEFORE: every gate in this file owned a private `OnceLock` — `ic_deep_enabled`,
// `lambda_pic_enabled`, `pic_heur_enabled`, `wide_pic_enabled`, `wide_pic_native_enabled`,
// `ic_parallel_enabled`, `dispatch_callbuf_slots`, plus TWO raw reads that had no memo at all
// (`RAVA_M3_IC_COLD` in `ic_worker_table_init`). A `OnceLock` read is an ACQUIRE-ordered load of
// its state word plus a branch plus the payload load, and an acquire load is an OPTIMISATION
// BARRIER: LLVM may not hoist it out of the caller's loop and may not forward one across the
// opaque calls that sit between two gate sites. Eight statics meant eight barriers and eight
// cache lines, all to learn things that cannot change after the CLI's config apply.
//
// WHAT THAT COST, ON THE POPULATIONS THAT MATTER:
//   * `Vm::dispatch_resolve_cacheable_cls` — run per dispatch-miss resolve, the engine's most
//     travelled host path — reads `dispatch_callbuf_slots()` TWICE and `ic_deep_enabled()` once,
//     with `is_gc_leaf` (a whole bytecode scan) sitting between two of them so nothing could be
//     forwarded. THREE separate statics, three barriers. Now one word load and three masked
//     tests against constants.
//   * `Vm::…register…` (`vm/selfreg.rs`) reads `ic_deep_enabled()` then `dispatch_callbuf_slots()`
//     back to back, per deep-shape decision. Two statics → one.
//   * `wide_site_pic_ok` reads `wide_pic_enabled()` and then `wide_pic_native_enabled()` per
//     emitted wide call site (~thousands a boot). Two statics → one.
//   * `emit_dispatch_pic` reads `dispatch_callbuf_slots()` at five sites and `lambda_pic_enabled()`
//     at three, per emitted PIC site (~9,500 sites a boot).
//
// SHAPE AFTER: one `OnceLock<u32>` — nine bits plus the call-buffer slot count packed into bits
// 16..31 (its range is 4..=256, so it fits). Every accessor keeps its exact name, signature and
// verdict; only the load it performs changes.
//
// ⚠ POSITION IS NOT LOAD-BEARING FOR ANY BIT HERE. Eight arms are pure `env` tests. The ninth,
// `IG_WIDE_PIC_NATIVE`, additionally consults `pic_native::ref_enabled()` and
// `heappic::emit_mode()`, which are themselves pure env-backed `OnceLock`s that never call back
// into this file — so latching all nine at the first `ic_gates()` call cannot deadlock and cannot
// reorder an observable side effect. None of them arms a thread.
//
// ⚠ NOT RE-ENTRANT, and it does not need to be: no arm calls `ic_gates()`. `raw_callbuf_slots`
// takes the deep verdict as an ARGUMENT rather than calling `ic_deep_enabled()`, which is what
// keeps the initialiser acyclic (and, incidentally, removes this file's last unconditional
// reference to a `next-gen-gc`-only accessor from a non-`cfg`'d function).
/// Bit ordinals in the IC substrate's process-constant word.
const IG_LEGACY: usize = 0;
const IG_DEEP: usize = 1;
const IG_LAMBDA_PIC: usize = 2;
const IG_PIC_HEUR: usize = 3;
const IG_WIDE_PIC: usize = 4;
const IG_WIDE_PIC_NATIVE: usize = 5;
const IG_PARALLEL: usize = 6;
const IG_IC_COLD: usize = 7;
const IG_OFFSET_TRACE: usize = 8;
const IG_N: usize = 9;
/// The call-buffer slot count lives in the high half of the same word (values 4..=256).
const IG_SLOTS_SHIFT: u32 = 16;

#[inline]
const fn ig_bit(i: usize) -> u32 {
    1u32 << i
}

/// THE SINGLE SOURCE OF TRUTH for what each bit means. Both the folded word and the opt-out
/// per-gate cells call this, so the two shapes cannot drift into disagreeing about a gate — the
/// failure mode that makes a kill switch worse than no kill switch.
fn raw_ic_gate(i: usize) -> bool {
    match i {
        // Kill switch for the FOLD ITSELF (default ON, i.e. this bit clear). Falls together with
        // the other emit-more switches, as every gate in this tree does.
        IG_LEGACY => {
            std::env::var_os("RAVA_NO_FAST_DRIVERENV").is_some()
                || crate::flags::emit_more_disabled()
        }
        IG_DEEP => std::env::var_os("RAVA_NO_IC_DEEP").is_none(),
        IG_LAMBDA_PIC => std::env::var("RAVA_LAMBDA_PIC").map(|v| v != "0").unwrap_or(false),
        IG_PIC_HEUR => std::env::var_os("RAVA_NO_PIC_HEUR").is_none(),
        IG_WIDE_PIC => std::env::var_os("RAVA_NO_WIDE_PIC").is_none(),
        IG_WIDE_PIC_NATIVE => {
            // Both halves of the typed native entry must be live or the old wide-owner regression
            // comes straight back — see `wide_pic_native_enabled` for the whole argument. The
            // predicate is unchanged, only its storage is.
            #[cfg(feature = "next-gen-gc")]
            {
                !crate::flags::emit_more_disabled()
                    && std::env::var_os("RAVA_NO_WIDE_PIC_NATIVE").is_none()
                    && crate::jit::pic_native::ref_enabled()
                    && crate::heappic::emit_mode() == 2
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                false
            }
        }
        IG_PARALLEL => std::env::var("RAVA_M3_IC_PARALLEL").ok().as_deref() == Some("1"),
        IG_IC_COLD => std::env::var_os("RAVA_M3_IC_COLD").is_some(),
        IG_OFFSET_TRACE => std::env::var_os("RAVA_IC_OFFSET_TRACE").is_some(),
        _ => false,
    }
}

/// The whole IC gate set as one word, computed once per process. Low bits = the gates, bits
/// 16..31 = the call-buffer slot count.
#[inline]
pub(crate) fn ic_gates() -> u32 {
    static W: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        // OPT-OUT: publish ONLY the legacy bit and latch nothing else, so every other gate stays
        // lazily resolved by its own cell at its own first call — byte-for-byte the pre-fold
        // shape, which is the only thing an A/B of this switch is for.
        if raw_ic_gate(IG_LEGACY) {
            return ig_bit(IG_LEGACY);
        }
        let mut w = 0u32;
        for i in 1..IG_N {
            if raw_ic_gate(i) {
                w |= ig_bit(i);
            }
        }
        let slots = raw_callbuf_slots(w & ig_bit(IG_DEEP) != 0);
        w | ((slots as u32) << IG_SLOTS_SHIFT)
    })
}

/// OPT-OUT SHAPE (`RAVA_NO_FAST_DRIVERENV=1`): one `OnceLock<bool>` per gate, populated from the
/// same `raw_ic_gate` expression — exactly what each accessor owned privately before the fold, so
/// the A/B measures the SHAPE and nothing else. Cold and out-of-line: a default run never enters
/// it and must not pay it a byte of I-cache.
#[cold]
#[inline(never)]
fn legacy_ic_gate(i: usize) -> bool {
    static CELLS: [std::sync::OnceLock<bool>; IG_N] = [const { std::sync::OnceLock::new() }; IG_N];
    *CELLS[i].get_or_init(|| raw_ic_gate(i))
}

/// One bit out of an ALREADY-READ word. This is the form a caller that needs TWO gates uses: the
/// acquire load happens once, every use after it is a test against a constant mask.
#[inline]
fn ic_gate_of(w: u32, i: usize) -> bool {
    if w & ig_bit(IG_LEGACY) != 0 {
        return legacy_ic_gate(i);
    }
    w & ig_bit(i) != 0
}

/// One gate, by ordinal. Non-legacy: one word load and one bit test against a constant mask.
#[inline]
fn ic_gate(i: usize) -> bool {
    ic_gate_of(ic_gates(), i)
}

/// `RAVA_NO_CALLBUF_WIDE` without the `next-gen-gc` reference leaking into this file's non-`cfg`'d
/// code (it is a bit of `jit::hot_gates()` and only exists under that feature).
#[inline]
fn callbuf_wide_disabled_raw() -> bool {
    #[cfg(feature = "next-gen-gc")]
    {
        callbuf_wide_disabled()
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        false
    }
}

pub(crate) fn dispatch_callbuf_slots() -> usize {
    let w = ic_gates();
    if w & ig_bit(IG_LEGACY) != 0 {
        // OPT-OUT: the private `OnceLock<usize>` this used to be.
        static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        return *N.get_or_init(|| raw_callbuf_slots(legacy_ic_gate(IG_DEEP)));
    }
    (w >> IG_SLOTS_SHIFT) as usize
}

/// The slot-count decision itself, unchanged. `deep` is passed in (rather than read from
/// `ic_deep_enabled()`) so this can run inside the word's initialiser without re-entering it.
fn raw_callbuf_slots(deep: bool) -> usize {
    // RAVA_CALLBUF overrides for the A/B: deep-on widens to 32 by default (the
    // worldgen compute bodies), but the widening itself costs stack across
    // EVERY PIC site — 16 caps deep service to small frames instead.
    {
        if let Some(n) = std::env::var("RAVA_CALLBUF")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            // Ceiling raised from 64 so the width can be MEASURED past it. 64 is not a hardware
            // limit, it was the range of the old A/B, and the one method that anchors the
            // gil-free lane's dispatch lock does not fit even at 64 — a question the old clamp
            // made unaskable.
            .filter(|&n| (4..=256).contains(&n))
        {
            return n;
        }
        // WIDE DEEP BUFFER — DEFAULT ON (`RAVA_NO_CALLBUF_WIDE` restores 32).
        //
        // The width decides which NON-LEAF callees the machine-code deep arm may serve at all:
        // `dispatch_resolve_cacheable_cls` returns NeverCacheable — permanently, for every site
        // and every receiver — unless `max_locals + max_stack <= slots`. At 32 that quietly
        // disqualifies the large worldgen bodies, and every call to one of them falls through to
        // the `jit_call` gauntlet forever.
        //
        // Found by asking which symbol HOLDS the gil-free lane's dispatch lock, not by looking for
        // a codegen knob: the answer was one method, `BiomeSource.findBiomeHorizontal`, and the
        // reason it fell to the general path turned out to be this cap rather than anything about
        // threading. It throttles the ordinary serial boot far harder than it throttles the lane.
        //
        // MEASURED, interleaved, min-of-3, on the real MC boot:
        //   wall  off min=113.5 [113.5/118.8/118.9]  on min=97.6  [97.6/101/108]      -14.01% ON CLEAN
        //   done  off min=49.281 [49.281/…/51.483]   on min=33.812 [34.681/…/37.641]  -31.39% ON CLEAN
        // Every ON run beats every OFF run on both headline metrics. Gates bit-exact
        // (ChunkFill 8790f04ace726ecf, RingScan(N=8) 37539c6c375d0c3), and those benches ran
        // 20-35% faster themselves.
        //
        // The cost is real and is why the knob survives: the buffer is the CALLER's spill area,
        // emitted at every PIC site, so the stack is paid everywhere while the benefit lands on
        // the few over-32 callees. The measurement says that trade is strongly worth it here.
        if callbuf_wide_disabled_raw() {
            return if deep { 32 } else { 16 };
        }
        if deep {
            64
        } else {
            16
        }
    }
}

/// DEEP dispatch-IC admission for self-registering non-leaf compiled callees.
/// V3 compiles each eligible callee with a prologue that checks faults, zeroes
/// non-argument slots, pushes its cell-root range through the baked per-VM
/// context slot, snapshots box length, and pops inline on return. The unified
/// caller hit always spills and reloads its own live references; the callee
/// prologue roots its frame with no TLS crossing.
///
/// Default on; `RAVA_NO_IC_DEEP=1` disables non-leaf admission and the
/// self-registering prologue while retaining GC-leaf cache entries. It also
/// controls the 32-slot deep call-buffer budget.
///
/// Verified bit-exact (RingScan 109205b3 / ChunkFill 8790 / Decode a17dbfa0)
/// plus ngg 228 / default 108.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn ic_deep_enabled() -> bool {
    // Now a bit of `ic_gates()`: read alongside `dispatch_callbuf_slots()` on the
    // dispatch-resolve and self-registration paths, so both come out of ONE acquire load.
    ic_gate(IG_DEEP)
}

/// λ-PIC (RAVA_LAMBDA_PIC, default OFF): serve NON-CAPTURING ("directable") lambda
/// receivers from the machine-code heap-Ref PIC by IDENTITY. All lambdas share the
/// "$Lambda" class word, so the class-keyed heap-Ref arm can never serve them
/// (~13.2M dispatch-miss falls per canonical run, ~6.2M directable). Under the gate
/// the refpic arm remaps a "$Lambda" class word to the receiver's identity key
/// `(recv_cell << 1) | 1` (see `emit_lambda_key_select`) and probes the SAME 4-way
/// cache; `Vm::lambda_pic_fill` populates it. Read ONCE at first use — the emitted
/// machine code and the fill path must agree for the whole process lifetime.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn lambda_pic_enabled() -> bool {
    // Read three times inside one `emit_dispatch_pic` (~9,500 sites a boot) and once per
    // dispatch miss in `vm::pic_fill`; now one bit of `ic_gates()`.
    ic_gate(IG_LAMBDA_PIC)
}

/// The interned `$Lambda` class KEY (first word of the interned `Arc<str>` class name
/// = pointer-to-ArcInner, exactly what the machine-code heap-Ref PIC reads at
/// `[box_ptr + Object::class_offset]` — every lambda object shares this one Arc).
/// Populated by the FIRST `Vm::lambda_pic_fill`; read at RUNTIME by the λ-PIC key
/// select (`emit_lambda_key_select`) from this stable static address — never baked
/// into machine code. 0 until populated → the select's equality can never fire (a
/// live class word is a non-null pointer), so pre-populate dispatch is byte-for-byte
/// today's class-word probe (a miss on lambdas — always correct, only slower). The
/// interner keeps the "$Lambda" Arc alive for the Vm's lifetime, so the published
/// word can never dangle.
#[cfg(feature = "next-gen-gc")]
pub(crate) static LAMBDA_CLASS_WORD: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

/// PIC-aware callback_heavy weighting: DEFAULT ON (opt-out `RAVA_NO_PIC_HEUR`).
/// User-bytecode virtual/interface sites count as native work (the dispatch PIC
/// serves them compiled→compiled) instead of refusing to compile the caller.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn pic_heur_enabled() -> bool {
    ic_gate(IG_PIC_HEUR)
}

/// Wide-param dispatch PIC: DEFAULT ON (opt-out `RAVA_NO_WIDE_PIC`). (D)D-style
/// call sites get the machine-code IC (slot-laid buffer) instead of the
/// `rava_invoke` callback. This switch covers the whole feature, the owner
/// relaxation below included — `wide_site_pic_ok` short-circuits on it.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn wide_pic_enabled() -> bool {
    ic_gate(IG_WIDE_PIC)
}

/// ⭐**THE NATIVE-SERVED OWNER REFUSAL, AND WHY ITS PREMISE IS GONE.**
/// DEFAULT ON; `RAVA_NO_WIDE_PIC_NATIVE=1` restores the refusal exactly.
///
/// The rule this replaces was: a wide-param site is admitted to the PIC ONLY if its declared
/// owner is user bytecode, because "a JDK/native-served owner can never be IC-cached
/// (`dispatch_resolve_cacheable` excludes it), so a PIC there turns every call into the pricier
/// `dispatch_miss`" — measured RingScan 38.6 s → 53.5 s. Emit-time, permanent, no cache to warm:
/// such a site went to `emit_invoke` → `rava_invoke` → `jit_call` (a `Vec<Value>` and a synthetic
/// frame) for the life of the process.
///
/// **THE PREMISE HAD TWO HALVES AND BOTH HAVE FAILED.**
///
/// 1. *"…can never be IC-cached."* `dispatch_resolve_cacheable_cls` is keyed on the RECEIVER's
///    class, never on the site's declared owner (`vm/dispatch.rs`: `cls`, plus the dense
///    `class_id` fast form). `native_served_namespace(owner)` is therefore a PROXY, and it is
///    wrong exactly where the PIC is worth most: an `invokeinterface` declared on a `java/*`
///    functional interface has a lambda or a guest receiver, which resolves `Compiled`/`NotYet`
///    and would cache a real machine-code body. One predicate answering a question about the
///    receiver by looking at the owner — the shape this engine has been caught by before.
///
/// 2. *"…so a PIC there turns every call into the pricier `dispatch_miss`."* That WAS the
///    mechanism of the 38.6 → 53.5 regression: with the verdict permanently `-1`, the emitted
///    probe found the negative entry, threw it away on `fp > 0`, and every call left machine code
///    for `rava_dispatch_miss` (worker cascade, devirt census, `dispatch_miss_resolve`, the fills,
///    the dispatch-table publish) before reaching the same `vm.jit_call` that `rava_invoke` would
///    have reached directly. `pic_native` ended that: the miss handler now REPLACES the `-1` with
///    a leaked descriptor, the same four compares HIT, and the arm calls a Rust thunk. For a wide
///    site the thunk is always `nt_rooted` (`leaf_op`/`leaf_op_ref` name no wide descriptor), and
///    `nt_rooted` IS `vm.jit_call` — i.e. the steady state after binding is `rava_invoke`'s work
///    minus the resolve, not `rava_dispatch_miss`'s.
///
/// **AND THE REFUSAL WAS ALREADY AN ANOMALY RATHER THAN A POLICY.** `wide_site_pic_ok` returns
/// `true` unconditionally for a NARROW descriptor, so every non-`fu_routable` narrow site with a
/// `java/*` owner has taken the PIC since it existed — a population an order of magnitude larger.
/// The single structural difference for a wide site is that `emit_dispatch_pic` builds a second,
/// PACKED copy of the cells for the miss/native ABI (the hit buffer is slot-laid); that is
/// `nargs` stores in front of a host call costing hundreds of ns. The rest of the delta against
/// `emit_invoke` is the 4-way probe — `emit_invoke` already does the identical `spill_for_gc` /
/// `reload_after_gc`, so the rooted native arm adds no rooting the old path did not pay.
///
/// **THE GATE IS COUPLED TO THE MECHANISM THAT INVALIDATED THE PREMISE, NOT MERELY TO A FLAG.**
/// Both halves of the typed native entry must be live or the old regression comes straight back:
///   * `pic_native::ref_enabled()` — implies `enabled()`, so the ARENA half (`try_fill`) and the
///     HEAP-REF half (`try_fill_ref`) can both bind. A `java/*` receiver is overwhelmingly a
///     handle, so with `RAVA_NO_EMIT_PICNAT2=1` its `-1` would never be bound and every call
///     would be a `rava_dispatch_miss` again — the measured shape, exactly.
///   * `heappic::emit_mode() == 2` — without the live heap-Ref arm a handle receiver is not even
///     probed: `emit_dispatch_pic` branches a non-arena receiver straight to `miss`.
/// All three are process constants read once, before the first site compiles, so the emit-time
/// verdict cannot change under a live pool base or a live `pool_span()` immediate.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn wide_pic_native_enabled() -> bool {
    // The conjunction itself is unchanged and now lives in `raw_ic_gate(IG_WIDE_PIC_NATIVE)`;
    // `wide_site_pic_ok` reads it right after `wide_pic_enabled()`, so the pair is one load.
    ic_gate(IG_WIDE_PIC_NATIVE)
}

/// The wide-PIC eligibility test for one call site (see `wide_pic_enabled`).
///
/// A missing `member_ref` still refuses: the site's owner is unknown, so no premise about it can
/// be checked, and refusing is what this returned before.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn wide_site_pic_ok(pool: &ConstantPool, idx: u16, desc: &str) -> bool {
    if !has_wide_params(desc) {
        return true; // non-wide sites keep their existing PIC unconditionally
    }
    if !wide_pic_enabled() {
        return false;
    }
    let Some(m) = pool.member_ref(idx) else { return false };
    // ⚠**AUDITED DEAD, AND DELIBERATELY KEPT.** Every caller of this predicate
    // (`invoke_virtual` 0xb6, `invoke_dynamic`'s 0xb9 arm, and the guarded-inline slow-arm
    // closures inside both) sits AFTER an unconditional `fu_routable` early return that emits
    // `env.fu_invoke` and returns, inside the same `!arena_inline_disabled()` block that gates
    // the PIC decision itself — so a routable site can never reach here, and when the block is
    // disabled neither branch runs. Removing the clause would be behaviour-identical; keeping it
    // costs nothing and keeps the refusal true by construction rather than by reachability.
    if fu_routable(m.owner, m.name, m.descriptor) {
        return false;
    }
    if !crate::opcodes::native_served_namespace(m.owner) {
        return true;
    }
    // Native-served DECLARED owner. Admitted only once the typed native entry can bind the `-1`
    // this site's cache will hold — see `wide_pic_native_enabled` for why that is the whole of
    // the original refusal's justification.
    wide_pic_native_enabled()
}

/// ⚙**PIC-ELIGIBILITY PROBE (`RAVA_PIC_ELIG=1`, default OFF, emit-time only).**
///
/// Names, once per distinct `(owner, name, descriptor)`, which branch an
/// invokevirtual/invokeinterface site took at the PIC decision and WHICH gate refused. The
/// census in `devirt.rs` can only see sites that reach `rava_dispatch_miss`, i.e. sites that
/// HAVE a 4-way cache; a site that never emits one is invisible there and indistinguishable
/// from a site that emits one and never misses. This separates those two.
///
/// Read at emit time only, behind a `OnceLock`, and it prints — so it is never on a hot path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn pic_elig_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PIC_ELIG").is_some())
}

/// Distinct members already reported, so a member compiled into hundreds of callers prints once.
#[cfg(feature = "next-gen-gc")]
static PIC_ELIG_SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

/// Record one site's PIC decision. `kind` is 1 (virtual) or 3 (interface).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn pic_elig_note(
    kind: u8,
    owner: &str,
    name: &str,
    desc: &str,
    arena_ok: bool,
    wide_ok: bool,
    slots_ok: bool,
) {
    if !pic_elig_enabled() {
        return;
    }
    let key = format!("{kind}|{owner}.{name}{desc}");
    {
        let mut g = PIC_ELIG_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let seen = g.get_or_insert_with(std::collections::HashSet::new);
        if !seen.insert(key) {
            return;
        }
    }
    let verdict = if arena_ok && wide_ok && slots_ok {
        "PIC"
    } else if !arena_ok {
        "INVOKE (arena_inline_disabled)"
    } else if !wide_ok {
        "INVOKE (wide_site_pic_ok refused)"
    } else {
        "INVOKE (arg slots > callbuf)"
    };
    let kindname = if kind == 3 { "iface" } else { "virt" };
    eprintln!(
        "[PIC-ELIG] {kindname} {owner}.{name}{desc} -> {verdict} \
         (arena_ok={arena_ok} wide_ok={wide_ok} slots_ok={slots_ok} \
         slots={} callbuf={})",
        crate::jit::arg_slot_widths(desc).iter().sum::<usize>(),
        dispatch_callbuf_slots()
    );
}

/// 11.1 — Per-Thread Inline Caches (Path A), STEP 1: metadata parametrisation, INERT.
///
/// Records one entry per emitted dispatch IC site so the eventual per-thread IC table can address
/// each site by a fixed offset instead of the single shared baked `cache_ptr` (the G5 wall — 10.2c:
/// workers torn-write the shared cache). `ic_offset` is the site's byte offset into that (future)
/// per-thread table; it is assigned MONOTONICALLY + globally at emit/link time and never moves.
///
/// STRIDE = the FULL per-site cache width (4 ways × (class_id i64, fptr i64) = 64 B = size of the
/// baked `[i64; 8]`), NOT 16 B. 16 B is a single (class_id, fptr) pair — one PIC way — so a 16 B
/// stride would overlap adjacent sites' ways in the future per-thread table. Reserving the whole
/// cache block per site keeps the slices disjoint. Derived from the array type so it tracks changes.
///
/// INERT this step: the codegen still bakes the shared global `cache_ptr` (machine code unchanged);
/// `ic_offset` is only computed + stored. A later increment swaps the baked address for
/// `per_thread_ic_base + ic_offset`.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
pub(crate) struct IcSiteMeta {
    /// Shared baked cache address the machine code currently uses (still authoritative this step).
    cache_ptr: usize,
    /// Fixed monotonic byte offset into the future per-thread IC table (inert).
    ic_offset: u32,
}

/// Full per-site IC cache width in bytes (4-way PIC of (class_id i64, fptr i64) pairs).
#[cfg(feature = "next-gen-gc")]
// 16-way per-thread sites (was [i64;8] = 4-way): the megamorphic density sites carry 5-15
// receiver classes, so a 4-way worker table evicted forever and EVERY dispatch exited machcode
// (~8µs Rust callback ×36M = the measured wall). The GLOBAL per-site cache stays a 4-way
// [i64;8] box — only the worker arm probes 16 ways (wide-PIC on the serial path regressed).
pub(crate) const IC_SITE_STRIDE: u32 = std::mem::size_of::<[i64; 32]>() as u32;
/// Bytes of one GLOBAL per-site cache ([i64;8] box) — the seed copies THIS much (the global is
/// only 64 B; copying the full 256 B stride would read out of bounds).
pub(crate) const IC_GLOBAL_BYTES: usize = std::mem::size_of::<[i64; 8]>();
/// Byte offset, within the GLOBAL per-site box, of the direct-mapped table pointer. It sits
/// immediately after the 4 ways, past `IC_GLOBAL_BYTES`, so per-thread seed copies never touch it.
/// Zero means no positive resolved fill has allocated a table for this site.
#[cfg(feature = "next-gen-gc")]
pub(crate) const DISPATCH_TABLE_SLOT: i32 = IC_GLOBAL_BYTES as i32;
/// Worker-arm way count (16 × (key,fptr) pairs fill the 256 B stride exactly).
pub(crate) const IC_WORKER_WAYS: usize = 16;
/// Global monotonic IC-site table. Index = allocation order; each site's offset is index * stride.
#[cfg(feature = "next-gen-gc")]
pub(crate) static IC_SITES: std::sync::Mutex<Vec<IcSiteMeta>> = std::sync::Mutex::new(Vec::new());

/// Allocate the next monotonic IC-site offset for a freshly emitted site and record its metadata.
/// Returns the assigned offset. The current code still bakes the shared `cache_ptr`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ic_site_alloc(cache_ptr: usize) -> u32 {
    let mut sites = IC_SITES.lock().unwrap_or_else(|e| e.into_inner());
    let ic_offset = (sites.len() as u32).wrapping_mul(IC_SITE_STRIDE);
    sites.push(IcSiteMeta { cache_ptr, ic_offset });
    if ic_gate(IG_OFFSET_TRACE) {
        eprintln!("[IC-OFFSET] site#{} cache_ptr={:#x} ic_offset={} (INERT)", sites.len() - 1, cache_ptr, ic_offset);
    }
    ic_offset
}

/// 11.2 — Per-Thread Inline Caches (Path A), STEP 2: PHYSICAL per-worker IC table + snapshot seeding.
///
/// A worker's private, flat, 64-B-aligned block of `IC_SITES.len() * IC_SITE_STRIDE` bytes: one
/// disjoint 64-B slot per dispatch site, addressed by the site's `ic_offset` (11.1). It is the
/// substrate that a later increment will bake into the machine code (`per_thread_ic_base + ic_offset`)
/// so workers stop torn-writing the shared global cache (the 10.2c G5 abort). RAII: `Drop` frees the
/// block, so a worker thread that terminates without an explicit `worker_tlab_clear` never leaks.
///
/// INERT this step: the table is allocated + seeded + freed on the worker lifecycle, but NOTHING reads
/// it — codegen still bakes the shared `cache_ptr`, so machine code + dispatch are byte-identical.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct PerThreadIc {
    base: *mut u8,
    layout: std::alloc::Layout,
}

#[cfg(feature = "next-gen-gc")]
impl Drop for PerThreadIc {
    fn drop(&mut self) {
        if !self.base.is_null() {
            // SAFETY: `base`/`layout` are the exact pair returned by `alloc_zeroed` in the seeder.
            unsafe { std::alloc::dealloc(self.base, self.layout) };
        }
    }
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// This worker thread's private IC table (`None` on the main thread / outside a region). The
    /// owning `PerThreadIc` frees on reassignment AND on thread-local teardown (thread exit) — the
    /// no-leak guarantee for scoped pool workers, which exit via `leave_region` without a clear.
    static PER_THREAD_IC: std::cell::RefCell<Option<PerThreadIc>> = const { std::cell::RefCell::new(None) };
}

/// Allocate + SNAPSHOT-SEED this worker's per-thread IC table. Size = `IC_SITES.len() * stride`.
/// Copies each site's CURRENT shared global cache (64 B) into `base + ic_offset`, giving the worker a
/// valid warmed slice at region entry (main is parked in the scope join, so the snapshot is stable).
/// Frees any prior table first (reassignment drops it). No-op when there are no sites. INERT: unread.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ic_worker_table_init() {
    // Snapshot the (cache_ptr, ic_offset) pairs under the lock, then release it BEFORE the memcpy so
    // concurrently-entering workers don't serialise on the copy loop.
    let (pairs, size): (Vec<(usize, u32)>, usize) = {
        let sites = IC_SITES.lock().unwrap_or_else(|e| e.into_inner());
        let size = sites.len() * IC_SITE_STRIDE as usize;
        (sites.iter().map(|s| (s.cache_ptr, s.ic_offset)).collect(), size)
    };
    if size == 0 {
        return;
    }
    let layout = match std::alloc::Layout::from_size_align(size, 64) {
        Ok(l) => l,
        Err(_) => return,
    };
    // SAFETY: non-zero size; alloc_zeroed gives a valid or null block.
    let base = unsafe { std::alloc::alloc_zeroed(layout) };
    if base.is_null() {
        return; // OOM → stay inert (nothing reads the table)
    }
    // 11.12 (RAVA_M3_IC_COLD): skip the snapshot-seed → each worker starts with a ZEROED per-thread
    // PIC and warms its own 4-way from scratch via dispatch_miss under the re-entrant VM_RESOLVE_LOCK.
    // This excludes any main-thread-specific baked cache address from entering worker dispatch (the
    // Handle-space-aliasing hypothesis). Default OFF = seeded (11.2 behaviour).
    // SHAPE BEFORE: a RAW `std::env::var_os` — on Windows/MSVC a UTF-16 encode of the key into a
    // fresh heap buffer plus a linear walk of the WHOLE process environment block under a lock —
    // once per per-thread IC table BUILD. POPULATION: every `worker_tlab_init` /
    // `worker_tlab_view` on every pool worker, i.e. once per worker per GIL-free region entry,
    // and a boot enters regions continuously (the column / chunk-fill forks). Now a bit of
    // `ic_gates()`, which is read ONCE here and reused by the trace test at the bottom.
    let w = ic_gates();
    let cold = ic_gate_of(w, IG_IC_COLD);
    if !cold {
        for (cache_ptr, ic_offset) in &pairs {
            // SAFETY: each shared cache is a leaked [i64; 8] (64 B); the dest slot is within `size` and
            // disjoint per site (monotonic non-overlapping offsets from 11.1).
            unsafe {
                std::ptr::copy_nonoverlapping(
                    *cache_ptr as *const u8,
                    base.add(*ic_offset as usize),
                    IC_GLOBAL_BYTES, // the global box is 64 B (4-way); ways 4..16 stay zeroed
                );
            }
        }
    }
    PER_THREAD_IC.with(|c| *c.borrow_mut() = Some(PerThreadIc { base, layout })); // drops any prior
    // Second use of the word already loaded above — no second static, no second acquire load.
    if ic_gate_of(w, IG_OFFSET_TRACE) {
        eprintln!("[IC-PT] worker table {} sites, {} B @ {:p} ({})", pairs.len(), size, base, if cold { "COLD" } else { "seeded" });
    }
}

/// Eagerly free this worker's per-thread IC table (region exit via `worker_tlab_clear`). Safe if
/// none is set; the RAII `Drop` is the backstop for paths that never call this.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ic_worker_table_free() {
    PER_THREAD_IC.with(|c| *c.borrow_mut() = None);
}

/// The base of this thread's per-thread IC table (null if unset).
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn per_thread_ic_base() -> *mut u8 {
    PER_THREAD_IC.with(|c| c.borrow().as_ref().map_or(std::ptr::null_mut(), |p| p.base))
}

/// 11.3 gate (RAVA_M3_IC_PARALLEL=1, default OFF). When OFF, `emit_dispatch_pic` bakes the shared
/// global `cache_ptr` exactly as before (machine code byte-identical). When ON, each site emits a
/// runtime branch: a pool worker (`per_thread_ic_base() != 0`) reads/writes its OWN table slice at
/// `base + ic_offset`; the main thread (base == 0) still uses the global cache. Serial execution
/// (no workers) always takes the global arm ⇒ hashes are bit-identical.
#[cfg(feature = "next-gen-gc")]
/// ⚠MUTUALLY EXCLUSIVE WITH THE DISPATCH TABLE, and the exclusion is silent.
/// `dispatch_table::note_serve` and `note` both refuse to fill while this is on
/// (under it `cache_eff` is a per-thread 16-way slice whose word 8 is a real PIC
/// way, so a table pointer written there would corrupt dispatch) — so arming
/// this DISABLES a feature measured at −19.9% on the main-thread compiled path.
/// Priced on fixed work: ChunkFill 1×64, n=7, **+32.61%, 0/7 pairs**, most of
/// which is the table it switched off rather than the per-thread ICs themselves.
/// Re-price both together before drawing any conclusion about either.
#[inline]
pub(crate) fn ic_parallel_enabled() -> bool {
    ic_gate(IG_PARALLEL)
}
