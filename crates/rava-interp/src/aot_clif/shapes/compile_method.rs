//! Compiling a whole method through the AOT path.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::aot_clif::shapes::*;

pub(crate) fn emit(
    b: &mut FunctionBuilder,
    bc: &[u8],
    leaders: &[usize],
    leader_set: &HashMap<usize, usize>,
    blocks: &[cranelift::codegen::ir::Block],
    locals: Value,
) -> Option<()> {
    let load_local = |b: &mut FunctionBuilder, idx: u32| -> Value {
        let v = b.ins().load(I32, MemFlags::trusted(), locals, (idx * 8) as i32);
        b.ins().sextend(I64, v)
    };
    let store_local = |b: &mut FunctionBuilder, idx: u32, v: Value| {
        // Sign-extend the low 32 bits into the full i64 slot (matches Value::Int).
        let lo = b.ins().ireduce(I32, v);
        let ext = b.ins().sextend(I64, lo);
        b.ins().store(MemFlags::trusted(), ext, locals, (idx * 8) as i32);
    };

    for (li, &start) in leaders.iter().enumerate() {
        let end = leaders.get(li + 1).copied().unwrap_or(bc.len());
        b.switch_to_block(blocks[li]);
        let mut stack: Vec<Value> = Vec::new();
        let mut pc = start;
        let mut terminated = false;
        while pc < end {
            let op = bc[pc];
            let len = insn_len_at(bc, pc)?;
            match op {
                0x02..=0x08 => stack.push(b.ins().iconst(I64, (op as i64) - 3)),
                0x10 => stack.push(b.ins().iconst(I64, bc[pc + 1] as i8 as i64)),
                0x11 => stack.push(b.ins().iconst(I64, s16(bc, pc + 1) as i64)),
                0x15 => stack.push(load_local(b, bc[pc + 1] as u32)),
                0x1a..=0x1d => stack.push(load_local(b, op as u32 - 0x1a)),
                0x36 => {
                    let v = stack.pop()?;
                    store_local(b, bc[pc + 1] as u32, v);
                }
                0x3b..=0x3e => {
                    let v = stack.pop()?;
                    store_local(b, op as u32 - 0x3b, v);
                }
                0x57 => {
                    stack.pop()?;
                }
                0x59 => {
                    let v = *stack.last()?;
                    stack.push(v);
                }
                0x60 | 0x64 | 0x68 | 0x74 | 0x78 | 0x7a | 0x7c | 0x7e | 0x80 | 0x82 => {
                    // All operate in 32-bit and re-sign-extend so overflow matches Java.
                    let res = if op == 0x74 {
                        let a = stack.pop()?;
                        let a32 = b.ins().ireduce(I32, a);
                        b.ins().ineg(a32)
                    } else {
                        let rb = stack.pop()?;
                        let ra = stack.pop()?;
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
                                let sh = b.ins().band_imm(c, 31);
                                b.ins().ishl(a, sh)
                            }
                            0x7a => {
                                let sh = b.ins().band_imm(c, 31);
                                b.ins().sshr(a, sh)
                            }
                            0x7c => {
                                let sh = b.ins().band_imm(c, 31);
                                b.ins().ushr(a, sh)
                            }
                            _ => unreachable!(),
                        }
                    };
                    stack.push(b.ins().sextend(I64, res));
                }
                0x84 => {
                    let idx = bc[pc + 1] as u32;
                    let c = bc[pc + 2] as i8 as i64;
                    let cur = load_local(b, idx);
                    let cur32 = b.ins().ireduce(I32, cur);
                    let nv = b.ins().iadd_imm(cur32, c);
                    let ext = b.ins().sextend(I64, nv);
                    store_local(b, idx, ext);
                }
                0x99..=0xa4 => {
                    // if<cc> (single operand vs 0) / if_icmp<cc> (two operands).
                    let target = (pc as i32 + s16(bc, pc + 1)) as usize;
                    let fallthrough = pc + len;
                    let (cc, cmp) = cond_of(op);
                    let taken = if cmp == CmpKind::Zero {
                        let a = stack.pop()?;
                        let a32 = b.ins().ireduce(I32, a);
                        let z = b.ins().iconst(I32, 0);
                        b.ins().icmp(cc, a32, z)
                    } else {
                        let rb = stack.pop()?;
                        let ra = stack.pop()?;
                        let a = b.ins().ireduce(I32, ra);
                        let c = b.ins().ireduce(I32, rb);
                        b.ins().icmp(cc, a, c)
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
                    let target = (pc as i32 + s16(bc, pc + 1)) as usize;
                    if !stack.is_empty() {
                        return None;
                    }
                    b.ins().jump(blocks[*leader_set.get(&target)?], &[]);
                    terminated = true;
                    break;
                }
                0xac => {
                    let v = stack.pop()?;
                    b.ins().return_(&[v]);
                    terminated = true;
                    break;
                }
                _ => return None,
            }
            pc += len;
        }
        // A block that ran off its end without a terminator falls through to the next
        // leader — emit the jump (operand stack must be empty at the boundary).
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

/// A4 — the unified per-opcode translator. Drives one pass over real bytecode, emitting
/// numeric ops inline (compile_numeric's core) AND callback ops via the proven templates
/// (compile_call_shape), building the per-method symbolic constant table as it goes. The
/// operand stack is TYPED (int vs reference cell) so a getfield receiver (a ref) and an
/// arithmetic value (an int) are threaded correctly. Bails to None on any opcode outside
/// the covered set (fail-closed → the method stays interpreted/JIT'd). This is the
/// assembly the boot-relevant AOT tier runs; coverage grows opcode-by-opcode.
///
/// `field_of(cp)` resolves a getfield/putfield cp index to `(owner, name, is_ref)` —
/// supplied by the caller (the VM's constant pool). Returns code + relocs + the const
/// descriptor table; the caller persists via `CompiledObj::from_full` and links with
/// `link_full` (resolving each descriptor to a per-process id).
pub fn compile_method(
    bc: &[u8],
    caller_class: &str,
    max_locals: u32,
    field_of: &dyn Fn(u16) -> Option<(String, String, FieldKind)>,
    method_of: &dyn Fn(u16) -> Option<InvokeInfo>,
    indy_of: &dyn Fn(u16) -> Option<InvokeInfo>,
    // ldc constant kind: Ref (String/Class → rava_ldc) or an inline Int/Float. None →
    // bail. CRITICAL: rava_ldc is REFERENCE-ONLY; a numeric ldc must be emitted inline,
    // and marking it as a ref would spill a fake handle → cell_to_root roots garbage.
    ldc_of: &dyn Fn(u16) -> Option<LdcConst>,
) -> Option<(Vec<u8>, Vec<(u32, u32, i64)>, Vec<crate::cnp::ConstDesc>)> {
    use crate::cnp::ConstDesc;
    use cranelift::codegen::binemit::Reloc;
    use cranelift::codegen::ir::{
        ExtFuncData, ExternalName, StackSlotData, StackSlotKind, UserExternalName,
    };

    let getfield_sym = crate::jit::aot_symtab().iter().position(|&(n, _)| n == "rava_getfield")? as u32;
    let invoke_sym = crate::jit::aot_symtab().iter().position(|&(n, _)| n == "rava_invoke")? as u32;

    let leaders = leaders(bc)?;
    let leader_set: HashMap<usize, usize> = leaders.iter().enumerate().map(|(i, &p)| (p, i)).collect();

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals
    sig.params.push(AbiParam::new(I64)); // consts
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    // Import rava_getfield(obj, fid, is_ref) once.
    let mut gs = Signature::new(isa.default_call_conv());
    for _ in 0..3 {
        gs.params.push(AbiParam::new(I64));
    }
    gs.returns.push(AbiParam::new(I64));
    let gsig = func.import_signature(gs);
    let gname = func.declare_imported_user_function(UserExternalName::new(0, getfield_sym));
    let getfield_fn = func.import_function(ExtFuncData {
        name: ExternalName::user(gname),
        signature: gsig,
        colocated: false,
        patchable: false,
    });
    // Import rava_invoke(caller_ptr, cp, kind, buf, n) once.
    let mut is = Signature::new(isa.default_call_conv());
    for _ in 0..5 {
        is.params.push(AbiParam::new(I64));
    }
    is.returns.push(AbiParam::new(I64));
    let isig = func.import_signature(is);
    let iname = func.declare_imported_user_function(UserExternalName::new(0, invoke_sym));
    let invoke_fn = func.import_function(ExtFuncData {
        name: ExternalName::user(iname),
        signature: isig,
        colocated: false,
        patchable: false,
    });
    // Import the remaining callbacks: ldc/new share (caller_ptr, cp)->i64; arraylength
    // is (arr)->i64. Symbol indices from aot_symtab.
    let import_n = |func: &mut Function, sym_name: &str, nargs: usize| -> Option<cranelift::codegen::ir::FuncRef> {
        let sym = crate::jit::aot_symtab().iter().position(|&(n, _)| n == sym_name)? as u32;
        let mut s = Signature::new(isa.default_call_conv());
        for _ in 0..nargs {
            s.params.push(AbiParam::new(I64));
        }
        s.returns.push(AbiParam::new(I64));
        let sr = func.import_signature(s);
        let nm = func.declare_imported_user_function(UserExternalName::new(0, sym));
        Some(func.import_function(ExtFuncData {
            name: ExternalName::user(nm),
            signature: sr,
            colocated: false,
            patchable: false,
        }))
    };
    let acmp_fn = import_n(&mut func, "rava_acmp", 2)?;
    let ldc_fn = import_n(&mut func, "rava_ldc", 2)?;
    let new_fn = import_n(&mut func, "rava_new", 2)?;
    let alen_fn = import_n(&mut func, "rava_arraylength", 1)?;
    let putfield_fn = import_n(&mut func, "rava_putfield", 4)?;
    let aload_fn = import_n(&mut func, "rava_aload", 3)?;
    let getstatic_fn = import_n(&mut func, "rava_getstatic", 3)?;
    let indy_fn = import_n(&mut func, "rava_indy", 4)?;
    let throw_fn = import_n(&mut func, "rava_throw", 1)?;
    let aastore_fn = import_n(&mut func, "rava_aastore", 3)?;
    let anewarray_fn = import_n(&mut func, "rava_anewarray", 3)?;
    let newarray_fn = import_n(&mut func, "rava_newarray", 2)?;
    let iastore_fn = import_n(&mut func, "rava_aot_primitive_store", 4)?;
    let divrem_fn = import_n(&mut func, "rava_i32_divrem", 3)?;
    let divrem64_fn = import_n(&mut func, "rava_i64_divrem", 3)?;
    // Float-typed libcall import (frem/drem): (ty, ty) -> ty, ty = F32 or F64.
    let import_f = |func: &mut Function, sym_name: &str, ty: cranelift::codegen::ir::Type| -> Option<cranelift::codegen::ir::FuncRef> {
        let sym = crate::jit::aot_symtab().iter().position(|&(n, _)| n == sym_name)? as u32;
        let mut s = Signature::new(isa.default_call_conv());
        s.params.push(AbiParam::new(ty));
        s.params.push(AbiParam::new(ty));
        s.returns.push(AbiParam::new(ty));
        let sr = func.import_signature(s);
        let nm = func.declare_imported_user_function(UserExternalName::new(0, sym));
        Some(func.import_function(ExtFuncData {
            name: ExternalName::user(nm),
            signature: sr,
            colocated: false,
            patchable: false,
        }))
    };
    let f32rem_fn = import_f(&mut func, "rava_f32_rem", F32)?;
    let f64rem_fn = import_f(&mut func, "rava_f64_rem", F64)?;
    let instanceof_fn = import_n(&mut func, "rava_instanceof", 3)?;
    let checkcast_fn = import_n(&mut func, "rava_aot_checkcast", 3)?;
    let null_fault_fn = import_n(&mut func, "rava_aot_null_fault", 4)?;
    let pending_fn = import_n(&mut func, "rava_aot_pending", 0)?;
    let progress_fn = import_n(&mut func, "rava_aot_progress", 0)?;
    let budget = func.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));

    // Args buffer for invoke marshalling (max operand cells a call consumes — bounded by
    // max_stack; a fixed 64-cell slot covers real methods, else bail).
    let argbuf = func.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 64 * 8, 3));
    let cb = Callbacks { pending: pending_fn, acmp: acmp_fn, getfield: getfield_fn, invoke: invoke_fn, ldc: ldc_fn, new: new_fn, arraylength: alen_fn, putfield: putfield_fn, aload: aload_fn, getstatic: getstatic_fn, throw: throw_fn, aastore: aastore_fn, iastore: iastore_fn, divrem: divrem_fn, divrem64: divrem64_fn, f32rem: f32rem_fn, f64rem: f64rem_fn, anewarray: anewarray_fn, newarray: newarray_fn, instanceof: instanceof_fn, checkcast: checkcast_fn, null_fault: null_fault_fn, indy: indy_fn };

    // caller_ptr constant (this compiling method's own class) — slot 0, resolved to
    // Arc::as_ptr at link. Reserved up front so its slot is stable.
    let mut consts: Vec<ConstDesc> = vec![ConstDesc::ClassPtr(caller_class.to_string())];

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let locals = b.block_params(entry)[0];
        let consts_ptr = b.block_params(entry)[1];
        let blocks: Vec<_> = leaders.iter().map(|_| b.create_block()).collect();
        b.switch_to_block(entry);
        let initial_budget = b.ins().iconst(I64, progress::INTERVAL);
        b.ins().stack_store(initial_budget, budget, 0);
        b.ins().jump(blocks[0], &[]);
        b.seal_block(entry);
        if emit_method(
            &mut b, bc, &leaders, &leader_set, &blocks, locals, consts_ptr, &cb, argbuf,
            max_locals, &mut consts, field_of, method_of, indy_of, ldc_of, budget, progress_fn,
        )
        .is_none()
        {
            return None;
        }
        b.finalize();
    }

    // Capture the user-named-function table (ref → (namespace, index)) BEFORE moving
    // func into the Context, so the reloc loop can map each target to its symbol index.
    let unf = func.params.user_named_funcs().clone();

    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    let mut relocs = Vec::new();
    for r in compiled.buffer.relocs() {
        if !matches!(r.kind, Reloc::Abs8) {
            return None;
        }
        let sym = match &r.target {
            cranelift::codegen::FinalizedRelocTarget::ExternalName(ExternalName::User(uref)) => {
                unf[*uref].index
            }
            _ => return None,
        };
        relocs.push((r.offset, sym, r.addend));
    }
    Some((compiled.code_buffer().to_vec(), relocs, consts))
}
