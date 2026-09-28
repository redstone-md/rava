//! **DEOPTIMISATION: leaving compiled code for the interpreter, mid-method.**
//!
//! Two measurements said this engine needs it. Guarded speculation that keeps a FALLBACK PATH in
//! the compiled method costs more than it saves — four inline arms removed 10.9 M calls a boot
//! for +0.81%, and guarded devirtualisation inlined 7.0 M for +2.42%. The more we inlined, the
//! worse it got, which is the signature of damaged code structure rather than cheap calls: every
//! speculation split a straight line into a diamond with a merge and a block parameter per live
//! reference, and register allocation and loop-invariant motion got worse across the whole method.
//!
//! HotSpot's shape keeps ONE path. The guard's failure edge LEAVES the function — an uncommon
//! trap that resumes in the interpreter — so the compiled body is straight-line code with no
//! fallback dispatch, no merge, and no extra live ranges. That is what this file implements.
//!
//! ## The transition
//!
//! ```text
//! guard fails -> spill locals and the operand stack into the frame buffer
//!             -> rava_deopt(class, code, site, buf)
//!             -> Frame rebuilt, `opcodes::run` resumes at the site's pc
//!             -> its result becomes the compiled method's return value
//! ```
//!
//! The buffer layout is already the interpreter's: `spill_for_gc` writes locals at `[i*8]` and
//! the operand stack at `[(nlocals+i)*8]`, which is exactly `Frame { buf, nlocals }`.
//!
//! ## Why the type tags exist
//!
//! A JIT cell is a raw `i64`; a `Frame` holds typed `Value`s and the interpreter is not tolerant
//! about it (`Value::as_int()` answers `None` for a `Long`). `jit::analysis::localtypes` recovers
//! the type of every local at every instruction boundary — proven at **96.4%** of them on a
//! product boot — and the operand stack's types the emitter already knows (`SV.t`). A site whose
//! types are not all proven is simply not emitted.
//!
//! ## Why this is safe here in a way it would not be elsewhere
//!
//! Only methods **without exception handlers** are ever compiled (`jit_run` bails on
//! `!code.exceptions.is_empty()`), so a deopt can never land inside a `try` block and there is no
//! handler stack to rebuild. That removes the hardest part of deoptimisation before it starts.
//!
//! ## ⛔What it still cannot do, measured
//!
//! It works: `RAVA_DEOPT_FORCE` makes every guard fail, and a product boot then executes
//! **6,651,357 deopts** with 4 bit-exact gates and 21 differential probes green. Reconstructing
//! locals, stack, pc and the return value is correct, and 6.65 M transitions is not a sample.
//!
//! But the trap shape still loses to the diamond it was built to replace — **-1.08% for the
//! diamond**, arms overlapping — and the counter says why. In normal operation the guards deopt
//! **727,602 times a boot across 89 sites**. HotSpot's traps are rare; ours are not, and every
//! one costs a full frame rebuild plus interpreting the method's remainder, where the diamond's
//! miss costs one ordinary call.
//!
//! Poisoning the site when its trap fires (`sitemono::poison`) is implemented here and changes
//! **nothing**: the count is identical to the digit. Poisoning only affects the NEXT compile, and
//! a method is compiled once — so the already-emitted code keeps deopting forever.
//!
//! ⇒ **The missing feature is invalidation: deoptimise AND recompile without the failed
//! speculation.** HotSpot's traps are rare because a trap that fires removes itself. Ours cannot,
//! so a wrong guess is paid for on every call for the life of the process. Deopt is necessary and
//! is now built; it is not sufficient without the ability to throw compiled code away.
//!
//! ---
//!
//! # Historical recompilation design (round 21)
//!
//! The profile-driven scheduler described in this historical analysis has been
//! removed. Only `rava_deopt` now calls `supersede_at_entry`; its guard-failure
//! invalidation and safety rules remain. See `docs/no-profile-recompile.md`.
//!
//! The product flag set runs `RAVA_JIT_TIER0=1`, so a method is compiled on its FIRST ENTRY and
//! its own call sites have never executed when the emitter runs. Every optimisation that needs a
//! receiver profile is therefore dead on arrival, and the cure is a SECOND compile once the first
//! body has run and its sites have been profiled. Two questions land on this file.
//!
//! ## (1) Is the full exit correct for a SUPERSEDED body? — It is correct, and it is the WRONG
//! ## MECHANISM. **A recompile needs no live-frame migration at all.**
//!
//! "Poisoned" and "superseded" are different facts. A poisoned body contains a speculation that
//! was WRONG at this site: the guard has already refused, the compiled code cannot proceed, and
//! the only way forward is to rebuild the frame and finish in the interpreter — the whole exit
//! below. A superseded body is still **completely correct**; it is merely no longer the best code
//! we could emit. Running it to completion is not a bug, it is just slower than the body we are
//! about to build. Paying a frame rebuild plus interpretation of the method's remainder to escape
//! a body that is *correct* is pure loss, and it is loss at the worst possible moment (deep in a
//! hot method, which is exactly the population a profile-driven recompile targets).
//!
//! There is a second, harder reason the exit cannot serve supersede even if we wanted it to: the
//! transition is only available at a **site the emitter chose to describe** (`emit_deopt_exit`
//! refuses without a complete type map, and returns `false` for a void method). A body worth
//! recompiling generally has no trap site at all — it was compiled BLIND, which is the entire
//! complaint. There is nowhere to jump from.
//!
//! ⇒ **A recompile takes effect at method ENTRY only.** Flip the `jit_cache` verdict from
//! `Compiled` back to `Cold`; the frames already running in the old body run it to completion and
//! return normally, and the *next* `jit_run` entry compiles the new one. Nothing migrates, no
//! frame is rebuilt, no trap is emitted, and this file participates only through the small
//! primitive `supersede_at_entry` below — which is the second half of `rava_deopt`, factored out
//! so the recompile driver has a correct and BUDGETED place to call.
//!
//! ### Why abandoning the old body at entry is safe, precisely
//!
//! * Finalized machine code is **never freed**. There is no `free_memory` call in the tree; the
//!   shared `JITModule` stays mapped for its thread's lifetime (`jit::SHARED_MODULE`, and
//!   `gil.rs`'s worker rule exists exactly to keep that true). A pointer captured before the flip
//!   calls valid code.
//! * That code is still **semantically correct** — this is the supersede case, not a miscompile.
//!   A caller that keeps entering the old body computes the same answers, just without the new
//!   profile's benefit.
//! * ⚠`vm/fork_apply.rs` justifies its captured `fptr` with "`jit_cache` never replaces a
//!   `Compiled` entry". That STATED REASON is already false — the invalidation below has been
//!   inserting `Cold` over `Compiled` since deopt landed — but the CONCLUSION survives for the
//!   two reasons above. Anyone reading that comment should read this one.
//!
//! ### ⚠What a supersede does NOT reach, and this is the number that bounds the whole feature
//!
//! `jit_cache` is consulted at **method entry** (`Vm::jit_run`) and by the dispatch tier-up arm.
//! It is NOT consulted by the callers that already found the body: `Vm::jit_direct_cache` stores
//! a raw `DirectEntry::fptr` per `(caller, cp, recv_guard)`, the machine-code IC bakes
//! `fptr | deep`, and the freeze table caches a resolved callee address. **None of those are
//! keyed by the callee's `Code` pointer, so none of them can be found and flushed from here.**
//! They keep calling the old body until they miss for some other reason. That is correct (see
//! above) but it caps the benefit: a recompiled body is only reached through traffic that goes
//! via `jit_run`. Flushing them is a change to those files, not this one, and it is the first
//! thing to measure before assuming a recompile pays.
//!
//! ## (2) What this mechanism costs on the paths that never deopt
//!
//! Named exactly, because "it will be fine" is not a measurement:
//!
//! * **Per safepoint: nothing.** There is no deopt poll anywhere. The only entry into this file
//!   is the trap edge itself.
//! * **Per call: nothing.** No table is probed, no epoch is compared, no version word is loaded.
//!   The ledger added below is touched ONLY inside `rava_deopt`, i.e. on a path that is already
//!   rebuilding a frame and interpreting a method — a hash probe there is unmeasurable, and it is
//!   deliberately not on any path that is already cheap (the tree has twice measured +3% / +2.3%
//!   for exactly that mistake).
//! * **Per emitted site: real, and it is the only real cost.** `emit_deopt_exit` writes a cold
//!   block that spills EVERY local and EVERY operand-stack slot. Those spills make every local
//!   and every `SV` live all the way to the trap edge, which is register-allocator pressure
//!   spread over the whole method — a milder form of exactly the damage the diamonds were blamed
//!   for (+0.81%, +2.42%). Plus one `Box::leak`ed `DeoptSite` per site (~40 B + one tag byte per
//!   slot), permanent.
//! * **Per RECOMPILE: the leak repeats.** The second compile leaks a fresh `DeoptSite` for every
//!   site it emits and the first one becomes unreachable garbage. Bounded only by the recompile
//!   budget enforced here.
//! * **Zero when no site is emitted** — and today no site is, because `RAVA_DEOPT` is opt-in.
//!
//! ## ⚠(3) The hazard recompilation introduces, which did not exist while a method compiled once
//!
//! Invalidation is only safe today because `sitemono::poison` disarms the failed speculation
//! before the recompile reads the profile. **That brake can silently fail.** `sitemono` is a
//! 65,536-slot direct-mapped table with no chaining, and `poison` writes nothing at all when the
//! slot has been stolen by a colliding site:
//!
//! ```text
//! if s.caller == caller && s.cp == cp { s.distinct.store(3) }   // else: silently a no-op
//! ```
//!
//! A boot warms tens of thousands of sites, so eviction is ordinary, not exotic. A lambda site is
//! worse: its speculation is keyed by SITE, not by `(caller, cp)` class identity, so a
//! `(caller, cp)` poison need not describe it at all. In any of those cases the recompile reads
//! the same profile, emits the same speculation, traps again, invalidates again — an unbounded
//! Cranelift ping-pong, each lap paying a full compile (regalloc alone is 2.82% of phase A) and
//! leaking another `DeoptSite`. Nothing in the tree bounds it, because until now a method was
//! compiled once and the question could not arise.
//!
//! So this file now keeps a **per-method recompile budget** and refuses past it. Refusing is not
//! a correctness compromise: the method simply keeps running the body it has and keeps taking its
//! deopts, which is precisely today's measured, correct behaviour (727,602 deopts a boot). An
//! unbounded compile loop is not.

