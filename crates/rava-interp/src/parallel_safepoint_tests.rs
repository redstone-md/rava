//! parallel_safepoint_tests.

use super::parallel;
use smallvec::smallvec;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn hot_stats_flush_only_at_outermost_region_exit() {
    let _serial = parallel::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !parallel::hot_cow_buffering_for_test() {
        return;
    }
    assert_eq!(parallel::hot_cow_pending_for_test(), [0, 0, 0]);

    parallel::enter_region();
    parallel::count_arena_alloc();
    parallel::shadow_put_field(1, 7, crate::Value::Int(42));
    assert!(matches!(
        parallel::shadow_get_field(1, 7),
        Some(crate::Value::Int(42))
    ));
    assert_eq!(parallel::hot_cow_pending_for_test(), [1, 1, 1]);
    assert_eq!(parallel::hot_arena_allocs_pending_for_test(), 1);

    parallel::enter_region();
    parallel::count_arena_alloc();
    assert!(matches!(
        parallel::shadow_get_field(1, 7),
        Some(crate::Value::Int(42))
    ));
    parallel::leave_region();
    assert_eq!(
        parallel::hot_cow_pending_for_test(),
        [1, 1, 2],
        "nested exit must not publish the outer region's counters"
    );
    assert_eq!(
        parallel::hot_arena_allocs_pending_for_test(),
        2,
        "nested exit must not publish arena allocation counters"
    );

    parallel::shadow_clear();
    parallel::leave_region();
    assert_eq!(
        parallel::hot_cow_pending_for_test(),
        [0, 0, 0],
        "outer exit must publish and reset worker-local counters"
    );
    assert_eq!(parallel::hot_arena_allocs_pending_for_test(), 0);
}

/// Stress the safepoint: N worker threads run; a stopper freezes them ALL,
/// verifies none make progress during the stop, then resumes. The properties
/// that matter (all parked after stop_the_world; zero progress while stopped)
/// are guaranteed by the primitive, not by timing, so this is not flaky.
#[test]
fn stop_the_world_freezes_all_region_threads() {
    let _serial = parallel::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const N: usize = 4;
    let quit = Arc::new(AtomicBool::new(false));
    let progress: Arc<Vec<AtomicUsize>> =
        Arc::new((0..N).map(|_| AtomicUsize::new(0)).collect());
    let mut handles = Vec::new();
    for i in 0..N {
        let quit = quit.clone();
        let progress = progress.clone();
        handles.push(std::thread::spawn(move || {
            parallel::enter_region();
            while !quit.load(Ordering::Relaxed) {
                parallel::safepoint();
                progress[i].fetch_add(1, Ordering::Relaxed);
            }
            parallel::leave_region();
        }));
    }
    std::thread::sleep(Duration::from_millis(30)); // let workers spin up
    for _round in 0..5 {
        // Blocks until all N region threads have parked.
        parallel::stop_the_world(N);
        assert_eq!(parallel::parked_count(), N, "all region threads parked");
        let snap: Vec<usize> = (0..N).map(|i| progress[i].load(Ordering::Relaxed)).collect();
        std::thread::sleep(Duration::from_millis(15));
        for i in 0..N {
            assert_eq!(
                progress[i].load(Ordering::Relaxed),
                snap[i],
                "thread {i} must not advance while the world is stopped"
            );
        }
        parallel::resume();
        std::thread::sleep(Duration::from_millis(15));
        for i in 0..N {
            assert!(
                progress[i].load(Ordering::Relaxed) > snap[i],
                "thread {i} must resume after the world resumes"
            );
        }
    }
    quit.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(parallel::region_count(), 0, "all regions left");
}

/// PARALLEL-WORLDGEN #4 integration gate: the safepoint (#1) composed with the
/// concurrent allocator (#2b). Worker threads continuously materialize objects
/// into their OWN reserved slots via `Heap::fill_reserved` while polling
/// `safepoint()` each iteration — the live op-loop shape. A "GC driver" stops the
/// world, and while stopped it READS the heap (as a real root-scan would) and
/// asserts NO worker advances + every reserved slot it inspects is a well-formed
/// object (never a torn half-write), then resumes. Proves a stop-the-world can
/// safely observe the heap mid-allocation-region before live offload wires it in.
#[test]
fn safepoint_lets_gc_observe_heap_mid_allocation() {
    let _serial = parallel::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use crate::heap::{Heap, Object, ObjectSlot};
    const N: usize = 4;
    let mut heap = Heap::default();
    let start = heap.reserve_obj_range(N); // one slot per worker
    let base = heap.objects_slot_base() as usize;
    let dbase = heap.obj_descs_slot_base() as usize;
    let obase = base; // base for the GC driver to read through (post-reserve, stable)
    let quit = Arc::new(AtomicBool::new(false));
    let progress: Arc<Vec<AtomicUsize>> =
        Arc::new((0..N).map(|_| AtomicUsize::new(0)).collect());
    let mut handles = Vec::new();
    for w in 0..N {
        let quit = quit.clone();
        let progress = progress.clone();
        handles.push(std::thread::spawn(move || {
            let base = base as *mut ObjectSlot;
            let dbase = dbase as *mut crate::heap::ObjDesc;
            let cls: Arc<str> = Arc::from(format!("W{w}").as_str());
            let mut n: i64 = 0;
            parallel::enter_region();
            while !quit.load(Ordering::Relaxed) {
                parallel::safepoint();
                n += 1;
                let mut fb = crate::heap::FieldBlob::new();
                fb.push(0, n, 0);
                let obj = Object {
                    class: cls.clone(),
                    fields: fb,
                    lambda: None,
                };
                // SAFETY: worker w owns ONLY slot start+w; disjoint; pre-reserved
                // so no realloc; the GC driver only reads while we are all parked.
                unsafe { Heap::fill_reserved(base, dbase, (start as usize) + w, obj); }
                progress[w].fetch_add(1, Ordering::Relaxed);
            }
            parallel::leave_region();
        }));
    }
    std::thread::sleep(Duration::from_millis(30));
    for _round in 0..5 {
        parallel::stop_the_world(N);
        assert_eq!(parallel::parked_count(), N, "all alloc workers parked");
        let snap: Vec<usize> = (0..N).map(|i| progress[i].load(Ordering::Relaxed)).collect();
        // The GC driver reads each reserved slot through the raw base — it must
        // see a coherent object (class "W{w}", one int field), never a tear.
        for w in 0..N {
            let o: &Object = unsafe {
                (*(obase as *const ObjectSlot).add((start as usize) + w))
                    .get()
                    .expect("reserved slot is initialized")
            };
            assert_eq!(&*o.class, format!("W{w}"), "slot {w} class coherent under stop");
            assert_eq!(o.fields.len(), 1, "slot {w} fields coherent under stop");
            assert!(o.fields.cells()[0] >= 1, "slot {w} value materialized");
        }
        std::thread::sleep(Duration::from_millis(15));
        for i in 0..N {
            assert_eq!(progress[i].load(Ordering::Relaxed), snap[i], "worker {i} froze");
        }
        parallel::resume();
        std::thread::sleep(Duration::from_millis(15));
        for i in 0..N {
            assert!(progress[i].load(Ordering::Relaxed) > snap[i], "worker {i} resumed");
        }
    }
    quit.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(parallel::region_count(), 0, "all regions left");
}

