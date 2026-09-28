//! Loop-shape matchers: the iterator foreach, the counted range, and the closed-form fold.

use super::*;

/// M3 (F1): the object-building for-each map `for (i=0;i<N;i++) out[i]=f(src[i])` is
/// recognised and its per-element call site captured — both the `f(src[i])` (static) and
/// `src[i].f()` (virtual) shapes.
#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_offloadable_foreach_accepts_static_and_virtual() {
    // iconst_0; istore_1; [i]load_1; sipush 20000; if_icmpge +18;
    //   aload_2(out); iload_1; aload_3(src); iload_1; aaload; invokestatic #5; aastore;
    //   iinc 1,1; goto -19; return
    let base = vec![
        0x03, 0x3c, // c0=0; istore iv=1
        0x1b, 0x11, 0x4e, 0x20, 0xa2, 0x00, 0x12, // header: iload iv; sipush 20000; if_icmpge +18
        0x2c, 0x1b, 0x2d, 0x1b, 0x32, // aload out=2; iload iv; aload src=3; iload iv; aaload
        0xb8, 0x00, 0x05, // invokestatic #5  (f(src[i]))
        0x53, // aastore  out[i] = result
        0x84, 0x01, 0x01, 0xa7, 0xff, 0xed, // iinc iv,1; goto header (-19)
        0xb1, // return
    ];
    let fe = analyze_offloadable_foreach(&base).expect("static per-element map recognised");
    assert_eq!((fe.iv_local, fe.out_local, fe.src_local), (1, 2, 3));
    assert_eq!((fe.call_op, fe.call_cp, fe.call_pc), (0xb8, 5, 14));
    assert_eq!(fe.step, 1);
    assert!(fe.limit_local.is_none() && fe.lim_const == 20000);

    // Virtual receiver form `src[i].f()` — same skeleton, invokevirtual.
    let mut virt = base.clone();
    virt[14] = 0xb6; // invokevirtual
    let fv = analyze_offloadable_foreach(&virt).expect("virtual per-element map recognised");
    assert_eq!(fv.call_op, 0xb6);
}

/// M3 (F1): the analyzer rejects shapes that break the disjoint-map guarantee.
#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_offloadable_foreach_rejects_unsafe_shapes() {
    let base = vec![
        0x03, 0x3c, 0x1b, 0x11, 0x4e, 0x20, 0xa2, 0x00, 0x12,
        0x2c, 0x1b, 0x2d, 0x1b, 0x32, 0xb8, 0x00, 0x05, 0x53,
        0x84, 0x01, 0x01, 0xa7, 0xff, 0xed, 0xb1,
    ];
    assert!(analyze_offloadable_foreach(&base).is_some());

    // (1) out == src (in-place) — not admitted as disjoint.
    let mut inplace = base.clone();
    inplace[11] = 0x2c; // aload src=3 → aload_2 (out) → out==src
    assert!(analyze_offloadable_foreach(&inplace).is_none(), "in-place out==src rejected");

    // (2) tiny constant trip below MIN_PARALLEL_TRIP: sipush 20000 → sipush 5.
    let mut small = base.clone();
    small[4] = 0x00;
    small[5] = 0x05; // LIM = 5
    assert!(analyze_offloadable_foreach(&small).is_none(), "tiny trip is not worth forking");

    // (3) an extra argument before the call (f takes 2 args) — a stray iload between the
    //     `aaload` and the invoke breaks the exact-one-element sequence.
    let two_arg = vec![
        0x03, 0x3c, 0x1b, 0x11, 0x4e, 0x20, 0xa2, 0x00, 0x13,
        0x2c, 0x1b, 0x2d, 0x1b, 0x32, 0x1b, 0xb8, 0x00, 0x05, 0x53,
        0x84, 0x01, 0x01, 0xa7, 0xff, 0xec, 0xb1,
    ];
    assert!(analyze_offloadable_foreach(&two_arg).is_none(), "an extra call arg is not the single-element shape");
}

