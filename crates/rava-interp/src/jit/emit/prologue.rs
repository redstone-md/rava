//! The Deep-IC v3 self-registering prologue.
//!
//! A non-leaf body registers its own inline-cache context on entry and restores the caller's
//! on every return path. It must precede every GC-capable prologue, because those run
//! callbacks that can re-enter.

use crate::jit::*;

/// FAST PROLOGUE (`RAVA_NO_FAST_PROLOGUE=1` restores the previous emission byte-for-byte).
///
/// The entry sequence below runs on EVERY compiled call, so everything it does that a given
/// body does not need is paid millions of times a boot. This gate covers the whole set of
/// entry-shape changes — the fused `JitCtx` gate word, the sunk serial-ctx load, the
/// register-ABI argument-hole fill, the once-per-entry TLAB cursor for late-bound `new`
/// bodies, and the dropped no-op spill in the static-LICM preheader — so a single
/// environment variable gives them one bisect handle and one A/B arm.
///
/// Read ONCE per process (a `OnceLock`, not a per-emit `var_os`) and consulted only at EMIT
/// time, where it decides codegen; nothing reads it from compiled code.
pub(crate) fn fast_prologue() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_PROLOGUE").is_none() && !crate::flags::emit_more_disabled()
    })
}

#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_deep_prologue(
    b: &mut FunctionBuilder,
    env: &Env,
    _max_locals: usize,
    locals_ptr: Value,
    array_desc_cell: Value,
    cow_armed: Option<Value>,
    // The two 32-bit `JitCtx` gate words (`fault` @8, `cow_armed` @12) already materialised by
    // `build_body` from ONE aligned 8-byte load, or `None` to keep the historic pair of
    // separate 32-bit loads. See `build_body`'s `ctx_words`.
    ctx_words: Option<(Value, Value)>,
) -> Option<DeepVars> {
    // Deep-IC v3 SELF-REGISTERING PROLOGUE (env.deep): make this non-leaf body
    // safe to enter DIRECTLY from the machine-code dispatch IC with no
    // caller-side registration. In order: (1) refuse to run on a latched fault
    // (mirrors dispatch_miss's pending-exception refusal — the caller continues
    // to its own return with 0 bits, exactly as a refused deep hit did);
    // (2) zero the non-argument slots of the frame buffer (a GC scans the whole
    // registered length — stale caller bits must not read as roots);
    // (3) push (buffer, frame_len) onto this thread's flat cell-root stack
    // INLINE via the baked per-Vm ctx slot (cold callback only on overflow);
    // (4) snapshot the box-table length for the epilogue's box discipline.
    // MUST precede every GC-capable prologue below (static-LICM callbacks).
    //
    // ⛔THE ZERO LOOP IS REDUNDANT ON THE `jit_enter` PATH AND MUST STAY ANYWAY. When a
    // self-registering body is entered through the wrapper, `Vm::jit_enter` has ALREADY
    // written zeros over `[cells_packed, frame_len)` via `cells.resize(.., 0)` — the same
    // bytes this loop writes a few instructions later. The obvious removal is to have the
    // wrapper `set_len` instead of `resize` and let the callee do the zeroing, and it is
    // WRONG twice: the two ranges are not the same (the prologue starts at `args_slots`,
    // the wrapper at however many locals it was handed, so a short `locals` leaves
    // `[locals.len(), args_slots)` covered by NEITHER), and a `set_len` over uninitialised
    // memory hands a `&mut [i64]` of undef to `call_raw`. The other direction — dropping
    // this loop — is worse: the dispatch IC calls a deep body DIRECTLY on a REUSED call
    // buffer, where the stale bits above the args are a live caller's handles and a
    // collector scanning the registered length would resurrect or forward them. The
    // duplicate costs `frame_len - args_slots` stores on the wrapper path only; the correct
    // fix is a caller-side one (a wrapper that knows the callee self-registers could size
    // the packed prefix to `args_slots`), not a change here.
    //
    // ⛔AND THE ZERO LOOP'S EXTENT CANNOT BE NARROWED FROM HERE EITHER, though it is the
    // biggest single item in this sequence. Two narrowings look available and neither is
    // provable in this file: (a) skipping `[args_slots, max_locals)` because `spill_for_gc`
    // rewrites every local at each GC point — that needs "no GC point is reachable before
    // the first spill", and `emit_licm_base` / the cursor fetch sit between the push and
    // that spill; (b) registering a length shorter than `frame_len` — that needs the
    // method's true operand-stack high-water mark at a GC point, which only the emission
    // walk knows, and `frame_len` is decided by the caller (`vm/selfreg.rs`, `vm/jit_run.rs`)
    // and is also the dispatch call-buffer size contract. Both were priced and left.
    #[cfg(feature = "next-gen-gc")]
    if let Some(ds) = env.deep {
        let fast = fast_prologue();
        // (1) FAULT GATE. `ctx_words` (when the caller built it) is the low half of ONE
        // aligned 8-byte read of `JitCtx { desc_base: usize @0, fault: u32 @8,
        // cow_armed: u32 @12 }` — repr(C), so `fault` and `cow_armed` are the two halves of
        // the same 8-aligned word and the same unsynchronised relaxed read they always were,
        // now costing one memory operation on entry instead of two.
        let ff = match ctx_words {
            Some((f, _)) => f,
            None => b.ins().load(I32, MemFlags::trusted(), array_desc_cell, Offset32::new(8)),
        };
        let bail = b.create_block();
        let cont = b.create_block();
        b.ins().brif(ff, bail, &[], cont, &[]);
        b.switch_to_block(bail);
        b.seal_block(bail);
        let z0 = abi_zero(b);
        b.ins().return_(&[z0]);
        b.switch_to_block(cont);
        b.seal_block(cont);
        let zero = b.ins().iconst(I64, 0);
        for s in (ds.args_slots as usize)..(ds.frame_len as usize) {
            b.ins().store(MemFlags::trusted(), zero, locals_ptr, Offset32::new((s * 8) as i32));
        }
        // ⛔**THIS IS WHERE THE PINNED CONTEXT REGISTER WOULD HAVE BEEN READ, AND WHY IT IS NOT.**
        // The whole diamond below exists to answer "which `JitThreadCtx`", and the answer is a
        // per-thread CONSTANT — precisely the shape a pinned register serves. `RAVA_R1_PINNED_CONTEXT`
        // already reserves R15 for it. It stays unread, and the block that would have written it
        // stays unwritten, for an ABI reason recorded in full in `jit::runtime::module`:
        // `cranelift-codegen`'s `is_callee_save_fastcall` returns FALSE for R15 as soon as
        // `enable_pinned_reg` is on, so a `set_pinned_reg` here emits no save and no restore, and
        // clobbers a Windows-x64 callee-saved register across every Rust frame beneath us. The
        // write has to happen in an asm shim at each of the eight Rust→compiled edges (none of
        // which is this file), or be undone at ~40 `return_` sites across 14 files where missing
        // one is silent corruption rather than a fault. Neither is a prologue change, so the
        // prologue was left byte-identical.
        //
        // What DID move is the cold arm's callee: `env.ctx_addr_fn` → `rava_jit_ctx_addr` →
        // `frames::ctx_addr`, which now answers from a per-thread memo word instead of a `RefCell`
        // borrow and an `Option` branch. The hot arm below is unchanged: the baked per-Vm slot is
        // one load from a constant address and there is nothing above it to remove without the
        // register.
        //
        // (3a) WHICH `JitThreadCtx`. The baked per-Vm slot names the executing thread's ctx on
        // the normal GIL-held path; a DETACHED (phase-B COW) worker must take the TLS lookup
        // instead. The detached arm is cold — only a fork worker ever enters it — so the
        // serial slot's address+load now sits INSIDE the serial arm rather than ahead of the
        // test: the load is on the hot path either way, but hoisted it also ran (and occupied
        // a register across the branch) for the worker that discards it.
        let hoisted_serial = if fast {
            None
        } else {
            let slot = b.ins().iconst(I64, ds.ctx_slot as i64);
            Some(b.ins().load(I64, MemFlags::trusted(), slot, Offset32::new(0)))
        };
        // Same invariant flag the field sites branch on — reuse the hoisted test when
        // this body has one, else the fused gate word, else the prologue's own load.
        let armed = match cow_armed {
            Some(v) => v,
            None => match ctx_words {
                Some((_, a)) => a,
                None => b.ins().load(I32, MemFlags::trusted(), array_desc_cell, Offset32::new(12)),
            },
        };
        let detached = b.create_block();
        let serial = b.create_block();
        let ctx_join = b.create_block();
        b.append_block_param(ctx_join, I64);
        b.ins().brif(armed, detached, &[], serial, &[]);
        b.switch_to_block(detached);
        b.seal_block(detached);
        let tls_call = b.ins().call(env.ctx_addr_fn, &[]);
        let tls_ctx = b.inst_results(tls_call)[0];
        b.ins().jump(ctx_join, &[tls_ctx.into()]);
        b.switch_to_block(serial);
        b.seal_block(serial);
        let serial_ctx = match hoisted_serial {
            Some(v) => v,
            None => {
                let slot = b.ins().iconst(I64, ds.ctx_slot as i64);
                b.ins().load(I64, MemFlags::trusted(), slot, Offset32::new(0))
            }
        };
        b.ins().jump(ctx_join, &[serial_ctx.into()]);
        b.switch_to_block(ctx_join);
        b.seal_block(ctx_join);
        let ctx = b.block_params(ctx_join)[0];
        let top = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(0));
        let cap = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(8));
        let flen = b.ins().iconst(I64, ds.frame_len as i64);
        let full = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, top, cap);
        let slow = b.create_block();
        let fast_blk = b.create_block();
        let joined = b.create_block();
        b.ins().brif(full, slow, &[], fast_blk, &[]);
        b.switch_to_block(fast_blk);
        b.seal_block(fast_blk);
        let bufp = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(16));
        let off = b.ins().ishl_imm(top, 4);
        let a = b.ins().iadd(bufp, off);
        b.ins().store(MemFlags::trusted(), locals_ptr, a, Offset32::new(0));
        b.ins().store(MemFlags::trusted(), flen, a, Offset32::new(8));
        // PHASE-A STACK MAPS (`RAVA_STACKMAPS=1`, else NOTHING is emitted here). Record this
        // compiled frame's SP into the ctx's parallel mirror at the SAME entry index the
        // cell-root range just took, so the collector's cross-check can pair a published
        // range with the frame that published it. Cranelift x64 reserves the whole frame in
        // the machine prologue and never moves SP inside the body, so the value taken here
        // is the SP at every safepoint in this function — the base a `UserStackMap`'s
        // offsets are relative to, and the base `*(SP-8)` finds the active call's return
        // address at. `sp_buf` is non-null exactly when this emission ran (both read the
        // same process-constant gate), so the store can never hit a null base.
        #[cfg(feature = "next-gen-gc")]
        if crate::jit::runtime::stackmaps::recording_sp() {
            let spb = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(32));
            let spoff = b.ins().ishl_imm(top, 3);
            let spa = b.ins().iadd(spb, spoff);
            let spv = b.ins().get_stack_pointer(I64);
            b.ins().store(MemFlags::trusted(), spv, spa, Offset32::new(0));
        }
        let t1 = b.ins().iadd_imm(top, 1);
        b.ins().store(MemFlags::trusted(), t1, ctx, Offset32::new(0));
        b.ins().jump(joined, &[]);
        b.switch_to_block(slow);
        b.seal_block(slow);
        b.ins().call(env.cells_slow, &[locals_ptr, flen]);
        b.ins().jump(joined, &[]);
        b.switch_to_block(joined);
        b.seal_block(joined);
        let bb = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(24));
        let ctx_v = b.declare_var(I64);
        b.def_var(ctx_v, ctx);
        let top_v = b.declare_var(I64);
        b.def_var(top_v, top);
        let base_v = b.declare_var(I64);
        b.def_var(base_v, bb);
        Some(DeepVars { ctx: ctx_v, top: top_v, base: base_v })
    } else {
        None
    }
}
