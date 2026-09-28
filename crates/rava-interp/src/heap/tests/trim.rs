//! Table trimming: what survives a capacity trim and what stays reusable.

use super::*;

fn reserve_table_trim_slack<T>(table: &mut Vec<T>) {
    let element = std::mem::size_of::<T>().max(1);
    let extra = crate::heap_table_trim::MIN_RECLAIMED_BYTES / element + 64;
    table.reserve_exact(extra);
}

#[test]
fn heap_table_trim_preserves_handles_and_republishes_jit_bases() {
    let mut heap = Heap::default();
    let object = heap.alloc("Keep");
    let array = heap.alloc_array("int", 1);
    heap.array_set(array, 0, Value::Int(37));

    reserve_table_trim_slack(&mut heap.objects.write().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.obj_descs.write().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.arrays.write().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.array_descs.write().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.free_map);
    let before = (
        heap.objects.read().unwrap_or_else(|e| e.into_inner()).capacity(),
        heap.obj_descs.read().unwrap_or_else(|e| e.into_inner()).capacity(),
        heap.arrays.read().unwrap_or_else(|e| e.into_inner()).capacity(),
        heap.array_descs.read().unwrap_or_else(|e| e.into_inner()).capacity(),
    );

    assert!(heap.trim_table_capacities(true) > 0);
    let after = (
        heap.objects.read().unwrap_or_else(|e| e.into_inner()).capacity(),
        heap.obj_descs.read().unwrap_or_else(|e| e.into_inner()).capacity(),
        heap.arrays.read().unwrap_or_else(|e| e.into_inner()).capacity(),
        heap.array_descs.read().unwrap_or_else(|e| e.into_inner()).capacity(),
    );
    assert!(after.0 < before.0 && after.1 < before.1);
    assert!(after.2 < before.2 && after.3 < before.3);
    assert_eq!(heap.class_of(object), Some("Keep"));
    assert_eq!(heap.array_get(array, 0).and_then(|value| value.as_int()), Some(37));
    assert_eq!(heap.jit_ctx.objects_base, heap.objects.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize);
    assert_eq!(heap.jit_ctx.obj_desc_base, heap.obj_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize);
    assert_eq!(heap.jit_ctx.desc_base, heap.array_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize);
}

#[test]
fn heap_table_trim_keeps_free_handles_unique_and_reusable() {
    let mut heap = Heap::default();
    let rooted_object = heap.alloc("Rooted");
    let dead_object = heap.alloc("Dead");
    let rooted_array = heap.alloc_array("int", 1);
    let dead_array = heap.alloc_array("int", 1);
    heap.gc_with_object_slot_policy(
        &[Value::Ref(rooted_object), Value::Array(rooted_array)],
        true,
    );
    assert_eq!(*heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()), [dead_object]);
    assert_eq!(*heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()), [dead_array]);

    reserve_table_trim_slack(&mut heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()));
    reserve_table_trim_slack(&mut heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()));
    assert!(heap.trim_table_capacities(true) > 0);
    assert_eq!(*heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()), [dead_object]);
    assert_eq!(*heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()), [dead_array]);
    assert_eq!(heap.alloc("Reused"), dead_object);
    assert_eq!(heap.alloc_array("int", 1), dead_array);
    assert!(heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
}

#[test]
fn heap_table_trim_skips_every_active_parallel_state() {
    let _serial = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut heap = Heap::default();
    heap.alloc("Keep");
    reserve_table_trim_slack(&mut heap.objects.write().unwrap_or_else(|e| e.into_inner()));
    let before_region = heap.objects.read().unwrap_or_else(|e| e.into_inner()).capacity();

    crate::parallel::enter_region();
    let region_result = heap.trim_table_capacities(true);
    crate::parallel::leave_region();
    assert_eq!(region_result, 0);
    assert_eq!(heap.objects.read().unwrap_or_else(|e| e.into_inner()).capacity(), before_region);

    heap.parallel_array_reserve(1);
    let before_array_region = heap.objects.read().unwrap_or_else(|e| e.into_inner()).capacity();
    assert_eq!(heap.trim_table_capacities(true), 0);
    assert_eq!(heap.objects.read().unwrap_or_else(|e| e.into_inner()).capacity(), before_array_region);
    heap.parallel_array_commit();

    assert!(heap.trim_table_capacities(true) > 0);
    assert_eq!(crate::parallel::region_count(), 0);
    assert!(!heap.parallel_array_active());
}

#[test]
fn swept_lambda_captures_leave_no_heap_budget_drift() {
    let mut heap = Heap::default();
    let site = Arc::new(LambdaSite::new(
        Arc::from("Owner"),
        Arc::from("apply"),
        Arc::from("(IJ)V"),
    ));
    let lambda = heap.alloc_lambda(Lambda {
        kind: 6,
        site,
        captured: vec![Value::Int(7), Value::Long(11), Value::Null],
    });
    let clone = heap.shallow_clone(lambda);
    let per_lambda = 48 + 3 * SLOT_BYTES;
    assert_eq!(heap.approx_bytes(), 2 * per_lambda);

    let freed = heap.gc(&[]);
    assert_eq!(freed, 2 * per_lambda);
    assert_eq!(heap.approx_bytes(), 0);
    assert!(!heap.is_ref_alive(lambda));
    assert!(!heap.is_ref_alive(clone));
}

