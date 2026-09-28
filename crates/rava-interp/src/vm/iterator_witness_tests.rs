use crate::{class_loader::ClassLoader, heap::Syn, Value, Vm};
use rava_classfile::{ClassFile, Constant, ConstantPool};
use std::sync::Arc;

#[test]
fn compiled_iterator_checks_source_before_advancing() {
    let mut vm = Vm::new(ClassLoader::new());
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("java/util/Iterator".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("next".into()),
        Constant::Utf8("()Ljava/lang/Object;".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::InterfaceMethodRef { class_index: 2, nat_index: 5 },
        Constant::Utf8("hasNext".into()),
        Constant::Utf8("()Z".into()),
        Constant::NameAndType { name_index: 7, desc_index: 8 },
        Constant::InterfaceMethodRef { class_index: 2, nat_index: 9 },
    ]);
    let caller = vm.loader.define_synthetic(ClassFile::synthesize(pool, 0x21,
        "IteratorWitnessCaller".into(), None, Vec::new(), Vec::new()));
    let source = vm.heap.alloc("java/util/ArrayList");
    let snapshot = vm.heap.alloc_values(crate::stream::OBJ, vec![Value::Null; 2]);
    vm.heap.put_field_syn(source, Syn::Elems, Value::Array(snapshot));
    let iterator = vm.heap.alloc("java/util/Iterator");
    vm.heap.put_field_syn(iterator, Syn::Elems, Value::Array(snapshot));
    vm.heap.put_field_syn(iterator, Syn::Pos, Value::Int(0));
    vm.heap.put_field_syn(iterator, Syn::Source, Value::Ref(source));
    vm.heap.put_field_syn(iterator, Syn::ExpSize, Value::Int(2));
    let raw = [crate::jit::box_ref(&vm, &Value::Ref(iterator))];
    let caller_ptr = Arc::as_ptr(&caller);
    assert_eq!(vm.jit_iter_shortcut(caller_ptr, 6, 3, &raw), Some(0));
    assert_eq!(vm.heap.get_field_syn(iterator, Syn::Pos).as_int(), Some(1));
    let changed = vm.heap.alloc_values(crate::stream::OBJ, vec![Value::Null; 3]);
    vm.heap.put_field_syn(source, Syn::Elems, Value::Array(changed));
    assert_eq!(vm.jit_iter_shortcut(caller_ptr, 10, 3, &raw), Some(1));
    assert_eq!(vm.jit_iter_shortcut(caller_ptr, 6, 3, &raw), None,
        "changed source must reach the ordinary throwing path");
    assert_eq!(vm.heap.get_field_syn(iterator, Syn::Pos).as_int(), Some(1));
    assert!(matches!(crate::stream::collections::iter_op(&mut vm, "next", &[Value::Ref(iterator)]),
        Err(crate::VmError::Throw(_))));
    assert_eq!(vm.heap.get_field_syn(iterator, Syn::Pos).as_int(), Some(1));
}

#[test]
fn clear_replaces_backing_and_invalidates_existing_iterator() {
    let mut vm = Vm::new(ClassLoader::new());
    let source = vm.heap.alloc("java/util/ArrayList");
    let backing = vm
        .heap
        .alloc_values(crate::stream::OBJ, vec![Value::Int(7), Value::Int(9)]);
    vm.heap
        .put_field_syn(source, Syn::Elems, Value::Array(backing));

    let iterator = vm.heap.alloc("java/util/Iterator");
    vm.heap
        .put_field_syn(iterator, Syn::Elems, Value::Array(backing));
    vm.heap.put_field_syn(iterator, Syn::Pos, Value::Int(0));
    vm.heap
        .put_field_syn(iterator, Syn::Source, Value::Ref(source));
    vm.heap.put_field_syn(iterator, Syn::ExpSize, Value::Int(2));

    assert!(matches!(
        crate::stream::collections::coll_op(&mut vm, "clear", "()V", &[Value::Ref(source)]),
        Ok(Some(None))
    ));
    let Value::Array(empty) = vm.heap.get_field_syn(source, Syn::Elems) else {
        panic!("clear must retain an empty backing array");
    };
    assert_eq!(vm.heap.elems_len(source, empty), 0);
    assert!(matches!(
        crate::stream::collections::iter_op(&mut vm, "next", &[Value::Ref(iterator)]),
        Err(crate::VmError::Throw(_))
    ));
}
