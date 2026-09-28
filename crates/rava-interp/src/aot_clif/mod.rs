//! Cranelift-tier AOT — increment 1: a self-contained numeric bytecode → CLIF
//! compiler that produces RELOCATABLE machine code (via `codegen::Context`, not
//! `cranelift_jit::JITModule`) so it can be serialized to the on-disk AOT cache and
//! linked in a later process. This is the real Cranelift-AOT path the boot lever
//! needs, proven first on the callback-free / constant-free numeric subset — where
//! the code carries NO relocations and NO inline-baked runtime constants, so it is
//! trivially position-independent (the exact property the AOT cache requires).
//!
//! Deliberately ISOLATED from the hot `jit::compile` path (JITModule) so it can be
//! built + tested with zero regression risk to the production JIT; the wider tier
//! (methods with callbacks → Abs8 relocs to a symbol table + a per-method constant
//! table) generalizes from here. The ABI is `extern "C" fn(locals: *mut i64) -> i64`:
//! locals are i64 slots at `[locals + idx*8]`, int values live in the low 32 bits
//! (sign-extended on store), matching the interpreter's `Value::Int` slot layout.



#![cfg(feature = "next-gen-gc")]

use crate::cnp::CompiledObj;
pub(crate) mod cache_key;
pub(crate) mod diagnostics;
mod class_compiler;
mod numeric_leaf;
mod object_leaf;
pub(crate) mod direct_leaf;
pub mod prepared;
mod prepared_archive;
pub(crate) mod runtime_context;
mod null_guard;
mod call_fault;
mod block_stacks;
mod ref_compare;
pub(crate) mod progress;
pub use class_compiler::ClassCompiler;
#[cfg(test)]
mod class_compiler_tests;
#[cfg(all(test, windows, target_arch = "x86_64"))]
mod link_retry_tests;
#[cfg(test)]
mod numeric_leaf_tests;
#[cfg(test)]
mod object_leaf_tests;
#[cfg(test)]
mod null_guard_tests;
#[cfg(test)]
mod arithmetic_fault_tests;
#[cfg(test)]
mod call_fault_tests;
#[cfg(test)]
mod callback_fault_tests;
#[cfg(test)]
mod int_shift_tests;
#[cfg(test)]
mod block_stack_tests;
use cranelift::codegen::ir::{types::F32, types::F64, types::I8, types::I16, types::I32, types::I64, AbiParam, InstBuilder, MemFlags, Signature};
use cranelift::codegen::ir::{Function, UserFuncName};
use cranelift::codegen::{settings, Context};
use cranelift::frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift::prelude::{Configurable, FloatCC, IntCC, Value};
use std::collections::HashMap;

use crate::jit::insn_len_at;

/// Step A1 — the AOT symbol resolver: maps a reloc's symbol index (baked at compile
/// time, order defined by `jit::aot_symtab`) to the callback's address in THIS process.
/// This is the `resolve` passed to `CompiledObj::link_with` for callback-carrying AOT
/// code — the mechanism that lets a persisted method's rava_* calls bind correctly
/// after a serialize→new-process→link cycle.
pub fn resolve_symbol(idx: u32) -> usize {
    crate::jit::aot_symbol_addr(idx)
}

/// Load-or-compile a numeric method through the on-disk AOT cache (RAVA_AOT_DIR), the
/// Cranelift tier's persist path. Distinct `clif` key namespace so Cranelift machine
/// code never collides with the cnp tier's x86 for the same bytecode. Reloc-free
/// (numeric subset), so `link()` needs no resolver. Unset dir → plain compile+link.
pub fn compile_numeric_aot(bc: &[u8]) -> Option<crate::cnp::CompiledFn> {
    let obj = match crate::cnp::aot_dir() {
        Some(dir) => {
            let path = dir.join(format!("clif_{}", crate::cnp::aot_key(bc)));
            if let Ok(bytes) = std::fs::read(&path) {
                if let Some(o) = CompiledObj::from_bytes(&bytes) {
                    if let Some(f) = o.link() {
                        return Some(f);
                    }
                }
            }
            let o = compile_numeric(bc)?;
            let _ = std::fs::write(&path, o.to_bytes());
            o
        }
        None => compile_numeric(bc)?,
    };
    obj.link()
}

