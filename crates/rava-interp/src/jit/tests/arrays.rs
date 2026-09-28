//! Array codegen: typed loads and stores, the bounds fault, and the store barrier.

use super::*;

#[test]
fn jit_iastore_updates_int_array() {
    let c = code(
        vec![
            0x2a, 0x03, 0x10, 0x0b, 0x4f, // a[0] = 11
            0x2a, 0x04, 0x10, 0x16, 0x4f, // a[1] = 22
            0x2a, 0x03, 0x2e, // a[0]
            0x2a, 0x04, 0x2e, // a[1]
            0x60, 0xac, // iadd; ireturn
        ],
        1,
    );
    let f = compile(&c, "([I)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let arr = vm.heap.alloc_array("int", 2);
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![pack_local(&JVal::Array(arr))];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) } as i32;
    set_current_vm(prev);

    assert_eq!(got, 33);
    assert_eq!(vm.heap.array_get(arr, 0).and_then(|v| v.as_int()), Some(11));
    assert_eq!(vm.heap.array_get(arr, 1).and_then(|v| v.as_int()), Some(22));
    assert!(!vm.jit_pending_is_set());
}

#[test]
fn jit_native_iaload_reads_and_faults() {
    // int f(int[] a, int i){ return a[i]; }  aload_0; iload_1; iaload; ireturn
    let c = code(vec![0x2a, 0x1b, 0x2e, 0xac], 2);
    let f = compile(&c, "([II)I", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let arr = vm.heap.alloc_array("int", 3);
    vm.heap.array_set(arr, 1, JVal::Int(77));
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);

    let mut inb = vec![pack_local(&JVal::Array(arr)), 1];
    let got = unsafe { f.call_desc(&mut inb, desc_cell) } as i32;

    let mut oob = vec![pack_local(&JVal::Array(arr)), 5];
    let oobr = unsafe { f.call_desc(&mut oob, desc_cell) } as i32;
    let oob_pending = vm.jit_pending_take();

    let mut neg = vec![pack_local(&JVal::Array(arr)), (-1i64) as i64];
    let negr = unsafe { f.call_desc(&mut neg, desc_cell) } as i32;
    let neg_pending = vm.jit_pending_take();

    let mut nul = vec![0i64, 0];
    let nulr = unsafe { f.call_desc(&mut nul, desc_cell) } as i32;
    let null_pending = vm.jit_pending_take();

    // aload_0; iload_1; iaload; pop; iconst_5; ireturn. A fault must
    // complete the compiled method immediately rather than reaching 5.
    let after_fault = compile(
        &code(vec![0x2a, 0x1b, 0x2e, 0x57, 0x08, 0xac], 2),
        "([II)I",
        &empty_pool(),
        0,
        &mut |_, _| (0, None),
    )
    .expect("compiled");
    let mut post = vec![pack_local(&JVal::Array(arr)), 9];
    let postr = unsafe { after_fault.call_desc(&mut post, desc_cell) } as i32;
    let post_pending = vm.jit_pending_take();
    set_current_vm(prev);

    assert_eq!(got, 77, "in-bounds native load");
    assert_eq!(oobr, 0);
    assert!(matches!(oob_pending, Some(JVal::Ref(_))), "OOB → AIOOBE");
    assert_eq!(negr, 0);
    assert!(matches!(neg_pending, Some(JVal::Ref(_))), "negative idx → AIOOBE");
    assert_eq!(nulr, 0);
    assert!(matches!(null_pending, Some(JVal::Ref(_))), "null → NPE");
    assert_eq!(postr, 0, "fault must bypass later bytecode");
    assert!(matches!(post_pending, Some(JVal::Ref(_))), "post-fault → AIOOBE");
}

