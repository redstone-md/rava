//! Part 1 of the fork tests.

use super::*;

/// Two subclasses overriding the same method with DIFFERENT bodies: the batch
/// resolves per receiver and must stay serial.
#[test]
fn heterogeneous_subclass_overrides_stay_serial() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtBase", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    define(&mut vm, "FbtSubA", Some("FbtBase"), &[], vec![method("m", "()V", 0x0001, Some(body(1)))]);
    define(&mut vm, "FbtSubB", Some("FbtBase"), &[], vec![method("m", "()V", 0x0001, Some(body(2)))]);
    let elems = vec![obj(&mut vm, "FbtSubA"), obj(&mut vm, "FbtSubB")];
    assert!(
        vm.fork_batch_target(&elems, "m", "()V").is_none(),
        "differing exact targets must keep the serial path"
    );
}

/// A homogeneous batch of one exact override resolves to one bytecode method and
/// may proceed to the per-class effect/compilation proofs.
#[test]
fn homogeneous_exact_override_admits_single_target() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtHBase", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    define(&mut vm, "FbtHSub", Some("FbtHBase"), &[], vec![method("m", "()V", 0x0001, Some(body(3)))]);
    let elems = vec![obj(&mut vm, "FbtHSub"), obj(&mut vm, "FbtHSub"), obj(&mut vm, "FbtHSub")];
    let batch = vm.fork_batch_target(&elems, "m", "()V").expect("homogeneous exact override admits");
    assert_eq!(batch.classes, vec!["FbtHSub".to_string()]);
    assert_eq!(batch.code, resolved_code_key(&mut vm, "FbtHSub"), "pinned to the resolved override");
}

/// Subclasses that INHERIT one identical implementation share its `Code`: the batch
/// deduplicates resolution per class but keeps EVERY receiver class for proof coverage.
#[test]
fn inherited_identical_implementation_deduplicates_without_losing_coverage() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtIBase", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    define(&mut vm, "FbtISub1", Some("FbtIBase"), &[], vec![]);
    define(&mut vm, "FbtISub2", Some("FbtIBase"), &[], vec![]);
    let elems = vec![
        obj(&mut vm, "FbtIBase"),
        obj(&mut vm, "FbtISub1"),
        obj(&mut vm, "FbtISub2"),
        obj(&mut vm, "FbtISub1"), // repeat — dedup, not a new entry
    ];
    let batch = vm.fork_batch_target(&elems, "m", "()V").expect("shared inherited body admits");
    assert_eq!(batch.code, resolved_code_key(&mut vm, "FbtIBase"));
    assert_eq!(
        batch.classes,
        vec!["FbtIBase".to_string(), "FbtISub1".to_string(), "FbtISub2".to_string()],
        "every distinct receiver class stays in the proof set"
    );
}

/// One subclass overriding while a sibling inherits → two exact targets → serial.
#[test]
fn override_mixed_with_inherited_stays_serial() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtMBase", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    define(&mut vm, "FbtMOvr", Some("FbtMBase"), &[], vec![method("m", "()V", 0x0001, Some(body(4)))]);
    define(&mut vm, "FbtMInh", Some("FbtMBase"), &[], vec![]);
    let elems = vec![obj(&mut vm, "FbtMOvr"), obj(&mut vm, "FbtMInh")];
    assert!(vm.fork_batch_target(&elems, "m", "()V").is_none());
}

/// Two implementors of one interface with distinct bodies must stay serial.
#[test]
fn two_interface_implementors_stay_serial() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtIface", None, &[], vec![method("m", "()V", 0x0401, None)]); // abstract
    define(&mut vm, "FbtImplX", None, &["FbtIface"], vec![method("m", "()V", 0x0001, Some(body(5)))]);
    define(&mut vm, "FbtImplY", None, &["FbtIface"], vec![method("m", "()V", 0x0001, Some(body(6)))]);
    let elems = vec![obj(&mut vm, "FbtImplX"), obj(&mut vm, "FbtImplY")];
    assert!(vm.fork_batch_target(&elems, "m", "()V").is_none());
}

