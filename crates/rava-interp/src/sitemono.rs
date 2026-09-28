//! **THE RECEIVER PROFILE, CHEAP ENOUGH TO SHIP — AND NOW SOUND ENOUGH TO WRITE FROM COMPILED
//! CODE.**
//!
//! `jit::emit::guarded_inline` needs one fact per call site at compile time: *has this site ever
//! seen more than one receiver class?* The existing `devirt` probe answers it, but it takes a
//! global `Mutex` on **every interpreted `invokevirtual`** — it was built as read-only telemetry
//! for an LLVM experiment, and with it armed a product boot no longer reaches `Done` inside the
//! measurement window. An optimisation whose profile costs more than the optimisation saves
//! cannot be measured, let alone shipped.
//!
//! So this table is written from a different place: `tlcache::obj_inline_put` / `ref_inline_put`,
//! which run on a **cache MISS** — once per (site, class) pair per thread, not once per call. A
//! monomorphic site writes here exactly once in its life and is then free forever.
//!
//! ## ⭐**WHY THIS FILE CHANGED: THE PRODUCT FLAG SET COMPILES ON FIRST ENTRY.**
//!
//! With `RAVA_JIT_TIER0=1` a method is compiled the first time it is entered, so at the moment
//! the emitter runs, that method's own call sites **have never executed** and this table is
//! empty for every one of them. Every reader above is therefore answered `None` by construction,
//! and guarded devirtualisation is dead on arrival. The cure is a SECOND compile with a profile
//! in hand — which means this table has to accept writes from **compiled code, on any thread,
//! concurrently with the interpreter's writes and with an emitter reading it**. It did not.
//!
//! Four things were wrong for that, and all four are fixed below without growing the table:
//!
//!  1. **The identity was published in two independent stores.** `caller` and `cp` were separate
//!     atomics claimed by "is `caller` zero?", so two threads claiming the same free slot both
//!     proceeded and the surviving row could be **spliced from two different sites** — caller
//!     from one, cp from the other, payload from either. Now the identity is ONE `u64`
//!     (`caller | cp<<48`) claimed by ONE `compare_exchange`: exactly one writer ever owns a
//!     slot's payload, and the loser goes silent. (An x86-64 user-mode pointer is below 2^47 and
//!     an `Arc<LoadedClass>` is 8-aligned, so the top 16 bits are free for `cp`; a caller that
//!     somehow had them set is simply refused, at both ends, and goes unprofiled.)
//!  2. **The payload was published with no ordering at all.** Payload stores are still relaxed,
//!     but the store that makes a slot READABLE (`distinct`) is now a RELEASE and every reader
//!     ACQUIRES it before touching the payload. On x86-64 both are plain `mov` — this costs
//!     nothing and buys the guarantee that a reader which sees `distinct == 1` sees the `word`,
//!     `kind` and `decl` that go with it, rather than a half-built row.
//!  3. **Poison did not survive a race.** `distinct` was bumped by a load-then-store pair, so a
//!     writer that had loaded `1` could store `2` **over** a `3` written by `poison` in between —
//!     re-arming a site whose speculation had already deoptimised. `distinct` is now MONOTONE
//!     (`fetch_max` / a saturating `fetch_update`), so 3 is absorbing: once poisoned, always
//!     poisoned. `poison` also CLAIMS a free slot as a tombstone, so a deopt cannot be forgotten
//!     merely because the profile that caused it was evicted by a collision.
//!  4. **A read of a never-written table cost 2 MiB of committed memory.** `mono_*` materialised
//!     the whole table on the first *read*. Under tier-0 that is the norm: the emitter asks about
//!     thousands of sites before anything has ever been profiled. Reads now use `OnceLock::get()`
//!     and answer `None` after one acquire load, touching nothing.
//!
//! The row also SHRANK, 40 bytes → 32: `class_id` folds into `word` (`kind` already says which
//! world it came from) and `caller`+`cp` fold into `key`. 65,536 slots is **2.00 MiB**, was 2.50,
//! and two rows now share one 64-byte line instead of straddling.
//!
//! ## The cheap negative — an 8 KiB table for profile reads
//!
//! `caller_epoch(caller)` is a 1024-bucket counter bumped whenever a site's profile CHANGES
//! (claimed, widened, poisoned). It provides a cheap negative for reads:
//!
//!  * **Read side:** `caller_epoch(c) == 0` PROVES no site of caller `c` has ever been profiled,
//!    so `mono_class`/`mono_ref` return `None` out of an 8 KiB L1-resident array instead of
//!    missing into a 2 MiB one. Under tier-0 that is almost every query the emitter makes.
//! Profile changes no longer schedule recompilation.
//!
//! ## Why an imprecise table is still safe
//!
//! A slot collision, a lost claim, or a class first seen after compilation can all make the
//! prediction WRONG. None of them can make the program wrong: the emitted code guards on the
//! receiver's class and falls back to the ordinary call when the guard fails. A bad prediction
//! costs one compare on a path that was already making a call — a performance risk, never a
//! correctness one. That is the whole reason speculating on a profile is safe.
//!
//! The one thing the table must never do is UNDER-report `distinct`. Reporting a polymorphic site
//! as monomorphic is still safe (the guard catches it) but wastes code space, so a slot whose
//! identity does not match is left alone rather than overwritten: a site that loses its slot goes
//! unprofiled, which refuses the inline. Silence, never a wrong answer.
//!
//! ## ⭐**WHY SO FEW SITES CARRY A PROFILE — and one defect that was making it worse**
//!
//! Selective recompilation fires ~78 times against a budget of 192, so what binds is not the
//! budget but how few sites this table can answer for. The chain has four gates, and only the
//! first two were ever visible:
//!
//!  1. **Who observes.** Under tier-0 a method's sites are never interpreted, so the interpreter's
//!     writers (`tlcache::obj_inline_put` / `ref_inline_put`) fire only for sites that reach the
//!     synthetic frame — PIC misses. Everything else has to come from the compiled-code writer in
//!     `vm::jit_direct_call::note_recv_profile`, which runs ONLY where a direct-call entry is
//!     BUILT. A (site, receiver class) the resolver REFUSES — the negative cache's own header puts
//!     refusals at 98% of direct misses — is never observed at all.
//!  2. **Slot collisions.** Direct-mapped, 65,536 slots, tens of thousands of warm sites: a site
//!     that loses its slot goes unprofiled for the life of the process.
//!  3. **`decl == 0`.** The compiled writer sees the receiver but not the resolution, so every
//!     heap-Ref row it claims is one `mono_ref` refuses forever unless an interpreter write
//!     late-fills the declaring class — and under tier-0 that write may never come. Heap-Ref is
//!     where the collections are, i.e. the half the inliner most wants.
//!  4. ⚠**THE TWO WRITERS WERE NAMING DIFFERENT WORDS, so gate 3 could not even be reached.**
//!     `note_ref`'s `word` is defined as the word the EMITTED GUARD compares — `ArcInner + 0`,
//!     `Heap::object_class_word`, which is what `emit_heap_ref_class_key` loads. The interpreter's
//!     writer passes exactly that. The compiled writer passed `class_name_ptr`, the `Arc<str>`
//!     STRING DATA pointer at `ArcInner + 16` — the same trap `tlcache::ref_inline_put` documents,
//!     from the other side. So for one monomorphic site the two writers disagreed, `note_common`
//!     took them for two different receivers and WIDENED the row to bimorphic; and because the
//!     `decl` late-fill only fires when `word` matches, a row claimed by the compiled writer could
//!     never be completed. **The compiled writer was destroying profiles the interpreter had
//!     written correctly.** Fixed at the writer (kill switch `RAVA_NO_WIDE_PROFILE=1`), because
//!     the guard's word is the definition and the writer is what was wrong.
//!
//! None of gates 1-3 is guessed at any more: `census` below counts every exit of both paths, and
//! the numbers print from the periodic sampler dump as the `profcov` gauges. **Read them before
//! widening anything else** — in particular `q_nodecl` / `tab_mono_ref_blind`, which price gate 3
//! exactly, and `w_newrefusal`, which prices gate 1.
//!
//! ⚠**A LAMBDA RECEIVER MUST NEVER BE PROFILED HERE BY CLASS.** An arena lambda's class id is its
//! CAPTURE SIGNATURE, shared by every closure in the program with that shape, so a class-keyed
//! "monomorphic" verdict on a lambda site would let the emitter splice **another site's body**
//! while reporting a hit (`nextgen::lambdas`, `LAMBDA_KEY_BASE`). Nothing in this file can tell a
//! lambda from an ordinary object, so the exclusion lives at every writer: the interpreter's
//! `obj_inline_put` never sees one (lambda dispatch does not reach it) and
//! `jit_direct_call`'s new writer tests `is_lambda_cid` / the interned `$Lambda` pointer and
//! refuses before calling in here.

