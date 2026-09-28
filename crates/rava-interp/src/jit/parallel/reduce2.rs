//! Two-accumulator fork: sum and sum-of-squares in one pass.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Compile a DUAL reduction's body into a `ReduceBody2` fn (`extern "C" fn(lo, hi, *ok, env,
/// *out)`): one fused pass folding val1→acc1 (op1) and val2→acc2 (op2) for `k in [lo, hi)`,
/// writing `out[0] = acc1`, `out[1] = acc2`. Mirrors `build_reduce_body_fn` with two
/// accumulators. Returns the machine-code address.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_reduce2_body_fn(module: &mut JITModule, r: &Reduce2Loop, bc: &[u8], sroa: &SroaCtx) -> Option<i64> {
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    for _ in 0..5 {
        sig.params.push(AbiParam::new(I64)); // lo, hi, *ok, env, *out
    }
    ctx.func.signature = sig.clone();
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
        let out_ptr = b.block_params(entry)[4];
        let one = b.ins().iconst(I32, 1);
        b.ins().store(MemFlags::trusted(), one, okp, Offset32::new(0));

        let lh = b.create_block();
        b.append_block_param(lh, I64); // k
        b.append_block_param(lh, I64); // acc1
        b.append_block_param(lh, I64); // acc2
        let body_blk = b.create_block();
        let done = b.create_block();
        b.append_block_param(done, I64);
        b.append_block_param(done, I64);
        let id1 = b.ins().iconst(I64, r.op1.identity());
        let id2 = b.ins().iconst(I64, r.op2.identity());
        b.ins().jump(lh, &[lo.into(), id1.into(), id2.into()]);

        b.switch_to_block(lh);
        let k = b.block_params(lh)[0];
        let acc1 = b.block_params(lh)[1];
        let acc2 = b.block_params(lh)[2];
        let go = b.ins().icmp(IntCC::SignedLessThan, k, hi);
        b.ins().brif(go, body_blk, &[], done, &[acc1.into(), acc2.into()]);

        b.switch_to_block(body_blk);
        b.seal_block(body_blk);
        let kf = b.ins().ireduce(I32, k);
        let c0v = b.ins().iconst(I32, r.c0 as i64);
        let i_val = b.ins().iadd(c0v, kf);
        let captures = &r.captures;
        let mut read_local = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
            let slot = captures.iter().position(|(l, _)| *l == local)?;
            let raw = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new((slot * 8) as i32));
            Some(if is_long { raw } else { b.ins().ireduce(I32, raw) })
        };
        let n_scalar = captures.len();
        let arrcaps = &r.array_captures;
        let mut read_array_elem = |b: &mut FunctionBuilder, local: usize, idx: Value, is_long: bool| -> Option<Value> {
            let slot = arrcaps.iter().position(|(l, _)| *l == local)?;
            let base = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new(((n_scalar + slot) * 8) as i32));
            let idx64 = b.ins().sextend(I64, idx);
            let width = if is_long { 8 } else { 4 };
            let off = b.ins().imul_imm(idx64, width);
            let addr = b.ins().iadd(base, off);
            Some(b.ins().load(if is_long { I64 } else { I32 }, MemFlags::trusted(), addr, Offset32::new(0)))
        };
        let (v1, l1) = emit_body_expr(&mut b, bc, r.val1_range, i_val, r.iv_local, &[], &[], &[], &[], sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
        let v1l = if l1 { v1 } else { b.ins().sextend(I64, v1) };
        let nacc1 = combine_ir(&mut b, r.op1, acc1, v1l);
        let (v2, l2) = emit_body_expr(&mut b, bc, r.val2_range, i_val, r.iv_local, &[], &[], &[], &[], sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
        let v2l = if l2 { v2 } else { b.ins().sextend(I64, v2) };
        let nacc2 = combine_ir(&mut b, r.op2, acc2, v2l);
        let k1 = b.ins().iadd_imm(k, 1);
        b.ins().jump(lh, &[k1.into(), nacc1.into(), nacc2.into()]);
        b.seal_block(lh);

        b.switch_to_block(done);
        b.seal_block(done);
        let a1 = b.block_params(done)[0];
        let a2 = b.block_params(done)[1];
        b.ins().store(MemFlags::trusted(), a1, out_ptr, Offset32::new(0));
        b.ins().store(MemFlags::trusted(), a2, out_ptr, Offset32::new(8));
        b.ins().return_(&[]);
        b.finalize();
    }
    let id = module.declare_anonymous_function(&sig).ok()?;
    module.define_function(id, &mut ctx).ok()?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().ok()?;
    Some(module.get_finalized_function(id) as i64)
}