/// M3 (F1): optional checkcasts (element and result) are absorbed.
#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_offloadable_foreach_absorbs_checkcasts() {
    let bc = vec![
        0x03, 0x3c, 0x1b, 0x11, 0x4e, 0x20, 0xa2, 0x00, 0x18, // header, if_icmpge +24
        0x2c, 0x1b, 0x2d, 0x1b, 0x32, // aload out; iload iv; aload src; iload iv; aaload
        0xc0, 0x00, 0x06, // checkcast #6 (element)
        0xb8, 0x00, 0x05, // invokestatic #5
        0xc0, 0x00, 0x07, // checkcast #7 (result)
        0x53, // aastore
        0x84, 0x01, 0x01, 0xa7, 0xff, 0xe7, // iinc; goto header (-25)
        0xb1, // return
    ];
    let fe = analyze_offloadable_foreach(&bc).expect("checkcast-wrapped map recognised");
    assert_eq!((fe.call_op, fe.call_cp, fe.call_pc), (0xb8, 5, 17));
}

/// M3 (F3.3): the iterator for-each `for (E e : coll) e.m();` skeleton is matched from the
/// `iterator()` invoke, capturing the per-element call site + the loop exit pc.
#[cfg(feature = "next-gen-gc")]
#[test]
fn match_iterator_foreach_recognises_the_canonical_loop() {
    // aload_2 coll; invokeinterface iterator #5; astore_3 it;
    // HEAD(7): aload_3; invokeinterface hasNext #6; ifeq END(35);
    //   aload_3; invokeinterface next #7; checkcast #8; astore e=4; aload 4;
    //   invokevirtual initCache #9; goto HEAD; END: return
    let bc = vec![
        0x2c, // aload_2 (coll)
        0xb9, 0x00, 0x05, 0x01, 0x00, // invokeinterface iterator #5
        0x4e, // astore_3 (it)
        0x2d, // HEAD: aload_3
        0xb9, 0x00, 0x06, 0x01, 0x00, // invokeinterface hasNext #6
        0x99, 0x00, 0x16, // ifeq END(+22 -> 35)
        0x2d, // aload_3
        0xb9, 0x00, 0x07, 0x01, 0x00, // invokeinterface next #7
        0xc0, 0x00, 0x08, // checkcast #8
        0x3a, 0x04, // astore e=4
        0x19, 0x04, // aload 4
        0xb6, 0x00, 0x09, // invokevirtual initCache #9
        0xa7, 0xff, 0xe7, // goto HEAD(-25 -> 7)
        0xb1, // END: return
    ];
    let m = match_iterator_foreach(&bc, 1).expect("iterator for-each recognised");
    assert_eq!((m.it_local, m.e_local), (3, 4));
    assert_eq!((m.elem_op, m.elem_cp), (0xb6, 9));
    assert_eq!(m.loop_end_pc, 35);
    assert!(!m.elem_result_popped);

    // A two-statement body (an extra `aload_3` before the goto) is NOT the single-`e.m()`
    // shape → rejected.
    let mut two = bc.clone();
    two[32] = 0x2d; // overwrite the goto opcode with aload_3 → no goto where expected
    assert!(match_iterator_foreach(&two, 1).is_none(), "non-canonical body rejected");
}