use std::sync::atomic::{
    AtomicU32, AtomicU64, AtomicU8, AtomicUsize,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};

/// Power of two. A boot warms tens of thousands of sites; a collision costs a prediction.
const SLOTS: usize = 1 << 16;

/// 32 bytes, 8-aligned: two rows per cache line, 2.00 MiB for the whole table.
struct Slot {
    /// The site identity, published by ONE `compare_exchange`: `caller | (cp as u64) << 48`.
    /// 0 = free. Claiming identity atomically is what makes a concurrent writer safe — the old
    /// two-store claim could publish a row spliced from two different sites.
    key: AtomicU64,
    /// The single receiver this site has seen. `kind == 1`: an arena `class_id`, zero-extended.
    /// `kind == 2`: the heap-Ref class word (the interned class-name pointer `ref_inline_put`
    /// records and `guarded_inline` compares).
    word: AtomicU64,
    /// The RESOLVED declaring class for this (site, receiver) — the body an inline would splice.
    /// `ref_inline_put` carries it, so the admission needs no name lookup at all. **0 = unknown**,
    /// which is what a compiled-code writer records: it observes the receiver but not the
    /// resolution, so it profiles polymorphism honestly and leaves `mono_ref` refusing until an
    /// interpreter write late-fills the declaring class (see `note_ref`).
    decl: AtomicUsize,
    /// 0 = unset, 1 = arena class id, 2 = heap-Ref class word.
    kind: AtomicU32,
    /// 0 = unpublished, 1 = monomorphic, 2 = bimorphic, 3 = polymorphic **or poisoned**.
    /// MONOTONE and saturating — never decreases, which is what makes 3 absorbing and lets a
    /// `poison` from a deopt survive a concurrent widen.
    distinct: AtomicU32,
}

