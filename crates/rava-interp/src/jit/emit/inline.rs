//! Inlined callee bodies: the general case, arena allocation, and the trivial constructor.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Kill switch for the emit-side inliner work of this round (`RAVA_NO_INLINE_EMIT2=1`),
/// DEFAULT ON. Consulted at EMIT time — it changes what IR is laid down, nothing at runtime —
/// and it also folds in the block-level `RAVA_NO_EMIT_MORE` handle so a single same-binary A/B
/// can price the whole round.
///
/// Three behaviours hang off it, all of them removals:
///   * the redundant zero-init of the callee local slots the argument binding overwrites,
///   * the `pinned_ref_slots` body scan when nothing is left for it to decide,
///   * (in `guarded_inline`) the receiver-kind test and the field-site `is_arena` diamonds that
///     the devirtualisation guard has ALREADY proven.
pub(crate) fn inline_emit2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_INLINE_EMIT2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Kill switch for THIS round's lambda-loop work (`RAVA_NO_INLINE_LAMBDA_LOOP=1`), DEFAULT ON.
/// Consulted at EMIT time only — it decides what IR is laid down, never what runs — and it folds
/// in the block-level `RAVA_NO_EMIT_MORE` handle so one same-binary A/B prices the whole round.
///
/// One behaviour hangs off it, a removal:
///   * the STRAIGHT-LINE SPLICE (`emit_inline` below): a callee that is ONE basic block ending in
///     ONE return is laid into the caller's current block, with no callee entry block, no
///     continuation block and no continuation phi.
///
/// ⚠**IT NO LONGER GATES THE LAMBDA-CLASS REFUSAL IN `guarded_inline`.** That refusal — a class-id
/// guard whose class is a capture SIGNATURE cannot pin an implementation — used to sit behind this
/// switch, which meant throwing an unrelated performance switch also disarmed a correctness guard.
/// It was harmless only while nothing could admit a lambda callee, and the lambda SAM inline chain
/// can. The refusal is now unconditional, keyed on the presence of a `SamInline` plan instead.
pub(crate) fn lambda_loop_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_INLINE_LAMBDA_LOOP").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

/// ⭐⭐**THE ROOTED SPLICE: ONE MECHANISM FOR `shape.new` AND `iface.calls != 1`.**
///
/// Kill switch `RAVA_NO_INLINE_GCPOINT=1` (also `crate::flags::emit_more_disabled()`), read once.
/// It gates ADMISSION (`vm/inline_targets.rs`), so with it set no callee ever carries
/// `splice_roots` and every line below is unreachable — the historic emission, byte for byte.
///
/// **What the two historic refusals were protecting.** `emit_inline` splices a callee by
/// declaring its locals as fresh Cranelift `Variable`s and running its body through the CALLER's
/// `locals_ptr`. A `spill_for_gc` inside that body therefore writes the CALLEE's variables from
/// offset 0 of the CALLER's frame buffer: the callee is rooted, the caller is clobbered, and the
/// caller's own live references — still in registers, now pointing at pre-move addresses — are
/// left stale. `partial_inline_gc_safe` records the crash that taught this (an enum `values()`
/// whose array-clone survived: a ref arriving as `Int(0)`, surfacing as
/// `UnsatisfiedLinkError: Integer.isEmpty()`).
///
/// **The cure, and why it is one thing and not two.** An allocation is a GC point; a surviving
/// call is a GC point. Both are refused for the same missing capability, so both are answered by
/// giving the splice the rooting an ordinary compiled call already performs:
///
///   1. a FRAME WINDOW of its own — `Env::splice_window`, one shared stack slot per function —
///      zeroed before it is registered, because the collector scans a registered range BY RANGE
///      and stale bits with low bits `00` inside the nursery decode as raw arena pointers;
///   2. that window PUSHED onto this thread's flat cell-root stack for the duration of the body,
///      exactly as `emit_deep_prologue` pushes a real frame, and popped on the way out exactly as
///      `emit_deep_epilogue` pops one (restore the saved `top`; a slow-path push through
///      `cells_slow` lands at the same `top + 1`, so one store restores both);
///   3. the CALLER's frame spilled into `locals_ptr` before the body and reloaded after it, which
///      is what makes the caller's registers correct again across a move.
///
/// The body then emits with `locals_ptr` = the window, so every `spill_for_gc` /
/// `reload_after_gc` the callee's own opcodes emit — the allocation overflow arm, the `rava_new`
/// slow arm, a surviving `emit_direct_mono` / `emit_dispatch_pic` / `emit_invoke` — roots the
/// CALLEE's frame in a region that is disjoint from the caller's by construction. No emitter
/// below `emit_inline` changed; they all already take `locals_ptr` as "this frame's root buffer".
///
/// **What is NOT relaxed.** The window roots the callee's frame and `locals_ptr` roots the
/// caller's — and `locals_ptr` is the compiled method's own frame only when the splice is
/// OUTERMOST. A splice nested inside another splice would need its parent's Cranelift variables
/// rooted too, and those live in no buffer at all. Admission therefore offers this at depth 0
/// only, and requires every nested spliced callee to be `is_gc_leaf`.
fn splice_roots_enabled() -> bool {
    crate::vm::inline_targets::inline_gcpoint_enabled()
}

/// The live handle a rooted splice carries from its entry to its exit: `(ctx, saved_top)`. Both
/// are defined in the block that dominates the whole spliced body, so they are usable at whichever
/// block the body ends in.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
struct SpliceWindow {
    ctx: Value,
    saved_top: Value,
}

/// ENTER a rooted splice: zero the window, register it, root the caller. Returns the window
/// pointer the body must use as its `locals_ptr`, and the handle its exit needs.
///
/// Ordering is load-bearing and each step states why:
///   * the window is zeroed BEFORE the push, so the collector never sees a range holding the
///     previous splice's dead pointers (the rule `emit_deep_prologue`'s zero loop exists for);
///   * the caller is spilled AFTER the arguments have been popped, so the values it roots are
///     exactly the ones still live when the body ends;
///   * the push happens before either, because nothing between it and the body can collect and
///     an already-registered zeroed range is trivially sound.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
fn enter_splice_roots(
    b: &mut FunctionBuilder,
    env: &Env,
    slots: usize,
    caller_vars: &[Variable],
    a_stack: &[SV],
    locals_ptr: Value,
    dv: Option<DeepVars>,
) -> (Value, SpliceWindow) {
    let ss = env.splice_window(b);
    let win = b.ins().stack_addr(I64, ss, 0);
    let zero = b.ins().iconst(I64, 0);
    for i in 0..slots {
        b.ins().store(MemFlags::trusted(), zero, win, Offset32::new((i * 8) as i32));
    }
    // WHICH `JitThreadCtx`. A self-registering body already resolved it in its prologue (and a
    // detached COW worker resolved the TLS one there) — reuse that. A body without one asks the
    // per-thread memo word through `rava_jit_ctx_addr`, which allocates nothing and cannot
    // collect. Either way this is the same ctx `cells_all` will read the range back out of.
    let ctx = match dv {
        Some(d) => b.use_var(d.ctx),
        None => {
            let c = b.ins().call(env.ctx_addr_fn, &[]);
            b.inst_results(c)[0]
        }
    };
    let saved_top = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(0));
    let cap = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(8));
    let lenv = b.ins().iconst(I64, slots as i64);
    let full = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, saved_top, cap);
    let slow = b.create_block();
    let fast = b.create_block();
    let joined = b.create_block();
    b.ins().brif(full, slow, &[], fast, &[]);
    b.switch_to_block(fast);
    b.seal_block(fast);
    let bufp = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(16));
    let off = b.ins().ishl_imm(saved_top, 4);
    let a = b.ins().iadd(bufp, off);
    b.ins().store(MemFlags::trusted(), win, a, Offset32::new(0));
    b.ins().store(MemFlags::trusted(), lenv, a, Offset32::new(8));
    let t1 = b.ins().iadd_imm(saved_top, 1);
    b.ins().store(MemFlags::trusted(), t1, ctx, Offset32::new(0));
    b.ins().jump(joined, &[]);
    b.switch_to_block(slow);
    b.seal_block(slow);
    b.ins().call(env.cells_slow, &[win, lenv]);
    b.ins().jump(joined, &[]);
    b.switch_to_block(joined);
    b.seal_block(joined);
    // ROOT THE CALLER. Its locals live only in Cranelift `Variable`s and its ref-typed operand
    // stack only in SSA values; a collection inside the body rewrites the rooted CELLS in place,
    // so this store is what gives the reload on the way out something forwarded to read.
    spill_for_gc(b, caller_vars, a_stack, locals_ptr);
    (win, SpliceWindow { ctx, saved_top })
}

