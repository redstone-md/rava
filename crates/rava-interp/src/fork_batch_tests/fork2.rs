//! Part 2 of the fork tests.

use super::*;

/// GAP-P04: equals-verified salted key probing. Guest classes with constant
/// hashCode 0 force every key into one (class, hc) chain; a constant-true
/// `equals` makes all keys converge to salt #0, a constant-false one splits
/// salts, removal holes are skipped (monotonic counters), and a snapshot
/// restore rebuilds the counters from the salted key strings.
#[test]
fn salted_keys_converge_split_and_survive_restore() {
    use crate::heap::MapKey;
    let hash0 = || Code { max_stack: 1, max_locals: 2, bytecode: vec![0x03, 0xac], exceptions: Vec::new(), ..Default::default() }; // iconst_0; ireturn
    let ret_true = Code { max_stack: 1, max_locals: 2, bytecode: vec![0x04, 0xac], exceptions: Vec::new(), ..Default::default() }; // iconst_1; ireturn
    let ret_false = Code { max_stack: 1, max_locals: 2, bytecode: vec![0x03, 0xac], exceptions: Vec::new(), ..Default::default() };

    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "SltEq", None, &[], vec![
        method("hashCode", "()I", 0x0001, Some(hash0())),
        method("equals", "(Ljava/lang/Object;)Z", 0x0001, Some(ret_true)),
    ]);
    define(&mut vm, "SltNeq", None, &[], vec![
        method("hashCode", "()I", 0x0001, Some(hash0())),
        method("equals", "(Ljava/lang/Object;)Z", 0x0001, Some(ret_false)),
    ]);
    let mid = vm.heap.alloc_map();

    // CONVERGE: first key mints #0; after inserting it, an "equal" second key
    // probes the live entry and reuses #0.
    let k1 = obj(&mut vm, "SltEq");
    let mk1 = crate::map::salted_probe_for_test(&mut vm, mid, &k1, "SltEq").expect("no throw").expect("eligible");
    assert_eq!(mk1, "o:gSSltEq:0#0");
    vm.heap.map_put(mid, MapKey::str(mk1.clone()), k1, Value::Int(1));
    let k2 = obj(&mut vm, "SltEq");
    let mk2 = crate::map::salted_probe_for_test(&mut vm, mid, &k2, "SltEq").expect("no throw").expect("eligible");
    assert_eq!(mk2, mk1, "equals-equal keys converge to one salted key");

    // SPLIT: same (class, hc) but equals=false → a fresh salt per distinct key.
    let n1 = obj(&mut vm, "SltNeq");
    let nk1 = crate::map::salted_probe_for_test(&mut vm, mid, &n1, "SltNeq").unwrap().unwrap();
    assert_eq!(nk1, "o:gSSltNeq:0#0");
    vm.heap.map_put(mid, MapKey::str(nk1.clone()), n1, Value::Int(2));
    let n2 = obj(&mut vm, "SltNeq");
    let nk2 = crate::map::salted_probe_for_test(&mut vm, mid, &n2, "SltNeq").unwrap().unwrap();
    assert_eq!(nk2, "o:gSSltNeq:0#1", "unequal same-hash keys split salts");
    vm.heap.map_put(mid, MapKey::str(nk2.clone()), n2, Value::Int(3));

    // REMOVAL HOLE: removing #0 never recycles its salt — the next distinct key
    // skips the hole and mints #2 (monotonic, no aliasing).
    vm.heap.map_remove(mid, &MapKey::str(nk1));
    let n3 = obj(&mut vm, "SltNeq");
    let nk3 = crate::map::salted_probe_for_test(&mut vm, mid, &n3, "SltNeq").unwrap().unwrap();
    assert_eq!(nk3, "o:gSSltNeq:0#2", "holes are skipped; counters are monotonic");

    // RESTORE: counters are not on the wire — rebuilt from salted key strings.
    let snap = vm.heap.snapshot();
    let restored = crate::heap::Heap::restore(&snap).expect("restore");
    assert_eq!(restored.map_salt_next(mid, "SltNeq", 0), 2, "max live salt (#1) + 1");
    assert_eq!(restored.map_salt_next(mid, "SltEq", 0), 1);
}

