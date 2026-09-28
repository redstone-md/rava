//! Every arena feature gate, read once and memoised.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;

// ── ONE PLAIN LOAD PER OPERATION FOR EVERY ARENA GATE ────────────────────────────────
//
// ⭐**KILL SWITCH `RAVA_NO_NG_GATEWORD=1` (also off under `flags::emit_more_disabled()`),
// DEFAULT ON.** Every accessor below keeps its historic `OnceLock` body verbatim as the
// OFF arm, so the A/B compares this round against the previous one and not a straw man.
//
// WHAT WAS WRONG. Each of the 23 gates in this file was its own `OnceLock<bool>`, and
// `OnceLock::get_or_init` opens with `state.load(Acquire)`. An acquire load is not a load
// LLVM may forward or hoist: EarlyCSE only caches loads for which `isUnordered()` holds,
// and an ordered atomic load **bumps the pass's generation counter**, discarding the loads
// it had already cached. So a path that consults N of these gates pays N barriers AND
// re-materialises the receiver, the class word and the cell base around each one. The same
// finding has now been priced three times in this tree: five acquire loads in front of a
// six-instruction nursery bump (`fast_birth3`), three in front of the arena field accessor
// (`classes::gates_once`), and four to serve one cached `u32`.
//
// HOW MANY, AND ON WHAT POPULATION — counted by reading the call sites, not sampled:
//
//   * PER ARENA REF CELL WRITE (`opcodes::arena_cells::encode_ref_cell` + its putfield
//     tail): `pinned_ptrs_enabled` on the `Value::Ref` arm, `arena_str_refcell_enabled` on
//     the `Value::Str` arm, then `shadow_enabled` on the card-barrier tail — **2 acquire
//     loads per store, now 1 plain TLS load shared with everything else in the caller.**
//   * PER HANDLE FIELD ACCESS (`heap::fields`): `pinned_read_enabled` on the read tail,
//     `pinned_mirror_enabled` on the write tail, `shadow_enabled` on the barrier —
//     **1-2 acquire loads per access → 0.**
//   * PER JIT→HOST FIELD CALLBACK (`jit::callbacks::fields::arena_slot_note`, entered by
//     every arena `getfield`/`putfield` the emitter declined to inline — 36.8 M arrivals in
//     the boot row 326 measured): `pinned_ptrs` + `pinned_read` + `shadow` back to back —
//     **3 acquire loads per callback → 1 plain load, and the three tests become
//     `and`/`jz` against one register.**
//   * PER TRACED OBJECT / PER EVACUATION (`nextgen::collect`, `nextgen::genheap`):
//     `fwd_log_enabled` per `copy_obj` edge, `dfs_copy_enabled` per drain, `poison_enabled`
//     + `gc_stats_enabled` per collection — **1 acquire load per copied object → 0.**
//   * PER LAMBDA BIRTH (`nextgen::lambdas`, 2.885 M births measured): `gc_stats_enabled`
//     three times plus `arena_lambda_enabled` — **4 acquire loads per birth → 1 plain load
//     that LLVM can and does forward across all four tests.**
//   * PER OBJECT BIRTH (`opcodes::alloc`, `vm::arena_alloc`, `vm::arena_by_name`):
//     `arena_enum_enabled` + `arena_synth_enabled` + `birth_ns_guard_enabled` on the same
//     birth — **2-3 acquire loads per `new` → 1.**
//
// THE SHAPE. One `u32`, one bit per gate, computed once per process from the environment
// and cached in a per-thread `Cell<u32>` whose initialiser is a `const` block and whose type
// has no destructor — the one combination `std` lowers to a bare `#[thread_local]` static,
// i.e. a TLS-slot load with no init check, no registration and nothing to call. It is a
// PLAIN load, so unlike the `OnceLock` it CSEs: a caller that tests four gates emits one
// load and four `test`s, and it survives being hoisted out of a loop.
//
// WHY THE WORD IS NOT MERELY A SHARED `OnceLock<u32>` (which is what `classes::gates()`
// still is): that would still be one acquire barrier per *function*, and it would still
// clobber the caller's cached loads once. The per-thread cell removes the barrier itself.
//
// AN ARMED INSTRUMENT DOES NOT MAKE THIS MORE EXPENSIVE, AND IT DOES NOT MAKE IT CHEAPER
// EITHER — it is the same load and the same bit test either way. The six diagnostic gates
// (`shadow`, `fwd_log`, `gc_stats`, `poison`, `bare_verify`, `pinned_verify`) share the
// word with the production gates deliberately, so arming one costs zero extra loads on the
// paths that also read a production gate. Routing an armed instrument to a separately
// compiled outline is the remaining half of that idea and belongs at the CALL SITES, which
// live in other modules — recorded as a hand-off, not done here.
//
// FREEZE POINT. The first read of ANY arena gate now freezes ALL of them. That is safe
// because the only writer of these variables is `rava-cli`'s `apply_clean_aliases`, which
// runs before the VM is constructed and therefore before any arena gate can be consulted;
// nothing in the engine or its tests calls `set_var` on a name in this file.

