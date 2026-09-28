//! Control flow, comparisons, switches and the register ABI.

use super::*;

#[test]
fn jit_detects_verify_unsafe_bytecodes() {
    assert!(has_verify_unsafe_ops(&code(
        vec![0x2a, 0x03, 0x04, 0x4f, 0xb1],
        1
    )));
    assert!(has_verify_unsafe_ops(&code(
        vec![0x2a, 0x03, 0x0f, 0x52, 0xb1],
        1
    )));
    assert!(has_verify_unsafe_ops(&code(
        vec![0x2a, 0xb6, 0x00, 0x01, 0xac],
        1
    )));
    assert!(has_verify_unsafe_ops(&code(
        vec![0xb2, 0x00, 0x01, 0xac],
        1
    )));
    assert!(!has_verify_unsafe_ops(&code(
        vec![0x2a, 0x03, 0x2e, 0xac],
        1
    )));
}

#[test]
fn jit_dup_x1() {
    // f(a,b): iload_0; iload_1; dup_x1; iadd; iadd; ireturn
    // stack: [a] [a,b] -dup_x1-> [b,a,b] -iadd-> [b,a+b] -iadd-> [a+2b]
    let bc = vec![0x1a, 0x1b, 0x5a, 0x60, 0x60, 0xac];
    let c = code(bc, 2);
    let f = compile(&c, "(II)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b) in [(1i32, 2i32), (10, -3), (0, 0), (100, 100), (-5, 7)] {
        let mut locals = vec![a as i64, b as i64];
        let r = unsafe { f.call(&mut locals) } as i32;
        assert_eq!(r, a.wrapping_add(2i32.wrapping_mul(b)), "dup_x1 f({a},{b})");
    }
}

#[test]
fn jit_sum_loop() {
    // int sum(int n){ int s=0; for(int i=0;i<n;i++) s+=i; return s; }
    // locals: 0=n, 1=s, 2=i
    let bc = vec![
        0x03, 0x3c, // 0: iconst_0; istore_1
        0x03, 0x3d, // 2: iconst_0; istore_2
        0x1c, 0x1a, 0xa2, 0x00, 0x0d, // 4: iload_2; iload_0; if_icmpge +13 -> 19
        0x1b, 0x1c, 0x60, 0x3c, // 9: iload_1; iload_2; iadd; istore_1
        0x84, 0x02, 0x01, // 13: iinc 2,1
        0xa7, 0xff, 0xf4, // 16: goto -12 -> 4
        0x1b, 0xac, // 19: iload_1; ireturn
    ];
    let c = code(bc, 3);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    assert_eq!(f.ret, Some(JT::Int));
    for n in [0i32, 1, 5, 100, 1000] {
        let mut locals = vec![n as i64, 0, 0];
        let r = unsafe { f.call(&mut locals) } as i32;
        let want: i32 = (0..n).sum();
        assert_eq!(r, want, "sum(0..{n})");
    }
}

#[test]
fn jit_tableswitch() {
    // int f(int x){ switch(x){case 0:return 10;case 1:return 11;case 2:return 12;
    //               default:return 99;} }  — tableswitch, default aligned to pc 4.
    let bc = vec![
        0x1a, // 0: iload_0
        0xaa, // 1: tableswitch
        0x00, 0x00, // 2-3: padding (defaultbyte at pc 4)
        0x00, 0x00, 0x00, 0x24, // 4-7: default = +36 -> pc 37
        0x00, 0x00, 0x00, 0x00, // 8-11: low = 0
        0x00, 0x00, 0x00, 0x02, // 12-15: high = 2
        0x00, 0x00, 0x00, 0x1b, // 16-19: case 0 = +27 -> pc 28
        0x00, 0x00, 0x00, 0x1e, // 20-23: case 1 = +30 -> pc 31
        0x00, 0x00, 0x00, 0x21, // 24-27: case 2 = +33 -> pc 34
        0x10, 0x0a, 0xac, // 28-30: bipush 10; ireturn
        0x10, 0x0b, 0xac, // 31-33: bipush 11; ireturn
        0x10, 0x0c, 0xac, // 34-36: bipush 12; ireturn
        0x10, 0x63, 0xac, // 37-39: bipush 99; ireturn (default)
    ];
    let c = code(bc, 1);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled tableswitch");
    let call = |x: i32| -> i32 {
        let mut l = vec![x as i64];
        unsafe { f.call(&mut l) as i32 }
    };
    assert_eq!(call(0), 10);
    assert_eq!(call(1), 11);
    assert_eq!(call(2), 12);
    assert_eq!(call(5), 99, "above high -> default");
    assert_eq!(call(-1), 99, "below low -> default");
}