/// Is this method a candidate for the numeric AOT path — a pure `(I..)I` static whose
/// body uses only the callback-free / constant-free opcode subset below? A no keeps it
/// on the interpreter/JIT; a yes means `compile_numeric` can produce relocatable code.
pub fn numeric_eligible(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if !supported(op) {
            return false;
        }
        match insn_len_at(bc, pc) {
            Some(l) => pc += l,
            None => return false,
        }
    }
    true
}

/// Public alias of the int-numeric subset predicate for the LLVM-AOT emitter (which
/// mirrors exactly this reloc-free `(I..)I` opcode set).
pub(crate) fn numeric_subset_op(op: u8) -> bool {
    supported(op)
}

fn supported(op: u8) -> bool {
    matches!(op,
        0x02..=0x08          // iconst_m1..5
        | 0x10 | 0x11        // bipush / sipush
        | 0x15               // iload
        | 0x1a..=0x1d        // iload_0..3
        | 0x36               // istore
        | 0x3b..=0x3e        // istore_0..3
        | 0x57 | 0x59        // pop / dup
        | 0x60 | 0x64 | 0x68 // iadd isub imul
        | 0x74               // ineg
        | 0x78 | 0x7a | 0x7c // ishl ishr iushr
        | 0x7e | 0x80 | 0x82 // iand ior ixor
        | 0x84               // iinc
        | 0x99..=0xa4        // if<cc> / if_icmp<cc>
        | 0xa7               // goto
        | 0xac,              // ireturn
    )
}

/// Whether `emit_method` has a match arm for this opcode (coverage predicate — keep in
/// sync with the emit_method match). Used by the blocker tally to find what to add next.
pub fn emit_covers(op: u8) -> bool {
    matches!(op,
        0x01 | 0x02..=0x08 | 0x09 | 0x0a | 0x10 | 0x11  // consts (incl aconst_null)
        | 0x15 | 0x1a..=0x1d | 0x16 | 0x1e..=0x21       // i/l loads
        | 0x19 | 0x2a..=0x2d                            // a loads
        | 0x36 | 0x3b..=0x3e | 0x37 | 0x3f..=0x42 | 0x3a | 0x4b..=0x4e  // stores (incl astore)
        | 0x57 | 0x58 | 0x59 | 0x5a | 0x5c | 0x5f       // stack ops
        | 0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 | 0x6c | 0x70 // i-arith (incl idiv/irem)
        | 0x61 | 0x65 | 0x69 | 0x7f | 0x81 | 0x83 | 0x6d | 0x71 // l-arith (incl ldiv/lrem)
        | 0x74 | 0x75                                   // i/l neg
        | 0x78 | 0x7a | 0x7c | 0x79 | 0x7b | 0x7d       // shifts
        | 0x84 | 0x85 | 0x88 | 0x94 | 0x91 | 0x92 | 0x93 // iinc, i2l, l2i, lcmp, i2b/i2c/i2s
        | 0x0e | 0x0f | 0x18 | 0x26..=0x29 | 0x39 | 0x47..=0x4a // double const/load/store
        | 0x63 | 0x67 | 0x6b | 0x6f | 0x77 | 0x97 | 0x98 | 0x73 // dadd/dsub/dmul/ddiv/dneg/dcmp/drem
        | 0x87 | 0x8a | 0x8e | 0x8f | 0xaf              // i2d/l2d/d2i/d2l/dreturn
        | 0x0b..=0x0d | 0x17 | 0x22..=0x25 | 0x38 | 0x43..=0x46 // float const/load/store
        | 0x62 | 0x66 | 0x6a | 0x6e | 0x76 | 0x95 | 0x96 | 0x72 // fadd/fsub/fmul/fdiv/fneg/fcmp/frem
        | 0x86 | 0x89 | 0x8b | 0x8c | 0x8d | 0x90 | 0xae // i2f/l2f/f2i/f2l/f2d/d2f/freturn
        | 0x2e | 0x2f | 0x30 | 0x31 | 0x32 | 0x33 | 0x34 | 0x35 // array loads (incl f/d/laload)
        | 0xb2 | 0xb4 | 0xb5 | 0xb6..=0xba | 0x12 | 0x13 | 0x14 | 0xbb | 0xbe // callbacks (incl indy 0xba, ldc2_w)
        | 0x53 | 0x4f | 0x50 | 0x51 | 0x52 | 0x54 | 0x55 | 0x56 // array stores (aastore + i/l/f/d/b/c/s)
        | 0xbd | 0xbc | 0xbf | 0xc1                     // anewarray, newarray, athrow, instanceof
        | 0xc0                                          // checkcast
        | 0x99..=0xa7 | 0xc6 | 0xc7 | 0xaa | 0xab       // branches (incl switches, if_acmp, ifnull/nonnull)
        | 0xac | 0xb0 | 0xb1 | 0xad,                    // returns (incl void)
    )
}