/// Bit 0, always set in a resolved ENABLED word, so "resolved" is simply "non-zero" and an
/// all-gates-off boot is still distinguishable from an unresolved slot.
const G_READY: u32 = 1 << 0;
const G_SHADOW: u32 = 1 << 1;
const G_DECOMMIT: u32 = 1 << 2;
const G_PIN_MIRROR: u32 = 1 << 3;
const G_PIN_VERIFY: u32 = 1 << 4;
const G_PIN_READ: u32 = 1 << 5;
const G_PIN_PTRS: u32 = 1 << 6;
const G_STR_REFCELL: u32 = 1 << 7;
const G_BIRTH_NS: u32 = 1 << 8;
const G_SYNTH: u32 = 1 << 9;
const G_NATIVE_NEW: u32 = 1 << 10;
const G_LAM_SINGLETON: u32 = 1 << 11;
const G_LAM_DEDUP: u32 = 1 << 12;
const G_LAMBDA: u32 = 1 << 13;
const G_DFS_COPY: u32 = 1 << 14;
const G_BARE_STORE: u32 = 1 << 15;
const G_BARE_VERIFY: u32 = 1 << 16;
const G_BARE_REF_LOAD: u32 = 1 << 17;
const G_ARENA_ENUM: u32 = 1 << 18;
const G_ARENA_RECORD: u32 = 1 << 19;
const G_PIN_ENUM: u32 = 1 << 20;
const G_FWD_LOG: u32 = 1 << 21;
const G_GC_STATS: u32 = 1 << 22;
const G_POISON: u32 = 1 << 23;
/// Sign bit: the kill switch is engaged. Chosen so that ONE signed compare separates all
/// three slot states — `w as i32 > 0` = resolved and folded, `w == 0` = unresolved,
/// negative = resolved and disabled — instead of two tests on the shipping path.
const G_WORD_OFF: u32 = 1 << 31;

