//! Unit tests for the direct-leaf classifier (split out of direct_leaf.rs
//! to keep the implementation file under the 500-line file budget).

use super::*;

fn leaf_code(max_locals: u16, max_stack: u16, bytecode: &[u8]) -> Code {
    Code {
        max_stack,
        max_locals,
        bytecode: bytecode.to_vec(),
        ..Default::default()
    }
}

// --- accepted shapes -------------------------------------------------

#[test]
fn accepts_int_shape() {
    // iload_0, iload_1, iadd, ireturn
    let code = leaf_code(2, 2, &[0x1a, 0x1b, 0x60, 0xac]);
    assert!(is_no_gc_primitive_leaf("(II)I", 0x0008, &code));
}

#[test]
fn accepts_long_shape() {
    // lload_0, lload_2, ladd, lreturn — (J,J) occupy slots 0..2 and 2..4.
    let code = leaf_code(4, 4, &[0x1e, 0x20, 0x61, 0xad]);
    assert!(is_no_gc_primitive_leaf("(JJ)J", 0x0008, &code));
}

#[test]
fn accepts_boolean_shape() {
    // iload_0, iload_1, iand, ireturn
    let code = leaf_code(2, 2, &[0x1a, 0x1b, 0x7e, 0xac]);
    assert!(is_no_gc_primitive_leaf("(IZ)Z", 0x0008, &code));
}

#[test]
fn accepts_zero_arg_shape() {
    // iconst_1, ireturn
    let code = leaf_code(0, 1, &[0x04, 0xac]);
    assert!(is_no_gc_primitive_leaf("()Z", 0x0008, &code));
}

#[test]
fn accepts_forward_branch() {
    // if (a != 0) return b; return 0;
    // pc0 iload_0; pc1 ifne +5 -> pc6; pc4 iconst_0; pc5 ireturn;
    // pc6 iload_1; pc7 ireturn.
    let code = leaf_code(2, 1, &[0x1a, 0x9a, 0x00, 0x05, 0x03, 0xac, 0x1b, 0xac]);
    assert!(is_no_gc_primitive_leaf("(II)I", 0x0008, &code));
}

#[test]
fn accepts_five_args() {
    // iload_0..iload_3, iload 4, four iadds, ireturn.
    let code = leaf_code(
        5,
        5,
        &[
            0x1a, 0x1b, 0x1c, 0x1d, 0x15, 0x04, 0x60, 0x60, 0x60, 0x60, 0xac,
        ],
    );
    assert!(is_no_gc_primitive_leaf("(IIIII)I", 0x0008, &code));
}

// --- rejected shapes -------------------------------------------------

#[test]
fn rejects_reference_args() {
    assert!(DirectLeafCall::from_member("Owner", "f", "(Ljava/lang/Object;)I").is_none());
    assert!(DirectLeafCall::from_member("Owner", "f", "([I)I").is_none());
    let code = leaf_code(1, 1, &[0xac]);
    assert!(!is_no_gc_primitive_leaf(
        "(Ljava/lang/String;)I",
        0x0008,
        &code
    ));
}

#[test]
fn rejects_void_and_wide_and_float_returns() {
    assert!(DirectLeafCall::from_member("Owner", "f", "(II)V").is_none());
    assert!(DirectLeafCall::from_member("Owner", "f", "(I)D").is_none());
    assert!(DirectLeafCall::from_member("Owner", "f", "(I)F").is_none());
    let code = leaf_code(1, 1, &[0xac]);
    assert!(!is_no_gc_primitive_leaf("(I)V", 0x0008, &code));
}

#[test]
fn rejects_bad_access_flags() {
    let code = leaf_code(2, 2, &[0x1a, 0x1b, 0x60, 0xac]);
    // Not static.
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0000, &code));
    // Static + synchronized / native / abstract / bridge / varargs / synthetic.
    for flags in [0x0028u16, 0x0108, 0x0408, 0x0048, 0x0088, 0x1008] {
        assert!(
            !is_no_gc_primitive_leaf("(II)I", flags, &code),
            "flags {flags:#x}"
        );
    }
}

#[test]
fn rejects_exception_handlers() {
    let mut code = leaf_code(2, 2, &[0x1a, 0x1b, 0x60, 0xac]);
    code.exceptions.push(rava_classfile::ExceptionHandler {
        start_pc: 0,
        end_pc: 2,
        handler_pc: 3,
        catch_type: 0,
    });
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &code));
}