/// M3 T2 (step 1): the numeric-range matcher recognises the bottom-test counted loop
/// `for (int i = 0; i < hi; i++) { <body> }` — worldgen's column-scan shape — capturing the
/// induction var, bound, step and body range. Rejects a `goto` that is not a counted loop.
#[cfg(feature = "next-gen-gc")]
#[test]
fn match_numeric_range_loop_recognises_counted_loop() {
    // 0: iconst_0            (lo = 0)
    // 1: istore_1            (i = local 1)
    // 2: goto +7 -> 9        (COND)
    // 5: nop                 (BODY)
    // 6: iinc 1,1            (step; body_end)
    // 9: iload_1 ; iload_2 ; if_icmplt -6 -> 5   (COND: i < hi=local 2)
    // 14: return             (EXIT)
    let bc = vec![
        0x03, // iconst_0
        0x3c, // istore_1
        0xa7, 0x00, 0x07, // goto -> 9
        0x00, // nop (body)
        0x84, 0x01, 0x01, // iinc 1,1
        0x1b, // iload_1
        0x1c, // iload_2 (bound)
        0xa1, 0xff, 0xfa, // if_icmplt -6 -> 5 (BODY)
        0xb1, // return (EXIT = 14)
    ];
    let m = match_numeric_range_loop(&bc, 2).expect("counted loop recognised");
    assert_eq!(m.iv_local, 1);
    assert_eq!(m.bound, RangeBound::Local(2));
    assert!(!m.inclusive);
    assert_eq!(m.step, 1);
    assert_eq!((m.body_start_pc, m.body_end_pc), (5, 6));
    assert_eq!(m.cond_pc, 9);
    assert_eq!(m.loop_exit_pc, 14);

    // A bare `goto` into a non-condition (return) is not a counted loop.
    let notloop = vec![0xa7, 0x00, 0x03, 0xb1];
    assert!(match_numeric_range_loop(&notloop, 0).is_none(), "non-counted goto rejected");
}

/// Const-bound variant `for (int i = 0; i <= 9; i++)` (bipush bound, if_icmple).
#[cfg(feature = "next-gen-gc")]
#[test]
fn match_numeric_range_loop_const_bound_inclusive() {
    // 0: iconst_0 ; istore_1 ; goto +8 -> 10 ; BODY(5): nop; nop ; iinc 1,1(body_end=7? )
    // Lay out so iinc ends right before COND.
    // 0: 0x03 iconst_0
    // 1: 0x3c istore_1
    // 2: 0xa7 0x00 0x08 goto -> 10
    // 5: 0x00 nop
    // 6: 0x00 nop
    // 7: 0x84 0x01 0x01 iinc 1,1 (body_end=7, cond=10)
    // 10: 0x1b iload_1 ; 0x10 0x09 bipush 9 ; 0xa4 0xff 0xf7 if_icmple -9 -> 5
    // 16: 0xb1 return
    let bc = vec![
        0x03, 0x3c, 0xa7, 0x00, 0x08, 0x00, 0x00, 0x84, 0x01, 0x01, 0x1b, 0x10, 0x09, 0xa4, 0xff, 0xf8, 0xb1,
    ];
    let m = match_numeric_range_loop(&bc, 2).expect("const-bound inclusive loop recognised");
    assert_eq!(m.bound, RangeBound::Const(9));
    assert!(m.inclusive);
    assert_eq!((m.body_start_pc, m.body_end_pc), (5, 7));
}

/// The TOP-test matcher recognises the shape javac ACTUALLY emits for `for (int i=0;i<n;i++)`
/// (COND at top, `if_icmpge END`, trailing backward `goto COND`) — the form the numeric probe
/// found 0 of with the bottom-test matcher on real code.
#[cfg(feature = "next-gen-gc")]
#[test]
fn match_numeric_range_loop_toptest_recognises_javac_for() {
    // 0: iconst_0; 1: istore_1;
    // 2: COND iload_1; iload_2; if_icmpge 14(END)
    // 7: nop (body); 8: iinc 1,1; 11: goto 2 (backward); 14: return
    let bc = vec![
        0x03, 0x3c, // i=0
        0x1b, 0x1c, 0xa2, 0x00, 0x0a, // COND(2): iload_1; iload_2; if_icmpge +10 -> 14
        0x00, // nop (body_start=7)
        0x84, 0x01, 0x01, // iinc 1,1 (body_end=8)
        0xa7, 0xff, 0xf7, // goto -9 -> 2 (COND)
        0xb1, // return (END=14)
    ];
    let m = match_numeric_range_loop_toptest(&bc, 11).expect("top-test javac for-loop recognised");
    assert_eq!(m.iv_local, 1);
    assert_eq!(m.bound, RangeBound::Local(2));
    assert!(!m.inclusive);
    assert_eq!(m.step, 1);
    assert_eq!((m.body_start_pc, m.body_end_pc), (7, 8));
    assert_eq!(m.cond_pc, 2);
    assert_eq!(m.loop_exit_pc, 14);
    // A forward goto is not a top-test back-edge.
    assert!(match_numeric_range_loop_toptest(&[0xa7, 0x00, 0x03, 0xb1], 0).is_none());
}