thread_local! {
    /// This thread's copy of the process gate word. `Cell<u32>` is `Copy`, has no
    /// destructor and its initialiser is a `const` block, so `std` emits a bare TLS slot:
    /// reading it is a load, not a `LocalKey::with` call and not an atomic.
    static NG_GW: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The word, computed once per process. Called at most once per THREAD (the cold fill
/// below caches the result), never per operation.
fn ng_word_proc() -> u32 {
    static W: OnceLock<u32> = OnceLock::new();
    *W.get_or_init(|| {
        if std::env::var_os("RAVA_NO_NG_GATEWORD").is_some() || crate::flags::emit_more_disabled() {
            return G_WORD_OFF;
        }
        let some = |k: &str| std::env::var_os(k).is_some();
        let mut w = G_READY;
        // Each arm is the SAME predicate as the accessor's own `OnceLock` body below. A
        // process constant read twice cannot disagree, so the switch selects only WHERE the
        // answer is read, never what it says — nothing observable can differ.
        if some("RAVA_SHADOW_GC") {
            w |= G_SHADOW;
        }
        if some("RAVA_ARENA_DECOMMIT") {
            w |= G_DECOMMIT;
        }
        if some("RAVA_PINNED_MIRROR") {
            w |= G_PIN_MIRROR;
        }
        if some("RAVA_PINNED_VERIFY") {
            w |= G_PIN_VERIFY;
        }
        if some("RAVA_PINNED_READ") {
            w |= G_PIN_READ;
        }
        if some("RAVA_PINNED_PTRS") {
            w |= G_PIN_PTRS;
        }
        if !some("RAVA_NO_ARENA_STR_REFCELL") {
            w |= G_STR_REFCELL;
        }
        if !some("RAVA_NO_ARENA_BIRTH_NS_GUARD") {
            w |= G_BIRTH_NS;
        }
        if some("RAVA_ARENA_SYNTH") {
            w |= G_SYNTH;
        }
        if some("RAVA_ARENA_NATIVE_NEW") {
            w |= G_NATIVE_NEW;
        }
        if !some("RAVA_NO_ARENA_LAMBDA_SINGLETON") {
            w |= G_LAM_SINGLETON;
        }
        if !some("RAVA_NO_ARENA_LAMBDA_DEDUP") {
            w |= G_LAM_DEDUP;
        }
        if !some("RAVA_NO_ARENA_LAMBDA") {
            w |= G_LAMBDA;
        }
        if some("RAVA_GC_DFS_COPY") {
            w |= G_DFS_COPY;
        }
        if !some("RAVA_NO_BARE_STORE") {
            w |= G_BARE_STORE;
        }
        if some("RAVA_BARE_VERIFY") {
            w |= G_BARE_VERIFY;
        }
        if some("RAVA_JIT_BARE_REF_LOAD") {
            w |= G_BARE_REF_LOAD;
        }
        // `RAVA_PINNED_ENUM` implies the arena-enum machinery even against the opt-out —
        // the accessor below carries the same disjunction, unchanged.
        if some("RAVA_PINNED_ENUM") || !some("RAVA_NO_ARENA_ENUM") {
            w |= G_ARENA_ENUM;
        }
        if !some("RAVA_NO_ARENA_RECORD") {
            w |= G_ARENA_RECORD;
        }
        if some("RAVA_PINNED_ENUM") {
            w |= G_PIN_ENUM;
        }
        if some("RAVA_PROBE_STALE") {
            w |= G_FWD_LOG;
        }
        if some("RAVA_GC_STATS") {
            w |= G_GC_STATS;
        }
        if some("RAVA_GC_POISON") {
            w |= G_POISON;
        }
        w
    })
}

/// The shipping read: one plain TLS load and one signed compare. `Some(w)` = test the bit;
/// `None` = the kill switch is engaged, so the caller falls back to its own historic
/// `OnceLock` verbatim and pays the acquire load exactly where it used to.
///
/// The OFF arm's one-load-plus-one-branch handicap is disclosed rather than hidden: it is
/// the price of having a runtime switch at all, and it is the same shape
/// `classes::gates_once` already ships.
#[inline(always)]
fn ng_gates() -> Option<u32> {
    let w = NG_GW.with(|c| c.get());
    if (w as i32) > 0 {
        Some(w)
    } else {
        ng_gates_cold(w)
    }
}

#[cold]
#[inline(never)]
fn ng_gates_cold(cur: u32) -> Option<u32> {
    if cur != 0 {
        return None; // resolved, and the switch is engaged — cached in the slot, not re-read
    }
    let w = ng_word_proc();
    NG_GW.with(|c| c.set(w));
    if (w as i32) > 0 {
        Some(w)
    } else {
        None
    }
}

/// RAVA_SHADOW_GC gate (default OFF): the shadow card-table + root-validation machinery is a
/// DRY-RUN the collector never reads (see the remembered-set comment in `minor_collect`) — pure
/// diagnostic. It used to run UNGATED in every measured boot (per-ref-putfield card try_lock +
/// per-GC root validation + stderr lines), a Rava-only tax with no JVM counterpart that biased
/// every benchmark. Opt-in when debugging GC root discovery.
#[inline]
pub fn shadow_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_SHADOW != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SHADOW_GC").is_some())
}

/// RAVA_ARENA_DECOMMIT gate: return a parked reserved span's committed pages to the OS
/// (`MEM_DECOMMIT`) instead of retaining them. Opt-in pending an A/B soak (bit-exact arbiter).
#[inline]
pub(crate) fn arena_decommit_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_DECOMMIT != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARENA_DECOMMIT").is_some())
}

/// B3-step-2 gate (RAVA_PINNED_MIRROR, default OFF). When set, `Heap::alloc`'s
/// pre-population choke point ALSO allocates a pinned header mirror for each
/// layout-known handle object (see `pinned_mirror_set`). Default OFF → no mirror is
/// ever allocated → the pinned region stays unmapped → canonical bit-exact. The
/// migration flips this ON only once the collector integration + field mirroring +
/// unified read path (later bricks) make a live mirror both safe and consumed.
#[inline]
pub fn pinned_mirror_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_PIN_MIRROR != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PINNED_MIRROR").is_some())
}

