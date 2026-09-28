//! Reduction fork: the worker body for a counter-dependent fold and its closed-form twin.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// LOOP-FOLDING codegen (next-gen-gc). Emit the matched method as its closed form
/// `acc_init + i2l(S) * max(0, LIM - C0)` (wrapping i64) — NO loop header, no
/// back-edge, no per-iteration work. `S` is the body's invariant int expression,
/// emitted ONCE via the normal `emit_op` (its getstatics use the static-LICM cells,
/// populated here at entry like `build_body`). The trip count `max(0, LIM - C0)` is
/// computed from the runtime limit. Returns the i64 result bits (= the long value).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_folded_body(
    b: &mut FunctionBuilder,
    bc: &[u8],
    max_locals: usize,
    pool: &ConstantPool,
    env: &Env,
    plan: &LoopFold,
) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let locals_ptr = b.block_params(entry)[0];
    let array_desc_cell = b.block_params(entry)[1];
    let vars: Vec<Variable> = (0..max_locals)
        .map(|i| {
            let var = b.declare_var(I64);
            let loaded = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((i * 8) as i32));
            b.def_var(var, loaded);
            var
        })
        .collect();
    b.seal_block(entry);

    // Static-LICM populate (the invariant getstatics read readonly cells, as in
    // build_body). Harmless when there are no statics.
    if env.static_licm {
        let cls = b.ins().iconst(I64, env.class_ptr);
        let isref0 = b.ins().iconst(I64, 0);
        spill_for_gc(b, &vars, &[], locals_ptr);
        for (&cp, &(slot, jt)) in env.statics {
            if jt == JT::Ref {
                continue; // ref cells stay on the gen-guarded diamond (no readonly arm)
            }
            let cpv = b.ins().iconst(I64, cp as i64);
            let slotv = b.ins().iconst(I64, slot as i64);
            b.ins().call(env.getstatic_cached, &[cls, cpv, isref0, slotv]);
        }
        reload_locals_after_gc(b, &vars, locals_ptr);
    }

    // Emit the invariant expression ONCE → S (a single int on the stack). The range
    // is straight-line (only getstatic/const/int-arith — no branches), so empty
    // block maps suffice.
    let blocks = std::collections::HashMap::new();
    let mut block_shapes = std::collections::HashMap::new();
    let mut cur_open = true;
    let mut stack: Vec<SV> = Vec::new();
    let mut pc = plan.inv_range.0;
    while pc < plan.inv_range.1 {
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        // The folded body only emits the loop's INVARIANT expression (getstatic /
        // const / int-arith — no field access), so it needs neither a COW decision
        // nor an arena proof; `PerSite` + the empty table keep the self-contained
        // per-site fallback for anything that ever does.
        emit_op(
            b, &vars, &mut stack, bc, pc, op, &blocks, &mut block_shapes, &mut cur_open, pool,
            env, array_desc_cell, locals_ptr, CowArm::PerSite, &[], None, None, None,
        )?;
        pc += len;
    }
    let s = stack.pop()?;
    if s.t != JT::Int || !stack.is_empty() {
        return None; // the invariant must reduce to exactly one int
    }
    let s64 = b.ins().sextend(I64, s.v); // i2l(S)

    // trip = max(0, LIM - C0) in i64 (LIM is a sign-extended int local or a const).
    let lim = match plan.limit_local {
        Some(l) => b.use_var(*vars.get(l)?),
        None => b.ins().iconst(I64, plan.lim_const as i64),
    };
    let c0 = b.ins().iconst(I64, plan.c0 as i64);
    let diff = b.ins().isub(lim, c0);
    let zero = b.ins().iconst(I64, 0);
    let neg = b.ins().icmp(IntCC::SignedLessThan, diff, zero);
    let trip = b.ins().select(neg, zero, diff);

    // acc_final = acc_init + i2l(S) * trip  (wrapping i64 — bit-exact with the loop).
    let prod = b.ins().imul(s64, trip);
    let init = b.ins().iconst(I64, plan.acc_init);
    let acc = b.ins().iadd(init, prod);
    b.ins().return_(&[acc]);
    Some(())
}

// ═══════════════════ Kit 3: data-parallel loop fork codegen ═══════════════════

