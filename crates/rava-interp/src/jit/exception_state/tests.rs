use super::*;
use crate::opcodes::exception_entry::tests::fixture;
use crate::opcodes::RunExit;
use crate::VmError;
use rava_classfile::ExceptionHandler;

fn handler(end: u16, target: u16) -> ExceptionHandler {
    ExceptionHandler { start_pc: 0, end_pc: end, handler_pc: target, catch_type: 0 }
}

#[test]
fn typed_cells_restore_wide_payloads_references_and_unused_slots() {
    let (vm, code, mut frame) = fixture(vec![0, 0xb1], vec![handler(1, 1)], vec![Value::Null; 9]);
    let map = ExceptionFrameMap::new(&code, "(IJFDLjava/lang/Object;[I)V", true).unwrap();
    let site = map.site(0).unwrap();
    let double = 0x7ff8_0000_0000_0055u64;
    let float = (-0.0f32).to_bits();
    let cells = [-9, i64::MIN + 13, -1, float as i64, double as i64, -1,
        crate::value::REF_TAG | 7, crate::value::ARRAY_TAG | 9, -1];
    frame.push(Value::Int(123));
    assert!(site.restore(&vm, &mut frame, &cells));
    assert_eq!(frame.buf.len(), 9);
    assert!(matches!(frame.buf[0], Value::Int(-9)));
    assert!(matches!(frame.buf[1], Value::Long(v) if v == i64::MIN + 13));
    assert!(matches!(frame.buf[2], Value::Null));
    assert!(matches!(frame.buf[3], Value::Float(v) if v.to_bits() == float));
    assert!(matches!(frame.buf[4], Value::Double(v) if v.to_bits() == double));
    assert!(matches!(frame.buf[5], Value::Null));
    assert!(matches!(frame.buf[6], Value::Ref(7)));
    assert!(matches!(frame.buf[7], Value::Array(9)));
    assert!(matches!(frame.buf[8], Value::Null));
}

#[test]
fn rejected_frame_identity_or_cell_count_leaves_frame_unchanged() {
    let (vm, code, mut frame) = fixture(vec![0, 0xb1], vec![handler(1, 1)], vec![Value::Int(77)]);
    let (_, other, _) = fixture(vec![0, 0xb1], vec![handler(1, 1)], vec![Value::Null]);
    let map = ExceptionFrameMap::new(&code, "(I)V", true).unwrap();
    let site = map.site(0).unwrap();
    frame.pc = 1;
    assert!(!site.restore(&vm, &mut frame, &[]));
    frame.code = &*other;
    assert!(!site.restore(&vm, &mut frame, &[9]));
    assert_eq!(frame.pc, 1);
    assert!(matches!(frame.buf.as_slice(), [Value::Int(77)]));
}

#[test]
fn unknown_unprotected_and_operand_byte_sites_are_refused() {
    // bipush; pop; null; astore_0; return; astore_1; return
    let (vm, code, _) = fixture(vec![0x10, 7, 0x57, 0x01, 0x4b, 0xb1, 0x4c, 0xb1],
        vec![handler(6, 6), ExceptionHandler {
            start_pc: 6, end_pc: 7, handler_pc: 7, catch_type: 0,
        }], vec![Value::Null; 2]);
    let map = ExceptionFrameMap::new(&code, "(I)V", true).unwrap();
    assert!(map.site(0).is_some());
    assert!(map.site(1).is_none());
    assert!(map.site(6).is_none(), "incompatible Int/Ref handler-local join");
    assert!(map.site(7).is_none(), "not protected");
    assert!(!vm.jit_pending_is_set());
}

#[test]
fn restored_cells_reach_handler_without_consuming_pending_early() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0x57, 0x1a, 0xac],
        vec![handler(1, 1)], vec![Value::Int(0)]);
    let map = ExceptionFrameMap::new(&code, "(I)I", true).unwrap();
    let site = map.site(0).unwrap();
    let exception = vm.make_exception("java/lang/ArithmeticException", "test");
    vm.jit_pending_set(exception);
    vm.heap.set_jit_fault();
    let result = site.resume_pending(&mut vm, &mut frame, &[91]).expect("transition");
    assert!(matches!(result, Ok(RunExit::Normal(Some(Value::Int(91))))));
    assert!(!vm.jit_pending_is_set());
    assert_eq!(crate::heap::active_jit_fault_for_test(&vm.heap.jit_ctx), 0);
}

