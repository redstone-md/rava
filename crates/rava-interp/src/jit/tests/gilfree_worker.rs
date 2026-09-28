//! GIL-free worker contract: what a worker may publish, and where its faults land.

use super::*;

/// GAP-S09 acceptance instrumentation: a SUCCESSFUL detached parallel region performs
/// ZERO class loading, ZERO method compilation and ZERO worker dispatch misses — the
/// workers execute only the detached compiled plan. Class loads are already impossible
/// by construction (the S07 gil-free loader guard aborts the region); this test pins
/// the resolver/compiler/dispatch halves with live counters around a real fork.
#[cfg(feature = "next-gen-gc")]
#[test]
fn s09_workers_do_zero_resolution_or_compilation() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Same associative-reduction kernel as jit_parallel_fork_matches_serial_for_both_paths.
    let c = code(
        vec![
            0x09, 0x40, 0x03, 0x3e, // a=0; i=0
            0x1d, 0x1a, 0xa2, 0x00, 0x15, // header: iload i; iload n; if_icmpge +21
            0x1f, // lload a
            0x1d, 0x10, 0x1f, 0x68, 0x1d, 0x06, 0x7a, 0x82, // i*31 ^ (i>>3)
            0x85, 0x61, 0x40, // i2l; ladd; lstore a
            0x84, 0x03, 0x01, // iinc i,1
            0xa7, 0xff, 0xec, // goto header (-20)
            0x1f, 0xad, // lload a; lreturn
        ],
        5,
    );
    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile(&c, "(I)J", &empty_pool(), 0, &mut |_, _| (0, None));
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("data-parallel loop compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![50_000i64, 0, 0, 0, 0, 0];
    let loads0 = vm.loader.loaded_count();
    let compiled0 = vm.jit_compiled;
    let miss0 = crate::gil::worker_miss_count();
    // GAP-S09 stage 4: the admitted kernel must trip ZERO mode-aware vm_ref
    // denials — its workers reach no plain `&mut Vm` escape.
    let vmref_ab0 = s09census::abort_count(s09census::IDX_VM_REF_WORKER);
    let got = unsafe { f.call_desc(&mut locals, desc_cell) };
    set_current_vm(prev);
    crate::parallel::clear_abort();

    let serial = {
        let mut a = 0i64;
        for i in 0..50_000i32 {
            a = a.wrapping_add((i.wrapping_mul(31) ^ (i >> 3)) as i64);
        }
        a
    };
    assert_eq!(got, serial, "fork result exact");
    assert_eq!(vm.loader.loaded_count(), loads0, "workers loaded classes");
    assert_eq!(vm.jit_compiled, compiled0, "workers (or the region) compiled methods");
    assert_eq!(crate::gil::worker_miss_count(), miss0, "workers took the dispatch-miss path");
    assert_eq!(
        s09census::abort_count(s09census::IDX_VM_REF_WORKER),
        vmref_ab0,
        "a worker hit a plain vm_ref() denial inside an admitted kernel (stage 4)"
    );
}