impl Slot {
    const fn new() -> Self {
        Slot {
            key: AtomicU64::new(0),
            word: AtomicU64::new(0),
            decl: AtomicUsize::new(0),
            kind: AtomicU32::new(0),
            distinct: AtomicU32::new(0),
        }
    }
}

static TABLE: std::sync::OnceLock<Vec<Slot>> = std::sync::OnceLock::new();

/// The WRITE side: materialises the 2 MiB table on first use.
fn table() -> &'static [Slot] {
    TABLE.get_or_init(|| (0..SLOTS).map(|_| Slot::new()).collect())
}

/// The READ side. Returns `None` — after one acquire load and nothing else — when no writer has
/// ever run. That is the common case under tier-0, where the emitter asks about every invoke in
/// every method it compiles before a single site has executed, and it used to answer by
/// committing and zeroing two megabytes.
fn table_ro() -> Option<&'static [Slot]> {
    TABLE.get().map(Vec::as_slice)
}

/// Pack `(caller, cp)` into one word. `None` refuses a caller that cannot be packed: a null
/// pointer, or one using the top 16 bits (impossible for an x86-64 user-mode heap allocation, and
/// refused rather than truncated so a bad pack can never be mistaken for another site's row).
#[inline]
fn key_of(caller: usize, cp: u16) -> Option<u64> {
    let c = caller as u64;
    if c == 0 || c >> 48 != 0 {
        return None;
    }
    Some(c | ((cp as u64) << 48))
}

