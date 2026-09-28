//! Emitting a whole method to Cranelift IR.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::aot_clif::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_method(
    b: &mut FunctionBuilder,
    bc: &[u8],
    leaders: &[usize],
    leader_set: &HashMap<usize, usize>,
    blocks: &[cranelift::codegen::ir::Block],
    locals: Value,
    consts_ptr: Value,
    cb: &Callbacks,
    argbuf: cranelift::codegen::ir::StackSlot,
    max_locals: u32,
    consts: &mut Vec<crate::cnp::ConstDesc>,
    field_of: &dyn Fn(u16) -> Option<(String, String, FieldKind)>,
    method_of: &dyn Fn(u16) -> Option<InvokeInfo>,
    indy_of: &dyn Fn(u16) -> Option<InvokeInfo>,
    ldc_of: &dyn Fn(u16) -> Option<LdcConst>,
    budget: cranelift::codegen::ir::StackSlot,
    progress_fn: cranelift::codegen::ir::FuncRef,
) -> Option<()> {
    use crate::cnp::ConstDesc;
    let getfield_fn = cb.getfield;
    // GC-SPILL: before any GC-capable callback (invoke/indy/new/ldc/anewarray/getstatic/
    // instanceof), spill every live REFERENCE still on the operand stack into the rooted
    // spill area (cells[max_locals + k]) so a collection during the call forwards them in
    // place; reload after so the SSA stack picks up the moved addresses. Refs held only
    // in a Cranelift SSA value are invisible to the moving GC — this is the fix that lets
    // invoke-carrying methods run AOT (getfield/putfield alone never allocate → no spill).
    let invoke_fn = cb.invoke;

    let mut flow = block_stacks::BlockStacks::new(blocks.len());
    let loop_headers = progress::loop_headers(bc)?;
    while let Some(li) = flow.next() {
        let start = leaders[li];
        let end = leaders.get(li + 1).copied().unwrap_or(bc.len());
        b.switch_to_block(blocks[li]);
        // (value, is_ref, is_wide). A LONG is modelled as ONE stack entry holding the
        // full i64 (double deferred) — self-consistent with rava's invoke buffer
        // (count_args counts a long as 1 cell). is_wide distinguishes it for dup2/pop2.
        let mut stack = flow.incoming(b, li, blocks[li]);
        if loop_headers.contains(&start) {
            progress::emit(b, budget, progress_fn, &mut stack, locals, max_locals);
        }
        let mut pc = start;
        let mut terminated = false;
        while pc < end {
            let op = bc[pc];
            let len = insn_len_at(bc, pc)?;
            null_guard::emit(b, bc, pc, &stack, consts_ptr, cb, method_of)?;
            match op {
                0x01 => stack.push((b.ins().iconst(I64, 0), true, false)), // aconst_null (null ref cell = 0)
                0x02..=0x08 => stack.push((b.ins().iconst(I64, (op as i64) - 3), false, false)),
                0x09 | 0x0a => stack.push((b.ins().iconst(I64, (op as i64) - 9), false, true)), // lconst_0/1
                0x10 => stack.push((b.ins().iconst(I64, bc[pc + 1] as i8 as i64), false, false)),
                0x11 => stack.push((b.ins().iconst(I64, s16(bc, pc + 1) as i64), false, false)),
                0x15 => stack.push((load_int(b, bc[pc + 1] as u32, locals), false, false)),
                0x1a..=0x1d => stack.push((load_int(b, op as u32 - 0x1a, locals), false, false)),
                0x16 => stack.push((load_ref(b, bc[pc + 1] as u32, locals), false, true)), // lload (full i64)
                0x1e..=0x21 => stack.push((load_ref(b, op as u32 - 0x1e, locals), false, true)), // lload_0..3
                0x19 => stack.push((load_ref(b, bc[pc + 1] as u32, locals), true, false)), // aload
                0x2a..=0x2d => stack.push((load_ref(b, op as u32 - 0x2a, locals), true, false)), // aload_0..3
                0x36 => {
                    let (v, _, _) = stack.pop()?;
                    store(b, bc[pc + 1] as u32, v, false, locals);
                }
                0x3b..=0x3e => {
                    let (v, _, _) = stack.pop()?;
                    store(b, op as u32 - 0x3b, v, false, locals);
                }
                0x37 => {
                    // lstore (full i64, no truncate — reuse the ref store path)
                    let (v, _, _) = stack.pop()?;
                    store(b, bc[pc + 1] as u32, v, true, locals);
                }
                0x3f..=0x42 => {
                    let (v, _, _) = stack.pop()?;
                    store(b, op as u32 - 0x3f, v, true, locals);
                }
                0x3a => {
                    // astore (reference, full i64)
                    let (v, _, _) = stack.pop()?;
                    store(b, bc[pc + 1] as u32, v, true, locals);
                }
                0x4b..=0x4e => {
                    // astore_0..3 (reference)
                    let (v, _, _) = stack.pop()?;
                    store(b, op as u32 - 0x4b, v, true, locals);
                }
                0x57 => {
                    stack.pop()?;
                }
                0x58 => {
                    // pop2: one category-2 (wide) value, or two category-1.
                    let (_, _, w) = stack.pop()?;
                    if !w {
                        stack.pop()?;
                    }
                }
                0x59 => {
                    let top = *stack.last()?;
                    stack.push(top);
                }
                0x5c => {
                    // dup2: duplicate top wide value, or top two narrow values.
                    let top = *stack.last()?;
                    if top.2 {
                        stack.push(top);
                    } else {
                        let n = stack.len();
                        let a = *stack.get(n - 2)?;
                        let bb = *stack.get(n - 1)?;
                        stack.push(a);
                        stack.push(bb);
                    }
                }
                0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 => {
                    let (rb, _, _) = stack.pop()?;
                    let (ra, _, _) = stack.pop()?;
                    let a = b.ins().ireduce(I32, ra);
                    let c = b.ins().ireduce(I32, rb);
                    let res = match op {
                        0x60 => b.ins().iadd(a, c),
                        0x64 => b.ins().isub(a, c),
                        0x68 => b.ins().imul(a, c),
                        0x7e => b.ins().band(a, c),
                        0x80 => b.ins().bor(a, c),
                        0x82 => b.ins().bxor(a, c),
                        _ => unreachable!(),
                    };
                    stack.push((b.ins().sextend(I64, res), false, false));
                }
                0x6c | 0x70 => {
                    // idiv / irem — inline sdiv/srem with a divisor==0 guard (→ the
                    // rava_i32_divrem callback which sets ArithmeticException pending and
                    // returns 0) and the INT_MIN/-1 overflow special-case (Cranelift's
                    // sdiv/srem TRAP on both). Direct port of the JIT's int_divrem. The
                    // internal blocks live WITHIN this basic block — the stack values below
                    // the operands are SSA values that dominate `merge`, so the empty-stack-
                    // at-bytecode-branch invariant is untouched.
                    let is_rem = op == 0x70;
                    let (ry, _, _) = stack.pop()?;
                    let (rx, _, _) = stack.pop()?;
                    let y = b.ins().ireduce(I32, ry);
                    let x = b.ins().ireduce(I32, rx);
                    let dbz = b.create_block();
                    let ok = b.create_block();
                    let neg1 = b.create_block();
                    let divb = b.create_block();
                    let merge = b.create_block();
                    b.append_block_param(merge, I32);
                    let is_zero = b.ins().icmp_imm(cranelift::codegen::ir::condcodes::IntCC::Equal, y, 0);
                    b.ins().brif(is_zero, dbz, &[], ok, &[]);
                    // Division by zero must leave before any following bytecode.
                    b.switch_to_block(dbz);
                    b.seal_block(dbz);
                    let lhs = b.ins().sextend(I64, x);
                    let rhs = b.ins().sextend(I64, y);
                    let remc = b.ins().iconst(I64, is_rem as i64);
                    b.ins().call(cb.divrem, &[lhs, rhs, remc]);
                    let zero = b.ins().iconst(I64, 0);
                    b.ins().return_(&[zero]);
                    b.switch_to_block(ok);
                    b.seal_block(ok);
                    let is_neg1 = b.ins().icmp_imm(cranelift::codegen::ir::condcodes::IntCC::Equal, y, -1);
                    b.ins().brif(is_neg1, neg1, &[], divb, &[]);
                    // divisor -1: x / -1 = wrapping negate (INT_MIN/-1 → INT_MIN); x % -1 = 0.
                    b.switch_to_block(neg1);
                    b.seal_block(neg1);
                    let r1 = if is_rem { b.ins().iconst(I32, 0) } else { b.ins().ineg(x) };
                    b.ins().jump(merge, &[r1.into()]);
                    // safe native path (divisor ∉ {0, -1}).
                    b.switch_to_block(divb);
                    b.seal_block(divb);
                    let r2 = if is_rem { b.ins().srem(x, y) } else { b.ins().sdiv(x, y) };
                    b.ins().jump(merge, &[r2.into()]);
                    b.switch_to_block(merge);
                    b.seal_block(merge);
                    let v = b.block_params(merge)[0];
                    stack.push((b.ins().sextend(I64, v), false, false));
                }
                0x61 | 0x65 | 0x69 | 0x7f | 0x81 | 0x83 => {
                    // ladd/lsub/lmul/land/lor/lxor — full i64, no reduce/re-extend.
                    let (c, _, _) = stack.pop()?;
                    let (a, _, _) = stack.pop()?;
                    let res = match op {
                        0x61 => b.ins().iadd(a, c),
                        0x65 => b.ins().isub(a, c),
                        0x69 => b.ins().imul(a, c),
                        0x7f => b.ins().band(a, c),
                        0x81 => b.ins().bor(a, c),
                        0x83 => b.ins().bxor(a, c),
                        _ => unreachable!(),
                    };
                    stack.push((res, false, true));
                }
                0x6d | 0x71 => {
                    // ldiv / lrem — the rava_i64_divrem callback handles the divisor==0
                    // ArithmeticException and the LONG_MIN/-1 wrap (mirrors the JIT's
                    // long_divrem; no inline blocks needed for i64).
                    let is_rem = op == 0x71;
                    let (y, _, _) = stack.pop()?;
                    let (x, _, _) = stack.pop()?;
                    let remc = b.ins().iconst(I64, is_rem as i64);
                    let call = b.ins().call(cb.divrem64, &[x, y, remc]);
                    let res = b.inst_results(call)[0];
                    let fault = b.create_block();
                    let resume = b.create_block();
                    let is_zero = b.ins().icmp_imm(IntCC::Equal, y, 0);
                    b.ins().brif(is_zero, fault, &[], resume, &[]);
                    b.seal_block(fault);
                    b.seal_block(resume);
                    b.switch_to_block(fault);
                    let zero = b.ins().iconst(I64, 0);
                    b.ins().return_(&[zero]);
                    b.switch_to_block(resume);
                    stack.push((res, false, true));
                }
                0x75 => {
                    // lneg
                    let (a, _, _) = stack.pop()?;
                    stack.push((b.ins().ineg(a), false, true));
                }
                0x78 | 0x7a | 0x7c => {
                    // Java int shifts mask the distance to five bits. Logical
                    // right shift still produces a signed int cell afterwards.
                    let (count, count_ref, count_wide) = stack.pop()?;
                    let (value, value_ref, value_wide) = stack.pop()?;
                    if count_ref || count_wide || value_ref || value_wide { return None; }
                    let value = b.ins().ireduce(I32, value);
                    let count = b.ins().ireduce(I32, count);
                    let shift = b.ins().band_imm(count, 31);
                    let result = match op {
                        0x78 => b.ins().ishl(value, shift),
                        0x7a => b.ins().sshr(value, shift),
                        0x7c => b.ins().ushr(value, shift),
                        _ => unreachable!(),
                    };
                    stack.push((b.ins().sextend(I64, result), false, false));
                }
                0x79 | 0x7b | 0x7d => {
                    // lshl/lshr/lushr — long shifted by (int_count & 63).
                    let (cnt, _, _) = stack.pop()?;
                    let (val, _, _) = stack.pop()?;
                    let sh = b.ins().band_imm(cnt, 63);
                    let res = match op {
                        0x79 => b.ins().ishl(val, sh),
                        0x7b => b.ins().sshr(val, sh),
                        0x7d => b.ins().ushr(val, sh),
                        _ => unreachable!(),
                    };
                    stack.push((res, false, true));
                }
                0x85 => {
                    // i2l — the int cell is already sign-extended to i64 ⇒ that IS the
                    // long. Just re-tag as wide.
                    let (v, _, _) = stack.pop()?;
                    stack.push((v, false, true));
                }
                0x91 | 0x92 | 0x93 => {
                    // i2b/i2c/i2s — narrow to the low 8/16 bits, then re-extend into the
                    // sign-extended I64 int cell (i2c zero-extends, i2b/i2s sign-extend).
                    let (v, _, _) = stack.pop()?;
                    let res = match op {
                        0x91 => {
                            let lo = b.ins().ireduce(I8, v);
                            b.ins().sextend(I64, lo)
                        }
                        0x92 => {
                            let lo = b.ins().ireduce(I16, v);
                            b.ins().uextend(I64, lo)
                        }
                        _ => {
                            let lo = b.ins().ireduce(I16, v);
                            b.ins().sextend(I64, lo)
                        }
                    };
                    stack.push((res, false, false));
                }
                0x88 => {
                    // l2i — truncate to 32 bits, sign-extend back into the cell.
                    let (v, _, _) = stack.pop()?;
                    let lo = b.ins().ireduce(I32, v);
                    stack.push((b.ins().sextend(I64, lo), false, false));
                }
                0x94 => {
                    // lcmp — push -1/0/1 (int) for a<b / a==b / a>b.
                    let (c, _, _) = stack.pop()?;
                    let (a, _, _) = stack.pop()?;
                    let lt = b.ins().icmp(IntCC::SignedLessThan, a, c);
                    let gt = b.ins().icmp(IntCC::SignedGreaterThan, a, c);
                    let neg1 = b.ins().iconst(I64, -1);
                    let zero = b.ins().iconst(I64, 0);
                    let one = b.ins().iconst(I64, 1);
                    let hi = b.ins().select(gt, one, zero);
                    let res = b.ins().select(lt, neg1, hi);
                    stack.push((res, false, false));
                }
                // ── DOUBLE (category-2, cell = f64 bits) ──────────────────────────────
                0x0e | 0x0f => {
                    // dconst_0/1
                    let f = b.ins().f64const((op - 0x0e) as f64);
                    stack.push((b.ins().bitcast(I64, MemFlags::new(), f), false, true));
                }
                0x18 => stack.push((load_ref(b, bc[pc + 1] as u32, locals), false, true)), // dload
                0x26..=0x29 => stack.push((load_ref(b, op as u32 - 0x26, locals), false, true)), // dload_0..3
                0x39 => {
                    let (v, _, _) = stack.pop()?;
                    store(b, bc[pc + 1] as u32, v, true, locals);
                }
                0x47..=0x4a => {
                    let (v, _, _) = stack.pop()?;
                    store(b, op as u32 - 0x47, v, true, locals);
                }
                0x63 | 0x67 | 0x6b | 0x6f => {
                    // dadd/dsub/dmul/ddiv — bitcast cells → f64, op, bitcast back.
                    let (rc, _, _) = stack.pop()?;
                    let (ra, _, _) = stack.pop()?;
                    let a = b.ins().bitcast(F64, MemFlags::new(), ra);
                    let c = b.ins().bitcast(F64, MemFlags::new(), rc);
                    let r = match op {
                        0x63 => b.ins().fadd(a, c),
                        0x67 => b.ins().fsub(a, c),
                        0x6b => b.ins().fmul(a, c),
                        _ => b.ins().fdiv(a, c),
                    };
                    stack.push((b.ins().bitcast(I64, MemFlags::new(), r), false, true));
                }
                0x77 => {
                    // dneg
                    let (v, _, _) = stack.pop()?;
                    let f = b.ins().bitcast(F64, MemFlags::new(), v);
                    let n = b.ins().fneg(f);
                    stack.push((b.ins().bitcast(I64, MemFlags::new(), n), false, true));
                }
                0x97 | 0x98 => {
                    // dcmpl/dcmpg → -1/0/1 (NaN → -1 for dcmpl, +1 for dcmpg).
                    let (rc, _, _) = stack.pop()?;
                    let (ra, _, _) = stack.pop()?;
                    let a = b.ins().bitcast(F64, MemFlags::new(), ra);
                    let c = b.ins().bitcast(F64, MemFlags::new(), rc);
                    let res = fcmp_result(b, a, c, op == 0x98);
                    stack.push((res, false, false));
                }
                0x31 => {
                    // daload — rava_aload returns the double element bits as i64.
                    let (idx, _, _) = stack.pop()?;
                    let (arr, _, _) = stack.pop()?;
                    let isr = b.ins().iconst(I64, 0);
                    let call = b.ins().call(cb.aload, &[arr, idx, isr]);
                    call_fault::emit(b, cb);
                    stack.push((b.inst_results(call)[0], false, true));
                }
                0xaf => {
                    // dreturn — the cell is the f64 bits.
                    let (v, _, _) = stack.pop()?;
                    b.ins().return_(&[v]);
                    terminated = true;
                    break;
                }
                // ── conversions int/long/double ──────────────────────────────────────
                0x87 => {
                    // i2d — sign-extended int cell → f64 bits.
                    let (v, _, _) = stack.pop()?;
                    let i = b.ins().ireduce(I32, v);
                    let f = b.ins().fcvt_from_sint(F64, i);
                    stack.push((b.ins().bitcast(I64, MemFlags::new(), f), false, true));
                }
                0x8a => {
                    // l2d — long cell → f64 bits.
                    let (v, _, _) = stack.pop()?;
                    let f = b.ins().fcvt_from_sint(F64, v);
                    stack.push((b.ins().bitcast(I64, MemFlags::new(), f), false, true));
                }
                0x8e => {
                    // d2i — f64 → int (saturating like Java: Cranelift fcvt_to_sint_sat).
                    let (v, _, _) = stack.pop()?;
                    let f = b.ins().bitcast(F64, MemFlags::new(), v);
                    let i = b.ins().fcvt_to_sint_sat(I32, f);
                    stack.push((b.ins().sextend(I64, i), false, false));
                }
                0x8f => {
                    // d2l — f64 → long.
                    let (v, _, _) = stack.pop()?;
                    let f = b.ins().bitcast(F64, MemFlags::new(), v);
                    stack.push((b.ins().fcvt_to_sint_sat(I64, f), false, true));
                }
                // ── FLOAT (category-1, cell = f32 bits in low 32) ─────────────────────
                0x0b | 0x0c | 0x0d => {
                    // fconst_0/1/2
                    let f = b.ins().f32const((op - 0x0b) as f32);
                    let i = b.ins().bitcast(I32, MemFlags::new(), f);
                    stack.push((b.ins().uextend(I64, i), false, false));
                }
                0x17 | 0x22..=0x25 | 0x38 | 0x43..=0x46 | 0x62 | 0x66 | 0x6a | 0x6e | 0x72 | 0x73 | 0x76 | 0x95 | 0x96 | 0x86 | 0x89 | 0x8b | 0x8c | 0x8d | 0x90 | 0xae | 0x30 | 0x2f | 0xad | 0x84 | 0xb4 | 0xb2 | 0xb5 | 0xb6 | 0xb7 | 0xb8 | 0xb9 | 0xba | 0x12 | 0x13 | 0x14 | 0xbb | 0xbd | 0xbc | 0xc1 | 0x53 | 0x4f | 0x50 | 0x51 | 0x52 | 0x54 | 0x55 | 0x56 | 0xbf | 0xbe | 0x2e | 0x32 | 0x33 | 0x34 | 0x35 | 0xc0 | 0x5f | 0x5a | 0x99..=0xa4 | 0xc6 | 0xc7 | 0xa5 | 0xa6 | 0xa7 | 0xaa | 0xab | 0xac | 0xb0 | 0xb1 => emit_op_rest(b, bc, pc, op, &mut stack, locals, consts_ptr, max_locals, cb, argbuf, consts, field_of, method_of, indy_of, ldc_of, leader_set, blocks, &mut terminated, len, &mut flow)?,
                _ => return None,
            }
            pc += len;
        }
        if !terminated {
            let next = leaders.get(li + 1).copied()?;
            let next = *leader_set.get(&next)?;
            let args = flow.edge(b, blocks, next, &stack)?;
            b.ins().jump(blocks[next], &args);
        }
    }
    if !flow.complete() { return None; }
    for &blk in blocks {
        b.seal_block(blk);
    }
    Some(())
}
