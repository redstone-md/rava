//! Static-intrinsic recognition and emission: sqrt, the max family, the lerp family, int bits.

use super::*;

#[test]
fn jit_math_sqrt() {
    // double f(double x){ return Math.sqrt(x) + Math.floor(x); }
    // pool: 1=Utf8"java/lang/Math" 2=Class(1) 3=Utf8"sqrt" 4=Utf8"(D)D"
    //       5=NameAndType(3,4) 6=MethodRef(2,5) 7=Utf8"floor" 8=NaT(7,4) 9=MethodRef(2,8)
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("java/lang/Math".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("sqrt".into()),
        Constant::Utf8("(D)D".into()),
        Constant::NameAndType {
            name_index: 3,
            desc_index: 4,
        },
        Constant::MethodRef {
            class_index: 2,
            nat_index: 5,
        },
        Constant::Utf8("floor".into()),
        Constant::NameAndType {
            name_index: 7,
            desc_index: 4,
        },
        Constant::MethodRef {
            class_index: 2,
            nat_index: 8,
        },
    ]);
    // dload_0; invokestatic #6 (sqrt); dload_0; invokestatic #9 (floor); dadd; dreturn
    let bc = vec![0x26, 0xb8, 0x00, 0x06, 0x26, 0xb8, 0x00, 0x09, 0x63, 0xaf];
    let c = code(bc, 2);
    let f = compile(&c, "(D)D", &pool, 0, &mut |_, _| (0, None)).expect("compiled");
    for x in [4.0f64, 2.25, 100.0, 0.0] {
        let mut l = vec![x.to_bits() as i64, 0];
        let r = f64::from_bits(unsafe { f.call(&mut l) } as u64);
        let want = x.sqrt() + x.floor();
        assert!((r - want).abs() < 1e-9, "f({x})={r} want {want}");
    }
}

#[test]
fn jit_math_max_long_exact_and_round_half_up() {
    // long f(long a, long b){ return Math.max(a, b); } — must be EXACT above
    // 2^53 (the interpreter's old f64 roundtrip lost low bits).
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("java/lang/Math".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("max".into()),
        Constant::Utf8("(JJ)J".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::MethodRef { class_index: 2, nat_index: 5 },
        Constant::Utf8("round".into()),
        Constant::Utf8("(D)J".into()),
        Constant::NameAndType { name_index: 7, desc_index: 8 },
        Constant::MethodRef { class_index: 2, nat_index: 9 },
    ]);
    // lload_0; lload_2; invokestatic #6 (max); lreturn
    let c = code(vec![0x1e, 0x20, 0xb8, 0x00, 0x06, 0xad], 4);
    let f = compile(&c, "(JJ)J", &pool, 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, b) in [
        (i64::MAX, i64::MAX - 1),
        ((1i64 << 53) + 1, 1i64 << 53),
        (-5, 3),
        (i64::MIN, i64::MIN + 1),
    ] {
        let mut l = vec![a, 0, b, 0];
        let r = unsafe { f.call(&mut l) };
        assert_eq!(r, a.max(b), "max({a},{b})");
    }
    // long g(double x){ return Math.round(x); } — Java HALF-UP semantics.
    let c = code(vec![0x26, 0xb8, 0x00, 0x0a, 0xad], 2);
    let g = compile(&c, "(D)J", &pool, 0, &mut |_, _| (0, None)).expect("compiled");
    let call = |x: f64| -> i64 {
        let mut l = vec![x.to_bits() as i64, 0];
        unsafe { g.call(&mut l) }
    };
    assert_eq!(call(2.5), 3);
    assert_eq!(call(-2.5), -2, "Java rounds half toward +inf");
    assert_eq!(call(0.49999999999999994), 0, "JDK-7 rounding fix case");
    assert_eq!(call(f64::NAN), 0);
    assert_eq!(call(f64::INFINITY), i64::MAX);
    assert_eq!(call(f64::NEG_INFINITY), i64::MIN);
}