/// B4-validation gate (RAVA_PINNED_VERIFY, default OFF; needs RAVA_PINNED_MIRROR
/// too, else there are no mirrors). When set, handle field READS cross-check the
/// pinned mirror cell against the authoritative FieldBlob value WITHOUT changing
/// the returned value — an assert-only execution validation of the dual-store: a
/// clean canonical run proves the mirror is consistent with live execution (the
/// first non-dead verification), before a later brick flips reads onto the mirror.
#[inline]
pub fn pinned_verify_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_PIN_VERIFY != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PINNED_VERIFY").is_some())
}

/// B4 read-flip gate (RAVA_PINNED_READ, default OFF; needs RAVA_PINNED_MIRROR).
/// When set, a handle field READ returns the value decoded from the pinned mirror
/// (via the arena cell path) instead of the FieldBlob — the first EXECUTION use of
/// the unified pointer object as the read source. A clean canonical run proves the
/// mirror can SERVE reads (including that no ref cell goes stale between writes),
/// the step before references themselves become pinned pointers.
#[inline]
pub fn pinned_read_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_PIN_READ != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PINNED_READ").is_some())
}

/// B5 PRODUCTION FLIP gate (RAVA_PINNED_PTRS, default OFF; needs MIRROR+READ).
/// When set, a handle reference stored into an ARENA ref cell is encoded as its
/// raw pinned-mirror ADDRESS (low bits 0b00 — the unified pointer form) instead
/// of the tagged `(h<<2)|1`. Readers decode it as Value::Obj(pinned); the five
/// coexistence bridges (ref_equal/ref_eq, liveness mark, identity-hash, map keys)
/// make that the SAME object as Ref(h). This is the step that begins retiring the
/// per-read handle discrimination.
#[inline]
pub fn pinned_ptrs_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_PIN_PTRS != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PINNED_PTRS").is_some())
}

/// STEP 1 BOX-ARM RETIREMENT — **DEFAULT ON** (`RAVA_NO_ARENA_STR_REFCELL` opts out).
/// A `Value::Str` stored into an arena reference cell is BORN as an arena-STRING
/// object (a raw 0b00 pointer, decoded with zero side-table lookup) instead of the
/// 0b11 arena_box side-table entry. Single-store: no dual write, no box index. The
/// 0b11 box arm remains only for the StdOut/StdErr sentinels, which have no arena
/// form. `tlab_alloc_string` never triggers a collection (returns None on overflow),
/// so the receiver at the putfield callsite cannot move — GC-safe. The resulting
/// 0b00 cell lives in a ref-kind slot, so the moving collector traces + forwards it
/// like any arena ref (the 0b11 box was skipped instead).
///
/// WHY IT IS ON DESPITE MEASURING NEUTRAL. This is step 1 of
/// `docs/value-model-rewrite-plan.md`, and that plan is explicit that
/// "steps 1-5 are correctness/migration progress with NO wall win" — the payoff comes
/// only at the FINAL flip to bare dereference, once no non-0b00 cell can persist.
/// Judged as a performance knob it would not ship; judged as what it is, it retires
/// the box arm on the default path, and the box bucket is 18% of the cells blocking
/// that flip (and is 97.9% strings, so this is nearly all of it).
///
/// Re-measured on the current tree, since its original verdict was taken when a boot
/// was 193s and it is now ~80s — a lever's value is not a constant of the lever:
///
///   done  off 25.830 / 26.693 / 29.571   on 25.849 / 26.927 / 27.869   +0.07%, OVERLAP
///
/// Bit-exact on all four gates ARMED (ChunkFill 8790f04ace726ecf, RingScan/RingLane/
/// RingLaneInstance(8) 37539c6c375d0c3), and — the evidence that matters here — three
/// full boots reached Done with it on. ChunkFill alone is NOT a sufficient gate for a
/// representation change: the arena-lambda step was bit-exact on ChunkFill and still
/// killed the real boot two minutes in, during the resource reload.
#[inline]
pub fn arena_str_refcell_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_STR_REFCELL != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_STR_REFCELL").is_none())
}

