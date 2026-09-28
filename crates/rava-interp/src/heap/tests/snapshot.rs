//! Snapshot/restore round trips, and guest map iteration order.

use super::*;

#[test]
fn indexed_maps_keep_java_iteration_order() {
    let mut heap = Heap::default();
    let id = heap.alloc_map();

    assert!(heap.map_put(id, MapKey::str("s:a"), Value::Str("a".into()), Value::Int(1)).is_none());
    assert!(heap.map_put(id, MapKey::str("s:b"), Value::Str("b".into()), Value::Int(2)).is_none());
    assert_eq!(heap.map_size(id), 2);
    assert_eq!(heap.map_get(id, &MapKey::str("s:a")).and_then(|v| v.as_int()), Some(1));

    assert_eq!(
        heap.map_put(id, MapKey::str("s:a"), Value::Str("a".into()), Value::Int(3))
            .and_then(|v| v.as_int()),
        Some(1)
    );
    assert_eq!(heap.map_get(id, &MapKey::str("s:a")).and_then(|v| v.as_int()), Some(3));
    let entries = heap.map_entries(id);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.display(), "a");
    assert_eq!(entries[0].1.as_int(), Some(3));
    assert_eq!(entries[1].0.display(), "b");
    assert_eq!(entries[1].1.as_int(), Some(2));

    assert_eq!(heap.map_remove(id, &MapKey::str("s:a")).and_then(|v| v.as_int()), Some(3));
    assert!(!heap.map_contains(id, &MapKey::str("s:a")));
    assert_eq!(heap.map_get(id, &MapKey::str("s:b")).and_then(|v| v.as_int()), Some(2));

    heap.map_clear(id);
    assert_eq!(heap.map_size(id), 0);
    assert!(heap.map_entries(id).is_empty());
}

#[test]
fn int_map_keys_are_alloc_free_and_disjoint_from_strings() {
    // The Collections fast path: `Value::Int` keys index via MapKey::Int (no
    // String alloc). They must NOT collide with the old `"i:n"` string form,
    // nor with each other across put/get/remove.
    let mut heap = Heap::default();
    let id = heap.alloc_map();
    assert!(heap.map_put(id, MapKey::Int(5), Value::Int(5), Value::Int(50)).is_none());
    assert!(heap.map_put(id, MapKey::Int(7), Value::Int(7), Value::Int(70)).is_none());
    // hit by the same Int key; replace returns the old value
    assert_eq!(heap.map_get(id, &MapKey::Int(5)).and_then(|v| v.as_int()), Some(50));
    assert_eq!(
        heap.map_put(id, MapKey::Int(5), Value::Int(5), Value::Int(500)).and_then(|v| v.as_int()),
        Some(50)
    );
    assert_eq!(heap.map_get(id, &MapKey::Int(5)).and_then(|v| v.as_int()), Some(500));
    // an int key and the legacy "i:5" string key are DISJOINT (no collision)
    assert!(heap.map_get(id, &MapKey::str("i:5")).is_none());
    heap.map_put(id, MapKey::str("i:5"), Value::Str("i:5".into()), Value::Int(999));
    assert_eq!(heap.map_get(id, &MapKey::Int(5)).and_then(|v| v.as_int()), Some(500));
    assert_eq!(heap.map_get(id, &MapKey::str("i:5")).and_then(|v| v.as_int()), Some(999));
    // remove an int key; positions fix up; the other keys survive
    assert_eq!(heap.map_remove(id, &MapKey::Int(5)).and_then(|v| v.as_int()), Some(500));
    assert!(!heap.map_contains(id, &MapKey::Int(5)));
    assert_eq!(heap.map_get(id, &MapKey::Int(7)).and_then(|v| v.as_int()), Some(70));
    assert_eq!(heap.map_get(id, &MapKey::str("i:5")).and_then(|v| v.as_int()), Some(999));
}

