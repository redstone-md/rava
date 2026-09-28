//! The second half of the AOT opcode translation.
//!
//! Split purely by size: emit_method was 907 lines and the rule is 500. This is the AOT
//! path (opt-in, not on a boot), so the extra call per opcode costs nothing that matters --
//! unlike the interpreter loop, where the same move was measured and reverted.

use crate::aot_clif::*;
use crate::cnp::ConstDesc;

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_op_rest(
    b: &mut FunctionBuilder,
    bc: &[u8],
    pc: usize,
    op: u8,
    stack: &mut Vec<(Value, bool, bool)>,
    locals: Value,
    consts_ptr: Value,
    max_locals: u32,
    cb: &Callbacks,
    argbuf: cranelift::codegen::ir::StackSlot,
    consts: &mut Vec<crate::cnp::ConstDesc>,
    field_of: &dyn Fn(u16) -> Option<(String, String, FieldKind)>,
    method_of: &dyn Fn(u16) -> Option<InvokeInfo>,
    indy_of: &dyn Fn(u16) -> Option<InvokeInfo>,
    ldc_of: &dyn Fn(u16) -> Option<LdcConst>,
    leader_set: &HashMap<usize, usize>,
    blocks: &[cranelift::codegen::ir::Block],
    terminated: &mut bool,
    len: usize,
    flow: &mut block_stacks::BlockStacks,
) -> Option<()> {
    let mut stack = &mut *stack;
    let _ = &mut stack;
    let invoke_fn = cb.invoke;
    let getfield_fn = cb.getfield;
    match op {
        0x17 => stack.push((load_ref(b, bc[pc + 1] as u32, locals), false, false)), // fload
        0x22..=0x25 => stack.push((load_ref(b, op as u32 - 0x22, locals), false, false)), // fload_0..3
        0x38 => {
            // fstore — write the full cell (f32 bits), NOT sign-truncated.
            let (v, _, _) = stack.pop()?;
            store(b, bc[pc + 1] as u32, v, true, locals);
        }
        0x43..=0x46 => {
            let (v, _, _) = stack.pop()?;
            store(b, op as u32 - 0x43, v, true, locals);
        }
        0x62 | 0x66 | 0x6a | 0x6e => {
            // fadd/fsub/fmul/fdiv
            let (rc, _, _) = stack.pop()?;
            let (ra, _, _) = stack.pop()?;
            let a = cell_to_f32(b, ra);
            let c = cell_to_f32(b, rc);
            let r = match op {
                0x62 => b.ins().fadd(a, c),
                0x66 => b.ins().fsub(a, c),
                0x6a => b.ins().fmul(a, c),
                _ => b.ins().fdiv(a, c),
            };
            stack.push((f32_to_cell(b, r), false, false));
        }
        0x72 => {
            // frem — truncated float remainder via the rava_f32_rem libcall (fmod).
            let (rc, _, _) = stack.pop()?;
            let (ra, _, _) = stack.pop()?;
            let a = cell_to_f32(b, ra);
            let c = cell_to_f32(b, rc);
            let call = b.ins().call(cb.f32rem, &[a, c]);
            let r = b.inst_results(call)[0];
            stack.push((f32_to_cell(b, r), false, false));
        }
        0x73 => {
            // drem — truncated double remainder via the rava_f64_rem libcall (fmod).
            // A double rides its raw i64 bits, so bitcast in and back out.
            let (rc, _, _) = stack.pop()?;
            let (ra, _, _) = stack.pop()?;
            let a = b.ins().bitcast(F64, MemFlags::new(), ra);
            let c = b.ins().bitcast(F64, MemFlags::new(), rc);
            let call = b.ins().call(cb.f64rem, &[a, c]);
            let r = b.inst_results(call)[0];
            let bits = b.ins().bitcast(I64, MemFlags::new(), r);
            stack.push((bits, false, true));
        }
        0x76 => {
            // fneg
            let (v, _, _) = stack.pop()?;
            let f = cell_to_f32(b, v);
            let n = b.ins().fneg(f);
            stack.push((f32_to_cell(b, n), false, false));
        }
        0x95 | 0x96 => {
            // fcmpl/fcmpg → -1/0/1.
            let (rc, _, _) = stack.pop()?;
            let (ra, _, _) = stack.pop()?;
            let a = cell_to_f32(b, ra);
            let c = cell_to_f32(b, rc);
            let res = fcmp_result(b, a, c, op == 0x96);
            stack.push((res, false, false));
        }
        0x86 => {
            // i2f
            let (v, _, _) = stack.pop()?;
            let i = b.ins().ireduce(I32, v);
            let f = b.ins().fcvt_from_sint(F32, i);
            stack.push((f32_to_cell(b, f), false, false));
        }
        0x89 => {
            // l2f
            let (v, _, _) = stack.pop()?;
            let f = b.ins().fcvt_from_sint(F32, v);
            stack.push((f32_to_cell(b, f), false, false));
        }
        0x8b => {
            // f2i (saturating)
            let (v, _, _) = stack.pop()?;
            let f = cell_to_f32(b, v);
            let i = b.ins().fcvt_to_sint_sat(I32, f);
            stack.push((b.ins().sextend(I64, i), false, false));
        }
        0x8c => {
            // f2l
            let (v, _, _) = stack.pop()?;
            let f = cell_to_f32(b, v);
            stack.push((b.ins().fcvt_to_sint_sat(I64, f), false, true));
        }
        0x8d => {
            // f2d
            let (v, _, _) = stack.pop()?;
            let f = cell_to_f32(b, v);
            let d = b.ins().fpromote(F64, f);
            stack.push((b.ins().bitcast(I64, MemFlags::new(), d), false, true));
        }
        0x90 => {
            // d2f
            let (v, _, _) = stack.pop()?;
            let d = b.ins().bitcast(F64, MemFlags::new(), v);
            let f = b.ins().fdemote(F32, d);
            stack.push((f32_to_cell(b, f), false, false));
        }
        0xae => {
            // freturn — cell is the f32 bits.
            let (v, _, _) = stack.pop()?;
            b.ins().return_(&[v]);
            *terminated = true;
            return Some(());
        }
        0x30 => {
            // faload — rava_aload returns the float element bits.
            let (idx, _, _) = stack.pop()?;
            let (arr, _, _) = stack.pop()?;
            let isr = b.ins().iconst(I64, 0);
            let call = b.ins().call(cb.aload, &[arr, idx, isr]);
            call_fault::emit(b, cb);
            stack.push((b.inst_results(call)[0], false, false));
        }
        0x2f => {
            // laload — rava_aload(arr, idx, 0) returns the long element as i64.
            let (idx, _, _) = stack.pop()?;
            let (arr, _, _) = stack.pop()?;
            let isr = b.ins().iconst(I64, 0);
            let call = b.ins().call(cb.aload, &[arr, idx, isr]);
            call_fault::emit(b, cb);
            stack.push((b.inst_results(call)[0], false, true));
        }
        0xad => {
            // lreturn — the top cell is the full i64 long.
            let (v, _, _) = stack.pop()?;
            b.ins().return_(&[v]);
            *terminated = true;
            return Some(());
        }
        0x84 => {
            let idx = bc[pc + 1] as u32;
            let c = bc[pc + 2] as i8 as i64;
            let cur = load_int(b, idx, locals);
            let cur32 = b.ins().ireduce(I32, cur);
            let nv = b.ins().iadd_imm(cur32, c);
            let ext = b.ins().sextend(I64, nv);
            b.ins()
                .store(MemFlags::trusted(), ext, locals, (idx * 8) as i32);
        }
        0xb4 => {
            // getfield: pop receiver (ref), field_id from const table, call.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let (owner, name, k) = field_of(idx)?;
            let (recv, _, _) = stack.pop()?;
            let slot = intern_const(consts, ConstDesc::FieldId(owner, name));
            let fid = b
                .ins()
                .load(I64, MemFlags::trusted(), consts_ptr, (slot * 8) as i32);
            let isr = b.ins().iconst(I64, k.is_ref() as i64);
            let call = b.ins().call(getfield_fn, &[recv, fid, isr]);
            let r = b.inst_results(call)[0];
            stack.push((r, k.is_ref(), k.is_wide()));
        }
        0xb2 => {
            // getstatic: rava_getstatic(caller_ptr, cp, is_ref) → the static's
            // value. is_ref from the field descriptor (field_of bails on J/D/F →
            // long/double statics fail-closed). No receiver popped.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let (_o, _n, k) = field_of(idx)?;
            let caller_ptr = load_const(b, 0, consts_ptr);
            let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
            let cp = load_const(b, cp_slot, consts_ptr);
            let isr = b.ins().iconst(I64, k.is_ref() as i64);
            spill(b, &stack, locals, max_locals); // getstatic can trigger <clinit> → alloc → GC
            let call = b.ins().call(cb.getstatic, &[caller_ptr, cp, isr]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            stack.push((b.inst_results(call)[0], k.is_ref(), k.is_wide()));
        }
        0xb5 => {
            // putfield: pop value, pop receiver, rava_putfield(obj, fid, val, kind).
            // kind = ref(4) or int(0); field_of returns None for wide (J/D/F) so
            // those methods bail (fail-closed — the int cell can't hold them).
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let (owner, name, k) = field_of(idx)?;
            let (val, _, _) = stack.pop()?;
            let (recv, _, _) = stack.pop()?;
            let slot = intern_const(consts, ConstDesc::FieldId(owner, name));
            let fid = b
                .ins()
                .load(I64, MemFlags::trusted(), consts_ptr, (slot * 8) as i32);
            let kind = b.ins().iconst(I64, k.put_kind());
            b.ins().call(cb.putfield, &[recv, fid, val, kind]);
        }
        0xb6 | 0xb7 | 0xb8 | 0xb9 => {
            // invoke*: marshal ncells operands into the args buffer (receiver
            // deepest), call rava_invoke(caller_ptr[const0], cp[const], kind,
            // buf, n), push result per return shape. Uncovered arity → bail.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let info = method_of(idx)?;
            // The OPCODE (not method_of) determines the receiver + dispatch kind:
            // invokestatic (0xb8) has NO receiver, so drop method_of's +1 receiver
            // cell; the others keep it. kind: static 0 / virtual 1 / special 2 /
            // interface 3.
            let (ncells, kind) = match op {
                0xb8 => (info.ncells.checked_sub(1)?, 0i64),
                0xb6 => (info.ncells, 1),
                0xb7 => (info.ncells, 2),
                _ => (info.ncells, 3), // 0xb9 interface
            };
            // Tracer 1: when the direct-leaf arm lowers this site it leaves the
            // builder on its FALLBACK block and hands back the shared resume
            // block; the old arms + rava_invoke tail then run VERBATIM on the
            // pre-invoke operand stack and close the fallback with a jump into
            // `resume`. `below`/`below_wide` snapshot the pre-invoke stack below
            // the operands so the resume-params rewrite can restore their
            // tagging. None → no arm was taken.
            let mut direct_leaf_resume: Option<cranelift::codegen::ir::Block> = None;
            let mut direct_leaf_below: Vec<(Value, bool, bool)> = Vec::new();
            let mut direct_leaf_post_len = 0usize;
            if ncells > 64 || (ncells as usize) > stack.len() {
                return None;
            }
            if op == 0xb8 {
                // ── Tracer 1: direct-leaf arm (BEFORE numeric/object leaves so a
                // native-ABI target is never shadowed by same-class inlining) ──
                // `direct_leaf` is a compile-time CANDIDATE only; the link
                // resolves `ConstDesc::AotLeaf` to the leaf fptr or 0. Nonzero →
                // System V call_indirect (no argbuf/rava_invoke/spill); zero →
                // the fallback block, which re-runs the existing arms below +
                // the unchanged rava_invoke tail — pointer-zero stays exactly on
                // the prior lowering.
                if let Some(leaf) = &info.direct_leaf {
                    if let Some(resume) =
                        emit_direct_leaf_arm(b, stack, consts_ptr, consts, leaf, ncells, &info)?
                    {
                        direct_leaf_resume = Some(resume);
                        let base = stack.len() - ncells as usize;
                        direct_leaf_below = stack[..base].to_vec();
                        direct_leaf_post_len = base + 1;
                    }
                }
                if let Some(leaf) = &info.numeric_leaf {
                    let args = stack.split_off(stack.len() - ncells as usize);
                    if args.iter().any(|(_, reference, wide)| *reference || *wide) {
                        return None;
                    }
                    let values: Vec<_> = args.iter().map(|(value, _, _)| *value).collect();
                    let result = leaf.emit(b, &values);
                    stack.push((result, false, false));
                    // On the direct arm's fallback path the old numeric inline is
                    // the zero-pointer destination — close into the resume block
                    // so both arms merge (identical post-invoke stack shape).
                    if let Some(resume) = direct_leaf_resume {
                        let (below, post_len) = (direct_leaf_below.clone(), direct_leaf_post_len);
                        finish_direct_leaf_fallback(b, stack, resume, post_len);
                        // Rewrite the caller's stack to the resume params so the
                        // method's continuation references dominating values.
                        let params: Vec<Value> = b.block_params(resume).to_vec();
                        *stack = params
                            .into_iter()
                            .zip(
                                below
                                    .iter()
                                    .map(|&(_, is_ref, is_wide)| (is_ref, is_wide))
                                    .chain(std::iter::once((false, false))),
                            )
                            .map(|(value, (is_ref, is_wide))| (value, is_ref, is_wide))
                            .collect();
                    }
                    return Some(());
                }
            }
            {
                if let Some(leaf) = &info.object_leaf {
                    let args = &stack[stack.len() - ncells as usize..];
                    if leaf.supports_opcode(op) && leaf.accepts(args) {
                        let fault = b.create_block();
                        let fast = b.create_block();
                        let null = leaf.guard(b, args);
                        b.ins().brif(null, fault, &[], fast, &[]);
                        b.seal_block(fault);
                        b.seal_block(fast);
                        b.switch_to_block(fault);
                        // Execute the original method on any null receiver BEFORE
                        // fast-path writes. It preserves pre-fault effects and NPE
                        // provenance. Every admitted receiver is dereferenced on
                        // this branch-free body, so this path must throw.
                        for (i, (value, _, _)) in args.iter().enumerate() {
                            b.ins().stack_store(*value, argbuf, (i * 8) as i32);
                        }
                        let buf = b.ins().stack_addr(I64, argbuf, 0);
                        let caller = load_const(b, 0, consts_ptr);
                        let cp = b.ins().iconst(I64, i64::from(idx));
                        let kind = b.ins().iconst(I64, kind);
                        let count = b.ins().iconst(I64, i64::from(ncells));
                        spill(b, &stack, locals, max_locals);
                        b.ins().call(invoke_fn, &[caller, cp, kind, buf, count]);
                        let zero = b.ins().iconst(I64, 0);
                        b.ins().return_(&[zero]);
                        b.switch_to_block(fast);
                        let result = leaf.emit(b, args, consts_ptr, consts, cb);
                        stack.truncate(stack.len() - ncells as usize);
                        if let Some(result) = result {
                            stack.push((result, info.ret_ref, false));
                        }
                        if let Some(resume) = direct_leaf_resume {
                            // Same as the numeric arm: this is the zero-pointer
                            // destination on the direct arm's fallback path —
                            // close into the shared resume block.
                            let (below, post_len) =
                                (direct_leaf_below.clone(), direct_leaf_post_len);
                            finish_direct_leaf_fallback(b, stack, resume, post_len);
                            let params: Vec<Value> = b.block_params(resume).to_vec();
                            *stack = params
                                .into_iter()
                                .zip(
                                    below
                                        .iter()
                                        .map(|&(_, is_ref, is_wide)| (is_ref, is_wide))
                                        .chain(std::iter::once((info.ret_ref, false))),
                                )
                                .map(|(value, (is_ref, is_wide))| (value, is_ref, is_wide))
                                .collect();
                        }
                        return Some(());
                    }
                }
            }
            // Pop args in reverse (top = last cell); store into buf[i].
            for i in (0..ncells).rev() {
                let (v, _, _) = stack.pop()?;
                b.ins().stack_store(v, argbuf, (i * 8) as i32);
            }
            let buf = b.ins().stack_addr(I64, argbuf, 0);
            let caller_ptr = load_const(b, 0, consts_ptr); // slot 0 = this class's ClassPtr
            let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
            let cp = load_const(b, cp_slot, consts_ptr);
            let kindv = b.ins().iconst(I64, kind);
            let nv = b.ins().iconst(I64, ncells as i64);
            spill(b, &stack, locals, max_locals);
            let call = b.ins().call(invoke_fn, &[caller_ptr, cp, kindv, buf, nv]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            if !info.ret_void {
                let r = b.inst_results(call)[0];
                stack.push((r, info.ret_ref, info.ret_wide));
            }
            if let Some(resume) = direct_leaf_resume {
                // The direct-leaf arm's fallback block ends here: the rava_invoke
                // lowering above is the UNCHANGED zero-pointer path. Close into
                // the resume block (the fallback edge carries exactly the shape
                // the direct branch jumped with: below cells + one result) and
                // rewrite the caller's stack to the resume params so the
                // method's continuation references dominating values.
                let (below, post_len) = (direct_leaf_below.clone(), direct_leaf_post_len);
                finish_direct_leaf_fallback(b, stack, resume, post_len);
                let params: Vec<Value> = b.block_params(resume).to_vec();
                *stack = params
                    .into_iter()
                    .zip(
                        below
                            .iter()
                            .map(|&(_, is_ref, is_wide)| (is_ref, is_wide))
                            .chain(std::iter::once((info.ret_ref, info.ret_wide))),
                    )
                    .map(|(value, (is_ref, is_wide))| (value, is_ref, is_wide))
                    .collect();
                return Some(());
            }
        }
        0xba
        | 0x12
        | 0x13
        | 0x14
        | 0xbb
        | 0xbd
        | 0xbc
        | 0xc1
        | 0x53
        | 0x4f
        | 0x50
        | 0x51
        | 0x52
        | 0x54
        | 0x55
        | 0x56
        | 0xbf
        | 0xbe
        | 0x2e
        | 0x32
        | 0x33
        | 0x34
        | 0x35
        | 0xc0
        | 0x5f
        | 0x5a
        | 0x99..=0xa4
        | 0xc6
        | 0xc7
        | 0xa5
        | 0xa6
        | 0xa7
        | 0xaa
        | 0xab
        | 0xac
        | 0xb0
        | 0xb1 => emit_op_rest2(
            b, bc, pc, op, stack, locals, consts_ptr, max_locals, cb, argbuf, consts, field_of,
            method_of, indy_of, ldc_of, leader_set, blocks, terminated, len, flow,
        )?,
        _ => return None,
    }
    Some(())
}