// ---------------------------------------------------------------------------
// GAP-A01 (drill iter 3): the natively-served Throwable `<init>` contract and
// the unloadable-owner closure fallback.
// ---------------------------------------------------------------------------

/// A `new IllegalArgumentException(String)` + athrow on the FRESH receiver
/// admits: no bytecode resolves, the class is unloadable and strictly provably
/// Throwable, and the receiver is private — the RECV_MUT ctor contract applies.
#[test]
fn fresh_throwable_ctor_admits() {
    let mut vm = Vm::new(ClassLoader::new());
    let pool = mref_pool("java/lang/IllegalArgumentException", "<init>", "(Ljava/lang/String;)V");
    define_with_pool(&mut vm, "ThrFresh", None, pool, vec![method("m", "()V", 0x0001, Some(fresh_throw_body()))]);
    assert!(
        vm.is_offloadable("ThrFresh", "m", "()V", false),
        "fresh IAE ctor + fresh athrow is the proven throw-path shape"
    );
}

/// The SAME ctor on a SHARED receiver rejects — RECV_MUT is only sound on a
/// private receiver (mutating a published object is a visible write).
#[test]
fn shared_receiver_throwable_ctor_rejects() {
    let mut vm = Vm::new(ClassLoader::new());
    let pool = mref_pool("java/lang/IllegalArgumentException", "<init>", "(Ljava/lang/String;)V");
    // aload_1 (shared arg); ldc "boom"; invokespecial <init>; return
    let body = Code { max_stack: 3, max_locals: 2, bytecode: vec![0x2b, 0x12, 0x07, 0xb7, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "ThrShared", None, pool, vec![method("m", "(Ljava/lang/Object;)V", 0x0001, Some(body))]);
    assert!(
        !vm.is_offloadable("ThrShared", "m", "(Ljava/lang/Object;)V", false),
        "the ctor contract must not apply to a shared receiver"
    );
}

/// An unresolvable ctor whose class is NOT provably Throwable rejects: the
/// "*Exception" suffix alone (the runtime routing predicate) is not the key —
/// the strict subtype proof over the fixed JDK table is.
#[test]
fn non_throwable_unresolvable_ctor_rejects() {
    let mut vm = Vm::new(ClassLoader::new());
    // Unloadable ghost with the suffix but no provable Throwable chain.
    let pool = mref_pool("com/example/FbtGhostException", "<init>", "(Ljava/lang/String;)V");
    define_with_pool(&mut vm, "ThrGhost", None, pool, vec![method("m", "()V", 0x0001, Some(fresh_throw_body()))]);
    assert!(
        !vm.is_offloadable("ThrGhost", "m", "()V", false),
        "an unknown JDK-boundary '*Exception' name is not strictly provable → reject"
    );
    // No suffix at all: the contract shape itself does not match.
    let pool2 = mref_pool("com/example/FbtGhostThing", "<init>", "(Ljava/lang/String;)V");
    define_with_pool(&mut vm, "ThrGhost2", None, pool2, vec![method("m", "()V", 0x0001, Some(fresh_throw_body()))]);
    assert!(!vm.is_offloadable("ThrGhost2", "m", "()V", false));
}

/// GAP-S03 invariant holds for the new contract: a LOADED guest class under the
/// IAE name has its real `<init>` bytecode analysed — the contract (which
/// requires the class to be unloadable and bytecode-free) never masks it.
#[test]
fn guest_bytecode_under_throwable_name_is_analysed_not_trusted() {
    let mut vm = Vm::new(ClassLoader::new());
    // Guest "java/lang/IllegalArgumentException" whose <init> does a putstatic.
    let impure_pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::FieldRef { class_index: 2, nat_index: 3 },
        Constant::Class { name_index: 4 },
        Constant::NameAndType { name_index: 5, desc_index: 6 },
        Constant::Utf8("Owner".into()),
        Constant::Utf8("f".into()),
        Constant::Utf8("I".into()),
    ]);
    let impure_init = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x03, 0xb3, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "java/lang/IllegalArgumentException", None, impure_pool,
        vec![method("<init>", "(Ljava/lang/String;)V", 0x0001, Some(impure_init))]);
    let pool = mref_pool("java/lang/IllegalArgumentException", "<init>", "(Ljava/lang/String;)V");
    define_with_pool(&mut vm, "ThrPoisoned", None, pool, vec![method("m", "()V", 0x0001, Some(fresh_throw_body()))]);
    assert!(
        !vm.is_offloadable("ThrPoisoned", "m", "()V", false),
        "a resolvable impure guest ctor body must be analysed and rejected, never contract-trusted"
    );
}

