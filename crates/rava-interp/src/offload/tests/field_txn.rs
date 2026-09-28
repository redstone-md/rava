//! Conservative write-only field-transaction admission tests.

use super::*;
// ---- FIELD-TXN (write-only transaction) mode — a DISTINCT admission question ----

#[test]
fn field_txn_shared_putfield_is_permitted() {
    // The txn runtime journals instance-field writes: a putfield to a SHARED
    // `this` is the shape the txn mode exists to admit. The same body is still
    // rejected by the pure gate (it is a visible mutation there).
    let c = code(vec![0x2a, 0x04, 0xb5, 0x00, 0x01, 0xb1]); // aload_0; iconst_1; putfield #1; return
    assert!(
        method_field_txn_safe_given(&c, &pool_field_i(), None, &mut yes),
        "the txn runtime journals a shared instance-field write — admitted"
    );
    // Fresh-target writes stay admissible for the same reason as before.
    let fresh = code(vec![0xbb, 0x00, 0x02, 0x59, 0x04, 0xb5, 0x00, 0x01, 0xb0]);
    assert!(method_field_txn_safe_given(
        &fresh,
        &pool_field_i(),
        None,
        &mut yes
    ));
    // ...and the PURE analysis of the shared-putfield body is unchanged.
    if !crate::parallel::cow_gate() {
        // COW relaxes shared-putfield rejection by design
        assert!(
            !method_offloadable_given(&c, &pool_field_i(), None, false, false, &mut yes),
            "the pure gate must keep rejecting a shared-putfield body"
        );
    }
}

#[test]
fn field_txn_shared_getfield_rejects() {
    // aload_0 (shared this); getfield #1; pop; return — a shared READ the
    // write-only txn runtime cannot journal.
    let c = code(vec![0x2a, 0xb4, 0x00, 0x01, 0x57, 0xb1]);
    assert!(
        !method_field_txn_safe_given(&c, &pool_field_i(), None, &mut yes),
        "a shared getfield needs a dynamic read barrier the txn does not have"
    );
    let r = method_field_txn_safe_reason(&c, &pool_field_i(), None, &mut yes);
    assert_eq!(
        r.map(|x| x.to_string()),
        Some("shared-read 0xb4@1".to_string())
    );
    // The same read is FINE in the pure analysis (pure reads are pure).
    assert!(method_offloadable_given(
        &c,
        &pool_field_i(),
        None,
        false,
        false,
        &mut yes
    ));
}

#[test]
fn field_txn_fresh_getfield_is_permitted() {
    // new #2; dup; getfield #1; pop; return — reading a FRESH object's field
    // reads only txn-private state. (javac loads fields of just-built objects.)
    let c = code(vec![0xbb, 0x00, 0x02, 0x59, 0xb4, 0x00, 0x01, 0x57, 0xb0]);
    assert!(
        method_field_txn_safe_given(&c, &pool_field_i(), None, &mut yes),
        "a fresh receiver's field read is txn-private"
    );
    // Freshness survives a local round-trip too: new; astore_1; aload_1; getfield.
    let via_local = code(vec![
        0xbb, 0x00, 0x02, 0x4c, 0x2b, 0xb4, 0x00, 0x01, 0x57, 0xb1,
    ]);
    assert!(method_field_txn_safe_given(
        &via_local,
        &pool_field_i(),
        None,
        &mut yes
    ));
}

#[test]
fn field_txn_getstatic_rejects() {
    // getstatic #1; pop; return — statics are never txn-private.
    let c = code(vec![0xb2, 0x00, 0x01, 0x57, 0xb1]);
    assert!(
        !method_field_txn_safe_given(&c, &pool_field_i(), None, &mut yes),
        "a getstatic reads shared state the txn cannot journal-shadow"
    );
    let r = method_field_txn_safe_reason(&c, &pool_field_i(), None, &mut yes);
    assert_eq!(
        r.map(|x| x.to_string()),
        Some("shared-read 0xb2@0".to_string())
    );
    // Pure analysis still admits the plain read.
    assert!(method_offloadable_given(
        &c,
        &pool_field_i(),
        None,
        false,
        false,
        &mut yes
    ));
}

#[test]
fn field_txn_shared_array_load_rejects_fresh_ok() {
    // SHARED: aload_0 (param array); iconst_0; iaload; pop; return — a shared
    // array read the txn runtime cannot intercept.
    let shared = code(vec![0x2a, 0x03, 0x2e, 0x57, 0xb1]);
    assert!(
        !method_field_txn_safe_given(&shared, &pool(), None, &mut yes),
        "reading a shared array element is an unjournaled read"
    );
    // FRESH: bipush 3; newarray int; dup; iconst_0; iaload; pop; areturn.
    let fresh = code(vec![0x10, 0x03, 0xbc, 0x0a, 0x59, 0x03, 0x2e, 0x57, 0xb0]);
    assert!(
        method_field_txn_safe_given(&fresh, &pool(), None, &mut yes),
        "a fresh array's elements are txn-private"
    );
}