/// BIRTH NAMESPACE GUARD gate (RAVA_ARENA_BIRTH_NS_GUARD, default OFF). The
/// object-BIRTH rule (`opcodes::new_object_value` + its JIT twin
/// `Vm::arena_new_site`) forces a class to a heap HANDLE when
/// `is_native_backed_alloc` says so — and that predicate's collection arm
/// (`stream::is_collection`) is a SUBSTRING test ("List"/"Set"/"Queue"/"Deque"/
/// "Collection"/"Vector"/"Stack" ANYWHERE in the name) with no namespace guard.
/// The JIT's own native-served test was fixed long ago (`native_served_namespace`
/// + `jit_native_served_deep`); the birth rule never got the same guard, so every
/// ordinary bytecode class whose name merely CONTAINS one of those tokens
/// (`…Settings`, `…Listener`, `ItemStack*`, DFU `ListCodec*`, anything ending
/// `…Set…`) is still born a handle and its reference cells stay 0b01 — blocking
/// the bare-deref flip for classes that have nothing to do with the native
/// collection layer.
///
/// When set, the BIRTH sites (and only they) evaluate the collection arm through
/// `opcodes::birth_native_backed_alloc`, which drops the substring match outside
/// the native namespaces. The exact-name arms (`is_native_map` / `is_entry_type`
/// / `is_native_io_class`) are UNGUARDED, so this subtracts exactly
/// `is_collection`'s false positives and nothing else. A GENUINE subclass of a
/// native-backed type is still forced to a handle: the ancestor scan
/// (`Vm::birth_native_backed_ancestor`) runs the same guarded predicate over the
/// WHOLE super chain, and a real `java/util/AbstractList` / `java/io/*` /
/// fastutil / guava ancestor is inside a native namespace by construction.
/// Dispatch semantics, `is_collection` itself, and the JIT's native-served logic
/// are untouched — flag OFF is byte-identical.
/// DEFAULT-ON since 2026-08-07 (opt out with `RAVA_NO_ARENA_BIRTH_NS_GUARD`) — promoted
/// with the arena-lambda group, see `arena_lambda_enabled`.
#[inline]
pub fn birth_ns_guard_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_BIRTH_NS != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_BIRTH_NS_GUARD").is_none())
}

/// LAMBDA MIGRATION gate (RAVA_ARENA_LAMBDA, default OFF). When set, a CAPTURING
/// functional-interface closure (`invokedynamic` LambdaMetafactory) is BORN in the
/// moving arena as a raw 0b00 `Value::Obj` (an `ArenaLambda`, see below) instead of
/// a 0b01 handle. Non-capturing lambdas stay per-site handle SINGLETONS — they are
/// the λ-PIC's stable-address direct-call targets (`indy_singleton_handles`,
/// lib.rs:9645), and keeping them handles keeps arena-lambdas OFF the machine-code
/// direct path entirely (an arena-lambda's class name is `[arena-lambda:…]`, never
/// `$Lambda`, and the λ-PIC's REF_TAG guard rejects non-handle receivers, so it
/// falls to the general `invoke_lambda`/`apply` dispatch — the sound coexistence
/// path). Flag OFF ⇒ no arena-lambda is ever born ⇒ byte-identical canonical.
/// ARENA CLOSURE DEDUP gate (`RAVA_ARENA_LAMBDA_DEDUP`, default OFF). The heap dedup
/// (`Vm::lambda_dedup`, default ON) stores handles, so an arena-born closure is never
/// inserted and every lookup misses: measured **2,885,078 arena-lambda births against 4,055
/// live**, a 712:1 churn, with that dedup active throughout. This arms the arena twin.
/// Default OFF until it is measured on a quiet box — it changes closure IDENTITY, which JLS
/// §15.27.4 permits but the four gates must still confirm bit-exact.
/// BORN-PINNED NON-CAPTURING LAMBDA (`RAVA_ARENA_LAMBDA_SINGLETON`, default OFF).
///
/// With arena lambdas armed the flip's largest single residue is `$Lambda`: **174,363
/// blocking cells over only 4,063 objects**. Those are the NON-capturing per-site
/// singletons, which `arena_lambda_enabled` deliberately leaves as handles because the
/// λ-PIC guards on a stable receiver address.
///
/// ★ This is the exact INVERSE of the case the pinned-closure experiment was killed on.
/// There, capturing closures showed a 2,885,078:4,055 birth-to-live churn, and a never-free
/// region cannot absorb that. Here the population IS the live set — one immortal object per
/// call site — which is precisely what `pinned_object_alloc` was built for, and a pinned
/// object's address is stable FOREVER, so it satisfies the λ-PIC's requirement more strongly
/// than a handle rather than less.
/// NATIVE-MINTED OBJECTS INTO THE ARENA (`RAVA_ARENA_NATIVE_NEW`, default OFF). Closes the
/// gap between what the `new` opcode does and what a native does: `reflect::new_instance`
/// allocates a bare heap handle and never asks the birth route, so ~150 k of the flip's
/// residue is native-minted and unreachable by every birth-route flag.
/// SYNTHETIC-STATE INJECTION gate (`RAVA_ARENA_SYNTH`, default OFF).
/// ⛔**OPT-IN. Row 436's default flip is WITHDRAWN** — see below.
///
/// A class whose state rava's own natives define (`nextgen::synthetic_layout`) is born in the
/// ARENA with those cells, instead of taking a heap handle it does not need.
///
/// ⛔⛔**AND THE REASON IS A MEASUREMENT I GOT WRONG.** Two ABBA rounds read **60.1 / 62.6 against
/// 67.3 / 65.7 — −8.52%, DISJOINT** — and I shipped a default on it. Six boots later the same
/// A/B reads **63.9 / 64.8 / 73.9 against 64.6 / 65.8 / 69.8 = −1.08%, heavily overlapping.**
/// ⇒ **The boot's wall floor is ±8%, so TWO rounds can produce a disjoint pair by luck.** The
/// old 2026-08-06 reading (~22 s of regression) is equally unsupported; the honest verdict on
/// this flag today is NEUTRAL, and it stays opt-in until an instrument can resolve it.
///
/// ⚠It is also the mechanism the handle-retirement rewrite runs on (rows 434-435): 6.2M objects
/// per boot are still born as handles, and `Map$Entry` alone is 48% of them.
#[inline]
pub fn arena_synth_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_SYNTH != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARENA_SYNTH").is_some())
}