#[test]
fn rejects_calls_and_gc_or_faulting_callbacks() {
    // invokestatic / invokedynamic-shaped call in the body.
    let call = leaf_code(2, 2, &[0x1a, 0x1b, 0xb8, 0x00, 0x01, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &call));
    // invokeinterface opcode.
    let iface = leaf_code(2, 2, &[0x1a, 0x1b, 0xb9, 0x00, 0x01, 0x01, 0x00, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &iface));
    // getstatic (class-init / GC callback).
    let getstatic = leaf_code(2, 2, &[0xb2, 0x00, 0x01, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &getstatic));
    // newarray (allocation).
    let alloc = leaf_code(2, 2, &[0x1a, 0xbc, 0x0a, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &alloc));
    // athrow.
    let throw = leaf_code(2, 2, &[0x1a, 0xbf]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &throw));
    // idiv (divide-by-zero trap needs runtime machinery).
    let div = leaf_code(2, 2, &[0x1a, 0x1b, 0x6c, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &div));
    // monitorenter.
    let monitor = leaf_code(2, 2, &[0x1a, 0xc2, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &monitor));
}

#[test]
fn rejects_backwards_branches() {
    // goto -5 from pc 3 → negative target, out of body.
    let neg = leaf_code(2, 2, &[0x1a, 0x1b, 0x60, 0xa7, 0xff, 0xfb, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &neg));
    // goto -1 from pc 0 → target lands at/before pc.
    let self_loop = leaf_code(2, 2, &[0xa7, 0xff, 0xff]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &self_loop));
    // goto_w backwards.
    let wide_back = leaf_code(2, 2, &[0x1a, 0xc8, 0xff, 0xff, 0xff, 0xf8, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &wide_back));
}

#[test]
fn rejects_malformed_descriptors() {
    for bad in [
        "", "(", "(I", "(II", "()", "(II)IJ", "(II)I ", "I(II)I", "(IK)I", "(ID)I", ")I(",
    ] {
        assert!(
            DirectLeafCall::from_member("Owner", "f", bad).is_none(),
            "{bad:?}"
        );
    }
}

#[test]
fn rejects_over_arity() {
    assert!(DirectLeafCall::from_member("Owner", "f", "(IIIIII)I").is_none());
    let code = leaf_code(6, 6, &[0xac]);
    assert!(!is_no_gc_primitive_leaf("(IIIIII)I", 0x0008, &code));
}

#[test]
fn rejects_oversized_and_unterminated_bodies() {
    // One byte over the conservative body budget.
    let big = leaf_code(2, 2, &vec![0x00u8; super::MAX_LEAF_BODY + 1]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &big));
    // Runs off the end without a return.
    let unterminated = leaf_code(2, 2, &[0x1a, 0x1b]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &unterminated));
    // A truncated operand must refuse rather than index past the bytecode.
    let truncated = leaf_code(1, 1, &[0x10]);
    assert!(!is_no_gc_primitive_leaf("(I)I", 0x0008, &truncated));
}

