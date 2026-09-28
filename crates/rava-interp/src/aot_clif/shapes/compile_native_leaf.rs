//! The Tracer 1 direct-leaf compiler: a verified no-GC primitive leaf body as
//! a standalone System V `(hidden consts_ptr: i64, args: i64...) -> i64`
//! function.
//!
//! Re-exported from the shapes module, so no call site outside it changed.

use crate::aot_clif::shapes::*;

// ═══════════════ Direct leaf: System V native-ABI compilation ═══════════════
//
// Tracer 1 — `compile_native_leaf(bytecode, max_locals, descriptor) -> Option<CompiledObj>`.
// Compiles a VERIFIED no-GC primitive leaf (the classifier's
// `is_no_gc_primitive_leaf` gate has already admitted it) as a standalone
// function with the direct ABI `extern "SystemV" fn(hidden consts_ptr: i64,
// args: i64...) -> i64`:
//
//   * the hidden consts pointer is RDI (unused by the leaf — its const table
//     is empty — but present so the call site passes a uniform hidden first
//     argument),
//   * ordinary SysV integer registers carry the user arguments (RSI, RDX, RCX,
//     R8, R9; a leaf with more than five user arguments never reaches this
//     emitter — `PrimitiveSignature::parse` refuses those shapes),
//   * the single i64 result is sign-extended into the caller's int-cell
//     convention (int/boolean results leave the low 32 bits sign-extended;
//     long results are the full i64).
//
// The method's locals start life as its register arguments: the entry block
// stores each incoming argument cell into the locals slot it claims
// (`value * 8` layout, wide longs occupying the pair), so the existing body
// emitter (`emit_leaf_body` in this module) runs unchanged against a real
// locals buffer. The emitted code must be RELOC-FREE — a leaf body is
// callback-free, constant-free bytecode, so any reloc proves the
// classifier/emitter contract broke and the object is refused rather than
// persisted un-linkable.

