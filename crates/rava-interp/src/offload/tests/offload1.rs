//! Part 1 of the offload tests.

use super::*;

#[test]
fn pure_arithmetic_is_offloadable() {
    // iload_0; iload_1; iadd; ireturn
    let c = code(vec![0x1a, 0x1b, 0x60, 0xac]);
    assert!(method_offloadable_given(
        &c,
        &pool(),
        None,
        false,
        false,
        &mut yes
    ));
}

#[test]
fn fresh_alloc_then_return_is_offloadable() {
    // bipush 5; newarray int; areturn
    let c = code(vec![0x10, 0x05, 0xbc, 0x0a, 0xb0]);
    assert!(method_offloadable_given(
        &c,
        &pool(),
        None,
        false,
        false,
        &mut yes
    ));
}

#[test]
fn putstatic_rejects() {
    let c = code(vec![0x2a, 0x2b, 0xb3, 0x00, 0x01, 0xb1]);
    assert!(!method_offloadable_given(
        &c,
        &pool(),
        None,
        false,
        false,
        &mut yes
    ));
    // putstatic is always shared — relaxed rejects too.
    assert!(!method_offloadable_given(
        &c,
        &pool(),
        None,
        true,
        false,
        &mut yes
    ));
}

// Pool whose #1 is a Fieldref Owner.f:I (so field_ref/field_width resolve).
#[test]
fn putfield_to_shared_this_rejects() {
    if crate::parallel::cow_gate() {
        return;
    } // COW relaxes shared-putfield rejection by design
      // NON-ctor: aload_0 (this = shared receiver); iconst_1; putfield #1; return
    let c = code(vec![0x2a, 0x04, 0xb5, 0x00, 0x01, 0xb1]);
    assert!(
        !method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
        "writing a shared `this` field is a visible mutation"
    );
    // relaxed ceiling probe allows it.
    assert!(method_offloadable_given(
        &c,
        &pool_field_i(),
        None,
        true,
        false,
        &mut yes
    ));
}

#[test]
fn forked_receiver_private_allows_this_field_write() {
    if crate::parallel::cow_gate() {
        return;
    } // COW relaxes shared-putfield rejection by design
      // NON-ctor `initCache`-style body: aload_0; iconst_1; putfield #1; return. Writing a
      // published `this`'s field is rejected by the plain gate (shared write) but PERMITTED
      // in a fork context (the receiver is the distinct per-iteration element → disjoint).
    let c = code(vec![0x2a, 0x04, 0xb5, 0x00, 0x01, 0xb1]);
    assert!(
        !method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
        "plain gate rejects a write to a shared `this`"
    );
    assert!(
        method_offloadable_forked(&c, &pool_field_i(), None, false, None, &mut yes),
        "element-ownership fork gate permits the per-element `this` write"
    );
}

#[test]
fn ctor_this_field_write_is_offloadable() {
    // <init>: aload_0 (this = FRESH under construction); iconst_1; putfield #1; return
    let c = code(vec![0x2a, 0x04, 0xb5, 0x00, 0x01, 0xb1]);
    assert!(
        method_offloadable_given(&c, &pool_field_i(), None, false, true, &mut yes),
        "a constructor writing its OWN fresh field is offloadable"
    );
}

#[test]
fn putfield_to_freshly_new_object_is_offloadable() {
    // new #? ; dup ; iconst_1 ; putfield #1 ; areturn  (build + return a fresh obj)
    // new needs a Class cp entry; reuse pool_field_i's #2 (Class Owner) for `new`.
    let c = code(vec![0xbb, 0x00, 0x02, 0x59, 0x04, 0xb5, 0x00, 0x01, 0xb0]);
    assert!(
        method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
        "writing a field of a freshly-new'd, unpublished object is private"
    );
}

#[test]
fn array_store_to_fresh_ok_shared_rejects() {
    // FRESH: bipush 3; newarray int; dup; iconst_0; iconst_9; iastore; areturn
    let fresh = code(vec![
        0x10, 0x03, 0xbc, 0x0a, 0x59, 0x03, 0x10, 0x09, 0x4f, 0xb0,
    ]);
    assert!(
        method_offloadable_given(&fresh, &pool(), None, false, false, &mut yes),
        "storing into a fresh array is private"
    );
    // SHARED: aload_0 (shared array param); iconst_0; iconst_9; iastore; return
    let shared = code(vec![0x2a, 0x03, 0x10, 0x09, 0x4f, 0xb1]);
    assert!(
        !method_offloadable_given(&shared, &pool(), None, false, false, &mut yes),
        "storing into a shared array is a visible mutation"
    );
}

#[test]
fn published_fresh_then_written_is_caught_via_shared_target() {
    // new; dup; astore_1; ... aload_1; putfield — after astore_1 local1 is FRESH,
    // so writing its field is still private. This asserts the fresh flag flows
    // through a local round-trip.
    // new #2; astore_1; aload_1; iconst_1; putfield #1; return
    let c = code(vec![
        0xbb, 0x00, 0x02, 0x4c, 0x2b, 0x04, 0xb5, 0x00, 0x01, 0xb1,
    ]);
    assert!(
        method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
        "freshness survives a store/load through a local"
    );
}

#[test]
fn durable_fresh_local_survives_join() {
    // A SINGLE-assignment local assigned a fresh value survives the conservative join reset
    // (its freshness is stable — never reassigned). This is what lets an object/array
    // allocated before a loop stay fresh through the loop body.
    // new #2; astore_1; goto -> 7; [7] aload_1; iconst_1; putfield #1; return
    let c = code(vec![
        0xbb, 0x00, 0x02, // new (local1's ONLY astore → durable)
        0x4c, // astore_1  (fresh → frozen)
        0xa7, 0x00, 0x03, // goto -> 7
        0x2b, // aload_1  (join target = 7): local1 kept fresh (durable)
        0x04, // iconst_1
        0xb5, 0x00, 0x01, // putfield  → to a FRESH object → offloadable
        0xb1, // return
    ]);
    assert!(
        method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
        "a durable single-assign fresh local survives the join (loop support)"
    );
}

