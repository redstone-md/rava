//! Null dereferences leave the method before any following bytecode executes.
use super::*;

pub(super) fn emit(
    b: &mut FunctionBuilder, bc: &[u8], pc: usize,
    stack: &[(Value, bool, bool)], constants: Value, cb: &Callbacks,
    method_of: &dyn Fn(u16) -> Option<InvokeInfo>,
) -> Option<()> {
    let op = bc[pc];
    let depth = match op {
        0xb4 | 0xbe | 0xbf => 0,
        0xb5 | 0x2e..=0x35 => 1,
        0x4f..=0x56 => 2,
        0xb6 | 0xb7 | 0xb9 => {
            let cp = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
            method_of(cp)?.ncells.checked_sub(1)? as usize
        }
        _ => return Some(()),
    };
    let receiver = stack.get(stack.len().checked_sub(depth + 1)?)?.0;
    let fault = b.create_block();
    let resume = b.create_block();
    let is_null = b.ins().icmp_imm(IntCC::Equal, receiver, 0);
    b.ins().brif(is_null, fault, &[], resume, &[]);
    b.seal_block(fault);
    b.seal_block(resume);
    b.switch_to_block(fault);
    let caller = load_const(b, 0, constants);
    let pc_value = b.ins().iconst(I64, pc as i64);
    let op_value = b.ins().iconst(I64, i64::from(op));
    let depth_value = b.ins().iconst(I64, depth as i64);
    b.ins().call(cb.null_fault, &[caller, pc_value, op_value, depth_value]);
    let zero = b.ins().iconst(I64, 0);
    b.ins().return_(&[zero]);
    b.switch_to_block(resume);
    Some(())
}