/// LEAVE a rooted splice: unregister the window and refresh the caller.
///
/// Popping by restoring the SAVED top is the same one-store discipline `emit_deep_epilogue` uses,
/// and it is correct whichever push arm ran: `rava_cells_push_slow` grows the stack and pushes,
/// leaving `top` at `saved_top + 1` exactly as the inline arm does. Every nested entry pushed by
/// a call inside the body has been popped by its own epilogue before control gets here.
///
/// An exception / deopt exit from inside the body never reaches this, and does not need to: those
/// leave through the function's own `return_`, whose `emit_deep_epilogue` restores the entry
/// `top` — which is below `saved_top` — and the caller's registers die with the frame.
#[cfg(feature = "next-gen-gc")]
fn leave_splice_roots(
    b: &mut FunctionBuilder,
    w: SpliceWindow,
    caller_vars: &[Variable],
    a_stack: &mut [SV],
    locals_ptr: Value,
) {
    b.ins().store(MemFlags::trusted(), w.saved_top, w.ctx, Offset32::new(0));
    reload_after_gc(b, caller_vars, a_stack, locals_ptr);
}

/// Is this callee body ONE basic block that ends in ONE return of the shape the descriptor calls
/// for (a value return, or `return` when `void`)?
///
/// ⭐**THE PRECONDITION IS "NOTHING SPLITS A BLOCK", NOT "`find_leaders` SAID ONE".** A body
/// whose last instruction is `goto 0` also has exactly one leader — 0 is already a leader and a
/// trailing branch has no fall-through pc to add — so a leader count would admit an infinite loop
/// that never reaches a return, and the splice would fall off the end having pushed nothing.
/// This asks the direct question instead: every opcode that begins, ends or leaves a basic block
/// is refused outright, and the ONE return is required to be the final instruction.
///
/// Decoded with `insn_len`, the SAME decoder `emit_inline` walks the spliced body with — not
/// `insn_len_at`, which admission uses. A body this decoder cannot read (it has no arm for the
/// variable-length switches) refuses here exactly as it would refuse there, so acceptance is
/// decided by one function and the fast path can never see a body the slow path would reject.
///
/// `void` says which terminal the DESCRIPTOR calls for: a void body must end in `return` (0xb1)
/// and a value body must not. Requiring agreement rather than accepting either is what keeps the
/// caller's operand stack right — the void arm pushes nothing, the value arm pushes exactly one,
/// and a body whose terminal disagrees with its descriptor would pick the wrong one.
fn straight_line_return(bc: &[u8], void: bool) -> bool {
    let mut pc = 0usize;
    let mut ends_in_return = false;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len(op) else { return false };
        match op {
            // Conditional branches, goto, jsr/ret, both switches, athrow, the wide branches:
            // each either creates a second block or leaves this one without reaching the return.
            0x99..=0xab | 0xbf | 0xc6..=0xc9 => return false,
            // A return. Legal only as the LAST instruction of the body, and only the one the
            // descriptor calls for.
            0xac..=0xb1 => {
                if (op == 0xb1) != void || pc + len != bc.len() {
                    return false;
                }
                ends_in_return = true;
            }
            _ => {}
        }
        pc += len;
    }
    ends_in_return
}

/// INLINE TLAB BUMP (next-gen-gc, Phase Raw Speed). Allocate an arena object of a
/// compile-time-resolved class with ZERO callback: load the thread-local cursor's
/// `[next, end]`, bump `next` by the constant object size, and on success
/// initialise the 24-byte header + zero the cells inline and push the raw pointer.
/// On nursery overflow, spill live roots, call the native Cheney safepoint
/// (`rava_arena_safepoint` → `arena_collect_jit` forwards the spilled roots in
/// place + republishes the cursor), reload, and loop back to re-bump into the freed
/// nursery — a machine back-edge, no trampoline.
///
/// SSA correctness across the overflow diamond: locals reload via `def_var`
/// (Cranelift inserts the merge phi at the back-edge join); ref-typed OPERAND-STACK
/// values are threaded explicitly as block params (retry → cont), since a raw SSA
/// `Value` reloaded only on the overflow arm would not dominate `cont`. Primitive
/// stack values are untouched by GC, so they need no threading.
///
/// `class_id`/`nfields` are compile-time constants (the class is arena-eligible AND
/// already initialised — resolved in `Vm::build_new_arena`). The pushed result is a
/// raw 8-aligned `ArenaPtr` (< 2^40), the exact cell `box_ref(Value::Obj)` produces,
/// so downstream field ops / GC scan / `unbox_ref` decode it with no extra work.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_inline_arena_new(
    b: &mut FunctionBuilder,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    cursor_var: Variable,
    class_id: u32,
    nfields: u32,
    safepoint_fn: cranelift::codegen::ir::FuncRef,
    deopt_alloc_fn: cranelift::codegen::ir::FuncRef,
    locals_ptr: Value,
) {
    let size = crate::nextgen::HDR as i64 + nfields as i64 * 8;
    let hdrword = ((nfields as i64) << 32) | class_id as i64;

    // Ref-typed operand-stack slots: threaded through the retry/cont merge so a
    // GC that moves them on the overflow arm is reflected on every path out.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();
    let nref = ref_slots.len();

    let retry = b.create_block();
    for _ in 0..nref {
        b.append_block_param(retry, I64);
    }
    let ok = b.create_block();
    let overflow = b.create_block();
    // BOUNDED EXIT. Sole successor of `overflow` when the safepoint reports it freed
    // nothing. Carries the same ref-stack values `retry` does, so it can hand `cont`
    // a complete parameter list. Takes NO back edge — that is the whole point.
    let bail = b.create_block();
    for _ in 0..nref {
        b.append_block_param(bail, I64);
    }
    let cont = b.create_block();
    b.append_block_param(cont, I64); // result pointer
    for _ in 0..nref {
        b.append_block_param(cont, I64);
    }

    // Enter the retry loop, seeding it with the current ref-stack values.
    let init: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();
    b.ins().jump(retry, &init);

    // ── retry: re-read the cursor each iteration (post-GC it points at base). NOT
    //    sealed yet — the overflow back-edge is a second predecessor.
    b.switch_to_block(retry);
    let rparams: Vec<Value> = b.block_params(retry).to_vec();
    let cur = b.use_var(cursor_var);
    let next = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(0));
    let end = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(8));
    let nn = b.ins().iadd_imm(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, nn, end);
    // retry dominates both ok and overflow (their sole predecessor), so values
    // defined here flow into them directly — no need to pass as block args.
    b.ins().brif(fits, ok, &[], overflow, &[]);

    // ── ok: publish the new top, lay down the header + zero the cells, hand the
    //    raw pointer (and the unchanged ref-stack values) to cont.
    b.switch_to_block(ok);
    b.seal_block(ok);
    b.ins().store(MemFlags::trusted(), nn, cur, Offset32::new(0));
    let hw = b.ins().iconst(I64, hdrword);
    b.ins().store(MemFlags::trusted(), hw, next, Offset32::new(0)); // [0]=class|nfields
    let zero = b.ins().iconst(I64, 0);
    // Zero the header's mutable tail, bytes [8, HDR) — mirrors `obj_init_hdr_tail`.
    // 24B layout: fwd word + hash word. 16B (`shrink-header`): one word holding the
    // narrow fwd offset + the hash.
    b.ins().store(MemFlags::trusted(), zero, next, Offset32::new(8)); // [8]=fwd(+hash)
    if crate::nextgen::HDR > 16 {
        b.ins().store(MemFlags::trusted(), zero, next, Offset32::new(16)); // [16]=hash
    }
    for i in 0..nfields as i32 {
        let off = crate::nextgen::HDR as i32 + i * 8;
        b.ins().store(MemFlags::trusted(), zero, next, Offset32::new(off));
    }
    let mut ok_args: Vec<cranelift::codegen::ir::BlockArg> = vec![next.into()];
    ok_args.extend(
        rparams
            .iter()
            .map(|&v| -> cranelift::codegen::ir::BlockArg { v.into() }),
    );
    b.ins().jump(cont, &ok_args);

    // ── overflow: spill live roots (reflecting this iteration's ref-stack values),
    //    run the native safepoint, reload, loop back with forwarded ref values.
    b.switch_to_block(overflow);
    b.seal_block(overflow);
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = rparams[k];
    }
    spill_for_gc(b, vars, stack, locals_ptr);
    let sp = b.ins().call(safepoint_fn, &[]);
    // NON-ZERO = "I could not free you any space". Without this edge the jump below is
    // UNCONDITIONAL, which is exactly the measured wall: a gil-free worker whose view is
    // full and whose region spill pool is exhausted re-enters the safepoint forever
    // (1.44 billion callbacks, 97% of them this one, on one thread). On the main thread
    // the safepoint always collects, so the status is a constant 0 and this branch is
    // never taken — the fast path keeps its shape.
    let starved = b.inst_results(sp)[0];
    // Locals reload diamond-safely via def_var; ref-stack values come back as the
    // retry back-edge args (their forwarded pointers from the spill buffer).
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut back: Vec<cranelift::codegen::ir::BlockArg> = Vec::with_capacity(nref);
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        back.push(rv.into());
    }
    b.ins().brif(starved, bail, &back, retry, &back);

    // Both predecessors of retry (entry jump + overflow back-edge) now emitted.
    b.seal_block(retry);

    // ── bail: the region is already aborted (the deopt allocator raises it), so this
    //    object's bytes are throwaway — every result downstream of here is discarded and
    //    the task re-runs serially. Lay the SAME header down as `ok` so the compiled
    //    body's field stores/loads stay well-typed on the way out, and let it return
    //    normally. No back edge exists from here, so the loop is bounded by construction.
    b.switch_to_block(bail);
    b.seal_block(bail);
    let bparams: Vec<Value> = b.block_params(bail).to_vec();
    let szc = b.ins().iconst(I64, size);
    let dcall = b.ins().call(deopt_alloc_fn, &[szc]);
    let scratch = b.inst_results(dcall)[0];
    let hw2 = b.ins().iconst(I64, hdrword);
    b.ins().store(MemFlags::trusted(), hw2, scratch, Offset32::new(0));
    let z2 = b.ins().iconst(I64, 0);
    b.ins().store(MemFlags::trusted(), z2, scratch, Offset32::new(8));
    if crate::nextgen::HDR > 16 {
        b.ins().store(MemFlags::trusted(), z2, scratch, Offset32::new(16));
    }
    for i in 0..nfields as i32 {
        let off = crate::nextgen::HDR as i32 + i * 8;
        b.ins().store(MemFlags::trusted(), z2, scratch, Offset32::new(off));
    }
    let mut bail_args: Vec<cranelift::codegen::ir::BlockArg> = vec![scratch.into()];
    bail_args.extend(
        bparams
            .iter()
            .map(|&v| -> cranelift::codegen::ir::BlockArg { v.into() }),
    );
    b.ins().jump(cont, &bail_args);

    // ── cont: the merge. Adopt the forwarded ref-stack values + push the result.
    b.switch_to_block(cont);
    b.seal_block(cont);
    let cparams: Vec<Value> = b.block_params(cont).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = cparams[1 + k];
    }
    push(stack, cparams[0], JT::Ref);
}

