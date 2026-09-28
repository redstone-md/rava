//! Execute generated code; a reachability-map test alone cannot prove emission.

use super::*;

#[test]
fn jit_skips_exception_shaped_trailer_without_inventing_stack_inputs() {
    // The trailer starts with astore, requiring a Throwable operand on a cold
    // handler path. There is no normal edge to it after the first ireturn.
    let c = code(vec![0x1a, 0xac, 0x4c, 0x05, 0xac], 2);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for value in [i32::MIN, -17, 0, 91, i32::MAX] {
        let mut locals = vec![value as i64, 0];
        assert_eq!(unsafe { f.call(&mut locals) } as i32, value);
    }
}

#[test]
fn jit_skips_dead_goto_gap_and_keeps_the_live_target() {
    let c = code(vec![0xa7, 0, 5, 0x4b, 0xb1, 0x1a, 0x04, 0x60, 0xac], 1);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for value in [i32::MIN, -2, 0, 127, i32::MAX] {
        let mut locals = vec![value as i64];
        assert_eq!(unsafe { f.call(&mut locals) } as i32, value.wrapping_add(1));
    }
}

#[test]
fn jit_dead_loop_does_not_hide_live_conditional_paths() {
    // if zero -> 10; return 1; dead self-loop at 6; dead nop; return 2.
    let c = code(vec![0x1a, 0x99, 0, 9, 0x04, 0xac, 0xa7, 0, 0, 0x00, 0x05, 0xac], 1);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (value, expected) in [(0, 2), (1, 1), (-8, 1)] {
        let mut locals = vec![value];
        assert_eq!(unsafe { f.call(&mut locals) }, expected);
    }
}

#[test]
fn jit_still_refuses_live_blocks_without_known_stack_inputs() {
    // A forward goto reaches a later backward jump into an earlier block.
    // Linear emission has no stack shape at 3 yet; normal reachability must
    // not misclassify it as dead just because its predecessor is not emitted.
    let c = code(vec![0xa7, 0, 5, 0x04, 0xac, 0xa7, 0xff, 0xfe], 1);
    assert!(compile(&c, "()I", &empty_pool(), 0, &mut |_, _| (0, None)).is_none());
}

#[test]
fn jit_handler_admission_remains_closed_until_fault_exits_are_connected() {
    let mut c = code(vec![0x1a, 0xac, 0x4c, 0x05, 0xac], 2);
    c.exceptions.push(rava_classfile::ExceptionHandler {
        start_pc: 0, end_pc: 2, handler_pc: 2, catch_type: 0,
    });
    assert!(compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).is_none());
}
