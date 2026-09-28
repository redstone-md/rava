//! Resolve-lock ownership and the parallel fork paths.

use super::*;

/// KIT 3 FORK e2e: a counter-dependent long-accumulator loop
/// `long h(int n){ long a=0; for(i=0;i<n;i++) a += (long)(i*31 ^ (i>>3)); return a; }`
/// compiled with RAVA_JIT_PARALLEL=1 forks across the pool for a big trip (≥10k)
/// and takes the serial fallback below it — BOTH must equal the exact serial fold.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_parallel_fork_matches_serial_for_both_paths() {
    // Serialise against other pool/region-stat users + the global env var.
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
    // Serial reference fold (i32-wrapping body expr, i2l, wrapping long add).
    let serial_ref = |n: i32| -> i64 {
        let mut a = 0i64;
        for i in 0..n {
            let e = i.wrapping_mul(31) ^ (i >> 3);
            a = a.wrapping_add(e as i64);
        }
        a
    };

    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile(&c, "(I)J", &empty_pool(), 0, &mut |_, _| (0, None));
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("data-parallel loop compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let run = |n: i32| -> i64 {
        let mut locals = vec![n as i64, 0, 0, 0, 0, 0];
        unsafe { f.call_desc(&mut locals, desc_cell) }
    };
    // 50_000 ≥ MIN_PARALLEL_TRIP → the FORK path; 5_000 + 0 + negative → SERIAL.
    for n in [50_000i32, 30_000, 5_000, 1, 0, -7] {
        assert_eq!(run(n), serial_ref(n), "fork/serial result for n={n}");
    }
    set_current_vm(prev);
    crate::parallel::clear_abort();
}

/// GAP-S09 stage 4 (negative): a gil-free thread calling PLAIN `vm_ref()`
/// WITHOUT the locked-exec envelope is DENIED — None back, the region abort
/// requested, and the denial tallied in the census ABORTS row — while
/// `vm_shared()` (the sound shared borrow) keeps working on the same thread.
/// Mirrors the stage-3 negative-test discipline (TEST_LOCK, set_gilfree on a
/// scoped thread, clear_abort after).
#[cfg(feature = "next-gen-gc")]
#[test]
fn s4_gilfree_plain_vm_ref_denied_and_aborts() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    crate::parallel::clear_abort();
    let ab0 = s09census::abort_count(s09census::IDX_VM_REF_WORKER);
    let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let prev = vmref.install_current();
            // Control: GIL-held/serial vm_ref is untouched by the mode gate.
            assert!(vm_ref().is_some(), "serial vm_ref unchanged");
            crate::parallel::set_gilfree(true);
            expect_worker_denial_for_test(true);
            assert!(vm_ref().is_none(), "gil-free plain vm_ref must be denied");
            expect_worker_denial_for_test(false);
            assert!(
                crate::parallel::abort_requested(),
                "denial must request the region abort"
            );
            assert!(
                vm_shared().is_some(),
                "the shared borrow stays available to gil-free workers"
            );
            // The sanctioned accessor is the documented escape (its sites
            // carry the soundness notes) — it still resolves.
            assert!(vm_ref_worker_sanctioned().is_some());
            crate::parallel::set_gilfree(false);
            set_current_vm(prev);
        });
    });
    crate::parallel::clear_abort();
    assert_eq!(
        s09census::abort_count(s09census::IDX_VM_REF_WORKER),
        ab0 + 1,
        "the denial is tallied under the vm_ref_worker census kind"
    );
}