#[inline]
pub fn arena_native_new_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_NATIVE_NEW != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARENA_NATIVE_NEW").is_some())
}

/// DEFAULT-ON since 2026-08-07 (opt out with `RAVA_NO_ARENA_LAMBDA_SINGLETON`) — promoted
/// with the group, see `arena_lambda_enabled`.
#[inline]
pub fn arena_lambda_singleton_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_LAM_SINGLETON != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_LAMBDA_SINGLETON").is_none())
}

/// DEFAULT-ON since 2026-08-07 (opt out with `RAVA_NO_ARENA_LAMBDA_DEDUP`) — see
/// `arena_lambda_enabled` for the measurement that promoted the whole group.
#[inline]
pub fn arena_lambda_dedup_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_LAM_DEDUP != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_LAMBDA_DEDUP").is_none())
}

/// DEFAULT-ON since 2026-08-07 (opt out with `RAVA_NO_ARENA_LAMBDA`).
///
/// Measured on the boot-trained production binary, 3 ABBA pairs: **peak RSS 2364 → 2101 MB
/// (−11.1%)** with wall 47.24 → 46.47 s and Done 15.66 → 15.05 — i.e. the memory win is not
/// bought with wall. Promoted together with `_DEDUP`, `_SINGLETON` and the birth NS guard,
/// which is how they were measured; splitting them afterwards would be a different arm.
///
/// ⚠ This group killed the boot once (2026-08-04: `uncaught java/lang/MatchException`, 2 of
/// 2) BEFORE the dedup and pinned-singleton work landed. It is promoted now because the boot
/// is what was checked — 3 of 3 valid runs here, plus 4/4 bit-exact gates and the suite. The
/// gates alone would NOT have caught that failure: it lives on the datapack/registry surface
/// they do not cover.
#[inline]
pub fn arena_lambda_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_LAMBDA != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_LAMBDA").is_none())
}

/// STEP 6 PAYOFF PILOT gate (RAVA_JIT_BARE_REF_LOAD, default OFF — UNSOUND in
/// general, a MEASUREMENT tool only). When set, the compiled ARENA arms of the
/// reference getfield/putfield emitters drop the low-2-bit tag discrimination:
/// a ref field load becomes a bare `load.i64 [obj+HDR+slot*8]` and a ref field
/// store a bare `store.i64`, exactly the "Bare Dereference Mode" the value-model
/// rewrite reaches only after 100% handle retirement (docs/value-model-rewrite-plan.md
/// step 6). It exists to MEASURE that endgame's prize NOW, on a program whose
/// object graph is provably 100% arena-born (every ref cell 0b00), before spending
/// the multi-session migration that would make it globally sound. On a mixed graph
/// a 0b01/0b10/0b11 cell would be returned as a wild pointer ⇒ crash — never
/// default-on, never a shipping flag.
/// COPY-ORDER gate (RAVA_GC_DFS_COPY, default OFF). Switches the minor collector's
/// evacuation from pure breadth-first Cheney to approximately-depth-first (Moon):
/// a freshly-copied object's referents are copied immediately, so parents and their
/// children land adjacent instead of ~256 KB apart (see `GenHeap::dfs_drain` for the
/// measured edge-distance histogram that motivates it). Placement only — same objects,
/// same copy count, same invariants; OFF is byte-identical to the previous collector.
#[inline]
pub fn dfs_copy_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_DFS_COPY != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GC_DFS_COPY").is_some())
}