#[test]
fn refused_pending_transition_keeps_exception_and_frame() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0xb1], vec![handler(1, 1)], vec![Value::Int(77)]);
    let map = ExceptionFrameMap::new(&code, "(I)V", true).unwrap();
    let site = map.site(0).unwrap();
    frame.pc = 1;
    assert!(site.resume_pending(&mut vm, &mut frame, &[9]).is_none());
    assert!(matches!(frame.buf.as_slice(), [Value::Int(77)]));
    let Value::Ref(expected) = vm.make_exception("java/lang/ArithmeticException", "pending")
        else { panic!("expected heap exception") };
    vm.jit_pending_set(Value::Ref(expected));
    vm.heap.set_jit_fault();
    assert!(site.resume_pending(&mut vm, &mut frame, &[]).is_none());
    vm.trampoline = true;
    assert!(site.resume_pending(&mut vm, &mut frame, &[9]).is_none());
    assert!(vm.trampoline);
    assert_eq!(frame.pc, 1);
    assert!(matches!(frame.buf.as_slice(), [Value::Int(77)]));
    assert_eq!(crate::heap::active_jit_fault_for_test(&vm.heap.jit_ctx), 1);
    assert!(matches!(vm.jit_pending_take(), Some(Value::Ref(got)) if got == expected));
}

#[cfg(feature = "next-gen-gc")]
#[test]
fn detached_context_cannot_consume_pending_exception_or_enter_handler() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0xb1], vec![handler(1, 1)], vec![Value::Int(77)]);
    let map = ExceptionFrameMap::new(&code, "(I)V", true).unwrap();
    let site = map.site(0).unwrap();
    let Value::Ref(expected) = vm.make_exception("java/lang/ArithmeticException", "worker")
        else { panic!("expected heap exception") };
    vm.jit_pending_set(Value::Ref(expected));
    vm.heap.set_jit_fault();
    crate::parallel::set_gilfree(true);
    let refused = site.resume_pending(&mut vm, &mut frame, &[9]).is_none();
    crate::parallel::set_gilfree(false);
    assert!(refused);
    assert!(matches!(frame.buf.as_slice(), [Value::Int(77)]));
    assert_eq!(crate::heap::active_jit_fault_for_test(&vm.heap.jit_ctx), 1);
    assert!(matches!(vm.jit_pending_take(), Some(Value::Ref(got)) if got == expected));
}

#[test]
fn pending_transition_returns_rethrow_as_the_same_error() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0xbf], vec![handler(1, 1)], vec![]);
    let map = ExceptionFrameMap::new(&code, "()V", true).unwrap();
    let site = map.site(0).unwrap();
    let Value::Ref(expected) = vm.make_exception("java/lang/ArithmeticException", "rethrow")
        else { panic!("expected heap exception") };
    vm.jit_pending_set(Value::Ref(expected));
    let result = site.resume_pending(&mut vm, &mut frame, &[]).expect("transition");
    assert!(matches!(result, Err(VmError::Throw(Value::Ref(got))) if got == expected));
    assert!(!vm.jit_pending_is_set());
}

#[test]
fn reference_slots_preserve_boxed_values_after_box_table_release() {
    let (vm, code, mut frame) = fixture(vec![0, 0xb1], vec![handler(1, 1)], vec![Value::Null; 3]);
    let map = ExceptionFrameMap::new(&code,
        "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)V", true).unwrap();
    let site = map.site(0).unwrap();
    let base = crate::frames::box_len();
    let cells = [super::super::box_ref(&vm, &Value::Str("retained".into())),
        super::super::box_ref(&vm, &Value::Int(42)), 0];
    let restored = site.restore(&vm, &mut frame, &cells);
    crate::frames::box_truncate(base);
    assert!(restored);
    assert!(matches!(&frame.buf[0], Value::Str(s) if s.as_ref() == "retained"));
    assert!(matches!(frame.buf[1], Value::Int(42)));
    assert!(matches!(frame.buf[2], Value::Null));
}
