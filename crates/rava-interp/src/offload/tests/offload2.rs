//! Part 2 of the offload tests.

use super::*;

#[test]
fn monitor_and_athrow_and_indy_reject() {
    assert!(!method_offloadable_given(&code(vec![0x2a, 0xc2, 0xb1]), &pool(), None, false, false, &mut yes)); // monitorenter
    // athrow of a SHARED value (aload_0) still rejects — only a FRESH throwable is path-terminal.
    assert!(!method_offloadable_given(&code(vec![0x2a, 0xbf]), &pool(), None, false, false, &mut yes)); // athrow (shared)
    // invokedynamic with no provable concat bootstrap (Unusable cp / no table) still rejects.
    assert!(!method_offloadable_given(&code(vec![0xba, 0x00, 0x01, 0x00, 0x00, 0xb1]), &pool(), None, false, false, &mut yes)); // invokedynamic
}

#[test]
fn unmodelled_opcode_rejects() {
    // 0xa9 ret — unmodelled → reject.
    assert!(!method_offloadable_given(&code(vec![0xa9, 0x00, 0xb1]), &pool(), None, false, false, &mut yes));
}

#[test]
fn receiver_mut_native_needs_fresh_receiver() {
    let mut no = |_: CalleeQuery| false; // no bytecode callee offloadable
    // FRESH: new #1; dup; iconst_5; invokevirtual #2 BitSet.set(I)V; pop; return
    let fresh = code(vec![0xbb, 0x00, 0x01, 0x59, 0x08, 0xb6, 0x00, 0x02, 0x57, 0xb1]);
    assert!(method_offloadable_given(&fresh, &pool_bitset_set(), None, false, false, &mut no),
        "BitSet.set on a FRESH bitset mutates a private object → offloadable");
    // SHARED: aload_0 (shared this); iconst_5; invokevirtual #2; return
    let shared = code(vec![0x2a, 0x08, 0xb6, 0x00, 0x02, 0xb1]);
    assert!(!method_offloadable_given(&shared, &pool_bitset_set(), None, false, false, &mut no),
        "BitSet.set on a SHARED bitset is a visible mutation → reject");
}

// Pool for the devirt tests: #1 Class Impl (the `new` site), #3 Impl.<init>()V,
// #7 Iface.m()V (declared on the interface Iface, impl on Impl).
#[test]
fn fresh_new_devirtualizes_virtual_call() {
    // new #1 Impl ; dup ; invokespecial #3 Impl.<init> ; invokevirtual #7 Iface.m ; return
    let c = code(vec![0xbb, 0x00, 0x01, 0x59, 0xb7, 0x00, 0x03, 0xb6, 0x00, 0x07, 0xb1]);
    // The callee is offloadable ONLY for the CONCRETE Impl, never the declared Iface owner.
    let mut concrete_only = |q: CalleeQuery| q.exact && q.owner == "Impl";
    assert!(method_offloadable_given(&c, &pool_devirt(), None, false, false, &mut concrete_only),
        "a virtual call on a fresh `new Impl` devirtualises Iface.m → Impl.m");
    // If even the concrete impl is not offloadable, the call is still rejected.
    let mut none_ok = |_: CalleeQuery| false;
    assert!(!method_offloadable_given(&c, &pool_devirt(), None, false, false, &mut none_ok));
}

#[test]
fn forked_this_call_devirtualizes_against_self_class() {
    // aload_0 (this) ; invokevirtual #7 Iface.m ; return — a `this`-receiver call to an
    // abstract/interface method. In an element-ownership fork with the concrete element
    // type known (self_class="Impl"), it devirtualises Iface.m → Impl.m.
    let c = code(vec![0x2a, 0xb6, 0x00, 0x07, 0xb1]);
    let mut concrete_only = |q: CalleeQuery| q.exact && q.owner == "Impl";
    assert!(
        method_offloadable_forked(&c, &pool_devirt(), None, false, Some("Impl"), &mut concrete_only),
        "a this-call devirtualises against the concrete element type in a fork"
    );
    // Without the concrete self type, the this-call can't devirtualise → rejected.
    assert!(!method_offloadable_forked(&c, &pool_devirt(), None, false, None, &mut concrete_only));
    // The plain gate (shared `this`) never devirtualises a this-call.
    assert!(!method_offloadable_given(&c, &pool_devirt(), None, false, false, &mut concrete_only));
}