/// A guest exception subclass's own `<init>` chains `super(String)` into the
/// unresolvable JDK ctor: `this` under construction is private, so the chained
/// super ctor admits through the same contract.
#[test]
fn guest_ctor_chaining_into_jdk_throwable_super_admits() {
    let mut vm = Vm::new(ClassLoader::new());
    let pool = mref_pool("java/lang/RuntimeException", "<init>", "(Ljava/lang/String;)V");
    // aload_0 (this, under construction); aload_1 (msg); invokespecial super.<init>; return
    let body = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2a, 0x2b, 0xb7, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "FbtChainException", Some("java/lang/RuntimeException"), pool,
        vec![method("<init>", "(Ljava/lang/String;)V", 0x0001, Some(body))]);
    assert!(
        vm.is_offloadable("FbtChainException", "<init>", "(Ljava/lang/String;)V", false),
        "super(String) into the unresolvable RuntimeException ctor admits on the ctor's fresh this"
    );
}

/// GAP-A01 task 2: a virtual call whose declared owner is UNLOADABLE admits iff
/// the owner's native service has a Pure non-callback contract AND no loaded
/// class possibly subtypes the owner; loading a guest subtype (epoch bump)
/// flips the verdict to reject.
#[test]
fn unloadable_owner_contract_fallback_admits_until_a_guest_subtype_loads() {
    let mut vm = Vm::new(ClassLoader::new());
    // aload_1 (shared Class mirror); aload_2; invokevirtual Class.cast; pop; return
    let pool = mref_pool("java/lang/Class", "cast", "(Ljava/lang/Object;)Ljava/lang/Object;");
    let body = Code { max_stack: 2, max_locals: 3, bytecode: vec![0x2b, 0x2c, 0xb6, 0x00, 0x01, 0x57, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "CastCaller", None, pool,
        vec![method("m", "(Ljava/lang/Class;Ljava/lang/Object;)V", 0x0001, Some(body))]);
    assert!(
        vm.is_offloadable("CastCaller", "m", "(Ljava/lang/Class;Ljava/lang/Object;)V", false),
        "Class.cast on a shared receiver admits via the unloadable-owner Pure-contract fallback"
    );
    // A loaded guest subtype of the unloadable owner opens the closure: the
    // receiver could now be guest bytecode the analysis has not seen → reject.
    define(&mut vm, "FbtFakeClass", Some("java/lang/Class"), &[], vec![]);
    assert!(
        !vm.is_offloadable("CastCaller", "m", "(Ljava/lang/Class;Ljava/lang/Object;)V", false),
        "a loaded guest subtype of the owner must flip the fallback to reject (epoch-correct)"
    );
}

/// The fallback requires a Pure non-callback contract on the owner — an
/// unloadable owner without one (Class.getName allocates per call and carries
/// no contract) stays rejected.
#[test]
fn unloadable_owner_without_contract_rejects() {
    let mut vm = Vm::new(ClassLoader::new());
    let pool = mref_pool("java/lang/Class", "getName", "()Ljava/lang/String;");
    let body = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2b, 0xb6, 0x00, 0x01, 0x57, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "NameCaller", None, pool,
        vec![method("m", "(Ljava/lang/Class;)V", 0x0001, Some(body))]);
    assert!(
        !vm.is_offloadable("NameCaller", "m", "(Ljava/lang/Class;)V", false),
        "no contract on the unloadable owner → the closure miss stays a reject"
    );
}