/// Splits the flip's STORE half from its LOAD half so a failure can be attributed to one
/// of them (`RAVA_NO_BARE_STORE=1` keeps the tagged store while loads go bare). The census
/// models CELLS, and a cell census can only ever see what a store left behind — it has no
/// way to represent a load that reads a correctly-tagged cell wrongly. When residue reached
/// 37 and the boot still produced 71,276 stale receivers, that gap was the first suspect.
#[inline]
pub fn bare_store_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_BARE_STORE != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_BARE_STORE").is_none())
}

/// Shadow-verify the flip with the flip OFF (`RAVA_BARE_VERIFY`). See `rava_bare_verify`.
#[inline]
pub fn bare_verify_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_BARE_VERIFY != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_BARE_VERIFY").is_some())
}

#[inline]
pub fn bare_ref_load_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_BARE_REF_LOAD != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_BARE_REF_LOAD").is_some())
}

/// ENUM MIGRATION gate (RAVA_ARENA_ENUM, default OFF). When set, a real bytecode
/// enum class (ACC_ENUM or a super chain reaching java/lang/Enum) is BORN in the
/// arena as a raw 0b00 `Value::Obj` instead of a 0b01 handle — the `is_enum`
/// forced-handle route in `new_object_value` is bypassed. The enum's `name`/
/// `ordinal` are carried by TWO synthetic cells appended to the class layout
/// (`$enumName` = ref/kind-6, `$enumOrd` = int/kind-0 — see `class_field_layout`),
/// so the moving GC traces `$enumName` (a String ref) and skips `$enumOrd` (an int)
/// via the per-class ref_mask like any other arena object — NO new GC infra.
/// `Enum.<init>`/`ordinal`/`name`/`toString`/`compareTo` + the ordinal-switch label
/// path route through `arena_getfield`/`arena_putfield` on the two cells. Identity
/// (`==`/`if_acmpeq`) is the arena-address form already served by `ref_equal`
/// (`Obj,Obj → addr==addr`); enum constants are static roots forwarded together by
/// the moving collector, so singleton identity holds. The 0b01 handle-enum path is
/// the coexistence safety net (flag OFF = byte-identical: no cell injection, natives
/// keep the Ref-only arms). Synthetic JDK enums (RoundingMode/TimeUnit/…) that
/// `heap.alloc` a handle directly in natives are unaffected — they stay handles and
/// coexist.
/// DEFAULT-ON since the `java/lang/Enum` chain terminator made it do anything at all
/// (`RAVA_NO_ARENA_ENUM` disables). It is shipped on MEMORY, not on speed, and the
/// distinction is the honest one: across two independent 3-pair ABBA batches peak RSS was
/// −4.1% and −5.8% with the direction never reversing, while wall/`Done` separated cleanly
/// in the first batch and overlapped in the second — one batch out of two is below this
/// tree's bar for a speed claim, so no speed claim is made. Two further reasons to make it
/// the default rather than a flag: it removes 12.9% of the cells that block the bare-deref
/// flip, and a mechanism that is off by default is a mechanism the four gates never arm —
/// the flip work would otherwise always be measured on a non-default path.
#[inline]
pub fn arena_enum_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_ARENA_ENUM != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    // RAVA_PINNED_ENUM implies the arena-enum OBJECT machinery (layout injection,
    // Value::Obj routing in the enum natives / switch / JIT) — it differs only in
    // WHERE the object is born (the non-moving pinned region vs the moving arena).
    // So that flag still forces this on even against the opt-out; `pinned_enum_enabled`
    // alone decides the birth site.
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_PINNED_ENUM").is_some()
            || std::env::var_os("RAVA_NO_ARENA_ENUM").is_none()
    })
}

