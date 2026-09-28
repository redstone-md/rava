//! Array descriptors, element interning, and the primitive sort.

use super::*;

#[test]
fn array_element_id_occupies_descriptor_padding() {
    assert_eq!(std::mem::size_of::<ArrayObj>(), 32);
    assert_eq!(std::mem::size_of::<ArrayDesc>(), 16);
    assert_eq!(std::mem::offset_of!(ArrayDesc, element_id), 12);
}

#[test]
fn array_descriptor_metadata_is_synced_across_reuse() {
    let mut heap = Heap::default();
    let handle = heap.alloc_array("int", 2);
    let index = handle as usize;
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].data, heap.arrays.read().unwrap_or_else(|e| e.into_inner())[index].data.data_ptr());
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].len, 2);
    assert_eq!(heap.array_element(handle).as_deref(), Some("int"));
    assert_eq!(heap.jit_ctx.desc_base, heap.array_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize);

    heap.array_push(handle, Value::Int(7));
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].data, heap.arrays.read().unwrap_or_else(|e| e.into_inner())[index].data.data_ptr());
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].len, 3);
    assert_eq!(heap.array_element(handle).as_deref(), Some("int"));

    heap.gc(&[]);
    assert!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].data.is_null());
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].len, 0);
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].element_id, DEAD_ARRAY_ELEMENT_ID);

    let reused = heap.alloc_array("long", 5);
    assert_eq!(reused, handle);
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].data, heap.arrays.read().unwrap_or_else(|e| e.into_inner())[index].data.data_ptr());
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[index].len, 5);
    assert_eq!(heap.array_element(handle).as_deref(), Some("long"));
}

#[test]
fn array_element_interner_is_canonical_and_resolves_edge_names() {
    let mut heap = Heap::default();
    let int_a = heap.intern_array_element("int");
    let int_b = heap.intern_array_element("int");
    let object = heap.intern_array_element("Ljava/lang/Object;");
    assert_eq!(int_a, int_b);
    assert_ne!(int_a, object);

    for name in ["", "[Lданные/世界;", "prefix\0suffix"] {
        let handle = heap.alloc_array(name, 0);
        assert_eq!(heap.array_element(handle).as_deref(), Some(name));
    }
}

#[test]
fn swept_array_sentinel_is_hidden_and_reused_with_a_valid_id() {
    let mut heap = Heap::default();
    let handle = heap.alloc_array("Old", 1);
    heap.gc(&[]);
    assert_eq!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[handle as usize].element_id, DEAD_ARRAY_ELEMENT_ID);
    assert_eq!(heap.array_element(handle), None);

    let reused = heap.alloc_array("New", 1);
    assert_eq!(reused, handle);
    assert_ne!(heap.array_descs.read().unwrap_or_else(|e| e.into_inner())[reused as usize].element_id, DEAD_ARRAY_ELEMENT_ID);
    assert_eq!(heap.array_element(reused).as_deref(), Some("New"));
}

#[test]
fn malformed_array_element_id_is_a_non_panicking_miss() {
    let mut heap = Heap::default();
    heap.put_array(17, ArrayData::Ref(Vec::new()));
    heap.arr_alive.write().unwrap_or_else(|e| e.into_inner()).push(true);
    assert_eq!(heap.array_element(0), None);
    assert!(!heap.snapshot().is_empty(), "diagnostic snapshot fallback must not panic");
}

#[test]
fn sort_primitive_int_natural_order() {
    let mut heap = Heap::default();
    let h = heap.alloc_values("int", vec![Value::Int(5), Value::Int(-3), Value::Int(0), Value::Int(2), Value::Int(-3)]);
    assert!(heap.sort_primitive(h, 0, 5));
    assert_eq!(vals(&heap, h), vec![-3, -3, 0, 2, 5]);
}

#[test]
fn sort_primitive_long_and_range() {
    let mut heap = Heap::default();
    let h = heap.alloc_values("long", vec![Value::Long(9), Value::Long(1), Value::Long(7), Value::Long(3)]);
    // sort only [1,3): elements 1 and 7 → already ordered; 9 and 3 untouched.
    assert!(heap.sort_primitive(h, 1, 3));
    assert_eq!(vals(&heap, h), vec![9, 1, 7, 3]);
    assert!(heap.sort_primitive(h, 0, 4));
    assert_eq!(vals(&heap, h), vec![1, 3, 7, 9]);
}

#[test]
fn sort_primitive_byte_is_signed() {
    let mut heap = Heap::default();
    // bytes 0xFF (-1), 0x01 (1), 0x80 (-128), 0x7F (127)
    let h = heap.alloc_values("byte", vec![Value::Int(-1), Value::Int(1), Value::Int(-128), Value::Int(127)]);
    assert!(heap.sort_primitive(h, 0, 4));
    // signed ascending: -128, -1, 1, 127
    let got: Vec<i32> = (0..4).map(|i| heap.array_get(h, i).unwrap().as_int().unwrap()).collect();
    assert_eq!(got, vec![-128, -1, 1, 127]);
}

#[test]
fn sort_primitive_parallel_threshold() {
    let mut heap = Heap::default();
    // 20000 > PAR_THRESHOLD → exercises the rayon path. Descending input.
    let n = 20000i32;
    let vs: Vec<Value> = (0..n).rev().map(Value::Int).collect();
    let h = heap.alloc_values("int", vs);
    assert!(heap.sort_primitive(h, 0, n as usize));
    let got = vals(&heap, h);
    assert!(got.windows(2).all(|w| w[0] <= w[1]), "not sorted");
    assert_eq!(got.first(), Some(&0));
    assert_eq!(got.last(), Some(&(n as i64 - 1)));
}

#[test]
fn sort_primitive_rejects_float_and_ref() {
    let mut heap = Heap::default();
    let hf = heap.alloc_values("double", vec![Value::Double(2.0), Value::Double(1.0)]);
    assert!(!heap.sort_primitive(hf, 0, 2)); // double → caller's generic path
    let hr = heap.alloc_values("java/lang/Object", vec![Value::Null, Value::Null]);
    assert!(!heap.sort_primitive(hr, 0, 2)); // reference → generic path
}