/// M3 (F3.3c): the split matcher recognises T1's real two-statement body
/// `for (s : states) { REGISTRY.add(s); s.initCache(); }` — a branch-free prefix followed by
/// the offloadable `e.m()` — capturing the prefix range + the final call. The strict
/// single-statement matcher correctly rejects it.
#[cfg(feature = "next-gen-gc")]
#[test]
fn match_iterator_foreach_split_recognises_prefix_plus_call() {
    // aload_2 coll; invokeinterface iterator #5; astore_2 it;
    // HEAD(7): aload_2; hasNext #6; ifeq END(40);
    //   aload_2; next #7; checkcast #8; astore_3 e;
    //   getstatic #10; aload_3; invokevirtual add #11;   <-- prefix (sequential registry add)
    //   aload_3; invokevirtual initCache #9;             <-- offloadable e.m()
    //   goto HEAD; END: return
    let bc = vec![
        0x2c, // aload_2 coll
        0xb9, 0x00, 0x05, 0x01, 0x00, // iterator #5
        0x4d, // astore_2 it=2
        0x2c, // HEAD aload_2
        0xb9, 0x00, 0x06, 0x01, 0x00, // hasNext #6
        0x99, 0x00, 0x1b, // ifeq END(+27 -> 40)
        0x2c, // aload_2
        0xb9, 0x00, 0x07, 0x01, 0x00, // next #7
        0xc0, 0x00, 0x08, // checkcast #8
        0x4e, // astore_3 e=3
        0xb2, 0x00, 0x0a, // getstatic #10 (prefix start 26)
        0x2d, // aload_3
        0xb6, 0x00, 0x0b, // invokevirtual add #11 (prefix end 33)
        0x2d, // aload_3 (aload e, pc 33)
        0xb6, 0x00, 0x09, // invokevirtual initCache #9 (pc 34)
        0xa7, 0xff, 0xe2, // goto HEAD(-30 -> 7)
        0xb1, // END: return
    ];
    let s = match_iterator_foreach_split(&bc, 1).expect("split iterator for-each recognised");
    assert_eq!((s.it_local, s.e_local), (2, 3));
    assert_eq!(s.prefix_range, (26, 33));
    assert_eq!((s.elem_op, s.elem_cp), (0xb6, 9));
    assert_eq!(s.loop_end_pc, 40);
    assert!(!s.elem_result_popped);
    // The strict single-statement matcher rejects the two-statement body.
    assert!(match_iterator_foreach(&bc, 1).is_none(), "strict matcher rejects the prefixed body");
}

/// LOOP-FOLDING e2e: a counted constant-accumulation loop compiles to its closed
/// form and returns the exact iterated sum for several trip counts — including
/// the empty (n ≤ 0) loop. No iteration runs; the result must still be bit-exact.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_loop_fold_returns_closed_form_sum() {
    let c = code(
        vec![
            0x09, 0x40, 0x03, 0x3e, 0x1d, 0x1a, 0xa2, 0x00, 0x0e, 0x1f, 0x08, 0x85, 0x61, 0x40,
            0x84, 0x03, 0x01, 0xa7, 0xff, 0xf3, 0x1f, 0xad,
        ],
        4,
    );
    let f = compile(&c, "(I)J", &empty_pool(), 0, &mut |_, _| (0, None))
        .expect("folded loop compiles");
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let run = |n: i32| -> i64 {
        let mut locals = vec![n as i64, 0, 0, 0, 0, 0];
        unsafe { f.call_desc(&mut locals, desc_cell) }
    };
    for n in [0i32, 1, 2, 7, 1000, 20_000_000, -5] {
        assert_eq!(run(n), 5i64 * n.max(0) as i64, "fold sum for n={n}");
    }
    set_current_vm(prev);
}