#[test]
fn jit_lookupswitch_with_negative_case() {
    // int g(int x){ switch(x){case -5:return 50;case 100:return 60;default:return 99;} }
    // lookupswitch — pairs sorted ascending; the NEGATIVE label is the sign gate.
    let bc = vec![
        0x1a, // 0: iload_0
        0xab, // 1: lookupswitch
        0x00, 0x00, // 2-3: padding
        0x00, 0x00, 0x00, 0x21, // 4-7: default = +33 -> pc 34
        0x00, 0x00, 0x00, 0x02, // 8-11: npairs = 2
        0xFF, 0xFF, 0xFF, 0xFB, // 12-15: match = -5
        0x00, 0x00, 0x00, 0x1b, // 16-19: offset = +27 -> pc 28
        0x00, 0x00, 0x00, 0x64, // 20-23: match = 100
        0x00, 0x00, 0x00, 0x1e, // 24-27: offset = +30 -> pc 31
        0x10, 0x32, 0xac, // 28-30: bipush 50; ireturn (case -5)
        0x10, 0x3c, 0xac, // 31-33: bipush 60; ireturn (case 100)
        0x10, 0x63, 0xac, // 34-36: bipush 99; ireturn (default)
    ];
    let c = code(bc, 1);
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled lookupswitch");
    let call = |x: i32| -> i32 {
        let mut l = vec![x as i64];
        unsafe { f.call(&mut l) as i32 }
    };
    assert_eq!(call(-5), 50, "negative case label matches");
    assert_eq!(call(100), 60);
    assert_eq!(call(0), 99, "no match -> default");
    assert_eq!(call(-1), 99, "near-but-not -5 -> default");
}

#[test]
fn jit_compiles_reference_return_identity() {
    // Object id(Object o){ return o; }  aload_0; areturn
    let c = code(vec![0x2a, 0xb0], 1);
    let f = compile(
        &c,
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &empty_pool(),
        0,
        &mut |_, _| (0, None),
    )
    .expect("reference-returning method should compile");
    assert_eq!(f.ret, Some(JT::Ref), "return type tracked as reference");
    // A tagged object handle round-trips verbatim through the result cell.
    let cell = pack_local(&JVal::Ref(42));
    let mut locals = vec![cell];
    assert_eq!(unsafe { f.call(&mut locals) }, cell);
    // null (cell 0) round-trips as 0.
    let mut nulls = vec![0i64];
    assert_eq!(unsafe { f.call(&mut nulls) }, 0);
}

#[test]
fn jit_compiles_if_acmp_branch() {
    // Object pick(Object a, Object b){ return a==b ? a : b; }
    //  aload_0; aload_1; if_acmpeq L; aload_1; areturn; L: aload_0; areturn
    let c = code(
        vec![0x2a, 0x2b, 0xa5, 0x00, 0x05, 0x2b, 0xb0, 0x2a, 0xb0],
        2,
    );
    let f = compile(
        &c,
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        &empty_pool(),
        0,
        &mut |_, _| (0, None),
    )
    .expect("if_acmp method should compile");
    assert_eq!(f.ret, Some(JT::Ref));
    // (Runtime identity is exercised via the boot path: the callback needs a
    // live VM, so this test asserts compilation accepts if_acmp + areturn.)
}

#[test]
fn jit_dcmpg_nan() {
    // int f(double x, double y){ return dcmpg(x,y); }  locals 0=x,1=y
    // dload_0(0x26); dload_1(0x27); dcmpg(0x98); ireturn(0xac)
    let c = code(vec![0x26, 0x27, 0x98, 0xac], 2);
    let f = compile(&c, "(DD)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    let call = |x: f64, y: f64| -> i32 {
        let mut l = vec![x.to_bits() as i64, y.to_bits() as i64];
        unsafe { f.call(&mut l) as i32 }
    };
    assert_eq!(call(1.0, 2.0), -1);
    assert_eq!(call(2.0, 1.0), 1);
    assert_eq!(call(2.0, 2.0), 0);
    assert_eq!(call(f64::NAN, 1.0), 1); // dcmpg: NaN -> 1
    assert_eq!(call(1.0, f64::NAN), 1);
}