/// GAP-S09 stage 4: with `RAVA_M3_WORKER_LOCKED_EXEC`-style conditions — the
/// re-entrant resolve lock HELD by this thread — the locked allowance admits
/// `vm_ref()` on a gil-free thread (the S2 locked-exec envelope). Uses the
/// lock directly rather than the env gate (read-once OnceLock), so only the
/// ownership half is provable here; the env half is covered by the denial
/// test above running with the gate unset.
#[cfg(feature = "next-gen-gc")]
#[test]
fn s4_resolve_lock_ownership_is_thread_local() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    assert!(!resolve_lock_held_by_me());
    {
        let _outer = resolve_lock_acquire();
        assert!(resolve_lock_held_by_me(), "owner sees its own hold");
        {
            let _nested = resolve_lock_acquire(); // re-entrant pass-through
            assert!(resolve_lock_held_by_me());
        }
        assert!(resolve_lock_held_by_me(), "still owned after nested release");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                assert!(
                    !resolve_lock_held_by_me(),
                    "a sibling thread must NOT see the envelope"
                );
            });
        });
    }
    assert!(!resolve_lock_held_by_me(), "released at the top frame");
}

/// GAP-S09 stage 4 — kind-A `WorkerContext` RAII: after a FORCED PANIC inside
/// the worker closure, the context's Drop (run during unwind) restores every
/// per-thread piece it installed — the gil-free flag, the active JitCtx (back
/// to the shared block) and CURRENT_VM — so a reused thread can never observe
/// worker state. Mirrors the s09_kind_a_worker_fault_lands_in_worker_latch
/// discipline.
#[cfg(feature = "next-gen-gc")]
#[test]
fn s4_worker_context_raii_restores_on_panic() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let shared_ctx = vm.heap.array_desc_cell_ptr();
    let vmref = crate::parallel::WorkerVmRef::new(&mut vm);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let ctx = crate::parallel::WorkerContext::enter(vmref, true, None);
                assert!(crate::parallel::gilfree(), "enter arms the gil-free flag");
                assert_ne!(
                    ctx.active_jit_ctx_ptr(),
                    shared_ctx,
                    "enter installs the private JitCtx"
                );
                assert!(vm_shared().is_some(), "enter publishes CURRENT_VM");
                panic!("forced worker panic (RAII probe)");
            }));
            assert!(unwound.is_err(), "the closure must have panicked");
            // Drop ran during the unwind: every flag/TLS piece is restored.
            assert!(!crate::parallel::gilfree(), "gil-free flag restored on unwind");
            assert_eq!(
                vmref.active_jit_ctx_ptr(),
                shared_ctx,
                "shared JitCtx restored on unwind"
            );
            assert!(vm_shared().is_none(), "CURRENT_VM cleared on unwind");
        });
    });
    // The coordinator side is untouched: shared latch never written, no abort raised.
    unsafe {
        assert_eq!((*(shared_ctx as *const crate::heap::JitCtx)).fault, 0);
    }
    crate::parallel::clear_abort();
}

/// KIT 4 / CAPTURE / INT-ACC e2e: `int hot(int n, int base){ int sum=0;
/// for(i=0;i<n;i++) sum += i*7 + base; return sum; }` — an INT accumulator
/// reading a CAPTURED external param (`base`). Must auto-parallelize (fork ≥10k,
/// serial below) and be byte-exact vs the i32-wrapping serial fold for both paths,
/// including negative `base` (sign-overflow truncation correctness).
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_parallel_fork_int_accumulator_with_captured_param() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let c = code(
        vec![
            0x03, 0x3d, 0x03, 0x3e, // sum=0; i=0
            0x1d, 0x1a, 0xa2, 0x00, 0x12, // header: iload i; iload n; if_icmpge +18
            0x1c, // iload_2 sum  (load acc)
            0x1d, 0x10, 0x07, 0x68, // i*7
            0x1b, 0x60, // + base  (CAPTURE local 1)
            0x60, 0x3d, // sum += ...; istore_2
            0x84, 0x03, 0x01, // iinc i,1
            0xa7, 0xff, 0xef, // goto header (-17)
            0x1c, 0xac, // iload_2; ireturn
        ],
        5,
    );
    // Confirm the analyzer captured `base` (local 1) for an int accumulator.
    let dp = analyze_data_parallel_loop(&c.bytecode, Some(JT::Int)).expect("data-parallel");
    assert!(!dp.acc_is_long, "int accumulator");
    assert_eq!(dp.captures, vec![(1usize, false)], "captured `base` (local 1, int)");

    let serial_ref = |n: i32, base: i32| -> i32 {
        let mut s = 0i32;
        for i in 0..n {
            s = s.wrapping_add(i.wrapping_mul(7).wrapping_add(base));
        }
        s
    };

    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile(&c, "(II)I", &empty_pool(), 0, &mut |_, _| (0, None));
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("int/capture data-parallel loop compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let run = |n: i32, base: i32| -> i32 {
        let mut locals = vec![n as i64, base as i64, 0, 0, 0, 0];
        unsafe { f.call_desc(&mut locals, desc_cell) as i32 }
    };
    for (n, base) in [(50_000i32, 3i32), (40_000, -11), (5_000, 7), (1, 100), (0, 5), (30_000, -1)] {
        assert_eq!(run(n, base), serial_ref(n, base), "int+capture for n={n} base={base}");
    }
    set_current_vm(prev);
    crate::parallel::clear_abort();
}

