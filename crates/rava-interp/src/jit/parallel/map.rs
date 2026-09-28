//! Map and filter forks: element-independent transform and predicate compaction.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Compile a MAP loop's body into a SECOND module function with the `MapBody` ABI
/// (`extern "C" fn(lo, hi, *mut ok, env)`): for `k in [lo, hi)` compute `out[c0+k] =
/// bodyexpr(c0+k)` and store it DIRECTLY into the output element base (no reduction, no
/// return). Mirrors `build_reduce_body_fn`, swapping the fold for a direct store. The env
/// layout is [scalar captures][input array bases][OUTPUT base]. Returns the machine-code
/// address, or `None` if the body isn't codegen-able.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_map_body_fn(module: &mut JITModule, m: &MapLoop, bc: &[u8], sroa: &SroaCtx) -> Option<i64> {
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(I64)); // lo
    sig.params.push(AbiParam::new(I64)); // hi
    sig.params.push(AbiParam::new(I64)); // *ok (i32)
    sig.params.push(AbiParam::new(I64)); // env
    // no returns (void MapBody)
    ctx.func.signature = sig.clone();
    let out_ty = if m.out_is_long { I64 } else { I32 };
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
        let body_blk = b.create_block();
        let done = b.create_block();
        b.ins().jump(lh, &[lo.into()]);

        b.switch_to_block(lh);
        let k = b.block_params(lh)[0];
        let go = b.ins().icmp(IntCC::SignedLessThan, k, hi);
        b.ins().brif(go, body_blk, &[], done, &[]);

        b.switch_to_block(body_blk);
        b.seal_block(body_blk);
        let kf = b.ins().ireduce(I32, k);
        let c0v = b.ins().iconst(I32, m.c0 as i64);
        let i_val = b.ins().iadd(c0v, kf);
        let captures = &m.captures;
        let mut read_local = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
            let slot = captures.iter().position(|(l, _)| *l == local)?;
            let raw = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new((slot * 8) as i32));
            Some(if is_long { raw } else { b.ins().ireduce(I32, raw) })
        };
        let n_scalar = captures.len();
        let arrcaps = &m.array_captures;
        let mut read_array_elem = |b: &mut FunctionBuilder, local: usize, idx: Value, is_long: bool| -> Option<Value> {
            let slot = arrcaps.iter().position(|(l, _)| *l == local)?;
            let base = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new(((n_scalar + slot) * 8) as i32));
            let idx64 = b.ins().sextend(I64, idx);
            let width = if is_long { 8 } else { 4 };
            let off = b.ins().imul_imm(idx64, width);
            let addr = b.ins().iadd(base, off);
            Some(b.ins().load(if is_long { I64 } else { I32 }, MemFlags::trusted(), addr, Offset32::new(0)))
        };
        // The value expr (no accumulator to skip → sentinel pc).
        let (bv, is_long) = emit_body_expr(&mut b, bc, m.body_range, i_val, m.iv_local, &m.scratch, &m.store_skip, &m.math_ops, &m.const_ops, sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
        // Coerce to the output element width.
        let val = if m.out_is_long {
            if is_long { bv } else { b.ins().sextend(I64, bv) }
        } else if is_long {
            b.ins().ireduce(I32, bv)
        } else {
            bv
        };
        // Store out[i] DIRECTLY: out base sits after the scalar + input-array slots.
        let out_base = b.ins().load(I64, MemFlags::trusted(), env_ptr, Offset32::new(((n_scalar + arrcaps.len()) * 8) as i32));
        let idx64 = b.ins().sextend(I64, i_val);
        let width = if m.out_is_long { 8 } else { 4 };
        let off = b.ins().imul_imm(idx64, width);
        let addr = b.ins().iadd(out_base, off);
        b.ins().store(MemFlags::trusted(), val, addr, Offset32::new(0));
        let _ = out_ty;
        let k1 = b.ins().iadd_imm(k, 1);
        b.ins().jump(lh, &[k1.into()]);
        b.seal_block(lh);

        b.switch_to_block(done);
        b.seal_block(done);
        b.ins().return_(&[]);
        b.finalize();
    }
    let id = module.declare_anonymous_function(&sig).ok()?;
    module.define_function(id, &mut ctx).ok()?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().ok()?;
    Some(module.get_finalized_function(id) as i64)
}