// ---------------------------------------------------------------------------
// GAP-A01 (drill iter 4): Boolean/Character rows in the JDK_SUPERS table close
// the unloadable-owner check for the boxed non-Number value types.
// ---------------------------------------------------------------------------

/// The fixed table must answer the boxed non-Number value types authoritatively:
/// Boolean/Character are provable Comparable subtypes (strict walk), are NOT
/// Numbers, and — the fix — a loaded class whose super chain dies at an unknown
/// JDK boundary (inconclusive) is NO LONGER a lenient "possible subtype" of
/// either (they are final JDK classes the walk can only reach by direct naming).
#[test]
fn boolean_and_character_subtype_answers_are_authoritative() {
    let mut vm = Vm::new(ClassLoader::new());
    for c in ["java/lang/Boolean", "java/lang/Character"] {
        assert!(crate::opcodes::name_is_subtype_strict(&mut vm, c, "java/lang/Comparable"), "{c} implements Comparable");
        assert!(!crate::opcodes::name_is_subtype_strict(&mut vm, c, "java/lang/Number"), "{c} is NOT a Number");
        assert!(crate::opcodes::name_is_subtype(&mut vm, c, c), "identity");
    }
    // A guest whose super is an unloadable, non-table JDK name: its chain is
    // inconclusive, which used to count as a possible subtype of ANY non-table
    // unloadable name — including java/lang/Boolean.
    define(&mut vm, "FbtInconclusive", Some("java/lang/ThreadLocal"), &[], vec![]);
    for sup in ["java/lang/Boolean", "java/lang/Character"] {
        assert!(
            !crate::opcodes::name_is_subtype(&mut vm, "FbtInconclusive", sup),
            "an inconclusive guest chain must not leniently subtype the final table-known {sup}"
        );
    }
    // The leniency the table rows replace is still there for genuinely unknown
    // JDK names (the conservative direction for the closed-check consumer): an
    // inconclusive chain still counts as a possible subtype of a NON-table
    // unloadable name.
    assert!(crate::opcodes::name_is_subtype(&mut vm, "FbtInconclusive", "java/util/concurrent/ConcurrentHashMap"));
}

/// The drill-4 leaf: `Boolean.booleanValue()Z` on a SHARED receiver admits via
/// the unloadable-owner Pure-contract fallback once the JDK_SUPERS rows make the
/// closed-check authoritative — even with a loaded class whose super chain is
/// inconclusive (the boot population that used to poison the scan).
#[test]
fn boolean_boolean_value_admits_via_unloadable_owner_fallback() {
    let mut vm = Vm::new(ClassLoader::new());
    // The boot condition: SOME loaded class has an inconclusive super chain.
    define(&mut vm, "FbtNoise", Some("java/lang/ThreadLocal"), &[], vec![]);
    // aload_1 (shared Boolean); invokevirtual Boolean.booleanValue()Z; pop; return
    let pool = mref_pool("java/lang/Boolean", "booleanValue", "()Z");
    let body = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2b, 0xb6, 0x00, 0x01, 0x57, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "BoolCaller", None, pool,
        vec![method("m", "(Ljava/lang/Object;)V", 0x0001, Some(body))]);
    assert!(
        vm.is_offloadable("BoolCaller", "m", "(Ljava/lang/Object;)V", false),
        "Boolean.booleanValue admits: Pure contract + closed unloadable owner"
    );
    // Character mirrors Boolean (same table row shape, same contract family).
    let pool2 = mref_pool("java/lang/Character", "charValue", "()C");
    let body2 = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2b, 0xb6, 0x00, 0x01, 0x57, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "CharCaller", None, pool2,
        vec![method("m", "(Ljava/lang/Object;)V", 0x0001, Some(body2))]);
    assert!(
        vm.is_offloadable("CharCaller", "m", "(Ljava/lang/Object;)V", false),
        "Character.charValue admits through the same fallback"
    );
}