/// Emit a method call: marshal args into the stack-slot buffer, call rava_invoke,
/// and push the typed result (nothing for void). `kind`: 0=static (others later).
/// Inline a small pure static callee (see `is_inlinable`) into the caller's body:
/// pop its args off the caller stack into fresh local variables, translate its
/// straight-line body with the SAME emit_op (using the callee's pool), and push
/// its returned value back onto the caller stack. No runtime call → no dispatch
/// or GC-across-call overhead; everything stays in the caller's frame.
pub(crate) fn emit_inline(
    b: &mut FunctionBuilder,
    a_stack: &mut Vec<SV>,
    env: &Env,
    array_desc_cell: Value,
    callee: &InlineCallee,
    locals_ptr: Value,
    // The CALLER's M3 COW arm knowledge — an inlined callee runs inside that same
    // invocation, so its field sites inherit it verbatim (including a proven
    // DISARMED body, where the callee's field sites also emit no COW code).
    cow: CowArm,
    #[cfg(feature = "next-gen-gc")] licm: Option<Licm>,
    #[cfg(feature = "next-gen-gc")] dv: Option<DeepVars>,
    #[cfg(feature = "next-gen-gc")] active_sam: Option<ActiveSamBinding<'_>>,
    // ⭐**THE CALLER'S OWN LOCALS, WHICH THIS FUNCTION HAS NEVER SEEN BEFORE.** A rooted splice
    // has to spill them into `locals_ptr` before the body runs and re-read them after, and until
    // this round `emit_inline` had no handle on them at all — which is not a detail of the bug,
    // it IS the bug: `spill_for_gc` inside a spliced body was reachable only with the CALLEE's
    // `Variable`s, so rooting the callee necessarily un-rooted the caller. Ignored (and the
    // historic emission kept byte for byte) unless `callee.splice_roots` is `Some`.
    #[cfg(feature = "next-gen-gc")] caller_vars: &[Variable],
) -> Option<()> {
    // `None` = VOID, and it is no longer a refusal: `is_inlinable_kind(.., allow_void = true)`
    // admits a void SAM body and the two return shapes below hand back nothing for it, leaving
    // the caller's operand stack exactly as the call would have left it. Every other callee still
    // arrives non-void, because `is_inlinable` still says so.
    let ret = ret_jt(&callee.descriptor);
    // ADDED, compile-time only: the population the widened interface-body arm actually serves.
    // Counted HERE and not at admission, because admission is asked once per compile per site
    // while a splice is what reaches machine code — an admitted callee whose caller then fails to
    // compile removes no call at all. One relaxed add per spliced site, never per call.
    #[cfg(feature = "next-gen-gc")]
    {
        let bc = &callee.bytecode;
        let mut pc = 0usize;
        let mut surviving_iface = false;
        while pc + 2 < bc.len() {
            let Some(len) = insn_len_at(bc, pc) else { break };
            if bc[pc] == 0xb9 {
                let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if !callee.nested.contains_key(&cp) {
                    surviving_iface = true;
                    break;
                }
            }
            pc += len;
        }
        if surviving_iface {
            crate::vm::inline_census::note(crate::vm::inline_census::E_IFACE_EMIT);
            // ADDED: and how many of those splices carried a BRANCH into machine code. Same
            // rule as the row above — counted at emit, because an admitted callee whose caller
            // then fails to compile removes no dispatch at all — and it is the row that says
            // whether the widened admission survived every later gate.
            if crate::vm::inline_targets::cf_shape(bc).0 {
                crate::vm::inline_census::note(crate::vm::inline_census::E_IF_CF);
            }
        }
    }
    // ⭐**A SAM SPLICE POPS THE SAM'S ARGUMENTS, NOT THE IMPLEMENTATION'S PARAMETERS.** The
    // implementation's descriptor lists `captures ++ samArgs`; only the tail is on the stack. See
    // `SamInline`.
    #[cfg(feature = "next-gen-gc")]
    let sam = callee.sam.as_ref();
    let nargs = crate::count_args(&callee.descriptor);
    // Instance callees take an implicit receiver at local slot 0; descriptor args start at slot 1.
    // 16.2: a wide param (long/double) occupies TWO local slots — arg i lands at the cumulative
    // slot `recv + Σ(widths[0..i])`, and its pair-slot (arg_slot[i]+1) stays dead (never loaded).
    // ⚠**THE SPLICED BODY'S staticness, not the SITE's.** A SAM plan sets `InlineCallee::is_static`
    // to false because the site pushes a closure; whether the BODY has a `this` in slot 0 is a
    // different fact and lives in the plan. Getting this backwards would shift every local slot by
    // one. See `SamInline::impl_is_static`.
    #[cfg(feature = "next-gen-gc")]
    let body_static = match sam {
        Some(s) => s.impl_is_static,
        None => callee.is_static,
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let body_static = callee.is_static;
    let recv = usize::from(!body_static);
    // arg_slot_widths = [recv=1, w(p0), w(p1), …]; drop the receiver prefix → per-arg slot widths.
    let arg_widths: Vec<usize> = arg_slot_widths(&callee.descriptor).into_iter().skip(1).collect();
    let mut arg_slot = vec![0usize; nargs];
    {
        let mut s = recv;
        for i in 0..nargs {
            arg_slot[i] = s;
            s += arg_widths.get(i).copied().unwrap_or(1);
        }
    }
    let nslots = recv + arg_widths.iter().sum::<usize>();
    let nvars = (callee.max_locals as usize).max(nslots);
    // The callee's incoming value POSITIONS: the receiver (when it has one) then each declared
    // parameter, ONE operand each even when a parameter is wide. `pos_slot` is the local slot a
    // position lands in — the identity mapping the ordinary path already performs, named so the
    // SAM path can bind a position that arrives from the closure's cells instead of the stack
    // through exactly the same arithmetic.
    #[cfg(feature = "next-gen-gc")]
    let n_pos = recv + nargs;
    #[cfg(feature = "next-gen-gc")]
    let pos_slot = |p: usize| -> usize {
        if p < recv {
            0
        } else {
            arg_slot[p - recv]
        }
    };
    // ⭐**DON'T ZERO WHAT THE NEXT INSTRUCTION OVERWRITES.** Every callee slot used to get an
    // `iconst 0` + `def_var` here, and then the argument binding immediately `def_var`d the
    // parameter slots again. The first definition of a parameter slot is dead by construction —
    // two IR instructions per parameter, per spliced site, at every level of a depth-8 chain,
    // that only exist for Cranelift's SSA builder to drop. `emit_op` never observes them: the
    // callee body's first read of a parameter slot is the arg value, on every path.
    //
    // Every OTHER slot keeps its zero, and that is not cosmetic: a callee may `aload`/`iload` a slot it
    // only stores on one arm of a branch, and a `use_var` with no reaching definition is not a
    // compile error — it is a phi over whatever the SSA builder invents. The wide-param PAIR slot
    // (`arg_slot[i]+1` for a long/double) is deliberately NOT in this set: nothing defines it,
    // so it keeps its zero.
    let mut prefilled = vec![false; nvars];
    if inline_emit2_enabled() {
        for &s in arg_slot.iter().take(nargs) {
            if s < nvars {
                prefilled[s] = true;
            }
        }
        if recv == 1 && nvars > 0 {
            prefilled[0] = true;
        }
    }
    // Fresh locals for the callee, zero-initialised; params filled from the args.
    let bvars: Vec<Variable> = (0..nvars)
        .map(|i| {
            let v = b.declare_var(I64);
            // PHASE-B STACK MAPS: an inlined callee's locals are spilled into (and reloaded
            // from) the splice's root buffer exactly like the caller's, so with the shadow
            // frame gone they need the same declaration the caller's locals get in
            // `emit::body`. Must precede the variable's first use.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::runtime::stackmaps::declaring() {
                b.declare_var_needs_stack_map(v);
            }
            if !prefilled[i] {
                let z = b.ins().iconst(I64, 0);
                b.def_var(v, z);
            }
            v
        })
        .collect();
    // FIELD-LICM: callee-local → OUTER-local provenance, carried through the arg
    // binding (a callee slot fed by a pinned outer `aload` inherits its pinning,
    // unless the callee itself stores to that slot).
    #[cfg(feature = "next-gen-gc")]
    let mut child_alias: Vec<Option<u16>> = vec![None; nvars];
    // ARENA PROOF, inherited the same way. An inlined callee runs INSIDE the caller's
    // invocation, so a value that arrived already proven arena is STILL that same
    // object here and needs no second guard. Pinning must hold on BOTH sides: the
    // caller only sets `SV::arena` from the slot its entry unswitch proved (never
    // stored there), and `callee_pin` re-checks that the CALLEE never stores to the
    // slot the value lands in. A store on either side leaves the entry false, so the
    // site keeps its own guard — this is where the proof expires under inlining.
    #[cfg(feature = "next-gen-gc")]
    let mut child_proof: Vec<bool> = vec![false; nvars];
    // `pinned_ref_slots` re-scans the WHOLE callee body and allocates a `Vec<bool>` on every
    // splice of it — and its answer is consumed by exactly two things, both of which need a
    // CALLER-side provenance to carry across the arg binding: the FIELD-LICM alias (`licm`) and
    // the arena proof (`SV::arena` on one of the values about to be popped). With neither
    // present the whole scan decides nothing, so don't run it — `callee_pin` stays empty and the
    // two `.get(..).unwrap_or(false)` reads below answer "not pinned", which is what an all-false
    // table would have said anyway.
    //
    // Note this is a REMOVAL, not a memo: a side table keyed by `code_key` was the obvious
    // alternative and this tree has twice measured that shape SLOWER than the plain scan it
    // replaced. Deciding the scan is unnecessary costs nothing and cannot go stale.
    #[cfg(feature = "next-gen-gc")]
    let callee_pin = {
        // A SAM splice has NO caller-side provenance to carry: every capture is read out of
        // memory (so it has no `src` alias and no `SV::arena` proof to inherit) and the closure
        // itself is consumed rather than bound to a slot. The scan would decide nothing, so it
        // does not run — the same "removal, not memo" reasoning as the arm below.
        let need = match sam {
            Some(_) => false,
            None => {
                !inline_emit2_enabled()
                    || licm.is_some()
                    // The emitter's operand stack holds ONE entry per value (a wide arg is one SV
                    // even though it lands in two local slots), so the incoming window is
                    // `nargs + recv` entries — NOT `nslots`.
                    || a_stack[a_stack.len().saturating_sub(nargs + recv)..]
                        .iter()
                        .any(|sv| sv.arena)
            }
        };
        if need { pinned_ref_slots(&callee.bytecode, nvars) } else { Vec::new() }
    };
    #[cfg(feature = "next-gen-gc")]
    let is_sam = sam.is_some();
    #[cfg(not(feature = "next-gen-gc"))]
    let is_sam = false;
    if is_sam {
        // ⭐⭐**THE SAM BINDING: CAPTURES FROM THE CLOSURE'S CELLS, ARGUMENTS FROM THE STACK.**
        //
        // This is the interpreted `splice_call_args` written as IR. `invoke_lambda_value` decodes
        // the closure's capture cells, prepends them to the SAM arguments, and calls the
        // implementation with that one vector; here the same vector is built directly in the
        // callee's local slots, with no `Vec` and no call.
        //
        // What licences reading a fixed offset off `clo.v` is the GUARD, not this code:
        // `emit_guarded_inline` has already proved the receiver is an arena object whose class id
        // is the profiled lambda class (⇒ its cell count and cell kinds are that capture
        // signature's, so `HDR + (AL_HDR_CELLS + p)*8` is in bounds and holds capture `p`) and
        // whose `AL_SITE_SLOT` cell is the profiled site index (⇒ the body below is THIS
        // closure's implementation, not another site's of the same capture shape). It has also
        // proved every REFERENCE capture cell is untagged, which is what makes the load below a
        // plain move: an untagged arena cell and a JIT operand cell are the same word.
        //
        // A primitive capture needs no proof at all — `encode_arena_cell` and `to_cell` agree
        // bit-for-bit on I/J/F/D — which is why `cap_is_ref` marks only the reference positions.
        #[cfg(feature = "next-gen-gc")]
        {
            let s = sam?;
            let n_caps = s.cap_is_ref.len();
            if n_caps + s.sam_nargs != n_pos {
                // Admission proves this equality (`ncaps + sam_nargs == positions`). Refusing
                // rather than proceeding is the difference between a missed inline and a local
                // slot left with no definition, which the SSA builder would fill with a phi over
                // whatever it invents.
                return None;
            }
            // The SAM's own arguments occupy the TAIL positions, last one highest on the stack.
            for p in (n_caps..n_pos).rev() {
                let arg = a_stack.pop()?;
                let cell = to_cell(b, arg);
                b.def_var(bvars[pos_slot(p)], cell);
            }
            // The closure sits below them. It is consumed here — it is the SOURCE of the leading
            // positions, never a value the implementation sees.
            let clo = a_stack.pop()?;
            for p in 0..n_caps {
                let off = (crate::nextgen::HDR + (crate::nextgen::AL_HDR_CELLS + p) * 8) as i32;
                let cell = b.ins().load(I64, MemFlags::trusted(), clo.v, Offset32::new(off));
                b.def_var(bvars[pos_slot(p)], cell);
            }
        }
    } else {
        // Args are on top (last arg highest); pop each into its (wide-aware) local slot.
        for i in (0..nargs).rev() {
            let arg = a_stack.pop()?;
            let slot = arg_slot[i];
            #[cfg(feature = "next-gen-gc")]
            if callee_pin.get(slot).copied().unwrap_or(false) {
                child_alias[slot] = arg.src;
                child_proof[slot] = arg.arena;
            }
            let cell = to_cell(b, arg);
            b.def_var(bvars[slot], cell);
        }
        // Receiver sits below the args; pop it into slot 0.
        if recv == 1 {
            let r = a_stack.pop()?;
            #[cfg(feature = "next-gen-gc")]
            if callee_pin.first().copied().unwrap_or(false) {
                child_alias[0] = r.src;
                child_proof[0] = r.arena;
            }
            let cell = to_cell(b, r);
            b.def_var(bvars[0], cell);
        }
    }
    // ── THE ROOTED SPLICE, entered here and left at every exit below. See
    //    `splice_roots_enabled` for the whole argument; `enter_splice_roots` for the ordering.
    //    Placed AFTER the argument binding on purpose: the caller's operand stack it roots must
    //    be the one that survives the splice, i.e. with this call's arguments already consumed.
    #[cfg(feature = "next-gen-gc")]
    let mut splice_win: Option<SpliceWindow> = None;
    #[allow(unused_mut)]
    let mut body_locals_ptr = locals_ptr;
    // ⛔A ROOTED SPLICE INSIDE ANOTHER SPLICE IS REFUSED, NOT ROOTED TWICE. The window is one
    // shared region per function, so a nested rooted splice would zero and re-register the region
    // its parent's live references are sitting in. Admission makes this unreachable (the arm is
    // depth-0 only, and a depth-0 entry lives only in the top-level `inline_targets` map); if it
    // ever becomes reachable, refusing the compile is the safe answer and this is where it happens.
    //
    // ⭐⭐**AND THE OBJECTION IS ABOUT THE WINDOW, WHICH UNDER THE STACK MAPS IS NOT EMITTED.**
    // Read the sentence above literally: every clause in it is about "the window" — one shared
    // region per function, zeroed, re-registered. `inline_free_enabled()` emits no window at any
    // depth (the `E_FREE_WINDOW` arm below), so a nested rooted splice re-registers nothing and
    // zeroes nothing; its callee locals are Cranelift variables declared into the maps at
    // creation, exactly like its parent's, and the parent's own ref operand stack is declared by
    // the parent's `declare_stack_roots` a few lines above THIS one, at whatever depth that is.
    //
    // ⚠**THIS REFUSAL ABORTS THE WHOLE COMPILE, IT DOES NOT SKIP A SITE** (`emit_inline`'s
    // `None` propagates through `emit_op`'s `?`), which is why it must stay in place whenever the
    // window can still be emitted. `RAVA_NO_DEEP_INLINE=1` and `RAVA_NO_STACKMAPS=1` both put it
    // back verbatim — and note the two conditions are not independent: with the maps off,
    // `deep_inline_enabled()` is false by construction.
    #[cfg(feature = "next-gen-gc")]
    if callee.splice_roots.is_some() && env.in_splice {
        if !crate::vm::inline_targets::deep_inline_enabled()
            || !crate::vm::inline_targets::inline_free_enabled()
        {
            return None;
        }
        crate::vm::inline_census::note(crate::vm::inline_census::E_DEEP_SPLICE);
    }
    // ⭐⭐**THE CALLER'S OPERAND STACK, DECLARED ONCE PER SPLICE — THE WHOLE ADDITION.**
    //
    // See `gcroots::declare_stack_roots`. Under the stack maps every other part of both frames is
    // already named by a declaration made where the value or variable is created; the caller's own
    // ref operand-stack values at THIS point are the only ones no declaration reaches, because
    // inside the body `spill_for_gc` is handed the CALLEE's stack. Declaring them costs no
    // instruction and is what lets every rooting-derived refusal below be retired.
    //
    // Placed after the argument binding for the reason `enter_splice_roots` documented: the stack
    // that must survive the splice is the one with this call's arguments already consumed.
    //
    // Unconditional over splices, not gated on "can this body collect": a value that is not live
    // across a safepoint produces no map entry, so a GC-leaf splice pays literally nothing, and a
    // gate here would be a second derivation of a fact `SafepointSpiller` already computes.
    #[cfg(feature = "next-gen-gc")]
    if crate::vm::inline_targets::inline_free_enabled() && declare_stack_roots(b, a_stack) > 0 {
        crate::vm::inline_census::note(crate::vm::inline_census::E_FREE_DECL);
    }
    // The `E_GP_*` rows count what a ROOTED splice contains. This is the same question for the
    // arm that never had a window — the widened interface body — so the two populations the round
    // moves are both a COUNT and not an estimate. One walk per spliced site at compile time.
    #[cfg(feature = "next-gen-gc")]
    if crate::vm::inline_targets::inline_free_enabled() && callee.splice_roots.is_none() {
        use crate::vm::inline_census as cen;
        let bc = &callee.bytecode;
        let mut pc = 0usize;
        let mut n = 0u32;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(l) = insn_len_at(bc, pc) else { break };
            if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) && pc + 2 < bc.len() {
                let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if !callee.nested.contains_key(&cp) && !callee.ctor_inlines.contains_key(&cp) {
                    n += 1;
                }
            }
            pc += l;
        }
        for _ in 0..n {
            cen::note(cen::E_IF_CALLS);
        }
    }
    #[cfg(feature = "next-gen-gc")]
    if let Some(slots) = callee.splice_roots.filter(|_| splice_roots_enabled()) {
        // ⭐**NO WINDOW UNDER THE STACK MAPS.** `enter_splice_roots` does three things and the
        // maps have made all three redundant: it ZEROES a frame window (so the collector never
        // scans stale words), PUSHES it on the thread's cell-root stack (so the callee's
        // `spill_for_gc` has a region disjoint from the caller's), and SPILLS the caller into
        // `locals_ptr` (so the caller's registers can be reloaded after a move). Under
        // `declaring()` the callee's `spill_for_gc` writes nothing into any window, and the
        // caller is rooted by declaration rather than by a store — so all that is left of the
        // three is a zero loop over a region nobody reads, a push/pop pair around it, and a
        // `reload_after_gc` that already emits nothing. The body then runs on the caller's
        // `locals_ptr` exactly as every non-rooted splice always has.
        //
        // ⚠**THE ONE OTHER READER OF A SPLICE'S `locals_ptr` IS NOT A ROOT SCAN.**
        // `emit_deopt_exit` hands the buffer to `rava_deopt`, which rebuilds an interpreter
        // `Frame` from those exact words at the CALLEE's own offsets — writing that image into
        // the caller's buffer would be a wrong frame, not a missed optimisation. It cannot
        // happen: a spliced body's `b_env` sets `local_types: None` (below), and `emit_deopt_exit`
        // returns `false` on that before it materialises anything. `emit_deopt_exit` now also
        // refuses `env.in_splice` outright, so the invariant is stated where it is relied on
        // instead of being a consequence of an unrelated field.
        if crate::vm::inline_targets::inline_free_enabled() {
            let _ = slots;
            crate::vm::inline_census::note(crate::vm::inline_census::E_FREE_WINDOW);
        } else {
            let (win, w) =
                enter_splice_roots(b, env, slots as usize, caller_vars, a_stack, locals_ptr, dv);
            splice_win = Some(w);
            body_locals_ptr = win;
        }
        // COMPILE-TIME CENSUS, one note per SPLICED site (never per call): what the round
        // actually put into machine code, and what is now running inside a spliced body. Counted
        // at emit and not at admission because an admitted callee whose caller then fails to
        // compile removes no dispatch at all.
        {
            use crate::vm::inline_census as cen;
            cen::note(cen::E_GP_EMIT);
            let bc = &callee.bytecode;
            let mut pc = 0usize;
            while pc < bc.len() {
                let op = bc[pc];
                let Some(l) = insn_len(op) else { break };
                if op == 0xbb {
                    cen::note(cen::E_GP_ALLOCS);
                } else if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) && pc + 2 < bc.len() {
                    let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                    if !callee.nested.contains_key(&cp) && !callee.ctor_inlines.contains_key(&cp) {
                        cen::note(cen::E_GP_CALLS);
                    }
                }
                pc += l;
            }
        }
    }

    #[cfg(feature = "next-gen-gc")]
    let child_licm = licm.map(|l| Licm {
        vars: l.vars,
        alias: &child_alias,
        vals: l.vals,
        flush: l.flush,
    });

    // ⚠⚠**EVERY LEVEL GETS ITS OWN cp-INDEXED SIDE MAPS, NOT ONLY A ROOTED ONE.**
    //
    // `Env::new_arena`, `ctor_inlines`, `concat_shapes` and `static_intrinsics` are keyed by
    // CONSTANT-POOL INDEX, and `..*env` hands a spliced body the CALLER's tables to look its own
    // indices up in. `env.rs` names this and records that the historic splice was left reading
    // them **deliberately**, because nothing could reach the first two — `is_inlinable_kind`
    // refuses `0xbb` and `0xba`, so `new_arena` and `concat_shapes` are unreachable from a
    // non-rooted splice.
    //
    // ⛔**THE OTHER TWO ARE REACHABLE AND ALWAYS WERE.** `is_inlinable_kind` admits `0xb8` and
    // `0xb7`, and `emit_op` consults `env.static_intrinsics` before anything else at an
    // invokestatic (`ops/invoke_static.rs`) and `env.ctor_inlines` at an invokespecial. A nested
    // callee's cp 12 finding whatever cp 12 means in the OUTER method's pool emits the wrong
    // intrinsic, or inlines the wrong constructor — a wrong VALUE, silently, not a missed
    // optimisation. Depth is what makes it likely: a depth-8 chain is eight unrelated pools
    // indexing one table.
    //
    // Both replacements are strictly CONSERVATIVE — an empty intrinsic map emits the ordinary
    // call, and the callee's own `ctor_inlines` (empty for every arm but the rooted splice) emits
    // the ordinary `<init>`. Gated so `RAVA_NO_DEEP_INLINE=1` restores the historic emission
    // byte for byte, which is the only reason it is not simply unconditional.
    #[cfg(feature = "next-gen-gc")]
    let own_cp_maps =
        callee.splice_roots.is_some() || crate::vm::inline_targets::deep_inline_enabled();
    #[cfg(not(feature = "next-gen-gc"))]
    let own_cp_maps = false;
    // The callee body runs under the same callbacks but the CALLEE's field map
    // (its getfield cp indices differ from the caller's pool). `statics` is keyed by
    // the CALLER's cp indices, so it must NOT carry into the callee (a colliding cp
    // would read the wrong cell) — the callee gets its OWN map (empty unless the
    // jit_run driver assigned cells under RAVA_REF_STATIC_CELLS).
    let b_env = Env {
        fields: &callee.fields,
        statics: &callee.statics,
        // The static-LICM readonly arm reads cells with NO gen guard and relies on
        // the OUTER prologue having populated them — which never populates CALLEE
        // cells. Force the callee's getstatics onto the gen-guarded diamond.
        static_licm: false,
        // 16.2 phase 2: the callee body's OWN inlinable invokes (keyed by the callee's cp indices).
        // Swapping this makes emit_op recurse into emit_inline for them → chained depth-N inlining.
        // Empty for a leaf callee (no invokes) — identical to the pre-16.2 behavior.
        inline_targets: &callee.nested,
        #[cfg(feature = "next-gen-gc")]
        hof_fusions: &callee.hof_fusions,
        #[cfg(feature = "next-gen-gc")]
        code_key: callee.code_key,
        #[cfg(feature = "next-gen-gc")]
        active_sam,
        // Everything below this point is a spliced body — see `Env::in_splice`.
        #[cfg(feature = "next-gen-gc")]
        in_splice: true,
        // 20.4 deep-inline: a NON-inlined invoke left mid-body (partial map) emits a real jit_call
        // that resolves the target via (env.class_ptr, cp). The cp is the CALLEE's — so class_ptr
        // MUST be the callee's class, not the outer method's, or (outer_class, callee_cp) resolves
        // the WRONG method → null (the 20.4 crash root, expert-confirmed). Full-collapse callees
        // leave no real call, so this override is inert for them.
        class_ptr: std::sync::Arc::as_ptr(&callee.class) as i64,
        // ⚠**THE cp-INDEXED SIDE MAPS, SWAPPED FOR A ROOTED SPLICE ONLY.** See `empty_new_arena`
        // for the full reasoning: these four are keyed by CONSTANT-POOL INDEX and `..*env` would
        // hand the callee the CALLER's tables to look its own indices up in. Nothing could reach
        // the first two before this round; a body containing `new` can. `ctor_inlines` gets the
        // CALLEE's own map rather than an empty one because collapsing `new X; …; <init>` into a
        // bump plus inline field stores is the point of admitting `new` at all.
        #[cfg(feature = "next-gen-gc")]
        new_arena: if own_cp_maps { empty_new_arena() } else { env.new_arena },
        #[cfg(feature = "next-gen-gc")]
        ctor_inlines: if own_cp_maps { &callee.ctor_inlines } else { env.ctor_inlines },
        #[cfg(feature = "next-gen-gc")]
        concat_shapes: if own_cp_maps { empty_concat_shapes() } else { env.concat_shapes },
        static_intrinsics: {
            #[cfg(feature = "next-gen-gc")]
            {
                if own_cp_maps { empty_static_intrinsics() } else { env.static_intrinsics }
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                env.static_intrinsics
            }
        },
        // ⛔**`array_fields` IS THE ONE MAP WHOSE EMPTY VALUE IS THE DANGEROUS ANSWER, SO IT IS
        // NOT SWAPPED WITH THE OTHERS.** Its consumer is `bare_ok = env.bare_allowed &&
        // !env.array_fields.contains(&idx)` — membership KEEPS the tag classify, so an EMPTY set
        // authorises MORE bare loads, not fewer, and `Env::bare_allowed`'s own comment records
        // exactly that inversion ("a subset authorises MORE bare loads"). The conservative answer
        // for a spliced body would be the CALLEE's own set, which no `InlineCallee` carries.
        // Left as the caller's, unchanged in both arms, and inert in every shipped configuration:
        // `bare_ok` is read only under `nextgen::bare_ref_load_enabled()` (`RAVA_JIT_BARE_REF_LOAD`),
        // a pilot that is default OFF and that stops a boot dead when it is on.
        array_fields: {
            #[cfg(feature = "next-gen-gc")]
            {
                if callee.splice_roots.is_some() {
                    empty_array_fields()
                } else {
                    env.array_fields
                }
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                env.array_fields
            }
        },
        // V4.1: inlined callee PCs are the callee's, not the top method's — never let a
        // numeric PC collision skip a bounds check in a spliced body. Keep all checks.
        // The caller's PCs belong to its bytecode; inline facts must not collide by number.
        nonnull_getfields: empty_bce(),
        bce_safe: empty_bce(),
        #[cfg(feature = "next-gen-gc")]
        prim_arrays: empty_prim_arrays(),
        #[cfg(feature = "next-gen-gc")]
        ref_arrays: empty_ref_arrays(),
        // ⚠**AND THE LOCAL TYPE MAP IS THE TOP METHOD'S, FOR THE SAME REASON.** Its only reader
        // is `emit_deopt_exit`, which indexes it BY PC and then describes the frame as the
        // interpreter must re-enter it. A spliced body's PCs belong to the callee, so a guarded
        // site inside one would be described with whatever the OUTER method's map happens to
        // hold at that numeric offset — a wrong frame, not a missed optimisation. Nothing else
        // reads the field, so clearing it here costs the deopt shape at nested guarded sites
        // (which then decline, see `emit_guarded_inline`) and nothing else. Until this round no
        // nested map could carry a guarded entry at all; `vm/lambda_prov.rs` plants one.
        local_types: None,
        ..*env
    };
    let bbc = &callee.bytecode;
    let bpool = &callee.class.file.pool;

    // ⭐⭐**A CALLEE THAT IS ONE BASIC BLOCK NEEDS NO BLOCKS AT ALL — AND MOST OF THEM ARE.**
    //
    // The general path below always builds a callee entry block, jumps into it from the caller's
    // current block, builds a `cont` block with ONE PARAMETER, and jumps the return value into
    // it. For a getter, a two-op arithmetic helper, a `Math.min` wrapper — the overwhelming
    // majority of what `is_inlinable` admits, and the whole population of a depth-8 chain — that
    // is two blocks, one block parameter and one jump laid down to express a value that never
    // left a register.
    //
    // Cranelift folds the single-predecessor jump long before the machine code, so **this changes
    // no emitted instruction**: the fast path pushes exactly the SV the `cont` parameter would
    // have produced (`push` sets `src: None` and `arena: false`, byte-for-byte the block-param
    // arm's construction), typed on the descriptor's return type exactly as `jt_ty(ret)` typed
    // the parameter. What it changes is how much IR the compiler has to chew: a block parameter
    // is a real phi, and a phi is a live range `regalloc2` must colour. Phase A's profile has our
    // OWN compiler — `regalloc2::ion` — holding a second thread at ~99% and 2.82% of all samples,
    // and every splice in every method compiled during that phase runs through this function.
    //
    // Three further removals fall out of taking this exit: `find_leaders` (a full second walk of
    // the body plus a `BTreeSet` and a `Vec`, per splice), the `bblocks` HashMap, and the
    // `bshapes` bookkeeping — all of them exist only to route branches this body does not have.
    //
    // ⚠**THE OPERAND STACK IS THE CALLEE'S, NOT THE CALLER'S.** `bstack` starts empty and the
    // return pops from it, exactly as the general path does; `a_stack` is only touched by the
    // final `push`. Emitting into the caller's *block* does not mean emitting onto the caller's
    // *stack*. (`emit_op` is free to create blocks of its own — a field diamond, an allocation
    // retry — and to leave the builder in whichever block it ends in; that is already true on
    // the general path, and "the caller's current block" here means only "no block boundary is
    // introduced BY THE SPLICE".)
    if lambda_loop_inline_enabled() && straight_line_return(bbc, ret.is_none()) {
        // `emit_op` consults these only for branch targets, and `straight_line_return` has proved
        // there are none. Empty maps, so a stray lookup answers "no such block" rather than
        // routing into a block that belongs to a different body.
        let no_blocks: std::collections::HashMap<usize, cranelift::prelude::Block> =
            std::collections::HashMap::new();
        let mut bshapes: std::collections::HashMap<usize, Vec<JT>> =
            std::collections::HashMap::new();
        let mut bstack: Vec<SV> = Vec::new();
        let mut cur_open = true;
        let mut bpc = 0usize;
        while bpc < bbc.len() {
            let op = bbc[bpc];
            let len = insn_len(op)?;
            if matches!(op, 0xac | 0xad | 0xae | 0xaf | 0xb0 | 0xb1) {
                // Unregister the splice window and refresh the caller's registers BEFORE the
                // result is handed over: the value the callee returns was produced after its last
                // GC point, but every reference the CALLER still holds was not.
                #[cfg(feature = "next-gen-gc")]
                if let Some(w) = splice_win {
                    leave_splice_roots(b, w, caller_vars, a_stack, locals_ptr);
                }
                // The single terminal return, whose shape `straight_line_return` has already
                // matched against the descriptor.
                match ret {
                    // Hand the value straight to the caller's stack under the DESCRIPTOR's return
                    // type — the same retyping the `cont` block parameter performed, and the
                    // reason a `JT::Int`-shaped `ireturn` serving a `Z`/`B`/`S` descriptor keeps
                    // behaving as it did.
                    Some(t) => {
                        let r = bstack.pop()?;
                        push(a_stack, r.v, t);
                    }
                    // VOID: push nothing. The callee's own operand stack is discarded with the
                    // splice, and the caller's is left exactly as the call would have left it —
                    // args consumed by the binding above, no result. Anything else here is a
                    // stack-shape corruption that no gate would catch.
                    None => {}
                }
                return Some(());
            }
            emit_op(
                b, &bvars, &mut bstack, bbc, bpc, op, &no_blocks, &mut bshapes, &mut cur_open,
                bpool, &b_env, array_desc_cell,
                // THE CALLEE'S ROOT BUFFER: its own registered window when this is a rooted
                // splice, the caller's frame (as always) otherwise.
                body_locals_ptr,
                cow,
                #[cfg(feature = "next-gen-gc")]
                &child_proof,
                #[cfg(feature = "next-gen-gc")]
                None,
                #[cfg(feature = "next-gen-gc")]
                child_licm,
                #[cfg(feature = "next-gen-gc")]
                dv,
            )?;
            bpc += len;
        }
        // Unreachable: `straight_line_return` proved the last instruction is the return the loop
        // exits through. Refuse rather than fall out having pushed nothing — a caller reading a
        // value that was never pushed is a stack-shape corruption, not a missed optimisation.
        return None;
    }

    let leaders = find_leaders(bbc)?;
    // The callee's OWN basic blocks (keyed by callee pc — a separate map from the
    // caller's). A shared continuation block collects every return's value.
    let mut bblocks = std::collections::HashMap::new();
    for &l in &leaders {
        bblocks.insert(l, b.create_block());
    }
    let mut bshapes: std::collections::HashMap<usize, Vec<JT>> = std::collections::HashMap::new();
    bshapes.insert(0, Vec::new());
    let cont = b.create_block();
    // A VOID body's continuation takes no parameter: there is no value for the returns to carry
    // and none for the caller to pop. Every value body keeps the single typed parameter it had.
    if let Some(t) = ret {
        b.append_block_param(cont, jt_ty(t));
    }

    // Enter the callee body; the caller's current block flows into callee block 0.
    b.ins().jump(bblocks[&0], &[]);

    let mut bstack: Vec<SV> = Vec::new();
    let mut bpc = 0usize;
    let mut cur_open = false;
    while bpc < bbc.len() {
        if let Some(&blk) = bblocks.get(&bpc) {
            if cur_open {
                jump_to(b, &bblocks, &mut bshapes, bpc, &bstack)?;
            } else if !bshapes.contains_key(&bpc) {
                return None;
            }
            b.switch_to_block(blk);
            let shape = bshapes.get(&bpc)?.clone();
            let params = b.block_params(blk);
            if params.len() != shape.len() {
                return None;
            }
            bstack = shape
                .iter()
                .zip(params.iter())
                .map(|(&t, &v)| SV {
                    v,
                    t,
                    src: None,
                    #[cfg(feature = "next-gen-gc")]
                    arena: false,
                })
                .collect();
            cur_open = true;
        } else if !cur_open {
            return None;
        }
        let op = bbc[bpc];
        let len = insn_len(op)?;
        match op {
            0xac | 0xad | 0xae | 0xaf | 0xb0 => {
                // A value return inside a body the descriptor calls void would have nothing to
                // hand to a parameterless continuation. Refuse rather than invent one.
                if ret.is_none() {
                    return None;
                }
                let r = bstack.pop()?;
                b.ins().jump(cont, &[r.v.into()]);
                cur_open = false;
            }
            // VOID RETURN. The mirror of the arm above: jump to the continuation carrying
            // nothing, and push nothing at the merge. A `return` in a body whose descriptor
            // promises a value is malformed — refuse it, exactly as the other direction does.
            0xb1 => {
                if ret.is_some() {
                    return None;
                }
                b.ins().jump(cont, &[]);
                cur_open = false;
            }
            _ => emit_op(
                b, &bvars, &mut bstack, bbc, bpc, op, &bblocks, &mut bshapes, &mut cur_open, bpool,
                &b_env, array_desc_cell,
                // The callee's own registered window when this is a rooted splice (see above).
                body_locals_ptr,
                cow,
                #[cfg(feature = "next-gen-gc")]
                &child_proof,
                // Inline callees never contain `new` (is_inlinable excludes 0xbb).
                #[cfg(feature = "next-gen-gc")]
                None,
                #[cfg(feature = "next-gen-gc")]
                child_licm,
                // The CALLER's frame discipline: an inlined athrow returns from
                // the caller function, so its epilogue must run there.
                #[cfg(feature = "next-gen-gc")]
                dv,
            )?,
        }
        bpc += len;
    }
    for &l in &leaders {
        b.seal_block(bblocks[&l]);
    }
    b.seal_block(cont);
    b.switch_to_block(cont);
    // Every return in the body jumps here, so this is the splice's single exit — see the
    // straight-line arm above for why the caller is refreshed before the result is pushed.
    #[cfg(feature = "next-gen-gc")]
    if let Some(w) = splice_win {
        leave_splice_roots(b, w, caller_vars, a_stack, locals_ptr);
    }
    if let Some(t) = ret {
        let result = b.block_params(cont)[0];
        push(a_stack, result, t);
    }
    Some(())
}