/// Compile the loop's body into a SECOND module function with the `ReduceBody` ABI
/// (`extern "C" fn(lo, hi, *mut ok) -> partial`): seed `acc = op.identity()`, fold
/// `acc = op(acc, bodyexpr(c0 + k))` for `k in [lo, hi)`, return `acc`. Pure
/// arithmetic → `*ok` stays 1 (the abort path is for future alloc bodies). Returns
/// the finalized machine-code address, or `None` if the body isn't codegen-able.
/// Restricted to step-1, long-accumulator loops (the safe first slice).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_reduce_body_fn(module: &mut JITModule, dp: &DataParallelLoop, bc: &[u8], sroa: &SroaCtx) -> Option<i64> {
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I64)); // lo
    sig.params.push(AbiParam::new(I64)); // hi
    sig.params.push(AbiParam::new(I64)); // *ok (i32)
    sig.params.push(AbiParam::new(I64)); // env (captured params), null if none
    sig.returns.push(AbiParam::new(I64)); // partial
    ctx.func.signature = sig.clone();
    let acc_ty = if dp.acc_is_long { I64 } else { I32 };
    let mut fbctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let lo = b.block_params(entry)[0];
        let hi = b.block_params(entry)[1];
        let okp = b.block_params(entry)[2];
        let env_ptr = b.block_params(entry)[3];
        let one = b.ins().iconst(I32, 1);
        b.ins().store(MemFlags::trusted(), one, okp, Offset32::new(0));

        let lh = b.create_block();
        b.append_block_param(lh, I64); // k
        b.append_block_param(lh, acc_ty); // acc (i32 or i64)
        let body_blk = b.create_block();
        let done = b.create_block();
        b.append_block_param(done, acc_ty);
        let id = b.ins().iconst(acc_ty, dp.op.identity());
        b.ins().jump(lh, &[lo.into(), id.into()]);

        b.switch_to_block(lh);
        let k = b.block_params(lh)[0];
        let acc = b.block_params(lh)[1];
        let go = b.ins().icmp(IntCC::SignedLessThan, k, hi);
        b.ins().brif(go, body_blk, &[], done, &[acc.into()]);

        b.switch_to_block(body_blk);
        b.seal_block(body_blk);
        // i = c0 + k (step == 1, enforced by the caller).
        let kf = b.ins().ireduce(I32, k);
        let c0v = b.ins().iconst(I32, dp.c0 as i64);
        let i_val = b.ins().iadd(c0v, kf);
        // Captured params read from env[slot*8] (i64 cell; ireduce for int).
        let captures = &dp.captures;
        let mut read_local = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
            let slot = captures.iter().position(|(l, _)| *l == local)?;
            let raw = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new((slot * 8) as i32));
            Some(if is_long { raw } else { b.ins().ireduce(I32, raw) })
        };
        // arr[i] in the worker: the element base sits at env[(n_scalar + arr_slot)*8]
        // (resolved under the GIL by the fork). DIRECT load `base[i*width]` — the fork's
        // `limit <= length` gate guarantees i < length, so no bounds check (and no GC
        // can move the buffer: the body allocates nothing).
        let n_scalar = captures.len();
        let arrcaps = &dp.array_captures;
        let mut read_array_elem = |b: &mut FunctionBuilder, local: usize, idx: Value, is_long: bool| -> Option<Value> {
            let slot = arrcaps.iter().position(|(l, _)| *l == local)?;
            let base = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new(((n_scalar + slot) * 8) as i32));
            let idx64 = b.ins().sextend(I64, idx);
            let width = if is_long { 8 } else { 4 };
            let off = b.ins().imul_imm(idx64, width);
            let addr = b.ins().iadd(base, off);
            Some(b.ins().load(if is_long { I64 } else { I32 }, MemFlags::trusted(), addr, Offset32::new(0)))
        };
        let (bv, is_long) = emit_body_expr(&mut b, bc, dp.body_range, i_val, dp.iv_local, &dp.scratch, &[dp.acc_load_pc], &[], &[], sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
        // Coerce the body value to the accumulator width, then fold.
        let bval = if dp.acc_is_long {
            if is_long { bv } else { b.ins().sextend(I64, bv) }
        } else if is_long {
            b.ins().ireduce(I32, bv)
        } else {
            bv
        };
        let nacc = combine_ir(&mut b, dp.op, acc, bval);
        let k1 = b.ins().iadd_imm(k, 1);
        b.ins().jump(lh, &[k1.into(), nacc.into()]);
        b.seal_block(lh);

        b.switch_to_block(done);
        b.seal_block(done);
        let res = b.block_params(done)[0];
        // Return i64: an int accumulator's partial sign-extends (the pool combines in
        // i64; the low 32 bits are the exact i32 fold by the mod-2^32 homomorphism).
        let ret = if dp.acc_is_long { res } else { b.ins().sextend(I64, res) };
        b.ins().return_(&[ret]);
        b.finalize();
    }
    let id = module.declare_anonymous_function(&sig).ok()?;
    module.define_function(id, &mut ctx).ok()?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().ok()?;
    Some(module.get_finalized_function(id) as i64)
}