use crate::value::Value;

#[cfg(all(test, feature = "next-gen-gc"))]
#[path = "deopt_exit_tests.rs"]
mod exit_tests;

/// One deopt site's frame state, leaked at compile time and addressed by a baked constant.
///
/// `tags` is one byte per slot — locals first, then operand stack — using the same encoding as
/// `tag_of`. A slot the compiler could not prove is never emitted, so every tag here is real.
pub(crate) struct DeoptSite {
    /// Bytecode index to resume at. The INVOKE itself, so the interpreter re-executes the call
    /// the guard refused: the compiled side has done nothing that must not happen twice.
    pub(crate) pc: u32,
    /// The site this trap belongs to — the caller's class pointer and cp index. A trap that
    /// FIRES poisons its site so nothing speculates there again: HotSpot keeps a per-bci trap
    /// count for exactly this reason, and without it a bad guess is paid for on every call
    /// forever. Measured: 727,602 deopts a boot across 89 sites before this existed.
    pub(crate) caller: usize,
    pub(crate) cp: u16,
    /// True when the enclosing method returns a REFERENCE. `jit_call_body` packs those with
    /// `box_ref` and everything else with `pack_local`, and getting that backwards is invisible
    /// until a bit-exact gate hashes the result: every gate went red on an unconditional deopt
    /// at a site whose method returns a reference.
    pub(crate) ret_ref: bool,
    pub(crate) nlocals: u16,
    pub(crate) nstack: u16,
    pub(crate) tags: Vec<u8>,
}