#[test]
fn jit_intbits_rotate_and_to_unsigned_long() {
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("java/lang/Long".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("rotateLeft".into()),
        Constant::Utf8("(JI)J".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::MethodRef { class_index: 2, nat_index: 5 }, // #6 Long.rotateLeft(JI)J
        Constant::Utf8("java/lang/Integer".into()),
        Constant::Class { name_index: 7 }, // #8
        Constant::Utf8("toUnsignedLong".into()),
        Constant::Utf8("(I)J".into()),
        Constant::NameAndType { name_index: 9, desc_index: 10 },
        Constant::MethodRef { class_index: 8, nat_index: 11 }, // #12 Integer.toUnsignedLong(I)J
    ]);
    // long f(long a, int d){ return Long.rotateLeft(a, d); }
    // lload_0; iload_2; invokestatic #6; lreturn
    let c = code(vec![0x1e, 0x1c, 0xb8, 0x00, 0x06, 0xad], 3);
    let f = compile(&c, "(JI)J", &pool, 0, &mut |_, _| (0, None)).expect("compiled");
    for (a, d) in [
        (0x0123_4567_89ab_cdefu64 as i64, 17),
        (-1, 63),
        (1, -1),          // negative distance == rotateRight(1)
        (i64::MIN, 64),   // distance mod 64 == 0
        (42, 0),
    ] {
        let mut l = vec![a, 0, d as i64];
        let r = unsafe { f.call(&mut l) };
        assert_eq!(
            r,
            (a as u64).rotate_left((d as u32) & 63) as i64,
            "rotateLeft({a},{d})"
        );
    }
    // long g(int x){ return Integer.toUnsignedLong(x); }
    // iload_0; invokestatic #12; lreturn
    let c = code(vec![0x1a, 0xb8, 0x00, 0x0c, 0xad], 1);
    let g = compile(&c, "(I)J", &pool, 0, &mut |_, _| (0, None)).expect("compiled");
    for x in [0i32, -1, i32::MIN, 12345] {
        let mut l = vec![x as i64];
        let r = unsafe { g.call(&mut l) };
        assert_eq!(r, (x as u32) as i64, "toUnsignedLong({x})");
    }
}

