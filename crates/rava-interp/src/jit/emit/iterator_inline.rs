//! **`Iterator.hasNext()` WITHOUT A CALL — 4,023,040 of them a boot.**
//!
//! The receiver census puts `java/util/Iterator#hasNext` at 4.0 M calls and `#next` at 3.2 M,
//! second only to the two `getDouble` families. Unlike those, this receiver is a class THIS
//! ENGINE fabricates, and its backing is ordinary guest fields — `$elems` and `$pos` — which is
//! exactly the condition the `getDouble` arm failed (fastutil keeps its doubles outside the field
//! table entirely) and the working `ArrayList.get` arm satisfies.
//!
//! The native's answer, from `fu_fastpath`'s iterator arm, is three reads and a compare:
//!
//! ```text
//! pos = $pos; elems = $elems; hasNext = pos < array_len(elems)
//! ```
//!
//! and it uses `array_len`, not `elems_len` — an iterator's snapshot is exactly sized.
//!
//! ## ⛔What the boot said about all four arms together
//!
//! ```text
//! it_fast  4,011,294   nx_fast 3,229,936   cb_fast 2,786,224   cs_fast 915,305
//! total   10,942,759 host calls a boot that no longer happen
//!
//! six alternated boots, CPU seconds:  off 37.1 37.3 37.7 | on 37.4 37.4 37.7  = +0.81%, overlapping
//! ```
//!
//! **Ten point nine million removed calls do not show.** At the meter's ~68 ns that should have
//! been 0.74 s of a 33 s boot, and it is not there — so the estimate was wrong, in a way worth
//! writing down:
//!
//! ⭐**The average price of a population is not the price of its cheapest members.** 68 ns is the
//! trimmed mean across everything `fu_fastpath` serves, and these four are the simplest arms it
//! has — a field read and a compare. Their real cost is far below the average that the expensive
//! operations pull up.
//!
//! ⚠And the guards are paid by more calls than they serve: `cb_cls` sees 6.97 M to serve 2.79 M,
//! and the `size` arm tests the class word at every `size()` site while 4.96 M of them still
//! reach the native. A guard that refuses is not free — the same admission symmetry recorded for
//! `fu_routable` earlier.
//!
//! ⇒ The arms stay (they are strictly less work, gated and probed), but the honest reading is
//! **counted, not measured**: 10.9 M calls removed, boot unchanged. To move a boot this way the
//! target has to be both large AND expensive per call, and this population is only large.
//!
//! ## ⛔Converting these arms to the deopt TRAP shape failed, and the gates caught it
//!
//! With deoptimisation built, the obvious next move was to give these four arms the shape that
//! flipped devirtualisation's sign (+2.42% as a diamond, -1.12% as a trap): guard, straight-line
//! body, and a failing edge that LEAVES the function instead of merging.
//!
//! A mechanical conversion of `hasNext` — the largest arm at 4.0 M calls — turned **all four
//! bit-exact gates red** while the 21 differential probes stayed green. Reverted.
//!
//! ⛔**And the obvious explanation is already ruled out.** The first guess was a frame-layout
//! mismatch — `emit_deopt_exit` lays out `locals[0..vars.len()]` then the stack, while `Frame`
//! uses `code.max_locals` as `nlocals`. They are the same number: `build_body` creates the
//! variables as `(0..max_locals)`, so the layout is consistent and that is not the fault.
//!
//! **Bisected: it is the TRANSITION, not the inline half.** Replacing the whole arm with an
//! UNCONDITIONAL deopt — every call leaves through the interpreter, no inline body at all — still
//! turns all four gates red. So the fault is in resuming from one of THESE sites, and two more
//! hypotheses are eliminated:
//!
//! * frame layout: `build_body` creates the locals as `(0..max_locals)`, so `vars.len()` is
//!   exactly `code.max_locals` and the deopt buffer matches `Frame` — ruled out;
//! * reference returns: `jit_call_body` packs a ref result with `box_ref` and everything else
//!   with `pack_local`, and `rava_deopt` now does the same — the gates stayed red, so that was
//!   not it either (the fix is kept because it is correct regardless).
//!
//! ⭐**The remaining hypothesis, and the one that fits: the arena bump cursor.** Compiled code
//! allocates inline by bumping a cursor and reconciles lazily (`sync_young_from_cursor` exists
//! for exactly this). A deopt in the middle of such a method hands the interpreter a heap whose
//! young region has objects the interpreter does not know were allocated. `guarded_inline`'s
//! deopt sites work, and they sit at calls to real bytecode methods; these sit inside
//! allocation-dense collection loops. **Check the cursor is synced before the transition.**
//!
//! ⭐The probes passing while the gates failed is itself the lesson repeated: the probes compare
//! against `java -Xint` on small programs, the gates hash a real workload. Neither subsumes the
//! other, and a frame-layout error is exactly the kind of thing only the second can see.
//!
//! ## What today taught this file before a line of it ran
//!
//! * **The guard word comes from a LIVE receiver.** Publishing it by interning the class name at
//!   VM construction produced a different `Arc<str>` than the objects carry, and cost six cycles
//!   and 16.9 M missed guards to find. `fu_fastpath` publishes this one where it classifies.
//! * **Live references cross the slow arm.** A callback can collect and move them, so each gets a
//!   merge-block parameter — the restriction that quietly held `ArrayList.get` to one percent of
//!   its population until it was lifted the same way.
//! * **Every guard step is counted.** Bit-exact gates cannot show a guard that never fires; two
//!   dead arms hid behind green gates for months. `IT_CLS` / `IT_FLD` / `IT_FAST` say which step
//!   the traffic dies at, in one run.

use crate::jit::*;

