//! Operand-stack bookkeeping and the small bytecode decoders emission reads.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Decode a constant-PUSH opcode (the loop init / limit constants) → (value, len).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn decode_iconst(bc: &[u8], pc: usize) -> Option<(i32, usize)> {
    match *bc.get(pc)? {
        op @ 0x02..=0x08 => Some((op as i32 - 0x03, 1)), // iconst_m1..5
        0x10 => Some((*bc.get(pc + 1)? as i8 as i32, 2)), // bipush
        0x11 => Some((((*bc.get(pc + 1)? as i8 as i32) << 8) | *bc.get(pc + 2)? as i32, 3)), // sipush
        _ => None,
    }
}

/// Decode an `istore`/`lstore` (any form) → (local index, len), or None.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn decode_store(bc: &[u8], pc: usize) -> Option<(usize, usize)> {
    match *bc.get(pc)? {
        0x36 | 0x37 => Some((*bc.get(pc + 1)? as usize, 2)), // istore / lstore <idx>
        op @ 0x3b..=0x3e => Some(((op - 0x3b) as usize, 1)), // istore_0..3
        op @ 0x3f..=0x42 => Some(((op - 0x3f) as usize, 1)), // lstore_0..3
        _ => None,
    }
}

/// Decode a constant push that seeds an accumulator → (value, len, is_long).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn decode_acc_init(bc: &[u8], pc: usize) -> Option<(i64, usize, bool)> {
    match *bc.get(pc)? {
        0x09 => Some((0, 1, true)),  // lconst_0
        0x0a => Some((1, 1, true)),  // lconst_1
        _ => decode_iconst(bc, pc).map(|(v, l)| (v as i64, l, false)),
    }
}

/// Decode a local store op → (local index, is_long), or `None` if not a store.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn decode_store_kind(bc: &[u8], pc: usize) -> Option<(usize, bool)> {
    match *bc.get(pc)? {
        0x36 => Some((*bc.get(pc + 1)? as usize, false)),  // istore idx
        0x37 => Some((*bc.get(pc + 1)? as usize, true)),   // lstore idx
        op @ 0x3b..=0x3e => Some(((op - 0x3b) as usize, false)), // istore_0..3
        op @ 0x3f..=0x42 => Some(((op - 0x3f) as usize, true)),  // lstore_0..3
        _ => None,
    }
}

/// Item-model stack delta for a MAP body op (long/double counts as 1 item) — used to locate
/// the `out[i] = …` store statement inside the body. `None` for ops the walk can't model
/// (calls / field access / branches / new), which bail the whole map to the serial path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn map_stack_delta(bc: &[u8], pc: usize) -> Option<i32> {
    Some(match bc[pc] {
        0x02..=0x0a | 0x0e | 0x0f | 0x10..=0x14 => 1, // iconst/lconst/dconst/bipush/sipush/ldc*
        0x15 | 0x16 | 0x18 | 0x1a..=0x21 | 0x26..=0x29 => 1, // int/long/double loads
        0x19 | 0x2a..=0x2d => 1,               // aload (array ref)
        0x2e | 0x2f | 0x31 => -1,              // iaload / laload / daload (pop ref+idx, push elem)
        0x36 | 0x37 | 0x39 | 0x3a | 0x3b..=0x42 | 0x47..=0x4e => -1, // local stores (int/long/double)
        0x57 | 0x58 => -1,                     // pop / pop2 (item model)
        0x59 => 1,                             // dup
        0x60..=0x73 => -1,                     // add / sub / mul / div / rem
        0x74..=0x77 => 0,                      // neg
        0x78..=0x83 => -1,                     // shifts / and / or / xor
        0x84 => 0,                             // iinc
        0x85..=0x93 => 0,                      // conversions i2l..i2s
        0xb8 => -1,                            // invokestatic Math.max/min (pop 2, push 1)
        _ => return None,
    })
}

pub(crate) fn cp_double(pool: &ConstantPool, bc: &[u8], at: usize) -> Option<f64> {
    if bc.get(at).copied()? != 0x14 {
        return None;
    }
    let idx = ((*bc.get(at + 1)? as u16) << 8) | *bc.get(at + 2)? as u16;
    match pool.get(idx)? {
        Constant::Double(v) => Some(*v),
        _ => None,
    }
}

pub(crate) fn cp_double_is(pool: &ConstantPool, bc: &[u8], at: usize, want: f64) -> bool {
    cp_double(pool, bc, at).is_some_and(|v| v.to_bits() == want.to_bits())
}

/// The reduce op's combine, in IR. Wrapping i64 (matches the JVM long fold).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn combine_ir(b: &mut FunctionBuilder, op: crate::parallel_pool::ReduceOp, a: Value, c: Value) -> Value {
    use crate::parallel_pool::ReduceOp;
    match op {
        ReduceOp::Add => b.ins().iadd(a, c),
        ReduceOp::Mul => b.ins().imul(a, c),
        ReduceOp::Xor => b.ins().bxor(a, c),
        ReduceOp::And => b.ins().band(a, c),
        ReduceOp::Or => b.ins().bor(a, c),
        // Signed max/min on the i64 partials (int elements ride sign-extended → exact).
        ReduceOp::Max => b.ins().smax(a, c),
        ReduceOp::Min => b.ins().smin(a, c),
    }
}

