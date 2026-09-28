//! Object layout: the field blob, the nullable slot, and the box classifier.

use super::*;

/// FieldBlob layout / grow / clear / clone / serde round-trip + the empty sentinel.
#[test]
fn field_blob_roundtrip() {
    // Empty = zero alloc, points at the shared sentinel.
    let mut b = FieldBlob::new();
    assert_eq!(b.len(), 0);
    assert!(b.is_empty());
    assert_eq!(b.cap(), 0);
    b.clear(); // must not write through the sentinel
    assert_eq!(b.len(), 0);

    // Push across several reallocs; verify all three sections stay coherent + 8-aligned cells.
    for i in 0..17u32 {
        b.push(i * 10, (i as i64) << 40 | 0xABC, (i % 8) as u8);
        assert_eq!(b.len() as u32, i + 1);
        // CELLS-FIRST: cells sits at a fixed offset 8 → always 8-aligned regardless of cap.
        assert_eq!(b.cells_ptr() as usize % 8, 0, "cells section must be 8-aligned");
        // ⚠**EXACT-CAP WAS RETIRED TWICE AND THIS ASSERTION OUTLIVED BOTH.** It read
        // `cap == i + 1`, which was true only while `push` called `reserve(count + 1)` and
        // `reserve` allocated precisely that. `a55c48fa` made `push` grow GEOMETRICALLY (the
        // one-slot-at-a-time regrow was O(N²) bytes moved) and `357464eb` then widened every
        // request to the slack the allocator was already reserving — so the very first push
        // now lands on cap 3, and this test has been failing since. What the blob actually
        // promises, and all it ever needed to promise, is that capacity COVERS the count and
        // that the sections stay coherent; the loop below re-reads every id/cell/kind, which
        // is the property a wrong cap would break. Bounded above so a growth policy that
        // started over-reserving without limit is still caught.
        assert!(
            b.cap() >= (i + 1) as usize && b.cap() <= 2 * (i + 1) as usize + 2,
            "cap {} must cover count {} without runaway slack",
            b.cap(),
            i + 1
        );
    }
    assert_eq!(b.len(), 17);
    for i in 0..17usize {
        assert_eq!(b.ids()[i], i as u32 * 10);
        assert_eq!(b.cells()[i], (i as i64) << 40 | 0xABC);
        assert_eq!(b.kinds()[i], (i % 8) as u8);
    }

    // In-place mutation via cells_mut + the GC raw pointer path.
    b.cells_mut()[3] = -1;
    assert_eq!(b.cells()[3], -1);
    unsafe { *b.cell_mut_ptr(5) = 999 };
    assert_eq!(b.cells()[5], 999);

    // Deep clone is independent.
    let c = b.clone();
    assert_eq!(c.cells()[0], 0xABC); // cell 0 == (0<<40)|0xABC
    b.cells_mut()[0] = 7777;
    assert_eq!(c.cells()[0], 0xABC); // clone untouched by the mutation of b
    assert_eq!(b.cells()[0], 7777);
    assert_eq!(c.len(), 17);

    // serde round-trip.
    let bytes = bincode::serialize(&b).unwrap();
    let d: FieldBlob = bincode::deserialize(&bytes).unwrap();
    assert_eq!(d.len(), 17);
    assert_eq!(d.ids(), b.ids());
    assert_eq!(d.cells(), b.cells());
    assert_eq!(d.kinds(), b.kinds());

    // clear keeps capacity, extend_from refills.
    let cap_before = b.cap();
    b.clear();
    assert_eq!(b.len(), 0);
    assert_eq!(b.cap(), cap_before);
    b.extend_from(&c);
    assert_eq!(b.cells(), c.cells());

    // Empty clone stays a zero-alloc sentinel.
    let e = FieldBlob::new().clone();
    assert!(e.is_empty());
}

/// Load-insensitive RAM probe (Ось 3 / task #4): report the per-Object struct cost so
/// the diet target is quantified. ~1.5M live handle objects at MC boot, so each byte
/// here is ~1.5MB of RSS. Prints (run with --nocapture) the total + each component.
#[test]
fn object_struct_size_report() {
    let obj = std::mem::size_of::<Object>();
    let fields = std::mem::size_of::<FieldBlob>();
    let class = std::mem::size_of::<Arc<str>>();
    let lambda = std::mem::size_of::<Option<Box<Lambda>>>();
    eprintln!(
        "[OBJSIZE] Object={obj}B = class(Arc<str>) {class} + fields(FieldBlob) {fields} + lambda {lambda} (+align). ~1.5M objs => ~{}MB",
        obj * 1_500_000 / 1_048_576
    );
    // FieldBlob collapsed the three SmallVecs into one 8B ptr: Object must be ≤40B now.
    assert!(obj <= 40, "Object grew to {obj}B (>40B budget)");
}

/// Regression: box-cell classification during ref-array compaction must NOT
/// misread a raw arena pointer (next-gen-gc `Value::Obj`) as a box index, which
/// indexed `array_box` out of bounds (`heap.rs:1797` panic at registry init).
#[test]
fn box_cell_classifier_excludes_arena_pointers_and_handles() {
    // Real box cells (idx | ABOX_TAG) → true.
    for idx in [0i64, 1, 25464, 0xFFFF_FFFF] {
        assert!(is_box_cell(idx | ABOX_TAG), "box idx {idx}");
    }
    // REF / ARRAY handle cells → false.
    assert!(!is_box_cell(7 | REF_TAG), "REF handle");
    assert!(!is_box_cell(7 | ARRAY_TAG), "ARRAY handle");
    // Null cell → false.
    assert!(!is_box_cell(0), "null");
    // Raw arena pointers (8-aligned, < 2^40, high word < 256) → false. These
    // are the values the old exclusion test wrongly treated as boxes.
    for addr in [0x200000868i64, 0x200058300, 0x6378, 0xFF_FFFF_FFF8] {
        assert!(!is_box_cell(addr), "arena ptr {addr:#x}");
    }
}