/// Abstract-only, native and unresolved targets reject the whole batch.
#[test]
fn abstract_native_and_unresolved_targets_stay_serial() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtAbs", None, &[], vec![method("m", "()V", 0x0401, None)]);
    define(&mut vm, "FbtNat", None, &[], vec![method("m", "()V", 0x0101, None)]); // ACC_NATIVE
    define(&mut vm, "FbtOk", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    let abs = vec![obj(&mut vm, "FbtAbs")];
    assert!(vm.fork_batch_target(&abs, "m", "()V").is_none(), "abstract target rejects");
    let nat = vec![obj(&mut vm, "FbtNat")];
    assert!(vm.fork_batch_target(&nat, "m", "()V").is_none(), "native target rejects");
    let ghost = vec![obj(&mut vm, "FbtGhost")]; // never defined
    assert!(vm.fork_batch_target(&ghost, "m", "()V").is_none(), "unresolved class rejects");
    // One bad element poisons the whole batch even next to a good one.
    let mixed = vec![obj(&mut vm, "FbtOk"), obj(&mut vm, "FbtNat")];
    assert!(vm.fork_batch_target(&mixed, "m", "()V").is_none(), "any unproved receiver rejects the batch");
}

/// Null and non-object elements reject.
#[test]
fn null_or_untyped_elements_stay_serial() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "FbtNBase", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    let elems = vec![obj(&mut vm, "FbtNBase"), Value::Null];
    assert!(vm.fork_batch_target(&elems, "m", "()V").is_none());
    assert!(vm.fork_batch_target(&[], "m", "()V").is_none(), "empty batch rejects");
    assert!(vm.fork_batch_target(&[Value::Int(3)], "m", "()V").is_none());
}

/// GAP-S06: `virtual_dispatch_closure` enumerates every CONCRETE loaded subtype,
/// deduplicates identical inherited implementations, skips abstract subclasses,
/// rejects interface owners, and picks up a late-loaded override (epoch bump).
#[test]
fn dispatch_closure_enumerates_concrete_subtypes() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "VdcBase", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    define(&mut vm, "VdcInh", Some("VdcBase"), &[], vec![]); // inherits — dedups with base
    define(&mut vm, "VdcOvr", Some("VdcBase"), &[], vec![method("m", "()V", 0x0001, Some(body(1)))]);
    define_flags(&mut vm, "VdcAbs", Some("VdcBase"), &[], vec![], 0x0421); // abstract — never a receiver
    define(&mut vm, "VdcOther", None, &[], vec![method("m", "()V", 0x0001, Some(body(2)))]); // unrelated

    let reps = vm.virtual_dispatch_closure("VdcBase", "m", "()V").expect("class owner closes");
    let got: Vec<&str> = reps.iter().map(String::as_str).collect();
    // One representative per DISTINCT Code: the override plus exactly one of the
    // base/inheritor pair (they share one Code — enumeration order is unspecified).
    assert_eq!(got.len(), 2, "one rep per distinct Code; abstract/unrelated excluded: {got:?}");
    assert!(got.contains(&"VdcOvr"), "{got:?}");
    assert!(got.contains(&"VdcBase") ^ got.contains(&"VdcInh"), "{got:?}");

    // Interface owners are not enumerable (lambda implementors).
    define_flags(&mut vm, "VdcIface", None, &[], vec![method("m", "()V", 0x0401, None)], 0x0601);
    assert!(vm.virtual_dispatch_closure("VdcIface", "m", "()V").is_none());
    // Unloadable owner rejects.
    assert!(vm.virtual_dispatch_closure("VdcGhost", "m", "()V").is_none());

    // A LATE-loaded override invalidates the memoised closure (hierarchy epoch).
    define(&mut vm, "VdcLate", Some("VdcBase"), &[], vec![method("m", "()V", 0x0001, Some(body(3)))]);
    let reps2 = vm.virtual_dispatch_closure("VdcBase", "m", "()V").expect("still closes");
    let got2: Vec<&str> = reps2.iter().map(String::as_str).collect();
    assert_eq!(got2.len(), 3, "late override joins the closure: {got2:?}");
    assert!(got2.contains(&"VdcLate") && got2.contains(&"VdcOvr"), "{got2:?}");
}

