//! The small pre-shaped compilations: local calls, getfield, static invokes.
//!
//! Re-exported from the parent module, so no call site outside it changed.


mod compile_method;
pub(crate) use compile_method::*;
mod compile_native_leaf;
pub(crate) use compile_native_leaf::*;
use crate::aot_clif::*;

/// Compile a single-block method that calls ONE runtime symbol (index `sym`, ABI
/// `extern "C" fn(i64)->i64`) on `locals[0]` and returns the result — the tracer that
/// proves the RELOCATABLE persist/link path end-to-end. It emits an Abs8 reloc to the
/// symbol (resolved to a per-process address by `CompiledObj::link_with`), the exact
/// mechanism a callback-carrying compiled method needs. Generalizing the per-opcode
/// translator (compile_numeric) to emit these call sites is the remaining tier work;
/// this locks down the format + reloc + link half first.
pub fn compile_call_local0(sym: u32) -> Option<CompiledObj> {
    use cranelift::codegen::binemit::Reloc;
    use cranelift::codegen::ir::{ExtFuncData, ExternalName, UserExternalName};

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals base
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    let mut callee_sig = Signature::new(isa.default_call_conv());
    callee_sig.params.push(AbiParam::new(I64));
    callee_sig.returns.push(AbiParam::new(I64));
    let sigref = func.import_signature(callee_sig);
    // UserExternalName(namespace=0, index=sym): the index IS our symbol number, which
    // comes back verbatim in the reloc so link_with can resolve it.
    let name = func.declare_imported_user_function(UserExternalName::new(0, sym));
    let extfn = func.import_function(ExtFuncData {
        name: ExternalName::user(name),
        signature: sigref,
        colocated: false,
        patchable: false,
    });

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let locals = b.block_params(blk)[0];
        let arg = b.ins().load(I64, MemFlags::trusted(), locals, 0);
        let call = b.ins().call(extfn, &[arg]);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.finalize();
    }

    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    // Exactly one imported symbol (`sym`) → each Abs8 reloc targets it. (The general
    // multi-symbol tier will read `r.target`'s UserExternalName index; here `sym` is
    // known and unique, so recording it directly is correct + version-agnostic.)
    let mut relocs: Vec<(u32, u32, i64)> = Vec::new();
    for r in compiled.buffer.relocs() {
        if !matches!(r.kind, Reloc::Abs8) {
            return None;
        }
        relocs.push((r.offset, sym, r.addend));
    }
    Some(CompiledObj::from_code_relocs(compiled.code_buffer().to_vec(), relocs))
}

/// A3 — first callback opcode: emit `getfield` using BOTH mechanisms together. Compiles
/// the `Object f(this){ return this.field }` shape: recv = locals[0], field_id from the
/// per-method constant table (slot 0, a `ConstDesc::FieldId` the linker resolves), then
/// call `rava_getfield(recv, field_id, is_ref)` via an Abs8 reloc to the AOT symbol
/// table. ABI: `extern "C" fn(locals: *mut i64, consts: *const i64) -> i64`. `getfield_sym`
/// is the symbol index of `rava_getfield` (0 in jit::aot_symtab; parameterized so the
/// test can substitute a stub without a live VM). Returns code + its ONE reloc; the
/// caller attaches the `FieldId` const descriptor via `CompiledObj::from_full`.
pub fn compile_getfield(getfield_sym: u32, is_ref: bool) -> Option<(Vec<u8>, Vec<(u32, u32, i64)>)> {
    use cranelift::codegen::binemit::Reloc;
    use cranelift::codegen::ir::{ExtFuncData, ExternalName, UserExternalName};

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals
    sig.params.push(AbiParam::new(I64)); // consts
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    // rava_getfield(obj, field_id, is_ref) -> i64
    let mut cs = Signature::new(isa.default_call_conv());
    for _ in 0..3 {
        cs.params.push(AbiParam::new(I64));
    }
    cs.returns.push(AbiParam::new(I64));
    let sigref = func.import_signature(cs);
    let name = func.declare_imported_user_function(UserExternalName::new(0, getfield_sym));
    let extfn = func.import_function(ExtFuncData {
        name: ExternalName::user(name),
        signature: sigref,
        colocated: false,
        patchable: false,
    });

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let locals = b.block_params(blk)[0];
        let consts = b.block_params(blk)[1];
        let recv = b.ins().load(I64, MemFlags::trusted(), locals, 0); // this = locals[0]
        let fid = b.ins().load(I64, MemFlags::trusted(), consts, 0); // const slot 0 = field_id
        let isr = b.ins().iconst(I64, is_ref as i64);
        let call = b.ins().call(extfn, &[recv, fid, isr]);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.finalize();
    }
    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    let mut relocs = Vec::new();
    for r in compiled.buffer.relocs() {
        if !matches!(r.kind, Reloc::Abs8) {
            return None;
        }
        relocs.push((r.offset, getfield_sym, r.addend));
    }
    Some((compiled.code_buffer().to_vec(), relocs))
}