pub(crate) const T_INT: u8 = 0;
pub(crate) const T_LONG: u8 = 1;
pub(crate) const T_FLOAT: u8 = 2;
pub(crate) const T_DOUBLE: u8 = 3;
pub(crate) const T_REF: u8 = 4;

pub(crate) fn tag_of(t: crate::jit::JT) -> u8 {
    match t {
        crate::jit::JT::Int => T_INT,
        crate::jit::JT::Long => T_LONG,
        crate::jit::JT::Float => T_FLOAT,
        crate::jit::JT::Double => T_DOUBLE,
        crate::jit::JT::Ref => T_REF,
    }
}

/// Deopts that actually happened, and the sites emitted — a guard that never fires and a guard
/// that always fires are both wrong, and only counters tell them apart. Three mechanisms in this
/// tree were dead behind green gates before anyone counted their arms.
pub(crate) static DEOPT_TAKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static DEOPT_SITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static DEOPT_INVALIDATED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static DEOPT_THROWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn deopt_taken() -> u64 {
    DEOPT_TAKEN.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn deopt_sites() -> u64 {
    DEOPT_SITES.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn deopt_invalidated() -> u64 {
    DEOPT_INVALIDATED.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn deopt_thrown() -> u64 {
    DEOPT_THROWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Recompile requests this file REFUSED, by reason. A guard that never fires and a guard that
/// always fires are both wrong, and only counters tell them apart — the same rule that put
/// `DEOPT_SITES` next to `DEOPT_TAKEN`. `[0]` = over budget, `[1]` = not the main thread.
static DEOPT_RECOMPILE_REFUSED: [std::sync::atomic::AtomicU64; 2] =
    [std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0)];

/// (over-budget, wrong-thread) recompile refusals.
#[allow(dead_code)] // read by a dump; kept next to the counters it explains.
pub(crate) fn deopt_recompile_refused() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        DEOPT_RECOMPILE_REFUSED[0].load(Relaxed),
        DEOPT_RECOMPILE_REFUSED[1].load(Relaxed),
    )
}

/// Kill switch, DEFAULT ON. `RAVA_NO_FAST_DEOPT2=1` (or the global `RAVA_NO_EMIT_MORE`) restores
/// the previous invalidation exactly: unconditional, unbudgeted, and from whichever thread the
/// trap fired on.
///
/// ⚠That previous behaviour includes the thread race documented on `supersede_at_entry`. The
/// switch exists so an A/B can attribute a number to this change, not because the old shape is
/// safe; do not run production under it.
#[cfg(feature = "next-gen-gc")]
fn fast_deopt2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_DEOPT2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// How many times one method may be dropped back to `Cold`. Two is the shape a tiered engine
/// actually needs: the blind first compile, one profile-driven recompile, and one correction if
/// that recompile's speculation turns out wrong. A third lap is a ping-pong, not a tier-up.
/// `RAVA_DEOPT_RECOMPILE_MAX=<n>` to explore.
#[cfg(feature = "next-gen-gc")]
fn recompile_max() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_DEOPT_RECOMPILE_MAX")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(2)
    })
}

