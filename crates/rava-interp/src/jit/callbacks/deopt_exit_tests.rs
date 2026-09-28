use super::*;

#[test]
fn native_deopt_preserves_an_interpreter_error() {
    let _serial = crate::parallel::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut vm, code, frame) =
        crate::opcodes::exception_entry::tests::fixture(vec![0xfe], vec![], vec![]);
    let site = DeoptSite {
        pc: 0,
        caller: 0,
        cp: 0,
        ret_ref: false,
        nlocals: 0,
        nstack: 0,
        tags: vec![],
    };
    let old = crate::jit::set_current_vm(&mut vm);
    let result = rava_deopt(
        std::sync::Arc::as_ptr(&frame.class) as i64,
        (&*code as *const rava_classfile::Code) as i64,
        (&site as *const DeoptSite) as i64,
        [].as_ptr(),
    );
    crate::jit::set_current_vm(old);
    assert_eq!(
        result, 0,
        "the native cell is ignored when the failure latch is set"
    );
    assert!(crate::execution_state::pending_is_set());
    assert!(matches!(
        crate::execution_state::pending_error_take(),
        Some(crate::VmError::UnknownOpcode(0xfe))
    ));
    vm.heap.clear_jit_fault();
}

#[test]
fn native_deopt_aborts_before_worker_vm_or_site_access() {
    let _serial = crate::parallel::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::parallel::clear_abort();
    crate::execution_state::register();
    crate::parallel::set_gilfree(true);

    let result = rava_deopt(0, 0, 0, std::ptr::null());

    crate::parallel::set_gilfree(false);
    assert_eq!(result, 0);
    assert!(crate::parallel::abort_requested());
    assert!(matches!(
        crate::execution_state::pending_error_take(),
        Some(crate::VmError::RegionAbort)
    ));
    crate::execution_state::unregister();
    crate::parallel::clear_abort();
}