/// SCRATCH-LOCAL e2e: the DIRTY javac shape with intermediate temps —
/// `long hot(int n, int base){ long sum=0; for(i=0;i<n;i++){ int a=i*base;
/// int b=(i^85)+a; sum += a+b; } }` — uses istore/iload for `a`,`b` inside the
/// body. Must now be recognised, compiled to a body fn (scratch → isolated
/// Cranelift Variables), and be byte-exact vs the serial fold for fork + serial.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_parallel_fork_scratch_locals_dirty_body() {
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let c = code(
        vec![
            0x09, 0x41, 0x03, 0x36, 0x04, // sum=0; i=0
            0x15, 0x04, 0x1a, 0xa2, 0x00, 0x22, // header: iload i; iload n; if_icmpge +34
            0x15, 0x04, 0x1b, 0x68, 0x36, 0x05, // a = i * base; istore 5
            0x15, 0x04, 0x10, 0x55, 0x82, 0x15, 0x05, 0x60, 0x36, 0x06, // b=(i^85)+a; istore 6
            0x20, // lload_2 (load acc, skipped)
            0x15, 0x05, 0x15, 0x06, 0x60, // iload a; iload b; iadd
            0x85, 0x61, 0x41, // i2l; ladd; lstore_2
            0x84, 0x04, 0x01, // iinc i,1
            0xa7, 0xff, 0xde, // goto header (-34)
            0x20, 0xad, // lload_2; lreturn
        ],
        7,
    );
    let dp = analyze_data_parallel_loop(&c.bytecode, Some(JT::Long)).expect("dirty loop recognised");
    assert!(dp.acc_is_long);
    assert_eq!(dp.captures, vec![(1usize, false)], "captured `base`");
    assert_eq!(dp.scratch.len(), 2, "scratch temps a + b");
    assert!(dp.scratch.iter().any(|&(l, _)| l == 5) && dp.scratch.iter().any(|&(l, _)| l == 6));

    let serial_ref = |n: i32, base: i32| -> i64 {
        let mut sum = 0i64;
        for i in 0..n {
            let a = i.wrapping_mul(base);
            let b = (i ^ 0x55).wrapping_add(a);
            sum = sum.wrapping_add(a.wrapping_add(b) as i64);
        }
        sum
    };

    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile(&c, "(II)J", &empty_pool(), 0, &mut |_, _| (0, None));
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("scratch-local data-parallel loop compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let run = |n: i32, base: i32| -> i64 {
        let mut locals = vec![n as i64, base as i64, 0, 0, 0, 0, 0];
        unsafe { f.call_desc(&mut locals, desc_cell) }
    };
    for (n, base) in [(50_000i32, 13i32), (40_000, -7), (5_000, 3), (1, 9), (0, 4), (25_000, -1)] {
        assert_eq!(run(n, base), serial_ref(n, base), "scratch fork/serial for n={n} base={base}");
    }
    set_current_vm(prev);
    crate::parallel::clear_abort();
}