#[test]
fn field_txn_shared_array_store_still_rejects() {
    // aload_0 (param array); iconst_0; iconst_9; iastore; return — the txn
    // runtime journals INSTANCE fields only, never array stores.
    let c = code(vec![0x2a, 0x03, 0x10, 0x09, 0x4f, 0xb1]);
    assert!(
        !method_field_txn_safe_given(&c, &pool(), None, &mut yes),
        "a shared array store is not journal-replayable"
    );
}

#[test]
fn field_txn_putstatic_still_rejects() {
    // aload_0; iconst_1; putstatic #1; return — statics are never journalled.
    let c = code(vec![0x2a, 0x04, 0xb3, 0x00, 0x01, 0xb1]);
    assert!(
        !method_field_txn_safe_given(&c, &pool(), None, &mut yes),
        "a putstatic write is not journal-replayable"
    );
}

#[test]
fn field_txn_invoke_must_be_proven_bytecode() {
    // invokestatic #1 (Owner.m()V); return — in txn mode EVERY invoke goes to
    // `callee` (the recv-mut-native shortcut is disabled). A receiver-mutating
    // native (BitSet.set on a fresh receiver) is admissible in the PURE mode
    // via that shortcut but must reach `callee` — and be REJECTED there — in
    // txn mode; the VM caller rejects native/no-bytecode targets.
    let fresh_native = code(vec![
        0xbb, 0x00, 0x01, 0x59, 0x08, 0xb6, 0x00, 0x02, 0x57, 0xb1,
    ]);
    let mut no = |_: CalleeQuery| false;
    assert!(
        !method_field_txn_safe_given(&fresh_native, &pool_bitset_set(), None, &mut no),
        "a native-shape invoke is never txn-provable via the recv-mut shortcut"
    );
    assert!(
        method_offloadable_given(
            &fresh_native,
            &pool_bitset_set(),
            None,
            false,
            false,
            &mut no
        ),
        "the PURE mode still admits the fresh-receiver mutating native (unchanged)"
    );
    // The same query DOES reach `callee` in txn mode: a bytecode-only callee
    // (proven by the resolver) is admissible.
    let mut bytecode_ok = |_: CalleeQuery| true;
    assert!(
        method_field_txn_safe_given(&fresh_native, &pool_bitset_set(), None, &mut bytecode_ok),
        "a txn-provable bytecode callee admits the fresh-receiver invoke"
    );
    // And an unproven static callee rejects the txn shape.
    let statik = code(vec![0xb8, 0x00, 0x01, 0xb1]);
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
    assert!(!method_field_txn_safe_given(&statik, &p, None, &mut no));
    assert!(method_field_txn_safe_given(&statik, &p, None, &mut yes));
}

#[test]
fn field_txn_pure_results_unchanged() {
    // The txn mode is a SEPARATE question: every existing pure verdict on the
    // shapes in this file stays as it was. Spot-check a few pairs where the
    // txn mode differs from the pure mode so a regression in either direction
    // surfaces.
    // (a) shared getfield: pure OK, txn reject (read of shared state).
    let gf = code(vec![0x2a, 0xb4, 0x00, 0x01, 0x57, 0xb1]);
    assert!(method_offloadable_given(
        &gf,
        &pool_field_i(),
        None,
        false,
        false,
        &mut yes
    ));
    assert!(!method_field_txn_safe_given(
        &gf,
        &pool_field_i(),
        None,
        &mut yes
    ));
    // (b) shared putfield: pure reject (outside COW), txn OK (journalled).
    let pf = code(vec![0x2a, 0x04, 0xb5, 0x00, 0x01, 0xb1]);
    if !crate::parallel::cow_gate() {
        assert!(!method_offloadable_given(
            &pf,
            &pool_field_i(),
            None,
            false,
            false,
            &mut yes
        ));
    }
    assert!(method_field_txn_safe_given(
        &pf,
        &pool_field_i(),
        None,
        &mut yes
    ));
    // (c) a pure-arithmetic method is admissible in BOTH modes.
    let arith = code(vec![0x1a, 0x1b, 0x60, 0xac]);
    assert!(method_offloadable_given(
        &arith,
        &pool(),
        None,
        false,
        false,
        &mut yes
    ));
    assert!(method_field_txn_safe_given(&arith, &pool(), None, &mut yes));
}
#[test]
fn field_txn_rejects_class_resolving_opcodes() {
    let cases = [
        vec![0x12, 0x01, 0xb0],                         // ldc
        vec![0x14, 0x00, 0x01, 0xad],                   // ldc2_w
        vec![0x04, 0xbd, 0x00, 0x01, 0xb0],             // anewarray
        vec![0x2a, 0xc0, 0x00, 0x01, 0xb0],             // checkcast
        vec![0x2a, 0xc1, 0x00, 0x01, 0xac],             // instanceof
        vec![0x04, 0x04, 0xc5, 0x00, 0x01, 0x02, 0xb0], // multianewarray
    ];
    for bytecode in cases {
        let candidate = code(bytecode);
        assert!(
            !method_field_txn_safe_given(&candidate, &pool(), None, &mut yes),
            "class-resolving operations cannot run during a field transaction"
        );
    }
}