/// INLINE TRIVIAL CONSTRUCTOR (next-gen-gc). The `<init>` was proven a pure field
/// setter (`analyze_trivial_ctor`), so its `invokespecial` folds to direct field
/// stores on the receiver with NO `rava_invoke` callback — the last per-allocation
/// callback on value-class construction. The operand stack is
/// `[..., receiver, arg0, arg1, ..., arg{n-1}]`; pop the args + receiver (void
/// return pushes nothing — `invokespecial <init>` consumes the receiver, leaving
/// any earlier `dup` copy below). Each store reuses the proven dual-path field
/// writers: primitives inline-store raw bits, refs encode → low-2-bit + inline-
/// store + the thin `rava_card_mark` barrier. A non-arena receiver (handle) takes
/// the dual path's callback arm, so the inline is always correct. No GC-capable
/// callback fires between the bump and these stores, so no spill/reload is needed.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_inline_ctor(
    b: &mut FunctionBuilder,
    env: &Env,
    stack: &mut Vec<SV>,
    ctx_cell: Value,
    ci: &CtorInline,
) -> Option<()> {
    let mut args: Vec<SV> = Vec::with_capacity(ci.n_params);
    for _ in 0..ci.n_params {
        args.push(stack.pop()?);
    }
    args.reverse(); // args[0] = arg0 … args[n-1] = arg{n-1}
    let recv = stack.pop()?; // the receiver (consumed by invokespecial)
    for &(arg_index, slot, fid, jt) in &ci.stores {
        let val = *args.get(arg_index)?;
        let val_cell = to_cell(b, val);
        if jt == JT::Ref {
            // Ctor-inline stores: the callee's own field descriptors are not in THIS pool's
            // index space, so the conservative answer is "keep the tag" — a spliced store
            // never goes bare. Costs nothing today (the flip is off) and cannot disagree
            // with a tagged load later.
            native_putfield_ref_arena_dual(b, env, ctx_cell, recv.v, val_cell, fid as i64, slot, false);
        } else {
            let kind = match jt {
                JT::Int => 0,
                JT::Long => 1,
                JT::Float => 2,
                JT::Double => 3,
                JT::Ref => 4,
            };
            native_putfield_const_prim_arena_dual(
                b, env, ctx_cell, recv.v, val_cell, fid as i64, slot, kind,
            );
        }
    }
    Some(())
}