/// GAP-S03: resolvable guest bytecode is ALWAYS analysed — a guest class that
/// happens to carry a JDK name gains nothing from the native contract table.
#[test]
fn guest_bytecode_is_analysed_even_under_jdk_names() {
    let mut vm = Vm::new(ClassLoader::new());
    // putstatic #1 is impure; the class calls itself java/lang/Integer.
    let impure = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x03, 0xb3, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() };
    define(&mut vm, "java/lang/Integer", None, &[], vec![method("fbtFrob", "()V", 0x0001, Some(impure))]);
    assert!(
        !vm.is_offloadable("java/lang/Integer", "fbtFrob", "()V", false),
        "an impure body under a JDK name must be analysed and rejected"
    );
}

/// GAP-S03: a genuinely native-served enum read is admitted only when the exact
/// receiver class is provably an enum (ACC_ENUM); an uncontracted native rejects.
#[test]
fn native_contract_requires_mechanical_proof() {
    let mut vm = Vm::new(ClassLoader::new());
    define_flags(&mut vm, "FbtRealEnum", Some("java/lang/Enum"), &[], vec![], 0x4031);
    define(&mut vm, "FbtFakeEnum", Some("java/lang/Enum"), &[], vec![]);
    assert!(vm.is_offloadable("FbtRealEnum", "ordinal", "()I", false), "ACC_ENUM class admits the enum read");
    assert!(!vm.is_offloadable("FbtFakeEnum", "ordinal", "()I", false), "no ACC_ENUM → no contract → reject");
    assert!(!vm.is_offloadable("FbtRealEnum", "fbtWeird", "()I", false), "uncontracted native rejects");
}

/// GAP-S02: the fork-safety tree proof follows the DISPATCH CLOSURE of a virtual
/// edge, not the constant-pool owner alone — an unsafe loaded override anywhere in
/// the closure rejects the whole tree; an exact (invokespecial) edge ignores it.
#[cfg(feature = "next-gen-gc")]
#[test]
fn fork_tree_proof_ranges_over_dispatch_closure() {
    use rava_classfile::Constant;
    // Pool with #1 = MethodRef S2Helper.h()V.
    let call_pool = || {
        ConstantPool::from_entries(vec![
            Constant::Unusable,
            Constant::MethodRef { class_index: 2, nat_index: 3 },
            Constant::Class { name_index: 4 },
            Constant::NameAndType { name_index: 5, desc_index: 6 },
            Constant::Utf8("S2Helper".into()),
            Constant::Utf8("h".into()),
            Constant::Utf8("()V".into()),
        ])
    };
    let virt_root = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2a, 0xb6, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() };
    let spec_root = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x2a, 0xb7, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() };
    let unsafe_h = Code { max_stack: 2, max_locals: 2, bytecode: vec![0x03, 0xb3, 0x00, 0x01, 0xb1], exceptions: Vec::new(), ..Default::default() }; // putstatic

    let build = |with_bad_override: bool| {
        let mut vm = Vm::new(ClassLoader::new());
        define(&mut vm, "S2Helper", None, &[], vec![method("h", "()V", 0x0001, Some(body(0)))]);
        if with_bad_override {
            define(&mut vm, "S2HBad", Some("S2Helper"), &[], vec![method("h", "()V", 0x0001, Some(unsafe_h.clone()))]);
        }
        let root = ClassFile::synthesize(
            call_pool(), 0x0021, "S2Root".into(), None, vec![],
            vec![
                MemberInfo { access_flags: 0x0001, name: "m".into(), descriptor: "()V".into(), code: Some(virt_root.clone()) },
                MemberInfo { access_flags: 0x0001, name: "ms".into(), descriptor: "()V".into(), code: Some(spec_root.clone()) },
            ],
        );
        vm.loader.define_synthetic(root);
        vm
    };

    // Clean hierarchy: the virtual edge's closure is all-safe → tree safe.
    let mut clean = build(false);
    let mut seen = std::collections::HashSet::new();
    assert!(clean.method_fork_safe("S2Root", "m", "()V", false, 0, &mut seen),
        "all-safe closure admits the tree");

    // An UNSAFE override loaded anywhere in the closure poisons the virtual edge...
    let mut poisoned = build(true);
    let mut seen = std::collections::HashSet::new();
    assert!(!poisoned.method_fork_safe("S2Root", "m", "()V", false, 0, &mut seen),
        "an unsafe loaded override anywhere in the dispatch closure rejects the tree");
    // ...but an EXACT invokespecial edge to the safe owner is unaffected by it.
    let mut seen = std::collections::HashSet::new();
    assert!(poisoned.method_fork_safe("S2Root", "ms", "()V", false, 0, &mut seen),
        "an exact special edge targets only its owner");
}