#[test]
fn nullable_object_slot_preserves_raw_pointer_layout() {
    assert_eq!(
        std::mem::size_of::<ObjectSlot>(),
        std::mem::size_of::<Box<Object>>()
    );
    assert_eq!(
        std::mem::align_of::<ObjectSlot>(),
        std::mem::align_of::<Box<Object>>()
    );

    let live = ObjectSlot::live(Object {
        class: Arc::from("LayoutProbe"),
        fields: FieldBlob::new(),
        lambda: None,
    });
    let object_ptr = live.get().unwrap() as *const Object;
    let slot_word = unsafe { *std::ptr::from_ref(&live).cast::<*const Object>() };
    assert_eq!(slot_word, object_ptr, "JIT slot word must be the boxed object pointer");

    let dead = ObjectSlot(None);
    let dead_word = unsafe { *std::ptr::from_ref(&dead).cast::<*const Object>() };
    assert!(dead_word.is_null(), "nullable tombstone must publish a null JIT slot word");
}

#[test]
fn nullable_object_slot_retires_once_and_reuses_handle() {
    let mut heap = Heap::default();
    let rooted = heap.alloc("Rooted");
    let dead = heap.alloc("Transient");
    assert_ne!(rooted, dead);

    let freed = heap.retire_object_slot(dead as usize, true);
    assert!(freed >= 48);
    assert!(heap.objects.read().unwrap_or_else(|e| e.into_inner())[dead as usize].get().is_none());
    assert!(heap.get(dead).is_none(), "dead handle lookup must not panic or expose a tombstone");
    assert_eq!(heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&h| h == dead).count(), 1);
    assert_eq!(heap.retire_object_slot(dead as usize, true), 0, "retirement is idempotent");
    assert_eq!(heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&h| h == dead).count(), 1);

    let reused = heap.alloc("Reused");
    assert_eq!(reused, dead);
    assert_eq!(heap.class_of(reused), Some("Reused"));
    assert!(heap.obj_alive.lock().unwrap_or_else(|e| e.into_inner())[reused as usize]);
    assert!(heap.objects.read().unwrap_or_else(|e| e.into_inner())[reused as usize].get().is_some());
}

#[test]
fn nullable_object_slot_gc_releases_only_unreachable_objects() {
    let mut heap = Heap::default();
    let rooted = heap.alloc("Rooted");
    let dead = heap.alloc("Transient");

    let freed = heap.gc_with_object_slot_policy(&[Value::Ref(rooted)], true);
    assert!(freed >= 48);
    assert_eq!(heap.class_of(rooted), Some("Rooted"));
    assert!(heap.objects.read().unwrap_or_else(|e| e.into_inner())[rooted as usize].get().is_some());
    assert!(heap.objects.read().unwrap_or_else(|e| e.into_inner())[dead as usize].get().is_none());
    assert!(heap.get(dead).is_none());
    assert_eq!(heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&h| h == dead).count(), 1);

    assert_eq!(heap.gc_with_object_slot_policy(&[Value::Ref(rooted)], true), 0);
    assert_eq!(heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&h| h == dead).count(), 1);
}

/// getstatic value-cell infra (the Rust side of the native getstatic cache):
/// the table base is published, slots are handed out uniquely + exhaust + roll
/// back, a written cell reads back `[gen, value]` at the layout compiled code
/// expects, and the gen mirror tracks `set_static_gen_mirror`.
#[test]
fn static_cell_infra_reserve_write_and_mirror() {
    let mut h = Heap::default();
    h.ensure_static_cells();
    assert_ne!(h.jit_ctx.static_cells_base, 0, "base published");
    assert_eq!(h.jit_ctx.static_cells_base, h.static_cells.as_ptr() as usize);

    // Unique, monotonic slot blocks.
    let a = h.reserve_static_slots(3).expect("room");
    let b = h.reserve_static_slots(2).expect("room");
    assert_eq!(a, 0);
    assert_eq!(b, 3, "second block starts after the first");

    // Rollback returns the top block (single-threaded compile invariant).
    h.free_static_slots(2);
    let c = h.reserve_static_slots(2).expect("room");
    assert_eq!(c, 3, "freed block is handed out again");

    // A written cell reads back as [gen, value] at stride 16 (2×i64).
    h.write_static_cell(c, 7, 12345);
    assert_eq!(h.static_cells[c as usize * 2], 7, "gen half");
    assert_eq!(h.static_cells[c as usize * 2 + 1], 12345, "value half");
    // The OTHER reserved cell keeps its sentinel gen (-1 ⇒ never matches).
    assert_eq!(h.static_cells[a as usize * 2], -1, "untouched cell stays sentinel");

    // Exhaustion is graceful (None, not panic), and the counter doesn't move.
    let before = h.next_static_slot;
    assert!(h.reserve_static_slots(JIT_STATIC_SLOTS as u32).is_none(), "over-cap → None");
    assert_eq!(h.next_static_slot, before, "failed reserve doesn't advance");

    // Gen mirror tracks the setter.
    h.set_static_gen_mirror(99);
    assert_eq!(h.jit_ctx.static_gen, 99);
}