#[test]
fn nondurable_local_resets_at_join() {
    if crate::parallel::cow_gate() {
        return;
    } // COW relaxes shared-putfield rejection by design
      // A MULTI-assignment local is NOT durable — the join conservatively resets it to Shared,
      // so a putfield through it after the join is rejected (sound under-approximation).
      // new #2; astore_1; aload_1; astore_1(2nd); goto -> 9; [9] aload_1; iconst_1; putfield; return
    let c = code(vec![
        0xbb, 0x00, 0x02, // 0: new
        0x4c, // 3: astore_1  (1st)
        0x2b, // 4: aload_1
        0x4c, // 5: astore_1  (2nd → local1 NOT durable)
        0xa7, 0x00, 0x03, // 6: goto -> 9
        0x2b, // 9: aload_1  (join target): reset to Shared
        0x04, // 10: iconst_1
        0xb5, 0x00, 0x01, // 11: putfield → to a SHARED target → rejected
        0xb1, // 14: return
    ]);
    assert!(
        !method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
        "a multi-assign local is conservatively reset at the join"
    );
}

#[test]
fn loop_fill_of_durable_fresh_array_is_offloadable() {
    // An array allocated BEFORE a loop and filled INSIDE it — the real VoxelShape/BitSet
    // build shape. The durable single-assign array stays fresh across the loop join, so the
    // in-loop iastore is a private write.
    // bipush 8; newarray int; astore_1; iconst_0; istore_2;
    // HEAD(7): iload_2; bipush 8; if_icmpge exit(23); aload_1; iload_2; iload_2; iastore;
    //          iinc 2,1; goto HEAD; exit: return
    let c = code(vec![
        0x10, 0x08, 0xbc, 0x0a, 0x4c, 0x03, 0x3d, // new int[8]; astore_1; i=0
        0x1c, 0x10, 0x08, 0xa2, 0x00, 0x0d, // HEAD: iload_2; bipush 8; if_icmpge +13
        0x2b, 0x1c, 0x1c, 0x4f, // aload_1; iload_2; iload_2; iastore
        0x84, 0x02, 0x01, 0xa7, 0xff, 0xf3, // iinc 2,1; goto HEAD (-13)
        0xb1, // exit: return
    ]);
    assert!(
        method_offloadable_given(&c, &pool(), None, false, false, &mut yes),
        "array alloc'd before a loop + filled inside stays fresh (durable single-assign)"
    );
}

/// M3 T2 (step 2): a counted loop writing `arr[i] = i` into a SHARED (param) array is
/// offloadable as an index-partitioned map — the store index is the induction var, so the
/// writes are disjoint across iterations. Writing a CONSTANT index (same slot every iteration)
/// is a cross-iteration conflict → rejected.
#[cfg(feature = "next-gen-gc")]
#[test]
fn counted_loop_index_partitioned_store_is_offloadable() {
    // void f(int[] arr=l0, int n=l1) { for (int i=l2=0; i<n; i++) arr[i] = i; }
    // 0: iconst_0; istore_2; 2: goto 12; 5: aload_0; iload_2; iload_2; iastore;
    // 9: iinc 2,1; 12: iload_2; iload_1; if_icmplt 5; 17: return
    let ok = code(vec![
        0x03, 0x3d, 0xa7, 0x00, 0x0a, // i=0; goto COND(12)
        0x2a, 0x1c, 0x1c,
        0x4f, // BODY(5): arr[i]=i  (aload_0; iload_2 idx; iload_2 val; iastore)
        0x84, 0x02, 0x01, // iinc 2,1
        0x1c, 0x1b, 0xa1, 0xff, 0xf7, // COND(12): iload_2; iload_1; if_icmplt -9 -> 5
        0xb1, // return
    ]);
    assert!(
        counted_loop_offloadable(&ok, &pool(), None, 2, &mut yes),
        "arr[iv]=iv on a shared array is index-partitioned → offloadable"
    );

    // Same, but store to a CONSTANT index arr[0]=i (same slot each iteration) → shared write.
    let bad = code(vec![
        0x03, 0x3d, 0xa7, 0x00, 0x0a, 0x2a, 0x03, 0x1c,
        0x4f, // BODY: arr[0]=i  (aload_0; iconst_0 idx; iload_2 val; iastore)
        0x84, 0x02, 0x01, 0x1c, 0x1b, 0xa1, 0xff, 0xf7, 0xb1,
    ]);
    assert!(
        !counted_loop_offloadable(&bad, &pool(), None, 2, &mut yes),
        "arr[0]=iv (constant index, same slot every iteration) is a cross-iteration conflict"
    );
}

#[test]
fn invoke_offloadability_follows_callee() {
    // invokestatic #1 (a ()V so no args) ; return
    let p = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::MethodRef {
            class_index: 2,
            nat_index: 3,
        },
        Constant::Class { name_index: 4 },
        Constant::NameAndType {
            name_index: 5,
            desc_index: 6,
        },
        Constant::Utf8("Owner".into()),
        Constant::Utf8("m".into()),
        Constant::Utf8("()V".into()),
    ]);
    let c = code(vec![0xb8, 0x00, 0x01, 0xb1]);
    let mut no = |_: CalleeQuery| false;
    assert!(!method_offloadable_given(
        &c, &p, None, false, false, &mut no
    ));
    assert!(method_offloadable_given(
        &c, &p, None, false, false, &mut yes
    ));
}