/// Compile a single-block method that calls symbol `sym` with `args` (each Local/Const/
/// Imm) and returns its i64 result. ABI `extern "C" fn(locals: *mut i64, consts: *const
/// i64) -> i64`. This is the shared template every simple callback opcode uses; the
/// caller attaches the matching const descriptors. Returns code + its ONE Abs8 reloc.
pub fn compile_call_shape(sym: u32, args: &[ArgSrc]) -> Option<(Vec<u8>, Vec<(u32, u32, i64)>)> {
    use cranelift::codegen::binemit::Reloc;
    use cranelift::codegen::ir::{ExtFuncData, ExternalName, UserExternalName};

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals
    sig.params.push(AbiParam::new(I64)); // consts
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    let mut cs = Signature::new(isa.default_call_conv());
    for _ in 0..args.len() {
        cs.params.push(AbiParam::new(I64));
    }
    cs.returns.push(AbiParam::new(I64));
    let sigref = func.import_signature(cs);
    let name = func.declare_imported_user_function(UserExternalName::new(0, sym));
    let extfn = func.import_function(ExtFuncData {
        name: ExternalName::user(name),
        signature: sigref,
        colocated: false,
        patchable: false,
    });

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let locals = b.block_params(blk)[0];
        let consts = b.block_params(blk)[1];
        let argvals: Vec<Value> = args
            .iter()
            .map(|a| match a {
                ArgSrc::Local(i) => b.ins().load(I64, MemFlags::trusted(), locals, (*i * 8) as i32),
                ArgSrc::Const(s) => b.ins().load(I64, MemFlags::trusted(), consts, (*s * 8) as i32),
                ArgSrc::Imm(v) => b.ins().iconst(I64, *v),
            })
            .collect();
        let call = b.ins().call(extfn, &argvals);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.finalize();
    }
    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    let mut relocs = Vec::new();
    for r in compiled.buffer.relocs() {
        if !matches!(r.kind, Reloc::Abs8) {
            return None;
        }
        relocs.push((r.offset, sym, r.addend));
    }
    Some((compiled.code_buffer().to_vec(), relocs))
}

/// A3.2 — the hardest callback opcode: `invokestatic` with argument marshalling. Emits
/// the `X f(a, b) { return callee(a, b) }` shape — the two args (locals[0], locals[1],
/// standing in for operand-stack values) are stored into a Cranelift stack-slot buffer,
/// then `rava_invoke(caller_ptr, cp, kind, buf, n)` is called via an Abs8 reloc. The
/// caller_ptr (the baked class pointer) comes from const-table slot 0 (`ConstDesc::
/// ClassPtr`, resolved per-process to `Arc::as_ptr`); cp from slot 1 (`ConstDesc::Imm`).
/// This proves the args-buffer + const-table + reloc composition every invoke follows.
pub fn compile_invokestatic_2(invoke_sym: u32, kind: i64) -> Option<(Vec<u8>, Vec<(u32, u32, i64)>)> {
    use cranelift::codegen::binemit::Reloc;
    use cranelift::codegen::ir::{
        ExtFuncData, ExternalName, StackSlotData, StackSlotKind, UserExternalName,
    };

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals
    sig.params.push(AbiParam::new(I64)); // consts
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    // rava_invoke(caller_ptr, cp, kind, buf, n) -> i64
    let mut cs = Signature::new(isa.default_call_conv());
    for _ in 0..5 {
        cs.params.push(AbiParam::new(I64));
    }
    cs.returns.push(AbiParam::new(I64));
    let sigref = func.import_signature(cs);
    let name = func.declare_imported_user_function(UserExternalName::new(0, invoke_sym));
    let extfn = func.import_function(ExtFuncData {
        name: ExternalName::user(name),
        signature: sigref,
        colocated: false,
        patchable: false,
    });
    // Args buffer: 2 i64 cells.
    let ss = func.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 16, 3));

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let locals = b.block_params(blk)[0];
        let consts = b.block_params(blk)[1];
        // Marshal the two operands into the buffer.
        let a0 = b.ins().load(I64, MemFlags::trusted(), locals, 0);
        let a1 = b.ins().load(I64, MemFlags::trusted(), locals, 8);
        b.ins().stack_store(a0, ss, 0);
        b.ins().stack_store(a1, ss, 8);
        let buf = b.ins().stack_addr(I64, ss, 0);
        let caller_ptr = b.ins().load(I64, MemFlags::trusted(), consts, 0); // ClassPtr
        let cp = b.ins().load(I64, MemFlags::trusted(), consts, 8); // Imm(cp)
        let kindv = b.ins().iconst(I64, kind);
        let n = b.ins().iconst(I64, 2);
        let call = b.ins().call(extfn, &[caller_ptr, cp, kindv, buf, n]);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.finalize();
    }
    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    let mut relocs = Vec::new();
    for r in compiled.buffer.relocs() {
        if !matches!(r.kind, Reloc::Abs8) {
            return None;
        }
        relocs.push((r.offset, invoke_sym, r.addend));
    }
    Some((compiled.code_buffer().to_vec(), relocs))
}
