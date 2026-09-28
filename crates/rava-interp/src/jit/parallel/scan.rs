//! Prefix-sum, count and argmax forks.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Emit a COUNT-where-predicate method as a fork: resolve the input base + trip, and if the
/// trip clears MIN_PARALLEL_TRIP AND the array is long enough, call `rava_parallel_count`.
/// Otherwise (or on abort) run a bounds-checked SERIAL count. Returns the count.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_parallel_count(
    b: &mut FunctionBuilder,
    pc_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    f: &CountLoop,
) -> Option<()> {
    // Encode the KEEP comparison for the inlined worker loop (see parallel_count).
    let cmp_code: i64 = match f.pred_keep {
        IntCC::SignedGreaterThan => 0,
        IntCC::SignedGreaterThanOrEqual => 1,
        IntCC::SignedLessThan => 2,
        IntCC::SignedLessThanOrEqual => 3,
        IntCC::NotEqual => 4,
        _ => 5, // Equal
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
    let in_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((f.in_local * 8) as i32));
    let call = b.ins().call(abl_ref, &[in_handle, len_ptr]);
    let in_base = b.inst_results(call)[0];
    let in_len = b.ins().load(I64, MemFlags::trusted(), len_ptr, Offset32::new(0));
    let ok_in = b.ins().icmp(IntCC::SignedLessThanOrEqual, lim64, in_len);
    let arrays_ok = b.ins().uextend(I32, ok_in);

    let fork = b.create_block();
    let serial = b.create_block();
    let done = b.create_block();
    b.append_block_param(done, I64); // count
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
    let call = b.ins().call(pc_ref, &[trip, in_base, cmpv, cstv, wv, abort_ptr]);
    let count = b.inst_results(call)[0];
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    b.ins().brif(aborted, serial, &[], done, &[count.into()]);

    // ── serial: k += (in[c0+j] cmp C) branchlessly. ──
    b.switch_to_block(serial);
    b.seal_block(serial);
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // j
    b.append_block_param(sloop, I64); // k
    let sbody = b.create_block();
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
    let inh = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((f.in_local * 8) as i32));
    let zc = b.ins().iconst(I64, 0);
    let acall = b.ins().call(al_ref, &[inh, si64, zc]);
    let v = b.inst_results(acall)[0];
    let cst = b.ins().iconst(I64, f.pred_const);
    let keep = b.ins().icmp(f.pred_keep, v, cst);
    let keep64 = b.ins().uextend(I64, keep);
    let k1 = b.ins().iadd(sk, keep64);
    let j1 = b.ins().iadd_imm(sj, 1);
    b.ins().jump(sloop, &[j1.into(), k1.into()]);
    b.seal_block(sloop);

    b.switch_to_block(done);
    b.seal_block(done);
    let result = b.block_params(done)[0];
    b.ins().return_(&[result]);
    Some(())
}