/// Emit a FILTER method as a fork: resolve the in/out long-array bases + trip, and if the trip
/// clears MIN_PARALLEL_TRIP AND both arrays are long enough, call `rava_parallel_filter` with
/// the predicate fn. Otherwise (or on abort) run a bounds-checked SERIAL compaction. Returns
/// the surviving count.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_parallel_filter(
    b: &mut FunctionBuilder,
    pf_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    las_ref: cranelift::codegen::ir::FuncRef,
    ias_ref: cranelift::codegen::ir::FuncRef,
    f: &FilterLoop,
) -> Option<()> {
    let store_ref = if f.elem_bytes == 4 { ias_ref } else { las_ref };
    let cmp_code: i64 = match f.pred_keep {
        IntCC::SignedGreaterThan => 0,
        IntCC::SignedGreaterThanOrEqual => 1,
        IntCC::SignedLessThan => 2,
        IntCC::SignedLessThanOrEqual => 3,
        IntCC::NotEqual => 4,
        _ => 5,
    };
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let locals_ptr = b.block_params(entry)[0];

    let limit = match f.limit_local {
        Some(l) => {
            let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((l * 8) as i32));
            b.ins().ireduce(I32, v)
        }
        None => b.ins().iconst(I32, f.lim_const as i64),
    };
    let c0v = b.ins().iconst(I32, f.c0 as i64);
    let trip32 = b.ins().isub(limit, c0v);
    let trip = b.ins().sextend(I64, trip32);
    let lim64 = b.ins().sextend(I64, limit);

    let len_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    let len_ptr = b.ins().stack_addr(I64, len_slot, 0);
    let mut resolve = |b: &mut FunctionBuilder, local: usize| -> (Value, Value) {
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        let call = b.ins().call(abl_ref, &[handle, len_ptr]);
        let base = b.inst_results(call)[0];
        let len = b.ins().load(I64, MemFlags::trusted(), len_ptr, Offset32::new(0));
        (base, len)
    };
    let (in_base, in_len) = resolve(b, f.in_local);
    let (out_base, out_len) = resolve(b, f.out_local);
    // Worst case every element survives, so the output must hold `trip` elements.
    let ok_in = b.ins().icmp(IntCC::SignedLessThanOrEqual, lim64, in_len);
    let ok_out = b.ins().icmp(IntCC::SignedLessThanOrEqual, trip, out_len);
    let both = b.ins().band(ok_in, ok_out);
    let arrays_ok = b.ins().uextend(I32, both);

    let fork = b.create_block();
    let serial = b.create_block();
    let done = b.create_block();
    b.append_block_param(done, I64); // surviving count
    let minv = b.ins().iconst(I64, MIN_PARALLEL_TRIP);
    let big = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, trip, minv);
    let big32 = b.ins().uextend(I32, big);
    let go = b.ins().band(big32, arrays_ok);
    b.ins().brif(go, fork, &[], serial, &[]);

    // ── fork ──
    b.switch_to_block(fork);
    b.seal_block(fork);
    let cmpv = b.ins().iconst(I64, cmp_code);
    let cstv = b.ins().iconst(I64, f.pred_const);
    let wv = b.ins().iconst(I64, f.elem_bytes);
    let abort_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        2,
    ));
    let abort_ptr = b.ins().stack_addr(I64, abort_slot, 0);
    let call = b.ins().call(pf_ref, &[trip, out_base, in_base, cmpv, cstv, wv, abort_ptr]);
    let count = b.inst_results(call)[0];
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    b.ins().brif(aborted, serial, &[], done, &[count.into()]);

    // ── serial compaction: k=0; for j in 0..trip { v=in[c0+j]; if keep(v){ out[k]=v; k++ } } ──
    b.switch_to_block(serial);
    b.seal_block(serial);
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // j
    b.append_block_param(sloop, I64); // k
    let sbody = b.create_block();
    let store_blk = b.create_block();
    let cont = b.create_block();
    b.append_block_param(cont, I64); // k after (maybe incremented)
    b.append_block_param(cont, I64); // j (carried)
    let z0 = b.ins().iconst(I64, 0);
    let z1 = b.ins().iconst(I64, 0);
    b.ins().jump(sloop, &[z0.into(), z1.into()]);
    b.switch_to_block(sloop);
    let sj = b.block_params(sloop)[0];
    let sk = b.block_params(sloop)[1];
    let sgo = b.ins().icmp(IntCC::SignedLessThan, sj, trip);
    b.ins().brif(sgo, sbody, &[], done, &[sk.into()]);
    b.switch_to_block(sbody);
    b.seal_block(sbody);
    let sjf = b.ins().ireduce(I32, sj);
    let sc0 = b.ins().iconst(I32, f.c0 as i64);
    let si = b.ins().iadd(sc0, sjf);
    let si64 = b.ins().sextend(I64, si);
    let in_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((f.in_local * 8) as i32));
    let zc = b.ins().iconst(I64, 0);
    let acall = b.ins().call(al_ref, &[in_handle, si64, zc]);
    let v = b.inst_results(acall)[0];
    let cst = b.ins().iconst(I64, f.pred_const);
    let keep = b.ins().icmp(f.pred_keep, v, cst);
    b.ins().brif(keep, store_blk, &[], cont, &[sk.into(), sj.into()]);
    b.switch_to_block(store_blk);
    b.seal_block(store_blk);
    let out_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((f.out_local * 8) as i32));
    b.ins().call(store_ref, &[out_handle, sk, v]);
    let k1 = b.ins().iadd_imm(sk, 1);
    b.ins().jump(cont, &[k1.into(), sj.into()]);
    b.switch_to_block(cont);
    b.seal_block(cont);
    let ck = b.block_params(cont)[0];
    let cj = b.block_params(cont)[1];
    let j1 = b.ins().iadd_imm(cj, 1);
    b.ins().jump(sloop, &[j1.into(), ck.into()]);
    b.seal_block(sloop);

    b.switch_to_block(done);
    b.seal_block(done);
    let result = b.block_params(done)[0];
    b.ins().return_(&[result]);
    Some(())
}