/// The λ-IMPL KEY invariant: closures born at ONE invokedynamic site share the
/// interned `Arc<LambdaSite>`, so they answer the SAME implementation key even
/// though they are distinct objects with distinct captures — which is exactly what
/// lets a capturing call site go monomorphic. A different site keys differently
/// even when its (owner, name, descriptor) is EQUAL, so a content collision can
/// never merge two sites' entries. The key is also tagged out of the range of every
/// other receiver guard sharing the direct-call cache (class-name pointers, arena
/// class-id+1, and `REF_TAG|handle` identity cells are all far below 2^63).
#[test]
fn lambda_impl_key_is_per_site_and_capture_independent() {
    let mut heap = Heap::default();
    let site = Arc::new(LambdaSite::new(
        Arc::from("Owner"),
        Arc::from("lambda$join$0"),
        Arc::from("(Ljava/lang/Object;II)V"),
    ));
    // Two INSTANTIATIONS of one site: different objects, different captures.
    let a = heap.alloc_lambda(Lambda { kind: 6, site: Arc::clone(&site), captured: vec![Value::Int(1)] });
    let b = heap.alloc_lambda(Lambda { kind: 6, site: Arc::clone(&site), captured: vec![Value::Int(2)] });
    assert_ne!(a, b, "distinct closures");
    assert_eq!(heap.lambda_site_key(a), heap.lambda_site_key(b), "same impl ⇒ same key");
    // The RECEIVER key (what the incumbent cache guards on) does NOT repeat.
    assert_ne!(a, b);
    // A DIFFERENT site with identical content is a different key.
    let twin = Arc::new(LambdaSite::new(
        Arc::from("Owner"),
        Arc::from("lambda$join$0"),
        Arc::from("(Ljava/lang/Object;II)V"),
    ));
    let c = heap.alloc_lambda(Lambda { kind: 6, site: twin, captured: vec![Value::Int(1)] });
    assert_ne!(heap.lambda_site_key(a), heap.lambda_site_key(c));
    // Tagged out of every other guard's range; 0 for a non-lambda.
    assert!(heap.lambda_site_key(a) & (1u64 << 63) != 0);
    let plain = heap.alloc("Owner");
    assert_eq!(heap.lambda_site_key(plain), 0);
    // Captures are BORROWED, not cloned — and they are the per-receiver part.
    assert_eq!(heap.lambda_captures(a).map(<[Value]>::len), Some(1));
    assert!(matches!(heap.lambda_captures(b), Some([Value::Int(2)])));
}

#[test]
fn removed_and_cleared_map_entries_leave_no_heap_budget_drift() {
    let mut heap = Heap::default();
    let map = heap.alloc_map();
    let first = MapKey::Int(7);
    let second = MapKey::str("registry:key");
    let first_bytes = first.entry_bytes();
    let second_bytes = second.entry_bytes();

    heap.map_put(map, first.clone(), Value::Int(7), Value::Long(70));
    heap.map_put(map, second.clone(), Value::Str("registry:key".into()), Value::Null);
    assert_eq!(heap.approx_bytes(), 32 + first_bytes + second_bytes);

    assert_eq!(heap.map_remove(map, &first).and_then(|v| v.as_long()), Some(70));
    assert_eq!(heap.approx_bytes(), 32 + second_bytes);
    heap.map_clear(map);
    assert_eq!(heap.approx_bytes(), 32);

    assert_eq!(heap.gc(&[]), 32);
    assert_eq!(heap.approx_bytes(), 0);
}

#[test]
fn compacted_box_side_entries_leave_no_heap_budget_drift() {
    let mut heap = Heap::default();
    let object = heap.alloc("Holder");
    heap.put_field(object, "boxed", Value::Str("old-field".into()));
    let array = heap.alloc_array("java/lang/Object", 1);
    heap.array_set(array, 0, Value::Str("old-array".into()));

    let live_container_bytes = (48 + 13) + (32 + 8);
    assert_eq!(heap.approx_bytes(), live_container_bytes + 2 * SLOT_BYTES);
    heap.put_field(object, "boxed", Value::Null);
    heap.array_set(array, 0, Value::Null);

    assert_eq!(heap.gc(&[Value::Ref(object), Value::Array(array)]), 2 * SLOT_BYTES);
    assert_eq!(heap.approx_bytes(), live_container_bytes);
    assert_eq!(heap.gc(&[]), live_container_bytes);
    assert_eq!(heap.approx_bytes(), 0);
}
