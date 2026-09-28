//! The last third of the AOT opcode translation.

use crate::aot_clif::*;
use crate::cnp::ConstDesc;


#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_op_rest2(
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
    let putfield_fn = cb.putfield;
    match op {
        0xba => {
            // invokedynamic: marshal ncells DYNAMIC args (no receiver) into the
            // buffer, call rava_indy(caller_ptr, cp, buf, n), push the call-site
            // result (a lambda ref / concatenated string / boxed primitive).
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let info = indy_of(idx)?;
            if info.ncells > 64 || (info.ncells as usize) > stack.len() {
                return None;
            }
            for i in (0..info.ncells).rev() {
                let (v, _, _) = stack.pop()?;
                b.ins().stack_store(v, argbuf, (i * 8) as i32);
            }
            let buf = b.ins().stack_addr(I64, argbuf, 0);
            let caller_ptr = load_const(b, 0, consts_ptr);
            let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
            let cp = load_const(b, cp_slot, consts_ptr);
            let nv = b.ins().iconst(I64, info.ncells as i64);
            spill(b, &stack, locals, max_locals);
            let call = b.ins().call(cb.indy, &[caller_ptr, cp, buf, nv]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            if !info.ret_void {
                let r = b.inst_results(call)[0];
                stack.push((r, info.ret_ref, info.ret_wide));
            }
        }
        0x12 | 0x13 => {
            // ldc / ldc_w. A REFERENCE constant (String/Class) goes through
            // rava_ldc (reference-only); a NUMERIC constant is emitted INLINE
            // (rava_ldc doesn't handle numerics). ldc_of=None → bail.
            let idx = if op == 0x12 { bc[pc + 1] as u16 } else { ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16 };
            match ldc_of(idx)? {
                LdcConst::Str(text) => {
                    // Interned PERM-pool address from the const table (stable,
                    // identity-consistent) — NO rava_ldc, matching the JIT.
                    let slot = intern_const(consts, ConstDesc::StrLit(text));
                    stack.push((load_const(b, slot, consts_ptr), true, false));
                }
                LdcConst::Class => {
                    let caller_ptr = load_const(b, 0, consts_ptr);
                    let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
                    let cp = load_const(b, cp_slot, consts_ptr);
                    spill(b, &stack, locals, max_locals);
                    let call = b.ins().call(cb.ldc, &[caller_ptr, cp]);
                    call_fault::emit(b, cb);
                    reload(b, &mut stack, locals, max_locals);
                    stack.push((b.inst_results(call)[0], true, false));
                }
                LdcConst::Int(v) => stack.push((b.ins().iconst(I64, v as i64), false, false)),
                LdcConst::Float(bits) => {
                    // Float pushed as its 32 bits zero-extended into the cell.
                    stack.push((b.ins().iconst(I64, bits as i64), false, false))
                }
                // A narrow ldc naming a long/double constant is malformed — bail.
                LdcConst::Long(_) | LdcConst::Double(_) => return None,
            }
        }
        0x14 => {
            // ldc2_w — a wide (long/double) constant. Long → the i64 value; double →
            // its raw 64 bits (the stack stores a double as its bit-pattern, matching
            // dconst's bitcast). Both push a WIDE cell.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            match ldc_of(idx)? {
                LdcConst::Long(v) => stack.push((b.ins().iconst(I64, v), false, true)),
                LdcConst::Double(bits) => stack.push((b.ins().iconst(I64, bits as i64), false, true)),
                _ => return None,
            }
        }
        0xbb => {
            // new: rava_new(caller_ptr, cp) → a fresh ref.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let caller_ptr = load_const(b, 0, consts_ptr);
            let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
            let cp = load_const(b, cp_slot, consts_ptr);
            spill(b, &stack, locals, max_locals);
            let call = b.ins().call(cb.new, &[caller_ptr, cp]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            stack.push((b.inst_results(call)[0], true, false));
        }
        0xbd => {
            // anewarray: rava_anewarray(caller_ptr, cp, count) → fresh ref array.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let (count, _, _) = stack.pop()?;
            let caller_ptr = load_const(b, 0, consts_ptr);
            let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
            let cp = load_const(b, cp_slot, consts_ptr);
            spill(b, &stack, locals, max_locals);
            let call = b.ins().call(cb.anewarray, &[caller_ptr, cp, count]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            stack.push((b.inst_results(call)[0], true, false));
        }
        0xbc => {
            // newarray: rava_newarray(atype, count) → fresh primitive array. atype is
            // the 1-byte operand (4 boolean … 11 long). Allocates → spill/reload.
            let atype = b.ins().iconst(I64, bc[pc + 1] as i64);
            let (count, _, _) = stack.pop()?;
            spill(b, &stack, locals, max_locals);
            let call = b.ins().call(cb.newarray, &[atype, count]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            stack.push((b.inst_results(call)[0], true, false));
        }
        0xc1 => {
            // instanceof: rava_instanceof(caller_ptr, cp, val) → int 0/1.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let (val, _, _) = stack.pop()?;
            let caller_ptr = load_const(b, 0, consts_ptr);
            let cp_slot = intern_const(consts, ConstDesc::Imm(idx as i64));
            let cp = load_const(b, cp_slot, consts_ptr);
            spill(b, &stack, locals, max_locals);
            let call = b.ins().call(cb.instanceof, &[caller_ptr, cp, val]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
            stack.push((b.inst_results(call)[0], false, false));
        }
        0x53 => {
            // aastore: rava_aastore(arr, idx, val) — ref array store, no push.
            let (val, _, _) = stack.pop()?;
            let (idx, _, _) = stack.pop()?;
            let (arr, _, _) = stack.pop()?;
            spill(b, &stack, locals, max_locals);
            b.ins().call(cb.aastore, &[arr, idx, val]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
        }
        0x4f | 0x50 | 0x51 | 0x52 | 0x54 | 0x55 | 0x56 => {
            // Preserve the opcode when dispatching packed value bits. A long or
            // floating-point store cannot pass through the int-only callback.
            let (val, _, _) = stack.pop()?;
            let (idx, _, _) = stack.pop()?;
            let (arr, _, _) = stack.pop()?;
            let opcode = b.ins().iconst(I64, i64::from(op));
            b.ins().call(cb.iastore, &[arr, idx, val, opcode]);
            call_fault::emit(b, cb);
        }
        0xbf => {
            // athrow: rava_throw(exc) sets jit_pending; the method then runs to
            // completion and the caller propagates. Terminate with a dummy return.
            let (exc, _, _) = stack.pop()?;
            b.ins().call(cb.throw, &[exc]);
            let z = b.ins().iconst(I64, 0);
            b.ins().return_(&[z]);
            *terminated = true;
            return Some(());
        }
        0xbe => {
            // arraylength: rava_arraylength(arr) → int.
            let (arr, _, _) = stack.pop()?;
            let call = b.ins().call(cb.arraylength, &[arr]);
            stack.push((b.inst_results(call)[0], false, false));
        }
        0x2e | 0x32 | 0x33 | 0x34 | 0x35 => {
            // iaload/aaload/baload/caload/saload: rava_aload(arr, idx, is_ref).
            // Int-family + ref loads (32-bit cell). laload 0x2f handled above;
            // faload/daload (float/double 0x30/0x31) aren't in any arm → bail.
            let is_ref = op == 0x32;
            let (idx, _, _) = stack.pop()?;
            let (arr, _, _) = stack.pop()?;
            let isr = b.ins().iconst(I64, is_ref as i64);
            let call = b.ins().call(cb.aload, &[arr, idx, isr]);
            call_fault::emit(b, cb);
            stack.push((b.inst_results(call)[0], is_ref, false));
        }
        0xc0 => {
            let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
            let value = stack.last()?.0;
            let caller = load_const(b, 0, consts_ptr);
            let cp = b.ins().iconst(I64, i64::from(idx));
            spill(b, &stack, locals, max_locals);
            b.ins().call(cb.checkcast, &[caller, cp, value]);
            call_fault::emit(b, cb);
            reload(b, &mut stack, locals, max_locals);
        }
        0x5f => {
            // swap (category-1 only — both narrow)
            let a = stack.pop()?;
            let c = stack.pop()?;
            stack.push(a);
            stack.push(c);
        }
        0x5a => {
            // dup_x1
            let a = stack.pop()?;
            let c = stack.pop()?;
            stack.push(a);
            stack.push(c);
            stack.push(a);
        }
        0x99..=0xa4 => {
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            let fallthrough = pc + len;
            let (cc, cmp) = cond_of(op);
            let taken = if cmp == CmpKind::Zero {
                let (a, _, _) = stack.pop()?;
                let a32 = b.ins().ireduce(I32, a);
                let z = b.ins().iconst(I32, 0);
                b.ins().icmp(cc, a32, z)
            } else {
                let (rb, _, _) = stack.pop()?;
                let (ra, _, _) = stack.pop()?;
                let a = b.ins().ireduce(I32, ra);
                let c = b.ins().ireduce(I32, rb);
                b.ins().icmp(cc, a, c)
            };
            flow.branch(b, blocks, taken, *leader_set.get(&target)?,
                *leader_set.get(&fallthrough)?, stack)?;
            *terminated = true;
            return Some(());
        }
        0xc6 | 0xc7 => {
            // ifnull / ifnonnull — a null ref cell is 0.
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            let fallthrough = pc + len;
            let (a, _, _) = stack.pop()?;
            let cc = if op == 0xc6 { IntCC::Equal } else { IntCC::NotEqual };
            let z = b.ins().iconst(I64, 0);
            let taken = b.ins().icmp(cc, a, z);
            flow.branch(b, blocks, taken, *leader_set.get(&target)?,
                *leader_set.get(&fallthrough)?, stack)?;
            *terminated = true;
            return Some(());
        }
        0xa5 | 0xa6 => {
            // Distinct ABI boxes can encode the same reference. Use the shared
            // identity operation; it neither allocates nor moves guest objects.
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            let fallthrough = pc + len;
            let (rb, _, _) = stack.pop()?;
            let (ra, _, _) = stack.pop()?;
            let equal = ref_compare::equal(b, cb.acmp, ra, rb);
            let cc = if op == 0xa5 { IntCC::NotEqual } else { IntCC::Equal };
            let taken = b.ins().icmp_imm(cc, equal, 0);
            flow.branch(b, blocks, taken, *leader_set.get(&target)?,
                *leader_set.get(&fallthrough)?, stack)?;
            *terminated = true;
            return Some(());
        }
        0xa7 => {
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            let target = *leader_set.get(&target)?;
            let args = flow.edge(b, blocks, target, stack)?;
            b.ins().jump(blocks[target], &args);
            *terminated = true;
            return Some(());
        }
        0xaa | 0xab => {
            // tableswitch / lookupswitch — pop the index and carry the remaining
            // operand stack through edge blocks (Switch has no block-argument API).
            // cranelift::frontend::Switch lowers to a jump table or compare chain and
            // covers both forms uniformly. Keys use unsigned 32-bit reinterpretation
            // so negative case labels match (identical to the JIT's switch emit).
            let (idx_raw, _, _) = stack.pop()?;
            let idx = b.ins().ireduce(I32, idx_raw);
            let (default_t, cases) = crate::jit::switch_targets(bc, pc)?;
            let mut sw = cranelift::frontend::Switch::new();
            let mut targets = HashMap::new();
            let mut bridges = Vec::new();
            for target in std::iter::once(&default_t).chain(cases.iter().map(|(_, t)| t)) {
                if targets.contains_key(target) { continue; }
                let target_index = *leader_set.get(target)?;
                let args = flow.edge(b, blocks, target_index, stack)?;
                let block = if args.is_empty() { blocks[target_index] } else {
                    let bridge = b.create_block();
                    bridges.push((bridge, blocks[target_index], args));
                    bridge
                };
                targets.insert(*target, block);
            }
            for (m, t) in &cases {
                sw.set_entry((*m as u32) as u128, targets[t]);
            }
            sw.emit(b, idx, targets[&default_t]);
            for (bridge, target, args) in bridges {
                b.switch_to_block(bridge);
                b.ins().jump(target, &args);
                b.seal_block(bridge);
            }
            *terminated = true;
            return Some(());
        }
        0xac | 0xb0 => {
            // ireturn / areturn — the top cell is the i64 result either way.
            let (v, _, _) = stack.pop()?;
            b.ins().return_(&[v]);
            *terminated = true;
            return Some(());
        }
        0xb1 => {
            // return (void) — the ABI returns i64; a void method's result is
            // ignored by the caller, so return a dummy 0.
            let z = b.ins().iconst(I64, 0);
            b.ins().return_(&[z]);
            *terminated = true;
            return Some(());
        }
        _ => return None,
        _ => return None,
        _ => return None,
    }
    Some(())
}