#[test]
fn shared_receiver_does_not_devirtualize() {
    // aload_0 (shared `this`) ; invokevirtual #7 Iface.m ; return — NOT a fresh receiver,
    // so the call resolves only against the declared Iface owner (not devirt'd to Impl).
    let c = code(vec![0x2a, 0xb6, 0x00, 0x07, 0xb1]);
    let mut concrete_only = |q: CalleeQuery| q.exact && q.owner == "Impl";
    assert!(!method_offloadable_given(&c, &pool_devirt(), None, false, false, &mut concrete_only),
        "devirtualisation requires a fresh, exactly-typed receiver");
}

/// GAP-S06: a virtual call on a SHARED receiver must reach the callee callback as a
/// CLOSURE query (exact=false) — declared-owner acceptance alone must not admit it.
#[test]
fn shared_receiver_virtual_requires_dispatch_closure() {
    // aload_0 (shared this); invokevirtual #7 Iface.m; return
    let c = code(vec![0x2a, 0xb6, 0x00, 0x07, 0xb1]);
    let mut owner_exact_only = |q: CalleeQuery| q.exact && q.owner == "Iface";
    assert!(
        !method_offloadable_given(&c, &pool_devirt(), None, false, false, &mut owner_exact_only),
        "declared-owner (exact) acceptance must not admit a shared-receiver virtual call"
    );
    let mut closure_grants = |q: CalleeQuery| !q.exact && q.owner == "Iface";
    assert!(
        method_offloadable_given(&c, &pool_devirt(), None, false, false, &mut closure_grants),
        "a proven dispatch closure admits the call"
    );
    // invokestatic stays an EXACT query (owner is the target by construction).
    let p = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::MethodRef { class_index: 2, nat_index: 3 },
        Constant::Class { name_index: 4 },
        Constant::NameAndType { name_index: 5, desc_index: 6 },
        Constant::Utf8("Owner".into()),
        Constant::Utf8("m".into()),
        Constant::Utf8("()V".into()),
    ]);
    let s = code(vec![0xb8, 0x00, 0x01, 0xb1]);
    let mut exact_only = |q: CalleeQuery| q.exact;
    assert!(method_offloadable_given(&s, &p, None, false, false, &mut exact_only));
}

#[test]
fn reason_names_the_blocking_callee() {
    // invokestatic #1 (Owner.m()V) with a NO callee → reason = "callee Owner.m".
    let p = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::MethodRef { class_index: 2, nat_index: 3 },
        Constant::Class { name_index: 4 },
        Constant::NameAndType { name_index: 5, desc_index: 6 },
        Constant::Utf8("Owner".into()),
        Constant::Utf8("m".into()),
        Constant::Utf8("()V".into()),
    ]);
    let c = code(vec![0xb8, 0x00, 0x01, 0xb1]);
    let mut no = |_: CalleeQuery| false;
    let r = method_offloadable_reason(&c, &p, None, false, false, &mut no);
    // The reject carries the DESCRIPTOR so a diagnostic can re-resolve the callee.
    assert_eq!(r.map(|x| x.to_string()), Some("callee Owner.m()V".to_string()));
    // putstatic → reason names the opcode.
    let ps = code(vec![0x2a, 0x2b, 0xb3, 0x00, 0x01, 0xb1]);
    let r2 = method_offloadable_reason(&ps, &pool(), None, false, false, &mut yes);
    assert!(r2.map(|x| x.to_string()).unwrap_or_default().starts_with("op 0xb3"));
}

// ---- INCREMENT 3: string-concat invokedynamic + fresh athrow -------------

#[test]
fn concat_indy_then_fresh_athrow_is_offloadable() {
    // javac's `throw new Exc("prefix " + i)` shape:
    // new #12 Exc; dup; iload_0; invokedynamic #1 (I)String; invokespecial #14; athrow
    // — the concat is a pure fresh-String expression, the thrown Exc is FRESH,
    // and the method (no handlers) terminates the path like a return.
    let p = concat_pool("(I)Ljava/lang/String;");
    let b = bsm_table();
    let c = code(vec![
        0xbb, 0x00, 0x0c, // new #12 Exc (fresh, exact type known)
        0x59, // dup
        0x1a, // iload_0
        0xba, 0x00, 0x01, 0x00, 0x00, // invokedynamic #1 → Fresh String
        0xb7, 0x00, 0x0e, // invokespecial #14 Exc.<init>(String)V on the FRESH receiver
        0xbf, // athrow (thrown value FRESH → path-terminal)
    ]);
    assert!(method_offloadable_given(&c, &p, Some(&b), false, false, &mut yes),
        "a fresh-exception concat+throw is a private alloc + unwind, not a publish");
    // Same method followed by a dead/next-block `return`: the post-athrow scan
    // continues on an emptied stack (mirrors the post-return continuation).
    let c2 = code(vec![
        0xbb, 0x00, 0x0c, 0x59, 0x1a, 0xba, 0x00, 0x01, 0x00, 0x00,
        0xb7, 0x00, 0x0e, 0xbf, 0xb1,
    ]);
    assert!(method_offloadable_given(&c2, &p, Some(&b), false, false, &mut yes));
    // WITHOUT the bootstrap table the indy cannot be proven a concat → reject.
    assert!(!method_offloadable_given(&c, &p, None, false, false, &mut yes),
        "no BootstrapMethods table ⇒ the indy stays a hard blocker");
}