/// Reached the class compare.
pub(crate) static IT_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Passed it, and `$elems` really was an array.
pub(crate) static IT_FLD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline.
pub(crate) static IT_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn it_cls() -> u64 {
    IT_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn it_fld() -> u64 {
    IT_FLD.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn it_fast() -> u64 {
    IT_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

fn bump(b: &mut FunctionBuilder, c: &'static std::sync::atomic::AtomicU64) {
    let addr = b.ins().iconst(I64, (c as *const _) as i64);
    let cur = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let inc = b.ins().iadd_imm(cur, 1);
    b.ins().store(MemFlags::trusted(), inc, addr, Offset32::new(0));
}

/// `RAVA_NO_ITER_HASNEXT=1` restores the callback.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ITER_HASNEXT").is_none())
}

/// True when the guard word and both field ids have been published, i.e. an iterator has been
/// seen at least once in this process. Until then the arm is not emitted at all.
pub(crate) fn ready() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    crate::jit::ITER_CLASS_WORD.load(Relaxed) != 0
        && crate::jit::ITER_ELEMS_FID.load(Relaxed) != 0
        && crate::jit::ITER_POS_FID.load(Relaxed) != 0
        && crate::jit::ITER_SOURCE_FID.load(Relaxed) != 0
}

/// ⭐**THE ONE THING THAT MADE THE TRAP SHAPE WRONG AT THESE FOUR SITES.**
///
/// A deopt resumes the interpreter AT THE INVOKE, so the frame handed over must be the frame the
/// invoke expects — **receiver and arguments still on the operand stack.** Every arm here pops
/// them at its top, so passing the arm's `stack` straight to `emit_deopt_exit` describes a stack
/// one or two slots SHORT, with an `nstack` to match. Nothing crashes: the interpreter simply
/// resumes on a frame that is wrong, and it surfaces only as the bit-exact gates going red.
///
/// That is exactly what happened, and it is why the 6.65 M forced deopts in `guarded_inline`
/// were green all along — that emitter leaves the stack untouched and says so. `popped` restores
/// what the arm consumed, in stack order (receiver first), for the DESCRIPTION only.
///
/// Answers false when the site cannot be described exactly, and then emits nothing: the caller
/// falls through to the native callback, which is always correct.
#[cfg(feature = "next-gen-gc")]
fn try_trap(
    b: &mut FunctionBuilder,
    env: &Env,
    vars: &[Variable],
    stack: &[SV],
    popped: &[SV],
    locals_ptr: Value,
    pc: usize,
    cp: u16,
    arm: u32,
) -> bool {
    if crate::jit::iter_trap_mask() & arm == 0 || !crate::jit::deopt_enabled() {
        return false;
    }
    // BISECTION ONLY (`RAVA_TRAP_ONLY=<substring>`): restrict the trap to sites in one enclosing
    // class, so the arm that turns a gate red can be found by name instead of by reading every
    // emission. Never a production input — this is the same tool as `RAVA_DEVIRT_LIMIT`.
    {
        static ONLY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        let only = ONLY.get_or_init(|| std::env::var("RAVA_TRAP_ONLY").ok());
        if let Some(want) = only {
            if env.class_ptr == 0 {
                return false;
            }
            let c = unsafe { &*(env.class_ptr as *const crate::class_loader::LoadedClass) };
            if !c.file.this_class.contains(want.as_str()) {
                return false;
            }
        }
    }
    let mut as_found: Vec<SV> = stack.to_vec();
    as_found.extend_from_slice(popped);
    crate::jit::emit::guarded_inline::emit_deopt_exit(b, env, vars, &as_found, locals_ptr, pc, cp)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_iter_hasnext_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
    // The bytecode index of the INVOKE this arm replaces. Only the trap shape reads it, and it
    // must be the invoke itself: a deopt resumes by re-executing the call the guard refused.
    pc: usize,
) -> Option<()> {
    use std::sync::atomic::Ordering::Relaxed;
    let elems_fid = crate::jit::ITER_ELEMS_FID.load(Relaxed) - 1;
    let pos_fid = crate::jit::ITER_POS_FID.load(Relaxed) - 1;

    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let read_blk = b.create_block();
    let calc_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    // Guard 1: a heap-Ref receiver.
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls_blk, &[], slow, &[]);

    // Guard 2: the exact published `java/util/Iterator` class word.
    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    bump(b, &IT_CLS);
    let cw_addr = b.ins().iconst(
        I64,
        (&crate::jit::ITER_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g2 = b.ins().band(cw_match, baked_nz);
    b.ins().brif(g2, read_blk, &[], slow, &[]);

    // Guard 3: `$elems` is an array.
    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    let elems = native_getfield_ref(b, env, array_desc_cell, recv, elems_fid);
    let ehi = b.ins().ushr_imm(elems, 32);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, ehi, ARRAY_TAG_HI as i64);
    b.ins().brif(is_arr, calc_blk, &[], slow, &[]);

    // `pos < len`, exactly as the native computes it.
    b.switch_to_block(calc_blk);
    b.seal_block(calc_blk);
    bump(b, &IT_FLD);
    let pos = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, pos_fid);
    let (_data, len) = load_array_desc(b, array_desc_cell, elems);
    let has = b.ins().icmp(IntCC::SignedLessThan, pos, len);
    let hasi = b.ins().uextend(I64, has);
    bump(b, &IT_FAST);
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![hasi.into()];
    for &i in &ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);

    // SLOW: the callback the site used before — the oracle for a different iterator shape, a
    // fabricated one with no backing, and every call before the word is published.
    b.switch_to_block(slow);
    b.seal_block(slow);

    // The trap shape: the failing edge LEAVES the function, so `merge` keeps its single
    // remaining predecessor (`calc_blk`) and the inline half stays straight-line code.
    #[cfg(feature = "next-gen-gc")]
    if try_trap(b, env, vars, stack, &[recv_sv], locals_ptr, pc, cp, 1) {
        b.switch_to_block(merge);
        b.seal_block(merge);
        let mparams: Vec<Value> = b.block_params(merge).to_vec();
        for (k, &i) in ref_slots.iter().enumerate() {
            stack[i].v = mparams[1 + k];
        }
        let sv = cell_to_sv(b, mparams[0], JT::Int);
        stack.push(sv);
        return Some(());
    }

    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Int);
    stack.push(sv);
    Some(())
}

