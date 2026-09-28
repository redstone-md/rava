//! Cold JIT null faults carry immutable bytecode identity instead of a synthetic frame.
use crate::jit::*;

/// Called only after a null receiver test, with live guest references spilled.
/// Class and Code pointers are the same pinned identities used by JIT deopt sites.
pub(crate) extern "C" fn rava_npe_invoke_site(caller: i64, code_key: i64, pc: i64) -> i64 {
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() { return 0; }
    #[cfg(feature = "next-gen-gc")]
    let _locked = match worker_illegal_gate(s09census::IDX_NPE_SITE, "npe invoke site") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(guard) => Some(guard),
        WorkerGate::Abort => return 0,
    };
    let message = invoke_message(caller, code_key, pc);
    match message {
        Some(message) => set_pending(vm, "java/lang/NullPointerException", &message),
        None => set_pending(vm, "java/lang/InternalError", "invalid JIT NPE site"),
    }
    0
}

fn invoke_message(caller: i64, code_key: i64, pc: i64) -> Option<String> {
    if caller == 0 || code_key == 0 { return None; }
    let pc = usize::try_from(pc).ok()?;
    // SAFETY: emission supplies pinned class/Code identities for this exact body,
    // including a splice's own environment; neither pointer is a guest cell.
    let class = unsafe { &*(caller as *const crate::LoadedClass) };
    let code = unsafe { &*(code_key as *const rava_classfile::Code) };
    if !matches!(code.bytecode.get(pc)?, 0xb6 | 0xb7 | 0xb9) { return None; }
    let cp = u16::from_be_bytes([*code.bytecode.get(pc + 1)?, *code.bytecode.get(pc + 2)?]);
    let member = class.file.pool.member_ref(cp)?;
    let source = crate::npe::source_at(&code.bytecode, pc,
        crate::count_args(member.descriptor), &class.file.pool);
    Some(crate::npe::invoke(member.owner, member.name, member.descriptor, source))
}