/// Opcodes in `bc` that `emit_method` does NOT cover (the coverage blockers). Stops at
/// the first undecodable instruction. Deduped.
pub fn uncovered_opcodes(bc: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if !emit_covers(op) && !out.contains(&op) {
            out.push(op);
        }
        match insn_len_at(bc, pc) {
            Some(l) => pc += l,
            None => break,
        }
    }
    out
}

fn s16(bc: &[u8], pc: usize) -> i32 {
    ((bc[pc] as i16) << 8 | bc[pc + 1] as i16) as i32
}

/// Resolved invoke shape (from the VM constant pool) for the AOT translator: how many
/// operand cells the call consumes (receiver + params, one cell each — matching
/// rava_invoke's buffer), the dispatch `kind` (0 static / 1 virtual / 2 special / 3
/// interface), and the return shape.
/// What an `ldc` constant is. A STRING is interned into the permanent arena pool and its
/// stable address loaded from the const table (ConstDesc::StrLit → intern_arena_string at
/// link) — matching the JIT's emit_ldc_ref, which bakes the interned address (NOT a fresh
/// alloc_arena_string per call, which breaks String identity + churns the nursery). A
/// Class goes through rava_ldc; numerics are inlined.
#[derive(Clone)]
pub enum LdcConst {
    Str(String),
    Class,
    Int(i32),
    Float(u32),
    /// ldc2_w wide constants: a long literal, and a double as its raw 64 bits (the
    /// AOT stack stores a double as the bit-pattern in the I64 cell).
    Long(i64),
    Double(u64),
}

/// A field's type, for getfield/putfield: reference, int-family (fits the 32-bit cell),
/// or a wide long/double (the whole i64 cell). Drives is_ref/is_wide + the rava_putfield
/// kind (0 int / 1 long / 2 float / 3 double / 4 ref).
#[derive(Clone, Copy, PartialEq)]
pub enum FieldKind {
    Ref,
    Int,
    Long,
    Float,
    Double,
}
impl FieldKind {
    fn is_ref(self) -> bool {
        self == FieldKind::Ref
    }
    fn is_wide(self) -> bool {
        matches!(self, FieldKind::Long | FieldKind::Double)
    }
    fn put_kind(self) -> i64 {
        match self {
            FieldKind::Int => 0,
            FieldKind::Long => 1,
            FieldKind::Float => 2,
            FieldKind::Double => 3,
            FieldKind::Ref => 4,
        }
    }
}

#[derive(Clone)]
pub struct InvokeInfo {
    pub numeric_leaf: Option<numeric_leaf::NumericLeaf>,
    pub object_leaf: Option<object_leaf::ObjectLeaf>,
    pub direct_leaf: Option<direct_leaf::DirectLeafCall>,
    pub ncells: u32,
    pub kind: i64,
    pub ret_void: bool,
    pub ret_ref: bool,
    /// Return is a category-2 value (long/double) — pushed as a wide stack entry.
    pub ret_wide: bool,
}

/// Basic-block leaders: pc 0, every branch target, and every fall-through after a
/// branch. Sorted + deduped.
fn leaders(bc: &[u8]) -> Option<Vec<usize>> {
    let mut set = vec![0usize];
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        match op {
            // if<cc>/if_icmp<cc> (0x99-0xa4), if_acmpeq/ne (0xa5/0xa6), goto (0xa7),
            // ifnull/ifnonnull (0xc6/0xc7) — all s16 branches.
            0x99..=0xa6 | 0xa7 | 0xc6 | 0xc7 => {
                let target = (pc as i32 + s16(bc, pc + 1)) as usize;
                set.push(target);
                if op != 0xa7 {
                    set.push(pc + len); // conditional fall-through
                }
            }
            0xaa | 0xab => {
                // tableswitch / lookupswitch: the default + every case target is a leader.
                let (default_t, cases) = crate::jit::switch_targets(bc, pc)?;
                set.push(default_t);
                for (_, t) in &cases {
                    set.push(*t);
                }
                set.push(pc + len);
            }
            _ => {}
        }
        pc += len;
    }
    set.retain(|&p| p < bc.len());
    set.sort_unstable();
    set.dedup();
    Some(set)
}

