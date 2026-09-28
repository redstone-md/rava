//! Arithmetic semantics: the JVM division, remainder and overflow rules, including the trapping cases.

use super::*;

#[test]
fn jit_double_math() {
    // double f(double x){ return x*x + x; }  (double = 1 slot in this VM)
    let bc = vec![0x26, 0x26, 0x6b, 0x26, 0x63, 0xaf];
    let c = code(bc, 2);
    let f = compile(&c, "(D)D", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    assert_eq!(f.ret, Some(JT::Double));
    for x in [0.0f64, 1.5, -3.25, 100.0] {
        let mut locals = vec![x.to_bits() as i64, 0];
        let r = f64::from_bits(unsafe { f.call(&mut locals) } as u64);
        assert!((r - (x * x + x)).abs() < 1e-9, "f({x})={r}");
    }
}

#[test]
fn jit_double_div() {
    // double f(double x){ return x / 16.0 ... } use ddiv: dload_0; ldc2_w not
    // supported, so build x/x = 1 via dload_0;dload_0;ddiv;dreturn
    let bc = vec![0x26, 0x26, 0x6f, 0xaf];
    let c = code(bc, 2);
    let f = compile(&c, "(D)D", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for x in [1.0f64, 2.0, -4.0, 0.5] {
        let mut locals = vec![x.to_bits() as i64, 0];
        let r = f64::from_bits(unsafe { f.call(&mut locals) } as u64);
        assert!((r - 1.0).abs() < 1e-12, "x/x for {x} = {r}");
    }
}

#[test]
fn jit_int_division_matches_jvm_overflow_rule() {
    let c = code(vec![0x1a, 0x1b, 0x6c, 0xac], 2); // iload_0; iload_1; idiv; ireturn
    let f = compile(&c, "(II)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b, want) in [
        (7, 3, 2),
        (-7, 3, -2),
        (7, -3, -2),
        (-7, -3, 2),
        (i32::MIN, -1, i32::MIN),
    ] {
        let mut locals = vec![a as i64, b as i64];
        let got = unsafe { f.call(&mut locals) } as i32;
        assert_eq!(got, want, "{a} / {b}");
    }
}

#[test]
fn jit_int_remainder_matches_jvm_sign_rules() {
    let c = code(vec![0x1a, 0x1b, 0x70, 0xac], 2); // iload_0; iload_1; irem; ireturn
    let f = compile(&c, "(II)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b, want) in [
        (7, 3, 1),
        (-7, 3, -1),
        (7, -3, 1),
        (-7, -3, -1),
        (i32::MIN, -1, 0),
    ] {
        let mut locals = vec![a as i64, b as i64];
        let got = unsafe { f.call(&mut locals) } as i32;
        assert_eq!(got, want, "{a} % {b}");
    }
}

#[test]
fn jit_int_remainder_by_zero_sets_pending_exception() {
    let c = code(vec![0x1a, 0x1b, 0x70, 0xac], 2); // iload_0; iload_1; irem; ireturn
    let f = compile(&c, "(II)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![7, 0];
    let got = unsafe { f.call(&mut locals) } as i32;
    set_current_vm(prev);
    assert_eq!(got, 0);
    match vm.jit_pending_take() {
        Some(JVal::Ref(h)) => assert!(
            matches!(vm.heap.get_field(h, "$message"), JVal::Str(ref s) if s.as_ref() == "/ by zero")
        ),
        other => panic!("expected pending ArithmeticException, got {other:?}"),
    }
}

#[test]
fn jit_long_division_matches_jvm_overflow_rule() {
    // lload_0; lload_2; ldiv; lreturn
    let c = code(vec![0x1e, 0x20, 0x6d, 0xad], 4);
    let f = compile(&c, "(JJ)J", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b, want) in [
        (7i64, 3i64, 2i64),
        (-7, 3, -2),
        (7, -3, -2),
        (-7, -3, 2),
        (i64::MIN, -1, i64::MIN),
    ] {
        let mut locals = vec![a, 0, b, 0];
        let got = unsafe { f.call(&mut locals) };
        assert_eq!(got, want, "{a} / {b}");
    }
}

#[test]
fn jit_adjacent_constant_idiv_uses_jvm_semantics() {
    // iload_0; iconst_2; idiv; ireturn
    let c = code(vec![0x1a, 0x05, 0x6c, 0xac], 1);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (input, expected) in [(7i32, 3i32), (-7, -3), (i32::MIN, i32::MIN / 2)] {
        let mut locals = vec![i64::from(input)];
        assert_eq!(unsafe { f.call(&mut locals) as i32 }, expected);
    }

    let leaders = std::collections::HashMap::new();
    assert_eq!(adjacent_i32_divisor(&[0x1a, 0x10, 0x07, 0x6c], 3, &leaders), Some(7));
    assert_eq!(adjacent_i32_divisor(&[0x1a, 0x03, 0x6c], 2, &leaders), None);
}

#[test]
fn positive_div_range_requires_same_local_guard_and_unique_fallthrough() {
    let guarded = [
        0x1a, 0x9e, 0x00, 0x07, // iload_0; ifle 8
        0x1a, 0x05, 0x6c, 0xac, // iload_0; iconst_2; idiv; ireturn
        0x03, 0xac, // iconst_0; ireturn
    ];
    assert!(guarded_nonnegative_i32_dividend(&guarded, 6));

    let wrong_local = [
        0x1b, 0x9e, 0x00, 0x07, 0x1a, 0x05, 0x6c, 0xac, 0x03, 0xac,
    ];
    assert!(!guarded_nonnegative_i32_dividend(&wrong_local, 6));

    let alternate_entry = [
        0x1a, 0x9e, 0x00, 0x0a, // guard
        0x1a, 0x05, 0x6c, 0xac, // candidate at pc 6
        0xa7, 0xff, 0xfc, // goto pc 4: alternate explicit entry
        0x03, 0xac,
    ];
    assert!(!guarded_nonnegative_i32_dividend(&alternate_entry, 6));

    let enter_guard = [
        0x1a, 0x9e, 0x00, 0x0b, // candidate guard at pc 1
        0x1a, 0x05, 0x6c, 0xac, // candidate div at pc 6
        0x04, 0xa7, 0xff, 0xf8, // iconst_1; goto pc 1
        0x03, 0xac,
    ];
    assert!(!guarded_nonnegative_i32_dividend(&enter_guard, 6));

    let enter_divisor = [
        0x1a, 0x9e, 0x00, 0x0b, // guard
        0x1a, 0x05, 0x6c, 0xac, // divisor producer at pc 5
        0x04, 0xa7, 0xff, 0xfc, // iconst_1; goto pc 5
        0x03, 0xac,
    ];
    assert!(!guarded_nonnegative_i32_dividend(&enter_divisor, 6));
}

#[test]
fn jit_long_remainder_matches_jvm_sign_rules() {
    // lload_0; lload_2; lrem; lreturn
    let c = code(vec![0x1e, 0x20, 0x71, 0xad], 4);
    let f = compile(&c, "(JJ)J", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b, want) in [
        (7i64, 3i64, 1i64),
        (-7, 3, -1),
        (7, -3, 1),
        (-7, -3, -1),
        (i64::MIN, -1, 0),
    ] {
        let mut locals = vec![a, 0, b, 0];
        let got = unsafe { f.call(&mut locals) };
        assert_eq!(got, want, "{a} % {b}");
    }
}

#[test]
fn jit_long_division_by_zero_sets_pending_exception() {
    let c = code(vec![0x1e, 0x20, 0x6d, 0xad], 4); // lload_0; lload_2; ldiv; lreturn
    let f = compile(&c, "(JJ)J", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![7i64, 0, 0, 0];
    let got = unsafe { f.call(&mut locals) };
    set_current_vm(prev);
    assert_eq!(got, 0);
    match vm.jit_pending_take() {
        Some(JVal::Ref(h)) => assert!(
            matches!(vm.heap.get_field(h, "$message"), JVal::Str(ref s) if s.as_ref() == "/ by zero")
        ),
        other => panic!("expected pending ArithmeticException, got {other:?}"),
    }
}

#[test]
fn jit_double_remainder_truncates() {
    // dload_0; dload_2; drem; dreturn
    let c = code(vec![0x26, 0x28, 0x73, 0xaf], 4);
    let f = compile(&c, "(DD)D", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b) in [(7.5f64, 2.0f64), (-7.5, 2.0), (5.3, 2.0), (7.5, -2.0)] {
        let mut locals = vec![a.to_bits() as i64, 0, b.to_bits() as i64, 0];
        let r = f64::from_bits(unsafe { f.call(&mut locals) } as u64);
        assert!((r - (a % b)).abs() < 1e-12, "{a} % {b} = {r}");
    }
}

#[test]
fn jit_float_remainder_truncates() {
    // fload_0; fload_1; frem; freturn
    let c = code(vec![0x22, 0x23, 0x72, 0xae], 2);
    let f = compile(&c, "(FF)F", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b) in [(7.5f32, 2.0f32), (-7.5, 2.0), (5.25, 2.0)] {
        let mut locals = vec![a.to_bits() as i64, b.to_bits() as i64];
        let r = f32::from_bits(unsafe { f.call(&mut locals) } as u32);
        assert!((r - (a % b)).abs() < 1e-6, "{a} % {b} = {r}");
    }
}