#[inline]
fn slot_of(caller: usize, cp: u16) -> usize {
    // ⭐**THE OLD INDEX GAVE `cp` THREE BITS, AND THE CENSUS PRICED IT.**
    //
    // It was `(caller >> 4) ^ ((cp as usize) << 13) ^ (caller >> 27)`, masked to 16 bits. Shifting
    // `cp` left by 13 puts it in bits 13..29, so **only its low three bits survive the mask** —
    // every call site of one class landed in one of EIGHT slots, distinguished solely by the
    // class pointer's contribution, which is a constant for that class. A class with two hundred
    // call sites therefore had eight usable rows.
    //
    // The `profcov` census measured exactly that: of 16,597 emitter queries, **11,023 (66%) failed
    // with `q_foreign` — the slot is owned by a different site — while the table held 4,305 rows
    // in 65,536 slots, i.e. it was 6.6% FULL.** At that occupancy a well-mixed index collides on
    // the order of a few percent, not two thirds.
    //
    // Now a multiplicative mix over the full 64 bits with a final xor-shift, so both the pointer's
    // varying bits and all sixteen bits of `cp` reach the masked window. Correctness does not
    // depend on this at all — every read verifies the packed `(caller, cp)` key and a wrong slot
    // is a MISS, never a wrong answer — so this can only change the hit rate.
    // ⛔**AND THE CORRECT HASH MAKES THE BOOT SLOWER, WHICH IS THE MORE INTERESTING RESULT.**
    // ABBA, three boots per arm: the mixed index reads CPU-s 33.0 / 33.4 / 32.9 against the old
    // one's 32.3 / 32.5 / 32.8 — **+1.5% median, +1.9% on the minimum, and the arms are DISJOINT**.
    // Nothing about the hash costs that; what costs is what the profiles it recovers then FEED.
    // Twice as many emitter queries are answered, so twice as many sites are admitted to guarded
    // devirtualisation and inlining, so more guard diamonds are emitted and the bodies handed to
    // `regalloc2` grow — and this boot executes most compiled methods a handful of times, so the
    // speculation never earns back its own compile cost. It is the same shape this tree measured
    // when it priced a whole-rest-of-the-whitelist inline ceiling at 0.39%.
    // ⇒ The mix is therefore OPT-IN (`RAVA_SITEMONO_MIX=1`), kept because the old index is a
    // genuine defect and because any future work that makes speculation cheaper — a smaller guard,
    // a recompile that only fires for methods that actually run hot — needs the profiles it
    // recovers. It is NOT tied to `RAVA_NO_FAST_SITEMONO`, which gates this file's concurrency
    // fixes and must stay on.
    static MIX: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*MIX.get_or_init(|| std::env::var_os("RAVA_SITEMONO_MIX").is_some()) {
        let h = (caller >> 4) ^ ((cp as usize) << 13) ^ (caller >> 27);
        return h & (SLOTS - 1);
    }
    let h = ((caller as u64) >> 4)
        ^ (cp as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((h ^ (h >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as usize & (SLOTS - 1)
}

// ---------------------------------------------------------------------------------------------
// PER-CALLER EPOCH — the cheap negative for reads.
// ---------------------------------------------------------------------------------------------

/// 1024 buckets × 8 bytes = **8 KiB, in `.bss`, never allocated**. Small enough to stay resident
/// while the emitter walks a method's invokes; large enough that unrelated callers rarely share.
const EPOCH_SLOTS: usize = 1024;
static EPOCH: [AtomicU64; EPOCH_SLOTS] = [const { AtomicU64::new(0) }; EPOCH_SLOTS];

#[inline]
fn epoch_slot(caller: usize) -> &'static AtomicU64 {
    // A different mix from `slot_of`: two callers that collide in one table should not be forced
    // to collide in the other.
    &EPOCH[((caller >> 4) ^ (caller >> 21)) & (EPOCH_SLOTS - 1)]
}

/// Bumped RELEASE and LAST, after the row it describes is published, so a reader that acquires a
/// non-zero epoch is guaranteed to see at least the write that produced it. A reader that sees 0
/// has proof that nothing for this caller was published — which is exactly the negative it wants.
#[inline]
fn bump_epoch(caller: usize) {
    epoch_slot(caller).fetch_add(1, Release);
}

/// How many profile CHANGES this caller's bucket has recorded. A compile that records this value
/// and finds it unchanged later knows no site in the method gained anything the emitter reads, so
/// a second compile would produce identical code — the selectivity the recompile round needs,
/// at O(1) and zero bytes per site.
///
/// ⚠Conservative in one direction only. A bucket collision can report a change that belongs to
/// another caller (cost: one wasted re-examination) but can never hide a real one, because every
/// writer bumps before returning. With `RAVA_NO_FAST_SITEMONO=1` no writer bumps, so this reads 0
/// forever and a caller comparing epochs concludes "nothing changed" — i.e. the kill switch
/// restores today's never-recompile behaviour rather than inverting it.
pub(crate) fn caller_epoch(caller: usize) -> u64 {
    epoch_slot(caller).load(Acquire)
}

/// Has ANY site anywhere ever been profiled? One acquire load, no table touch. Cheap enough to
/// put in front of a whole-method scan.
#[allow(dead_code)] // the recompile decider's coarse pre-test
pub(crate) fn any_profile() -> bool {
    table_ro().is_some()
}

// ---------------------------------------------------------------------------------------------
// KILL SWITCH
// ---------------------------------------------------------------------------------------------

/// `RAVA_NO_FAST_SITEMONO=1` (or `RAVA_NO_EMIT_MORE=1`) turns off the two behaviours that are new
/// and therefore optional:
///
///  * the epoch bump, and with it the epoch-based cheap negative on the read path and the
///    recompile trigger (everything reads 0 ⇒ "nothing changed" ⇒ today's behaviour);
///  * the compiled-code writer in `vm::jit_direct_call`, which consults this same gate.
///
/// It does NOT revert the atomic claim, the release/acquire publication or the monotone
/// `distinct`. Those change no verdict this table can produce — they only decide which of two
/// racing writers is the one whose verdict is published, and remove a row that belongs to neither.
/// There is nothing to A/B there and nothing to fall back to.
///
/// ⭐**Latched in a relaxed byte rather than a `OnceLock`**, for the reason `jit_direct_call.rs`
/// records at `fast_dprobe`: `OnceLock::get_or_init` is an ACQUIRE load, i.e. a reordering
/// barrier LLVM must honour, and spending one to read a switch is how a win gets handed straight
/// back. The latched value is a constant of the process (an env var read before any guest code
/// runs), so two threads racing to latch it compute the SAME byte — a memo of a constant, not
/// shared mutable state.
const GATE_UNSET: u8 = 2;
static FAST_SITEMONO: AtomicU8 = AtomicU8::new(GATE_UNSET);

/// Out of line so the env query — a Win32 environment-block walk under a lock — never appears in
/// the instruction stream of the paths that read the gate.
#[cold]
#[inline(never)]
fn fast_sitemono_latch() -> bool {
    let on = std::env::var_os("RAVA_NO_FAST_SITEMONO").is_none()
        && !crate::flags::emit_more_disabled();
    FAST_SITEMONO.store(u8::from(on), Relaxed);
    on
}

#[inline(always)]
pub(crate) fn fast_sitemono() -> bool {
    match FAST_SITEMONO.load(Relaxed) {
        0 => false,
        1 => true,
        _ => fast_sitemono_latch(),
    }
}

/// `RAVA_NO_WIDE_PROFILE=1` (or `RAVA_NO_EMIT_MORE=1`) restores the pre-round behaviour of the
/// COMPILED-CODE receiver writer in `vm::jit_direct_call::note_recv_profile` — see the census
/// finding recorded at `census` below. It governs BEHAVIOUR only; the counters stay on, because a
/// census that has to be enabled is a census nobody reads.
///
/// Latched in a relaxed byte for the reason `fast_sitemono` records: the value is a constant of
/// the process, and spending an `OnceLock` ACQUIRE to read it is a reordering barrier LLVM must
/// honour on a path this round is trying to shorten.
static WIDE_PROFILE: AtomicU8 = AtomicU8::new(GATE_UNSET);

#[cold]
#[inline(never)]
fn wide_profile_latch() -> bool {
    let on = std::env::var_os("RAVA_NO_WIDE_PROFILE").is_none()
        && !crate::flags::emit_more_disabled();
    WIDE_PROFILE.store(u8::from(on), Relaxed);
    on
}

#[inline(always)]
pub(crate) fn wide_profile() -> bool {
    match WIDE_PROFILE.load(Relaxed) {
        0 => false,
        1 => true,
        _ => wide_profile_latch(),
    }
}

// ---------------------------------------------------------------------------------------------
// PROFILE COVERAGE CENSUS — "why do so few sites carry a profile?"
// ---------------------------------------------------------------------------------------------

/// ⭐**THE QUESTION THIS ANSWERS.** Selective recompilation fires ~78 times against a budget of
/// 192, so the binding constraint is not the budget — it is how few sites carry a profile the
/// emitter can read. Nothing in the engine could say WHERE the population is lost, and five
/// earlier censuses in this tree reported from a teardown a server boot never reaches. These are
/// plain relaxed counters plus one dump-time scan of the table itself, exposed as `profcov`
/// gauges, so they print from the PERIODIC sampler dump (`RAVA_DIAG_EVERY_MS`) and from every
/// other trigger — no teardown required.
///
/// **Read them as a funnel, write side first:**
///
/// ```text
///   offer_*        an observation was OFFERED by a writer (once per (site, receiver class))
///     nokey        refused: the (caller, cp) identity does not pack into 48+16 bits
///     claimed      it CLAIMED a free slot   => this is the count of distinct profiled sites
///     collided     the slot belongs to ANOTHER site => this site goes unprofiled, forever
///     widened      a second/third receiver at a site we own => speculation correctly refused
///     halfbuilt    the owner had claimed but not yet published => deliberately silent
///     decl_blind   a heap-Ref row claimed with decl == 0 (a compiled-code-only observation)
///     decl_filled  an interpreter write late-filled that 0
/// ```
///
/// **Then the read side, which is the emitter asking:**
///
/// ```text
///   q              mono_class + mono_ref calls (compile path only — ~2k compiles a boot)
///     q_epoch0     answered None by the 8 KiB caller-epoch proof: this CALLER has no profile
///     q_notable    answered None because nothing anywhere has ever been profiled
///     q_foreign    the site's slot is owned by a different site (a collision, above)
///     q_poly       the row exists but is bimorphic / polymorphic / poisoned
///     q_kind       the row exists and is monomorphic, but in the OTHER world (arena vs Ref)
///     q_nodecl     ⭐monomorphic heap-Ref row REFUSED only because its declaring class is 0
///     q_hit        the emitter got an answer
/// ```
///
/// `q_nodecl` is the one that decides whether the next round should teach the compiled writer a
/// declaring class: it is the population that is profiled, monomorphic, and thrown away.
///
/// **And the table census** (`rows`, `mono_arena`, `mono_ref`, `mono_ref_blind`, `bi`, `poly`) is
/// a dump-time scan of the 65,536 slots — the standing shape of the profile, independent of how
/// many times anything was asked. It never MATERIALISES the table: an unwritten table reports
/// zeros out of one acquire load.
///
/// ⚠**COST, STATED.** Every write-side counter sits on a path that is already a cache MISS or a
/// host crossing and runs once per (site, receiver class); every read-side counter sits on the
/// COMPILE path, which runs ~2,000 times a boot and already takes a lock and walks bytecode.
/// There is no per-call increment here. That is the whole reason they are unconditional: a gate
/// would cost more instructions on those paths than the counter it guards.
pub(crate) mod census {
    use super::table_ro;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    macro_rules! ctrs {
        ($($n:ident),* $(,)?) => {
            $(pub(crate) static $n: AtomicU64 = AtomicU64::new(0);)*
        };
    }
    ctrs!(
        OFFER_ARENA,
        OFFER_REF,
        NOKEY,
        CLAIMED,
        COLLIDED,
        WIDENED,
        HALFBUILT,
        DECL_BLIND,
        DECL_FILLED,
        POISON,
        POISON_TOMB,
        POISON_STEAL,
        Q,
        Q_EPOCH0,
        Q_NOTABLE,
        Q_FOREIGN,
        Q_EMPTY,
        Q_POLY,
        Q_KIND,
        Q_NODECL,
        Q_HIT,
    );

    /// The compiled-code writer's OWN refusals, counted where they happen
    /// (`vm::jit_direct_call::note_recv_profile`). Together with `OFFER_*` these say what
    /// fraction of dispatches that reach a profile writer actually produce an observation.
    ctrs!(
        W_FILL,       // a direct-cache FILL reached the writer at all
        W_NOTVIRT,    // invokestatic / invokespecial: no receiver to profile
        W_OFF,        // the kill switch is set
        W_LAMBDA,     // refused: lambda-shaped entry, lambda class id, or the "$Lambda" name
        W_GUARD,      // refused: entry.guard does not name the class this receiver decodes to
        W_DEAD,       // refused: dead handle / unreadable class word
        W_SHAPE,      // refused: the receiver cell is neither a heap Ref nor an arena object
        W_NEWREFUSAL, // a genuinely NEW (site, receiver class) the resolver REFUSED — no profile
    );

    #[inline]
    pub(crate) fn bump(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }

    /// Standing shape of the table, scanned at DUMP time only:
    /// `[rows, mono_arena, mono_ref_usable, mono_ref_blind, bi, poly_or_poisoned]`.
    ///
    /// Reads are relaxed on purpose — this is a census, not a decision, and a row caught
    /// mid-publication is at worst counted in the previous bucket for one dump.
    fn scan() -> [u64; 6] {
        let mut out = [0u64; 6];
        let Some(t) = table_ro() else { return out };
        for s in t {
            if s.key.load(Relaxed) == 0 {
                continue;
            }
            out[0] += 1;
            let d = s.distinct.load(Relaxed);
            let k = s.kind.load(Relaxed);
            match d {
                1 if k == 1 => out[1] += 1,
                1 if k == 2 => {
                    if s.decl.load(Relaxed) == 0 {
                        out[3] += 1; // profiled, monomorphic — and unreadable by the emitter
                    } else {
                        out[2] += 1;
                    }
                }
                2 => out[4] += 1,
                3 => out[5] += 1,
                _ => {}
            }
        }
        out
    }

    macro_rules! scan_gauges {
        ($($f:ident = $i:expr),* $(,)?) => {
            $(pub(crate) fn $f() -> u64 { scan()[$i] })*
        };
    }
    scan_gauges!(
        rows = 0,
        mono_arena = 1,
        mono_ref = 2,
        mono_ref_blind = 3,
        bi = 4,
        poly = 5,
    );
}

// ---------------------------------------------------------------------------------------------
// WRITE PATH
// ---------------------------------------------------------------------------------------------

/// A second distinct receiver has been seen at a slot we own. Saturating and monotone, so a widen
/// that races a `poison` cannot pull 3 back down to 2. Returns whether the row actually changed —
/// an already-polymorphic site is noted again on every new class it sees, and bumping the epoch
/// for a verdict that did not move would ask for a recompile that reads exactly what it read
/// before.
#[inline]
fn widen(s: &Slot) -> bool {
    s.distinct
        .fetch_update(Release, Relaxed, |d| {
            if d >= 3 {
                None // absorbing: poisoned or already polymorphic
            } else {
                Some(d + 1)
            }
        })
        .is_ok()
}

/// The one write path. `kind`/`word` name the receiver in whichever of the two worlds it came
/// from; `decl` is the resolved declaring class, or 0 when the writer does not know one.
///
/// Sound for concurrent callers on any thread: the identity is claimed by a single
/// `compare_exchange`, the payload is written only by the winner of that claim, and the store
/// that makes the row readable is a release paired with the readers' acquire.
fn note_common(caller: usize, cp: u16, kind: u32, word: u64, decl: usize) {
    census::bump(if kind == 1 { &census::OFFER_ARENA } else { &census::OFFER_REF });
    let Some(key) = key_of(caller, cp) else {
        census::bump(&census::NOKEY);
        return;
    };
    let s = &table()[slot_of(caller, cp)];
    if s.key.load(Relaxed) == 0 && s.key.compare_exchange(0, key, AcqRel, Relaxed).is_ok() {
        census::bump(&census::CLAIMED);
        // A heap-Ref row claimed with no declaring class is a COMPILED-CODE-ONLY observation:
        // `mono_ref` refuses it until an interpreter write late-fills `decl`, so under tier-0 —
        // where the interpreter never runs the site — it is a profile that exists and can never
        // be read. Counted separately because that population, not the recompile budget, is
        // what the coverage question is about.
        if kind == 2 && decl == 0 {
            census::bump(&census::DECL_BLIND);
        }
        // We own the payload — nobody else can write these fields for this slot, ever.
        s.word.store(word, Relaxed);
        s.decl.store(decl, Relaxed);
        s.kind.store(kind, Relaxed);
        // RELEASE, and `fetch_max` rather than `store`, so a `poison` that beat us here (the
        // tombstone arm below can set 3 on a slot the instant after we claim it) is not undone.
        s.distinct.fetch_max(1, Release);
        bump_epoch(caller);
        return;
    }
    if s.key.load(Relaxed) != key {
        census::bump(&census::COLLIDED);
        return; // another site owns this slot; this one goes unprofiled
    }
    // ACQUIRE the publication flag BEFORE comparing against the payload. A row whose owner has
    // claimed the key but not yet stored `kind`/`word` reads as `kind == 0`, which would compare
    // unequal to whatever we hold and widen a site that may well be monomorphic — turning a
    // nanosecond-wide race into a permanently refused speculation. Staying silent is the right
    // way to be wrong here: a genuinely polymorphic site is noted again on its next cache miss,
    // of which there are many, while a spurious widen is forever.
    if s.distinct.load(Acquire) == 0 {
        census::bump(&census::HALFBUILT);
        return;
    }
    if s.kind.load(Relaxed) != kind || s.word.load(Relaxed) != word {
        // A DIFFERENT receiver than the one on file.
        census::bump(&census::WIDENED);
        if widen(s) {
            bump_epoch(caller);
        }
        return;
    }
    // Same receiver as on file. If the row was claimed by a writer that had no declaring class —
    // a compiled-code observation — and this writer does, fill it in: `decl` is a function of the
    // caller's constant-pool entry and the receiver's class, both of which the matching `key` and
    // `word` already pin, so the two writers cannot disagree about it. Release-paired with
    // `mono_ref`'s acquire, which reads `decl` last and refuses while it is 0.
    if kind == 2 && decl != 0 && s.decl.load(Relaxed) == 0 {
        census::bump(&census::DECL_FILLED);
        s.decl.store(decl, Release);
        bump_epoch(caller);
    }
}

/// Record an ARENA receiver at this site, by its class id.
///
/// ⚠The caller must have established that the receiver is not a lambda — see the module note.
pub(crate) fn note(caller: usize, cp: u16, class_id: u32) {
    note_common(caller, cp, 1, u64::from(class_id), 0);
}

/// Record a HEAP-REF receiver at this site, by its class word, with the declaring class whose
/// body an inline would splice — or 0 when the writer observed the receiver but not the
/// resolution. A 0 keeps the polymorphism accounting honest (which is what stops the emitter
/// speculating on a site that only LOOKS monomorphic because one writer was blind to the rest)
/// while `mono_ref` stays silent until a writer that knows the declaring class arrives.
pub(crate) fn note_ref(caller: usize, cp: u16, word: u64, decl: usize) {
    if word == 0 {
        return;
    }
    note_common(caller, cp, 2, word, decl);
}

/// Mark a site as never-again-speculatable. Called when a deopt actually fires: the guess was
/// wrong, and a wrong guess repeated costs a full frame rebuild on every call.
///
/// Implemented as "polymorphic", which every reader already refuses — no new state, no new branch
/// on the read path. Two things changed with concurrency in mind:
///
///  * `distinct` is monotone, so a concurrent widen can no longer store 2 over this 3;
///  * an UNCLAIMED slot is claimed as a **tombstone**. A deopt whose profile row had already been
///    lost to a collision used to be forgotten entirely, and the next writer would re-arm the
///    same speculation that had just trapped.
///
/// A never-written table means nothing was ever speculated from here, so there is nothing to
/// poison and the 2 MiB is not materialised to say so.
///
/// ⭐**AND IT NOW POISONS UNCONDITIONALLY, WHICH IS THE POINT OF THE WHOLE BRAKE.**
/// `jit::callbacks::deopt` states the hazard in as many words: recompilation is only safe because
/// this call disarms a failed speculation before the second compile reads the profile, and the
/// old form *silently wrote nothing* whenever the slot had been claimed by a colliding site —
/// after which the recompile reads the same profile, emits the same speculation, traps again, and
/// pays a full Cranelift compile per lap. Direct-mapped with 65,536 slots and tens of thousands
/// of warm sites, eviction is ordinary, not exotic.
///
/// So a poison that finds a FOREIGN owner now poisons the row first and then STEALS it, in that
/// order. Both steps only ever REFUSE:
///
///  * the `fetch_max(3)` costs the colliding site its prediction, which the module's own rule
///    already licenses ("a site that loses its slot goes unprofiled"), and closes the window in
///    which a reader could see our key over the victim's payload;
///  * the steal makes the poison durable — a later `note` from the victim finds a key that is not
///    its own and stays silent, instead of re-arming the speculation that just trapped.
///
/// The trade is one lost prediction for a guaranteed disarm, and a poison only fires when a
/// speculation has ALREADY trapped, so it is paid at most once per deoptimising site.
/// `RAVA_NO_FAST_SITEMONO=1` restores the identity-match-only poison — and with it the hazard.
pub(crate) fn poison(caller: usize, cp: u16) {
    let Some(key) = key_of(caller, cp) else { return };
    let Some(t) = table_ro() else { return };
    let s = &t[slot_of(caller, cp)];
    let owner = s.key.load(Relaxed);
    // Did the OWNERSHIP change? A steal leaves `distinct` already at 3, so the final `fetch_max`
    // cannot detect it, and the recompile decider must still be told this caller's profile moved.
    // (The victim's own epoch is deliberately NOT bumped: losing a row can only make its method
    // speculate LESS, so a stale "nothing changed" for the victim is safe and costs one fewer
    // recompile.)
    census::bump(&census::POISON);
    let mut claimed = false;
    if owner != key {
        census::bump(if owner == 0 { &census::POISON_TOMB } else { &census::POISON_STEAL });
        if owner == 0 {
            // A TOMBSTONE. The slot was never claimed (or the profile that produced this deopt
            // was evicted), so claim it poisoned: a later writer must not be able to re-arm a
            // site the engine has already watched trap.
            claimed = s.key.compare_exchange(0, key, AcqRel, Relaxed).is_ok();
        } else if fast_sitemono() {
            // Foreign owner: refuse the row, then take it. Order matters — see the doc above.
            s.distinct.fetch_max(3, Release);
            claimed = s.key.compare_exchange(owner, key, AcqRel, Relaxed).is_ok();
        } else {
            return; // pre-fix behaviour: a collision loses the poison entirely
        }
    }
    if s.key.load(Relaxed) == key && (s.distinct.fetch_max(3, Release) != 3 || claimed) {
        bump_epoch(caller);
    }
}

// ---------------------------------------------------------------------------------------------
// READ PATH — every exit before the table is a load out of 8 KiB or less.
// ---------------------------------------------------------------------------------------------

/// The row owned by this site, or `None`. Three cheap negatives in front of the 2 MiB miss:
/// an unpackable identity, a caller whose epoch proves nothing was ever written for it, and a
/// table that has never been written at all.
///
/// ⭐**Every exit is COUNTED** (`census`), because "so few sites carry a profile" is a claim about
/// exactly these exits and nothing in the engine could previously say which one swallows the
/// population. Readers run on the compile path (`collect_inline_targets`);
/// these counters never request another compilation.
#[inline]
fn slot_ro(caller: usize, cp: u16) -> Option<&'static Slot> {
    census::bump(&census::Q);
    let key = key_of(caller, cp)?;
    let t = if fast_sitemono() {
        if caller_epoch(caller) == 0 {
            census::bump(&census::Q_EPOCH0);
            return None; // proof: no site of this caller has ever been profiled
        }
        match table_ro() {
            Some(t) => t,
            None => {
                census::bump(&census::Q_NOTABLE);
                return None;
            }
        }
    } else {
        table()
    };
    let s = &t[slot_of(caller, cp)];
    let have = s.key.load(Relaxed);
    if have != key {
        // ⚠**"FOREIGN" CONFLATED TWO THINGS, AND THE HEADLINE READ THE WRONG ONE.** This branch is
        // taken both when another site OWNS the slot and when the slot is EMPTY — an unwritten
        // slot holds key 0, which is also `!= key`. With the table around a tenth full, a query
        // for a site that was simply never profiled finds an EMPTY slot nine times in ten, so one
        // counter here reads as a collision rate when it is mostly a COVERAGE rate. The two call
        // for opposite fixes — a collision wants a better index or more slots, an empty slot wants
        // more writers — so they are counted apart.
        census::bump(if have == 0 { &census::Q_EMPTY } else { &census::Q_FOREIGN });
        return None;
    }
    Some(s)
}

/// The single ARENA receiver class this site has ever seen, or `None` when it is unprofiled,
/// polymorphic, poisoned, or lost its slot.
pub(crate) fn mono_class(caller: usize, cp: u16) -> Option<u32> {
    let s = slot_ro(caller, cp)?;
    // ACQUIRE: pairs with the release in `note_common`, so the `kind`/`word` read below is the
    // payload that belongs to this verdict and not a half-built row.
    if s.distinct.load(Acquire) != 1 {
        census::bump(&census::Q_POLY);
        return None;
    }
    if s.kind.load(Relaxed) != 1 {
        census::bump(&census::Q_KIND);
        return None;
    }
    let r = u32::try_from(s.word.load(Relaxed)).ok();
    if r.is_some() {
        census::bump(&census::Q_HIT);
    }
    r
}

/// The single heap-Ref class word this site has seen, with the declaring class whose body an
/// inline would splice. `None` when unprofiled, polymorphic, poisoned, lost to a collision, or
/// recorded by a writer that did not know the declaring class.
pub(crate) fn mono_ref(caller: usize, cp: u16) -> Option<(u64, usize)> {
    let s = slot_ro(caller, cp)?;
    if s.distinct.load(Acquire) != 1 {
        census::bump(&census::Q_POLY);
        return None;
    }
    if s.kind.load(Relaxed) != 2 {
        census::bump(&census::Q_KIND);
        return None;
    }
    // Read `decl` with ACQUIRE too: it can be late-filled AFTER the release that published the
    // row, so the row's own publication does not order it.
    let decl = s.decl.load(Acquire);
    if decl == 0 {
        // ⭐**PROFILED, MONOMORPHIC, AND THROWN AWAY.** This is the compiled-code-only
        // observation (`DECL_BLIND` on the write side): the row knows the receiver but not the
        // declaring class whose body an inline would splice, and under tier-0 no interpreter
        // write ever arrives to fill it. If this counter is large, teaching the compiled writer a
        // declaring class is the next lever; if it is small, it is not.
        census::bump(&census::Q_NODECL);
        return None;
    }
    census::bump(&census::Q_HIT);
    Some((s.word.load(Relaxed), decl))
}

// ---------------------------------------------------------------------------------------------
// PER-SITE TABLE DUMP (`RAVA_SITEMONO_TABLE=1`, default OFF, read-only)
// ---------------------------------------------------------------------------------------------

/// ⭐**WHY THIS EXISTS.** `census` above answers "how many sites are monomorphic"; nothing could
/// answer "is THIS site monomorphic", which is the question a per-call-site hot-shape table asks.
/// This is a dump-time scan of the 65,536 slots, printed only when the env gate is set, so the
/// default path is untouched: an unwritten table reports nothing and is never materialised.
/// Diagnostic only — nothing routes on it.
pub fn dump_table() {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_SITEMONO_TABLE").is_some()) {
        return;
    }
    let Some(t) = table_ro() else {
        eprintln!("[SITEMONO-TABLE] table never materialised: no site was ever profiled");
        return;
    };
    let mut rows: Vec<String> = Vec::new();
    let (mut mono, mut bi, mut poly) = (0u64, 0u64, 0u64);
    for s in t.iter() {
        let key = s.key.load(Acquire);
        if key == 0 {
            continue;
        }
        let d = s.distinct.load(Acquire);
        match d {
            1 => mono += 1,
            2 => bi += 1,
            _ => poly += 1,
        }
        let caller = (key & 0x0000_ffff_ffff_ffff) as usize;
        let cp = (key >> 48) as u16;
        // SAFETY: the loader never evicts, so a class pointer handed to compiled code outlives
        // the process. Dump-time only, exactly as `rava_sampler::report` does it.
        let name = if caller != 0 {
            let lc: &crate::class_loader::LoadedClass = unsafe { &*(caller as *const _) };
            match lc.file.pool.member_ref(cp) {
                Some(r) => format!(
                    "{} @cp{cp} -> {}.{}{}",
                    lc.file.this_class, r.owner, r.name, r.descriptor
                ),
                None => format!("{} @cp{cp}", lc.file.this_class),
            }
        } else {
            "<no site>".to_string()
        };
        let kind = match s.kind.load(Relaxed) {
            1 => "arena",
            2 => "heapref",
            _ => "unset",
        };
        let state = match d {
            0 => "halfbuilt",
            1 => "MONO",
            2 => "bi",
            _ => "poly/poisoned",
        };
        rows.push(format!(
            "[SITEMONO-TABLE] {state:<13} {kind:<7} word={:#018x} decl={} {name}",
            s.word.load(Relaxed),
            if s.decl.load(Acquire) == 0 { "0(blind)" } else { "yes" },
        ));
    }
    rows.sort();
    eprintln!(
        "[SITEMONO-TABLE] {} claimed slots: MONO {mono}, bi {bi}, poly/poisoned {poly}",
        rows.len()
    );
    for r in rows {
        eprintln!("{r}");
    }
}