/// Emit an ARGMAX method as a fork: resolve the array base + limit, and if the trip clears
/// MIN_PARALLEL_TRIP AND the array is long enough, call `rava_parallel_argmax`. Otherwise (or
/// on abort) run a bounds-checked SERIAL argmax (branchless dual max/best update). Returns the
/// index of the maximum (ties → lowest index). Scans `[0, limit)` (the guest's `a[0]` seed).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build_parallel_argmax(
    b: &mut FunctionBuilder,
    pa_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    m: &ArgmaxLoop,
) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let locals_ptr = b.block_params(entry)[0];

    let limit = match m.limit_local {
        Some(l) => {
            let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((l * 8) as i32));
            b.ins().ireduce(I32, v)
        }
        None => b.ins().iconst(I32, m.lim_const as i64),
    };
    let trip = b.ins().sextend(I64, limit); // scans [0, limit)

    let len_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    let len_ptr = b.ins().stack_addr(I64, len_slot, 0);
    let arr_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((m.arr_local * 8) as i32));
    let call = b.ins().call(abl_ref, &[arr_handle, len_ptr]);
    let arr_base = b.inst_results(call)[0];
    let arr_len = b.ins().load(I64, MemFlags::trusted(), len_ptr, Offset32::new(0));
    let ok = b.ins().icmp(IntCC::SignedLessThanOrEqual, trip, arr_len);
    let arrays_ok = b.ins().uextend(I32, ok);

    let fork = b.create_block();
    let serial = b.create_block();
    let done = b.create_block();
    b.append_block_param(done, I64); // result index
    let minv = b.ins().iconst(I64, MIN_PARALLEL_TRIP);
    let big = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, trip, minv);
    let big32 = b.ins().uextend(I32, big);
    let go = b.ins().band(big32, arrays_ok);
    b.ins().brif(go, fork, &[], serial, &[]);

    // ── fork ──
    b.switch_to_block(fork);
    b.seal_block(fork);
    let wv = b.ins().iconst(I64, m.elem_bytes);
    let wmv = b.ins().iconst(I64, m.want_max as i64);
    let abort_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        2,
    ));
    let abort_ptr = b.ins().stack_addr(I64, abort_slot, 0);
    let call = b.ins().call(pa_ref, &[trip, arr_base, wmv, wv, abort_ptr]);
    let idx = b.inst_results(call)[0];
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    b.ins().brif(aborted, serial, &[], done, &[idx.into()]);

    // ── serial: max = a[0]; best = 0; for i in 1..trip { v=a[i]; gt=v>max; max=sel; best=sel } ──
    b.switch_to_block(serial);
    b.seal_block(serial);
    let ah = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((m.arr_local * 8) as i32));
    let zc = b.ins().iconst(I64, 0);
    let seed = b.ins().call(al_ref, &[ah, zc, zc]);
    let v0 = b.inst_results(seed)[0];
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // i
    b.append_block_param(sloop, I64); // max value
    b.append_block_param(sloop, I64); // best index
    let sbody = b.create_block();
    let one = b.ins().iconst(I64, 1);
    let zero = b.ins().iconst(I64, 0);
    b.ins().jump(sloop, &[one.into(), v0.into(), zero.into()]);
    b.switch_to_block(sloop);
    let si = b.block_params(sloop)[0];
    let smax = b.block_params(sloop)[1];
    let sbest = b.block_params(sloop)[2];
    let sgo = b.ins().icmp(IntCC::SignedLessThan, si, trip);
    b.ins().brif(sgo, sbody, &[], done, &[sbest.into()]);
    b.switch_to_block(sbody);
    b.seal_block(sbody);
    let vcall = b.ins().call(al_ref, &[ah, si, zc]);
    let v = b.inst_results(vcall)[0];
    let cc = if m.want_max { IntCC::SignedGreaterThan } else { IntCC::SignedLessThan };
    let gt = b.ins().icmp(cc, v, smax);
    let nm = b.ins().select(gt, v, smax);
    let nb = b.ins().select(gt, si, sbest);
    let i1 = b.ins().iadd_imm(si, 1);
    b.ins().jump(sloop, &[i1.into(), nm.into(), nb.into()]);
    b.seal_block(sloop);

    b.switch_to_block(done);
    b.seal_block(done);
    let result = b.block_params(done)[0];
    b.ins().return_(&[result]);
    Some(())
}