/// Reached the class compare on `next()`.
pub(crate) static NX_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline: element read and `$pos` advanced.
pub(crate) static NX_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn nx_cls() -> u64 {
    NX_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn nx_fast() -> u64 {
    NX_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// **`Iterator.next()` — 3,173,120 calls a boot.** The same three reads as `hasNext`, plus the
/// element load and the `$pos` advance, byte-identical to the native:
///
/// ```text
/// if pos < 0 || pos >= len -> the callback, so an exhausted iterator throws exactly as before
/// v = elems[pos]; $pos = pos + 1; return v
/// ```
///
/// ⚠**This arm MUTATES**, which the read-only arms do not. The write happens only on the path
/// where every guard passed and the index is proven in range, so the slow arm still sees an
/// untouched receiver — a partial advance followed by a callback would double-step the iterator.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_iter_next_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
    // The bytecode index of the INVOKE this arm replaces — the pc a trap resumes at.
    pc: usize,
) -> Option<()> {
    use std::sync::atomic::Ordering::Relaxed;
    let elems_fid = crate::jit::ITER_ELEMS_FID.load(Relaxed) - 1;
    let pos_fid = crate::jit::ITER_POS_FID.load(Relaxed) - 1;

    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let read_blk = b.create_block();
    let bounds_blk = b.create_block();
    let load_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls_blk, &[], slow, &[]);

    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    bump(b, &NX_CLS);
    let cw_addr = b.ins().iconst(
        I64,
        (&crate::jit::ITER_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g2 = b.ins().band(cw_match, baked_nz);
    b.ins().brif(g2, read_blk, &[], slow, &[]);

    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    // A source-backed iterator needs the shared fail-fast contract. Refuse
    // before loading an element or advancing; bare iterators retain the inline path.
    let source_fid = crate::jit::ITER_SOURCE_FID.load(Relaxed) - 1;
    let source = native_getfield_ref(b, env, array_desc_cell, recv, source_fid);
    let bare = b.ins().icmp_imm(IntCC::Equal, source, 0);
    let bare_blk = b.create_block();
    b.ins().brif(bare, bare_blk, &[], slow, &[]);
    b.switch_to_block(bare_blk);
    b.seal_block(bare_blk);
    let elems = native_getfield_ref(b, env, array_desc_cell, recv, elems_fid);
    let ehi = b.ins().ushr_imm(elems, 32);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, ehi, ARRAY_TAG_HI as i64);
    b.ins().brif(is_arr, bounds_blk, &[], slow, &[]);

    // Bounds, both ends: an exhausted or odd iterator goes to the callback so the real one
    // raises exactly what it raised before.
    b.switch_to_block(bounds_blk);
    b.seal_block(bounds_blk);
    let pos = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, pos_fid);
    let (_data, len) = load_array_desc(b, array_desc_cell, elems);
    let ge0 = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, pos, 0);
    let lt = b.ins().icmp(IntCC::SignedLessThan, pos, len);
    let inb = b.ins().band(ge0, lt);
    b.ins().brif(inb, load_blk, &[], slow, &[]);

    b.switch_to_block(load_blk);
    b.seal_block(load_blk);
    let cell = crate::jit::emit::arrays::native_aaload(b, env, array_desc_cell, elems, pos, true);
    // Advance only here, where the read has already succeeded.
    let next_pos = b.ins().iadd_imm(pos, 1);
    let np64 = b.ins().sextend(I64, next_pos);
    crate::jit::emit::fields_prim_putfield(b, env, array_desc_cell, recv, np64, pos_fid);
    bump(b, &NX_FAST);
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![cell.into()];
    for &i in &ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);

    b.switch_to_block(slow);
    b.seal_block(slow);

    #[cfg(feature = "next-gen-gc")]
    if try_trap(b, env, vars, stack, &[recv_sv], locals_ptr, pc, cp, 2) {
        b.switch_to_block(merge);
        b.seal_block(merge);
        let mparams: Vec<Value> = b.block_params(merge).to_vec();
        for (k, &i) in ref_slots.iter().enumerate() {
            stack[i].v = mparams[1 + k];
        }
        stack.push(SV { v: mparams[0], t: JT::Ref, src: None, arena: false });
        return Some(());
    }

    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    stack.push(SV { v: mparams[0], t: JT::Ref, src: None, arena: false });
    Some(())
}

/// Reached the class compare on `CubePointRange.getDouble`.
pub(crate) static CB_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline: `i / parts` computed in machine code.
pub(crate) static CB_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn cb_cls() -> u64 {
    CB_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn cb_fast() -> u64 {
    CB_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// ⭐**THE CLASS WORD IS NOT AN EMIT-TIME PRECONDITION.** It can only be taken from a LIVE
/// receiver, and the emitted guard already loads it from the atomic on every call and refuses
/// while it is zero — so requiring it here only decides WHEN the arm may be emitted, and it
/// decided wrongly: a `getDouble` site that compiles before the first `CubePointRange` exists got
/// no cube shape, and the list arm's chained cube shape read `cg_fast = 0` for exactly this
/// reason while 3,696,640 `CubePointRange#getDouble` kept crossing.
///
/// The same trap, in the same file, that kept the `DoubleArrayList` arm at zero for months: a
/// bootstrap of the form "not ready ⇒ not emitted" cannot converge when the thing it waits for is
/// published by the very calls the arm exists to serve. The FIELD ID is the only thing that must
/// be baked, and `jit_run` now resolves it before compiling.
pub(crate) fn cube_ready() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    // The emitted probe is safe before any class exists: an empty or colliding immutable entry
    // takes the callback. Eligibility therefore cannot depend on load order or an earlier call.
    if crate::vm::fu_fastpath::cubecall_enabled() {
        return true;
    }
    crate::jit::CUBE_PARTS_FID.load(Relaxed) != 0
}

// ═══════════ THE ARENA VALUE-COMPUTER ARM (kill switch `RAVA_NO_CUBECALL=1`) ═══════════
//
// ⭐**THE BIRTH SIDE PUT 2,480,066 OBJECTS ON THE ARENA AND THEIR CALLS STILL LEFT MACHINE CODE.**
// `Vm::value_computer_verdict` (`vm/native_backing.rs`) admits a pure value-computer subclass —
// `CubePointRange`: one `int parts`, `getDouble(i) = i / parts`, `size() = parts + 1` — to the
// arena at birth: `rava_new` callbacks 2,695,047 → 213,133 on the bootstrap bench. It measured
// only +3%, because every `getDouble`/`size` on the new arena receiver still crossed: the two
// arms below guarded `hi == 1` (a HANDLE) and then compared `CUBE_CLASS_WORD`, a word that only
// a `Value::Ref` in `fu_fastpath`'s `CubeRange` arm ever published — so an arena receiver took
// the callback on every call, into an arm that decoded it, read `parts` by an owner-qualified
// id and computed the quotient BY HAND. Three name keys (`fk::CUBE_RANGE`, the `CPR` constant,
// `CUBE_CLASS_WORD`) and one formula, all for one class.
//
// **WHAT REPLACES IT, AND WHY IT IS NOT A FORMULA.** The arena edge of each arm probes the
// immutable STRUCTURAL accessor table (`jit/structural_accessor.rs`) keyed by the complete
// `(method selector, receiver class id)` pair: `emit_value_computer_arm` bakes the selector at
// compile time and reads the entry at runtime, so a caller compiled before the receiver's
// class was even instantiated starts hitting after the first declined callback publishes the
// exact bytecode and layout facts (`Vm::link_structural_accessor`). A positive entry names the
// callee's own recognized SHAPE — `field+1`, `i/parts`, the plain `aload_0; getfield f; ireturn`
// getter, or an `iconst_n; ireturn` literal — plus the slot; a missing, refused, or colliding
// entry keeps the original callback edge. No name is read anywhere on this path; receiver
// frequency and compile order never change the structural answer.
//
// **WHY THE GUARD CANNOT ADMIT AN UNSAFE SHAPE.** The entry is keyed by the complete
// `(class id, selector)` pair, and the reader verifies the whole key before the shape runs —
// so the body can never run on a receiver whose fields it was not resolved against. The
// recognizer (`vm/structural_accessor.rs`) admits only exact straight-line bytecode with an
// empty exception table, an int-family field resolved ON the defining class into the frozen
// layout, no `static`/`synchronized` flags; everything else links as `OP_REFUSED`, a permanent
// refusal. Every refusal keeps the callback edge, so a declined edge is exactly the callback
// edge it was.
//
// This arm consumes runtime table state, not a compilation-time target; no second compile is
// scheduled or needed. Safety guards remain required.

/// Arena receiver reached the value-computer class-id compare.
pub(crate) static VC_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Served: the receiver class's own accessor bytecode ran inline.
pub(crate) static VC_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Sites whose arena edge emitted an immutable structural lookup (compile-time count).
pub(crate) static VC_SITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Sites whose arena edge compiled with NO profiled arena callee — the edge stays the callback
/// until the site's caller is recompiled with a profile (compile-time count).
pub(crate) static VC_NOCALLEE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn vc_cls() -> u64 {
    VC_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn vc_fast() -> u64 {
    VC_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// `[CUBECALL]` — printed from the periodic profiler dump beside `[VALUECOMP]`
/// (`pc_profile/dump.rs`). Served vs callback for the arena value-computer accessor population.
pub(crate) fn cubecall_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let sites = VC_SITES.load(Relaxed);
    let nocallee = VC_NOCALLEE.load(Relaxed);
    let cls = vc_cls();
    let fast = vc_fast();
    let cb = crate::vm::fu_fastpath::vc_callback();
    if sites + nocallee + cls + fast + cb == 0 {
        return String::new();
    }
    let total = fast + cb;
    format!(
        "[CUBECALL] arena value-computer accessors: {sites} immutable lookup sites emitted, {nocallee} unsupported callsites | lookup reached {cls}, SERVED INLINE {fast} ({:.1}% of {total}) | DECLINED to the compiled accessor via fu_fastpath {cb}{}",
        if total == 0 { 0.0 } else { fast as f64 * 100.0 / total as f64 },
        if crate::vm::fu_fastpath::cubecall_enabled() { "" } else { " (RAVA_NO_CUBECALL: round off)" }
    )
}

fn cubecall_counting_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_PROFILE_PC").is_some()
            || std::env::var_os("RAVA_CUBECALL_STATS").is_some()
    })
}