#[test]
fn non_concat_bootstrap_still_rejects() {
    // Identical bytecode, but the bootstrap handle resolves to
    // LambdaMetafactory.metafactory — NOT the modelled concat → reject.
    let p = pool_indy("java/lang/invoke/LambdaMetafactory", "metafactory", "(I)Ljava/lang/String;");
    let b = bsm_table();
    let c = code(vec![0x1a, 0xba, 0x00, 0x01, 0x00, 0x00, 0xb0]);
    assert!(!method_offloadable_given(&c, &p, Some(&b), false, false, &mut yes),
        "only the StringConcatFactory concat bootstrap is modelled");
    // makeConcatWithConstants NAME on a different owner also rejects.
    let p2 = pool_indy("some/other/Factory", "makeConcatWithConstants", "(I)Ljava/lang/String;");
    assert!(!method_offloadable_given(&c, &p2, Some(&b), false, false, &mut yes));
}

#[test]
fn concat_ref_arg_requires_tostring_proof() {
    // (LFoo;)String concat: the serving implementation stringifies the Foo arg
    // via its guest toString() — admissible only when the toString dispatch
    // closure over the declared type proves out (same envelope as a virtual
    // call on a shared receiver).
    let p = concat_pool("(LFoo;)Ljava/lang/String;");
    let b = bsm_table();
    let c = code(vec![0x2a, 0xba, 0x00, 0x01, 0x00, 0x00, 0xb0]); // aload_0; indy; areturn
    let mut tostring_ok = |q: CalleeQuery| !q.exact && q.owner == "Foo" && q.name == "toString" && q.desc == "()Ljava/lang/String;";
    assert!(method_offloadable_given(&c, &p, Some(&b), false, false, &mut tostring_ok),
        "a proven toString closure admits the reference concat arg");
    let mut no = |_: CalleeQuery| false;
    assert!(!method_offloadable_given(&c, &p, Some(&b), false, false, &mut no),
        "an unproven toString keeps the concat rejected");
    // The reject names the blocking toString edge.
    let r = method_offloadable_reason(&c, &p, Some(&b), false, false, &mut no);
    assert_eq!(r.map(|x| x.to_string()), Some("callee Foo.toString (concat arg)".to_string()));
    // A String arg needs no proof: rava renders Str/ArenaString in pure Rust.
    let ps = concat_pool("(Ljava/lang/String;I)Ljava/lang/String;");
    let cs = code(vec![0x2a, 0x1b, 0xba, 0x00, 0x01, 0x00, 0x00, 0xb0]);
    assert!(method_offloadable_given(&cs, &ps, Some(&b), false, false, &mut no),
        "String + primitive concat args stringify without guest re-entry");
    // An ARRAY arg is Object.toString territory — no pure proof → reject.
    let pa = concat_pool("([I)Ljava/lang/String;");
    let ca = code(vec![0x2a, 0xba, 0x00, 0x01, 0x00, 0x00, 0xb0]);
    assert!(!method_offloadable_given(&ca, &pa, Some(&b), false, false, &mut no));
}

#[test]
fn shared_athrow_still_rejects_even_with_bsm() {
    // aload_0 (shared throwable) ; athrow — throwing a PUBLISHED object hands
    // it to an unknown handler in another frame → reject, table or no table.
    let p = concat_pool("(I)Ljava/lang/String;");
    let b = bsm_table();
    let c = code(vec![0x2a, 0xbf]);
    assert!(!method_offloadable_given(&c, &p, Some(&b), false, false, &mut yes));
    let r = method_offloadable_reason(&c, &p, Some(&b), false, false, &mut yes);
    assert!(r.map(|x| x.to_string()).unwrap_or_default().starts_with("op 0xbf"));
}

#[test]
fn ref_param_types_parses_descriptors() {
    assert_eq!(ref_param_types("(I)Ljava/lang/String;"), Vec::<&str>::new());
    assert_eq!(ref_param_types("(LFoo;)V"), vec!["Foo"]);
    assert_eq!(
        ref_param_types("(Ljava/lang/String;IJLa/B;)Ljava/lang/String;"),
        vec!["java/lang/String", "a/B"]
    );
    assert_eq!(ref_param_types("([I[LFoo;D)V"), vec!["[I", "[LFoo;"]);
}