/// Emit a prefix-SCAN method as a fork: resolve the in/out long-array bases + trip, and if
/// the trip clears MIN_PARALLEL_TRIP AND both arrays are long enough, call `rava_parallel_scan`
/// (the 2-pass engine). Otherwise (or on abort) run a bounds-checked SERIAL scan (acc =
/// op-fold; out[i] = acc) that throws AIOOBE/NPE exactly where the guest would. Returns void.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_parallel_scan(
    b: &mut FunctionBuilder,
    ps_ref: cranelift::codegen::ir::FuncRef,
    abl_ref: cranelift::codegen::ir::FuncRef,
    al_ref: cranelift::codegen::ir::FuncRef,
    las_ref: cranelift::codegen::ir::FuncRef,
    ias_ref: cranelift::codegen::ir::FuncRef,
    m: &ScanLoop,
) -> Option<()> {
    // The serial store: int[] → rava_iastore, long[] → rava_lastore.
    let store_ref = if m.elem_bytes == 4 { ias_ref } else { las_ref };
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let locals_ptr = b.block_params(entry)[0];

    let limit = match m.limit_local {
        Some(l) => {
            let v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((l * 8) as i32));
            b.ins().ireduce(I32, v)
        }
        None => b.ins().iconst(I32, m.lim_const as i64),
    };
    let c0v = b.ins().iconst(I32, m.c0 as i64);
    let trip32 = b.ins().isub(limit, c0v);
    let trip = b.ins().sextend(I64, trip32);
    let lim64 = b.ins().sextend(I64, limit);

    // Resolve the input + output element bases + lengths (under the GIL, before the fork).
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
    let (in_base, in_len) = resolve(b, m.in_local);
    let (out_base, out_len) = resolve(b, m.out_local);
    let ok_in = b.ins().icmp(IntCC::SignedLessThanOrEqual, lim64, in_len);
    let ok_out = b.ins().icmp(IntCC::SignedLessThanOrEqual, lim64, out_len);
    let both = b.ins().band(ok_in, ok_out);
    let arrays_ok = b.ins().uextend(I32, both);

    let fork = b.create_block();
    let serial = b.create_block();
    let done = b.create_block();
    let minv = b.ins().iconst(I64, MIN_PARALLEL_TRIP);
    let big = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, trip, minv);
    let big32 = b.ins().uextend(I32, big);
    let go = b.ins().band(big32, arrays_ok);
    b.ins().brif(go, fork, &[], serial, &[]);

    // ── fork: hand trip + the two bases to the 2-pass scan engine. ──
    b.switch_to_block(fork);
    b.seal_block(fork);
    let opv = b.ins().iconst(I64, op_code(m.op));
    let abort_slot = b.create_sized_stack_slot(cranelift::codegen::ir::StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        8,
        2,
    ));
    let abort_ptr = b.ins().stack_addr(I64, abort_slot, 0);
    let wv = b.ins().iconst(I64, m.elem_bytes);
    b.ins().call(ps_ref, &[trip, out_base, in_base, opv, wv, abort_ptr]);
    let ab = b.ins().load(I32, MemFlags::trusted(), abort_ptr, Offset32::new(0));
    let aborted = b.ins().icmp_imm(IntCC::NotEqual, ab, 0);
    b.ins().brif(aborted, serial, &[], done, &[]);

    // ── serial: acc = identity; for k in 0..trip { i=c0+k; acc = op(acc, in[i]); out[i]=acc } ──
    b.switch_to_block(serial);
    b.seal_block(serial);
    let sloop = b.create_block();
    b.append_block_param(sloop, I64); // k
    b.append_block_param(sloop, I64); // acc
    let sbody = b.create_block();
    let zero = b.ins().iconst(I64, 0);
    let id = b.ins().iconst(I64, m.op.identity());
    b.ins().jump(sloop, &[zero.into(), id.into()]);
    b.switch_to_block(sloop);
    let sk = b.block_params(sloop)[0];
    let sacc = b.block_params(sloop)[1];
    let sgo = b.ins().icmp(IntCC::SignedLessThan, sk, trip);
    b.ins().brif(sgo, sbody, &[], done, &[]);
    b.switch_to_block(sbody);
    b.seal_block(sbody);
    let skf = b.ins().ireduce(I32, sk);
    let sc0 = b.ins().iconst(I32, m.c0 as i64);
    let si = b.ins().iadd(sc0, skf);
    let si64 = b.ins().sextend(I64, si);
    let in_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((m.in_local * 8) as i32));
    let zc = b.ins().iconst(I64, 0);
    let acall = b.ins().call(al_ref, &[in_handle, si64, zc]);
    let v = b.inst_results(acall)[0];
    let nacc = combine_ir(b, m.op, sacc, v);
    let out_handle = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((m.out_local * 8) as i32));
    b.ins().call(store_ref, &[out_handle, si64, nacc]);
    let sk1 = b.ins().iadd_imm(sk, 1);
    b.ins().jump(sloop, &[sk1.into(), nacc.into()]);
    b.seal_block(sloop);

    b.switch_to_block(done);
    b.seal_block(done);
    let z = b.ins().iconst(I64, 0);
    b.ins().return_(&[z]);
    Some(())
}