#[test]
fn jit_native_typed_loads_read_correctly() {
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);

    // baload: byte sign-extends. a[0]=-7 (0xF9). aload_0; iconst_0; baload; ireturn
    let bc = vm.heap.alloc_array("byte", 1);
    vm.heap.array_set(bc, 0, JVal::Int(-7));
    let fb = compile(&code(vec![0x2a, 0x03, 0x33, 0xac], 1), "([B)I", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let mut lb = vec![pack_local(&JVal::Array(bc))];
    assert_eq!(unsafe { fb.call_desc(&mut lb, desc_cell) } as i32, -7, "baload sign-extends");

    // laload: aload_0; iconst_0; laload; lreturn
    let lc = vm.heap.alloc_array("long", 1);
    vm.heap.array_set(lc, 0, JVal::Long(0x1_2345_6789));
    let fl = compile(&code(vec![0x2a, 0x03, 0x2f, 0xad], 1), "([J)J", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let mut ll = vec![pack_local(&JVal::Array(lc))];
    assert_eq!(unsafe { fl.call_desc(&mut ll, desc_cell) }, 0x1_2345_6789);

    // daload: aload_0; iconst_0; daload; dreturn
    let dc = vm.heap.alloc_array("double", 1);
    vm.heap.array_set(dc, 0, JVal::Double(2.5));
    let fd = compile(&code(vec![0x2a, 0x03, 0x31, 0xaf], 1), "([D)D", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let mut ld = vec![pack_local(&JVal::Array(dc))];
    assert_eq!(f64::from_bits(unsafe { fd.call_desc(&mut ld, desc_cell) } as u64), 2.5);

    // faload: aload_0; iconst_0; faload; freturn
    let fc = vm.heap.alloc_array("float", 1);
    vm.heap.array_set(fc, 0, JVal::Float(1.5));
    let ff = compile(&code(vec![0x2a, 0x03, 0x30, 0xae], 1), "([F)F", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let mut lf = vec![pack_local(&JVal::Array(fc))];
    assert_eq!(f32::from_bits(unsafe { ff.call_desc(&mut lf, desc_cell) } as u32), 1.5);

    set_current_vm(prev);
}

#[test]
fn jit_putfield_writes_through_callback() {
    // int f(P p, int v){ p.x = v; return p.x; }
    //  aload_0; iload_1; putfield #6; aload_0; getfield #6; ireturn
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("P".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("x".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    let c = code(vec![0x2a, 0x1b, 0xb5, 0x00, 0x06, 0x2a, 0xb4, 0x00, 0x06, 0xac], 2);
    let f = compile(&c, "(LP;I)I", &pool, 0, &mut |_, _| (0, None)).expect("putfield method compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let p = vm.heap.alloc("P");
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![pack_local(&JVal::Ref(p)), 42];
    // putfield stores at field-id 0 (the test intern); getfield reads it back
    // (native, via the object descriptor) — proving the write + native read.
    let got = unsafe { f.call_desc(&mut locals, desc_cell) } as i32;
    set_current_vm(prev);
    assert_eq!(got, 42, "putfield then native getfield round-trips");
}

#[test]
fn jit_native_typed_stores_write_correctly() {
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);

    // bastore: void f(byte[] a){ a[0] = -5; }  aload_0; iconst_0; bipush -5; bastore; return
    let ba = vm.heap.alloc_array("byte", 1);
    let fb = compile(&code(vec![0x2a, 0x03, 0x10, 0xfb, 0x54, 0xb1], 1), "([B)V", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let mut lb = vec![pack_local(&JVal::Array(ba))];
    unsafe { fb.call_desc(&mut lb, desc_cell) };
    assert_eq!(vm.heap.array_get(ba, 0).and_then(|v| v.as_int()), Some(-5), "bastore");

    // dastore: void f(double[] a){ a[0] = 2.0 + 1.0; }  aload_0; iconst_0; dconst_1; dconst_1; dadd; ... use 1.0+1.0
    // aload_0; iconst_0; dconst_1; dconst_1; dadd; dastore; return
    let da = vm.heap.alloc_array("double", 1);
    let fd = compile(&code(vec![0x2a, 0x03, 0x0f, 0x0f, 0x63, 0x52, 0xb1], 1), "([D)V", &empty_pool(), 0, &mut |_, _| (0, None)).unwrap();
    let mut ld = vec![pack_local(&JVal::Array(da))];
    unsafe { fd.call_desc(&mut ld, desc_cell) };
    match vm.heap.array_get(da, 0) {
        Some(JVal::Double(v)) => assert!((v - 2.0).abs() < 1e-12, "dastore got {v}"),
        other => panic!("dastore: {other:?}"),
    }

    set_current_vm(prev);
}

#[test]
fn jit_iastore_oob_sets_pending_and_suppresses_later_store() {
    let c = code(
        vec![
            0x2a, 0x04, 0x10, 0x09, 0x4f, // a[1] = 9; OOB for len 1
            0x2a, 0x03, 0x10, 0x63, 0x4f, // a[0] = 99; must no-op after pending
            0xb1,
        ],
        1,
    );
    let f = compile(&c, "([I)V", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let arr = vm.heap.alloc_array("int", 1);
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![pack_local(&JVal::Array(arr))];
    unsafe { f.call_desc(&mut locals, desc_cell) };
    set_current_vm(prev);

    // The OOB store at index 1 faults; the in-bounds a[0]=99 that follows must
    // be suppressed by the latched fault, leaving the slot at 0.
    assert_eq!(vm.heap.array_get(arr, 0).and_then(|v| v.as_int()), Some(0));
    match vm.jit_pending_take() {
        // AIOOBE carries both the offending index and the array length.
        Some(JVal::Ref(h)) => assert!(
            matches!(vm.heap.get_field(h, "$message"), JVal::Str(ref s)
                if s.as_ref() == "Index 1 out of bounds for length 1")
        ),
        other => panic!("expected pending ArrayIndexOutOfBoundsException, got {other:?}"),
    }
}

#[test]
fn jit_dup2_dastore_updates_double_array() {
    let c = code(
        vec![
            0x2a, // aload_0
            0x03, // iconst_0
            0x5c, // dup2: keep array/index for the eventual dastore
            0x31, // daload
            0x0f, // dconst_1
            0x63, // dadd
            0x52, // dastore
            0xb1, // return
        ],
        1,
    );
    let f = compile(&c, "([D)V", &empty_pool(), 0, &mut |_, _| (0, None)).expect("compiled");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let arr = vm.heap.alloc_array("double", 1);
    vm.heap.array_set(arr, 0, JVal::Double(2.5));
    let desc_cell = vm.heap.array_desc_cell_ptr();

    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals = vec![pack_local(&JVal::Array(arr))];
    unsafe { f.call_desc(&mut locals, desc_cell) };
    set_current_vm(prev);

    match vm.heap.array_get(arr, 0) {
        Some(JVal::Double(v)) => assert!((v - 3.5).abs() < 1e-12),
        other => panic!("expected updated double, got {other:?}"),
    }
    assert!(!vm.jit_pending_is_set());
}
