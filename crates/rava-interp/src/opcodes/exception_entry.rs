//! Cold handler entry for a reconstructed compiled frame.
//!
//! The emitter must supply current, typed locals and the original throwing PC.
//! This transition neither invalidates compiled code nor replays the faulting op.

use super::{Code, Frame, FramePtrGuard, RunExit, Value, Vm, VmError};

/// Resume a reconstructed frame at its matching handler, preserving all exits.
///
/// The caller owns the fault latch: take the pending guest exception and clear
/// the compiled fault before entering here. Worker aborts must be passed as
/// `RegionAbort`, never converted into a guest Throwable. The frame and Code
/// must belong to the same method and remain alive throughout this call.
///
/// This is the transition foundation; JIT admission remains closed until every
/// protected throwing instruction can supply a verified reconstruction state.
#[allow(dead_code)]
pub(crate) fn resume_exception(
    vm: &mut Vm,
    code: &Code,
    frame: &mut Frame,
    fault_pc: usize,
    error: VmError,
) -> Result<RunExit, VmError> {
    let VmError::Throw(exception) = error else {
        return Err(error);
    };
    // Discard the pre-fault operand stack, not the locals. Store the exception
    // in the frame before any matching or execution can inspect guest objects.
    frame.clear_stack();
    frame.push(exception);
    frame.pc = fault_pc;
    crate::frames::push(std::ptr::addr_of!(*frame) as usize);
    let _roots = FramePtrGuard;
    let handler = super::find_handler(vm, frame, code, fault_pc, &frame.buf[frame.nlocals])?;
    match handler {
        Some(pc) => {
            frame.pc = pc;
            // run registers its own nested root scope. Keep this outer scope
            // alive through the whole transition; both pop on every exit.
            super::run(vm, code, frame)
        }
        None => Err(VmError::Throw(frame.pop()?)),
    }
}

#[cfg(test)]
pub(crate) mod tests;