/// Compile a numeric method to a relocatable `CompiledObj`. Returns None on any
/// unsupported shape (non-empty operand stack across a block boundary, a bad branch,
/// an opcode outside the subset) — the caller then keeps the method interpreted/JIT'd.
pub fn compile_numeric(bc: &[u8]) -> Option<CompiledObj> {
    if !numeric_eligible(bc) {
        return None;
    }
    let leaders = leaders(bc)?;
    let leader_set: HashMap<usize, usize> = leaders.iter().enumerate().map(|(i, &p)| (p, i)).collect();

    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals base
    sig.returns.push(AbiParam::new(I64)); // result (sign-extended int)
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);
    let mut fctx = FunctionBuilderContext::new();

    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let blocks: Vec<_> = leaders.iter().map(|_| b.create_block()).collect();
        let entry = blocks[0];
        b.append_block_params_for_function_params(entry);
        let locals = b.block_params(entry)[0];
        if emit(&mut b, bc, &leaders, &leader_set, &blocks, locals).is_none() {
            return None;
        }
        b.finalize();
    }

    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    // The numeric subset is callback-free + constant-free ⇒ no relocations. If any
    // appeared (a shape slipped through), bail rather than persist un-relinkable code.
    if !compiled.buffer.relocs().is_empty() {
        return None;
    }
    Some(CompiledObj::from_code(compiled.code_buffer().to_vec()))
}

/// A callback argument source (A3.3 / step toward A4's unified emit): a value loaded
/// from the local buffer, from a per-method constant-table slot (resolved per process),
/// or an inline immediate. Composing these covers every simple (non-args-buffer)
/// callback opcode uniformly — getfield/putfield/new/newarray/getstatic/aaload/…
#[derive(Clone, Copy)]
pub enum ArgSrc {
    Local(u32),
    Const(u32),
    Imm(i64),
}

/// CONSTANT TABLE proof (lever #1's named remaining piece): compile a method whose
/// ABI is `extern "C" fn(locals: *mut i64, consts: *const i64) -> i64` and which
/// returns `consts[const_idx] + locals[0]`. The runtime constant (a field id, a
/// class_ptr, an interned-string address — values baked inline by the current JIT and
/// thus NOT position-independent) instead lives in the per-method `consts` array the
/// LINKER fills per process. The code only ever loads from an argument offset, so it is
/// fully position-independent and AOT-serializable; the values are supplied at link.
/// This is the exact mechanism that lets a callback-carrying method's baked constants
/// survive the serialize→new-process→link cycle, complementing the Abs8 call relocs.
pub fn compile_const_plus_local0(const_idx: u32) -> Option<CompiledObj> {
    let mut fb = settings::builder();
    fb.set("is_pic", "false").ok()?;
    fb.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder().ok()?.finish(settings::Flags::new(fb)).ok()?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(I64)); // locals base
    sig.params.push(AbiParam::new(I64)); // constant-table base
    sig.returns.push(AbiParam::new(I64));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let locals = b.block_params(blk)[0];
        let consts = b.block_params(blk)[1];
        let l0 = b.ins().load(I64, MemFlags::trusted(), locals, 0);
        let c = b.ins().load(I64, MemFlags::trusted(), consts, (const_idx * 8) as i32);
        let r = b.ins().iadd(l0, c);
        b.ins().return_(&[r]);
        b.finalize();
    }
    let mut ctx = Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut Default::default()).ok()?;
    if !compiled.buffer.relocs().is_empty() {
        return None; // pure loads-from-args ⇒ no relocs
    }
    Some(CompiledObj::from_code(compiled.code_buffer().to_vec()))
}

fn intern_const(consts: &mut Vec<crate::cnp::ConstDesc>, d: crate::cnp::ConstDesc) -> u32 {
    if let Some(i) = consts.iter().position(|c| *c == d) {
        return i as u32;
    }
    consts.push(d);
    (consts.len() - 1) as u32
}