/// The arena edge of the `getDouble`/`size` arms. Generated code probes immutable linkage by the
/// complete `(method selector, receiver class id)` key, so a caller compiled before the receiver
/// class was loaded starts hitting after the first ordinary callback publishes exact bytecode and
/// layout facts. A missing, refused, or colliding entry keeps the original callback edge.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
fn emit_value_computer_arm(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    _array_desc_cell: Value,
    _locals_ptr: Value,
    _vars: &[Variable],
    stack: &mut Vec<SV>,
    recv_sv: SV,
    args: &[SV],
    ref_slots: &[usize],
    ret: JT,
    merge: Block,
    slow: Block,
) -> crate::jit::emit::guarded_inline::Guarded {
    use crate::jit::emit::guarded_inline::Guarded;
    use crate::jit::structural_accessor as sa;
    use std::sync::atomic::Ordering::Relaxed;

    if env.class_ptr == 0 {
        VC_NOCALLEE.fetch_add(1, Relaxed);
        return Guarded::Declined;
    }
    let caller = unsafe { &*(env.class_ptr as *const crate::class_loader::LoadedClass) };
    let Some(member) = caller.file.pool.member_ref(cp) else {
        VC_NOCALLEE.fetch_add(1, Relaxed);
        return Guarded::Declined;
    };
    // The op the reader accepts is a SET, not one value, for the `()I` shape: the
    // shipped `field+1` arm plus — behind this round's own kill switch — the plain
    // getter and literal bodies the recognizer also publishes. An entry with any
    // other op (or a refused/colliding entry) keeps the callback edge, exactly as
    // before; with the switch off the set is the historical singleton and the
    // emitted code is byte-identical.
    let allowed: &[u8] = match (member.descriptor, ret, args) {
        ("()I", JT::Int, []) => {
            if crate::jit::emit::collections::mg_fast_enabled() {
                &[
                    sa::OP_INT_FIELD_ADD_ONE,
                    sa::OP_INT_FIELD,
                    sa::OP_INT_CONST,
                ]
            } else {
                &[sa::OP_INT_FIELD_ADD_ONE]
            }
        }
        ("(I)D", JT::Double, [arg]) if arg.t == JT::Int => &[sa::OP_INT_ARG_DIV_FIELD],
        _ => return Guarded::Declined,
    };
    let Some(selector) = sa::selector_id(member.name, member.descriptor) else {
        return Guarded::Declined;
    };

    VC_SITES.fetch_add(1, Relaxed);
    let recv = recv_sv.v;
    let probe0 = b.create_block();
    let found = b.create_block();
    let calc = b.create_block();
    b.append_block_param(found, I64);

    let is_a = emit_is_arena(b, recv);
    b.ins().brif(is_a, probe0, &[], slow, &[]);

    b.switch_to_block(probe0);
    b.seal_block(probe0);
    if cubecall_counting_enabled() {
        bump(b, &VC_CLS);
    }
    let cid32 = b
        .ins()
        .load(I32, MemFlags::trusted(), recv, Offset32::new(0));
    let cid = b.ins().uextend(I64, cid32);
    let key_hi = b.ins().ishl_imm(cid, 16);
    let key = b.ins().bor_imm(key_hi, i64::from(selector));
    let shr17 = b.ins().ushr_imm(key, 17);
    let mix17 = b.ins().bxor(key, shr17);
    let shr31 = b.ins().ushr_imm(key, 31);
    let mixed = b.ins().bxor(mix17, shr31);
    let bucket = b.ins().band_imm(mixed, sa::BUCKET_MASK as i64);
    let byte = b.ins().ishl_imm(bucket, 3);
    let base = b.ins().iconst(I64, sa::table_base());
    let addr = b.ins().iadd(base, byte);

    let entry0 = b
        .ins()
        .load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let entry0_key = b.ins().ushr_imm(entry0, 16);
    let hit0 = b.ins().icmp(IntCC::Equal, entry0_key, key);
    b.ins().brif(hit0, found, &[entry0.into()], slow, &[]);

    b.switch_to_block(found);
    b.seal_block(found);
    let entry = b.block_params(found)[0];
    let op = b.ins().ushr_imm(entry, 12);
    let op = b.ins().band_imm(op, 0xF);
    let op_ok = {
        let mut acc = b.ins().icmp_imm(IntCC::Equal, op, i64::from(allowed[0]));
        for &more in &allowed[1..] {
            let m = b.ins().icmp_imm(IntCC::Equal, op, i64::from(more));
            acc = b.ins().bor(acc, m);
        }
        acc
    };
    let slot_p1 = b.ins().band_imm(entry, 0xFFF);
    let ready = b.ins().icmp_imm(IntCC::NotEqual, slot_p1, 0);
    let valid = b.ins().band(op_ok, ready);
    b.ins().brif(valid, calc, &[], slow, &[]);

    b.switch_to_block(calc);
    b.seal_block(calc);
    // The answer dispatches on the entry's op. The shipped `field+1` and the division
    // shapes are compile-time-known per site (a singleton allowed set); the `()I` round
    // adds the PLAIN GETTER (one cell load) and the LITERAL (its value rides the slot
    // word biased by 1 — it must never reach the slot arithmetic, which would address
    // far outside the object; a branch, not a select). With the round off the set is the
    // historical singleton and the emission is byte-identical.
    if member.descriptor == "()I" && crate::jit::emit::collections::mg_fast_enabled() {
        let is_const = b
            .ins()
            .icmp_imm(IntCC::Equal, op, i64::from(sa::OP_INT_CONST));
        let lit_blk = b.create_block();
        let cell_blk = b.create_block();
        b.ins().brif(is_const, lit_blk, &[], cell_blk, &[]);

        b.switch_to_block(lit_blk);
        b.seal_block(lit_blk);
        let lit = b.ins().iadd_imm(slot_p1, -1);
        bump(b, &crate::jit::emit::collections::MG_FAST);
        let mut lit_args: Vec<cranelift::codegen::ir::BlockArg> = vec![lit.into()];
        for &i in ref_slots {
            lit_args.push(stack[i].v.into());
        }
        b.ins().jump(merge, &lit_args);

        b.switch_to_block(cell_blk);
        b.seal_block(cell_blk);
        let cell_addr = emit_arena_cell_addr(b, recv, slot_p1);
        let field_cell = b
            .ins()
            .load(I64, MemFlags::trusted(), cell_addr, Offset32::new(0));
        let field = cell_to_sv(b, field_cell, JT::Int).v;
        let is_plain = b
            .ins()
            .icmp_imm(IntCC::Equal, op, i64::from(sa::OP_INT_FIELD));
        let plain_blk = b.create_block();
        let add_blk = b.create_block();
        b.ins().brif(is_plain, plain_blk, &[], add_blk, &[]);

        // The plain getter: ONE cell load, answered. `MG_FAST` counts this round's
        // served population unconditionally — the ABBA attribution pair with `MG_CLS`.
        b.switch_to_block(plain_blk);
        b.seal_block(plain_blk);
        bump(b, &crate::jit::emit::collections::MG_FAST);
        let plain = b.ins().sextend(I64, field);
        let mut plain_args: Vec<cranelift::codegen::ir::BlockArg> = vec![plain.into()];
        for &i in ref_slots {
            plain_args.push(stack[i].v.into());
        }
        b.ins().jump(merge, &plain_args);

        // The shipped `field+1` shape, unchanged.
        b.switch_to_block(add_blk);
        b.seal_block(add_blk);
        let n = b.ins().iadd_imm(field, 1);
        let result = b.ins().sextend(I64, n);
        if cubecall_counting_enabled() {
            bump(b, &VC_FAST);
        }
        let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![result.into()];
        for &i in ref_slots {
            fast_args.push(stack[i].v.into());
        }
        b.ins().jump(merge, &fast_args);
        return Guarded::Emitted;
    }
    let cell_addr = emit_arena_cell_addr(b, recv, slot_p1);
    let field_cell = b
        .ins()
        .load(I64, MemFlags::trusted(), cell_addr, Offset32::new(0));
    let field = cell_to_sv(b, field_cell, JT::Int).v;
    let result = if member.descriptor == "()I" {
        let n = b.ins().iadd_imm(field, 1);
        b.ins().sextend(I64, n)
    } else {
        let numerator = b.ins().fcvt_from_sint(F64, args[0].v);
        let denominator = b.ins().fcvt_from_sint(F64, field);
        let quotient = b.ins().fdiv(numerator, denominator);
        b.ins().bitcast(I64, MemFlags::new(), quotient)
    };
    if cubecall_counting_enabled() {
        bump(b, &VC_FAST);
    }
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![result.into()];
    for &i in ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);
    Guarded::Emitted
}