/// Emit a DUAL-reduction method as a fork: marshal captures + array bases, fork
/// `rava_parallel_reduce2` → `(acc1, acc2)`, then emit the guest's return expression over
/// them; on abort / sub-threshold trip run the serial dual-fold. Both paths converge on a
/// `ret_blk` that emits the return expression with `acc1/acc2` bound to the reduced values.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_parallel_reduce2(
    b: &mut FunctionBuilder,
    body_ptr: i64,
    pr2_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    bc: &[u8],
    r: &Reduce2Loop,
    sroa: &SroaCtx,
) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let locals_ptr = b.block_params(entry)[0];

    let limit = match r.limit_local {
        Some(l) => {
            let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((l * 8) as i32));
            b.ins().ireduce(I32, v)
        }
        None => b.ins().iconst(I32, r.lim_const as i64),
    };
    let c0v = b.ins().iconst(I32, r.c0 as i64);
    let trip32 = b.ins().isub(limit, c0v);
    let trip = b.ins().sextend(I64, trip32);

    let n_scalar = r.captures.len();
    let n_env = n_scalar + r.array_captures.len();
    let mut arrays_ok = b.ins().iconst(I32, 1);
    let env_ptr = if n_env == 0 {
        b.ins().iconst(I64, 0)
    } else {
        let env_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (n_env as u32) * 8,
            3,
        ));
        for (s, &(local, _)) in r.captures.iter().enumerate() {
            let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
            b.ins().stack_store(v, env_slot, (s as i32) * 8);
        }
        if !r.array_captures.is_empty() {
            let len_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let len_ptr = b.ins().stack_addr(I64, len_slot, 0);
            let lim64 = b.ins().sextend(I64, limit);
            for (s, &(local, _)) in r.array_captures.iter().enumerate() {
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
    let ret_blk = b.create_block();
    b.append_block_param(ret_blk, I64); // acc1
    b.append_block_param(ret_blk, I64); // acc2
    let minv = b.ins().iconst(I64, MIN_PARALLEL_TRIP);
    let big = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, trip, minv);
    let big32 = b.ins().uextend(I32, big);
    let go = b.ins().band(big32, arrays_ok);
    b.ins().brif(go, fork, &[], serial, &[]);

    // ── fork: reduce2 → out[0..2], then converge on ret_blk. ──
    b.switch_to_block(fork);
    b.seal_block(fork);
    let bptr = b.ins().iconst(I64, body_ptr);
    let o1 = b.ins().iconst(I64, op_code(r.op1));
    let o2 = b.ins().iconst(I64, op_code(r.op2));
    let out_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        16,
        3,
    ));
    let out_ptr = b.ins().stack_addr(I64, out_slot, 0);
    let abort_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        2,
    ));
    let abort_ptr = b.ins().stack_addr(I64, abort_slot, 0);
    b.ins().call(pr2_ref, &[trip, bptr, env_ptr, o1, o2, out_ptr, abort_ptr]);
    let fa1 = b.ins().load(I64, MemFlags::trusted(), out_ptr, Offset32::new(0));
    let fa2 = b.ins().load(I64, MemFlags::trusted(), out_ptr, Offset32::new(8));
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    b.ins().brif(aborted, serial, &[], ret_blk, &[fa1.into(), fa2.into()]);

    // ── serial: dual-fold; acc1=op1-fold(val1), acc2=op2-fold(val2). ──
    b.switch_to_block(serial);
    b.seal_block(serial);
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // k
    b.append_block_param(sloop, I64); // acc1
    b.append_block_param(sloop, I64); // acc2
    let sbody = b.create_block();
    let z = b.ins().iconst(I64, 0);
    let sid1 = b.ins().iconst(I64, r.op1.identity());
    let sid2 = b.ins().iconst(I64, r.op2.identity());
    b.ins().jump(sloop, &[z.into(), sid1.into(), sid2.into()]);
    b.switch_to_block(sloop);
    let sk = b.block_params(sloop)[0];
    let sa1 = b.block_params(sloop)[1];
    let sa2 = b.block_params(sloop)[2];
    let sgo = b.ins().icmp(IntCC::SignedLessThan, sk, trip);
    b.ins().brif(sgo, sbody, &[], ret_blk, &[sa1.into(), sa2.into()]);
    b.switch_to_block(sbody);
    b.seal_block(sbody);
    let skf = b.ins().ireduce(I32, sk);
    let sc0 = b.ins().iconst(I32, r.c0 as i64);
    let si = b.ins().iadd(sc0, skf);
    let scaps = &r.captures;
    let mut read_local = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
        scaps.iter().position(|(l, _)| *l == local)?;
        let raw = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        Some(if is_long { raw } else { b.ins().ireduce(I32, raw) })
    };
    let arrcaps = &r.array_captures;
    let mut read_array_elem = |b: &mut FunctionBuilder, local: usize, idx: Value, is_long: bool| -> Option<Value> {
        arrcaps.iter().position(|(l, _)| *l == local)?;
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        let idx64 = b.ins().sextend(I64, idx);
        let zc = b.ins().iconst(I64, 0);
        let call = b.ins().call(al_ref, &[handle, idx64, zc]);
        let res = b.inst_results(call)[0];
        Some(if is_long { res } else { b.ins().ireduce(I32, res) })
    };
    let (sv1, sl1) = emit_body_expr(b, bc, r.val1_range, si, r.iv_local, &[], &[], &[], &[], sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
    let sv1l = if sl1 { sv1 } else { b.ins().sextend(I64, sv1) };
    let snacc1 = combine_ir(b, r.op1, sa1, sv1l);
    let (sv2, sl2) = emit_body_expr(b, bc, r.val2_range, si, r.iv_local, &[], &[], &[], &[], sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
    let sv2l = if sl2 { sv2 } else { b.ins().sextend(I64, sv2) };
    let snacc2 = combine_ir(b, r.op2, sa2, sv2l);
    let sk1 = b.ins().iadd_imm(sk, 1);
    b.ins().jump(sloop, &[sk1.into(), snacc1.into(), snacc2.into()]);
    b.seal_block(sloop);

    // ── ret_blk: emit the return expression with acc1/acc2 bound to the reduced values. ──
    b.switch_to_block(ret_blk);
    b.seal_block(ret_blk);
    let a1 = b.block_params(ret_blk)[0];
    let a2 = b.block_params(ret_blk)[1];
    let acc1_local = r.acc1_local;
    let acc2_local = r.acc2_local;
    let mut read_ret = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
        let v = if local == acc1_local {
            a1
        } else if local == acc2_local {
            a2
        } else {
            return None;
        };
        // Accumulators are held as i64; an int `iload acc` reduces to i32 for the int return.
        Some(if is_long { v } else { b.ins().ireduce(I32, v) })
    };
    let mut no_array = |_b: &mut FunctionBuilder, _local: usize, _idx: Value, _is_long: bool| -> Option<Value> { None };
    let dummy_i = b.ins().iconst(I32, 0);
    let (rv, rvl) = emit_body_expr(b, bc, r.ret_range, dummy_i, r.iv_local, &[], &[], &[], &[], sroa, &[], &mut read_ret, &mut no_array)?;
    // The compiled fn returns i64; an int return value sign-extends into the cell.
    let ret = if rvl { rv } else { b.ins().sextend(I64, rv) };
    b.ins().return_(&[ret]);
    Some(())
}
