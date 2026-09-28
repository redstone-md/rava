//! **THE CALL A JVM DOES NOT MAKE.**
//!
//! `collect_inline_targets` binds a callee by TYPE: a static, a special, or a virtual whose
//! method is final-effective. An INTERFACE call can never bind that way, and neither can a
//! virtual call to an overridable method — so `List.size()`, `List.get()`, `Iterator.hasNext()`
//! and `Map.get()`, the collection API as real code writes it, stayed real calls. On
//! `bench/StreamApplyCost.java` the inliner logged **zero** collapses.
//!
//! A JVM binds those by PROFILE. A site that has only ever seen one concrete receiver class gets
//! that class's body spliced in behind a class guard, and `size()` becomes a field load. That is
//! the difference this file closes, and it is why two months of levers measured nothing: every
//! one of them made a CALL cheaper, and a cheap call cannot beat an absent call.
//!
//! Census that justified building it (`RAVA_DIAG=devirt`, product boot): **76.8% of virtual and
//! interface calls are monomorphic**, 86% mono-or-bi, and 723 sites are admissible today.
//!
//! ## The shape emitted
//!
//! ```text
//!            is_arena(recv) ? guard : slow
//!   guard:   load class_id(recv) == K ? inline : slow
//!   inline:  <callee body spliced, no call>      -> merge(result, refs…)
//!   slow:    <the ordinary dispatch for this site> -> merge(result, refs…)
//!   merge:   result and every live reference arrive as block params
//! ```
//!
//! ## …and the shape a LAMBDA SAM site adds
//!
//! ```text
//!   guard:   load class_id(recv) == K ? site : slow      (K = the capture signature)
//!   site:    load recv[AL_SITE_SLOT] == S ? caps : slow  (S = the interned impl site)
//!   caps:    every REFERENCE capture cell & 3 == 0 ? inline : slow
//!   inline:  <implementation body spliced; captures loaded from recv's cells>
//! ```
//!
//! The extra two questions are what a class-id compare cannot answer: a lambda's class is its
//! CAPTURE SIGNATURE, shared by every closure of that shape in the program, so it identifies a
//! layout and not an implementation; and a capture cell is in the ARENA encoding, which agrees
//! with the JIT operand encoding on primitives and on the untagged pointer/null form, and on
//! nothing else. A void SAM adds one more difference: the merge carries no result at all.
//!
//! ## Two things that make it safe
//!
//! **The guard, not the profile, is what makes it correct.** A stale profile, a class first seen
//! after compilation, or a hash collision in the site table can all make the speculation WRONG —
//! and a wrong speculation takes the `slow` arm, which is the same code the site ran before. The
//! cost of being wrong is one compare on a path that was already making a call.
//!
//! **Live references cross the slow arm and must be re-materialised.** The ordinary dispatch can
//! run guest code, collect, and MOVE objects, so every `JT::Ref` on the operand stack below this
//! call is stale afterwards. `emit_dispatch_pic` solves this by passing each live ref through its
//! merge block's parameters; this diamond does the same, and the inline arm — which moves
//! nothing — passes its unchanged values through the same parameters so both arms agree.
//! Getting this wrong would not fail a gate: it produces a stale pointer that survives until the
//! next collection touches it.

use super::inline::{emit_inline, inline_emit2_enabled};
use crate::jit::*;