// ---------------------------------------------------------------------------
// THE RECOMPILE LEDGER
// ---------------------------------------------------------------------------
//
// One `u32` per method that has ever been superseded, keyed by its `Code` pointer — the same
// identity `jit_cache` is keyed by, so no name is built and no string is hashed.
//
// Direct-mapped with a short linear probe, and it may only ever REFUSE. A key that cannot find a
// slot is denied a recompile rather than granted one: the failure mode of a lost slot is "this
// method keeps the code it has", which is today's correct behaviour, instead of "this method
// recompiles forever", which is the hazard the ledger exists to stop. Same discipline as
// `sitemono`: silence, never a wrong answer.
//
// SIZE. 4,096 slots against a compiled population of ~1,950 methods — load factor under 0.5, so
// the probe almost always terminates on the first or second slot. 64 KB of BSS that stays on
// untouched (zero) pages until a deopt actually fires, and it is read ONLY from `rava_deopt`, so
// it never competes for L1 with anything on a working path. (The tree has one measurement of a
// cache that stopped fitting in L1 costing +6.8%; that table was probed per class-name lookup.
// This one is probed per deoptimisation.)
#[cfg(feature = "next-gen-gc")]
const LEDGER_SLOTS: usize = 4096;
#[cfg(feature = "next-gen-gc")]
const LEDGER_PROBE: usize = 8;