/// Emit the MAIN method as a data-parallel MAP FORK: resolve the trip + the input/OUTPUT
/// array bases, and if the trip clears `MIN_PARALLEL_TRIP` AND every array is provably long
/// enough, call `rava_parallel_map` (workers write disjoint `out[i]` directly). Otherwise
/// (or on abort) run a bounds-checked serial map that throws AIOOBE/NPE exactly where the
/// guest would. The method returns void.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_parallel_map(
    b: &mut FunctionBuilder,
    body_ptr: i64,
    pm_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    alen_ref: cranelift::codegen::ir::FuncRef,
    ias_ref: cranelift::codegen::ir::FuncRef,
    las_ref: cranelift::codegen::ir::FuncRef,
    bc: &[u8],
    m: &MapLoop,
    sroa: &SroaCtx,
) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let locals_ptr = b.block_params(entry)[0];

    // limit (i32) → trip = limit - c0 (step 1). Hoisted `arr.length` resolves at runtime.
    let limit = if let Some(arr) = m.limit_from_array {
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((arr * 8) as i32));
        let call = b.ins().call(alen_ref, &[handle]);
        let len = b.inst_results(call)[0];
        b.ins().ireduce(I32, len)
    } else {
        match m.limit_local {
            Some(l) => {
                let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((l * 8) as i32));
                b.ins().ireduce(I32, v)
            }
            None => b.ins().iconst(I32, m.lim_const as i64),
        }
    };
    let c0v = b.ins().iconst(I32, m.c0 as i64);
    let trip32 = b.ins().isub(limit, c0v);
    let trip = b.ins().sextend(I64, trip32);

    // ENV: [scalar captures][input array bases][OUTPUT base]. arrays_ok ANDs `limit <=
    // length` over every input array AND the output array (a short/null out → serial store,
    // which raises AIOOBE/NPE). Env is never null (the out base is always present).
    let n_scalar = m.captures.len();
    let n_arrcap = m.array_captures.len();
    let n_env = n_scalar + n_arrcap + 1;
    let mut arrays_ok = b.ins().iconst(I32, 1);
    let env_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (n_env as u32) * 8,
        3,
    ));
    for (s, &(local, _)) in m.captures.iter().enumerate() {
        let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        b.ins().stack_store(v, env_slot, (s as i32) * 8);
    }
    let len_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    let len_ptr = b.ins().stack_addr(I64, len_slot, 0);
    let lim64 = b.ins().sextend(I64, limit);
    // Input arrays are read up to `in[limit-1 + read_off_hi]`, so gate them by `limit +
    // read_off_hi <= length` (the low end `c0 + read_off_lo >= 0` was checked at analyze).
    // The output is written at offset 0, so it uses the plain `limit <= length`.
    let in_lim64 = b.ins().iadd_imm(lim64, m.read_off_hi as i64);
    let mut resolve = |b: &mut FunctionBuilder, local: usize, env_off: i32, check_lim: Value, arrays_ok: Value| -> Value {
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        let call = b.ins().call(abl_ref, &[handle, len_ptr]);
        let base = b.inst_results(call)[0];
        b.ins().stack_store(base, env_slot, env_off);
        let len = b.ins().load(I64, MemFlags::trusted(), len_ptr, Offset32::new(0));
        let in_bounds = b.ins().icmp(IntCC::SignedLessThanOrEqual, check_lim, len);
        let ib32 = b.ins().uextend(I32, in_bounds);
        b.ins().band(arrays_ok, ib32)
    };
    for (s, &(local, _)) in m.array_captures.iter().enumerate() {
        arrays_ok = resolve(b, local, ((n_scalar + s) as i32) * 8, in_lim64, arrays_ok);
    }
    arrays_ok = resolve(b, m.out_local, ((n_scalar + n_arrcap) as i32) * 8, lim64, arrays_ok);
    let env_ptr = b.ins().stack_addr(I64, env_slot, 0);

    let fork = b.create_block();
    let serial = b.create_block();
    let done = b.create_block();

    let minv = b.ins().iconst(I64, MIN_PARALLEL_TRIP);
    let big = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, trip, minv);
    let big32 = b.ins().uextend(I32, big);
    let go = b.ins().band(big32, arrays_ok);
    b.ins().brif(go, fork, &[], serial, &[]);

    // ── fork: hand the body fn + trip + env to the pool; abort → serial, else done. ──
    b.switch_to_block(fork);
    b.seal_block(fork);
    let bptr = b.ins().iconst(I64, body_ptr);
    let abort_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        2,
    ));
    let abort_ptr = b.ins().stack_addr(I64, abort_slot, 0);
    b.ins().call(pm_ref, &[bptr, trip, abort_ptr, env_ptr]);
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    b.ins().brif(aborted, serial, &[], done, &[]);

    // ── serial fallback: for k in 0..trip { out[c0+k] = body(c0+k) } with checked stores. ──
    b.switch_to_block(serial);
    b.seal_block(serial);
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // k
    let sbody = b.create_block();
    let zero = b.ins().iconst(I64, 0);
    b.ins().jump(sloop, &[zero.into()]);
    b.switch_to_block(sloop);
    let sk = b.block_params(sloop)[0];
    let sgo = b.ins().icmp(IntCC::SignedLessThan, sk, trip);
    b.ins().brif(sgo, sbody, &[], done, &[]);
    b.switch_to_block(sbody);
    b.seal_block(sbody);
    let skf = b.ins().ireduce(I32, sk);
    let sc0 = b.ins().iconst(I32, m.c0 as i64);
    let si = b.ins().iadd(sc0, skf);
    let mut read_local = |b: &mut FunctionBuilder, local: usize, is_long: bool| -> Option<Value> {
        let raw = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        Some(if is_long { raw } else { b.ins().ireduce(I32, raw) })
    };
    let arrcaps = &m.array_captures;
    let mut read_array_elem = |b: &mut FunctionBuilder, local: usize, idx: Value, is_long: bool| -> Option<Value> {
        arrcaps.iter().position(|(l, _)| *l == local)?;
        let handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((local * 8) as i32));
        let idx64 = b.ins().sextend(I64, idx);
        let zero = b.ins().iconst(I64, 0);
        let call = b.ins().call(al_ref, &[handle, idx64, zero]);
        let res = b.inst_results(call)[0];
        Some(if is_long { res } else { b.ins().ireduce(I32, res) })
    };
    let (sbv, sl) = emit_body_expr(b, bc, m.body_range, si, m.iv_local, &m.scratch, &m.store_skip, &m.math_ops, &m.const_ops, sroa, arrcaps, &mut read_local, &mut read_array_elem)?;
    // Store out[si] via the bounds-checked callback (val marshalled to an i64 cell).
    let out_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((m.out_local * 8) as i32));
    let si64 = b.ins().sextend(I64, si);
    if m.out_is_long {
        let v = if sl { sbv } else { b.ins().sextend(I64, sbv) };
        b.ins().call(las_ref, &[out_handle, si64, v]);
    } else {
        let v32 = if sl { b.ins().ireduce(I32, sbv) } else { sbv };
        let v = b.ins().sextend(I64, v32);
        b.ins().call(ias_ref, &[out_handle, si64, v]);
    }
    let sk1 = b.ins().iadd_imm(sk, 1);
    b.ins().jump(sloop, &[sk1.into()]);
    b.seal_block(sloop);

    // ── done: the method is void, but the compiled-fn signature is always i64 → return a
    //    dummy the caller discards. ──
    b.switch_to_block(done);
    b.seal_block(done);
    let z = b.ins().iconst(I64, 0);
    b.ins().return_(&[z]);
    Some(())
}
