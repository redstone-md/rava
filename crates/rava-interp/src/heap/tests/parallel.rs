//! Arrays under a parallel region: claims, epochs, holes and disjointness.

use super::*;


#[test]
fn parallel_array_region_reserve_put_commit() {
    // G3.5: pre-reserve slots, "workers" claim disjoint ones via worker_array_put (&self),
    // then commit trims the unused tail. Handles + contents are correct; exhaustion → None.
    let mut heap = Heap::default();
    let pre_handle = heap.alloc_array("int", 3); // so base != 0
    let base = heap.parallel_array_reserve(4);
    assert!(!heap.array_in_parallel_region(pre_handle));
    let element_id = heap.intern_array_element("int");
    let interner_len = heap.array_element_names.read().unwrap_or_else(|e| e.into_inner()).len();
    let h0 = heap.worker_array_put(element_id, ArrayData::Int(vec![10, 20])).unwrap();
    let h1 = heap.worker_array_put(element_id, ArrayData::Int(vec![30])).unwrap();
    let h2 = heap.worker_array_put(element_id, ArrayData::Int(vec![40, 50, 60])).unwrap();
    assert_eq!((h0, h1, h2), (base, base + 1, base + 2));
    assert!(heap.array_in_parallel_region(h0));
    assert!(heap.array_in_parallel_region(h1));
    assert_eq!(heap.array_len(h0), Some(2));
    assert_eq!(heap.array_get(h1, 0).and_then(|v| v.as_long()), Some(30));
    assert_eq!(heap.array_get(h2, 2).and_then(|v| v.as_long()), Some(60));
    assert_eq!(heap.array_element(h2).as_deref(), Some("int"));
    assert_eq!(heap.array_element_names.read().unwrap_or_else(|e| e.into_inner()).len(), interner_len, "worker path must not mutate the interner");
    // 4th claim uses the last slot; the 5th exhausts the region → None (→ abort).
    assert!(heap.worker_array_put(element_id, ArrayData::Int(vec![0])).is_some());
    assert!(heap.worker_array_put(element_id, ArrayData::Int(vec![0])).is_none());
    let exhausted = heap.parr_next.load(std::sync::atomic::Ordering::Relaxed);
    assert!(heap.worker_array_put(element_id, ArrayData::Int(vec![0])).is_none());
    assert_eq!(heap.parr_next.load(std::sync::atomic::Ordering::Relaxed), exhausted);
    let count = heap.parallel_array_commit();
    assert_eq!(count, 4, "four slots claimed");
    // Committed arrays survive + are readable; the unused tail was trimmed.
    assert_eq!(heap.array_get(h2, 1).and_then(|v| v.as_long()), Some(50));
    assert!(!heap.array_in_parallel_region(h0));
    assert!(heap.worker_array_put(element_id, ArrayData::Int(vec![0])).is_none());
}

#[test]
fn parallel_array_cursor_batches_shared_claims() {
    let mut heap = Heap::default();
    let base = heap.parallel_array_reserve(32) as usize;
    let element_id = heap.intern_array_element("int");
    for value in 0..3 {
        heap.worker_array_put(element_id, ArrayData::Int(vec![value])).unwrap();
    }
    assert_eq!(
        heap.parr_next.load(std::sync::atomic::Ordering::Relaxed),
        base + PARALLEL_ARRAY_CLAIM_CHUNK,
        "three allocations must share one atomic range claim",
    );
    assert_eq!(heap.parallel_array_commit(), 3);
}

#[test]
fn parallel_array_cursor_epoch_resets_between_regions() {
    let mut heap = Heap::default();
    let element_id = heap.intern_array_element("int");
    heap.parallel_array_reserve(16);
    heap.worker_array_put(element_id, ArrayData::Int(vec![1])).unwrap();
    heap.worker_array_put(element_id, ArrayData::Int(vec![2])).unwrap();
    assert_eq!(heap.parallel_array_commit(), 2);

    let base = heap.parallel_array_reserve(16);
    let h0 = heap.worker_array_put(element_id, ArrayData::Int(vec![3])).unwrap();
    let h1 = heap.worker_array_put(element_id, ArrayData::Int(vec![4])).unwrap();
    assert_eq!((h0, h1), (base, base + 1));
    assert_eq!(heap.parallel_array_commit(), 2);
    assert_eq!(heap.array_get(h1, 0).and_then(|v| v.as_long()), Some(4));
}