/// GAP-S09 stage 2 — census acceptance: with `RAVA_S09_CB_AUDIT` armed, a
/// successful detached fork region records NO gil-free putstatic / indy /
/// cold-ldc / fu_invoke callbacks for this kernel (the worker-illegal group the
/// stage-3 abort stubs will target). Delta-based so the process-wide counters
/// can carry other census-enabled activity from sibling tests.
#[cfg(feature = "next-gen-gc")]
#[test]
fn s09_census_gilfree_counts_no_frozen_mutators() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    s09census::force_enable_for_test();
    // Same associative-reduction kernel as s09_workers_do_zero_resolution_or_compilation.
    let c = code(
        vec![
            0x09, 0x40, 0x03, 0x3e, // a=0; i=0
            0x1d, 0x1a, 0xa2, 0x00, 0x15, // header: iload i; iload n; if_icmpge +21
            0x1f, // lload a
            0x1d, 0x10, 0x1f, 0x68, 0x1d, 0x06, 0x7a, 0x82, // i*31 ^ (i>>3)
            0x85, 0x61, 0x40, // i2l; ladd; lstore a
            0x84, 0x03, 0x01, // iinc i,1
            0xa7, 0xff, 0xec, // goto header (-20)
            0x1f, 0xad, // lload a; lreturn
        ],
        5,
    );
    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile(&c, "(I)J", &empty_pool(), 0, &mut |_, _| (0, None));
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("data-parallel loop compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let before = [
        s09census::count(2),                       // putstatic
        s09census::count(s09census::IDX_INDY),     // invokedynamic
        s09census::count(s09census::IDX_LDC),      // cold ldc
        s09census::count(s09census::IDX_FU_INVOKE) // fastutil invoke
    ];
    // Stage-3 acceptance: the admitted kernel must never fire an abort stub.
    // Stage-4 extends the row with the mode-aware vm_ref denial kind: the
    // reduction fork must complete without ANY plain-vm_ref worker denial.
    const AB_KINDS: [usize; 8] = [
        2, // putstatic
        5, // newarray/anewarray
        s09census::IDX_INDY,
        s09census::IDX_LDC,
        s09census::IDX_FU_INVOKE,
        s09census::IDX_MAP_MUT,
        s09census::IDX_BITSET_MUT,
        s09census::IDX_VM_REF_WORKER,
    ];
    let ab_before: Vec<u64> = AB_KINDS.iter().map(|&k| s09census::abort_count(k)).collect();
    let mut locals = vec![50_000i64, 0, 0, 0, 0, 0];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) };
    set_current_vm(prev);
    crate::parallel::clear_abort();

    let serial = {
        let mut a = 0i64;
        for i in 0..50_000i32 {
            a = a.wrapping_add((i.wrapping_mul(31) ^ (i >> 3)) as i64);
        }
        a
    };
    assert_eq!(got, serial, "fork result exact");
    assert_eq!(s09census::count(2), before[0], "gil-free putstatic callbacks fired");
    assert_eq!(s09census::count(s09census::IDX_INDY), before[1], "gil-free indy callbacks fired");
    assert_eq!(s09census::count(s09census::IDX_LDC), before[2], "gil-free cold-ldc callbacks fired");
    assert_eq!(s09census::count(s09census::IDX_FU_INVOKE), before[3], "gil-free fu_invoke callbacks fired");
    for (i, &k) in AB_KINDS.iter().enumerate() {
        assert_eq!(
            s09census::abort_count(k),
            ab_before[i],
            "stage-3 abort stub (census kind {k}) fired inside an admitted kernel"
        );
    }
}

/// GAP-S09 stage 2 — LIVE HOLE 2 closure: a kind-A-style worker with the
/// `WorkerJitCtxGuard` installed latches a callback fault (`rava_throw` →
/// `set_jit_fault`) into its PRIVATE JitCtx, never the shared block — exactly
/// the apply-leaf worker discipline (install after coordinator reserves, RAII
/// drop before teardown), verified against a real `Vm` heap.
#[cfg(feature = "next-gen-gc")]
#[test]
fn s09_kind_a_worker_fault_lands_in_worker_latch() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    // No guard on the coordinator thread → the SHARED JitCtx address.
    let shared_ctx = vm.heap.array_desc_cell_ptr();
    let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let prev = vmref.install_current();
            {
                let _guard = vmref.install_worker_jit_ctx();
                let worker_ctx = vmref.active_jit_ctx_ptr();
                assert_ne!(worker_ctx, shared_ctx, "guard must swap the active JitCtx");
                // The exact snapshot bases the shared block published.
                unsafe {
                    assert_eq!(
                        (*(worker_ctx as *const crate::heap::JitCtx)).desc_base,
                        (*(shared_ctx as *const crate::heap::JitCtx)).desc_base,
                        "worker ctx snapshots the coordinator-published bases"
                    );
                }
                // A compiled-code fault path: rava_throw sets the pending slot
                // (worker TLS) and latches the ACTIVE JitCtx fault word.
                rava_throw(0);
                unsafe {
                    assert_eq!(
                        (*(worker_ctx as *const crate::heap::JitCtx)).fault,
                        1,
                        "fault lands in the WORKER latch"
                    );
                    assert_eq!(
                        (*(shared_ctx as *const crate::heap::JitCtx)).fault,
                        0,
                        "shared latch untouched while the guard is installed"
                    );
                }
                crate::execution_state::pending_clear();
            } // RAII: guard restores the shared ctx here (also on unwind).
            assert_eq!(
                vmref.active_jit_ctx_ptr(),
                shared_ctx,
                "dropping the guard restores the shared JitCtx"
            );
            set_current_vm(prev);
        });
    });
    // After the region: the coordinator's latch was never written.
    unsafe {
        assert_eq!((*(shared_ctx as *const crate::heap::JitCtx)).fault, 0);
    }
}

