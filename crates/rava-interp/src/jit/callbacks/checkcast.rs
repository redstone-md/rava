//! Checked casts share the interpreter's compatibility rule, including unknown types.
use crate::jit::*;

/// Returns one on success, zero with a pending exception on rejection.
/// The emitter roots the operand and all other live references across this call.
pub(crate) extern "C" fn rava_checkcast(caller: i64, cp: i64, cell: i64) -> i64 {
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() { return 0; }
    if cell == 0 { return 1; }
    let class = unsafe { &*(caller as *const crate::LoadedClass) };
    let value = unbox_ref(vm, cell, "");
    match crate::opcodes::checkcast_reject(vm, class, cp as u16, &value) {
        Ok(None) => return 1,
        Ok(Some(message)) => set_pending(vm, "java/lang/ClassCastException", &message),
        Err(crate::VmError::Throw(exc)) => {
            vm.jit_pending_set(exc);
            vm.heap.set_jit_fault();
        }
        Err(_) => set_pending(vm, "java/lang/InternalError", "invalid JIT checkcast"),
    }
    0
}