/// Regression: a SYNTHETIC GUEST SUBCLASS of java/lang/Boolean (illegal on a
/// real JVM — Boolean is final — but rava must stay sound even for hostile
/// class files) reopens the closure: the closed-check sees a loaded possible
/// receiver and the fallback flips back to reject (epoch-correct).
#[test]
fn guest_subclass_of_boolean_flips_the_closed_check_back_to_reject() {
    let mut vm = Vm::new(ClassLoader::new());
    let pool = mref_pool("java/lang/Boolean", "booleanValue", "()Z");
    let body = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2b, 0xb6, 0x00, 0x01, 0x57, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define_with_pool(&mut vm, "BoolCaller2", None, pool,
        vec![method("m", "(Ljava/lang/Object;)V", 0x0001, Some(body))]);
    assert!(vm.is_offloadable("BoolCaller2", "m", "(Ljava/lang/Object;)V", false));
    // The hostile subclass loads → its name-walk reaches java/lang/Boolean
    // directly → the owner is no longer closed → reject.
    define(&mut vm, "FbtFakeBoolean", Some("java/lang/Boolean"), &[], vec![]);
    assert!(
        !vm.is_offloadable("BoolCaller2", "m", "(Ljava/lang/Object;)V", false),
        "a loaded guest subtype of java/lang/Boolean must flip the fallback to reject"
    );
}

// ---------------------------------------------------------------------------
// GAP-A01 STAGE 1 (unloadable-iface closure): the over-inclusive may-subtype
// walk and the Gate-1 implementor enumeration. Diagnostic-only in stage 1 —
// no admission path reads either, so these pin the ENUMERATION contract the
// later gates will stand on.
// ---------------------------------------------------------------------------

/// `name_may_subtype` answers the ENUMERATION direction: a conclusive
/// non-implementor is provably out; identity/Object/direct-edge/table-walk are
/// in; and — the delta vs the lenient refutation walk — an INCONCLUSIVE chain
/// may subtype ANYTHING, including a JDK_SUPERS table KEY like java/util/List
/// (where `name_is_subtype`'s fallback answers no).
#[test]
fn may_subtype_over_includes_inconclusive_chains_even_for_table_key_sups() {
    let mut vm = Vm::new(ClassLoader::new());
    // Conclusive non-implementor: loaded, no interfaces, no super — provably NOT.
    define(&mut vm, "MsPlain", None, &[], vec![]);
    assert!(!crate::opcodes::name_may_subtype(&mut vm, "MsPlain", "java/lang/Comparable"));
    assert!(!crate::opcodes::name_may_subtype(&mut vm, "MsPlain", "java/util/List"));
    // Identity / Object are always in.
    assert!(crate::opcodes::name_may_subtype(&mut vm, "MsPlain", "MsPlain"));
    assert!(crate::opcodes::name_may_subtype(&mut vm, "MsPlain", "java/lang/Object"));
    // Direct interface edge.
    define(&mut vm, "MsDirect", None, &["java/lang/Comparable"], vec![]);
    assert!(crate::opcodes::name_may_subtype(&mut vm, "MsDirect", "java/lang/Comparable"));
    // Table walk: a guest under java/util/ArrayList reaches the java/util/List
    // KEY purely through JDK_SUPERS rows (AbstractList -> List).
    define(&mut vm, "MsGuestList", Some("java/util/ArrayList"), &[], vec![]);
    assert!(crate::opcodes::name_may_subtype(&mut vm, "MsGuestList", "java/util/List"));
    // THE DELTA: a super chain dying at an unloadable, non-table JDK name is
    // inconclusive — it MAY subtype anything, table-key sups included.
    define(&mut vm, "MsIncon", Some("java/lang/ThreadLocal"), &[], vec![]);
    for sup in ["java/util/List", "java/lang/Comparable", "com/example/FbtUnknownIface"] {
        assert!(
            crate::opcodes::name_may_subtype(&mut vm, "MsIncon", sup),
            "an enumeration must over-include the inconclusive chain for {sup}"
        );
    }
    // Pin the direction split the two consumers rely on: on the SAME pair the
    // refutation walk stays authoritative-no (sup is a table key).
    assert!(
        !crate::opcodes::name_is_subtype(&mut vm, "MsIncon", "java/util/List"),
        "name_is_subtype must keep answering no where name_may_subtype over-includes"
    );
}