#[cfg(feature = "next-gen-gc")]
struct LedgerSlot {
    /// The method's `Code` pointer; 0 = free.
    key: std::sync::atomic::AtomicUsize,
    /// Times this method has been dropped back to `Cold`.
    n: std::sync::atomic::AtomicU32,
}

#[cfg(feature = "next-gen-gc")]
static LEDGER: std::sync::OnceLock<Vec<LedgerSlot>> = std::sync::OnceLock::new();

#[cfg(feature = "next-gen-gc")]
fn ledger() -> &'static [LedgerSlot] {
    LEDGER.get_or_init(|| {
        (0..LEDGER_SLOTS)
            .map(|_| LedgerSlot {
                key: std::sync::atomic::AtomicUsize::new(0),
                n: std::sync::atomic::AtomicU32::new(0),
            })
            .collect()
    })
}

/// Claim one recompile for `code_key`, or answer `false` when the budget is spent (or when no
/// slot is available, which is deliberately the same answer).
///
/// Racy by construction — two threads can both claim the same lap. That is harmless: the worst a
/// lost race buys is one extra compile, and the count still saturates, so the loop is still
/// bounded. Taking a lock here to make an approximate brake exact would be the wrong trade.
#[cfg(feature = "next-gen-gc")]
fn ledger_claim(code_key: usize) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if code_key == 0 {
        return false;
    }
    let t = ledger();
    // Multiply-shift over the pointer with the low alignment bits discarded — a `Code` address is
    // at least 8-aligned, so hashing them in would waste three bits of the index.
    let mut i = (((code_key >> 3) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - 12)) as usize
        & (LEDGER_SLOTS - 1);
    let cap = recompile_max();
    for _ in 0..LEDGER_PROBE {
        let s = &t[i];
        let k = s.key.load(Relaxed);
        if k == 0 {
            // Claim the slot. `compare_exchange` because a concurrent deopt may be claiming the
            // same free slot for a DIFFERENT method; losing the race means falling through to
            // re-inspect this slot as an occupied one.
            if s.key
                .compare_exchange(0, code_key, Relaxed, Relaxed)
                .is_ok()
            {
                s.n.store(1, Relaxed);
                return true;
            }
        }
        if s.key.load(Relaxed) == code_key {
            let n = s.n.load(Relaxed);
            if n >= cap {
                return false;
            }
            s.n.store(n + 1, Relaxed);
            return true;
        }
        i = (i + 1) & (LEDGER_SLOTS - 1);
    }
    false // table full along this probe run: refuse, never grant
}