/// Emit the MAIN method as a data-parallel FORK: compute the trip count, and if it
/// clears `MIN_PARALLEL_TRIP`, call `rava_parallel_reduce` with the body fn's address
/// + op code; on `RegionAbort` (or a sub-threshold trip) run the SERIAL fallback loop
/// (the same fold, sequential, under the GIL). The reduced accumulator (combined with
/// `acc_init`) is returned. Restricted to step-1, long-accumulator loops.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_parallel_body(
    b: &mut FunctionBuilder,
    body_ptr: i64,
    pr_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    alen_ref: cranelift::codegen::ir::FuncRef,
    bc: &[u8],
    dp: &DataParallelLoop,
    sroa: &SroaCtx,
) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let locals_ptr = b.block_params(entry)[0];
    let acc_ty = if dp.acc_is_long { I64 } else { I32 };

    // limit (i32) → trip = limit - c0 (step 1), sign-extended to i64.
    // When the limit is a hoisted `arr.length` (limit_from_array), resolve it from the
    // array's RUNTIME length — the replaced method never runs the `n = arr.length`
    // prologue, so the limit local is uninitialised. Otherwise read the param/const.
    let limit = if let Some(arr) = dp.limit_from_array {
        // Faulting arraylength: a null array sets a pending NPE (observed when the method
        // returns) and yields 0 → trip 0 → serial runs no iterations, preserving the
        // guest's `arr.length`-on-null NPE. A valid array gives its true length.
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((arr * 8) as i32));
        let call = b.ins().call(alen_ref, &[handle]);
        let len = b.inst_results(call)[0];
        b.ins().ireduce(I32, len)
    } else {
        match dp.limit_local {
            Some(l) => {
                let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((l * 8) as i32));
                b.ins().ireduce(I32, v)
            }
            None => b.ins().iconst(I32, dp.lim_const as i64),
        }
    };
    let c0v = b.ins().iconst(I32, dp.c0 as i64);
    let trip32 = b.ins().isub(limit, c0v);
    let trip = b.ins().sextend(I64, trip32);

    // ENV CAPTURE: marshal each invariant captured param into a WorkerEnv array (one
    // i64 cell per capture, in slot order), THEN one cell per captured array holding its
    // resolved element base pointer. The body reads a scalar at env[slot*8] and an array
    // base at env[(n_scalar + arr_slot)*8]. Null env only when nothing is captured.
    let n_scalar = dp.captures.len();
    let n_env = n_scalar + dp.array_captures.len();
    // `arrays_ok` ANDs `limit <= length` over every captured array — when it fails (a
    // short array / null / reference array → length ≤ -1), the parallel path is skipped
    // and the bounds-checked serial fallback runs (and throws AIOOBE/NPE if the guest
    // would). Resolved here, under the GIL, before any worker starts.
    let mut arrays_ok = b.ins().iconst(I32, 1);
    let env_ptr = if n_env == 0 {
        b.ins().iconst(I64, 0)
    } else {
        let env_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (n_env as u32) * 8,
            3,
        ));
        for (s, &(local, _)) in dp.captures.iter().enumerate() {
            let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
            b.ins().stack_store(v, env_slot, (s as i32) * 8);
        }
        if !dp.array_captures.is_empty() {
            let len_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let len_ptr = b.ins().stack_addr(I64, len_slot, 0);
            let lim64 = b.ins().sextend(I64, limit);
            for (s, &(local, _)) in dp.array_captures.iter().enumerate() {
                let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
                let call = b.ins().call(abl_ref, &[handle, len_ptr]);
                let base = b.inst_results(call)[0];
                b.ins().stack_store(base, env_slot, ((n_scalar + s) as i32) * 8);
                let len = b.ins().load(I64, MemFlags::trusted(), len_ptr, Offset32::new(0));
                let in_bounds = b.ins().icmp(IntCC::SignedLessThanOrEqual, lim64, len);
                let ib32 = b.ins().uextend(I32, in_bounds);
                arrays_ok = b.ins().band(arrays_ok, ib32);
            }
        }
        b.ins().stack_addr(I64, env_slot, 0)
    };

    let fork = b.create_block();
    let serial = b.create_block();
    let done = b.create_block();
    b.append_block_param(done, I64); // final accumulator (long bits / sign-extended int)

    let minv = b.ins().iconst(I64, MIN_PARALLEL_TRIP);
    let big = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, trip, minv);
    // Fork only when the trip clears the threshold AND every captured array is provably
    // long enough (limit <= length) — otherwise the bounds-checking serial path runs.
    let big32 = b.ins().uextend(I32, big);
    let go = b.ins().band(big32, arrays_ok);
    b.ins().brif(go, fork, &[], serial, &[]);

    // ── fork: hand the body fn + trip + env to the pool; abort → serial, else combine. ──
    b.switch_to_block(fork);
    b.seal_block(fork);
    let bptr = b.ins().iconst(I64, body_ptr);
    let opv = b.ins().iconst(I64, op_code(dp.op));
    let abort_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        2,
    ));
    let abort_ptr = b.ins().stack_addr(I64, abort_slot, 0);
    let call = b.ins().call(pr_ref, &[bptr, trip, opv, abort_ptr, env_ptr]);
    let result = b.inst_results(call)[0]; // i64 reduced partials
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    // Combine with acc_init at the accumulator width; an int acc truncates the i64
    // pool result to i32 (low 32 bits exact), then sign-extends for the i64 return.
    let combined = if dp.acc_is_long {
        let initv = b.ins().iconst(I64, dp.acc_init);
        combine_ir(b, dp.op, initv, result)
    } else {
        let r32 = b.ins().ireduce(I32, result);
        let init32 = b.ins().iconst(I32, dp.acc_init);
        let c32 = combine_ir(b, dp.op, init32, r32);
        b.ins().sextend(I64, c32)
    };
    b.ins().brif(aborted, serial, &[], done, &[combined.into()]);

    // ── serial fallback: acc = acc_init; for k in 0..trip { acc = op(acc, body(c0+k)) }.
    b.switch_to_block(serial);
    b.seal_block(serial);
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // k
    b.append_block_param(sloop, acc_ty); // acc (i32 or i64)
    let sbody = b.create_block();
    let zero = b.ins().iconst(I64, 0);
    let init2 = b.ins().iconst(acc_ty, dp.acc_init);
    b.ins().jump(sloop, &[zero.into(), init2.into()]);
    b.switch_to_block(sloop);
    let sk = b.block_params(sloop)[0];
    let sacc = b.block_params(sloop)[1];
    let sgo = b.ins().icmp(IntCC::SignedLessThan, sk, trip);
    let sfinal = if dp.acc_is_long { sacc } else { b.ins().sextend(I64, sacc) };
    b.ins().brif(sgo, sbody, &[], done, &[sfinal.into()]);
    b.switch_to_block(sbody);
    b.seal_block(sbody);
    let skf = b.ins().ireduce(I32, sk);
    let sc0 = b.ins().iconst(I32, dp.c0 as i64);
    let si = b.ins().iadd(sc0, skf);
    // Serial captures read directly from the live locals.
    let mut read_local = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
        let raw = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        Some(if is_long { raw } else { b.ins().ireduce(I32, raw) })
    };
    // Serial arr[i]: the BOUNDS-CHECKED `rava_aload` (the serial path runs precisely
    // when the parallel gate could not prove safety — e.g. limit > length — so it must
    // raise AIOOBE/NPE exactly where the guest would). Sets a pending fault on a bad
    // access; the compiled method's caller observes it on return.
    let arrcaps = &dp.array_captures;
    let mut read_array_elem = |b: &mut FunctionBuilder, local: usize, idx: Value, is_long: bool| -> Option<Value> {
        arrcaps.iter().position(|(l, _)| *l == local)?;
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        let idx64 = b.ins().sextend(I64, idx);
        let zero = b.ins().iconst(I64, 0);
        let call = b.ins().call(al_ref, &[handle, idx64, zero]);
        let res = b.inst_results(call)[0];
        Some(if is_long { res } else { b.ins().ireduce(I32, res) })
    };
    let (sbv, sl) = emit_body_expr(b, bc, dp.body_range, si, dp.iv_local, &dp.scratch, &[dp.acc_load_pc], &[], &[], sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
    let sbval = if dp.acc_is_long {
        if sl { sbv } else { b.ins().sextend(I64, sbv) }
    } else if sl {
        b.ins().ireduce(I32, sbv)
    } else {
        sbv
    };
    let snacc = combine_ir(b, dp.op, sacc, sbval);
    let sk1 = b.ins().iadd_imm(sk, 1);
    b.ins().jump(sloop, &[sk1.into(), snacc.into()]);
    b.seal_block(sloop);

    // ── done: return the reduced accumulator (i64 bits). ──
    b.switch_to_block(done);
    b.seal_block(done);
    let acc = b.block_params(done)[0];
    b.ins().return_(&[acc]);
    Some(())
}