#[test]
fn snapshot_restore_round_trips_objects_arrays_maps() {
    let mut h = Heap::default();
    let oh = h.alloc("com/example/Foo");
    h.put_field(oh, "x", Value::Int(42));
    h.put_field(oh, "big", Value::Long(9_000_000_000));
    h.put_field(oh, "name", Value::Str("hello".into()));
    h.put_field(oh, "ratio", Value::Double(1.5));
    let ah = h.alloc_array("int", 3);
    h.array_set(ah, 0, Value::Int(7));
    h.array_set(ah, 2, Value::Int(9));
    let mh = h.alloc_map();
    h.map_put(mh, MapKey::str("k"), Value::Str("k".into()), Value::Int(99));

    let bytes = h.snapshot();
    assert!(!bytes.is_empty());
    let h2 = Heap::restore(&bytes).expect("restore");

    // Object fields survive (int / long / Str-via-box / double).
    assert_eq!(h2.get_field(oh, "x").as_int(), Some(42));
    assert_eq!(h2.get_field(oh, "big").as_long(), Some(9_000_000_000));
    assert_eq!(h2.get_field(oh, "name").display(), "hello");
    assert_eq!(h2.get_field(oh, "ratio").as_f64(), 1.5);
    // Primitive array (native desc regenerated → array_get works).
    assert_eq!(h2.array_get(ah, 0).and_then(|v| v.as_int()), Some(7));
    assert_eq!(h2.array_get(ah, 2).and_then(|v| v.as_int()), Some(9));
    // Map.
    assert_eq!(h2.map_get(mh, &MapKey::str("k")).and_then(|v| v.as_int()), Some(99));
    // Class-name re-interning produced a usable object class.
    assert!(h2.snapshot().len() > 0);
}

#[test]
fn snapshot_array_wire_matches_legacy_string_shape() {
    #[derive(serde::Serialize)]
    struct LegacyArrayRef<'a> {
        element: &'a Arc<str>,
        data: &'a ArrayData,
    }

    for name in ["", "[Lданные/世界;", "prefix\0suffix"] {
        let element: Arc<str> = Arc::from(name);
        let data = ArrayData::Int(vec![1, 2, 3]);
        let legacy = bincode::serialize(&LegacyArrayRef { element: &element, data: &data }).unwrap();
        let current = bincode::serialize(&SnapshotArrayRef { element: name, data: &data }).unwrap();
        assert_eq!(current, legacy, "snapshot wire changed for {name:?}");
    }

    let element: Arc<str> = Arc::from("int");
    let data = ArrayData::Int(vec![4, 5, 6]);
    let legacy_sequence = bincode::serialize(&vec![LegacyArrayRef {
        element: &element,
        data: &data,
    }])
    .unwrap();
    let mut heap = Heap::default();
    let element_id = heap.intern_array_element("int");
    heap.put_array(element_id, ArrayData::Int(vec![4, 5, 6]));
    let streamed_sequence = bincode::serialize(&SnapshotArrays(&heap)).unwrap();
    assert_eq!(streamed_sequence, legacy_sequence, "snapshot sequence wire changed");
}

#[test]
fn snapshot_array_elements_and_handle_indices_round_trip() {
    let mut heap = Heap::default();
    let empty = heap.alloc_array("", 0);
    let unicode = heap.alloc_array("[Lданные/世界;", 1);
    let nul = heap.alloc_array("prefix\0suffix", 2);
    let dead = heap.alloc_array("Dead", 1);
    heap.gc(&[Value::Array(empty), Value::Array(unicode), Value::Array(nul)]);
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[dead as usize].element_id, DEAD_ARRAY_ELEMENT_ID);

    let bytes = heap.snapshot();
    let mut restored = Heap::restore(&bytes).expect("restore array element ids");
    assert_eq!(restored.array_element(empty).as_deref(), Some(""));
    assert_eq!(restored.array_element(unicode).as_deref(), Some("[Lданные/世界;"));
    assert_eq!(restored.array_element(nul).as_deref(), Some("prefix\0suffix"));
    assert_eq!(restored.array_element(dead), None);
    assert_eq!(restored.array_descs.read().unwrap_or_else(|e| e.into_inner())[dead as usize].element_id, DEAD_ARRAY_ELEMENT_ID);
    assert_eq!(restored.alloc_array("AfterRestore", 0), dead);
    assert_eq!(restored.array_element(dead).as_deref(), Some("AfterRestore"));
}

#[test]
fn snapshot_restore_keeps_dead_handle_reusable() {
    let mut heap = Heap::default();
    let live = heap.alloc("Live");
    let dead = heap.alloc("Dead");
    heap.retire_object_slot(dead as usize, true);

    let bytes = heap.snapshot();
    let mut restored = Heap::restore(&bytes).expect("restore nullable-slot snapshot");
    assert_eq!(restored.class_of(live), Some("Live"));
    assert!(restored.get(dead).is_none());
    assert!(!restored.obj_alive.lock().unwrap_or_else(|e| e.into_inner())[dead as usize]);
    assert_eq!(restored.free_obj.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&h| h == dead).count(), 1);

    let reused = restored.alloc("AfterRestore");
    assert_eq!(reused, dead);
    assert_eq!(restored.class_of(reused), Some("AfterRestore"));
}