/// The reduce op's wire code for `rava_parallel_reduce` (0=Add,1=Mul,2=Xor).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn op_code(op: crate::parallel_pool::ReduceOp) -> i64 {
    use crate::parallel_pool::ReduceOp;
    match op {
        ReduceOp::Add => 0,
        ReduceOp::Mul => 1,
        ReduceOp::Xor => 2,
        ReduceOp::And => 3,
        ReduceOp::Or => 4,
        ReduceOp::Max => 5,
        ReduceOp::Min => 6,
    }
}

pub(crate) fn push(stack: &mut Vec<SV>, v: Value, t: JT) {
    stack.push(SV {
        v,
        t,
        src: None,
        #[cfg(feature = "next-gen-gc")]
        arena: false,
    });
}

pub(crate) fn stack_shape(stack: &[SV]) -> Vec<JT> {
    stack.iter().map(|sv| sv.t).collect()
}

pub(crate) fn stack_args(stack: &[SV]) -> Vec<cranelift::codegen::ir::BlockArg> {
    stack.iter().map(|sv| sv.v.into()).collect()
}

pub(crate) fn ensure_block_shape(
    b: &mut FunctionBuilder,
    blocks: &std::collections::HashMap<usize, cranelift::prelude::Block>,
    shapes: &mut std::collections::HashMap<usize, Vec<JT>>,
    target: usize,
    stack: &[SV],
) -> Option<()> {
    let shape = stack_shape(stack);
    if let Some(existing) = shapes.get(&target) {
        if existing.as_slice() != shape.as_slice() {
            return None;
        }
        return Some(());
    }

    let block = blocks[&target];
    for &t in &shape {
        b.append_block_param(block, jt_ty(t));
    }
    shapes.insert(target, shape);
    Some(())
}

pub(crate) fn jump_to(
    b: &mut FunctionBuilder,
    blocks: &std::collections::HashMap<usize, cranelift::prelude::Block>,
    shapes: &mut std::collections::HashMap<usize, Vec<JT>>,
    target: usize,
    stack: &[SV],
) -> Option<()> {
    ensure_block_shape(b, blocks, shapes, target, stack)?;
    let args = stack_args(stack);
    b.ins().jump(blocks[&target], &args);
    Some(())
}

pub(crate) fn brif_to(
    b: &mut FunctionBuilder,
    blocks: &std::collections::HashMap<usize, cranelift::prelude::Block>,
    shapes: &mut std::collections::HashMap<usize, Vec<JT>>,
    cond: Value,
    target: usize,
    fallthrough: usize,
    stack: &[SV],
) -> Option<()> {
    ensure_block_shape(b, blocks, shapes, target, stack)?;
    ensure_block_shape(b, blocks, shapes, fallthrough, stack)?;
    let args = stack_args(stack);
    b.ins()
        .brif(cond, blocks[&target], &args, blocks[&fallthrough], &args);
    Some(())
}

pub(crate) fn ldc_const(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    pool: &ConstantPool,
    idx: u16,
) -> Option<()> {
    match pool.get(idx)? {
        Constant::Integer(v) => {
            let c = b.ins().iconst(I32, *v as i64);
            push(stack, c, JT::Int);
        }
        Constant::Float(v) => {
            let c = b.ins().f32const(*v);
            push(stack, c, JT::Float);
        }
        Constant::Long(v) => {
            let c = b.ins().iconst(I64, *v);
            push(stack, c, JT::Long);
        }
        Constant::Double(v) => {
            let c = b.ins().f64const(*v);
            push(stack, c, JT::Double);
        }
        _ => return None,
    }
    Some(())
}

pub(crate) fn expect_pop(stack: &mut Vec<SV>, t: JT) -> Option<Value> {
    let sv = stack.pop()?;
    if sv.t == t {
        Some(sv.v)
    } else {
        None
    }
}

pub(crate) fn bin(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    t: JT,
    f: impl FnOnce(&mut FunctionBuilder, Value, Value) -> Value,
) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    let v = f(b, x.v, y.v);
    push(stack, v, t);
    Some(())
}

pub(crate) fn previous_insn_start(bc: &[u8], pc: usize) -> Option<usize> {
    let mut at = 0usize;
    let mut previous = None;
    while at < pc {
        previous = Some(at);
        at += insn_len_at(bc, at)?;
    }
    if at == pc { previous } else { None }
}

/// JVM ldiv/lrem via the i64 divrem libcall (Java zero/overflow semantics).
/// The literal behind `v` if it was produced by an `iconst`, else `None`. Read straight
/// off the DFG, so it sees exactly what the emitter itself pushed (a `ldc2_w`, a folded
/// constant) and nothing it merely guessed.
pub(crate) fn const_i64_of(b: &FunctionBuilder, v: Value) -> Option<i64> {
    use cranelift::codegen::ir::InstructionData;
    let inst = b.func.dfg.value_def(v).inst()?;
    match b.func.dfg.insts[inst] {
        InstructionData::UnaryImm { opcode: cranelift::codegen::ir::Opcode::Iconst, imm } => {
            Some(imm.bits())
        }
        _ => None,
    }
}