#[test]
fn jit_dcmpl_nan() {
    let c = code(vec![0x26, 0x27, 0x97, 0xac], 2);
    let f = compile(&c, "(DD)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    let call = |x: f64, y: f64| -> i32 {
        let mut locals = vec![x.to_bits() as i64, y.to_bits() as i64];
        unsafe { f.call(&mut locals) as i32 }
    };
    assert_eq!(call(1.0, 2.0), -1);
    assert_eq!(call(2.0, 1.0), 1);
    assert_eq!(call(2.0, 2.0), 0);
    assert_eq!(call(f64::NAN, 1.0), -1);
    assert_eq!(call(1.0, f64::NAN), -1);
}

// Register-ABI arc (I1a): a pure-primitive leaf compiled with the register entry
// `fn(array_desc_cell, f64) -> f64` computes bit-identically to the memory-locals ABI.
// Register-ABI arc (I1a): a pure-primitive leaf compiled with the register entry
// `fn(array_desc_cell, f64) -> f64` computes bit-identically to the memory-locals ABI.
#[test]
fn jit_register_abi_leaf_double() {
    // double f(double x){ return x*x + 1.0; }
    //   dload_0; dload_0; dmul; dconst_1; dadd; dreturn
    let c = code(vec![0x26, 0x26, 0x6b, 0x0f, 0x63, 0xaf], 2);
    let fr = compile_register(&c, "(D)D", &empty_pool(), 0, false, &mut |_, _| (0, None))
        .expect("register-ABI leaf should compile");
    assert!(fr.reg.is_some(), "carries the register shape");
    let reg: extern "C" fn(i64, f64) -> f64 = unsafe { std::mem::transmute(fr.raw()) };
    let fm = compile(&c, "(D)D", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    for x in [0.0_f64, 1.5, -2.25, 3.0, 1e6, -0.0] {
        let got = reg(0, x);
        let want = x * x + 1.0;
        assert_eq!(got.to_bits(), want.to_bits(), "register f({x})");
        let mut l = vec![x.to_bits() as i64, 0];
        let mbits = unsafe { fm.call(&mut l) } as u64;
        assert_eq!(got.to_bits(), mbits, "register == memory ABI for f({x})");
    }
}

// Register-ABI arc (I1a): a leaf with an integer descriptor round-trips via i32 regs.
// Register-ABI arc (I1a): a leaf with an integer descriptor round-trips via i32 regs.
#[test]
fn jit_register_abi_leaf_int() {
    // int f(int a, int b){ return a*b + a; }  iload_0; iload_1; imul; iload_0; iadd; ireturn
    let c = code(vec![0x1a, 0x1b, 0x68, 0x1a, 0x60, 0xac], 2);
    let fr = compile_register(&c, "(II)I", &empty_pool(), 0, false, &mut |_, _| (0, None))
        .expect("register-ABI int leaf should compile");
    let reg: extern "C" fn(i64, i32, i32) -> i32 = unsafe { std::mem::transmute(fr.raw()) };
    for (a, b) in [(3, 4), (-2, 5), (0, 9), (7, -1)] {
        assert_eq!(reg(0, a, b), a * b + a, "register f({a},{b})");
    }
}

// The reference extension of the register-ABI classifier. `allow_ref=false` must stay
// EXACTLY the old all-primitive predicate (the default build depends on it being
// byte-neutral); `allow_ref=true` admits `L…;`/`[…` params and returns as one narrow
// register operand each.
// The reference extension of the register-ABI classifier. `allow_ref=false` must stay
// EXACTLY the old all-primitive predicate (the default build depends on it being
// byte-neutral); `allow_ref=true` admits `L…;`/`[…` params and returns as one narrow
// register operand each.
#[test]
fn reg_abi_desc_reference_params_and_returns() {
    // Rejected without the widening, accepted with it.
    for d in ["(LNode;)I", "(I)Ljava/lang/String;", "([I)I", "([[Ljava/lang/Object;)J"] {
        assert!(all_primitive_desc(d).is_none(), "{d} must stay out of the primitive ABI");
        assert!(reg_abi_desc(d, true).is_some(), "{d} should be reference-eligible");
    }
    // A reference is ONE narrow slot, and an array of long/double must not be read as
    // a wide scalar — the classic descriptor-walker trap.
    let a = reg_abi_desc("(Ljava/lang/String;J[DI)V", true);
    assert!(a.is_none(), "void return is still refused");
    let a = reg_abi_desc("(Ljava/lang/String;J[DI)I", true).expect("params parse");
    assert_eq!(a.params, vec![PrimKind::Ref, PrimKind::I64, PrimKind::Ref, PrimKind::I32]);
    assert_eq!(a.params.iter().map(|k| k.slots()).sum::<usize>(), 5, "J takes 2, [D takes 1");
    assert!(a.carries_ref());
    assert_eq!(a.ret, PrimKind::I32);
    // A reference return is detected even with an all-primitive parameter list.
    let r = reg_abi_desc("(II)LBigGraph$Node;", true).expect("ref return parses");
    assert!(r.params.iter().all(|k| !k.is_ref()));
    assert!(r.carries_ref(), "the RETURN alone makes it reference-carrying");
    assert_eq!(r.ret, PrimKind::Ref);
    // An all-primitive descriptor is classified identically either way.
    for d in ["(II)I", "(D)D", "(JF)Z", "()I"] {
        assert_eq!(
            format!("{:?}", all_primitive_desc(d)),
            format!("{:?}", reg_abi_desc(d, true)),
            "{d} must classify identically with and without the widening",
        );
    }
    // Malformed descriptors are refused rather than mis-parsed.
    for d in ["(LNode)I", "([)I", "(I", "(V)I"] {
        assert!(reg_abi_desc(d, true).is_none(), "{d} must be refused");
    }
}

#[test]
fn jit_fcmpg_ifge_preserves_ordered_and_nan_branches() {
    // return x < y ? 1 : 0, javac shape: fcmpg; ifge false.
    let c = code(
        vec![0x22, 0x23, 0x96, 0x9c, 0x00, 0x05, 0x04, 0xac, 0x03, 0xac],
        2,
    );
    let f = compile(&c, "(FF)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    let call = |x: f32, y: f32| -> i32 {
        let mut locals = vec![x.to_bits() as i64, y.to_bits() as i64];
        unsafe { f.call(&mut locals) as i32 }
    };
    assert_eq!(call(1.0, 2.0), 1);
    assert_eq!(call(2.0, 1.0), 0);
    assert_eq!(call(2.0, 2.0), 0);
    assert_eq!(call(f32::NAN, 1.0), 0);
    assert_eq!(call(1.0, f32::NAN), 0);
}

#[test]
fn jit_ifnull_branches_on_reference_cells() {
    let c = code(vec![0x2a, 0xc6, 0x00, 0x05, 0x05, 0xac, 0x04, 0xac], 1);
    let f =
        compile(&c, "(Ljava/lang/Object;)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");

    let mut null_local = vec![0];
    assert_eq!(unsafe { f.call(&mut null_local) } as i32, 1);

    let mut ref_local = vec![pack_local(&JVal::Ref(123))];
    assert_eq!(unsafe { f.call(&mut ref_local) } as i32, 2);
}

#[test]
fn jit_ifnonnull_branches_on_reference_cells() {
    let c = code(vec![0x2a, 0xc7, 0x00, 0x05, 0x04, 0xac, 0x05, 0xac], 1);
    let f =
        compile(&c, "(Ljava/lang/Object;)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");

    let mut null_local = vec![0];
    assert_eq!(unsafe { f.call(&mut null_local) } as i32, 1);

    let mut ref_local = vec![pack_local(&JVal::Array(456))];
    assert_eq!(unsafe { f.call(&mut ref_local) } as i32, 2);
}

#[test]
fn jit_branch_join_preserves_non_empty_operand_stack() {
    // int f(int flag) { return 10 + (flag == 0 ? 1 : 2); }
    // The constant 10 is already on the operand stack while ifeq chooses
    // the second addend; both paths join at iadd with stack [10, chosen].
    let c = code(
        vec![
            0x10, 0x0a, // 0: bipush 10
            0x1a, // 2: iload_0
            0x99, 0x00, 0x07, // 3: ifeq 10
            0x05, // 6: iconst_2
            0xa7, 0x00, 0x04, // 7: goto 11
            0x04, // 10: iconst_1
            0x60, // 11: iadd
            0xac, // 12: ireturn
        ],
        1,
    );
    let f = compile(&c, "(I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");

    let mut zero = vec![0];
    assert_eq!(unsafe { f.call(&mut zero) } as i32, 11);

    let mut nonzero = vec![7];
    assert_eq!(unsafe { f.call(&mut nonzero) } as i32, 12);
}