/// GAP-S09 stage 3 — LIVE HOLE 1 closure (negative): a synthetic gil-free
/// caller invoking `rava_putstatic` hits the abort stub — the region abort is
/// requested, the return is the neutral 0, and NOTHING was published (the
/// target class's statics stay empty). A serial control call on the same site
/// first proves the machinery would otherwise publish. Mirrors the
/// s09_kind_a_worker_fault_lands_in_worker_latch discipline (TEST_LOCK,
/// set_gilfree on the worker, clear_abort after).
#[cfg(feature = "next-gen-gc")]
#[test]
fn s09_worker_illegal_putstatic_aborts_without_publication() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Pin the OUTSIDE-the-envelope half; see the companion test for the inside half.
    let _mode = LockedExecMode::force(false);
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    // Pool: [0]=Unusable [1]=Utf8 class [2]=Class [3]=Utf8 "F" [4]=Utf8 "I"
    //       [5]=NameAndType [6]=FieldRef  →  putstatic cp = 6.
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("S09PutTgt".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("F".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    let file = rava_classfile::ClassFile::synthesize(
        pool,
        0x0021,
        "S09PutTgt".into(),
        None,
        Vec::new(),
        Vec::new(),
    );
    let cls = vm.loader.define_synthetic(file);
    let caller = std::sync::Arc::as_ptr(&cls) as i64;

    // Serial CONTROL: with the GIL held (not gil-free) the same call publishes.
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    assert_eq!(rava_putstatic(caller, 6, 7, 0), 0);
    set_current_vm(prev);
    assert!(
        matches!(cls.statics.read().unwrap_or_else(|e| e.into_inner()).get("F"), Some(crate::Value::Int(7))),
        "serial control call must publish the static"
    );
    cls.statics.write().unwrap_or_else(|e| e.into_inner()).clear();

    // Synthetic gil-free caller: the stub aborts BEFORE the static write.
    let ab0 = s09census::abort_count(2);
    let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let prev = vmref.install_current();
            crate::parallel::set_gilfree(true);
            let r = rava_putstatic(caller, 6, 7, 0);
            crate::parallel::set_gilfree(false);
            assert_eq!(r, 0, "abort stub must return the neutral 0");
            assert!(crate::parallel::abort_requested(), "stub must request the region abort");
            set_current_vm(prev);
        });
    });
    assert!(crate::parallel::abort_requested());
    crate::parallel::clear_abort();
    assert_eq!(s09census::abort_count(2), ab0 + 1, "putstatic abort tallied in the census");
    assert!(
        cls.statics.read().unwrap_or_else(|e| e.into_inner()).is_empty(),
        "gil-free putstatic must publish NOTHING (static cell unchanged)"
    );
    assert!(!vm.jit_pending_is_set(), "no pending exception on the coordinator");
}

/// The OTHER half of the same contract, and the half that had no test.
///
/// Under `worker_locked_exec` a gil-free worker does NOT abort at a worker-illegal mutator:
/// `worker_illegal_gate` hands it `WorkerGate::Locked`, it takes the re-entrant resolve lock,
/// and it publishes. That is the entire premise of the envelope — main is parked at the scope
/// join and every sibling blocks on that same lock, so the holder has exactly the exclusivity
/// `&mut` denotes — and it is the rule `vm_ref` already uses to hand out `&mut Vm`.
///
/// `putstatic` is the right site to assert it on: `Heap::write_static_cell` is precisely what
/// `RAVA_FREEZE_TALLY` recorded a real ring region reaching, twice in 430 million callbacks.
///
/// Asserting this matters because the frozen-field guard used to forbid ALL gil-free mutation,
/// contradicting `vm_ref`'s admission rule. The contradiction stayed invisible while the ring
/// lane was opt-in — no checked build ever ran a worker through the envelope — and surfaced
/// the moment the lane was made a default.
#[cfg(feature = "next-gen-gc")]
#[test]
fn s09_worker_putstatic_publishes_inside_the_locked_envelope() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _mode = LockedExecMode::force(true);
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("S09PutEnv".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("F".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    let file = rava_classfile::ClassFile::synthesize(
        pool,
        0x0021,
        "S09PutEnv".into(),
        None,
        Vec::new(),
        Vec::new(),
    );
    let cls = vm.loader.define_synthetic(file);
    let caller = std::sync::Arc::as_ptr(&cls) as i64;

    // Serial control first — it also INITIALISES the class. Without it the worker trips the
    // clinit barrier (a worker may never run `<clinit>`) and aborts before ever reaching the
    // static write, so the test would be asserting the barrier rather than the envelope.
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    assert_eq!(rava_putstatic(caller, 6, 7, 0), 0);
    set_current_vm(prev);
    assert!(
        matches!(cls.statics.read().unwrap_or_else(|e| e.into_inner()).get("F"), Some(crate::Value::Int(7))),
        "serial control call must publish the static"
    );
    cls.statics.write().unwrap_or_else(|e| e.into_inner()).clear();

    crate::parallel::clear_abort();
    let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let prev = vmref.install_current();
            crate::parallel::set_gilfree(true);
            let r = rava_putstatic(caller, 6, 9, 0);
            crate::parallel::set_gilfree(false);
            assert_eq!(r, 0, "putstatic returns the neutral 0 either way");
            set_current_vm(prev);
        });
    });
    assert!(
        !crate::parallel::abort_requested(),
        "inside the envelope the worker must NOT tear the region down"
    );
    assert!(
        matches!(cls.statics.read().unwrap_or_else(|e| e.into_inner()).get("F"), Some(crate::Value::Int(9))),
        "inside the envelope the worker PUBLISHES the static"
    );
    assert!(!vm.jit_pending_is_set(), "no pending exception on the coordinator");
}