/// PARALLEL-WORLDGEN #21 (live offload, piece 2): N gil-free workers ALLOCATE
/// through their private arenas (`Heap::alloc_in_arena`) concurrently, while a
/// GC driver stops the world and inspects the heap mid-allocation. Proves the
/// arena cursor + routing + the disjoint raw-slot path compose race-free under a
/// real stop-the-world — the allocation half of the GIL-free worker engine.
#[test]
fn gilfree_workers_allocate_through_arenas_race_free() {
    let _serial = parallel::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use crate::heap::{Heap, Object};
    const N: usize = 4;
    const K: usize = 1500;
    let mut heap = Heap::default();
    let start = heap.reserve_obj_range(N * K); // one disjoint K-block per worker
    let hptr = &heap as *const Heap as usize; // Send: workers share &Heap, write disjoint slots
    let quit = Arc::new(AtomicBool::new(false));
    let progress: Arc<Vec<AtomicUsize>> =
        Arc::new((0..N).map(|_| AtomicUsize::new(0)).collect());
    let mut handles = Vec::new();
    for w in 0..N {
        let quit = quit.clone();
        let progress = progress.clone();
        handles.push(std::thread::spawn(move || {
            let heap: &Heap = unsafe { &*(hptr as *const Heap) };
            parallel::set_gilfree(true);
            parallel::arena_setup(start + (w * K) as u32, K as u32);
            parallel::enter_region();
            let cls: Arc<str> = Arc::from(format!("W{w}").as_str());
            let mut j: i64 = 0;
            while !quit.load(Ordering::Relaxed) {
                parallel::safepoint();
                let mut fb = crate::heap::FieldBlob::new();
                fb.push(0, j, 0);
                let obj = Object {
                    class: cls.clone(),
                    fields: fb,
                    lambda: None,
                };
                // SAFETY: gil-free worker, active region, arena from reserve_obj_range.
                if unsafe { heap.alloc_in_arena(obj) }.is_some() {
                    j += 1;
                    progress[w].fetch_add(1, Ordering::Relaxed);
                } else {
                    // arena full — keep looping (still polls safepoint) until quit.
                    std::hint::spin_loop();
                }
            }
            parallel::leave_region();
            parallel::set_gilfree(false);
            parallel::arena_clear();
        }));
    }
    std::thread::sleep(Duration::from_millis(30));
    for _round in 0..5 {
        parallel::stop_the_world(N);
        assert_eq!(parallel::parked_count(), N, "all arena workers parked");
        let snap: Vec<usize> = (0..N).map(|i| progress[i].load(Ordering::Relaxed)).collect();
        // Driver reads each worker's most-recent arena object — must be coherent.
        for w in 0..N {
            let filled = snap[w].min(K);
            if filled > 0 {
                let h = start + (w * K) as u32 + (filled as u32 - 1);
                let o = heap.get(h).expect("arena object present");
                assert_eq!(&*o.class, format!("W{w}"), "w{w} class coherent under stop");
                assert_eq!(o.fields.ids(), [0u32].as_slice(), "w{w} fields coherent under stop");
            }
        }
        std::thread::sleep(Duration::from_millis(15));
        for i in 0..N {
            assert_eq!(progress[i].load(Ordering::Relaxed), snap[i], "worker {i} froze under stop");
        }
        parallel::resume();
        std::thread::sleep(Duration::from_millis(10));
    }
    quit.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(parallel::region_count(), 0, "all regions left");
    // Every produced arena object is correct + in its worker's disjoint block.
    for w in 0..N {
        let made = progress[w].load(Ordering::Relaxed).min(K);
        for j in 0..made {
            let h = start + (w * K) as u32 + j as u32;
            let o = heap.get(h).expect("present");
            assert_eq!(&*o.class, format!("W{w}"), "final w{w} j{j} class");
            assert_eq!(o.fields.cells(), [j as i64].as_slice(), "final w{w} j{j} value");
        }
    }
}