/// ARENA RECORD gate (`RAVA_ARENA_RECORD`, default OFF). The sibling of the enum
/// terminator: `java/lang/Record` is abstract with ZERO instance fields — a record's
/// components are declared on the record class itself, which is already in the chain —
/// so a chain that stops on an unloadable `java/lang/Record` is missing NOTHING and its
/// layout is complete. Without this the chain reads as partial, the layout is never
/// cached, and every record falls back to a heap handle (measured: 3,258 chain breaks at
/// `java/lang/Record`, the single most common terminator, and `Climate$Parameter` alone
/// holds 26,622 blocking cells over 1,979 objects).
///
/// This exact `complete = true` already existed but was gated behind `RAVA_PINNED_MIRROR`,
/// which wanted it for a different reason (no `set_layout` ⇒ no pinned mirror). Bundling
/// it there meant the arena-eligibility effect could not be measured on its own, which is
/// the same shape of mistake as the enum flag: a lever that is only reachable through
/// another lever's bundle is not a lever you can price.
/// DEFAULT-ON (`RAVA_NO_ARENA_RECORD` disables) — measured together with the enum
/// terminator, same evidence and same reasoning as `arena_enum_enabled`. It clears a
/// further 4.1% of blocking cells (`Climate$Parameter` alone was 26,622 over 1,979
/// objects); the two together take the flip's blocker population from 945,595 to 789,989,
/// bare-ready 68.71% → 74.00%.
#[inline]
pub fn arena_record_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_ARENA_RECORD != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_RECORD").is_none())
}

/// BORN-PINNED ENUM gate (RAVA_PINNED_ENUM, default OFF). Implies `arena_enum_enabled`
/// (an enum is a first-class arena OBJECT with injected $enumName/$enumOrd cells), but
/// the constant is BORN in the non-moving PINNED region (`pinned_object_alloc`, 0b00,
/// never copied by the moving Cheney collector) instead of the moving young arena. Enum
/// constants are immortal singletons, so the pinned bump allocator's never-free policy
/// is a perfect fit — and their stable address makes `==`/`if_acmpeq` identity even
/// firmer than a moving-arena object (which relies on the collector forwarding all
/// references together). The moving GC must still trace INTO a pinned enum's ref cells
/// (its $enumName / declared ref fields can point at moving-arena objects) — see
/// `pinned_region_root_walk`, hooked into `arena_collect_jit`'s root phase. This
/// RECOVERS the STEP-3 interim cost: a moving-arena enum is copied every GC (+130MB
/// peakWS / +1.8s boot); a born-pinned enum is never copied.
#[inline]
pub fn pinned_enum_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_PIN_ENUM != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PINNED_ENUM").is_some())
}

/// Variant B gate (RAVA_M3_ARENA_COLLECTIONS=1). Default OFF.
#[allow(dead_code)]
pub fn arena_collections_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_M3_ARENA_COLLECTIONS").ok().as_deref() == Some("1"))
}

/// RAVA_PROBE_STALE — forward-log forensic (3c stackful stale-receiver hunt).
/// `copy_obj` records every evacuation `old→new` here; when a later `arena_getfield`
/// hits a STALE receiver it asks `lookup_fwd`: in the log ⇒ the object WAS forwarded
/// but some root location wasn't written back (a WRITEBACK MISS); absent ⇒ it was
/// NEVER a root (freed as garbage — an in-flight transient). Poison-proof (this is a
/// separate record, not the forwarding word poison clobbers). Capped so the diagnostic
/// can't grow unbounded; clearing loses only the oldest history (the stale event's
/// forward is recent).
#[inline]
pub fn fwd_log_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_FWD_LOG != 0;
    }
    use std::sync::OnceLock;
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("RAVA_PROBE_STALE").is_some())
}
/// Is collector accounting on? (`RAVA_GC_STATS=1`.)
#[inline]
pub fn gc_stats_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_GC_STATS != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GC_STATS").is_some())
}

/// DIAGNOSTIC (RAVA_GC_POISON): after a collection resets the nursery, overwrite the
/// whole young region with a sentinel (`0xDEADBEEF_DEADBEEF` per 8 bytes). A pointer
/// that a JIT register held across the collection (and was NOT reloaded) then derefs
/// into poison: `obj_class` reads `0xDEADBEEF` (an absurd class id → `class None`)
/// DETERMINISTICALLY, instead of sometimes-valid bytes of a slot a later alloc reused.
/// Off by default (one env probe); when on, costs a memset of the nursery per GC.
#[inline]
pub(crate) fn poison_enabled() -> bool {
    if let Some(w) = ng_gates() {
        return w & G_POISON != 0;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GC_POISON").is_some())
}