/// ⭐⭐**THE GUARD THAT PINS A CLOSURE'S IMPLEMENTATION — the one a class-id compare cannot be.**
///
/// Emitted on the arm where `class_id(recv) == K` already holds, so `recv` is known to be an arena
/// object of the profiled lambda CLASS. That proves the capture SIGNATURE (cell count and cell
/// kinds) and nothing else: every closure in the program that captures the same shape shares that
/// class, so the body about to be spliced could still belong to a different indy site. Two further
/// facts close the gap, in this order:
///
///  1. **The site.** `HDR + AL_SITE_SLOT*8` holds the interned `LambdaSite` index the closure was
///     minted with — the same cell, at the same offset, that `emit_table_probe` reads for its
///     `LAMBDA_KEY_BASE + site_idx` key, so there is one rule and two transcriptions of it. A
///     `LambdaSite` is content-interned over (owner, name, descriptor), so equality here means the
///     spliced body IS this closure's implementation.
///  2. **The capture cells the splice will read as JIT operands.** A capture cell carries the
///     ARENA encoding. For a primitive that is bit-identical to the JIT operand cell and needs no
///     test at all. For a REFERENCE the two encodings agree only on the untagged form: arena tags
///     a heap handle `(h<<2)|1`, an array `(h<<2)|2`, a box `(idx<<2)|3`, none of which mean
///     anything to a compiled body that expects `h|1<<40`, `h|1<<42`, `idx|1<<41`. So each
///     reference capture is required to be untagged (`cell & 3 == 0`), which admits exactly the
///     null and arena-pointer forms — the two the encodings share. A capture position that is the
///     implementation's RECEIVER additionally has to be non-null and pointer-shaped, so it gets
///     the full `emit_is_arena`, because the spliced body will dereference it without a null check
///     the way `invokespecial` would have.
///
/// Every failure branches to `fail`, which the caller has made the site's ordinary dispatch (or,
/// under deopt, the interpreter re-executing the call). Being wrong costs the compares; it can
/// never run the wrong body. Leaves the builder in a fresh sealed block dominated by all of them.
#[cfg(feature = "next-gen-gc")]
fn emit_sam_site_guard(
    b: &mut FunctionBuilder,
    recv: Value,
    sam: &SamInline,
    is_static: bool,
    fail: cranelift::prelude::Block,
) {
    let site_off = (crate::nextgen::HDR + crate::nextgen::AL_SITE_SLOT * 8) as i32;
    let site = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(site_off));
    let same_site = b.ins().icmp_imm(IntCC::Equal, site, i64::from(sam.site_idx));
    let next = b.create_block();
    b.ins().brif(same_site, next, &[], fail, &[]);
    b.switch_to_block(next);
    b.seal_block(next); // exactly one predecessor: the branch above
    for (p, &is_ref) in sam.cap_is_ref.iter().enumerate() {
        if !is_ref {
            continue; // primitive: the arena cell IS the JIT cell, bit for bit
        }
        let off = (crate::nextgen::HDR + (crate::nextgen::AL_HDR_CELLS + p) * 8) as i32;
        let cell = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(off));
        let low = b.ins().band_imm(cell, 3);
        let untagged = b.ins().icmp_imm(IntCC::Equal, low, 0);
        // Position 0 of an INSTANCE implementation is the body's `this`. `invokespecial` would
        // have null-checked it; a splice does not, so require the pointer form outright.
        let ok = if p == 0 && !is_static {
            let arena = emit_is_arena(b, cell);
            b.ins().band(untagged, arena)
        } else {
            untagged
        };
        let nb = b.create_block();
        b.ins().brif(ok, nb, &[], fail, &[]);
        b.switch_to_block(nb);
        b.seal_block(nb);
    }
}

/// What happened at a guarded site. ⚠**The three cases are not interchangeable, and conflating
/// two of them was this feature's first bug:** returning a bare `None` for "declined" made the
/// caller abort the whole method's compilation, because `None` out of an emitter means "this
/// method cannot be compiled". A gate went red with the guard emitting NOTHING at all.
pub(crate) enum Guarded {
    /// Nothing was emitted and the operand stack is untouched: the caller must emit the ordinary
    /// call for this site.
    Declined,
    /// The diamond is complete and the result is on the stack.
    Emitted,
    /// Emission began and then failed. No fall-back is possible — a block was left without a
    /// terminator — so the caller must abort this compilation.
    Aborted,
}