/// Emit one instruction of a leaf body over the typed cell stack. Mirrors the
/// classifier's `leaf_body_ok` walk exactly (same admitted set, same
/// typed-stack rules, forward-only branches), so a classify-yes body compiles
/// here or the contract is broken — in which case the compile fails closed to
/// None and the caller keeps the method on `rava_invoke`.
fn emit_leaf_body(
    b: &mut FunctionBuilder,
    bc: &[u8],
    leaders: &[usize],
    leader_set: &HashMap<usize, usize>,
    blocks: &[cranelift::codegen::ir::Block],
    locals: Value,
) -> Option<()> {
    use cranelift::codegen::ir::condcodes::IntCC;
    use cranelift::codegen::ir::types::{I32, I64};
    use cranelift::codegen::ir::InstBuilder;

    let load_int_local = |b: &mut FunctionBuilder, idx: u32| -> Value {
        let v = b
            .ins()
            .load(I32, MemFlags::trusted(), locals, (idx * 8) as i32);
        b.ins().sextend(I64, v)
    };
    let load_long_local = |b: &mut FunctionBuilder, idx: u32| -> Value {
        b.ins()
            .load(I64, MemFlags::trusted(), locals, (idx * 8) as i32)
    };
    let cond_of = |op: u8| -> IntCC {
        match op {
            0x99 => IntCC::Equal,
            0x9a => IntCC::NotEqual,
            0x9b => IntCC::SignedLessThan,
            0x9c => IntCC::SignedGreaterThanOrEqual,
            0x9d => IntCC::SignedGreaterThan,
            0x9e => IntCC::SignedLessThanOrEqual,
            0x9f => IntCC::Equal,
            0xa0 => IntCC::NotEqual,
            0xa1 => IntCC::SignedLessThan,
            0xa2 => IntCC::SignedGreaterThanOrEqual,
            0xa3 => IntCC::SignedGreaterThan,
            0xa4 => IntCC::SignedLessThanOrEqual,
            _ => unreachable!(),
        }
    };

    for (li, &start) in leaders.iter().enumerate() {
        let end = leaders.get(li + 1).copied().unwrap_or(bc.len());
        b.switch_to_block(blocks[li]);
        let mut stack: Vec<(Value, bool)> = Vec::new(); // (cell, is_long)
        let mut pc = start;
        let mut terminated = false;
        while pc < end {
            let op = bc[pc];
            let len = insn_len_at(bc, pc)?;
            match op {
                0x00 => {} // nop
                0x02..=0x08 => stack.push((b.ins().iconst(I64, (op as i64) - 3), false)),
                0x0a | 0x0b => stack.push((b.ins().iconst(I64, (op as i64) - 0x0a), true)), // lconst_0/1
                0x10 => stack.push((b.ins().iconst(I64, bc[pc + 1] as i8 as i64), false)),
                0x11 => stack.push((b.ins().iconst(I64, s16(bc, pc + 1) as i64), false)),
                0x15 => stack.push((load_int_local(b, bc[pc + 1] as u32), false)),
                0x16 => stack.push((load_long_local(b, bc[pc + 1] as u32), true)),
                0x1a..=0x1d => stack.push((load_int_local(b, op as u32 - 0x1a), false)),
                0x1e..=0x21 => stack.push((load_long_local(b, op as u32 - 0x1e), true)),
                // int arith — 32-bit ops re-sign-extended (Java wrap semantics).
                0x60 | 0x64 | 0x68 | 0x74 | 0x78 | 0x7a | 0x7c | 0x7e | 0x80 | 0x82 => {
                    let res = if op == 0x74 {
                        let (a, _) = stack.pop()?;
                        let a32 = b.ins().ireduce(I32, a);
                        b.ins().ineg(a32)
                    } else {
                        let (rb, _) = stack.pop()?;
                        let (ra, _) = stack.pop()?;
                        let a = b.ins().ireduce(I32, ra);
                        let c = b.ins().ireduce(I32, rb);
                        match op {
                            0x60 => b.ins().iadd(a, c),
                            0x64 => b.ins().isub(a, c),
                            0x68 => b.ins().imul(a, c),
                            0x7e => b.ins().band(a, c),
                            0x80 => b.ins().bor(a, c),
                            0x82 => b.ins().bxor(a, c),
                            0x78 => {
                                let shift = b.ins().band_imm(c, 31);
                                b.ins().ishl(a, shift)
                            }
                            0x7a => {
                                let shift = b.ins().band_imm(c, 31);
                                b.ins().sshr(a, shift)
                            }
                            0x7c => {
                                let shift = b.ins().band_imm(c, 31);
                                b.ins().ushr(a, shift)
                            }
                            _ => unreachable!(),
                        }
                    };
                    stack.push((b.ins().sextend(I64, res), false));
                }
                // long arith — full i64 cells.
                0x61 | 0x65 | 0x69 | 0x75 | 0x7f | 0x81 | 0x83 => {
                    let res = if op == 0x75 {
                        let (a, _) = stack.pop()?;
                        b.ins().ineg(a)
                    } else {
                        let (rb, _) = stack.pop()?;
                        let (ra, _) = stack.pop()?;
                        match op {
                            0x61 => b.ins().iadd(ra, rb),
                            0x65 => b.ins().isub(ra, rb),
                            0x69 => b.ins().imul(ra, rb),
                            0x7f => b.ins().band(ra, rb),
                            0x81 => b.ins().bor(ra, rb),
                            0x83 => b.ins().bxor(ra, rb),
                            _ => unreachable!(),
                        }
                    };
                    stack.push((res, true));
                }
                0x79 | 0x7b | 0x7d => {
                    // lshl/lshr/lushr — long shifted by (int_count & 63).
                    let (cnt, _) = stack.pop()?;
                    let (val, _) = stack.pop()?;
                    let sh = b.ins().band_imm(cnt, 63);
                    let res = match op {
                        0x79 => b.ins().ishl(val, sh),
                        0x7b => b.ins().sshr(val, sh),
                        _ => b.ins().ushr(val, sh),
                    };
                    stack.push((res, true));
                }
                0x85 => {
                    // i2l — the int cell is already sign-extended ⇒ that IS the long.
                    let (v, _) = stack.pop()?;
                    stack.push((v, true));
                }
                0x88 => {
                    // l2i — truncate to 32 bits, sign-extend into the int cell.
                    let (v, _) = stack.pop()?;
                    let v32 = b.ins().ireduce(I32, v);
                    let v = b.ins().sextend(I64, v32);
                    stack.push((v, false));
                }
                0x94 => {
                    // lcmp — push -1/0/1 (int) for a<b / a==b / a>b.
                    let (rc, _) = stack.pop()?;
                    let (ra, _) = stack.pop()?;
                    let lt = b.ins().icmp(IntCC::SignedLessThan, ra, rc);
                    let gt = b.ins().icmp(IntCC::SignedGreaterThan, ra, rc);
                    let neg1 = b.ins().iconst(I64, -1);
                    let zero = b.ins().iconst(I64, 0);
                    let one = b.ins().iconst(I64, 1);
                    let hi = b.ins().select(gt, one, zero);
                    let res = b.ins().select(lt, neg1, hi);
                    stack.push((res, false));
                }
                0x99..=0xa4 => {
                    // if<cc> (single operand vs 0) / if_icmp<cc> (two operands).
                    let target = (pc as i32 + s16(bc, pc + 1)) as usize;
                    let fallthrough = pc + len;
                    let cc = cond_of(op);
                    let taken = if matches!(op, 0x99..=0x9e) {
                        let (a, _) = stack.pop()?;
                        let a32 = b.ins().ireduce(I32, a);
                        let z = b.ins().iconst(I32, 0);
                        b.ins().icmp(cc, a32, z)
                    } else {
                        let (rb, _) = stack.pop()?;
                        let (ra, _) = stack.pop()?;
                        let ra = b.ins().ireduce(I32, ra);
                        let rb = b.ins().ireduce(I32, rb);
                        b.ins().icmp(cc, ra, rb)
                    };
                    if !stack.is_empty() {
                        return None; // non-empty operand stack across a branch — bail
                    }
                    let tb = blocks[*leader_set.get(&target)?];
                    let fb_ = blocks[*leader_set.get(&fallthrough)?];
                    b.ins().brif(taken, tb, &[], fb_, &[]);
                    terminated = true;
                    break;
                }
                0xa7 => {
                    // goto — forward only (the classifier refuses backward edges).
                    let target = (pc as i32 + s16(bc, pc + 1)) as usize;
                    if !stack.is_empty() {
                        return None;
                    }
                    b.ins().jump(blocks[*leader_set.get(&target)?], &[]);
                    terminated = true;
                    break;
                }
                0xac => {
                    // ireturn — int/boolean results leave sign-extended i64.
                    let (v, _) = stack.pop()?;
                    b.ins().return_(&[v]);
                    terminated = true;
                    break;
                }
                0xad => {
                    // lreturn — the top cell is the full i64 long.
                    let (v, _) = stack.pop()?;
                    b.ins().return_(&[v]);
                    terminated = true;
                    break;
                }
                _ => return None,
            }
            pc += len;
        }
        // A block that ran off its end without a terminator falls through to
        // the next leader — emit the jump (operand stack must be empty at the
        // boundary, matching the classifier's block model).
        if !terminated {
            if !stack.is_empty() {
                return None;
            }
            let next = leaders.get(li + 1).copied()?;
            b.ins().jump(blocks[*leader_set.get(&next)?], &[]);
        }
    }
    for &blk in blocks {
        b.seal_block(blk);
    }
    Some(())
}