#[test]
fn parallel_array_partial_ranges_become_unique_reusable_holes() {
    let mut a = Heap::default();
    let mut b = Heap::default();
    let a_base = a.parallel_array_reserve(24);
    let b_base = b.parallel_array_reserve(8);
    let a_element_id = a.intern_array_element("int");
    let b_element_id = b.intern_array_element("int");

    let a0 = a.worker_array_put(a_element_id, ArrayData::Int(vec![10])).unwrap();
    let b0 = b.worker_array_put(b_element_id, ArrayData::Int(vec![20])).unwrap();
    // Switching heaps invalidates the TLS cursor. Returning to `a` claims a new range,
    // leaving the unpublished remainder of its first range as interior holes.
    let a8 = a.worker_array_put(a_element_id, ArrayData::Int(vec![30])).unwrap();
    assert_eq!((a0, a8, b0), (a_base, a_base + 8, b_base));
    assert_eq!(a.parallel_array_commit(), 2);
    assert_eq!(b.parallel_array_commit(), 1);

    let expected: std::collections::HashSet<u32> = (a_base + 1..a_base + 8).collect();
    let free: std::collections::HashSet<u32> = a.free_arr.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
    assert_eq!(free, expected);
    assert_eq!(free.len(), a.free_arr.lock().unwrap_or_else(|e| e.into_inner()).len(), "free list must not contain duplicates");
    a.gc(&[Value::Array(a0), Value::Array(a8)]);
    let after_gc: std::collections::HashSet<u32> = a.free_arr.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
    assert_eq!(after_gc, expected);
    assert_eq!(after_gc.len(), a.free_arr.lock().unwrap_or_else(|e| e.into_inner()).len(), "GC must not re-add reserved holes");

    let reused: std::collections::HashSet<u32> =
        (0..7).map(|_| a.alloc_array("int", 1)).collect();
    assert_eq!(reused, expected);
    assert!(a.free_arr.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert_eq!(a.array_get(a0, 0).and_then(|v| v.as_long()), Some(10));
    assert_eq!(a.array_get(a8, 0).and_then(|v| v.as_long()), Some(30));
}

#[test]
fn parallel_array_concurrent_claims_are_disjoint() {
    let mut heap = Heap::default();
    heap.parallel_array_reserve(64);
    let heap_ptr = &heap as *const Heap as usize;
    let element_id = heap.intern_array_element("int");
    let mut allocated = Vec::new();
    std::thread::scope(|scope| {
        let joins: Vec<_> = (0..4)
            .map(|worker| {
                scope.spawn(move || {
                    let heap = unsafe { &*(heap_ptr as *const Heap) };
                    (0..5)
                        .map(|item| {
                            let value = worker * 10 + item;
                            let h = heap
                                .worker_array_put(element_id, ArrayData::Int(vec![value]))
                                .unwrap();
                            (h, value as i64)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for join in joins {
            allocated.extend(join.join().unwrap());
        }
    });
    let handles: std::collections::HashSet<u32> =
        allocated.iter().map(|(h, _)| *h).collect();
    assert_eq!(handles.len(), allocated.len());
    assert_eq!(heap.parallel_array_commit(), allocated.len());
    for (h, value) in allocated {
        assert_eq!(heap.array_get(h, 0).and_then(|v| v.as_long()), Some(value));
    }
}

/// PARALLEL-WORLDGEN #2b safety gate: prove that N worker threads filling their
/// OWN disjoint pre-reserved object slots (via the raw base pointer, unsafe)
/// concurrently produce zero corruption. Repeated many times to expose any race.
/// This validates the unsafe disjoint-write mechanism IN ISOLATION before it is
/// wired into the live engine.
#[test]
fn concurrent_disjoint_object_writes_are_race_free() {
    const N: usize = 4;
    const K: usize = 2000;
    for _iter in 0..25 {
        let mut heap = Heap::default();
        let start = heap.reserve_obj_range(N * K);
        assert_eq!(start, 0);
        let base = heap.objects_slot_base() as usize; // raw ptr → usize so it's Send
        let dbase = heap.obj_descs_slot_base() as usize;
        let handles: Vec<_> = (0..N)
            .map(|w| {
                std::thread::spawn(move || {
                    let base = base as *mut ObjectSlot;
                    let dbase = dbase as *mut ObjDesc;
                    let cls: std::sync::Arc<str> = std::sync::Arc::from(format!("W{w}").as_str());
                    for j in 0..K {
                        let idx = start as usize + w * K + j;
                        // Two consistent (id, cell, kind) triples — FieldBlob keeps the
                        // three sections in lock-step (unlike the old independent SmallVecs).
                        let mut fb = FieldBlob::new();
                        fb.push(w as u32, (w * K + j) as i64, 0);
                        fb.push(j as u32, (w * K + j) as i64, 0);
                        let obj = Object {
                            class: cls.clone(),
                            fields: fb,
                            lambda: None,
                        };
                        // SAFETY: each worker writes ONLY its own disjoint slots
                        // [w*K, w*K+K); `objects`/`obj_descs` are pre-reserved so
                        // they never realloc here; main joins all workers before
                        // reading.
                        unsafe { Heap::fill_reserved(base, dbase, idx, obj); }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        for w in 0..N {
            for j in 0..K {
                let idx = (start as usize + w * K + j) as u32;
                let o = heap.get(idx).expect("object present");
                assert_eq!(o.fields.ids(), [w as u32, j as u32].as_slice(), "w{w} j{j} ids");
                assert_eq!(o.fields.cells()[0], (w * K + j) as i64, "w{w} j{j} cell");
                assert_eq!(&*o.class, format!("W{w}"), "w{w} j{j} class");
                // The concurrently-synced native descriptor must read the SAME
                // fields (the JIT/native field path goes through obj_descs).
                let d = heap.obj_descs.read().unwrap_or_else(|e| e.into_inner())[idx as usize];
                // count lives in the FieldBlob header at [cells-8] (cells-first layout).
                let count = unsafe { *(d.cells as *const u32).sub(2) };
                assert_eq!(count, 2, "w{w} j{j} desc count");
                let (id0, id1) = unsafe { (*d.ids, *d.ids.add(1)) };
                let cell0 = unsafe { *d.cells };
                assert_eq!((id0, id1), (w as u32, j as u32), "w{w} j{j} desc ids");
                assert_eq!(cell0, (w * K + j) as i64, "w{w} j{j} desc cell");
            }
        }
    }
}

/// PARALLEL-WORLDGEN increment #3 safety gate: workers READ shared immutable
/// "registry" objects (allocated before the region) through the same raw object
/// base, CONCURRENTLY with writing their own disjoint reserved slots. Proves the
/// read-of-immutable + disjoint-write access pattern the live offload relies on
/// (gen reads Block/BlockState/biome registries while building a detached chunk)
/// races cleanly. The registry slots are never written during the region, so the
/// concurrent reads have no writer → safe; this test asserts that mechanically.
#[test]
fn concurrent_immutable_reads_during_disjoint_writes_are_consistent() {
    const REG: usize = 256; // immutable "registry" objects at indices [0, REG)
    const N: usize = 4;
    const K: usize = 1500;
    for _iter in 0..20 {
        let mut heap = Heap::default();
        // Build the immutable registry: object r has a single field id=0 = r*7.
        for r in 0..REG {
            let h = heap.alloc("Registry");
            heap.put_field(h, "v", Value::Int((r * 7) as i32));
            assert_eq!(h, r as u32);
        }
        let start = heap.reserve_obj_range(N * K);
        assert_eq!(start as usize, REG);
        let base = heap.objects_slot_base() as usize;
        let dbase = heap.obj_descs_slot_base() as usize;
        // Each worker returns the checksum of every registry value it read, so a
        // torn/garbage read would corrupt the sum and fail the assert.
        let handles: Vec<_> = (0..N)
            .map(|w| {
                std::thread::spawn(move || {
                    let base = base as *mut ObjectSlot;
                    let dbase = dbase as *mut ObjDesc;
                    let cls: std::sync::Arc<str> = std::sync::Arc::from(format!("W{w}").as_str());
                    let mut read_sum: i64 = 0;
                    for j in 0..K {
                        // READ a shared immutable registry object (no writer touches
                        // [0,REG) during the region) via the same raw base.
                        let r = (w * K + j) % REG;
                        // SAFETY: index < REG is an immutable slot, never written in
                        // this region; concurrent reads of immutable data are safe.
                        let reg = unsafe { (*base.add(r)).get().expect("registry slot is live") };
                        read_sum += reg.fields.cells()[0];
                        // WRITE this worker's own disjoint reserved slot.
                        let idx = start as usize + w * K + j;
                        let mut fb = FieldBlob::new();
                        fb.push(0, (w * K + j) as i64, 0);
                        let obj = Object {
                            class: cls.clone(),
                            fields: fb,
                            lambda: None,
                        };
                        unsafe { Heap::fill_reserved(base, dbase, idx, obj); }
                    }
                    read_sum
                })
            })
            .collect();
        let mut total = 0i64;
        for h in handles {
            total += h.join().unwrap();
        }
        // Expected: each worker reads K values, value at r = r*7, r cycling
        // (w*K+j)%REG. Recompute the same sum single-threaded.
        let mut expect = 0i64;
        for w in 0..N {
            for j in 0..K {
                expect += (((w * K + j) % REG) * 7) as i64;
            }
        }
        assert_eq!(total, expect, "torn/garbage registry read under concurrency");
        // Registry must be UNCHANGED by the region (no accidental write).
        for r in 0..REG {
            assert_eq!(heap.get_field(r as u32, "v").as_int(), Some((r * 7) as i32), "registry r{r} mutated");
        }
        // And each worker's disjoint writes landed correctly.
        for w in 0..N {
            for j in 0..K {
                let idx = (start as usize + w * K + j) as u32;
                assert_eq!(heap.get(idx).unwrap().fields.cells()[0], (w * K + j) as i64, "w{w} j{j} write");
            }
        }
    }
}
