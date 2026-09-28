//! Interpreter unit tests: argument-buffer recycling, the instanceof memo, lambda dedup,
//! and arena field access.

use super::*;

#[cfg(test)]
mod argbuf_recycle_tests {
    use super::PooledVals;
    use crate::Value;

    /// Empty this thread's Vec pool so a test observes only its own traffic.
    /// The drained buffers are dropped, never handed back.
    fn drain_pool() -> Vec<Vec<Value>> {
        let mut held = Vec::new();
        loop {
            let v = crate::frames::pool_take();
            if v.capacity() == 0 || held.len() > 4096 {
                return held;
            }
            held.push(v);
        }
    }

    /// The whole point of the guard: a buffer that the native arm only BORROWED
    /// goes back to the pool, so the next `pop_args` does not have to allocate.
    #[test]
    fn drop_returns_the_buffer_to_the_pool() {
        let _drained = drain_pool();
        let mut v = Vec::with_capacity(16);
        v.push(Value::Int(1));
        let addr = v.as_ptr() as usize;
        drop(PooledVals::with_recycle(v, true));

        let back = crate::frames::pool_take();
        assert_eq!(back.as_ptr() as usize, addr, "the SAME allocation came back");
        assert!(back.capacity() >= 16, "capacity is retained, so no regrow");
    }

    /// The pool must never hold a `Value` across a GC point (the contract
    /// `natives::CanonArgs` already documents), so the guard clears first.
    #[test]
    fn recycled_buffer_is_cleared_so_no_value_is_held() {
        let _drained = drain_pool();
        let mut v = Vec::with_capacity(8);
        v.push(Value::Int(7));
        v.push(Value::Null);
        drop(PooledVals::with_recycle(v, true));

        let back = crate::frames::pool_take();
        assert!(back.is_empty(), "pooled buffer must hold no Values");
    }

    /// `into_inner` hands ownership to a callee that recycles the buffer itself
    /// (`Vm::invoke` / the trampoline's `pending`). If the guard ALSO pooled it,
    /// the same allocation would sit in the pool while still owned elsewhere —
    /// two live `Vec`s over one buffer.
    #[test]
    fn into_inner_disarms_the_guard() {
        let _drained = drain_pool();
        let v = Vec::with_capacity(16);
        let taken = PooledVals::with_recycle(v, true).into_inner();
        assert_eq!(taken.capacity(), 16, "the buffer is handed on intact");

        let back = crate::frames::pool_take();
        assert_eq!(back.capacity(), 0, "nothing was pooled behind our back");
    }

    /// Flag OFF must behave exactly as the code did before this change.
    #[test]
    fn recycle_off_drops_as_before() {
        let _drained = drain_pool();
        drop(PooledVals::with_recycle(Vec::with_capacity(16), false));
        let back = crate::frames::pool_take();
        assert_eq!(back.capacity(), 0, "flag OFF pools nothing");
    }

    /// Deref/DerefMut keep the call sites reading like a plain `Vec<Value>` —
    /// this is what lets the guard wrap the dispatch path without rewriting it.
    #[test]
    fn derefs_to_the_underlying_vec() {
        let mut g = PooledVals::with_recycle(Vec::new(), false);
        g.push(Value::Int(3));
        g.insert(0, Value::Int(9));
        assert_eq!(g.len(), 2);
        assert_eq!(g.first().and_then(Value::as_int), Some(9));
    }
}
#[cfg(test)]
mod instanceof_memo_tests {
    use super::{instanceof_class_key, IOF_REF_TAG};
    use crate::heap::Heap;
    use crate::Value;