/// The typed-stack opcode loop for `compile_method` — numeric ops + getfield, control
/// flow, returns. Stack entries carry `(value, is_ref)`: a ref cell (i64, from aload /
/// getfield) vs an int (sign-extended i32). Bails to None on any uncovered opcode.
struct Callbacks {
    acmp: cranelift::codegen::ir::FuncRef,
    getfield: cranelift::codegen::ir::FuncRef,
    invoke: cranelift::codegen::ir::FuncRef,
    ldc: cranelift::codegen::ir::FuncRef,
    new: cranelift::codegen::ir::FuncRef,
    arraylength: cranelift::codegen::ir::FuncRef,
    putfield: cranelift::codegen::ir::FuncRef,
    aload: cranelift::codegen::ir::FuncRef,
    getstatic: cranelift::codegen::ir::FuncRef,
    throw: cranelift::codegen::ir::FuncRef,
    aastore: cranelift::codegen::ir::FuncRef,
    iastore: cranelift::codegen::ir::FuncRef,
    divrem: cranelift::codegen::ir::FuncRef,
    divrem64: cranelift::codegen::ir::FuncRef,
    f32rem: cranelift::codegen::ir::FuncRef,
    f64rem: cranelift::codegen::ir::FuncRef,
    anewarray: cranelift::codegen::ir::FuncRef,
    newarray: cranelift::codegen::ir::FuncRef,
    instanceof: cranelift::codegen::ir::FuncRef,
    checkcast: cranelift::codegen::ir::FuncRef,
    null_fault: cranelift::codegen::ir::FuncRef,
    pending: cranelift::codegen::ir::FuncRef,
    indy: cranelift::codegen::ir::FuncRef,
}

#[derive(PartialEq)]
enum CmpKind {
    Zero,
    Icmp,
}

/// Cell (f32 bits in low 32) → an F32 SSA value.
fn cell_to_f32(b: &mut FunctionBuilder, cell: Value) -> Value {
    let bits = b.ins().ireduce(I32, cell);
    b.ins().bitcast(F32, MemFlags::new(), bits)
}
/// F32 SSA value → a cell (f32 bits zero-extended into the i64 slot).
fn f32_to_cell(b: &mut FunctionBuilder, f: Value) -> Value {
    let bits = b.ins().bitcast(I32, MemFlags::new(), f);
    b.ins().uextend(I64, bits)
}

/// fcmp<l|g> result: -1 (a<b) / 0 (a==b) / +1 (a>b); a NaN operand yields +1 for the
/// `g` form (nan_pos=true) and -1 for the `l` form. Shared by dcmpl/dcmpg/fcmpl/fcmpg.
fn fcmp_result(b: &mut FunctionBuilder, a: Value, c: Value, nan_pos: bool) -> Value {
    let lt = b.ins().fcmp(FloatCC::LessThan, a, c);
    let gt = b.ins().fcmp(FloatCC::GreaterThan, a, c);
    let uno = b.ins().fcmp(FloatCC::Unordered, a, c);
    let neg1 = b.ins().iconst(I64, -1);
    let zero = b.ins().iconst(I64, 0);
    let one = b.ins().iconst(I64, 1);
    let hi = b.ins().select(gt, one, zero);
    let base = b.ins().select(lt, neg1, hi);
    let nanval = if nan_pos { one } else { neg1 };
    b.ins().select(uno, nanval, base)
}

fn cond_of(op: u8) -> (IntCC, CmpKind) {
    match op {
        0x99 => (IntCC::Equal, CmpKind::Zero),
        0x9a => (IntCC::NotEqual, CmpKind::Zero),
        0x9b => (IntCC::SignedLessThan, CmpKind::Zero),
        0x9c => (IntCC::SignedGreaterThanOrEqual, CmpKind::Zero),
        0x9d => (IntCC::SignedGreaterThan, CmpKind::Zero),
        0x9e => (IntCC::SignedLessThanOrEqual, CmpKind::Zero),
        0x9f => (IntCC::Equal, CmpKind::Icmp),
        0xa0 => (IntCC::NotEqual, CmpKind::Icmp),
        0xa1 => (IntCC::SignedLessThan, CmpKind::Icmp),
        0xa2 => (IntCC::SignedGreaterThanOrEqual, CmpKind::Icmp),
        0xa3 => (IntCC::SignedGreaterThan, CmpKind::Icmp),
        0xa4 => (IntCC::SignedLessThanOrEqual, CmpKind::Icmp),
        _ => unreachable!(),
    }
}


#[cfg(test)]
mod tests;

mod emit_direct_leaf;
pub(crate) use emit_direct_leaf::*;
mod emit_ops;
pub(crate) use emit_ops::*;
mod emit_ops_rest2;
pub(crate) use emit_ops_rest2::*;
mod emit_ops_rest;
pub(crate) use emit_ops_rest::*;
mod emit_method;
pub(crate) use emit_method::*;
mod shapes;
pub(crate) use shapes::*;