/// Lay the arena header and wipe the cells, exactly as `tlab_try_alloc` does: header word at
/// `+0`, `obj_init_hdr_tail` over bytes `[8, HDR)`, then every cell zeroed. The nursery is
/// recycled by resetting `top` WITHOUT wiping bytes, so a skipped tail leaves a stale forwarding
/// word that the next collection mines as a real forwarding pointer, and a skipped cell leaves a
/// stale pointer the tracer follows — both are silent heap corruption, not a wrong value.
///
/// `unroll` = the cell count when it is known at emit time (a store run, no branches); `None`
/// emits a counted loop over `nfv`. The loop is ~5 instructions a cell against the hundreds of
/// nanoseconds of `new_object_value` this whole arm removes, so it is still the cheap side.
/// Leaves the builder in the block that follows the wipe.
#[cfg(feature = "next-gen-gc")]
fn emit_arena_hdr_and_wipe(
    b: &mut FunctionBuilder,
    base: Value,
    hdrw: Value,
    nfv: Value,
    unroll: Option<u32>,
) {
    b.ins().store(MemFlags::trusted(), hdrw, base, Offset32::new(0));
    let zero = b.ins().iconst(I64, 0);
    b.ins().store(MemFlags::trusted(), zero, base, Offset32::new(8));
    if crate::nextgen::HDR > 16 {
        b.ins().store(MemFlags::trusted(), zero, base, Offset32::new(16));
    }
    if let Some(nf) = unroll {
        for i in 0..nf as i32 {
            let off = crate::nextgen::HDR as i32 + i * 8;
            b.ins().store(MemFlags::trusted(), zero, base, Offset32::new(off));
        }
        return;
    }
    let head = b.create_block();
    b.append_block_param(head, I64);
    let body = b.create_block();
    let done = b.create_block();
    let i0 = b.ins().iconst(I64, 0);
    b.ins().jump(head, &[i0.into()]);

    b.switch_to_block(head); // two predecessors (entry + back edge) — sealed below
    let i = b.block_params(head)[0];
    let more = b.ins().icmp(IntCC::UnsignedLessThan, i, nfv);
    b.ins().brif(more, body, &[], done, &[]);

    b.switch_to_block(body);
    b.seal_block(body);
    let byte_off = b.ins().imul_imm(i, 8);
    let addr = b.ins().iadd(base, byte_off);
    let z = b.ins().iconst(I64, 0);
    b.ins().store(MemFlags::trusted(), z, addr, Offset32::new(crate::nextgen::HDR as i32));
    let i2 = b.ins().iadd_imm(i, 1);
    b.ins().jump(head, &[i2.into()]);
    b.seal_block(head);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// LATE-BOUND INLINE TLAB BUMP for a `new` (0xbb) site that `Vm::build_new_arena` could NOT
/// prove arena-eligible at emit time (next-gen-gc). Kill switch `RAVA_NO_INLINE_NEW=1`.
///
/// ⭐**The site's shape is read at RUNTIME, not baked, and that is the whole point.**
/// `build_new_arena`'s predicate has one non-permanent term — `initialized` — and a method
/// compiles at its threshold, routinely before a cold `new` inside it has ever run its target's
/// `<clinit>`. Such a site was then stuck on `rava_new` forever, because nothing re-asked. Here
/// the emitter bakes only the ADDRESS of the site's shape word; `publish_new_site` fills it in
/// from the object the callback actually built, and the guard below reads it every execution.
/// This is the same discipline `lambda_mint` needed for indy shapes, for the same reason.
///
/// The guard rejects both non-inlinable states with ONE signed compare: `0` (never observed) and
/// bit-63 POISON (observed and refused) are both `<= 0`, and a real header word
/// `(nfields<<32)|class_id` is positive because `nfields` is capped at 4096 on publication.
///
/// REFUSAL IS INVISIBLE: the slow arm is the pre-existing `new_obj` callback, unmodified —
/// same spill, same call, same reload — so a site that never publishes runs exactly the code it
/// ran before, plus one load and one branch off the hot path.
///
/// Overflow behaves exactly like `emit_inline_arena_new`: spill, native Cheney safepoint, reload,
/// machine back-edge into the freed nursery, and the same BOUNDED EXIT when the safepoint reports
/// it freed nothing (a gil-free worker with an exhausted spill pool) — without that arm the back
/// edge is unconditional and spins, which is a measured wall, not a hypothetical.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_arena_new_late(
    b: &mut FunctionBuilder,
    env: &Env,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    cursor_var: Option<Variable>,
    slot_addr: i64,
    cp: u16,
    locals_ptr: Value,
) -> Option<()> {
    use cranelift::codegen::ir::BlockArg;

    // The shape as of EMIT time. Published → bake the size, the header and an unrolled wipe and
    // guard on exact equality. Unpublished → the generic arm, sized from the word it loads.
    let known = crate::jit::new_site_known(slot_addr);
    // Unroll the cell wipe only for shapes small enough that a store run is cheaper than a
    // counted loop AND does not bloat the body: past 32 cells the run stops paying for itself
    // in icache, and the loop is exact for any width.
    let unroll = known.map(|hw| (hw >> 32) as u32).filter(|&n| n <= 32);

    // Ref-typed operand-stack slots: threaded through the retry/cont merge so a GC on the
    // overflow arm (or in the callback on the slow arm) is reflected on every path out.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();
    let nref = ref_slots.len();
    // The stack as it stands AT THE SITE. The overflow arm rewrites `stack[i].v` to its
    // iteration's values and the slow arm rewrites them to its post-callback reloads, so the
    // emit-time view has to be restored between the two.
    let entry_refs: Vec<Value> = ref_slots.iter().map(|&i| stack[i].v).collect();

    let have = b.create_block();
    let slow = b.create_block();
    let retry = b.create_block();
    for _ in 0..nref {
        b.append_block_param(retry, I64);
    }
    let ok = b.create_block();
    let overflow = b.create_block();
    // BOUNDED EXIT. Sole successor of `overflow` when the safepoint reports it freed nothing.
    // Takes NO back edge — that is the whole point.
    let bail = b.create_block();
    for _ in 0..nref {
        b.append_block_param(bail, I64);
    }
    let cont = b.create_block();
    b.append_block_param(cont, I64); // result cell
    for _ in 0..nref {
        b.append_block_param(cont, I64);
    }

    // ── guard: one load of the site's shape word, one compare. Static address, always mapped.
    let sa = b.ins().iconst(I64, slot_addr);
    let w = b.ins().load(I64, MemFlags::trusted(), sa, Offset32::new(0));
    let shape_ok = match known {
        Some(hw) => b.ins().icmp_imm(IntCC::Equal, w, hw as i64),
        None => b.ins().icmp_imm(IntCC::SignedGreaterThan, w, 0),
    };
    b.ins().brif(shape_ok, have, &[], slow, &[]);

    // ── slow: the untouched callback. Emitted FIRST so it still sees the operand stack as it
    //    stood at the site. `rava_new` also PUBLISHES this site's shape, which is what makes the
    //    first execution the last one to come here.
    b.switch_to_block(slow);
    b.seal_block(slow);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    spill_for_gc(b, vars, stack, locals_ptr);
    let call = b.ins().call(env.new_obj, &[cls, cpv]);
    let res = b.inst_results(call)[0];
    reload_after_gc(b, vars, stack, locals_ptr);
    let mut slow_args: Vec<BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(cont, &slow_args);
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = entry_refs[k];
    }

    // ── have: the cursor address. A method with at least one compile-time-eligible `new` (or an
    //    inline concat) already fetched it in the prologue, so reuse that Variable. A method
    //    whose `new` sites are ALL late-bound has no prologue fetch to reuse and takes one
    //    `jit_cursor_addr` call here instead — it is idempotent (it syncs `young.top` from the
    //    cursor BEFORE republishing, so the cursor never moves backwards), it never collects,
    //    and it is the difference between this arm reaching those methods and not existing for
    //    them at all. `RAVA_NO_INLINE_NEW_SITECURSOR=1` gives that half its own bisect handle.
    b.switch_to_block(have);
    b.seal_block(have);
    let cur = match cursor_var {
        Some(cv) => b.use_var(cv),
        None => {
            let c = b.ins().call(env.cursor_addr_fn, &[]);
            b.inst_results(c)[0]
        }
    };
    let init: Vec<BlockArg> = entry_refs.iter().map(|&v| -> BlockArg { v.into() }).collect();
    b.ins().jump(retry, &init);

    // ── retry: re-read the cursor each iteration (post-GC it points at the freed base). NOT
    //    sealed yet — the overflow back-edge is a second predecessor. `have` is retry's
    //    immediate dominator, so `cur` (and `w`, from the guard) flow in without block args.
    b.switch_to_block(retry);
    let rparams: Vec<Value> = b.block_params(retry).to_vec();
    let next = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(0));
    let end = b.ins().load(I64, MemFlags::trusted(), cur, Offset32::new(8));
    let (size, hdrw, nfv) = match known {
        Some(hw) => {
            let nf = (hw >> 32) as i64;
            (
                b.ins().iconst(I64, crate::nextgen::HDR as i64 + nf * 8),
                b.ins().iconst(I64, hw as i64),
                b.ins().iconst(I64, nf),
            )
        }
        None => {
            let nf = b.ins().ushr_imm(w, 32);
            let bytes = b.ins().imul_imm(nf, 8);
            (b.ins().iadd_imm(bytes, crate::nextgen::HDR as i64), w, nf)
        }
    };
    let nn = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, nn, end);
    b.ins().brif(fits, ok, &[], overflow, &[]);

    // ── ok: publish the new top, lay the header + wipe the cells, hand the raw pointer on. The
    //    result is the exact cell `box_ref(Value::Obj)` produces, so every downstream field op,
    //    GC scan and `unbox_ref` decodes it with no extra work.
    b.switch_to_block(ok);
    b.seal_block(ok);
    b.ins().store(MemFlags::trusted(), nn, cur, Offset32::new(0));
    emit_arena_hdr_and_wipe(b, next, hdrw, nfv, unroll);
    let mut ok_args: Vec<BlockArg> = vec![next.into()];
    ok_args.extend(rparams.iter().map(|&v| -> BlockArg { v.into() }));
    b.ins().jump(cont, &ok_args);

    // ── overflow: spill live roots (reflecting THIS iteration's ref-stack values), run the
    //    native safepoint, reload, loop back with the forwarded pointers.
    b.switch_to_block(overflow);
    b.seal_block(overflow);
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = rparams[k];
    }
    spill_for_gc(b, vars, stack, locals_ptr);
    let sp = b.ins().call(env.arena_safepoint_fn, &[]);
    // NON-ZERO = "I could not free you any space". On the main thread the safepoint always
    // collects, so this is a constant 0 and the fast path keeps its shape.
    let starved = b.inst_results(sp)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut back: Vec<BlockArg> = Vec::with_capacity(nref);
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        back.push(rv.into());
    }
    b.ins().brif(starved, bail, &back, retry, &back);
    b.seal_block(retry); // both predecessors (have + the back edge) are now emitted
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = entry_refs[k];
    }

    // ── bail: the region is already aborted (the deopt allocator raises it), so these bytes are
    //    throwaway — every result downstream is discarded and the task re-runs serially. Lay the
    //    SAME header down as `ok` so the compiled body's field ops stay well-typed on the way
    //    out, and return normally. No back edge exists here, so the loop is bounded.
    b.switch_to_block(bail);
    b.seal_block(bail);
    let bparams: Vec<Value> = b.block_params(bail).to_vec();
    let dcall = b.ins().call(env.arena_deopt_alloc_fn, &[size]);
    let scratch = b.inst_results(dcall)[0];
    emit_arena_hdr_and_wipe(b, scratch, hdrw, nfv, unroll);
    let mut bail_args: Vec<BlockArg> = vec![scratch.into()];
    bail_args.extend(bparams.iter().map(|&v| -> BlockArg { v.into() }));
    b.ins().jump(cont, &bail_args);

    // ── cont: the merge. Adopt the forwarded ref-stack values + push the result.
    b.switch_to_block(cont);
    b.seal_block(cont);
    let cparams: Vec<Value> = b.block_params(cont).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = cparams[1 + k];
    }
    push(stack, cparams[0], JT::Ref);
    Some(())
}

/// `RAVA_NO_INLINE_NEW_SITECURSOR=1`: keep the late-bound arm, but only in methods that already
/// fetched the TLAB cursor in their prologue. Bisect handle for the one half of this arm that
/// adds a call rather than removing one — `lambda_mint` has three reverted widenings on record
/// that all looked like the prologue and were not, so the two halves stay separable.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inline_new_site_cursor_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_INLINE_NEW_SITECURSOR").is_none())
}