    /// The memo is keyed on the receiver's CLASS, so it may only be consulted for
    /// receivers whose `instance_of` answer is a function of that class. This
    /// pins the exclusion set: every shape `instance_of` decides from a
    /// per-OBJECT property (a lambda's functional interface) or already answers
    /// in O(1) must key to `None`.
    #[test]
    fn only_class_decided_receivers_are_keyed() {
        let mut heap = Heap::default();

        // A plain object IS class-decided → keyed, and two instances of the same
        // class share one key (that is the whole point of the memo).
        let a = heap.alloc("java/util/ArrayDeque");
        let b = heap.alloc("java/util/ArrayDeque");
        let ka = instanceof_class_key(&heap, &Value::Ref(a));
        assert_eq!(ka, instanceof_class_key(&heap, &Value::Ref(b)));
        assert!(ka.is_some_and(|k| k & IOF_REF_TAG != 0), "Ref keys carry the pointer tag");

        // A different class must not collide with it.
        let other = heap.alloc("java/util/BitSet");
        assert_ne!(ka, instanceof_class_key(&heap, &Value::Ref(other)));

        // A LAMBDA is decided by its own `$fi`, not by its class (every handle
        // lambda shares the one interned `$Lambda` class name), so it must be
        // excluded — a key here would answer one lambda's instanceof with
        // another's functional interface.
        // `new` is the only constructor: it pins the `fp` fold the direct-call cache
        // reads, so a struct literal here would not compile (and must not).
        let site = std::sync::Arc::new(crate::heap::LambdaSite::new(
            "Owner".into(),
            "run".into(),
            "()V".into(),
        ));
        let l = heap.alloc_lambda(crate::Lambda { kind: 6, site, captured: Vec::new() });
        assert_eq!(instanceof_class_key(&heap, &Value::Ref(l)), None);

        // Shapes `instance_of` already answers in O(1) are not worth a memo entry
        // and must never take one.
        for v in [Value::Null, Value::Int(1), Value::Long(1), Value::Float(1.0),
                  Value::Double(1.0), Value::Str("x".into()), Value::Array(0)] {
            assert_eq!(instanceof_class_key(&heap, &v), None, "{v:?} must not be keyed");
        }
    }
}
#[cfg(test)]
mod lambda_dedup_tests {
    use super::lambda_capture_key;
    use crate::Value;

    #[test]
    fn capture_keys_are_exact_and_skip_unstable_payloads() {
        assert_eq!(lambda_capture_key(&Value::Ref(7)), Some((1, 7)));
        assert_eq!(lambda_capture_key(&Value::Array(7)), Some((2, 7)));
        assert_eq!(lambda_capture_key(&Value::Int(-7)), Some((3, -7)));
        assert_eq!(lambda_capture_key(&Value::Long(i64::MIN)), Some((4, i64::MIN)));
        assert_eq!(lambda_capture_key(&Value::Null), Some((6, 0)));

        assert_eq!(lambda_capture_key(&Value::Str("x".into())), None);
        assert_eq!(lambda_capture_key(&Value::Float(1.0)), None);
        assert_eq!(lambda_capture_key(&Value::Double(1.0)), None);
    }
}
#[cfg(all(test, feature = "next-gen-gc"))]
mod arena_field_tests {
    use super::{
        arena_getfield, arena_putfield, decode_arena_cell, encode_arena_cell,
        is_stray_primitive_factory, object_class_name, object_field_by_id,
    };
    use crate::value::Value;
    use std::sync::Arc;

    /// Regression: the record ObjectMethods helpers must read an arena record
    /// (`Value::Obj`), not drop it to Null. Dropping arena records collapsed every
    /// record to "null"/equal-alike and re-broke the ResourceKey$InternKey interner
    /// ("Adding duplicate key" at registry bootstrap) — the same Obj-vs-Ref bug
    /// class as the gson factory guard.
    #[test]
    fn record_helpers_read_arena_objects_not_null() {
        let cid = crate::nextgen::register_class(
            &(Arc::from("ArenaRecT") as Arc<str>),
            &(vec![(900u32, 0u8), (901u32, 1u8)].into()),
        );
        let p = crate::nextgen::tlab_try_alloc(cid, 2).unwrap();
        arena_putfield(p, 900, &Value::Int(42));
        arena_putfield(p, 901, &Value::Long(7));
        let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
        let obj = Value::Obj(p);
        assert_eq!(object_class_name(&mut vm, &obj).as_deref(), Some("ArenaRecT"));
        assert!(matches!(object_field_by_id(&mut vm, &obj, 900), Value::Int(42)));
        assert!(matches!(object_field_by_id(&mut vm, &obj, 901), Value::Long(7)));
        // A non-object value has no class and reads as Null (the stray case).
        assert_eq!(object_class_name(&mut vm, &Value::Int(1)), None);
        assert!(matches!(object_field_by_id(&mut vm, &Value::Int(1), 900), Value::Null));
    }

