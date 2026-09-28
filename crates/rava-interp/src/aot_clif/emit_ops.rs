//! The small emitters emit_method leans on: spill/reload of the rooted stack, and the
//! loads and stores against the locals array and the constant table.
//!
//! They were closures inside emit_method, which is what kept that function from being split.

use crate::aot_clif::*;

pub(crate) fn spill(b: &mut FunctionBuilder, stack: &[(Value, bool, bool)], locals: Value, max_locals: u32) {
    for (k, (v, is_ref, _)) in stack.iter().enumerate() {
        if *is_ref {
            b.ins().store(MemFlags::trusted(), *v, locals, ((max_locals as usize + k) * 8) as i32);
        }
    }
}

pub(crate) fn reload(b: &mut FunctionBuilder, stack: &mut [(Value, bool, bool)], locals: Value, max_locals: u32) {
    for (k, e) in stack.iter_mut().enumerate() {
        if e.1 {
            e.0 = b.ins().load(I64, MemFlags::trusted(), locals, ((max_locals as usize + k) * 8) as i32);
        }
    }
}

pub(crate) fn load_const(b: &mut FunctionBuilder, slot: u32, consts_ptr: Value) -> Value {
    b.ins().load(I64, MemFlags::trusted(), consts_ptr, (slot * 8) as i32)
}

pub(crate) fn load_int(b: &mut FunctionBuilder, idx: u32, locals: Value) -> Value {
    let v = b.ins().load(I32, MemFlags::trusted(), locals, (idx * 8) as i32);
    b.ins().sextend(I64, v)
}

pub(crate) fn load_ref(b: &mut FunctionBuilder, idx: u32, locals: Value) -> Value {
    b.ins().load(I64, MemFlags::trusted(), locals, (idx * 8) as i32)
}

pub(crate) fn store(b: &mut FunctionBuilder, idx: u32, v: Value, is_ref: bool, locals: Value) {
    if is_ref {
        b.ins().store(MemFlags::trusted(), v, locals, (idx * 8) as i32);
    } else {
        let lo = b.ins().ireduce(I32, v);
        let ext = b.ins().sextend(I64, lo);
        b.ins().store(MemFlags::trusted(), ext, locals, (idx * 8) as i32);
    }
}
