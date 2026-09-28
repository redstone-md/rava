//! Return to the shared runner before executing bytecode after a throwing call.
use super::*;

pub(super) fn emit(b: &mut FunctionBuilder, cb: &Callbacks) {
    let call = b.ins().call(cb.pending, &[]);
    let pending = b.inst_results(call)[0];
    let fault = b.create_block();
    let resume = b.create_block();
    b.ins().brif(pending, fault, &[], resume, &[]);
    b.seal_block(fault);
    b.seal_block(resume);
    b.switch_to_block(fault);
    let zero = b.ins().iconst(I64, 0);
    b.ins().return_(&[zero]);
    b.switch_to_block(resume);
}