    /// Regression: the gson `TypeAdapterFactory.create` skip must treat an arena
    /// object (`Value::Obj`) as a REAL factory, never as a stray primitive. The
    /// pre-arena guard `!matches!(Ref | Array)` wrongly classified `Obj` as
    /// non-object, returned a null adapter for every factory, and made
    /// `Gson.getAdapter` throw "GSON cannot handle <type>" for every class. Only
    /// numeric primitives are the stray-primitive case.
    #[test]
    fn gson_factory_skip_spares_arena_and_handle_receivers() {
        let cid = crate::nextgen::register_class(
            &(Arc::from("FactoryT") as Arc<str>),
            &(vec![(800u32, 0u8)].into()),
        );
        let p = crate::nextgen::tlab_try_alloc(cid, 1).unwrap();
        // Real factories must dispatch normally — NOT be skipped.
        assert!(!is_stray_primitive_factory(&Value::Obj(p)), "arena factory");
        assert!(!is_stray_primitive_factory(&Value::Ref(1)), "legacy-heap factory");
        assert!(!is_stray_primitive_factory(&Value::Array(1)), "array (defensive)");
        assert!(!is_stray_primitive_factory(&Value::Null), "null is not a stray primitive");
        // Only genuine numeric primitives are the stray-in-`factories` case.
        for prim in [Value::Int(0), Value::Long(0), Value::Float(0.0), Value::Double(0.0)] {
            assert!(is_stray_primitive_factory(&prim), "primitive {prim:?} is skippable");
        }
    }

    /// Regression: `Map.Entry` types (key/value modeled as synthetic heap-only
    /// `$key`/`$val`) MUST be handle-backed, never arena. An arena entry has no
    /// cell for them and `entry_method` rejects the arena receiver → getKey/getValue
    /// no-op → caller's `areturn` underflows (guava ImmutableMapEntry at registry
    /// bootstrap). `is_native_backed_alloc` keeps them off the arena allocator.
    #[test]
    fn entry_types_are_native_backed_handle_alloc() {
        for c in [
            "java/util/Map$Entry",
            "java/util/AbstractMap$SimpleEntry",
            "java/util/AbstractMap$SimpleImmutableEntry",
        ] {
            assert!(crate::map::is_entry_type(c), "{c} is an entry type");
            assert!(super::is_native_backed_alloc(c), "{c} must be handle-backed");
        }
        // A plain bytecode value class is NOT entry/native-backed (stays arena).
        assert!(!crate::map::is_entry_type("net/minecraft/sounds/SoundEvent"));
        assert!(!super::is_native_backed_alloc("net/minecraft/sounds/SoundEvent"));
    }

    #[test]
    fn arena_cell_codec_roundtrips_primitives_and_null() {
        let cases = [
            (Value::Int(7), 0u8),
            (Value::Long(99), 1),
            (Value::Float(1.5), 2),
            (Value::Double(2.5), 3),
            (Value::Null, 6),
        ];
        for (v, kind) in cases {
            let cell = encode_arena_cell(&v);
            let back = decode_arena_cell(cell, kind);
            assert_eq!(format!("{v:?}"), format!("{back:?}"), "roundtrip {v:?}");
        }
    }

    #[test]
    fn value_obj_field_read_write_through_opcode_path() {
        // Register a class with an int + a long field; TLAB-alloc a Value::Obj;
        // putfield then getfield through the arena dual-dispatch helpers.
        let name: Arc<str> = Arc::from("OpcodeLabT");
        let layout: Arc<[(u32, u8)]> = vec![(500u32, 0u8), (501, 1)].into();
        let cid = crate::nextgen::register_class(&name, &layout);
        let p = crate::nextgen::tlab_try_alloc(cid, 2).unwrap();
        let obj = Value::Obj(p);
        // sanity: it IS a Value::Obj
        assert!(matches!(obj, Value::Obj(_)));

        arena_putfield(p, 500, &Value::Int(123));
        arena_putfield(p, 501, &Value::Long(456));
        match arena_getfield(p, 500) {
            Value::Int(123) => {}
            o => panic!("field 500 expected Int(123), got {o:?}"),
        }
        match arena_getfield(p, 501) {
            Value::Long(456) => {}
            o => panic!("field 501 expected Long(456), got {o:?}"),
        }
        // overwrite + reread
        arena_putfield(p, 500, &Value::Int(-9));
        assert!(matches!(arena_getfield(p, 500), Value::Int(-9)));
    }