/// **`CubePointRange.getDouble(int)` — 6,504,704 calls a boot**, the second largest
/// (class, method) pair in the engine. Minecraft's own class, and the value is not stored at all:
/// the native computes `f64::from(i) / f64::from(parts)` from one guest int field. Four
/// instructions of bytecode, and until now a ~65 ns host call each time.
///
/// Out of range (`i < 0 || i > parts`) takes the callback, so the real method throws exactly what
/// it threw before — the bound is `<= parts`, not `< parts`, because the range has `parts + 1`
/// points.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_cube_getdouble_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
    // The bytecode index of the INVOKE this arm replaces — the pc a trap resumes at.
    pc: usize,
) -> Option<()> {
    use std::sync::atomic::Ordering::Relaxed;
    // `RAVA_NO_CUBECALL=1` → the historic arm verbatim: the handle cube shape below reads `parts`
    // by the name-resolved id and the arena receiver goes straight to the callback. With the
    // round on, that id is never published and the shape is not emitted; the arena edge splices
    // the profiled receiver class's own accessor instead (`emit_value_computer_arm`).
    let cc = crate::vm::fu_fastpath::cubecall_enabled();
    let parts_fid = if cc { 0 } else { crate::jit::CUBE_PARTS_FID.load(Relaxed) - 1 };

    let idx_sv = stack.pop()?;
    let recv_sv = stack.pop()?;
    let idx = idx_sv.v;
    let recv = recv_sv.v;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    // The handle cube shape's two blocks exist only on the cold arm — an unfilled Cranelift
    // block is a panic at finalise, so they are not created when they are not emitted.
    let (bounds_blk, calc_blk) =
        if cc { (None, None) } else { (Some(b.create_block()), Some(b.create_block())) };
    // ⭐**THE TWO getDouble ARMS WERE REJECTING EACH OTHER'S RECEIVERS.** Only one arm is emitted
    // per site, and a `getDouble` site sees BOTH shapes, so whichever arm won the site sent the
    // other's receiver to the callback. The counters are almost a mirror image:
    //
    //   cb_cls 6,970,301 -> cb_fast 2,785,716   miss 4,184,585  ~= DoubleArrayList#getDouble left
    //   dg_cls 9,887,620 -> dg_fast 6,168,411   miss 3,719,209  ~= CubePointRange#getDouble
    //
    // So the cube word's MISMATCH edge tries the list shape before giving up, exactly as the
    // `size` arm now does. Unlike `size` — which measured neutral because a `size` crossing is
    // cheap — `getDouble` is the operation the -2.83% came from.
    let dal_blk = b.create_block();
    let dal_bounds = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    // THE ARENA EDGE. Historic: straight to the callback. Under the round: the value-computer arm
    // (a filled block is created only when it is emitted).
    let arena_blk = if cc { b.create_block() } else { slow };
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls_blk, &[], arena_blk, &[]);

    #[cfg(feature = "next-gen-gc")]
    if cc {
        use crate::jit::emit::guarded_inline::Guarded;
        b.switch_to_block(arena_blk);
        b.seal_block(arena_blk);
        match emit_value_computer_arm(
            b, env, cp, array_desc_cell, locals_ptr, vars, stack, recv_sv, &[idx_sv], &ref_slots,
            JT::Double, merge, slow,
        ) {
            Guarded::Emitted => {}
            // Nothing emitted: this edge is the callback it was.
            Guarded::Declined => {
                b.ins().jump(slow, &[]);
            }
            Guarded::Aborted => return None,
        }
    }

    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    bump(b, &CB_CLS);
    let cw = if let Some(bounds_blk) = bounds_blk {
        // COLD ARM: the name-published cube word, then the `i / parts` handle shape.
        let cw_addr = b.ins().iconst(
            I64,
            (&crate::jit::CUBE_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
        );
        let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
        let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
        let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
        let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
        let g2 = b.ins().band(cw_match, baked_nz);
        b.ins().brif(g2, bounds_blk, &[], dal_blk, &[]);
        cw
    } else {
        // Under the round no handle receiver of a value-computer exists (they are born on the
        // arena) and the cube word is never published, so the compare could only ever fail: the
        // handle receiver goes straight to the second shape with its class word in hand.
        let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
        b.ins().jump(dal_blk, &[]);
        cw
    };

    // SECOND SHAPE: `DoubleArrayList.getDouble(i)` — the receiver this arm used to hand back to
    // the callback. `a` by published id, bound by `min(size, phys)`, same as the dedicated arm.
    b.switch_to_block(dal_blk);
    b.seal_block(dal_blk);
    if crate::jit::dalist_ready() && crate::jit::dalist_get_inline_enabled() {
        let dw_addr = b.ins().iconst(
            I64,
            (&crate::jit::DALIST_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
        );
        let dbaked = b.ins().load(I64, MemFlags::trusted(), dw_addr, Offset32::new(0));
        let dmatch = b.ins().icmp(IntCC::Equal, cw, dbaked);
        let dnz = b.ins().icmp_imm(IntCC::NotEqual, dbaked, 0);
        let ge0d = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, idx, 0);
        let dg0 = b.ins().band(dmatch, dnz);
        let dg = b.ins().band(dg0, ge0d);
        b.ins().brif(dg, dal_bounds, &[], slow, &[]);

        b.switch_to_block(dal_bounds);
        b.seal_block(dal_bounds);
        let a_fid = crate::jit::DALIST_A_FID.load(std::sync::atomic::Ordering::Relaxed) - 1;
        let size_fid = crate::jit::DALIST_SIZE_FID.load(std::sync::atomic::Ordering::Relaxed) - 1;
        let arr = native_getfield_ref(b, env, array_desc_cell, recv, a_fid);
        let ahi = b.ins().ushr_imm(arr, 32);
        let is_arr = b.ins().icmp_imm(IntCC::Equal, ahi, ARRAY_TAG_HI as i64);
        let sz = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, size_fid);
        let (_dd, phys) = load_array_desc(b, array_desc_cell, arr);
        let in_size = b.ins().icmp(IntCC::SignedLessThan, idx, sz);
        let in_phys = b.ins().icmp(IntCC::UnsignedLessThan, idx, phys);
        let inb0 = b.ins().band(in_size, in_phys);
        let inb = b.ins().band(inb0, is_arr);
        let load_blk = b.create_block();
        b.ins().brif(inb, load_blk, &[], slow, &[]);
        b.switch_to_block(load_blk);
        b.seal_block(load_blk);
        let val = crate::jit::emit::arrays::native_primitive_load(
            b, env, array_desc_cell, arr, idx, F64, 3, false, true, cp as i64,
        );
        let dbits = b.ins().bitcast(I64, MemFlags::new(), val);
        bump(b, &DB_FAST);
        let mut dal_args: Vec<cranelift::codegen::ir::BlockArg> = vec![dbits.into()];
        for &i in &ref_slots {
            dal_args.push(stack[i].v.into());
        }
        b.ins().jump(merge, &dal_args);
    } else {
        b.ins().jump(slow, &[]);
        b.switch_to_block(dal_bounds);
        b.seal_block(dal_bounds);
        b.ins().jump(slow, &[]);
    }

    // COLD ARM: the hand-written handle cube shape, `RAVA_NO_CUBECALL=1` only.
    // `0 <= i <= parts` — the range holds `parts + 1` points, so the upper bound is inclusive.
    if let (Some(bounds_blk), Some(calc_blk)) = (bounds_blk, calc_blk) {
        b.switch_to_block(bounds_blk);
        b.seal_block(bounds_blk);
        let parts = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, parts_fid);
        let ge0 = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, idx, 0);
        let le = b.ins().icmp(IntCC::SignedLessThanOrEqual, idx, parts);
        let inb = b.ins().band(ge0, le);
        b.ins().brif(inb, calc_blk, &[], slow, &[]);

        b.switch_to_block(calc_blk);
        b.seal_block(calc_blk);
        let fi = b.ins().fcvt_from_sint(F64, idx);
        let fp = b.ins().fcvt_from_sint(F64, parts);
        let q = b.ins().fdiv(fi, fp);
        let bits = b.ins().bitcast(I64, MemFlags::new(), q);
        bump(b, &CB_FAST);
        let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![bits.into()];
        for &i in &ref_slots {
            fast_args.push(stack[i].v.into());
        }
        b.ins().jump(merge, &fast_args);
    }

    b.switch_to_block(slow);
    b.seal_block(slow);

    // Two slots consumed here, and the order matters: the invoke expects `[.., recv, idx]`.
    #[cfg(feature = "next-gen-gc")]
    if try_trap(b, env, vars, stack, &[recv_sv, idx_sv], locals_ptr, pc, cp, 4) {
        b.switch_to_block(merge);
        b.seal_block(merge);
        let mparams: Vec<Value> = b.block_params(merge).to_vec();
        for (k, &i) in ref_slots.iter().enumerate() {
            stack[i].v = mparams[1 + k];
        }
        let sv = cell_to_sv(b, mparams[0], JT::Double);
        stack.push(sv);
        return Some(());
    }

    let recv_cell = to_cell(b, recv_sv);
    let idx_cell = to_cell(b, idx_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    b.ins().stack_store(idx_cell, env.args_slot, 8);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 2);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Double);
    stack.push(sv);
    Some(())
}