/// Emit the guarded diamond for a profile-devirtualised site. `slow` is the emission that would
/// have run at this site with no inlining at all; it is called with the operand stack restored,
/// exactly as it found it.
///
/// Every fallible CHECK happens before the first instruction is emitted, so `Declined` always
/// means the stack and the block are exactly as they were found.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_guarded_inline<F>(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    env: &Env,
    array_desc_cell: Value,
    callee: &InlineCallee,
    locals_ptr: Value,
    cow: CowArm,
    // The compiled method's local Variables and this site's bytecode index: the frame state a
    // deopt exit needs. Unused when deopt is off.
    vars: &[Variable],
    pc: usize,
    site_cp: u16,
    #[cfg(feature = "next-gen-gc")] licm: Option<Licm>,
    #[cfg(feature = "next-gen-gc")] dv: Option<DeepVars>,
    mut slow: F,
) -> Guarded
where
    F: FnMut(&mut FunctionBuilder, &mut Vec<SV>) -> Option<()>,
{
    // Two worlds, two guards. An ARENA receiver is pinned by its class id; a HEAP-REF receiver by
    // its class word, read exactly as `emit_heap_ref_class_key` reads it. The second is the one
    // the collections need — arena-only admission reached 190 Minecraft classes and zero
    // collections, because every collection receiver lives on the handle heap.
    let arena_cid = callee.guard_class;
    // Extra receiver classes that resolve to the SAME body (see `InlineCallee::guard_class_more`).
    let arena_cid_more: &[u32] = &callee.guard_class_more;
    let ref_word = callee.guard_ref_word;
    if arena_cid.is_none() && ref_word.is_none() {
        return Guarded::Declined;
    }
    // The lambda SAM plan, when the admission built one. `None` ⇒ every line below behaves
    // exactly as it did, which is what makes `RAVA_NO_LAMBDA_INLINE_CHAIN=1` (consulted where the
    // plan is built) a true kill switch rather than a second code path.
    #[cfg(feature = "next-gen-gc")]
    let sam = callee.sam.as_ref();
    // A SAM site's receiver is an ARENA closure by construction, so it is guarded by class id and
    // never by a heap-Ref class word. Refuse the combination rather than emit half a guard.
    #[cfg(feature = "next-gen-gc")]
    if sam.is_some() && (ref_word.is_some() || arena_cid.is_none()) {
        return Guarded::Declined;
    }

    // ⛔⭐**A CLASS-ID COMPARE CANNOT PIN A CLOSURE'S IMPLEMENTATION, AND THIS IS THE ONE SHAPE
    // WHERE THAT MATTERS.**
    //
    // An arena lambda's class id is its CAPTURE SIGNATURE — one class per (kind, kind, …) tuple,
    // shared by every closure in the program that captures that shape. `nextgen::lambdas` says so
    // in as many words beside `LAMBDA_KEY_BASE` (ledger rows 332/333): *"the per-site dispatch
    // table is indexed by receiver class id, and every lambda of one capture signature shares one
    // — so it cannot serve a lambda without calling another site's implementation"*, and the cure
    // there was to key by `LAMBDA_KEY_BASE + site_idx` (`lambda_table_key`), read out of the
    // object's own `AL_SITE_SLOT` cell. Row 333 also records the alternative — a class per site —
    // being tried and costing **8×**, because a class is consulted per object and per birth.
    //
    // A class-id compare followed by a splice, on a lambda receiver, is satisfied by EVERY closure
    // of the same capture shape — so the spliced body could be another site's `lambda$…$N`. That
    // is not a mispredict the cold arm catches; it is the wrong code running with the guard
    // reporting a hit.
    //
    // ⚠**AND SOMETHING DOES ADMIT LAMBDA CALLEES NOW**, so this is no longer a costless
    // precaution: `vm/inline_targets.rs` builds them from the interpreter's own SAM-apply profile.
    // The refusal therefore stays for exactly the case it was written about — a lambda class id
    // with no `SamInline` behind it — and the case that HAS one goes through the site guard below
    // instead. There is no third path.
    //
    // ⇒ **THE GUARD THAT WORKS, AND IT IS NOW BUILT: `emit_sam_site_guard` above.** Keep the
    // receiver-kind test, then compare the cell at `HDR + AL_SITE_SLOT*8` against the site index
    // the profile observed — the same two loads `emit_table_probe` already emits for its lambda
    // key, in the same order, so there is one rule and two transcriptions of it. `InlineCallee`
    // now carries the site index (`SamInline`, `jit/model/plans.rs`) and the admission
    // (`vm/inline_targets.rs`) fills it in from the interpreter's own SAM-apply profile.
    //
    // ⚠**SO THE REFUSAL STAYS, NARROWED TO EXACTLY ITS ORIGINAL SUBJECT:** a lambda class id with
    // NO site plan behind it is still a class-id compare pretending to pin an implementation, and
    // is still refused. The refusal is also no longer wrapped in `lambda_loop_inline_enabled()` —
    // that made the protection disappear whenever that unrelated kill switch was thrown, which
    // was harmless only while nothing could admit a lambda callee. Something can now.
    #[cfg(feature = "next-gen-gc")]
    if sam.is_none() {
        if let Some(cid) = arena_cid {
            if crate::nextgen::lambdas::is_lambda_cid(cid) {
                return Guarded::Declined;
            }
        }
    }
    // BISECTION: emit the guard for at most `RAVA_DEVIRT_LIMIT` sites, so the first miscompiling
    // callee can be found by binary search instead of by reading 723 of them.
    {
        static LIMIT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        let lim = *LIMIT.get_or_init(|| {
            std::env::var("RAVA_DEVIRT_LIMIT").ok().and_then(|v| v.parse::<u64>().ok())
        });
        if let Some(lim) = lim {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n >= lim {
                return Guarded::Declined;
            }
            if std::env::var_os("RAVA_DEVIRT_LOG").is_some() {
                eprintln!(
                    "[DEVIRT {n}] {}{} guard={}",
                    callee.class.file.this_class,
                    callee.descriptor,
                    match (arena_cid, ref_word) {
                        (Some(c), _) => format!("arena_cid:{c}"),
                        (_, Some(w)) => format!("ref_word:{w:#x}"),
                        _ => "none".to_string(),
                    }
                );
            }
        }
    }
    // `None` = a VOID callee, which is now a legal shape: `emit_inline` splices it and pushes
    // nothing, and the diamond below carries no result through its merge. Only the lambda SAM
    // chain admits one (`is_inlinable_kind`), so every other site still arrives with a value.
    let ret = crate::jit::ret_jt(&callee.descriptor);
    // Operands ABOVE the receiver. For a SAM site that is the interface method's arity, NOT the
    // implementation's — the implementation's leading parameters are the captures, which are not
    // on the stack at all.
    #[cfg(feature = "next-gen-gc")]
    let nargs = match sam {
        Some(s) => s.sam_nargs,
        None => crate::count_args(&callee.descriptor),
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let nargs = crate::count_args(&callee.descriptor);
    // [.. refs .., receiver, arg0 .. argN-1]
    if stack.len() < nargs + 1 {
        return Guarded::Declined;
    }
    let recv_idx = stack.len() - 1 - nargs;
    let recv = stack[recv_idx].v;

    // ⭐**THE CALLER'S OWN PROOF, READ INSTEAD OF RE-TESTED.**
    //
    // `SV::arena` is a PROOF, not a hint. It is only ever set from a slot an enclosing
    // `emit_is_arena` has already branched on (the method-entry unswitch, or a guard exactly like
    // the one below), and arena-ness is a LIFETIME property of the object — an evacuation moves
    // an arena object to another arena, a heap handle is never rewritten into a raw pointer — so a
    // proof taken further up still holds at this site even across a collection. Two consequences,
    // and they point in opposite directions:
    //
    //  * A `guard_ref_word` site tests `recv >> 40 == 1`, i.e. "this is a HANDLE". On a receiver
    //    already proven to be a raw arena pointer that test can never hit, so the diamond would be
    //    a guaranteed-cold spliced body, a merge, and a live range per stack reference, in exchange
    //    for nothing. DECLINE: the site keeps the ordinary call it would have emitted anyway. This
    //    is the shape whose two earlier attempts measured +0.81% and +2.42%, bought for zero hits.
    //
    //  * An arena-id site's own `emit_is_arena` is then redundant — only the class-id compare is
    //    new information — so that half of the guard is dropped below.
    #[cfg(feature = "next-gen-gc")]
    let recv_proven_arena = inline_emit2_enabled() && stack[recv_idx].arena;
    #[cfg(not(feature = "next-gen-gc"))]
    let recv_proven_arena = false;
    if recv_proven_arena && ref_word.is_some() {
        return Guarded::Declined;
    }

    // ⭐**THE TRAP SHAPE.** With deopt available the failing edge leaves the function, so there is
    // no merge, no block parameter per live reference, and no second live range — the inline body
    // continues in straight-line code. That is the whole difference between this and the diamond
    // whose two attempts measured +0.81% and +2.42%.
    //
    // The deopt is emitted with the stack UNTOUCHED — receiver and arguments still on it and the
    // pc pointing at the invoke — because the interpreter re-executes the call the guard refused.
    //
    // ⚠**AND IT NEEDS A FRAME DESCRIPTION IT CAN ACTUALLY BUILD.** `emit_deopt_exit` returns
    // false without `env.local_types`, and by then the blocks are wired, so the only exit left is
    // `Aborted` — the whole method's compilation refused. Asking the question BEFORE anything is
    // emitted turns that into a plain `Declined`, i.e. the site keeps the call it would have
    // emitted anyway. This is the shape a guarded site inside a SPLICED body always has, because
    // `emit_inline` clears the map there (a callee's PCs are not the outer method's).
    #[cfg(feature = "next-gen-gc")]
    if crate::jit::deopt_enabled() && env.local_types.is_none() {
        return Guarded::Declined;
    }
    #[cfg(feature = "next-gen-gc")]
    if crate::jit::deopt_enabled() {
        let inl_t = b.create_block();
        let trap = b.create_block();
        let guard_t = b.create_block();

        if let Some(word) = ref_word {
            let hi = b.ins().ushr_imm(recv, 40);
            let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
            b.ins().brif(is_ref, guard_t, &[], trap, &[]);
            b.switch_to_block(guard_t);
            b.seal_block(guard_t);
            let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, trap);
            let want = b.ins().iconst(I64, if crate::jit::deopt_force() { -1 } else { word as i64 });
            let hit = b.ins().icmp(IntCC::Equal, cw, want);
            b.ins().brif(hit, inl_t, &[], trap, &[]);
        } else {
            // The receiver-kind half of the guard, skipped when an enclosing `emit_is_arena`
            // already proved it (see `recv_proven_arena`). `guard_t` keeps exactly one
            // predecessor either way, so the seal below is unchanged.
            if recv_proven_arena {
                b.ins().jump(guard_t, &[]);
            } else {
                let is_a = emit_is_arena(b, recv);
                b.ins().brif(is_a, guard_t, &[], trap, &[]);
            }
            b.switch_to_block(guard_t);
            b.seal_block(guard_t);
            let cid = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(0));
            let want = if crate::jit::deopt_force() {
                -1i64
            } else {
                i64::from(arena_cid.unwrap_or(0) as i32)
            };
            let hit = b.ins().icmp_imm(IntCC::Equal, cid, want);
            // A SAM site needs the SITE and the capture cells proven too; the class id alone is
            // the capture signature. `sam_t` exists only for that, so the ordinary site keeps
            // exactly the block count it had.
            if let Some(s) = sam {
                let sam_t = b.create_block();
                b.ins().brif(hit, sam_t, &[], trap, &[]);
                b.switch_to_block(sam_t);
                b.seal_block(sam_t);
                emit_sam_site_guard(b, recv, s, s.impl_is_static, trap);
                b.ins().jump(inl_t, &[]);
            } else {
                b.ins().brif(hit, inl_t, &[], trap, &[]);
            }
        }

        b.switch_to_block(trap);
        b.seal_block(trap);
        if !emit_deopt_exit(b, env, vars, stack, locals_ptr, pc, site_cp) {
            // The site could not be described exactly. The blocks are already wired, so there is
            // no clean fall-back: refuse this compilation rather than leave a block unterminated.
            return Guarded::Aborted;
        }

        b.switch_to_block(inl_t);
        b.seal_block(inl_t);
        // ADDED, compile-time only: SAM diamonds that actually reach machine code, split by where
        // the closure identity came from. Counted at EMIT and not at admission, because an admitted
        // callee whose caller then fails to compile removes no dispatch at all.
        #[cfg(feature = "next-gen-gc")]
        if let Some(s) = sam {
            crate::vm::inline_census::note(if s.static_prov {
                crate::vm::inline_census::E_SAM_PROV
            } else {
                crate::vm::inline_census::E_SAM_PROFILE
            });
        }
        // ARENA PROOF INHERITED FROM THE GUARD — see the diamond arm below for why it holds.
        #[cfg(feature = "next-gen-gc")]
        if ref_word.is_none() && inline_emit2_enabled() {
            stack[recv_idx].arena = true;
        }
        let ok = emit_inline(
            b, stack, env, array_desc_cell, callee, locals_ptr, cow,
            #[cfg(feature = "next-gen-gc")] licm,
            #[cfg(feature = "next-gen-gc")] dv,
            #[cfg(feature = "next-gen-gc")] None,
            #[cfg(feature = "next-gen-gc")] vars,
        );
        return if ok.is_some() { Guarded::Emitted } else { Guarded::Aborted };
    }

    // Every live reference STRICTLY BELOW the receiver survives this call and can be moved by
    // the slow arm. The receiver and the args above it are consumed by both arms.
    //
    // ⚠**Including the receiver here was the second bug**: after the arm ran, the stack was
    // `recv_idx` long and reading slot `recv_idx` panicked — inside the compiler thread, which
    // does not surface as a compile error but as a HANG, because the JIT waits for a result that
    // will never arrive. The Cranelift verifier (`RAVA_JIT_VERIFY=1`) is what turned the hang
    // into a one-line panic with a file and a line.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .take(recv_idx)
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    // BISECTION (`RAVA_DEVIRT_SLOWONLY=1`): emit ONLY what the site would have emitted anyway,
    // with no blocks at all. This separates "the diamond is wrong" from "the emission I chose for
    // the slow arm is wrong at this site".
    {
        static SO: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *SO.get_or_init(|| std::env::var_os("RAVA_DEVIRT_SLOWONLY").is_some()) {
            return if slow(b, stack).is_some() { Guarded::Emitted } else { Guarded::Aborted };
        }
    }
    let guard = b.create_block();
    let inl = b.create_block();
    let slow_blk = b.create_block();
    let merge = b.create_block();
    // A VOID callee produces no result, so the merge carries only the live references. `nres` is
    // that 0-or-1 and indexes everything below, so the two shapes share one piece of code.
    let nres = usize::from(ret.is_some());
    if nres == 1 {
        b.append_block_param(merge, I64);
    }
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    // ── the guard, one branch per world ──
    if let Some(word) = ref_word {
        // HEAP-REF: the tag, then the class word — read exactly as the four working inline arms
        // read it. This is the path the collections take.
        let hi = b.ins().ushr_imm(recv, 40);
        let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
        b.ins().brif(is_ref, guard, &[], slow_blk, &[]);
        b.switch_to_block(guard);
        b.seal_block(guard);
        let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow_blk);
        let want = b.ins().iconst(I64, word as i64);
        let hit = b.ins().icmp(IntCC::Equal, cw, want);
        b.ins().brif(hit, inl, &[], slow_blk, &[]);
    } else {
        let guard_class = arena_cid.unwrap_or(0);
        // The receiver-kind half of the guard, skipped when an enclosing `emit_is_arena` already
        // proved it. `guard` keeps exactly one predecessor either way (and `slow_blk` keeps at
        // least one, from the class-id compare below), so the seals are unchanged.
        if recv_proven_arena {
            b.ins().jump(guard, &[]);
        } else {
            let is_a = emit_is_arena(b, recv);
            b.ins().brif(is_a, guard, &[], slow_blk, &[]);
        }
        b.switch_to_block(guard);
        b.seal_block(guard);
        let cid = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(0));
    // BISECTION HOOK (`RAVA_DEVIRT_NEVER=1`): compare against an id no class can have, so the
    // diamond is emitted in full and the inline arm is never taken. A gate that passes with this
    // set and fails without it puts the fault in the spliced body or the predicate; a gate that
    // fails with it set puts the fault in the diamond and its merge.
    let never = {
        static N: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *N.get_or_init(|| std::env::var_os("RAVA_DEVIRT_NEVER").is_some())
    };
        let want = if never { -1i64 } else { i64::from(guard_class as i32) };
        let mut hit = b.ins().icmp_imm(IntCC::Equal, cid, want);
        // ⭐**THE DISJUNCTION IS THE WHOLE POLYMORPHIC EXTENSION.** Each extra id names another
        // receiver class whose resolution is the body already spliced below, so widening the
        // compare widens the guard's ACCEPTANCE without changing what runs behind it. Cranelift
        // folds these into a compare chain; the miss path is untouched and still the ordinary
        // dispatch. Under `RAVA_DEVIRT_NEVER` the extras are skipped too, so the bisection hook
        // keeps meaning "the inline arm is never taken".
        if !never {
            for &extra in arena_cid_more {
                let h = b.ins().icmp_imm(IntCC::Equal, cid, i64::from(extra as i32));
                hit = b.ins().bor(hit, h);
            }
        }
        // ⭐**THE SAM HALF OF THE GUARD.** The class-id compare has proved the capture SIGNATURE;
        // `emit_sam_site_guard` proves the SITE (hence the implementation) and that every
        // reference capture cell is in the one encoding a JIT operand and an arena cell share.
        // Its failures land on the same `slow_blk` — the site's ordinary dispatch — so a closure
        // from another site, or a capture that happens to hold a tagged handle, costs the
        // compares and then runs exactly the code it ran before.
        #[cfg(feature = "next-gen-gc")]
        if let Some(s) = sam {
            let sam_ok = b.create_block();
            b.ins().brif(hit, sam_ok, &[], slow_blk, &[]);
            b.switch_to_block(sam_ok);
            b.seal_block(sam_ok);
            emit_sam_site_guard(b, recv, s, s.impl_is_static, slow_blk);
            b.ins().jump(inl, &[]);
        } else {
            b.ins().brif(hit, inl, &[], slow_blk, &[]);
        }
        #[cfg(not(feature = "next-gen-gc"))]
        b.ins().brif(hit, inl, &[], slow_blk, &[]);
    }

    let snapshot = stack.clone();

    // --- the arm that makes no call ---
    b.switch_to_block(inl);
    b.seal_block(inl);
    *stack = snapshot.clone();
    // ⭐⭐**THE GUARD ALREADY PROVED WHAT EVERY FIELD SITE IN THE SPLICED BODY IS ABOUT TO
    // RE-TEST.** On this arm `emit_is_arena(recv)` has held (`recv != 0` and no tag bits above
    // bit 40) AND `class_id(recv) == K`. That is STRICTLY STRONGER than the method-entry unswitch,
    // which is the only other producer of `SV::arena` — the unswitch proves arena-ness alone,
    // this proves arena-ness plus the exact concrete class the callee's field slots were resolved
    // against. Yet without this line the spliced body re-emitted
    // `native_getfield_{ref,const_prim}_arena_dual` at every read of its own receiver: an
    // `emit_is_arena` (three instructions and a branch), a whole checked-handle block that the
    // guard has proven unreachable, and a merge phi — per field, per site. With it, each of those
    // collapses to `emit_arena_getfield_*_flat`, which is byte-for-byte the dual's arena arm with
    // the diamond deleted, so the value read is identical by construction.
    //
    // Three things keep it honest:
    //  * Only the KIND is hoisted, never the POINTER. A collection inside the body still reloads
    //    the receiver; arena-ness survives evacuation (arena → arena, and a handle is never
    //    rewritten into a raw pointer), which is the same invariance the entry unswitch relies on.
    //  * `emit_inline` re-checks `pinned_ref_slots` on the CALLEE before handing the proof to
    //    slot 0, so a callee that `astore`s over its receiver keeps its own guards. The proof
    //    expires exactly where it stops being true.
    //  * The heap-Ref arm proves the OPPOSITE (a handle), so it must never take this line — and
    //    a receiver already proven arena declines a ref-word site outright, further up.
    #[cfg(feature = "next-gen-gc")]
    if ref_word.is_none() && inline_emit2_enabled() {
        stack[recv_idx].arena = true;
    }
    // ⭐**DOES THE GUARD ACTUALLY HIT?** (`RAVA_DEVIRT_STATS=1`, off by default.) The profile is
    // taken from the INTERPRETER's warm-up, and this tree already recorded that such a profile
    // "biases toward monomorphism" — that is why a separate steady-state probe exists. If the
    // guards mostly MISS, every site pays a compare and a duplicated body and still makes the
    // call, which is exactly the shape of the interface-half regression (+13.5% on the probe).
    // One relaxed increment inside the taken arm answers it.
    if devirt_stats_enabled() {
        let addr = b.ins().iconst(I64, (&GUARD_HITS as *const _) as i64);
        let cur = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
        let inc = b.ins().iadd_imm(cur, 1);
        b.ins().store(MemFlags::trusted(), inc, addr, Offset32::new(0));
    }
    // ADDED, compile-time only: SAM diamonds that actually reach machine code, split by where
    // the closure identity came from. Counted at EMIT and not at admission, because an admitted
    // callee whose caller then fails to compile removes no dispatch at all.
    #[cfg(feature = "next-gen-gc")]
    if let Some(s) = sam {
        crate::vm::inline_census::note(if s.static_prov {
            crate::vm::inline_census::E_SAM_PROV
        } else {
            crate::vm::inline_census::E_SAM_PROFILE
        });
    }
    let ok = emit_inline(
        b,
        stack,
        env,
        array_desc_cell,
        callee,
        locals_ptr,
        cow,
        #[cfg(feature = "next-gen-gc")]
        licm,
        #[cfg(feature = "next-gen-gc")]
        dv,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        vars,
    );
    if ok.is_none() {
        return Guarded::Aborted;
    }
    // A void splice leaves nothing on the stack to carry, so the merge's argument list is the
    // live references alone — the same list the slow arm builds below.
    let mut jargs: Vec<cranelift::codegen::ir::BlockArg> = Vec::with_capacity(nres + ref_slots.len());
    if nres == 1 {
        let Some(res) = stack.pop() else { return Guarded::Aborted };
        jargs.push(to_cell(b, res).into());
    }
    for &i in &ref_slots {
        jargs.push(stack[i].v.into());
    }
    b.ins().jump(merge, &jargs);

    // --- the arm that does what the site did before ---
    b.switch_to_block(slow_blk);
    b.seal_block(slow_blk);
    *stack = snapshot.clone();
    if slow(b, stack).is_none() {
        return Guarded::Aborted;
    }
    let mut jargs2: Vec<cranelift::codegen::ir::BlockArg> =
        Vec::with_capacity(nres + ref_slots.len());
    if nres == 1 {
        let Some(res2) = stack.pop() else { return Guarded::Aborted };
        jargs2.push(to_cell(b, res2).into());
    }
    for &i in &ref_slots {
        jargs2.push(stack[i].v.into());
    }
    b.ins().jump(merge, &jargs2);

    // --- rejoin ---
    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    *stack = snapshot;
    stack.truncate(recv_idx); // receiver and args consumed
    for (k, &i) in ref_slots.iter().enumerate() {
        if i < stack.len() {
            stack[i].v = mparams[nres + k];
        }
    }
    // A void site pushes nothing — both arms consumed the receiver and the arguments and neither
    // produced a value, which is exactly the stack effect the call had.
    if let Some(t) = ret {
        let sv = cell_to_sv(b, mparams[0], t);
        stack.push(sv);
    }
    crate::diag::devirt_emitted().inc();
    Guarded::Emitted
}