/// **ENTRY-ONLY SUPERSEDE: stop handing out a compiled body without disturbing any frame that is
/// currently running it.**
///
/// This is the whole of what a recompile needs from this file, and it is deliberately separate
/// from the deopt exit below: a body being replaced because a better one can now be built is
/// still correct, so nothing may be interrupted, rebuilt or migrated. The next `Vm::jit_run`
/// entry sees `Cold` at one below the threshold and compiles again; every frame already inside
/// the old body runs it to completion against code that is never freed.
///
/// Returns whether the flip happened. Refuses, and says which, when:
///
/// * **the budget is spent** — see the ping-pong hazard in the module header. The method keeps the
///   code it has and keeps taking its deopts, which is correct and bounded;
/// * **this is not a thread that may mutate `Vm`.** ⚠The old inline invalidation did not check.
///   `vm_ref_worker_sanctioned` is `vm_ref_unchecked` — a bare TLS read with no thread test — so a
///   trap firing inside a gil-free region was writing `Vm::jit_cache` (a `FastMap`, which can
///   rehash and move its buckets) while the main thread read it. That is the exact class of
///   mutation `Vm::debug_assert_not_gilfree` was built to catch and that `jit_run` refuses
///   compilation for. It has been latent only because deopt is opt-in and rarely fires on a
///   worker; this round is about to make deopt carry more traffic, so it is closed now.
///   Skipping is semantically NEUTRAL — the flip is a request to recompile, not a fact anyone
///   depends on, so dropping it costs one missed tier-up and a later main-thread deopt re-asks.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn supersede_at_entry(vm: &mut crate::Vm, code_key: usize) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if !crate::jit::deopt_invalidate() {
        return false;
    }
    // Only the owning thread may touch `jit_cache`. This safety boundary must
    // not depend on the fast-deopt experiment's kill switch.
    if crate::gil::is_worker() || crate::parallel::gilfree() {
        DEOPT_RECOMPILE_REFUSED[1].fetch_add(1, Relaxed);
        return false;
    }
    if fast_deopt2_enabled() {
        if !ledger_claim(code_key) {
            DEOPT_RECOMPILE_REFUSED[0].fetch_add(1, Relaxed);
            return false;
        }
    }
    // One below the threshold, so the next entry crosses it. ⚠`jit_run` interprets unconditionally
    // when the stored count is 0 (`n == 0` is its own arm), so a threshold of 1 would cost one
    // extra interpreted entry — which is harmless here and in fact warms the profile the recompile
    // is being requested for. Under `RAVA_JIT_TIER0` the threshold is 16, so this lands on Cold(15)
    // and the very next entry recompiles.
    crate::vm::jit_run::clear_jit_entry_hint(code_key);
    let t = vm.jit_threshold;
    vm.jit_cache
        .insert(code_key, crate::jit::JitStatus::Cold(t.saturating_sub(1)));
    DEOPT_INVALIDATED.fetch_add(1, Relaxed);
    true
}

