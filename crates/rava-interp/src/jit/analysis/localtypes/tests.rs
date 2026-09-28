use super::*;
use rava_classfile::ExceptionHandler;

fn handler(start: u16, end: u16, target: u16) -> ExceptionHandler {
    ExceptionHandler { start_pc: start, end_pc: end, handler_pc: target, catch_type: 0 }
}

fn body(bytecode: Vec<u8>, exceptions: Vec<ExceptionHandler>) -> Code {
    Code { bytecode, exceptions, max_locals: 2, max_stack: 4, ..Default::default() }
}

#[test]
fn exceptional_only_handler_is_reachable_with_entry_locals() {
    // aconst_null; athrow; astore_1; iload_0; ireturn
    let c = body(vec![0x01, 0xbf, 0x4c, 0x1a, 0xac], vec![handler(0, 2, 2)]);
    let types = compute(&c, "(I)I", true).unwrap();
    assert_eq!(types.at(2), Some([LT::Known(JT::Int), LT::Unset].as_slice()));
    assert_eq!(types.at(3), Some([LT::Known(JT::Int), LT::Known(JT::Ref)].as_slice()));
}

#[test]
fn protected_store_publishes_pre_instruction_state_and_end_is_exclusive() {
    // aconst_null; astore_0; return; astore_1; return
    let c = body(vec![0x01, 0x4b, 0xb1, 0x4c, 0xb1], vec![handler(1, 2, 3)]);
    let types = compute(&c, "(I)V", true).unwrap();
    assert_eq!(types.at(2).unwrap()[0], LT::Known(JT::Ref));
    assert_eq!(types.at(3).unwrap()[0], LT::Known(JT::Int));
}

#[test]
fn incompatible_exception_paths_refuse_a_reconstruction_type() {
    let c = body(vec![0x01, 0x4b, 0xb1, 0x4c, 0xb1], vec![handler(0, 3, 3)]);
    let types = compute(&c, "(I)V", true).unwrap();
    assert_eq!(types.at(3).unwrap()[0], LT::Unknown);
    assert!(!types.all_known(3, 2));
}

#[test]
fn backward_handler_edge_reaches_fixpoint() {
    // goto 5; astore_1; return; aconst_null; athrow
    let c = body(vec![0xa7, 0, 5, 0x4c, 0xb1, 0x01, 0xbf], vec![handler(5, 7, 3)]);
    let types = compute(&c, "(J)V", true).unwrap();
    assert_eq!(types.at(3).unwrap()[0], LT::Known(JT::Long));
    assert_eq!(types.at(4).unwrap()[1], LT::Known(JT::Ref));
}

#[test]
fn enclosing_handler_receives_state_from_rethrowing_handler() {
    // null; throw; astore_1; aload_1; throw; astore_0; return
    let c = body(vec![0x01, 0xbf, 0x4c, 0x2b, 0xbf, 0x4b, 0xb1],
        vec![handler(0, 2, 2), handler(3, 5, 5)]);
    let types = compute(&c, "(I)V", true).unwrap();
    assert_eq!(types.at(5), Some([LT::Known(JT::Int), LT::Known(JT::Ref)].as_slice()));
    assert_eq!(types.at(6), Some([LT::Known(JT::Ref), LT::Known(JT::Ref)].as_slice()));
}

#[test]
fn shadowed_handlers_are_conservatively_reachable() {
    let c = body(vec![0x01, 0xbf, 0x4c, 0xb1, 0x4c, 0xb1],
        vec![handler(0, 2, 2), handler(0, 2, 4)]);
    let types = compute(&c, "(I)V", true).unwrap();
    assert!(types.at(2).is_some());
    assert!(types.at(4).is_some());
}

#[test]
fn malformed_exception_boundaries_are_refused() {
    // bipush occupies PCs 0 and 1; only 0 and 2 are instruction starts.
    for h in [handler(1, 2, 2), handler(0, 1, 2), handler(0, 2, 1),
        handler(2, 2, 2), handler(2, 0, 2), handler(0, 4, 2), handler(0, 2, 3)]
    {
        assert!(compute(&body(vec![0x10, 1, 0xb1], vec![h]), "()V", true).is_none());
    }
    assert!(compute(&body(vec![0x10, 1, 0xb1], vec![handler(0, 3, 2)]), "()V", true).is_some());
}

#[test]
fn exceptional_edge_work_is_statically_bounded() {
    let c = body(vec![0; 1025], vec![handler(0, 1, 1); 1024]);
    assert!(compute(&c, "()V", true).is_none());
}

#[test]
fn truncated_protected_instruction_is_refused() {
    let c = body(vec![0xb1, 0x11, 0], vec![handler(1, 3, 0)]);
    assert!(compute(&c, "()V", true).is_none());
}

#[test]
fn ordinary_branch_join_still_marks_incompatible_locals() {
    // iload_0; ifeq 9; null; astore_1; goto 11; iconst_1; istore_1; return
    let c = body(vec![0x1a, 0x99, 0, 8, 0x01, 0x4c, 0xa7, 0, 5, 0x04, 0x3c, 0xb1], vec![]);
    let types = compute(&c, "(I)V", true).unwrap();
    assert_eq!(types.at(11), Some([LT::Known(JT::Int), LT::Unknown].as_slice()));
    assert!(types.at(2).is_none(), "an operand byte has no frame state");
}
