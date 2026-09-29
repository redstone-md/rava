//! arena_root_tests.

use crate::value::Value;

/// DANGLING SHIELD (Fix 1): an arena object held ONLY by a native HashMap must
/// survive repeated minor collections — the moving GC roots it via the map,
/// forwards it, updates the stored pointer, and never corrupts its header/fields
/// with recycled nursery garbage. Without the fix the object is rooted by NEITHER
/// GC → reclaimed → dangling → garbage coordinates.
#[test]
fn arena_object_in_map_survives_minor_gc_address_updated_data_intact() {
    // A 3-int-field value class (X/Y/Z) — ref_mask 0, like BlockPosition.
    let name: std::sync::Arc<str> = std::sync::Arc::from("LBlockPosRootTest");
    let layout: std::sync::Arc<[(u32, u8)]> = vec![(901u32, 0u8), (902, 0), (903, 0)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    let p = crate::nextgen::tlab_try_alloc(cid, 3).expect("arena alloc");
    unsafe {
        crate::nextgen::set_cell(p.as_ptr(), 0, 1111);
        crate::nextgen::set_cell(p.as_ptr(), 1, 2222);
        crate::nextgen::set_cell(p.as_ptr(), 2, 3333);
    }
    assert!(crate::nextgen::tlab_in_nursery(p), "freshly allocated → in nursery");
    let orig_addr = p.as_ptr() as usize;

    // Store ONLY in a native HashMap (no frame/static/cell root). The collection
    // root pass is the only thing that can keep it alive + forwarded.
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let mapid = vm.heap.alloc_map();
    vm.heap.map_put(mapid, crate::heap::MapKey::Int(7), Value::Int(7), Value::Obj(p));

    // Hammer minor GC with fresh unrooted garbage between cycles.
    for _ in 0..6 {
        for _ in 0..16 {
            let _ = crate::nextgen::tlab_try_alloc(cid, 3); // dead garbage
        }
        vm.arena_collect_jit();
    }

    // Read the object back THROUGH the map (the forwarded pointer).
    let entries = vm.heap.map_entries(mapid);
    let np = match &entries[0].1 {
        Value::Obj(ap) => *ap,
        other => panic!("map value corrupted: {other:?}"),
    };
    assert!(!crate::nextgen::tlab_in_nursery(np), "evacuated out of the nursery");
    assert_ne!(np.as_ptr() as usize, orig_addr, "stored pointer updated to the new address");
    assert_eq!(unsafe { crate::nextgen::obj_class(np.as_ptr()) }, cid, "class id intact");
    unsafe {
        assert_eq!(crate::nextgen::cell(np.as_ptr(), 0), 1111, "X intact (not nursery garbage)");
        assert_eq!(crate::nextgen::cell(np.as_ptr(), 1), 2222, "Y intact");
        assert_eq!(crate::nextgen::cell(np.as_ptr(), 2), 3333, "Z intact");
    }
}

/// >32-FIELD TRACING: a class with 40 fields whose reference cell is at slot 35
/// (beyond the old u32 mask cap) must have that child traced + forwarded by the
/// moving collector — proving the dynamic word bitmask covers any field count.
#[test]
fn arena_object_over_32_fields_traces_ref_at_slot_35() {
    // 40 fields, all int (kind 0) except slot 35 which is a reference (kind 6).
    let mut layout_v: Vec<(u32, u8)> = (0..40).map(|i| (1000u32 + i as u32, 0u8)).collect();
    layout_v[35] = (1035, 6); // reference field at slot 35
    let parent_layout: std::sync::Arc<[(u32, u8)]> = layout_v.into();
    let pname: std::sync::Arc<str> = std::sync::Arc::from("LBigFieldsTest");
    let pcid = crate::nextgen::register_class(&pname, &parent_layout);

    let cname: std::sync::Arc<str> = std::sync::Arc::from("LBigChildTest");
    let clayout: std::sync::Arc<[(u32, u8)]> = vec![(2000u32, 0u8)].into();
    let ccid = crate::nextgen::register_class(&cname, &clayout);

    let child = crate::nextgen::tlab_try_alloc(ccid, 1).expect("child");
    unsafe { crate::nextgen::set_cell(child.as_ptr(), 0, 9999) };

    let parent = crate::nextgen::tlab_try_alloc(pcid, 40).expect("parent");
    unsafe {
        crate::nextgen::set_cell(parent.as_ptr(), 39, 7777); // an int cell past slot 32
        crate::nextgen::set_cell(parent.as_ptr(), 35, child.as_ptr() as i64); // the ref cell
    }

    // Root ONLY the parent (child reachable only via parent's slot-35 ref).
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let mapid = vm.heap.alloc_map();
    vm.heap.map_put(mapid, crate::heap::MapKey::Int(0), Value::Int(0), Value::Obj(parent));

    for _ in 0..4 {
        for _ in 0..8 {
            let _ = crate::nextgen::tlab_try_alloc(ccid, 1); // garbage
        }
        vm.arena_collect_jit();
    }

    let np = match &vm.heap.map_entries(mapid)[0].1 {
        Value::Obj(ap) => *ap,
        o => panic!("parent corrupted: {o:?}"),
    };
    assert!(!crate::nextgen::tlab_in_nursery(np), "parent evacuated");
    assert_eq!(unsafe { crate::nextgen::cell(np.as_ptr(), 39) }, 7777, "int cell at slot 39 intact");
    // The slot-35 ref must have been forwarded to the child's new address.
    let ncp_raw = unsafe { crate::nextgen::cell(np.as_ptr(), 35) } as u64;
    let ncp = crate::arena::ArenaPtr::new(ncp_raw as *mut u8).expect("child ptr");
    assert!(crate::nextgen::arena_contains(ncp.as_ptr()), "child forwarded into an arena region");
    assert_eq!(unsafe { crate::nextgen::cell(ncp.as_ptr(), 0) }, 9999, "child data traced + intact via slot 35");
}

/// MAJOR GC (Fix 2): minor never reclaims tenured, so dead promoted objects leak.
/// A major collection must reclaim dead tenured (compacting `old.top`) while
/// keeping live objects — including one held only by a map — relocated + intact.
#[test]
fn major_gc_reclaims_dead_tenured_keeps_live_intact() {
    // This is cached process configuration in production; the test override is thread-local.
    let _major_threshold = crate::nextgen::ArenaMajorKbTestOverride::set(0);

    let name: std::sync::Arc<str> = std::sync::Arc::from("LMajorTest");
    let layout: std::sync::Arc<[(u32, u8)]> = vec![(801u32, 0u8), (802, 0), (803, 0)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let mapid = vm.heap.alloc_map();

    // A = live; B0..B49 = soon-dead. All rooted via the map → promoted on a minor.
    let a = crate::nextgen::tlab_try_alloc(cid, 3).expect("alloc A");
    unsafe {
        crate::nextgen::set_cell(a.as_ptr(), 0, 4040);
        crate::nextgen::set_cell(a.as_ptr(), 1, 5050);
        crate::nextgen::set_cell(a.as_ptr(), 2, 6060);
    }
    vm.heap.map_put(mapid, crate::heap::MapKey::Int(0), Value::Int(0), Value::Obj(a));
    for i in 1..=50i64 {
        let b = crate::nextgen::tlab_try_alloc(cid, 3).expect("alloc B");
        vm.heap.map_put(mapid, crate::heap::MapKey::Int(i), Value::Int(i as i32), Value::Obj(b));
    }

    // MINOR: promote all 51 into tenured.
    vm.arena_collect_jit();
    let old_full = crate::nextgen::arena_old_bytes();
    assert!(old_full > 0, "objects promoted to tenured");

    // Kill the 50 B's (Java drops them) → dead tenured garbage.
    for i in 1..=50i64 {
        vm.heap.map_remove(mapid, &crate::heap::MapKey::Int(i));
    }

    // MAJOR: force via the tiny threshold, then compact.
    vm.arena_collect_jit();

    let old_after = crate::nextgen::arena_old_bytes();
    assert!(old_after < old_full, "major reclaimed dead tenured: {old_after} < {old_full}");
    // Only A (one 3-field object) should remain live.
    assert!(old_after <= crate::nextgen::obj_size(3) * 2, "tenured compacted to ~1 object, got {old_after}B");

    // A survives, relocated, data intact.
    let entries = vm.heap.map_entries(mapid);
    assert_eq!(entries.len(), 1, "only A remains in the map");
    let na = match &entries[0].1 {
        Value::Obj(ap) => *ap,
        other => panic!("A corrupted: {other:?}"),
    };
    assert_eq!(unsafe { crate::nextgen::obj_class(na.as_ptr()) }, cid, "A class id intact");
    unsafe {
        assert_eq!(crate::nextgen::cell(na.as_ptr(), 0), 4040, "A.X intact after major");
        assert_eq!(crate::nextgen::cell(na.as_ptr(), 1), 5050, "A.Y intact");
        assert_eq!(crate::nextgen::cell(na.as_ptr(), 2), 6060, "A.Z intact");
    }
}