/// Leave compiled code and finish the method in the interpreter.
///
/// Returns the method's result as a packed cell, or sets `jit_pending` and returns 0 on a throw —
/// the same protocol every other callback here uses.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_deopt(class_ptr: i64, code_key: i64, site: i64, buf: *const i64) -> i64 {
    use std::sync::atomic::Ordering::Relaxed;
    DEOPT_TAKEN.fetch_add(1, Relaxed);
    // Rebuilding an interpreter frame mutates shared VM state and is never a
    // legal worker continuation. Abort the transaction before reading site or
    // VM pointers; the coordinator will retry the region serially.
    if crate::gil::is_worker() || crate::parallel::gilfree() {
        crate::execution_state::pending_error_set_first(crate::VmError::RegionAbort);
        return 0;
    }
    // Poison the site before doing anything else: whatever happens next, this speculation was
    // wrong here and must not be made again.
    let poisoned_code = {
        let s: &DeoptSite = unsafe { &*(site as *const DeoptSite) };
        crate::sitemono::poison(s.caller, s.cp);
        code_key as usize
    };
    let Some(vm): Option<&mut crate::Vm> = crate::jit::vm_ref_worker_sanctioned() else {
        return 0;
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    // ⭐**INVALIDATION: poisoning the site is useless while the OLD code keeps running.**
    // Measured — with poisoning alone the deopt count did not move by a single digit (727,602
    // both ways), because a method here is compiled once and the emitted speculation lives for
    // the process. HotSpot's traps are rare precisely because a trap that fires REMOVES itself.
    //
    // So drop this method back to Cold at the compile threshold: the next entry through
    // `jit_enter` recompiles it, and the site is poisoned by then so the speculation is not
    // emitted again. The old machine code is never freed — other methods may hold a baked
    // pointer into it — it is simply no longer handed out, which is the cheap half of
    // invalidation and the only half this engine needs.
    //
    // Keep invalidation behind the bounded, thread-checked entry replacement.
    // Profile changes no longer call this operation.
    supersede_at_entry(vm, poisoned_code);
    let site: &DeoptSite = unsafe { &*(site as *const DeoptSite) };
    let code: &rava_classfile::Code = unsafe { &*(code_key as *const rava_classfile::Code) };
    let n = site.nlocals as usize + site.nstack as usize;
    let cells = unsafe { std::slice::from_raw_parts(buf, n) };

    // Rebuild the frame's values. A reference cell goes through the same unboxing every native
    // boundary uses, so a pinned/arena/handle pointer all decode the way the interpreter expects.
    let mut vals: Vec<Value> = Vec::with_capacity(n + code.max_stack as usize);
    for (i, &c) in cells.iter().enumerate() {
        let t = site.tags.get(i).copied().unwrap_or(T_REF);
        vals.push(match t {
            T_INT => Value::Int(c as i32),
            T_LONG => Value::Long(c),
            T_FLOAT => Value::Float(f32::from_bits(c as u32)),
            T_DOUBLE => Value::Double(f64::from_bits(c as u64)),
            _ => crate::jit::unbox_ref(vm, c, ""),
        });
    }

    // The class Arc: bump the count and take ownership of one, exactly as `jit_call_body` does,
    // so the frame holds a live reference without disturbing the loader's count.
    let class: std::sync::Arc<crate::class_loader::LoadedClass> = unsafe {
        std::sync::Arc::increment_strong_count(class_ptr as *const crate::class_loader::LoadedClass);
        std::sync::Arc::from_raw(class_ptr as *const crate::class_loader::LoadedClass)
    };

    if { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); crate::jit::env_gate("RAVA_DEOPT_LOG", &ON) } {
        eprintln!(
            "[DEOPT] {} pc={} nlocals={} (max_locals={}) nstack={} (max_stack={}) ret_ref={} op={:#x} tags={:?}",
            class.file.this_class,
            site.pc,
            site.nlocals,
            code.max_locals,
            site.nstack,
            code.max_stack,
            site.ret_ref,
            code.bytecode.get(site.pc as usize).copied().unwrap_or(0),
            site.tags
        );
    }

    let mut frame = crate::vmframe::Frame::from_parts(
        code as *const rava_classfile::Code,
        vals,
        site.nlocals as usize,
        class,
    );
    frame.pc = site.pc as usize;

    // The frame's buffer is a GC root for the duration, the same way an interpreted frame is.
    crate::frames::push(std::ptr::addr_of!(frame) as usize);
    let r = crate::opcodes::run(vm, code, &mut frame);
    crate::frames::pop();

    match r {
        // `Normal(Some(v))` is a returning method; any other exit (a staged call, a bounded stop,
        // a park) means the interpreter did not finish the frame, and this transition has no way
        // to represent that — refuse rather than invent a value.
        Ok(crate::opcodes::RunExit::Normal(Some(v))) => {
            if site.ret_ref {
                crate::jit::box_ref(vm, &v)
            } else {
                crate::jit::pack_local(&v)
            }
        }
        Ok(crate::opcodes::RunExit::Normal(None)) => 0,
        Ok(exit) => {
            let kind = match exit {
                crate::opcodes::RunExit::Call => "Call",
                crate::opcodes::RunExit::Yield => "Yield",
                #[cfg(feature = "next-gen-gc")]
                crate::opcodes::RunExit::Bounded => "Bounded",
                #[cfg(feature = "next-gen-gc")]
                crate::opcodes::RunExit::NurseryFull => "NurseryFull",
                #[cfg(feature = "next-gen-gc")]
                crate::opcodes::RunExit::ClinitPending(_) => "ClinitPending",
                crate::opcodes::RunExit::Normal(_) => unreachable!(),
            };
            crate::execution_state::pending_error_set_first(crate::VmError::UnsupportedNative(
                format!("nonterminal JIT deopt exit: {kind}")));
            vm.heap.set_jit_fault();
            0
        }
        Err(crate::VmError::Throw(v)) => {
            DEOPT_THROWN.fetch_add(1, Relaxed);
            vm.jit_pending_set(v);
            vm.heap.set_jit_fault();
            0
        }
        Err(error) => {
            crate::execution_state::pending_error_set_first(error);
            vm.heap.set_jit_fault();
            0
        }
    }
}