    #[test]
    fn arena_cell_codec_roundtrips_cross_gen_handles_and_str() {
        // Phase 4: a ref-ish cell (kind 6) can hold a tagged cross-generation
        // handle (Ref/Array) — distinguishable from an arena→arena pointer (low 2
        // bits 0) and from null (cell == 0).
        for v in [Value::Ref(12345), Value::Array(67890)] {
            let cell = encode_arena_cell(&v);
            assert_ne!(cell & 3, 0, "cross-gen cell must be tagged: {v:?}");
            let back = decode_arena_cell(cell, 6);
            assert_eq!(format!("{v:?}"), format!("{back:?}"), "roundtrip {v:?}");
        }
        // A `Str` NO LONGER BOXES. Value-model step 1 (box-arm retirement,
        // `arena_str_refcell_enabled`, now default ON) makes a string in a ref cell born
        // as a 0b00 arena-string OBJECT, which is the entire point of that step — so
        // this case asserts the NEW contract rather than the old one. It used to demand
        // `cell & 3 != 0` and a Debug-format roundtrip, both of which encode precisely
        // the representation being retired; a test that pins the old invariant would
        // have to be deleted at the final bare-deref flip, so it is corrected here
        // instead. What must still hold is that the cell survives the round trip AS THE
        // SAME STRING.
        let s = "native-backed string";
        let cell = encode_arena_cell(&Value::Str(s.into()));
        assert_eq!(cell & 3, 0, "a Str in a ref cell must be born 0b00, not boxed");
        assert_ne!(cell, 0, "and must not collide with null");
        match decode_arena_cell(cell, 6) {
            Value::Obj(p) => {
                assert!(crate::nextgen::is_arena_string(p), "decoded to a non-string arena object");
                assert_eq!(crate::nextgen::arena_str_bytes(p), s.as_bytes(), "string content changed");
            }
            o => panic!("Str cell expected to decode to an arena string, got {o:?}"),
        }
        // null and an aligned arena pointer must NOT collide with the tags.
        assert!(matches!(decode_arena_cell(0, 6), Value::Null));
        let p = crate::nextgen::tlab_try_alloc(
            crate::nextgen::register_class(
                &(Arc::from("TagPtrT") as Arc<str>),
                &(vec![(700u32, 0u8)].into()),
            ),
            1,
        )
        .unwrap();
        let obj = Value::Obj(p);
        let cell = encode_arena_cell(&obj);
        assert_eq!(cell & 3, 0, "arena pointer is 8-aligned (untagged)");
        assert!(matches!(decode_arena_cell(cell, 6), Value::Obj(_)));
    }
}

#[cfg(test)]
mod activation_stack_call_plan_tests {
    use crate::{class_loader::ClassLoader, opcodes::ref_site_ref, Value, Vm};
    use rava_classfile::{ClassFile, Code, Constant, ConstantPool, MemberInfo};
    use std::sync::Arc;