/// `interface_dispatch_closure` enumerates loaded concrete may-subtype
/// implementors of an UNLOADABLE interface owner: a transitive interface chain
/// (impl -> loaded iface -> owner) is IN, identical inherited bodies dedup to
/// one representative, abstract implementors and conclusive non-implementors
/// are OUT, an inconclusive-chain class is IN (over-inclusion), and a
/// late-loaded implementor joins on the epoch bump.
#[test]
fn iface_closure_includes_transitive_interface_chain_implementors() {
    let mut vm = Vm::new(ClassLoader::new());
    let eq = "(Ljava/lang/Object;)Z";
    // Bridge: IcImpl -> IcMid (loaded interface) -> java/lang/Comparable (unloadable).
    define_flags(&mut vm, "IcMid", None, &["java/lang/Comparable"], vec![], 0x0601);
    define(&mut vm, "IcImpl", None, &["IcMid"], vec![method("equals", eq, 0x0001, Some(body(0)))]);
    // Direct implementor + its body-inheriting subclass (Code-ptr dedup pair).
    define(&mut vm, "IcDirect", None, &["java/lang/Comparable"], vec![method("equals", eq, 0x0001, Some(body(1)))]);
    define(&mut vm, "IcDirectSub", Some("IcDirect"), &[], vec![]);
    // Abstract implementor — never a receiver's exact class.
    define_flags(&mut vm, "IcAbs", None, &["java/lang/Comparable"], vec![], 0x0421);
    // Conclusive unrelated class — provably out.
    define(&mut vm, "IcPlain", None, &[], vec![method("equals", eq, 0x0001, Some(body(2)))]);

    let reps = vm.interface_dispatch_closure("java/lang/Comparable", "equals", eq);
    let got: Vec<&str> = reps.iter().map(String::as_str).collect();
    assert!(got.contains(&"IcImpl"), "transitive interface chain joins the closure: {got:?}");
    assert!(
        got.contains(&"IcDirect") ^ got.contains(&"IcDirectSub"),
        "one representative per shared inherited Code: {got:?}"
    );
    assert!(!got.contains(&"IcAbs"), "abstract implementor excluded: {got:?}");
    assert!(!got.contains(&"IcPlain"), "conclusive non-implementor excluded: {got:?}");

    // OVER-INCLUSION + epoch: a late-loaded inconclusive-chain class joins the
    // (previously memoised) closure on the loader epoch bump.
    define(&mut vm, "IcIncon", Some("java/lang/ThreadLocal"), &[], vec![method("equals", eq, 0x0001, Some(body(3)))]);
    let reps2 = vm.interface_dispatch_closure("java/lang/Comparable", "equals", eq);
    let got2: Vec<&str> = reps2.iter().map(String::as_str).collect();
    assert!(got2.contains(&"IcIncon"), "inconclusive chain must be enumerated (over-include): {got2:?}");
    assert!(got2.contains(&"IcImpl"), "earlier members survive the recompute: {got2:?}");
}

/// Renaming every synthetic class must not change any verdict — admission has no
/// name-based routing (names are dispatch inputs only).
#[test]
fn renaming_classes_does_not_change_admission() {
    for (base, s1, s2) in [
        ("Zq0", "Zq1", "Zq2"),
        ("net/minecraft/FbtRenBase", "net/minecraft/FbtRenA", "net/minecraft/FbtRenB"),
        ("a", "b", "c"),
    ] {
        let mut vm = Vm::new(ClassLoader::new());
        define(&mut vm, base, None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
        define(&mut vm, s1, Some(base), &[], vec![]);
        define(&mut vm, s2, Some(base), &[], vec![method("m", "()V", 0x0001, Some(body(7)))]);
        // Inherited-identical pair admits; override pair rejects — for every naming.
        let ok = vec![obj(&mut vm, base), obj(&mut vm, s1)];
        assert!(vm.fork_batch_target(&ok, "m", "()V").is_some(), "naming {base}/{s1}");
        let bad = vec![obj(&mut vm, s1), obj(&mut vm, s2)];
        assert!(vm.fork_batch_target(&bad, "m", "()V").is_none(), "naming {s1}/{s2}");
    }
}
