//! cow_bloom_tests.

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn noted_shadow_keys_are_never_rejected_and_clear_resets_filter() {
    let _guard = TEST_LOCK.lock().unwrap();
    assert_eq!(std::mem::offset_of!(crate::heap::JitCtx, cow_bloom_base), 56);
    assert_ne!(crate::heap::JitCtx::default().cow_bloom_base, 0);
    crate::parallel::shadow_clear();
    let keys = [
        (0x0000_1234_5678_u64, 17_u32),
        (0x0000_4321_1000_u64, 9_001_u32),
        (23_u64, 17_u32),
    ];

    for (index, &(object, field)) in keys.iter().enumerate() {
        crate::parallel::shadow_put_field(object, field, crate::Value::Int(index as i32));
        assert!(crate::heap::cow_bloom_maybe(object, field));
        assert!(crate::parallel::shadow_get_field(object, field).is_some());
    }

    crate::parallel::shadow_clear();
    for &(object, field) in &keys {
        assert!(!crate::heap::cow_bloom_maybe(object, field));
        assert!(crate::parallel::shadow_get_field(object, field).is_none());
    }
}

#[test]
fn concurrent_shadows_balance_counting_filter() {
    let _guard = TEST_LOCK.lock().unwrap();
    const WORKERS: usize = 4;
    const KEYS: usize = 32;
    let ready = std::sync::Arc::new(std::sync::Barrier::new(WORKERS));

    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let ready = std::sync::Arc::clone(&ready);
            scope.spawn(move || {
                crate::parallel::shadow_clear();
                for index in 0..KEYS {
                    let object = ((worker as u64 + 1) << 32) | index as u64;
                    let field = (worker * KEYS + index) as u32;
                    crate::parallel::shadow_put_field(
                        object,
                        field,
                        crate::Value::Int(index as i32),
                    );
                }
                ready.wait();
                for index in 0..KEYS {
                    let object = ((worker as u64 + 1) << 32) | index as u64;
                    let field = (worker * KEYS + index) as u32;
                    assert!(crate::heap::cow_bloom_maybe(object, field));
                }
                ready.wait();
                crate::parallel::shadow_clear();
            });
        }
    });

    for worker in 0..WORKERS {
        for index in 0..KEYS {
            let object = ((worker as u64 + 1) << 32) | index as u64;
            let field = (worker * KEYS + index) as u32;
            assert!(!crate::heap::cow_bloom_maybe(object, field));
        }
    }
}

#[test]
fn worker_contexts_isolate_identical_shadow_keys() {
    let _guard = TEST_LOCK.lock().unwrap();
    let key = (0x1234_5678_u64, 77_u32);
    crate::parallel::shadow_clear();
    assert!(!crate::heap::cow_bloom_maybe(key.0, key.1));

    let published = std::sync::Arc::new(std::sync::Barrier::new(2));
    let checked = std::sync::Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let published_a = std::sync::Arc::clone(&published);
        let checked_a = std::sync::Arc::clone(&checked);
        scope.spawn(move || {
            let _ctx = crate::heap::install_worker_jit_ctx_for_test(
                crate::heap::JitCtx::default(),
            );
            crate::heap::cow_bloom_note(key.0, key.1);
            assert!(crate::heap::cow_bloom_maybe(key.0, key.1));
            published_a.wait();
            checked_a.wait();
            crate::heap::cow_bloom_remove(key.0, key.1);
        });

        let published_b = std::sync::Arc::clone(&published);
        let checked_b = std::sync::Arc::clone(&checked);
        scope.spawn(move || {
            let _ctx = crate::heap::install_worker_jit_ctx_for_test(
                crate::heap::JitCtx::default(),
            );
            published_b.wait();
            assert!(
                !crate::heap::cow_bloom_maybe(key.0, key.1),
                "another worker's shadow must not create a local false positive"
            );
            checked_b.wait();
        });
    });

    assert!(!crate::heap::cow_bloom_maybe(key.0, key.1));
}

#[test]
fn worker_bloom_storage_is_fresh_after_each_guard() {
    let _guard = TEST_LOCK.lock().unwrap();
    let key = (0x8765_4321_u64, 91_u32);
    crate::parallel::shadow_clear();
    assert!(!crate::heap::cow_bloom_maybe(key.0, key.1));

    for value in 0..8 {
        {
            let _ctx = crate::heap::install_worker_jit_ctx_for_test(
                crate::heap::JitCtx::default(),
            );
            assert!(!crate::heap::cow_bloom_maybe(key.0, key.1));
            crate::parallel::shadow_put_field(key.0, key.1, crate::Value::Int(value));
            assert!(crate::heap::cow_bloom_maybe(key.0, key.1));
        }
        assert!(
            !crate::heap::cow_bloom_maybe(key.0, key.1),
            "worker operations must not mutate the shared canonical table"
        );
    }
}

#[test]
fn worker_fault_latch_does_not_mutate_shared_context() {
    let _guard = TEST_LOCK.lock().unwrap();
    let mut shared = crate::heap::JitCtx::default();
    {
        let _ctx = crate::heap::install_worker_jit_ctx_for_test(shared);
        crate::heap::set_jit_fault_for_test(&mut shared, 1);
        assert_eq!(shared.fault, 0);
        assert_eq!(crate::heap::active_jit_fault_for_test(&shared), 1);
        crate::heap::set_jit_fault_for_test(&mut shared, 0);
        assert_eq!(crate::heap::active_jit_fault_for_test(&shared), 0);
    }

    crate::heap::set_jit_fault_for_test(&mut shared, 1);
    assert_eq!(shared.fault, 1, "ordinary GIL path keeps the shared latch");
}