    #[test]
    fn stackful_static_and_special_calls_stage_cached_plans() {
        let pool = ConstantPool::from_entries(vec![
            Constant::Unusable,
            Constant::MethodRef { class_index: 2, nat_index: 3 },
            Constant::Class { name_index: 4 },
            Constant::NameAndType { name_index: 5, desc_index: 6 },
            Constant::Utf8("StackfulPlanFixture".into()),
            Constant::Utf8("target".into()),
            Constant::Utf8("()I".into()),
            Constant::Class { name_index: 4 },
            Constant::NameAndType { name_index: 9, desc_index: 10 },
            Constant::Utf8("<init>".into()),
            Constant::Utf8("()V".into()),
            Constant::MethodRef { class_index: 7, nat_index: 8 },
        ]);
        let caller = MemberInfo {
            access_flags: 0x0009,
            name: "root".into(),
            descriptor: "()I".into(),
            code: Some(Code {
                max_stack: 1,
                max_locals: 0,
                bytecode: vec![0xb8, 0, 1, 0xac],
                exceptions: Vec::new(),
                ..Default::default()
            }),
        };
        let target = MemberInfo {
            access_flags: 0x0009,
            name: "target".into(),
            descriptor: "()I".into(),
            code: Some(Code {
                max_stack: 1,
                max_locals: 0,
                bytecode: vec![0x10, 7, 0xac],
                exceptions: Vec::new(),
                ..Default::default()
            }),
        };
        let special_caller = MemberInfo {
            access_flags: 0x0009,
            name: "specialRoot".into(),
            descriptor: "()I".into(),
            code: Some(Code {
                max_stack: 2,
                bytecode: vec![0xbb, 0, 7, 0x59, 0xb7, 0, 11, 0x57, 0x10, 8, 0xac],
                exceptions: Vec::new(),
                ..Default::default()
            }),
        };
        let constructor = MemberInfo {
            access_flags: 0x0001,
            name: "<init>".into(),
            descriptor: "()V".into(),
            code: Some(Code {
                max_stack: 0,
                max_locals: 1,
                bytecode: vec![0xb1],
                exceptions: Vec::new(),
                ..Default::default()
            }),
        };
        let mut vm = Vm::new(ClassLoader::new());
        vm.jit_enabled = false;
        let class = vm.loader.define_synthetic(ClassFile::synthesize(
            pool,
            0x0021,
            "StackfulPlanFixture".into(),
            None,
            Vec::new(),
            vec![caller, target, special_caller, constructor],
        ));

        assert!(crate::opcodes::call_plan_enabled());
        let stackful = vm.execute_stackful(Arc::clone(&class), "root", "()I", Vec::new()).unwrap();
        assert!(matches!(stackful, Some(Value::Int(7))));
        assert!(ref_site_ref(&class, 1).unwrap().plan.get().is_some());
        let recursive = vm.invoke(Arc::clone(&class), "root", "()I", Vec::new()).unwrap();
        assert!(matches!(recursive, Some(Value::Int(7))));
        let stackful = vm.execute_stackful(Arc::clone(&class), "root", "()I", Vec::new()).unwrap();
        assert!(matches!(stackful, Some(Value::Int(7))));

        let stackful = vm.execute_stackful(Arc::clone(&class), "specialRoot", "()I", Vec::new()).unwrap();
        assert!(matches!(stackful, Some(Value::Int(8))));
        assert!(ref_site_ref(&class, 11).unwrap().plan.get().is_some());
        let recursive = vm.invoke(Arc::clone(&class), "specialRoot", "()I", Vec::new()).unwrap();
        assert!(matches!(recursive, Some(Value::Int(8))));
        let stackful = vm.execute_stackful(class, "specialRoot", "()I", Vec::new()).unwrap();
        assert!(matches!(stackful, Some(Value::Int(8))));
    }
}

#[cfg(feature = "next-gen-gc")]
mod stackful_compiled_leaf_tests {
    use crate::{class_loader::ClassLoader, Value, Vm};
    use rava_classfile::{ClassFile, Code, Constant, ConstantPool, MemberInfo};
    use std::sync::Arc;

    #[test]
    fn stackful_driver_runs_gc_leaf_through_jit() {
        let file = ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0x0021,
            "StackfulJitLeaf".into(),
            None,
            Vec::new(),
            vec![MemberInfo {
                access_flags: 0x0009,
                name: "answer".into(),
                descriptor: "()I".into(),
                code: Some(Code {
                    max_stack: 1,
                    max_locals: 0,
                    bytecode: vec![0x08, 0xac],
                    exceptions: Vec::new(),
                    ..Default::default()
                }),
            }],
        );
        let mut vm = Vm::new(ClassLoader::new());
        vm.jit_enabled = true;
        vm.jit_threshold = 1;
        vm.background_jit = None;
        let class = vm.loader.define_synthetic(file);
        let code = class
            .file
            .method("answer", "()I")
            .unwrap()
            .code
            .as_ref()
            .unwrap();
        assert!(crate::jit::is_gc_leaf(&code.bytecode));
        let key = code as *const Code as usize;

        let first = vm
            .execute_stackful(Arc::clone(&class), "answer", "()I", Vec::new())
            .unwrap();
        let second = vm
            .execute_stackful(Arc::clone(&class), "answer", "()I", Vec::new())
            .unwrap();
        assert!(matches!(first, Some(Value::Int(5))));
        assert!(matches!(second, Some(Value::Int(5))));

        let status = match vm.jit_cache.get(&key) {
            None => "absent",
            Some(crate::jit::JitStatus::Cold(_)) => "cold",
            Some(crate::jit::JitStatus::Compiling) => "compiling",
            Some(crate::jit::JitStatus::Failed) => "failed",
            Some(crate::jit::JitStatus::Compiled(_)) => "compiled",
        };
        assert_eq!(status, "compiled", "eligible stackful leaf JIT status");

        let compiled = vm
            .execute_stackful(Arc::clone(&class), "answer", "()I", Vec::new())
            .unwrap();
        assert!(matches!(compiled, Some(Value::Int(5))));
    }
}