#[test]
fn jit_inlines_static_floor_intrinsic_by_resolved_bytecode_shape() {
    let pool = static_method_pool("floorLike", "(D)I");
    let c = code(vec![0x26, 0xb8, 0x00, 0x06, 0xac], 2);
    let mut intrinsics = std::collections::HashMap::new();
    intrinsics.insert(6, StaticIntrinsic::FloorDToI);
    let f = compile_with_static_intrinsics(&c, "(D)I", &pool, 0, &mut |_, _| (0, None), &intrinsics, &no_inline(), &no_native(), &no_statics(),
        #[cfg(feature = "next-gen-gc")] &no_new_arena(),
        #[cfg(feature = "next-gen-gc")] &no_ctor_inlines(),
        #[cfg(feature = "next-gen-gc")] &no_concat_shapes(),
        #[cfg(feature = "next-gen-gc")] &no_hof_fusions(),
        #[cfg(feature = "next-gen-gc")] &no_getter_inlines(),
            #[cfg(feature = "next-gen-gc")]
            None,
        #[cfg(feature = "next-gen-gc")] None,
        #[cfg(feature = "next-gen-gc")] empty_prim_arrays())
        .expect("compiled");

    let call = |x: f64| -> i32 {
        let mut locals = vec![x.to_bits() as i64, 0];
        unsafe { f.call(&mut locals) as i32 }
    };
    assert_eq!(call(-1.2), -2);
    assert_eq!(call(-0.1), -1);
    assert_eq!(call(-0.0), 0);
    assert_eq!(call(1.9), 1);
    assert_eq!(call(f64::NAN), 0);
    assert_eq!(call(f64::INFINITY), i32::MAX);
    assert_eq!(call(f64::NEG_INFINITY), i32::MAX);
}

#[test]
fn jit_inlines_static_lerp_intrinsic_family_by_resolved_bytecode_shape() {
    let lerp_pool = static_method_pool("lerpLike", "(DDD)D");
    let lerp_code = code(vec![0x26, 0x28, 0x18, 0x04, 0xb8, 0x00, 0x06, 0xaf], 6);
    let mut intrinsics = std::collections::HashMap::new();
    intrinsics.insert(6, StaticIntrinsic::LerpD);
    let lerp = compile_with_static_intrinsics(
        &lerp_code,
        "(DDD)D",
        &lerp_pool,
        0,
        &mut |_, _| (0, None),
        &intrinsics,
        &no_inline(),
        &no_native(),
        &no_statics(),
        #[cfg(feature = "next-gen-gc")]
        &no_new_arena(),
        #[cfg(feature = "next-gen-gc")]
        &no_ctor_inlines(),
        #[cfg(feature = "next-gen-gc")]
        &no_concat_shapes(),
        #[cfg(feature = "next-gen-gc")]
        &no_hof_fusions(),
        #[cfg(feature = "next-gen-gc")]
        &no_getter_inlines(),
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("compiled");
    let mut locals = vec![
        0.25f64.to_bits() as i64,
        0,
        10.0f64.to_bits() as i64,
        0,
        18.0f64.to_bits() as i64,
        0,
    ];
    let got = f64::from_bits(unsafe { lerp.call(&mut locals) } as u64);
    assert!((got - 12.0).abs() < 1e-12, "lerp={got}");

    let lerp2_pool = static_method_pool("lerp2Like", "(DDDDDD)D");
    let lerp2_code = code(
        vec![
            0x26, 0x28, 0x18, 0x04, 0x18, 0x06, 0x18, 0x08, 0x18, 0x0a, 0xb8, 0x00, 0x06, 0xaf,
        ],
        12,
    );
    intrinsics.insert(6, StaticIntrinsic::Lerp2D);
    let lerp2 = compile_with_static_intrinsics(
        &lerp2_code,
        "(DDDDDD)D",
        &lerp2_pool,
        0,
        &mut |_, _| (0, None),
        &intrinsics,
        &no_inline(),
        &no_native(),
        &no_statics(),
        #[cfg(feature = "next-gen-gc")]
        &no_new_arena(),
        #[cfg(feature = "next-gen-gc")]
        &no_ctor_inlines(),
        #[cfg(feature = "next-gen-gc")]
        &no_concat_shapes(),
        #[cfg(feature = "next-gen-gc")]
        &no_hof_fusions(),
        #[cfg(feature = "next-gen-gc")]
        &no_getter_inlines(),
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("compiled");
    let mut locals = vec![
        0.25f64.to_bits() as i64,
        0,
        0.5f64.to_bits() as i64,
        0,
        0.0f64.to_bits() as i64,
        0,
        10.0f64.to_bits() as i64,
        0,
        20.0f64.to_bits() as i64,
        0,
        30.0f64.to_bits() as i64,
        0,
    ];
    let got = f64::from_bits(unsafe { lerp2.call(&mut locals) } as u64);
    assert!((got - 12.5).abs() < 1e-12, "lerp2={got}");

    let lerp3_pool = static_method_pool("lerp3Like", "(DDDDDDDDDDD)D");
    let lerp3_code = code(
        vec![
            0x26, 0x28, 0x18, 0x04, 0x18, 0x06, 0x18, 0x08, 0x18, 0x0a, 0x18, 0x0c, 0x18, 0x0e,
            0x18, 0x10, 0x18, 0x12, 0x18, 0x14, 0xb8, 0x00, 0x06, 0xaf,
        ],
        22,
    );
    intrinsics.insert(6, StaticIntrinsic::Lerp3D);
    let lerp3 = compile_with_static_intrinsics(
        &lerp3_code,
        "(DDDDDDDDDDD)D",
        &lerp3_pool,
        0,
        &mut |_, _| (0, None),
        &intrinsics,
        &no_inline(),
        &no_native(),
        &no_statics(),
        #[cfg(feature = "next-gen-gc")]
        &no_new_arena(),
        #[cfg(feature = "next-gen-gc")]
        &no_ctor_inlines(),
        #[cfg(feature = "next-gen-gc")]
        &no_concat_shapes(),
        #[cfg(feature = "next-gen-gc")]
        &no_hof_fusions(),
        #[cfg(feature = "next-gen-gc")]
        &no_getter_inlines(),
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("compiled");
    let mut locals = vec![
        0.25f64.to_bits() as i64,
        0,
        0.5f64.to_bits() as i64,
        0,
        0.75f64.to_bits() as i64,
        0,
        0.0f64.to_bits() as i64,
        0,
        10.0f64.to_bits() as i64,
        0,
        20.0f64.to_bits() as i64,
        0,
        30.0f64.to_bits() as i64,
        0,
        40.0f64.to_bits() as i64,
        0,
        50.0f64.to_bits() as i64,
        0,
        60.0f64.to_bits() as i64,
        0,
        70.0f64.to_bits() as i64,
        0,
    ];
    let got = f64::from_bits(unsafe { lerp3.call(&mut locals) } as u64);
    assert!((got - 42.5).abs() < 1e-12, "lerp3={got}");
}

#[test]
fn jit_detects_static_numeric_intrinsic_bytecode_shapes() {
    let floor = code(
        vec![
            0x26, 0x8e, 0x3d, 0x26, 0x1c, 0x87, 0x98, 0x9c, 0x00, 0x09, 0x1c, 0x04, 0x64, 0xa7,
            0x00, 0x04, 0x1c, 0xac,
        ],
        3,
    );
    assert_eq!(
        detect_static_intrinsic(&floor, "(D)I", &empty_pool(), &mut |_| None),
        Some(StaticIntrinsic::FloorDToI)
    );

    let lfloor = code(
        vec![
            0x26, 0x8f, 0x41, 0x26, 0x20, 0x8a, 0x98, 0x9c, 0x00, 0x09, 0x20, 0x0a, 0x65, 0xa7,
            0x00, 0x04, 0x20, 0xad,
        ],
        4,
    );
    assert_eq!(
        detect_static_intrinsic(&lfloor, "(D)J", &empty_pool(), &mut |_| None),
        Some(StaticIntrinsic::LFloorDToJ)
    );

    let const_pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Double(6.0),
        Constant::Double(15.0),
        Constant::Double(10.0),
        Constant::Double(30.0),
    ]);
    let smoothstep = code(
        vec![
            0x26, 0x26, 0x6b, 0x26, 0x6b, 0x26, 0x26, 0x14, 0x00, 0x01, 0x6b, 0x14, 0x00, 0x02,
            0x67, 0x6b, 0x14, 0x00, 0x03, 0x63, 0x6b, 0xaf,
        ],
        2,
    );
    assert_eq!(
        detect_static_intrinsic(&smoothstep, "(D)D", &const_pool, &mut |_| None),
        Some(StaticIntrinsic::SmoothstepD)
    );
    let derivative = code(
        vec![
            0x14, 0x00, 0x04, 0x26, 0x6b, 0x26, 0x6b, 0x26, 0x0f, 0x67, 0x6b, 0x26, 0x0f, 0x67,
            0x6b, 0xaf,
        ],
        2,
    );
    assert_eq!(
        detect_static_intrinsic(&derivative, "(D)D", &const_pool, &mut |_| None),
        Some(StaticIntrinsic::SmoothstepDerivativeD)
    );

    let lerp = code(
        vec![0x28, 0x26, 0x18, 0x04, 0x28, 0x67, 0x6b, 0x63, 0xaf],
        6,
    );
    assert_eq!(
        detect_static_intrinsic(&lerp, "(DDD)D", &empty_pool(), &mut |_| None),
        Some(StaticIntrinsic::LerpD)
    );
    let lerp2 = code(
        vec![
            0x28, 0x26, 0x18, 0x04, 0x18, 0x06, 0xb8, 0x00, 0x06, 0x26, 0x18, 0x08, 0x18, 0x0a,
            0xb8, 0x00, 0x06, 0xb8, 0x00, 0x06, 0xaf,
        ],
        12,
    );
    assert_eq!(
        detect_static_intrinsic(&lerp2, "(DDDDDD)D", &empty_pool(), &mut |cp| {
            (cp == 6).then_some(StaticIntrinsic::LerpD)
        }),
        Some(StaticIntrinsic::Lerp2D)
    );
    let lerp3 = code(
        vec![
            0x18, 0x04, 0x26, 0x28, 0x18, 0x06, 0x18, 0x08, 0x18, 0x0a, 0x18, 0x0c, 0xb8, 0x00,
            0x07, 0x26, 0x28, 0x18, 0x0e, 0x18, 0x10, 0x18, 0x12, 0x18, 0x14, 0xb8, 0x00, 0x07,
            0xb8, 0x00, 0x06, 0xaf,
        ],
        22,
    );
    assert_eq!(
        detect_static_intrinsic(
            &lerp3,
            "(DDDDDDDDDDD)D",
            &empty_pool(),
            &mut |cp| match cp {
                6 => Some(StaticIntrinsic::LerpD),
                7 => Some(StaticIntrinsic::Lerp2D),
                _ => None,
            }
        ),
        Some(StaticIntrinsic::Lerp3D)
    );
}
