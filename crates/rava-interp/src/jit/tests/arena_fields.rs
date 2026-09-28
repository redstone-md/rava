//! Arena field access: the callback-free getfield/putfield/acmp paths.

use super::*;

/// TRACER end-to-end at the machine-code level: compile a real `getfield`-int
/// method and run it against a hand-built LOW-ADDRESS arena object. The receiver
/// arrives as a raw pointer (`pack_local(Obj)`), the compiled dual path takes
/// the inline arena arm (`obj >> 40 == 0`), loads `[obj + HDR + slot*8]`, and
/// the `getfield` callback counter must NOT move — proving zero-callback inline
/// arena field access. (The recursive/live runtime can't exercise this yet —
/// recursive+arena segfaults until Task 2 — so this is the codegen proof.)
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_getfield_int_fires_no_callback() {
    if crate::parallel::cow_gate() { return; } // COW build routes fields via the callback by design
    use std::sync::Arc;
    // Arena class with one int field; object with cell[0] = 0x4D2 (1234).
    let name: Arc<str> = Arc::from("InlineGF");
    let layout: Arc<[(u32, u8)]> = vec![(0u32, 0u8)].into();
    let cid = crate::nextgen::register_class(&name, &layout);
    let p = crate::nextgen::tlab_try_alloc(cid, 1).unwrap();
    crate::nextgen::arena_set_cell(p, 0, 1234);
    assert_eq!(p.addr() >> 40, 0, "arena ptr must be low for the inline arm");

    // int f(InlineGF o){ return o.x; }  aload_0; getfield #6; ireturn
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("InlineGF".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("x".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    let c = code(vec![0x2a, 0xb4, 0x00, 0x06, 0xac], 1);
    // intern → field id 0 at const slot 0 (the inline arm reads [obj+HDR+0]).
    let f = compile(&c, "(LInlineGF;)I", &pool, 0, &mut |_, _| (0, Some(0)))
        .expect("getfield-int method compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let before = tl_cb(6); // 6 = getfield (thread-local, race-free)
    let mut locals = vec![pack_local(&JVal::Obj(p))];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) } as i32;
    let after = tl_cb(6);
    set_current_vm(prev);

    assert_eq!(got, 1234, "inline arena getfield returns the field value");
    assert_eq!(after, before, "getfield callback must NOT fire — the read was inlined");
}

/// Inline arena getfield for EVERY field type (long/float/double/ref) fires no
/// `getfield` runtime callback. Float/double are read then narrowed to int so
/// `call_desc` (an i64-returning thunk) can observe the value; the inline LOAD
/// is what's under test. Ref covers both re-tag forms: low-2-bit Ref handle →
/// high-word `REF_TAG`, and a raw `ArenaPtr` passing through unchanged.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_getfield_all_types_no_callback() {
    if crate::parallel::cow_gate() { return; } // COW build routes fields via the callback by design
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let g0 = tl_cb(6);

    // long: long f(O o){ return o.x; }  aload_0; getfield#6; lreturn
    {
        let p = arena_obj_one_field(1);
        crate::nextgen::arena_set_cell(p, 0, 0x1_0000_002A);
        let c = code(vec![0x2a, 0xb4, 0x00, 0x06, 0xad], 1);
        let f = compile(&c, "(LO;)J", &field_pool("J"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        assert_eq!(unsafe { f.call_desc(&mut l, desc_cell) }, 0x1_0000_002A, "getfield long");
    }
    // float: int f(O o){ return (int) o.x; }  aload_0; getfield#6; f2i; ireturn
    {
        let p = arena_obj_one_field(2);
        crate::nextgen::arena_set_cell(p, 0, (7.5f32).to_bits() as i64);
        let c = code(vec![0x2a, 0xb4, 0x00, 0x06, 0x8b, 0xac], 1);
        let f = compile(&c, "(LO;)I", &field_pool("F"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        assert_eq!(unsafe { f.call_desc(&mut l, desc_cell) } as i32, 7, "getfield float");
    }
    // double: int f(O o){ return (int) o.x; }  aload_0; getfield#6; d2i; ireturn
    {
        let p = arena_obj_one_field(3);
        crate::nextgen::arena_set_cell(p, 0, (42.0f64).to_bits() as i64);
        let c = code(vec![0x2a, 0xb4, 0x00, 0x06, 0x8e, 0xac], 1);
        let f = compile(&c, "(LO;)I", &field_pool("D"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        assert_eq!(unsafe { f.call_desc(&mut l, desc_cell) } as i32, 42, "getfield double");
    }
    // ref (Ref handle re-tag): O f(O o){ return o.x; }  aload_0; getfield#6; areturn
    {
        let p = arena_obj_one_field(4);
        crate::nextgen::arena_set_cell(p, 0, (5i64 << 2) | 1); // arena-encoded Ref(5)
        let c = code(vec![0x2a, 0xb4, 0x00, 0x06, 0xb0], 1);
        let f = compile(&c, "(LO;)LO;", &field_pool("LO;"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        assert_eq!(
            unsafe { f.call_desc(&mut l, desc_cell) },
            pack_local(&JVal::Ref(5)),
            "getfield ref re-tags low-2-bit Ref → high-word REF_TAG"
        );
    }
    // ref (raw ArenaPtr passthrough)
    {
        let p = arena_obj_one_field(4);
        let q = arena_obj_one_field(4);
        crate::nextgen::arena_set_cell(p, 0, q.addr() as i64);
        let c = code(vec![0x2a, 0xb4, 0x00, 0x06, 0xb0], 1);
        let f = compile(&c, "(LO;)LO;", &field_pool("LO;"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        assert_eq!(unsafe { f.call_desc(&mut l, desc_cell) }, q.addr() as i64, "getfield ref passes raw Obj");
    }

    let g1 = tl_cb(6);
    set_current_vm(prev);
    assert_eq!(g1, g0, "NO getfield callback for any inline arena field type");
}

/// Inline arena putfield for EVERY field type fires no `putfield` callback; the
/// stored cell is verified in arena memory. Ref covers raw `ArenaPtr` passthrough
/// AND a Ref handle encoded down to the low-2-bit `(h<<2)|1` arena form.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_putfield_all_types_no_callback() {
    if crate::parallel::cow_gate() { return; } // COW build routes fields via the callback by design
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let p0 = tl_cb(7);

    // int: void f(O o){ o.x = 99; }  aload_0; bipush 99; putfield#6; return
    {
        let p = arena_obj_one_field(0);
        let c = code(vec![0x2a, 0x10, 0x63, 0xb5, 0x00, 0x06, 0xb1], 1);
        let f = compile(&c, "(LO;)V", &field_pool("I"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        unsafe { f.call_desc(&mut l, desc_cell) };
        assert_eq!(crate::nextgen::arena_cell(p, 0), 99, "putfield int");
    }
    // long: void f(O o){ o.x = 1L; }  aload_0; lconst_1; putfield#6; return
    {
        let p = arena_obj_one_field(1);
        let c = code(vec![0x2a, 0x0a, 0xb5, 0x00, 0x06, 0xb1], 1);
        let f = compile(&c, "(LO;)V", &field_pool("J"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        unsafe { f.call_desc(&mut l, desc_cell) };
        assert_eq!(crate::nextgen::arena_cell(p, 0), 1, "putfield long");
    }
    // float: void f(O o){ o.x = 2.0f; }  aload_0; fconst_2; putfield#6; return
    {
        let p = arena_obj_one_field(2);
        let c = code(vec![0x2a, 0x0d, 0xb5, 0x00, 0x06, 0xb1], 1);
        let f = compile(&c, "(LO;)V", &field_pool("F"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        unsafe { f.call_desc(&mut l, desc_cell) };
        assert_eq!(crate::nextgen::arena_cell(p, 0), (2.0f32).to_bits() as i64, "putfield float");
    }
    // double: void f(O o){ o.x = 1.0; }  aload_0; dconst_1; putfield#6; return
    {
        let p = arena_obj_one_field(3);
        let c = code(vec![0x2a, 0x0f, 0xb5, 0x00, 0x06, 0xb1], 1);
        let f = compile(&c, "(LO;)V", &field_pool("D"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p))];
        unsafe { f.call_desc(&mut l, desc_cell) };
        assert_eq!(crate::nextgen::arena_cell(p, 0), (1.0f64).to_bits() as i64, "putfield double");
    }
    // ref (raw Obj): void f(O o, O v){ o.x = v; }  aload_0; aload_1; putfield#6; return
    {
        let p = arena_obj_one_field(4);
        let q = arena_obj_one_field(4);
        let c = code(vec![0x2a, 0x2b, 0xb5, 0x00, 0x06, 0xb1], 2);
        let f = compile(&c, "(LO;LO;)V", &field_pool("LO;"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p)), pack_local(&JVal::Obj(q))];
        unsafe { f.call_desc(&mut l, desc_cell) };
        assert_eq!(crate::nextgen::arena_cell(p, 0), q.addr() as i64, "putfield ref raw Obj passthrough");
    }
    // ref (Ref handle → low-2-bit encode): value is a Ref(5) handle
    {
        let p = arena_obj_one_field(4);
        let c = code(vec![0x2a, 0x2b, 0xb5, 0x00, 0x06, 0xb1], 2);
        let f = compile(&c, "(LO;LO;)V", &field_pool("LO;"), 0, &mut |_, _| (0, Some(0))).unwrap();
        let mut l = vec![pack_local(&JVal::Obj(p)), pack_local(&JVal::Ref(5))];
        unsafe { f.call_desc(&mut l, desc_cell) };
        assert_eq!(crate::nextgen::arena_cell(p, 0), (5i64 << 2) | 1, "putfield ref encodes handle → low-2-bit");
    }

    let p1 = tl_cb(7);
    set_current_vm(prev);
    assert_eq!(p1, p0, "NO putfield callback for any inline arena field type");
}

/// Reference comparison on arena receivers compares raw pointers inline (arena
/// identity == pointer identity) and fires no `acmp` callback.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_acmp_no_callback() {
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let a0 = tl_cb(9);

    // int f(O a, O b){ return a==b ? 1 : 0; }
    //  aload_0; aload_1; if_acmpeq +5; iconst_0; ireturn; iconst_1; ireturn
    let c = code(vec![0x2a, 0x2b, 0xa5, 0x00, 0x05, 0x03, 0xac, 0x04, 0xac], 2);
    let f = compile(&c, "(LO;LO;)I", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let p = arena_obj_one_field(4);
    let q = arena_obj_one_field(4);

    let mut eq = vec![pack_local(&JVal::Obj(p)), pack_local(&JVal::Obj(p))];
    assert_eq!(unsafe { f.call_desc(&mut eq, desc_cell) } as i32, 1, "arena acmp equal");
    let mut ne = vec![pack_local(&JVal::Obj(p)), pack_local(&JVal::Obj(q))];
    assert_eq!(unsafe { f.call_desc(&mut ne, desc_cell) } as i32, 0, "arena acmp not-equal");

    let a1 = tl_cb(9);
    set_current_vm(prev);
    assert_eq!(a1, a0, "NO acmp callback for arena reference compares");
}
