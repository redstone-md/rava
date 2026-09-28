use crate::{Value, Vm, class_loader::ClassLoader};

fn arena(text: &str) -> Value {
    let cell = crate::nextgen::intern_arena_string(text);
    assert_ne!(cell, 0);
    // The intern pool publishes a stable arena address for the process lifetime.
    Value::Obj(unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) })
}

#[test]
fn arena_string_collection_hash_matches_string_value() {
    let mut vm = Vm::new(ClassLoader::new());
    for text in ["", "a", "hello", "Привет"] {
        let flat = Value::Str(text.into());
        let object = arena(text);
        let expected = text.encode_utf16().fold(0i32,
            |hash, unit| hash.wrapping_mul(31).wrapping_add(unit as i32));
        assert_eq!(crate::map::element_hash(&mut vm, &flat), expected);
        assert_eq!(crate::map::element_hash(&mut vm, &object), expected);
        assert_eq!(crate::map::list_hash(&mut vm, &[flat.clone(), object]),
            crate::map::list_hash(&mut vm, &[flat.clone(), flat]));
    }
}

#[test]
fn arena_string_case_insensitive_comparison_accepts_mixed_representations() {
    let mut vm = Vm::new(ClassLoader::new());
    let comparator = vm.heap.alloc("java/util/Comparator");
    vm.heap.put_field(comparator, "$cmp", Value::Str("ciorder".into()));
    for (left, right, expected) in [("A", "a", 0), ("a", "B", -1), ("b", "A", 1)] {
        for a in [Value::Str(left.into()), arena(left)] {
            for b in [Value::Str(right.into()), arena(right)] {
                assert_eq!(vm.compare(&Value::Ref(comparator), &a, &b).unwrap(), expected);
            }
        }
    }
}

#[test]
fn arena_string_cast_message_uses_java_type_and_module() {
    let vm = Vm::new(ClassLoader::new());
    let expected = "class java.lang.String cannot be cast to class java.lang.Integer (java.lang.String and java.lang.Integer are in module java.base of loader 'bootstrap')";
    assert_eq!(crate::opcodes::cast_message(&vm, &arena("x"), "java/lang/Integer"), expected);
}

/// A native TreeMap's order is keyed to the keys' TEXT, not to which regime minted
/// them: an interpreted caller's inline `Value::Str`, a compiled caller's interned
/// arena string and a boxed `java/lang/String` handle are one key set to the guest,
/// so the sorted views, first/last and the `natural_cmp` range/probe methods must
/// all answer `apple, fig, pear` — whichever representation each key or bound is in.
#[test]
fn tree_map_orders_mixed_representation_string_keys() {
    let mut vm = Vm::new(ClassLoader::new());
    let tm = vm.heap.alloc("java/util/TreeMap");
    let id = vm.heap.alloc_map();
    vm.heap.put_field_syn(tm, crate::heap::Syn::Map, Value::Int(id as i32));

    // fig: an exact java/lang/String handle carrying $str.
    let fig = vm.heap.alloc("java/lang/String");
    vm.heap.put_field_syn(fig, crate::heap::Syn::Str, Value::Str("fig".into()));

    // Insert deliberately unsorted, one key per representation.
    for (k, v) in [
        (Value::Str("pear".into()), 3),
        (arena("apple"), 1),
        (Value::Ref(fig), 2),
    ] {
        let text = crate::natives::str_of(&vm, Some(&k)).unwrap();
        vm.heap.map_put(id, crate::heap::MapKey::str(format!("s:{text}")), k, Value::Int(v));
    }

    let text_of = |vm: &Vm, v: &Value| crate::natives::str_of(vm, Some(v)).unwrap();
    let key_texts = |vm: &mut Vm, owner: u32| {
        crate::map::map_view_elements(vm, owner, "key")
            .unwrap()
            .iter()
            .map(|k| text_of(vm, k))
            .collect::<Vec<String>>()
    };
    let call = |vm: &mut Vm, name: &str, args: &[Value]| {
        crate::map::map_method(vm, name, args).unwrap().unwrap().unwrap()
    };

    // The view iterates in the guest's text order, keeping each key's own representation.
    let view = crate::map::map_view_elements(&mut vm, tm, "key").unwrap();
    assert_eq!(key_texts(&mut vm, tm), ["apple", "fig", "pear"]);
    assert!(matches!(view[0], Value::Obj(_)), "the arena key stays arena");
    assert!(matches!(view[1], Value::Ref(_)), "the handle key stays a handle");
    assert!(matches!(view[2], Value::Str(_)), "the inline key stays inline");

    // first/last agree with the view order.
    let first = call(&mut vm, "firstKey", &[Value::Ref(tm)]);
    assert_eq!(text_of(&vm, &first), "apple");
    let last = call(&mut vm, "lastKey", &[Value::Ref(tm)]);
    assert_eq!(text_of(&vm, &last), "pear");

    // Ranges read the same order from ANY representation's bound.
    let head = call(&mut vm, "headMap", &[Value::Ref(tm), arena("fig")]);
    let Value::Ref(head) = head else { panic!("headMap returns a map") };
    assert_eq!(key_texts(&mut vm, head), ["apple"]);
    let tail = call(&mut vm, "tailMap", &[Value::Ref(tm), Value::Ref(fig)]);
    let Value::Ref(tail) = tail else { panic!("tailMap returns a map") };
    assert_eq!(key_texts(&mut vm, tail), ["fig", "pear"]);

    // So do the four probes, again with a bound in each representation.
    for (name, bound, want) in [
        ("higherKey", arena("apple"), "fig"),
        ("ceilingKey", Value::Str("fi".into()), "fig"),
        ("floorKey", Value::Ref(fig), "fig"),
        ("lowerKey", Value::Str("pear".into()), "fig"),
    ] {
        let got = call(&mut vm, name, &[Value::Ref(tm), bound]);
        assert_eq!(text_of(&vm, &got), want, "{name} across representations");
    }
}

/// A `$str` carrier that is NOT a String (BigInteger) must not be ordered by its
/// text: `string_key_text` gates `Ref` on `java/lang/String`, so a map holding one
/// keeps the insertion-order fallback exactly as before the representation-blind fix.
#[test]
fn tree_map_does_not_order_a_non_string_str_carrier() {
    let mut vm = Vm::new(ClassLoader::new());
    let tm = vm.heap.alloc("java/util/TreeMap");
    let id = vm.heap.alloc_map();
    vm.heap.put_field_syn(tm, crate::heap::Syn::Map, Value::Int(id as i32));

    let bi = vm.heap.alloc("java/math/BigInteger");
    vm.heap.put_field_syn(bi, crate::heap::Syn::Str, Value::Str("zz".into()));
    vm.heap.map_put(id, crate::heap::MapKey::str("s:zz"), Value::Ref(bi), Value::Int(1));
    vm.heap.map_put(id, crate::heap::MapKey::str("s:apple"), Value::Str("apple".into()), Value::Int(2));

    // Insertion order, not text order: no order was invented for the carrier.
    let view = crate::map::map_view_elements(&mut vm, tm, "key").unwrap();
    let texts: Vec<String> = view
        .iter()
        .map(|k| crate::natives::str_of(&vm, Some(k)).unwrap())
        .collect();
    assert_eq!(texts, ["zz", "apple"]);
}