/// GAP-S09 stage 3 (negative, map-mutator family): a synthetic gil-free caller
/// invoking `rava_map_put` gets the abort stub — abort requested, neutral 0
/// return, and the shared native `$map` is untouched (the key was never
/// inserted; a previously-inserted entry survives intact).
#[cfg(feature = "next-gen-gc")]
#[test]
fn s09_worker_illegal_map_put_aborts_without_publication() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Pin the mode this test is about. The abort stub is the OUTSIDE-the-envelope half of the
    // contract; with locked-exec on, the same call legitimately publishes under the resolve
    // lock (see the companion test). Without pinning, which half runs would depend on how the
    // process was launched — and once the ring lane becomes a default, on nothing the test
    // controls at all.
    let _mode = LockedExecMode::force(false);
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let h = vm.heap.alloc("java/util/HashMap");
    let recv = (h as i64) | REF_TAG;

    // Serial CONTROL: put(1 → 11) publishes.
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let k1 = box_ref(&vm, &JVal::Int(1));
    let v1 = box_ref(&vm, &JVal::Int(11));
    rava_map_put(recv, k1, v1);
    assert_eq!(vm.jit_map_contains(h, k1), 1, "serial control put must publish");

    // Synthetic gil-free caller: put(2 → 22) must abort without publishing.
    let k2 = box_ref(&vm, &JVal::Int(2));
    let v2 = box_ref(&vm, &JVal::Int(22));
    set_current_vm(prev);
    let ab0 = s09census::abort_count(s09census::IDX_MAP_MUT);
    let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let prev = vmref.install_current();
            crate::parallel::set_gilfree(true);
            let r = rava_map_put(recv, k2, v2);
            crate::parallel::set_gilfree(false);
            assert_eq!(r, 0, "abort stub must return the neutral 0 (null previous value)");
            assert!(crate::parallel::abort_requested(), "stub must request the region abort");
            set_current_vm(prev);
        });
    });
    assert!(crate::parallel::abort_requested());
    crate::parallel::clear_abort();
    assert_eq!(
        s09census::abort_count(s09census::IDX_MAP_MUT),
        ab0 + 1,
        "map-mutator abort tallied in the census"
    );
    // The map is unchanged: key 2 absent, key 1 → 11 intact.
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    assert_eq!(vm.jit_map_contains(h, k2), 0, "aborted put must publish NOTHING");
    let got = vm.jit_map_get(h, k1);
    assert!(matches!(unbox_ref(&vm, got, ""), JVal::Int(11)), "pre-existing entry intact");
    set_current_vm(prev);
}

#[cfg(feature = "next-gen-gc")]
#[test]
fn access_order_get_callbacks_abort_before_reordering_on_workers() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _mode = LockedExecMode::force(false);
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let h = vm.heap.alloc("java/util/LinkedHashMap");
    let id = vm.heap.alloc_map();
    vm.heap.put_field(h, "$map", JVal::Int(id as i32));
    vm.heap.put_field(h, "$accessOrder", JVal::Int(1));
    for i in 1..=2 {
        vm.heap.map_put(id, crate::heap::MapKey::Int(i), JVal::Int(i as i32), JVal::Int(i as i32));
    }
    let version = vm.heap.map_version(id);
    let recv = (h as i64) | REF_TAG;
    for callback in [rava_map_get as extern "C" fn(i64, i64) -> i64, rava_amap_get] {
        let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let prev = vmref.install_current();
                let key = box_ref(vm_ref_worker_sanctioned().unwrap(), &JVal::Int(1));
                crate::parallel::set_gilfree(true);
                let result = callback(recv, key);
                crate::parallel::set_gilfree(false);
                set_current_vm(prev);
                assert_eq!(result, 0);
            });
        });
        assert!(crate::parallel::abort_requested());
        crate::parallel::clear_abort();
        assert_eq!(vm.heap.map_version(id), version);
        assert_eq!(vm.heap.map_index_lookup(id, &crate::heap::MapKey::Int(1)), Some(0));
    }
}
