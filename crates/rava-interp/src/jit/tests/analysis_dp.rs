//! Data-parallel loop recognition: which bytecode shapes each analyzer accepts and refuses.

use super::*;

/// LOOP-FOLDING matcher: the exact `javac` shape of a counted invariant-
/// accumulation loop (`Statics.hot`) is recognised; a body that touches the
/// induction var (`iload i` → non-invariant) or has a side effect is rejected.
#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_trivial_loop_matches_invariant_accumulation_only() {
    // long f(int n){ long a=0; for(int i=0;i<n;i++) a += 5; return a; }
    let foldable = vec![
        0x09, 0x40, 0x03, 0x3e, 0x1d, 0x1a, 0xa2, 0x00, 0x0e, 0x1f, 0x08, 0x85, 0x61, 0x40,
        0x84, 0x03, 0x01, 0xa7, 0xff, 0xf3, 0x1f, 0xad,
    ];
    let plan = analyze_trivial_loop(&foldable, Some(JT::Long)).expect("foldable");
    assert_eq!(plan.acc_init, 0);
    assert_eq!(plan.c0, 0);
    assert_eq!(plan.limit_local, Some(0));
    assert_eq!(plan.inv_range, (10, 11)); // the single `iconst_5`

    // Non-invariant body: `a += i` (the invariant scan hits `iload_3`).
    let mut taint = foldable.clone();
    taint[10] = 0x1d; // iconst_5 → iload_3
    assert!(
        analyze_trivial_loop(&taint, Some(JT::Long)).is_none(),
        "a body depending on the induction var must NOT fold"
    );
    assert!(analyze_trivial_loop(&foldable, Some(JT::Int)).is_none()); // wrong ret
    assert!(analyze_trivial_loop(&[0x09, 0xad], Some(JT::Long)).is_none()); // no loop
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_loop_accepts_counter_dependent_reduction() {
    use crate::parallel_pool::ReduceOp;
    // long f(int n){ long a=0; for(int i=0;i<n;i++) a += i; return a; }
    // Identical skeleton to the foldable case but the body LOADS the counter
    // (`a += i`) → not loop-invariant, but data-parallel (associative sum).
    let dp = vec![
        0x09, 0x40, 0x03, 0x3e, 0x1d, 0x1a, 0xa2, 0x00, 0x0e, 0x1f, 0x1d, 0x85, 0x61, 0x40,
        0x84, 0x03, 0x01, 0xa7, 0xff, 0xf3, 0x1f, 0xad,
    ];
    let p = analyze_data_parallel_loop(&dp, Some(JT::Long)).expect("data-parallel");
    assert_eq!(p.op, ReduceOp::Add);
    assert!(p.acc_is_long);
    assert_eq!(p.acc_local, 1);
    assert_eq!(p.iv_local, 3);
    assert_eq!(p.c0, 0);
    assert_eq!(p.step, 1);
    assert_eq!(p.limit_local, Some(0)); // runtime limit → forker emits a trip guard
    assert_eq!(p.body_range, (9, 11)); // [load acc(skipped) .. fold) — the `iload_3`
    assert_eq!(p.acc_load_pc, 9); // the `lload_1` (load acc) is skipped by the emitter
    assert!(p.scratch.is_empty() && p.captures.is_empty());

    // ^ (lxor) fold is associative-reducible too.
    let mut xorred = dp.clone();
    xorred[12] = 0x83; // ladd → lxor
    assert_eq!(
        analyze_data_parallel_loop(&xorred, Some(JT::Long)).map(|p| p.op),
        Some(ReduceOp::Xor)
    );
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_loop_accepts_array_reduction() {
    use crate::parallel_pool::ReduceOp;
    // long hot(int[] a){ long sum=0; int n=a.length; for(int i=0;i<n;i++) sum += a[i]; return sum; }
    // a=L0, sum=L1(long), n=L3, i=L4. Exercises the length-hoist prologue, the
    // `aload; iload iv; iaload` triple, and limit_from_array resolution.
    let bc = vec![
        0x09, 0x40, // lconst_0; lstore_1            (sum = 0)
        0x2a, 0xbe, 0x3e, // aload_0; arraylength; istore_3  (n = a.length)
        0x03, 0x36, 0x04, // iconst_0; istore 4      (i = 0)
        0x15, 0x04, 0x1d, 0xa2, 0x00, 0x11, // iload 4; iload_3; if_icmpge +17 → exit(28)
        0x1f, 0x2a, 0x15, 0x04, 0x2e, 0x85, 0x61, 0x40, // lload_1; aload_0; iload 4; iaload; i2l; ladd; lstore_1
        0x84, 0x04, 0x01, // iinc 4, 1
        0xa7, 0xff, 0xef, // goto -17 → header(8)
        0x1f, 0xad, // lload_1; lreturn
    ];
    let p = analyze_data_parallel_loop(&bc, Some(JT::Long)).expect("array reduction recognised");
    assert_eq!(p.op, ReduceOp::Add);
    assert!(p.acc_is_long);
    assert_eq!(p.acc_local, 1);
    assert_eq!(p.iv_local, 4);
    assert_eq!(p.c0, 0);
    assert_eq!(p.limit_local, Some(3));
    assert_eq!(p.limit_from_array, Some(0), "limit n is the hoisted a.length");
    assert_eq!(p.array_captures, vec![(0, false)], "a[i] reads int array L0");
    assert!(p.captures.is_empty(), "the array is not a scalar capture");
    assert!(p.scratch.is_empty());
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_map_accepts_element_independent_map() {
    // void map(long[] out, long[] in, int n){ for(i=0;i<n;i++) out[i] =
    //   in[i]*in[i] + (in[i]>>>13) - (in[i]<<7); }  out=L0, in=L1, n=L2, i=L3.
    let bc = vec![
        0x03, 0x3e, // iconst_0; istore_3            (i = 0)
        0x1d, 0x1c, 0xa2, 0x00, 0x21, // iload_3; iload_2; if_icmpge +33 → exit(37)
        0x2a, 0x1d, // aload_0; iload_3              (store target out[i])
        0x2b, 0x1d, 0x2f, // aload_1; iload_3; laload  (in[i])
        0x2b, 0x1d, 0x2f, 0x69, // in[i]; lmul
        0x2b, 0x1d, 0x2f, 0x10, 0x0d, 0x7d, // in[i]; bipush 13; lushr
        0x61, // ladd
        0x2b, 0x1d, 0x2f, 0x10, 0x07, 0x79, // in[i]; bipush 7; lshl
        0x65, // lsub
        0x50, // lastore
        0x84, 0x03, 0x01, // iinc 3, 1
        0xa7, 0xff, 0xe0, // goto -32 → header(2)
        0xb1, // return
    ];
    let m = analyze_data_parallel_map(&bc).expect("element-independent map recognised");
    assert_eq!(m.iv_local, 3);
    assert_eq!(m.c0, 0);
    assert_eq!(m.step, 1);
    assert_eq!(m.limit_local, Some(2));
    assert_eq!(m.out_local, 0);
    assert!(m.out_is_long, "lastore → long output");
    assert_eq!(m.array_captures, vec![(1, true)], "in[i] is a long array read L1");
    assert!(m.captures.is_empty());
    assert!(m.scratch.is_empty());

    // Reject: a second array store (shared write) in the value expr — swap the lsub
    // (0x65) for an iastore (0x4f) makes an unbalanced/illegal body → not our shape.
    let mut bad = bc.clone();
    bad[31] = 0x53; // lastore → aastore (still a store, but wrong element kind path)
    // (Sanity: analyzer still only trusts the single terminal store; a random mid-body
    //  store-op would fail is_parallel_body_op.)
    let mut midwrite = bc.clone();
    midwrite[15] = 0x4f; // turn the `lmul` into an iastore (array store) in the value
    assert!(analyze_data_parallel_map(&midwrite).is_none(), "a shared array store in the value → serial");
    let _ = bad;
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_loop_accepts_math_max_fold() {
    // long maxr(long[] a, int n){ long m=Long.MIN_VALUE; for(i;i<n;i++) m=Math.max(m,a[i]); }
    // a=L0, n=L1, m=L2(long), i=L4. Seed is ldc2_w (#9), fold is invokestatic Math.max
    // (#11) — both captured for the dispatch to resolve (the analyzer has no pool).
    let bc = vec![
        0x14, 0x00, 0x09, // ldc2_w #9 (Long.MIN_VALUE)
        0x41, // lstore_2
        0x03, 0x36, 0x04, // iconst_0; istore 4
        0x15, 0x04, 0x1b, 0xa2, 0x00, 0x12, // iload 4; iload_1; if_icmpge +18 → 28
        0x20, 0x2a, 0x15, 0x04, 0x2f, 0xb8, 0x00, 0x0b, 0x41, // lload_2; aload_0; iload 4; laload; invokestatic #11; lstore_2
        0x84, 0x04, 0x01, // iinc 4,1
        0xa7, 0xff, 0xee, // goto -18 → 7
        0x20, 0xad, // lload_2; lreturn
    ];
    let dp = analyze_data_parallel_loop(&bc, Some(JT::Long)).expect("Math.max reduce recognised");
    assert_eq!(dp.acc_local, 2);
    assert!(dp.acc_is_long);
    assert_eq!(dp.iv_local, 4);
    assert_eq!(dp.fold_static_cp, Some(11), "invokestatic Math.max CP captured for dispatch");
    assert_eq!(dp.acc_init_cp, Some(9), "ldc2_w seed CP captured for dispatch");
    assert_eq!(dp.array_captures, vec![(0, true)], "a[i] long array read");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_scan_accepts_prefix_sum() {
    // void scan(long[] out, long[] in, int n){ long acc=0; for(i<n){acc+=in[i]; out[i]=acc;} }
    // out=L0, in=L1, n=L2, acc=L3(long), i=L5.
    let bc = vec![
        0x09, 0x42, // lconst_0; lstore_3       (acc = 0)
        0x03, 0x36, 0x05, // iconst_0; istore 5 (i = 0)
        0x15, 0x05, 0x1c, 0xa2, 0x00, 0x15, // iload 5; iload_2; if_icmpge +21 → 29
        0x21, 0x2b, 0x15, 0x05, 0x2f, 0x61, 0x42, // lload_3; aload_1; iload 5; laload; ladd; lstore_3
        0x2a, 0x15, 0x05, 0x21, 0x50, // aload_0; iload 5; lload_3; lastore
        0x84, 0x05, 0x01, // iinc 5,1
        0xa7, 0xff, 0xeb, // goto -21 → 5
        0xb1, // return
    ];
    let s = analyze_data_parallel_scan(&bc).expect("prefix-sum scan recognised");
    assert_eq!(s.iv_local, 5);
    assert_eq!(s.c0, 0);
    assert_eq!(s.in_local, 1);
    assert_eq!(s.out_local, 0);
    assert_eq!(s.limit_local, Some(2));
    assert_eq!(s.op, crate::parallel_pool::ReduceOp::Add);
    assert_eq!(s.elem_bytes, 8, "long[] scan → 8-byte stride");
    // A non-identity seed (acc=1 for an Add scan) must be rejected.
    let mut bad = bc.clone();
    bad[0] = 0x0a; // lconst_1 → acc seeded to 1 ≠ Add identity 0
    assert!(analyze_data_parallel_scan(&bad).is_none(), "non-identity seed → serial");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_reduce2_accepts_sum_sumsq() {
    // long f(long[] a, int n){ long s=0,ss=0; for(i<n){s+=a[i]; ss+=a[i]*a[i];} return ss-s*s; }
    // s=L2, ss=L4, i=L6, a=L0, n=L1 (exact javac output).
    let bc = vec![
        0x09, 0x41, // lconst_0; lstore_2       (s=0)
        0x09, 0x37, 0x04, // lconst_0; lstore 4 (ss=0)
        0x03, 0x36, 0x06, // iconst_0; istore 6 (i=0)
        0x15, 0x06, 0x1b, 0xa2, 0x00, 0x1e, // iload 6; iload_1; if_icmpge +30 → 41
        0x20, 0x2a, 0x15, 0x06, 0x2f, 0x61, 0x41, // lload_2; a[i]; ladd; lstore_2   (s+=a[i])
        0x16, 0x04, 0x2a, 0x15, 0x06, 0x2f, 0x2a, 0x15, 0x06, 0x2f, 0x69, 0x61, 0x37, 0x04, // ss+=a[i]*a[i]
        0x84, 0x06, 0x01, // iinc 6,1
        0xa7, 0xff, 0xe2, // goto -30 → 8
        0x16, 0x04, 0x20, 0x20, 0x69, 0x65, 0xad, // lload 4; lload_2; lload_2; lmul; lsub; lreturn
    ];
    let r = analyze_data_parallel_reduce2(&bc).expect("sum+sumsq dual reduce recognised");
    assert_eq!(r.acc1_local, 2);
    assert_eq!(r.acc2_local, 4);
    assert_eq!(r.iv_local, 6);
    assert_eq!(r.limit_local, Some(1));
    assert_eq!(r.op1, crate::parallel_pool::ReduceOp::Add);
    assert_eq!(r.op2, crate::parallel_pool::ReduceOp::Add);
    assert_eq!(r.val1_range, (15, 19));
    assert_eq!(r.val2_range, (23, 32));
    assert_eq!(r.ret_range, (41, 47));
    assert_eq!(r.array_captures, vec![(0, true)], "both values read a[i] (long)");
    assert!(r.captures.is_empty(), "no scalar captures");
    // A seed that isn't the fold's identity must be rejected: seed s to 1 (lconst_1)
    // while its fold is ladd (identity 0) → the parallel partials would be off by 1.
    let mut bad = bc.clone();
    bad[0] = 0x0a; // lconst_0 → lconst_1
    assert!(analyze_data_parallel_reduce2(&bad).is_none(), "seed ≠ op identity → serial");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_argmax_accepts_long_peak() {
    // int argmax(long[] a, int n){ long max=a[0]; int best=0; for(i=1;i<n;i++)
    //   if(a[i]>max){max=a[i];best=i;} return best; }  a=L0,n=L1,max=L2,best=L4,i=L5.
    let bc = vec![
        0x2a, 0x03, 0x2f, 0x41, // aload_0; iconst_0; laload; lstore_2  (max=a[0])
        0x03, 0x36, 0x04, // iconst_0; istore 4  (best=0)
        0x04, 0x36, 0x05, // iconst_1; istore 5  (i=1)
        0x15, 0x05, 0x1b, 0xa2, 0x00, 0x1b, // iload 5; iload_1; if_icmpge +27 → 40
        0x2a, 0x15, 0x05, 0x2f, 0x20, 0x94, 0x9e, 0x00, 0x0c, // a[i]; lload_2; lcmp; ifle +12 → 34
        0x2a, 0x15, 0x05, 0x2f, 0x41, // aload_0; iload 5; laload; lstore_2  (max=a[i])
        0x15, 0x05, 0x36, 0x04, // iload 5; istore 4  (best=i)
        0x84, 0x05, 0x01, // iinc 5,1
        0xa7, 0xff, 0xe5, // goto -27 → 10
        0x15, 0x04, 0xac, // iload 4; ireturn
    ];
    let m = analyze_data_parallel_argmax(&bc).expect("long argmax recognised");
    assert_eq!(m.arr_local, 0);
    assert_eq!(m.max_local, 2);
    assert_eq!(m.best_local, 4);
    assert_eq!(m.iv_local, 5);
    assert_eq!(m.limit_local, Some(1));
    assert_eq!(m.elem_bytes, 8);
    assert!(m.want_max, "ifle → argmax");
    // Flip ifle (0x9e) → ifge (0x9c) makes it an argmin.
    let mut bmin = bc.clone();
    bmin[22] = 0x9c;
    assert!(!analyze_data_parallel_argmax(&bmin).expect("argmin recognised").want_max, "ifge → argmin");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_argmax_accepts_int_peak() {
    // int argmax(int[] a, int n){ int max=a[0]; int best=0; for(i=1;i<n;i++)
    //   if(a[i]>max){max=a[i];best=i;} return best; } — iaload + if_icmple (no lcmp).
    // a=L0,n=L1,max=L2,best=L3,i=L4.
    let bc = vec![
        0x2a, 0x03, 0x2e, 0x3d, // aload_0; iconst_0; iaload; istore_2  (max=a[0])
        0x03, 0x3e, // iconst_0; istore_3  (best=0)
        0x04, 0x36, 0x04, // iconst_1; istore 4  (i=1)
        0x15, 0x04, 0x1b, 0xa2, 0x00, 0x19, // iload 4; iload_1; if_icmpge +25 → 37
        0x2a, 0x15, 0x04, 0x2e, 0x1c, 0xa4, 0x00, 0x0b, // a[i]; iload_2; if_icmple +11 → 31
        0x2a, 0x15, 0x04, 0x2e, 0x3d, // aload_0; iload 4; iaload; istore_2  (max=a[i])
        0x15, 0x04, 0x3e, // iload 4; istore_3  (best=i)
        0x84, 0x04, 0x01, // iinc 4,1
        0xa7, 0xff, 0xe7, // goto -25 → 9
        0x1d, 0xac, // iload_3; ireturn
    ];
    let m = analyze_data_parallel_argmax(&bc).expect("int argmax recognised");
    assert_eq!(m.arr_local, 0);
    assert_eq!(m.max_local, 2);
    assert_eq!(m.best_local, 3);
    assert_eq!(m.iv_local, 4);
    assert_eq!(m.elem_bytes, 4);
    assert!(m.want_max, "if_icmple → argmax");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_count_accepts_int_predicate() {
    // int count(int[] in, int n){ int c=0; for(i<n) if(in[i]>100) c++; return c; }
    // in=L0, n=L1, c=L2, i=L3.
    let bc = vec![
        0x03, 0x3d, 0x03, 0x3e, // c=0; i=0
        0x1d, 0x1b, 0xa2, 0x00, 0x14, // iload_3; iload_1; if_icmpge +20 → 26
        0x2a, 0x1d, 0x2e, 0x10, 0x64, 0xa4, 0x00, 0x06, // in[i]; bipush 100; if_icmple +6 → 20
        0x84, 0x02, 0x01, // iinc 2,1  (c++)
        0x84, 0x03, 0x01, // iinc 3,1  (i++)
        0xa7, 0xff, 0xed, // goto -19 → 4
        0x1c, 0xac, // iload_2; ireturn
    ];
    let cf = analyze_data_parallel_count(&bc).expect("int count-where recognised");
    assert_eq!(cf.in_local, 0);
    assert_eq!(cf.c_local, 2);
    assert_eq!(cf.iv_local, 3);
    assert_eq!(cf.pred_keep, IntCC::SignedGreaterThan);
    assert_eq!(cf.pred_const, 100);
    assert_eq!(cf.elem_bytes, 4);
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_filter_accepts_gt_compaction() {
    // int filter(long[] out, long[] in, int n){ int k=0; for(i<n) if(in[i]>C){out[k]=in[i];k++;} return k; }
    // out=L0, in=L1, n=L2, k=L3, i=L4. C = ldc2_w #7.
    let bc = vec![
        0x03, 0x3e, // iconst_0; istore_3   (k=0)
        0x03, 0x36, 0x04, // iconst_0; istore 4 (i=0)
        0x15, 0x04, 0x1c, 0xa2, 0x00, 0x1e, // iload 4; iload_2; if_icmpge +30 → 38
        0x2b, 0x15, 0x04, 0x2f, 0x14, 0x00, 0x07, 0x94, 0x9e, 0x00, 0x0d, // in[i]; ldc2_w #7; lcmp; ifle +13 → 32
        0x2a, 0x1d, 0x2b, 0x15, 0x04, 0x2f, 0x50, // aload_0; iload_3; in[i]; lastore
        0x84, 0x03, 0x01, // iinc 3,1  (k++)
        0x84, 0x04, 0x01, // iinc 4,1  (i++)
        0xa7, 0xff, 0xe2, // goto -30 → 5
        0x1d, 0xac, // iload_3; ireturn
    ];
    let ff = analyze_data_parallel_filter(&bc).expect("gt-filter compaction recognised");
    assert_eq!(ff.in_local, 1);
    assert_eq!(ff.out_local, 0);
    assert_eq!(ff.iv_local, 4);
    assert_eq!(ff.k_local, 3);
    assert_eq!(ff.pred_keep, IntCC::SignedGreaterThan, "ifle skip → keep if element > C");
    assert_eq!(ff.pred_cp, 7);
    assert_eq!(ff.limit_local, Some(2));
    assert_eq!(ff.elem_bytes, 8);
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_filter_accepts_int_compaction() {
    // int filter(int[] out, int[] in, int n){ int k=0; for(i<n) if(in[i]>100){out[k]=in[i];k++;} return k; }
    // int[] → iaload; bipush 100; if_icmple skip. Constant is inline (pred_cp==0).
    let bc = vec![
        0x03, 0x3e, 0x03, 0x36, 0x04, // k=0; i=0
        0x15, 0x04, 0x1c, 0xa2, 0x00, 0x1c, // iload 4; iload_2; if_icmpge +28 → 36
        0x2b, 0x15, 0x04, 0x2e, 0x10, 0x64, 0xa4, 0x00, 0x0d, // in[i]; bipush 100; if_icmple +13 → 30
        0x2a, 0x1d, 0x2b, 0x15, 0x04, 0x2e, 0x4f, // aload_0; iload_3; in[i]; iastore
        0x84, 0x03, 0x01, // iinc 3,1
        0x84, 0x04, 0x01, // iinc 4,1
        0xa7, 0xff, 0xe4, // goto -28 → 5
        0x1d, 0xac, // iload_3; ireturn
    ];
    let ff = analyze_data_parallel_filter(&bc).expect("int gt-filter recognised");
    assert_eq!(ff.in_local, 1);
    assert_eq!(ff.out_local, 0);
    assert_eq!(ff.pred_keep, IntCC::SignedGreaterThan, "if_icmple skip → keep >");
    assert_eq!(ff.pred_const, 100);
    assert_eq!(ff.pred_cp, 0, "int constant is inline");
    assert_eq!(ff.elem_bytes, 4);
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_scan_accepts_int_prefix_sum() {
    // void scan(int[] out, int[] in, int n){ int acc=0; for(i<n){acc+=in[i]; out[i]=acc;} }
    // out=L0, in=L1, n=L2, acc=L3(int), i=L4.
    let bc = vec![
        0x03, 0x3e, // iconst_0; istore_3        (acc = 0)
        0x03, 0x36, 0x04, // iconst_0; istore 4  (i = 0)
        0x15, 0x04, 0x1c, 0xa2, 0x00, 0x15, // iload 4; iload_2; if_icmpge +21 → 29
        0x1d, 0x2b, 0x15, 0x04, 0x2e, 0x60, 0x3e, // iload_3; aload_1; iload 4; iaload; iadd; istore_3
        0x2a, 0x15, 0x04, 0x1d, 0x4f, // aload_0; iload 4; iload_3; iastore
        0x84, 0x04, 0x01, // iinc 4,1
        0xa7, 0xff, 0xeb, // goto -21 → 5
        0xb1, // return
    ];
    let s = analyze_data_parallel_scan(&bc).expect("int prefix-sum scan recognised");
    assert_eq!(s.in_local, 1);
    assert_eq!(s.out_local, 0);
    assert_eq!(s.iv_local, 4);
    assert_eq!(s.op, crate::parallel_pool::ReduceOp::Add);
    assert_eq!(s.elem_bytes, 4, "int[] scan → 4-byte stride");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_map_accepts_stencil_offsets() {
    // void map(long[] out, long[] in, int m){ for(i<m) out[i]=in[i]+in[i+1]; }  a forward
    // 2-point stencil — out=L0, in=L1, m=L2, i=L3. in[i+1] = aload_1; iload_3; iconst_1;
    // iadd; laload → offset +1 captured for the fork's bounds gate.
    let bc = vec![
        0x03, 0x3e, // iconst_0; istore_3
        0x1d, 0x1c, 0xa2, 0x00, 0x15, // iload_3; iload_2; if_icmpge +21 → 25
        0x2a, 0x1d, // aload_0; iload_3  (store target)
        0x2b, 0x1d, 0x2f, // aload_1; iload_3; laload  (in[i])
        0x2b, 0x1d, 0x04, 0x60, 0x2f, // aload_1; iload_3; iconst_1; iadd; laload  (in[i+1])
        0x61, // ladd
        0x50, // lastore
        0x84, 0x03, 0x01, // iinc 3,1
        0xa7, 0xff, 0xec, // goto -20
        0xb1, // return
    ];
    let m = analyze_data_parallel_map(&bc).expect("forward stencil recognised");
    assert_eq!(m.out_local, 0);
    assert_eq!(m.array_captures, vec![(1, true)], "in read at offsets 0 and +1 → one capture");
    assert_eq!(m.read_off_lo, 0);
    assert_eq!(m.read_off_hi, 1, "in[i+1] → max offset +1");
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_map_accepts_double_arrays() {
    // void map(double[] out, double[] a, double[] b, int n){ for(i<n) out[i]=a[i]*b[i]; }
    // out=L0, a=L1, b=L2, n=L3, i=L4. Exercises daload (0x31) reads + dastore (0x52).
    let bc = vec![
        0x03, 0x36, 0x04, // iconst_0; istore 4
        0x15, 0x04, 0x1d, 0xa2, 0x00, 0x16, // iload 4; iload_3; if_icmpge +22 → 28
        0x2a, 0x15, 0x04, // aload_0; iload 4  (store target out[i])
        0x2b, 0x15, 0x04, 0x31, // aload_1; iload 4; daload  (a[i])
        0x2c, 0x15, 0x04, 0x31, // aload_2; iload 4; daload  (b[i])
        0x6b, // dmul
        0x52, // dastore
        0x84, 0x04, 0x01, // iinc 4,1
        0xa7, 0xff, 0xea, // goto -22
        0xb1, // return
    ];
    let m = analyze_data_parallel_map(&bc).expect("double array-array map recognised");
    assert_eq!(m.out_local, 0);
    assert!(m.out_is_long, "dastore → 8-byte (double) output");
    assert_eq!(m.iv_local, 4);
    assert_eq!(m.array_captures, vec![(1, true), (2, true)], "a[i], b[i] daloads (8-byte)");
    assert_eq!(m.store_skip, vec![9, 10]);
    assert!(m.scratch.is_empty());
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_map_accepts_explicit_scratch_temp() {
    // for(i<n){ long x = in[i]; out[i] = x*x + (x>>>13) - (x<<7); }  out=L0,in=L1,n=L2,
    // i=L3, x=L4 — the store target is MID-body (after the temp store), found via the
    // stack-depth pass.
    let bc = vec![
        0x03, 0x3e, // iconst_0; istore_3
        0x1d, 0x1c, 0xa2, 0x00, 0x22, // iload_3; iload_2; if_icmpge +34
        0x2b, 0x1d, 0x2f, 0x37, 0x04, // aload_1; iload_3; laload; lstore 4  (x = in[i])
        0x2a, 0x1d, // aload_0; iload_3   (store target, mid-body)
        0x16, 0x04, 0x16, 0x04, 0x69, // lload4; lload4; lmul
        0x16, 0x04, 0x10, 0x0d, 0x7d, // lload4; bipush 13; lushr
        0x61, // ladd
        0x16, 0x04, 0x10, 0x07, 0x79, // lload4; bipush 7; lshl
        0x65, // lsub
        0x50, // lastore
        0x84, 0x03, 0x01, // iinc 3,1
        0xa7, 0xff, 0xdf, // goto -33
        0xb1, // return
    ];
    let m = analyze_data_parallel_map(&bc).expect("scratch-temp map recognised");
    assert_eq!(m.out_local, 0);
    assert!(m.out_is_long);
    assert_eq!(m.store_skip, vec![12, 13], "store target aload_0(12) + iload_3(13) skipped");
    assert_eq!(m.scratch, vec![(4, true)], "x is a long scratch temp");
    assert_eq!(m.array_captures, vec![(1, true)], "in[i] read");
    assert!(m.captures.is_empty());
}

#[test]
#[cfg(feature = "next-gen-gc")]
fn analyze_data_parallel_loop_rejects_unsafe_and_nonassociative() {
    // Base: the accepted `a += i` loop.
    let dp = vec![
        0x09, 0x40, 0x03, 0x3e, 0x1d, 0x1a, 0xa2, 0x00, 0x0e, 0x1f, 0x1d, 0x85, 0x61, 0x40,
        0x84, 0x03, 0x01, 0xa7, 0xff, 0xf3, 0x1f, 0xad,
    ];
    assert!(analyze_data_parallel_loop(&dp, Some(JT::Long)).is_some());

    // (1) Non-associative fold: ladd → lsub.
    let mut sub = dp.clone();
    sub[12] = 0x65; // lsub
    assert!(analyze_data_parallel_loop(&sub, Some(JT::Long)).is_none(), "subtraction is not associative-reducible");

    // (2) Shared write in the body: the counter load → an `aastore` (1-byte op).
    let mut shared = dp.clone();
    shared[10] = 0x53; // aastore — a shared array write
    assert!(analyze_data_parallel_loop(&shared, Some(JT::Long)).is_none(), "a shared write blocks parallelism");

    // (3) Compile-time trip below the threshold: limit `iload_0` → `iconst_5`.
    let mut small = dp.clone();
    small[5] = 0x08; // iconst_5 → constant trip 5 < MIN_PARALLEL_TRIP
    assert!(analyze_data_parallel_loop(&small, Some(JT::Long)).is_none(), "tiny constant trip is not worth forking");

    // Not a loop at all.
    assert!(analyze_data_parallel_loop(&[0x09, 0xad], Some(JT::Long)).is_none());
}
