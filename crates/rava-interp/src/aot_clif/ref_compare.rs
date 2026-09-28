//! Reference identity without a runtime transition for ordinary reference cells.
use super::*;

pub(super) fn equal(b: &mut FunctionBuilder, callback: cranelift::codegen::ir::FuncRef,
    left: Value, right: Value) -> Value {
    let same = b.ins().icmp(IntCC::Equal, left, right);
    let different = b.ins().icmp(IntCC::NotEqual, left, right);
    let tag = (crate::value::BOX_TAG as u64 >> 32) as i64;
    let left_high = b.ins().ushr_imm(left, 32);
    let right_high = b.ins().ushr_imm(right, 32);
    let left_box = b.ins().icmp_imm(IntCC::Equal, left_high, tag);
    let right_box = b.ins().icmp_imm(IntCC::Equal, right_high, tag);
    let boxed = b.ins().bor(left_box, right_box);
    let decode = b.ins().band(different, boxed);
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I8);
    b.ins().brif(decode, slow, &[], merge, &[same.into()]);
    b.switch_to_block(slow);
    b.seal_block(slow);
    let call = b.ins().call(callback, &[left, right]);
    let result = b.inst_results(call)[0];
    let result = b.ins().icmp_imm(IntCC::NotEqual, result, 0);
    b.ins().jump(merge, &[result.into()]);
    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn decode(_: i64, _: i64) -> i64 { CALLS.fetch_add(1, Ordering::SeqCst); 1 }

    #[test]
    fn only_distinct_boxed_cells_cross_the_runtime_boundary() {
        let bytes = [0x2a, 0x2b, 0xa6, 0, 7, 0x04, 0xa7, 0, 4, 0x03, 0xac];
        let (code, relocs, consts) = compile_method(&bytes, "Identity", 2,
            &|_| None, &|_| None, &|_| None, &|_| None).unwrap();
        let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
        let symbol = crate::jit::aot_symtab().iter().position(|(name, _)| *name == "rava_acmp").unwrap();
        let (function, constants) = obj.link_full(&|index| {
            assert_eq!(index as usize, symbol);
            decode as extern "C" fn(i64, i64) -> i64 as usize
        }, &mut |_| Some(0)).unwrap();
        let boxed = crate::value::BOX_TAG;
        for (left, right, result, calls) in [
            (0, 0, 1, 0), (0, 16, 0, 0), (16, 16, 1, 0), (16, 32, 0, 0),
            (boxed, boxed, 1, 0), (boxed, boxed + 1, 1, 1),
            (boxed, 0, 1, 1), (0, boxed, 1, 1),
        ] {
            CALLS.store(0, Ordering::SeqCst);
            let mut locals = [left, right];
            assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) }, result);
            assert_eq!(CALLS.load(Ordering::SeqCst), calls);
        }
    }
}