#[test]
fn rejects_local_stores_and_non_param_loads() {
    // istore 0 (no locals buffer under the direct ABI).
    let store = leaf_code(4, 2, &[0x1a, 0x36, 0x00, 0x1b, 0x60, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &store));
    // iinc mutates a local.
    let iinc = leaf_code(2, 2, &[0x84, 0x00, 0x01, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(I)I", 0x0008, &iinc));
    // iload of a non-parameter slot.
    let stray = leaf_code(4, 2, &[0x15, 0x02, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &stray));
    // lload of a category-2 high half slot.
    let high_half = leaf_code(4, 4, &[0x1f, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(JJ)J", 0x0008, &high_half));
}

#[test]
fn rejects_local_kind_mismatches() {
    // iload from a long parameter's start slot (param 0 is J; iload_0 must fail).
    let iload_long_start = leaf_code(4, 4, &[0x1a, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(JJ)J", 0x0008, &iload_long_start));
    // Indexed iload from a long start slot.
    let iload_long_start_indexed = leaf_code(4, 4, &[0x15, 0x00, 0xac]);
    assert!(!is_no_gc_primitive_leaf(
        "(JJ)J",
        0x0008,
        &iload_long_start_indexed
    ));
    // lload from an int/boolean parameter slot (param 0 is I; lload 0 must fail)
    // even though max_locals has spare slots beyond it.
    let lload_int_slot = leaf_code(4, 4, &[0x16, 0x00, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(II)J", 0x0008, &lload_int_slot));
    // lload from a boolean parameter slot.
    let lload_bool_slot = leaf_code(4, 4, &[0x16, 0x00, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(Z)J", 0x0008, &lload_bool_slot));
    // lload from the category-2 high half is still refused (map keeps it None).
    let high_half = leaf_code(4, 4, &[0x16, 0x01, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(JJ)J", 0x0008, &high_half));
    // Short-form lload_2 with (J,J): slot 2 is the SECOND long's start — valid.
    let second_long = leaf_code(4, 4, &[0x20, 0xad]);
    assert!(is_no_gc_primitive_leaf("(JJ)J", 0x0008, &second_long));
    // Slot beyond the parameters entirely.
    let beyond = leaf_code(6, 4, &[0x16, 0x04, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(JJ)J", 0x0008, &beyond));
}

#[test]
fn rejects_stack_type_mismatches_and_overfull_stacks() {
    // Long value returned through ireturn.
    let wrong_kind = leaf_code(2, 2, &[0x0a, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(I)I", 0x0008, &wrong_kind));
    // max_stack under-declares the needed depth.
    let overfull = leaf_code(2, 0, &[0x1a, 0x1b, 0x60, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(II)I", 0x0008, &overfull));
}

#[test]
fn rejects_return_op_vs_declared_result_mismatch() {
    // (I)J with iconst_0; ireturn — int value through a long-return descriptor.
    let int_via_long = leaf_code(2, 2, &[0x03, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(I)J", 0x0008, &int_via_long));
    // (I)I with lconst_0; lreturn — long value through an int-return descriptor.
    let long_via_int = leaf_code(2, 2, &[0x0a, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(I)I", 0x0008, &long_via_int));
    // (I)Z with ireturn IS valid (boolean is an int-kind result).
    let bool_via_ireturn = leaf_code(2, 2, &[0x1a, 0xac]);
    assert!(is_no_gc_primitive_leaf("(I)Z", 0x0008, &bool_via_ireturn));
    // (I)I with lreturn refused even when the operand would type-check as long.
    let lreturn_wrong_desc = leaf_code(2, 2, &[0x0a, 0xad]);
    assert!(!is_no_gc_primitive_leaf(
        "(I)I",
        0x0008,
        &lreturn_wrong_desc
    ));
    // (I)Z with lreturn refused (boolean cannot come out of lreturn).
    let lreturn_bool = leaf_code(2, 2, &[0x0a, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(I)Z", 0x0008, &lreturn_bool));
}

#[test]
fn rejects_max_stack_counting_long_as_one_slot() {
    // A long occupies TWO stack slots: lload_0 needs max_stack >= 2.
    // One entry only (max_stack=1) must be rejected even though it "fits".
    let one_long = leaf_code(2, 1, &[0x1e, 0xad]);
    assert!(!is_no_gc_primitive_leaf("(J)J", 0x0008, &one_long));
    // With max_stack=2 the same body is admissible.
    let two_slots = leaf_code(2, 2, &[0x1e, 0xad]);
    assert!(is_no_gc_primitive_leaf("(J)J", 0x0008, &two_slots));
    // Two longs live simultaneously: 4 slots needed.
    let two_longs = leaf_code(4, 3, &[0x1e, 0x20, 0x94, 0xac]);
    assert!(!is_no_gc_primitive_leaf("(JJ)I", 0x0008, &two_longs));
    let two_longs_ok = leaf_code(4, 4, &[0x1e, 0x20, 0x94, 0xac]);
    let mut ok_stack = two_longs_ok;
    ok_stack.max_stack = 4;
    assert!(is_no_gc_primitive_leaf("(JJ)I", 0x0008, &ok_stack));
}

#[test]
fn from_member_candidate_and_initializer_rejection() {
    assert!(DirectLeafCall::from_member("Owner", "f", "(I)I").is_some());
    assert!(DirectLeafCall::from_member("Owner", "f", "(JJ)Z").is_some());
    assert!(DirectLeafCall::from_member("Owner", "<init>", "(I)V").is_none());
    assert!(DirectLeafCall::from_member("Owner", "<clinit>", "()I").is_none());
    assert!(DirectLeafCall::from_member("", "f", "(I)I").is_none());
    assert!(DirectLeafCall::from_member("Owner", "", "(I)I").is_none());
}