/// The classifier calls this after `is_no_gc_primitive_leaf` has admitted the
/// target: compile a leaf body under the direct System V ABI and hand back a
/// reloc-free object. Every unsupported shape (a body the classifier should
/// never have admitted, a decode failure, an unexpected relocation) fails
/// closed to None.
pub fn compile_native_leaf(
    bytecode: &[u8],
    max_locals: u32,
    descriptor: &str,
) -> Option<CompiledObj> {
    use cranelift::codegen::ir::StackSlotData;
    use cranelift::codegen::ir::StackSlotKind;
    use cranelift::codegen::isa::CallConv;

    // The direct ABI is System V by contract (Linux x86_64 tracer); a callee
    // compiled with any other convention would corrupt the caller's frame.
    if <CallConv as std::str::FromStr>::from_str("system_v") != Ok(CallConv::SystemV) {
        return None;
    }

    let leaders = crate::aot_clif::leaders(bytecode)?;
    let leader_set: HashMap<usize, usize> =
        leaders.iter().enumerate().map(|(i, &p)| (p, i)).collect();

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder()
        .ok()?
        .finish(settings::Flags::new(fb))
        .ok()?;

    // Direct ABI: (hidden consts_ptr: i64, args: i64...) -> i64, System V.
    // Arity is bounded by the SysV integer-register budget — the five user
    // registers after RDI. Arity >5 never reaches this emitter
    // (PrimitiveSignature::parse refuses it); the dynamic check below is the
    // fail-closed net for the emitter's own arm. The DESCRIPTOR is
    // authoritative for arity and per-param kinds — count_args would count a
    // long as one cell too, but PrimitiveSignature::parse additionally refuses
    // every non-(I/J/Z) type letter.
    let Some(sig_shape) = crate::aot_clif::direct_leaf::PrimitiveSignature::parse(descriptor)
    else {
        return None;
    };
    let nparams = sig_shape.params.len();
    if nparams > 5 {
        return None;
    }
    // Per-call locals storage is bounded by the ABI, not the class file: the
    // classifier admits ONLY parameter-local loads (no stores/iinc), so slots
    // beyond the parameters are dead. Derive the JVM slot count from the
    // descriptor (a Long takes two JVM slots, its high half unusable) and
    // refuse a class file whose max_locals cannot even hold the parameters —
    // the classifier makes the same check, so this is the emitter's net.
    let parameter_slots: usize = sig_shape
        .params
        .iter()
        .map(|p| {
            if *p == crate::aot_clif::direct_leaf::PrimKind::Long {
                2
            } else {
                1
            }
        })
        .sum();
    if (max_locals as usize) < parameter_slots {
        return None;
    }
    let mut sig = Signature::new(CallConv::SystemV);
    sig.params.push(AbiParam::new(I64)); // hidden consts pointer
    for _ in 0..nparams {
        sig.params.push(AbiParam::new(I64)); // one i64 cell per user arg
    }
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    // The leaf's locals buffer: parameter_slots i64 slots, initialised from
    // the incoming register arguments in the entry block (wide longs occupy
    // the slot pair). The body only ever LOADS parameter slots, so unused
    // extra locals of the class file are irrelevant — allocating them would
    // let a tiny method with a huge max_locals reserve hundreds of KiB per
    // call. A zero-parameter leaf needs no locals slot at all (its body reads
    // no local): `locals` is then a dead pointer never dereferenced, and no
    // zero-sized slot (illegal in Cranelift) is created.
    let locals_bytes = (parameter_slots * 8) as u32;
    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let params: Vec<Value> = b.block_params(entry).to_vec();
        b.switch_to_block(entry);
        // Initialise method locals from the register arguments in descriptor
        // order — a long param consumes the slot pair (its high half is left
        // unwritten, exactly like the classifier's unusable-high-half model).
        // Kinds come from the authoritative PrimitiveSignature parse. A
        // zero-parameter leaf creates no locals slot at all: its body reads no
        // local, and Cranelift forbids zero-sized explicit slots.
        let locals = if parameter_slots == 0 {
            b.ins().iconst(I64, 0)
        } else {
            let slot = b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                locals_bytes,
                3,
            ));
            b.ins().stack_addr(I64, slot, 0)
        };
        let mut slot = 0usize;
        for (k, kind) in sig_shape.params.iter().enumerate() {
            let v = params[1 + k]; // params[0] is the hidden consts pointer
            if *kind == crate::aot_clif::direct_leaf::PrimKind::Long {
                b.ins()
                    .store(MemFlags::trusted(), v, locals, (slot * 8) as i32);
                slot += 2;
            } else {
                let lo = b.ins().ireduce(I32, v);
                let ext = b.ins().sextend(I64, lo);
                b.ins()
                    .store(MemFlags::trusted(), ext, locals, (slot * 8) as i32);
                slot += 1;
            }
        }
        debug_assert_eq!(slot, parameter_slots);
        let blocks: Vec<_> = leaders.iter().map(|_| b.create_block()).collect();
        b.ins().jump(blocks[0], &[]);
        b.seal_block(entry);
        if emit_leaf_body(&mut b, bytecode, &leaders, &leader_set, &blocks, locals).is_none() {
            return None;
        }
        b.finalize();
    }

    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    // A leaf body is callback-free + constant-free ⇒ no relocations. If any
    // appeared (a shape slipped past both the classifier and this emitter),
    // bail rather than persist un-relinkable code.
    if !compiled.buffer.relocs().is_empty() {
        return None;
    }
    Some(CompiledObj::from_code(compiled.code_buffer().to_vec()))
}