/// Answered inline: `parts + 1`.
pub(crate) static CS_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline by the SECOND shape on the cube `getDouble` site: `DoubleArrayList.getDouble`.
pub(crate) static DB_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn db_fast() -> u64 {
    DB_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// Answered inline by the SECOND shape on the same site: `DoubleArrayList.size()`.
pub(crate) static DS_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn ds_fast() -> u64 {
    DS_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn cs_fast() -> u64 {
    CS_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// **`CubePointRange.size()` — 1,554,688 calls a boot.** The cheapest possible arm: the guard
/// word and field id are already published for `getDouble`, and the native's answer is
/// `parts + 1` (the range holds one more point than it has parts). No bounds test, no array.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_cube_size_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
    // The bytecode index of the INVOKE this arm replaces — the pc a trap resumes at.
    pc: usize,
) -> Option<()> {
    use std::sync::atomic::Ordering::Relaxed;
    // `RAVA_NO_CUBECALL=1` → the historic arm verbatim (see `emit_cube_getdouble_inline`).
    let cc = crate::vm::fu_fastpath::cubecall_enabled();
    let parts_fid = if cc { 0 } else { crate::jit::CUBE_PARTS_FID.load(Relaxed) - 1 };

    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    // (`calc_blk`, the handle cube shape's block, is created inside the cold arm below — an
    // unfilled Cranelift block is a panic at finalise, so it must not exist when not emitted.)
    // ⭐**A SECOND SHAPE ON THE SAME SITE.** `size()` arrives on more than one class and only ONE
    // arm can be emitted per site, so a `DoubleArrayList` receiver used to fall straight to the
    // callback here — 2,668,288 of them on a datapack run, the largest single (class, method)
    // pair left after `getDouble` was fixed. Rather than steal these sites from the cube arm,
    // the cube word's MISMATCH edge now tries the list word before giving up. Each shape keeps
    // its own guard; a receiver that is neither takes exactly the callback it took before.
    let dal_blk = b.create_block();
    let dal_calc = b.create_block();
    // ⭐⭐**A FOURTH SHAPE, AND IT IS THE ORIGINAL ONE — `java/util/ArrayList`, 4,439,370 calls a
    // boot, the largest single operation in the whole `fu_fastpath` census.**
    //
    // ⛔The chain on this site grew cube → +DoubleArrayList → +arena trivial getter, and the shape
    // that started the whole `size()` line was never added to it: it lives in
    // `emit_arraylist_size_inline`, which `invoke_virtual` and `invoke_dynamic` both try AFTER
    // this arm. Only one arm is emitted per site, so once `cube_ready()` holds — the moment
    // `CubePointRange` loads — this arm claims every `size()I` site in the program and the
    // `ArrayList` shape becomes unreachable at all of them. A permanent emit-time refusal whose
    // stated justification ("the size arm is opt-in and unproven") stopped holding when the arm
    // ahead of it started taking the sites; the population it refuses is the biggest one here.
    //
    // The shape itself is shared with `emit_arraylist_size_inline` verbatim, so there is one
    // implementation of "an `ArrayList`'s size is its backing array's length" and one place where
    // the `any_override()` correctness condition on that claim is checked.
    let alist_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }
    let alist_ok = crate::jit::emit::collections::alist_size_shape_ok(env);
    let pass: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();

    // ⭐**A THIRD SHAPE, AND THE FIRST ARENA ONE — on the edge the other two cannot reach.**
    //
    // Both shapes above guard on a heap-Ref class WORD, so their very first test (`hi == 1`)
    // sends every ARENA receiver straight to the callback. Three of the largest (class, method)
    // pairs left on a datapack run are exactly that: `IndirectMerger#size` 850,176,
    // `Reference2ObjectArrayMap#size` 461,824, `DiscreteCubeMerger#size` 234,752 — all of them
    // `aload_0; getfield f; ireturn`, all of them paying a crossing to read one cell.
    //
    // The pair `(class id, slot)` answers come from the STRUCTURAL accessor table — immutable
    // `(class id, selector)` linkage facts published by the callback the shape replaces — no
    // name is matched, the callee's bytecode is what qualifies it, and the class-id compare
    // below is what makes it correct — any other receiver takes the callback.
    let arena_blk = b.create_block();
    let mg_calc = b.create_block();
    // ⭐**A FIFTH SHAPE, ON THE ARENA EDGE.** `size()` on an arena value-computer is
    // `aload_0 getfield parts iconst_1 iadd ireturn`; the plain getters and literals of the
    // chained-shape population (IndirectMerger 850,176, the DFU templates' `Const` 87,001,
    // …) are `aload_0; getfield f; ireturn` and `iconst_n; ireturn`. Under the round every
    // one of those lands in `vc_blk`, where the emitted probe reads the STRUCTURAL accessor
    // table (`emit_value_computer_arm`): guard on the complete `(class id, selector)` key,
    // one arena cell load (or the entry's literal), callback on any miss. `vc_blk` itself
    // falls to `slow`, so nothing that reached the callback fails to reach it. On the cold
    // arm `vc_blk` IS `slow`, verbatim.
    let vc_blk = if cc { b.create_block() } else { slow };
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls_blk, &[], arena_blk, &[]);

    b.switch_to_block(arena_blk);
    b.seal_block(arena_blk);
    bump(b, &crate::jit::emit::collections::MG_CLS);
    // The retired `trivial_getter` probe lived here — a SITE-keyed table retired by 47c7c73a
    // ("first-writer receiver selection violates no-adapt admission"), its `enabled()` a
    // `const fn false`, so the block folded to this jump: the arena edge goes straight to
    // the value-computer shape below, and `mg_calc` (created so every block is filled at
    // finalise) goes straight to the callback. The chained-shape population is now served by
    // the STRUCTURAL accessor table — immutable `(class id, selector)` linkage facts — through
    // that arm; see `emit_value_computer_arm`.
    b.ins().jump(vc_blk, &[]);
    b.switch_to_block(mg_calc);
    b.seal_block(mg_calc);
    b.ins().jump(slow, &[]);

    // THE VALUE-COMPUTER SHAPE on the trivial getter's failing edge (round on only; on the cold
    // arm `vc_blk == slow` and nothing is emitted here).
    #[cfg(feature = "next-gen-gc")]
    if cc {
        use crate::jit::emit::guarded_inline::Guarded;
        b.switch_to_block(vc_blk);
        b.seal_block(vc_blk);
        match emit_value_computer_arm(
            b, env, cp, array_desc_cell, locals_ptr, vars, stack, recv_sv, &[], &ref_slots, JT::Int,
            merge, slow,
        ) {
            Guarded::Emitted => {}
            Guarded::Declined => {
                b.ins().jump(slow, &[]);
            }
            Guarded::Aborted => return None,
        }
    }

    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    let cw = if cc {
        // Under the round the cube word is never published and no value-computer is a handle:
        // the handle receiver goes straight to the second shape with its class word in hand.
        let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
        b.ins().jump(dal_blk, &[]);
        cw
    } else {
        // COLD ARM: the name-published cube word, then `parts + 1` from the name-resolved id.
        let calc_blk = b.create_block();
        let cw_addr = b.ins().iconst(
            I64,
            (&crate::jit::CUBE_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
        );
        let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
        let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
        let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
        let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
        let g2 = b.ins().band(cw_match, baked_nz);
        b.ins().brif(g2, calc_blk, &[], dal_blk, &[]);

        b.switch_to_block(calc_blk);
        b.seal_block(calc_blk);
        let parts = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, parts_fid);
        let n = b.ins().iadd_imm(parts, 1);
        let n64 = b.ins().sextend(I64, n);
        bump(b, &CS_FAST);
        let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![n64.into()];
        for &i in &ref_slots {
            fast_args.push(stack[i].v.into());
        }
        b.ins().jump(merge, &fast_args);
        cw
    };

    // SECOND SHAPE: `DoubleArrayList.size()`. Same class-word guard discipline, reading the ids
    // `jit_run` resolves before compiling. The answer must be `min(size, phys)` — the list
    // over-allocates, so the `size` field is the logical length, and both are checked for the
    // same reason `fu_double_view` checks both.
    b.switch_to_block(dal_blk);
    b.seal_block(dal_blk);
    // `RAVA_NO_DALIST_SIZE=1` drops the second shape, so its share is measured rather than
    // assumed — the same discipline the `getDouble` arm was held to.
    let dal_ready = crate::jit::dalist_ready() && {
        static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        !*OFF.get_or_init(|| std::env::var_os("RAVA_NO_DALIST_SIZE").is_some())
    };
    // A receiver that is not a `DoubleArrayList` either now falls to the ArrayList shape rather
    // than to the callback; `alist_blk` itself falls to `slow`, so nothing that used to reach the
    // callback fails to reach it.
    let dal_miss = if alist_ok { alist_blk } else { slow };
    if dal_ready {
        let dw_addr = b.ins().iconst(
            I64,
            (&crate::jit::DALIST_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
        );
        let dbaked = b.ins().load(I64, MemFlags::trusted(), dw_addr, Offset32::new(0));
        let dmatch = b.ins().icmp(IntCC::Equal, cw, dbaked);
        let dnz = b.ins().icmp_imm(IntCC::NotEqual, dbaked, 0);
        let dg = b.ins().band(dmatch, dnz);
        b.ins().brif(dg, dal_calc, &[], dal_miss, &[]);

        b.switch_to_block(dal_calc);
        b.seal_block(dal_calc);
        let a_fid = crate::jit::DALIST_A_FID.load(std::sync::atomic::Ordering::Relaxed) - 1;
        let size_fid = crate::jit::DALIST_SIZE_FID.load(std::sync::atomic::Ordering::Relaxed) - 1;
        let arr = native_getfield_ref(b, env, array_desc_cell, recv, a_fid);
        let ahi = b.ins().ushr_imm(arr, 32);
        let is_arr = b.ins().icmp_imm(IntCC::Equal, ahi, ARRAY_TAG_HI as i64);
        // A non-array backing (never initialised) takes the callback, as it already did.
        let sz = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, size_fid);
        let (_d, phys) = load_array_desc(b, array_desc_cell, arr);
        let smaller = b.ins().icmp(IntCC::SignedLessThan, sz, phys);
        let logical = b.ins().select(smaller, sz, phys);
        let l64 = b.ins().sextend(I64, logical);
        bump(b, &DS_FAST);
        let mut dal_args: Vec<cranelift::codegen::ir::BlockArg> = vec![l64.into()];
        for &i in &ref_slots {
            dal_args.push(stack[i].v.into());
        }
        // The array test decides between the fast answer and the callback; both paths below
        // already exist, so this is a branch, not a new exit.
        let ok_blk = b.create_block();
        b.ins().brif(is_arr, ok_blk, &[], slow, &[]);
        b.switch_to_block(ok_blk);
        b.seal_block(ok_blk);
        b.ins().jump(merge, &dal_args);
    } else {
        b.ins().jump(dal_miss, &[]);
        b.switch_to_block(dal_calc);
        b.seal_block(dal_calc);
        b.ins().jump(slow, &[]);
    }

    // FOURTH SHAPE: `java/util/ArrayList.size()`. The block is always FILLED — an unfilled
    // Cranelift block is a panic at finalise, not dead code — so when the shape is switched off
    // or cannot be served it is one unconditional jump onto the same callback edge.
    b.switch_to_block(alist_blk);
    b.seal_block(alist_blk);
    if alist_ok {
        crate::jit::emit::collections::emit_alist_size_shape(
            b, env, array_desc_cell, recv, cw, merge, &pass, slow,
        );
    } else {
        b.ins().jump(slow, &[]);
    }

    b.switch_to_block(slow);
    b.seal_block(slow);

    #[cfg(feature = "next-gen-gc")]
    if try_trap(b, env, vars, stack, &[recv_sv], locals_ptr, pc, cp, 8) {
        b.switch_to_block(merge);
        b.seal_block(merge);
        let mparams: Vec<Value> = b.block_params(merge).to_vec();
        for (k, &i) in ref_slots.iter().enumerate() {
            stack[i].v = mparams[1 + k];
        }
        let sv = cell_to_sv(b, mparams[0], JT::Int);
        stack.push(sv);
        return Some(());
    }

    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Int);
    stack.push(sv);
    Some(())
}