/// Guard-arm executions, counted only under `RAVA_DEVIRT_STATS`. The emitted code does a
/// non-atomic load/add/store through this address, which can lose counts under threading and is
/// fine — the question is orders of magnitude, not exactness.
///
/// ⚠**It must be an ATOMIC, not a plain array.** The first version was `static [u64; 1]`, which
/// the linker places in READ-ONLY memory, so the generated store faulted:
/// `[CRASH] 0xc0000005 write of 0x7ff6871f00c0`. An immutable Rust static is not a writable
/// counter just because machine code holds its address.
pub(crate) static GUARD_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn devirt_stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DEVIRT_STATS").is_some())
}

/// Read the counter for a dump.
pub(crate) fn guard_hits() -> u64 {
    GUARD_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// ⭐**THE UNCOMMON TRAP: the guard's failure edge LEAVES the function.**
///
/// This is the shape the two failed experiments were missing. A diamond keeps the fallback
/// dispatch inside the method, so every speculation adds a merge, a block parameter per live
/// reference, and a second live range — and the measurements said that costs more than the calls
/// it removes (+0.81% and +2.42%). Here the failing edge spills the frame, hands it to the
/// interpreter, and RETURNS. What stays behind is straight-line code.
///
/// Emits nothing and answers `false` when the site cannot be described exactly: no type map, a
/// local whose type is not proven, or a return type the transition cannot carry. Refusing is
/// always available; guessing a type would silently corrupt a resumed frame.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_deopt_exit(
    b: &mut FunctionBuilder,
    env: &Env,
    vars: &[Variable],
    stack: &[SV],
    locals_ptr: Value,
    pc: usize,
    site_cp: u16,
) -> bool {
    // ⚠**A DEOPT EXIT MUST NEVER BE EMITTED FROM INSIDE A SPLICED BODY, AND NOW IT SAYS SO.**
    // `rava_deopt` rebuilds an interpreter `Frame` out of the words at `locals_ptr`, at the
    // offsets of the frame that wrote them — a SEMANTIC use of that buffer, not a root scan. A
    // spliced body's locals are the CALLEE's, and its `locals_ptr` is the CALLER's frame buffer
    // (the stack-map round removed the splice's own window, which was the only thing that had
    // ever made those two different), so describing a splice through this would resume the
    // interpreter in the wrong frame.
    //
    // This is a NO-OP today and is meant to stay one: `emit_inline`'s `b_env` sets
    // `local_types: None`, so the very next line already refuses every spliced site. It is
    // written here because the invariant belongs where it is relied on — a future round that
    // gives a spliced body a local-type map would otherwise re-open a frame-rebuild hazard by
    // fixing something unrelated.
    if env.in_splice {
        return false;
    }
    let Some(lt) = env.local_types else { return false };
    let Some(locals) = lt.at(pc) else { return false };
    let nlocals = vars.len();
    if !lt.all_known(pc, nlocals) {
        return false;
    }
    let Some(ret) = env.ret_jt else { return false }; // a void method has nothing to return
    if env.class_ptr == 0 || env.code_key == 0 {
        return false;
    }

    // The frame state: one tag per local, then one per operand-stack slot.
    let mut tags: Vec<u8> = Vec::with_capacity(nlocals + stack.len());
    for t in locals.iter().take(nlocals) {
        match t {
            crate::jit::analysis::localtypes::LT::Known(jt) => {
                tags.push(crate::jit::callbacks::deopt::tag_of(*jt))
            }
            // An `Unset` slot has never been written on any path reaching here, so nothing can
            // read it before it is stored; a reference tag is the conservative choice because it
            // decodes a zero cell to null rather than to a bogus primitive.
            _ => tags.push(crate::jit::callbacks::deopt::T_REF),
        }
    }
    for sv in stack {
        tags.push(crate::jit::callbacks::deopt::tag_of(sv.t));
    }

    // BISECTION (`RAVA_DEOPT_REFONLY=1`): emit only sites whose whole frame is references, so a
    // fault that lives in the PRIMITIVE half — the type map's proof, or the cell encoding — can
    // be separated from one that lives in the transition itself.
    {
        static REFONLY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *REFONLY.get_or_init(|| std::env::var_os("RAVA_DEOPT_REFONLY").is_some())
            && tags.iter().any(|&t| t != crate::jit::callbacks::deopt::T_REF)
        {
            return false;
        }
    }

    let site = Box::leak(Box::new(crate::jit::callbacks::deopt::DeoptSite {
        pc: pc as u32,
        caller: env.class_ptr as usize,
        ret_ref: matches!(ret, JT::Ref),
        cp: site_cp,
        nlocals: nlocals as u16,
        nstack: stack.len() as u16,
        tags,
    }));
    crate::jit::callbacks::deopt::DEOPT_SITES
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Materialise the frame. `spill_for_gc` writes every local and only the REFERENCE stack
    // slots; a deopt needs the primitives too, so the stack is written in full here.
    //
    // PHASE B (`RAVA_STACKMAPS=1`): `spill_for_gc` stops emitting those stores — it only
    // declares. But this buffer is not a root set here, it is the interpreter's `Frame`
    // (`rava_deopt` rebuilds locals + operand stack from these exact words), so the store half
    // is re-emitted unconditionally through `spill_frame_words`. Rooting is what the stack maps
    // replace; the deopt transition is a different use of the same memory and keeps its writes.
    spill_for_gc(b, vars, stack, locals_ptr);
    #[cfg(feature = "next-gen-gc")]
    if crate::jit::runtime::stackmaps::declaring() {
        spill_frame_words(b, vars, stack, locals_ptr);
    }
    for (i, sv) in stack.iter().enumerate() {
        if sv.t != JT::Ref {
            let cell = to_cell(b, *sv);
            let off = Offset32::new(((nlocals + i) * 8) as i32);
            b.ins().store(MemFlags::trusted(), cell, locals_ptr, off);
        }
    }

    let clsv = b.ins().iconst(I64, env.class_ptr);
    let codev = b.ins().iconst(I64, env.code_key as i64);
    let sitev = b.ins().iconst(I64, (site as *const _) as i64);
    let call = b.ins().call(env.deopt, &[clsv, codev, sitev, locals_ptr]);
    let packed = b.inst_results(call)[0];
    let sv = cell_to_sv(b, packed, ret);
    b.ins().return_(&[sv.v]);
    true
}