/// GAP-S07/S09: a GIL-free worker thread can READ already-loaded classes but a
/// cache MISS (a load) fails closed and requests the region abort.
#[test]
fn gilfree_thread_cannot_load_classes() {
    let _serial = crate::parallel::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "GflLoaded", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    crate::parallel::clear_abort();
    crate::parallel::set_gilfree(true);
    let hit = vm.loader.get_or_load("GflLoaded");
    let miss = vm.loader.get_or_load("GflNeverLoaded");
    let aborted = crate::parallel::abort_requested();
    crate::parallel::set_gilfree(false);
    crate::parallel::clear_abort();
    assert!(hit.is_ok(), "pre-published cache entries stay readable in a region");
    assert!(miss.is_err(), "a cache miss must not load on a gil-free worker");
    assert!(aborted, "the failed load must request the region abort (serial retry)");
    // Off the gil-free path the same miss may load (and here: fail as truly absent).
    assert!(vm.loader.get_or_load("GflNeverLoaded").is_err());
}

/// GAP-S08: one textual name = one class identity for the process lifetime. A
/// second definition under an already-resolved name cannot replace or alias it,
/// and resolution keeps returning the ORIGINAL identity (and its method bodies).
#[test]
fn class_identity_is_stable_per_name() {
    let mut vm = Vm::new(ClassLoader::new());
    define(&mut vm, "S8One", None, &[], vec![method("m", "()V", 0x0001, Some(body(0)))]);
    let first = vm.loader.cached("S8One").expect("defined");
    let first_code = resolved_code_key(&mut vm, "S8One");
    // Attempt to redefine the same name with a DIFFERENT body: must be a no-op.
    define(&mut vm, "S8One", None, &[], vec![method("m", "()V", 0x0001, Some(body(9)))]);
    let second = vm.loader.cached("S8One").expect("still defined");
    assert!(Arc::ptr_eq(&first, &second), "a published class identity is never replaced");
    assert_eq!(resolved_code_key(&mut vm, "S8One"), first_code, "resolution keeps the original body");
}

/// GAP-S08: admission proofs must not INITIALIZE classes — running `<clinit>`
/// merely to make a fork eligible would move JVM-visible initialization order.
/// Every proof (batch target, offload, closure, tree safety) leaves the class's
/// initialization state untouched.
#[test]
fn admission_proofs_do_not_initialize_classes() {
    let mut vm = Vm::new(ClassLoader::new());
    // A class WITH a <clinit> body: only ensure_init may run it.
    let clinit = Code { max_stack: 2, max_locals: 2, bytecode: vec![0xb1], exceptions: Vec::new(), ..Default::default() };
    define(&mut vm, "S8Init", None, &[], vec![
        method("m", "()V", 0x0001, Some(body(0))),
        method("<clinit>", "()V", 0x0008, Some(clinit)),
    ]);
    let elems = vec![obj(&mut vm, "S8Init")];
    let _ = vm.fork_batch_target(&elems, "m", "()V");
    let _ = vm.is_offloadable("S8Init", "m", "()V", false);
    let _ = vm.virtual_dispatch_closure("S8Init", "m", "()V");
    #[cfg(feature = "next-gen-gc")]
    {
        let mut seen = std::collections::HashSet::new();
        let _ = vm.method_fork_safe("S8Init", "m", "()V", false, 0, &mut seen);
    }
    let c = vm.loader.cached("S8Init").expect("loaded");
    assert!(!c.initialized.load(std::sync::atomic::Ordering::Relaxed), "no admission proof may start <clinit>");
    assert!(!c.clinit_done.load(std::sync::atomic::Ordering::Relaxed));
}
