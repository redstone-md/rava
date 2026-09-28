//! Arena allocation: inline new, the trivial-ctor plan, and SROA over a confined object.

use super::*;

/// INLINE TLAB BUMP: a compiled `new` of an arena-eligible class allocates with
/// ZERO `new` callback. Compiles `O f(){ return new O(); }` (new #2; areturn)
/// with a `new_arena` entry for cp #2, runs it on a fresh TLAB, and asserts:
/// the `new` callback counter does NOT move (honest 0 — the design's headline),
/// the result is a low-address raw `ArenaPtr`, and its inline-initialised header
/// (class_id + nfields) and zeroed cells match a TLAB-allocated object exactly.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_new_fires_no_callback() {
    use std::sync::Arc;
    // Arena class "O" with two int fields → nfields = 2, obj_size = HDR + 16.
    let name: Arc<str> = Arc::from("O");
    let layout: Arc<[(u32, u8)]> = vec![(0u32, 0u8), (1, 0)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    // static O f(){ return new O(); }  →  new #2 ; areturn
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("O".into()),
        Constant::Class { name_index: 1 },
    ]);
    let c = code(vec![0xbb, 0x00, 0x02, 0xb0], 1);
    let mut new_arena: std::collections::HashMap<u16, (u32, u32)> =
        std::collections::HashMap::new();
    new_arena.insert(2, (cid, 2));
    let f = compile_with_static_intrinsics(
        &c,
        "()LO;",
        &pool,
        0,
        &mut |_, _| (0, None),
        &std::collections::HashMap::new(),
        &no_inline(),
        &no_native(),
        &no_statics(),
        &new_arena,
        &no_ctor_inlines(),
        &no_concat_shapes(),
        &no_hof_fusions(),
        &no_getter_inlines(),
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("inline-new method compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let before = tl_cb(3); // 3 = new (thread-local, race-free)
    // Buffer spans max_locals + max_stack + the GC spill area (as jit_run sizes it).
    let mut locals: Vec<i64> = vec![0; 8];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) };
    let after = tl_cb(3);
    set_current_vm(prev);

    assert_eq!(after, before, "new callback must NOT fire — the alloc was inlined");
    let raw = got as u64;
    assert!(raw != 0 && raw >> 40 == 0, "result is a low-address raw ArenaPtr");
    assert_eq!(raw & 7, 0, "arena object is 8-aligned");
    let p = unsafe { crate::arena::ArenaPtr::from_addr(raw) };
    assert_eq!(crate::nextgen::arena_class_id(p), cid, "header class_id inlined");
    assert_eq!(crate::nextgen::arena_nfields(p), 2, "header nfields inlined");
    assert_eq!(crate::nextgen::arena_cell(p, 0), 0, "cell 0 zero-initialised");
    assert_eq!(crate::nextgen::arena_cell(p, 1), 0, "cell 1 zero-initialised");
}

/// CONSTRUCTOR INLINER analyzer: a Point-like `<init>(III)V` (super
/// Object.<init> + three `aload_0; iload_n; putfield` triples + return) is
/// recognised as trivial, and the extracted plan maps each arg to its field in
/// order. A non-trivial body (a branch op) is rejected.
#[cfg(feature = "next-gen-gc")]
#[test]
fn analyze_trivial_ctor_extracts_field_setter_plan() {
    // 1 Pt / 2 Class(1) / 3 Object / 4 Class(3) / 5 <init> / 6 ()V /
    // 7 NaT(5,6) / 8 MethodRef Object.<init> / 9 x / 10 I / 11 NaT(9,10) /
    // 12 FieldRef Pt.x / 13 y / 14 NaT(13,10) / 15 FieldRef Pt.y /
    // 16 z / 17 NaT(16,10) / 18 FieldRef Pt.z
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("Pt".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("java/lang/Object".into()),
        Constant::Class { name_index: 3 },
        Constant::Utf8("<init>".into()),
        Constant::Utf8("()V".into()),
        Constant::NameAndType { name_index: 5, desc_index: 6 },
        Constant::MethodRef { class_index: 4, nat_index: 7 },
        Constant::Utf8("x".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 9, desc_index: 10 },
        Constant::FieldRef { class_index: 2, nat_index: 11 },
        Constant::Utf8("y".into()),
        Constant::NameAndType { name_index: 13, desc_index: 10 },
        Constant::FieldRef { class_index: 2, nat_index: 14 },
        Constant::Utf8("z".into()),
        Constant::NameAndType { name_index: 16, desc_index: 10 },
        Constant::FieldRef { class_index: 2, nat_index: 17 },
    ]);
    // aload_0; invokespecial #8; (aload_0; iload_n; putfield #f)x3; return
    let bc = vec![
        0x2a, 0xb7, 0x00, 0x08, 0x2a, 0x1b, 0xb5, 0x00, 0x0c, 0x2a, 0x1c, 0xb5, 0x00, 0x0f,
        0x2a, 0x1d, 0xb5, 0x00, 0x12, 0xb1,
    ];
    let plan = analyze_trivial_ctor(&bc, &pool, "(III)V", false).expect("trivial");
    assert_eq!(plan.len(), 3);
    let got: Vec<(usize, &str, JT)> =
        plan.iter().map(|s| (s.arg_index, s.field_name.as_str(), s.jt)).collect();
    assert_eq!(
        got,
        vec![(0, "x", JT::Int), (1, "y", JT::Int), (2, "z", JT::Int)],
        "each arg maps to its field in order"
    );
    for s in &plan {
        assert_eq!(s.field_owner, "Pt");
    }
    // A branch op (goto) anywhere → not trivial.
    let mut nontrivial = bc.clone();
    nontrivial.insert(4, 0xa7); // goto — breaks the pure setter shape
    assert!(
        analyze_trivial_ctor(&nontrivial, &pool, "(III)V", false).is_none(),
        "a branch makes the constructor non-trivial"
    );
    // Exception handlers → not trivial.
    assert!(analyze_trivial_ctor(&bc, &pool, "(III)V", true).is_none());
}

/// CONSTRUCTOR INLINER end-to-end: `Pt f(){ return new Pt(1,7,3); }` compiles
/// the `new` to an inline bump AND the trivial `<init>` to inline field stores —
/// NEITHER the `new` NOR the `invoke` callback fires, and the object's cells hold
/// the constructor args. The last per-allocation callback on value-class
/// construction, gone.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_ctor_fires_no_invoke_callback() {
    use std::sync::Arc;
    let name: Arc<str> = Arc::from("Pt2");
    let layout: Arc<[(u32, u8)]> = vec![(100u32, 0u8), (101, 0), (102, 0)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    // static Pt2 f(){ return new Pt2(1,7,3); }
    // new #2; dup; iconst_1; bipush 7; iconst_3; invokespecial #6; areturn
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("Pt2".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("<init>".into()),
        Constant::Utf8("(III)V".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::MethodRef { class_index: 2, nat_index: 5 },
    ]);
    let c = code(
        vec![0xbb, 0x00, 0x02, 0x59, 0x04, 0x10, 0x07, 0x06, 0xb7, 0x00, 0x06, 0xb0],
        1,
    );
    let mut new_arena = std::collections::HashMap::new();
    new_arena.insert(2u16, (cid, 3u32));
    let mut ctor_inlines = std::collections::HashMap::new();
    ctor_inlines.insert(
        6u16,
        CtorInline {
            stores: vec![(0, 0, 100, JT::Int), (1, 1, 101, JT::Int), (2, 2, 102, JT::Int)],
            n_params: 3,
        },
    );
    let f = compile_with_static_intrinsics(
        &c,
        "()LPt2;",
        &pool,
        0,
        &mut |_, _| (0, None),
        &std::collections::HashMap::new(),
        &no_inline(),
        &no_native(),
        &no_statics(),
        &new_arena,
        &ctor_inlines,
        &no_concat_shapes(),
        &no_hof_fusions(),
        &no_getter_inlines(),
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("inline-ctor method compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let inv_before = tl_cb(0); // 0 = invoke
    let new_before = tl_cb(3); // 3 = new
    let mut locals: Vec<i64> = vec![0; 16];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) };
    let inv_after = tl_cb(0);
    let new_after = tl_cb(3);
    set_current_vm(prev);

    assert_eq!(inv_after, inv_before, "invoke callback must NOT fire — <init> inlined");
    assert_eq!(new_after, new_before, "new callback must NOT fire — alloc inlined");
    let raw = got as u64;
    assert!(raw != 0 && raw >> 40 == 0, "result is a low-address arena ptr");
    let p = unsafe { crate::arena::ArenaPtr::from_addr(raw) };
    assert_eq!(crate::nextgen::arena_cell(p, 0), 1, "field x inline-stored = 1");
    assert_eq!(crate::nextgen::arena_cell(p, 1), 7, "field y inline-stored = 7");
    assert_eq!(crate::nextgen::arena_cell(p, 2), 3, "field z inline-stored = 3");
}

/// SROA MATERIALIZATION e2e: a data-parallel loop that creates a CONFINED object
/// per iteration (`Pt p = new Pt(i, base); sum += p.a + p.b`). The body compiles
/// to the worker fn with the object SCALAR-REPLACED — `new`/`<init>`/`getfield`
/// route to field registers, NO arena allocation. Correctness proves the field
/// mapping; that it forks an alloc-shaped body without overflowing the worker
/// nursery proves the allocation is physically erased.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_sroa_confined_object_in_parallel_loop_is_scalar_replaced() {
    use std::sync::Arc;
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Arena class Pt with int fields a(id100,slot0), b(id101,slot1).
    let name: Arc<str> = Arc::from("PtSroa");
    let layout: Arc<[(u32, u8)]> = vec![(100u32, 0u8), (101, 0)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    // long hot(int n, int base){ long sum=0; for(i=0;i<n;i++){ Pt p=new Pt(i,base);
    //   sum += p.a + p.b; } return sum; }
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("PtSroa".into()),                          // 1
        Constant::Class { name_index: 1 },                        // 2
        Constant::Utf8("<init>".into()),                          // 3
        Constant::Utf8("(II)V".into()),                           // 4
        Constant::NameAndType { name_index: 3, desc_index: 4 },    // 5
        Constant::MethodRef { class_index: 2, nat_index: 5 },      // 6  Pt.<init>(II)V
        Constant::Utf8("a".into()),                               // 7
        Constant::Utf8("I".into()),                               // 8
        Constant::NameAndType { name_index: 7, desc_index: 8 },    // 9
        Constant::FieldRef { class_index: 2, nat_index: 9 },       // 10 Pt.a:I
        Constant::Utf8("b".into()),                               // 11
        Constant::NameAndType { name_index: 11, desc_index: 8 },   // 12
        Constant::FieldRef { class_index: 2, nat_index: 12 },      // 13 Pt.b:I
    ]);
    let c = code(
        vec![
            0x09, 0x41, 0x03, 0x36, 0x04, // sum=0; i=0
            0x15, 0x04, 0x1a, 0xa2, 0x00, 0x24, // header: iload i; iload n; if_icmpge +36
            0xbb, 0x00, 0x02, 0x59, // new Pt; dup
            0x15, 0x04, 0x1b, // iload i; iload_1 base
            0xb7, 0x00, 0x06, // invokespecial Pt.<init>(II)V
            0x3a, 0x05, // astore p (local 5)
            0x20, // lload_2 (load acc)
            0x19, 0x05, 0xb4, 0x00, 0x0a, // aload p; getfield a
            0x19, 0x05, 0xb4, 0x00, 0x0d, // aload p; getfield b
            0x60, 0x85, 0x61, 0x41, // iadd; i2l; ladd; lstore_2
            0x84, 0x04, 0x01, // iinc i,1
            0xa7, 0xff, 0xdc, // goto header (-36)
            0x20, 0xad, // lload_2; lreturn
        ],
        6,
    );
    let mut new_arena = std::collections::HashMap::new();
    new_arena.insert(2u16, (cid, 2u32));
    let mut ctor_inlines = std::collections::HashMap::new();
    ctor_inlines.insert(
        6u16,
        CtorInline { stores: vec![(0, 0, 100, JT::Int), (1, 1, 101, JT::Int)], n_params: 2 },
    );
    let mut intern = |owner: &str, fname: &str| -> (u32, Option<u32>) {
        match (owner, fname) {
            ("PtSroa", "a") => (100, Some(0)),
            ("PtSroa", "b") => (101, Some(1)),
            _ => (0, None),
        }
    };

    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile_with_static_intrinsics(
        &c, "(II)J", &pool, 0, &mut intern,
        &std::collections::HashMap::new(), &no_inline(), &no_native(), &no_statics(),
        &new_arena, &ctor_inlines, &no_concat_shapes(), &no_hof_fusions(), &no_getter_inlines(), None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    );
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("SROA data-parallel loop compiles");

    let serial_ref = |n: i32, base: i32| -> i64 {
        let mut sum = 0i64;
        for i in 0..n {
            sum = sum.wrapping_add(i.wrapping_add(base) as i64); // p.a + p.b = i + base
        }
        sum
    };
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let run = |n: i32, base: i32| -> i64 {
        let mut locals = vec![n as i64, base as i64, 0, 0, 0, 0, 0, 0];
        unsafe { f.call_desc(&mut locals, desc_cell) }
    };
    // ≥10k → fork (alloc body, scalar-replaced → no nursery overflow); <10k → serial.
    for (n, base) in [(50_000i32, 7i32), (30_000, -5), (5_000, 9), (0, 3)] {
        assert_eq!(run(n, base), serial_ref(n, base), "SROA fork/serial for n={n} base={base}");
    }
    set_current_vm(prev);
    crate::parallel::clear_abort();
}

/// SYNERGY e2e (Layers 1 + 2 of the inline → SROA → fork pipeline): a parallel
/// loop creating a confined object and reading it through TRIVIAL GETTERS
/// (`p.getA() + p.getB()`, not direct field reads). The getter calls inline to
/// field-register reads of the scalar-replaced object — no allocation, no call,
/// no Rust-runtime crossing — and the loop forks. Proves byte-exactness for both
/// the fork (n>=10k) and serial (n<10k) paths.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_synergy_getter_inline_plus_sroa_in_parallel_loop() {
    use std::sync::Arc;
    let _g = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let name: Arc<str> = Arc::from("PtGetSyn");
    let layout: Arc<[(u32, u8)]> = vec![(100u32, 0u8), (101, 0)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    // long hot(int n, int base){ long sum=0; for(i){ Pt p=new Pt(i,base);
    //   sum += p.getA() + p.getB(); } } — getA/getB are trivial getters.
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("PtGetSyn".into()),                        // 1
        Constant::Class { name_index: 1 },                        // 2
        Constant::Utf8("<init>".into()),                          // 3
        Constant::Utf8("(II)V".into()),                           // 4
        Constant::NameAndType { name_index: 3, desc_index: 4 },    // 5
        Constant::MethodRef { class_index: 2, nat_index: 5 },      // 6  <init>
        Constant::Utf8("getA".into()),                            // 7
        Constant::Utf8("()I".into()),                             // 8
        Constant::NameAndType { name_index: 7, desc_index: 8 },    // 9
        Constant::MethodRef { class_index: 2, nat_index: 9 },      // 10 getA()I
        Constant::Utf8("getB".into()),                            // 11
        Constant::NameAndType { name_index: 11, desc_index: 8 },   // 12
        Constant::MethodRef { class_index: 2, nat_index: 12 },     // 13 getB()I
    ]);
    let c = code(
        vec![
            0x09, 0x41, 0x03, 0x36, 0x04,
            0x15, 0x04, 0x1a, 0xa2, 0x00, 0x24, // header
            0xbb, 0x00, 0x02, 0x59, 0x15, 0x04, 0x1b, 0xb7, 0x00, 0x06, 0x3a, 0x05, // new+ctor+astore
            0x20, // lload_2 (acc)
            0x19, 0x05, 0xb6, 0x00, 0x0a, // aload p; invokevirtual getA
            0x19, 0x05, 0xb6, 0x00, 0x0d, // aload p; invokevirtual getB
            0x60, 0x85, 0x61, 0x41, // iadd; i2l; ladd; lstore_2
            0x84, 0x04, 0x01, 0xa7, 0xff, 0xdc, // iinc; goto
            0x20, 0xad,
        ],
        6,
    );
    let mut new_arena = std::collections::HashMap::new();
    new_arena.insert(2u16, (cid, 2u32));
    let mut ctor_inlines = std::collections::HashMap::new();
    ctor_inlines.insert(
        6u16,
        CtorInline { stores: vec![(0, 0, 100, JT::Int), (1, 1, 101, JT::Int)], n_params: 2 },
    );
    let mut getter_inlines = std::collections::HashMap::new();
    getter_inlines.insert(10u16, (100u32, JT::Int)); // getA → field 100 (a)
    getter_inlines.insert(13u16, (101u32, JT::Int)); // getB → field 101 (b)

    std::env::set_var("RAVA_JIT_PARALLEL", "1");
    let f = compile_with_static_intrinsics(
        &c, "(II)J", &pool, 0, &mut |_, _| (0, None),
        &std::collections::HashMap::new(), &no_inline(), &no_native(), &no_statics(),
        &new_arena, &ctor_inlines, &no_concat_shapes(), &no_hof_fusions(), &getter_inlines, None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    );
    std::env::remove_var("RAVA_JIT_PARALLEL");
    let f = f.expect("getter+SROA data-parallel loop compiles");

    let serial_ref = |n: i32, base: i32| -> i64 {
        let mut sum = 0i64;
        for i in 0..n {
            sum = sum.wrapping_add(i.wrapping_add(base) as i64); // getA()+getB() = i+base
        }
        sum
    };
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let run = |n: i32, base: i32| -> i64 {
        let mut locals = vec![n as i64, base as i64, 0, 0, 0, 0, 0, 0];
        unsafe { f.call_desc(&mut locals, desc_cell) }
    };
    for (n, base) in [(50_000i32, 11i32), (30_000, -8), (5_000, 4), (0, 6)] {
        assert_eq!(run(n, base), serial_ref(n, base), "getter+SROA fork/serial n={n} base={base}");
    }
    set_current_vm(prev);
    crate::parallel::clear_abort();
}

/// FIELDLESS INLINE TLAB BUMP — the `new Object()` shape, the one the boot-price map
/// names as the allocation target ("finding what compiled `new` emits for a fieldless
/// class"). `nfields = 0` is the boundary of the emit loop: `obj_size = HDR` exactly,
/// no cell-wipe stores after the header, and the header tail must still read zero.
/// Compiles `E f(){ return new E(); }` with a zero-field `new_arena` entry, runs it,
/// and asserts the same contract the two-field test does: no `new` callback, a raw
/// low-address 8-aligned `ArenaPtr`, a header whose class/nfields read back as
/// (cid, 0), and a header tail that reads zero — the state `obj_init_new` leaves.
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_new_fieldless_zero_fields() {
    use std::sync::Arc;
    // A fieldless class "E" → nfields = 0, obj_size = HDR exactly.
    let name: Arc<str> = Arc::from("E");
    let layout: Arc<[(u32, u8)]> = vec![].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    // static E f(){ return new E(); }  →  new #2 ; areturn
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("E".into()),
        Constant::Class { name_index: 1 },
    ]);
    let c = code(vec![0xbb, 0x00, 0x02, 0xb0], 1);
    let mut new_arena: std::collections::HashMap<u16, (u32, u32)> =
        std::collections::HashMap::new();
    new_arena.insert(2, (cid, 0));
    let f = compile_with_static_intrinsics(
        &c,
        "()LE;",
        &pool,
        0,
        &mut |_, _| (0, None),
        &std::collections::HashMap::new(),
        &no_inline(),
        &no_native(),
        &no_statics(),
        &new_arena,
        &no_ctor_inlines(),
        &no_concat_shapes(),
        &no_hof_fusions(),
        &no_getter_inlines(),
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("fieldless inline-new method compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    let before = tl_cb(3); // 3 = new (thread-local, race-free)
    let mut locals: Vec<i64> = vec![0; 8];
    let got = unsafe { f.call_desc(&mut locals, desc_cell) };
    let after = tl_cb(3);
    set_current_vm(prev);

    assert_eq!(after, before, "new callback must NOT fire — the alloc was inlined");
    let raw = got as u64;
    assert!(raw != 0 && raw >> 40 == 0, "result is a low-address raw ArenaPtr");
    assert_eq!(raw & 7, 0, "arena object is 8-aligned");
    let p = unsafe { crate::arena::ArenaPtr::from_addr(raw) };
    assert_eq!(crate::nextgen::arena_class_id(p), cid, "header class_id inlined");
    assert_eq!(crate::nextgen::arena_nfields(p), 0, "header nfields is the boundary 0");
    // The mutable header tail ([8, HDR)) must read zero even with no cells to wipe.
    assert_eq!(crate::nextgen::arena_cell(p, 0), 0, "first word past the header is zero");
    // Distinct successive births: a second inline bump must not overlap the first
    // (obj_size(HDR) exactly — the boundary where an off-by-HDR would alias).
    let prev2 = set_current_vm(&mut vm as *mut crate::Vm);
    let mut locals2: Vec<i64> = vec![0; 8];
    let got2 = unsafe { f.call_desc(&mut locals2, desc_cell) };
    set_current_vm(prev2);
    let raw2 = got2 as u64;
    assert!(raw2 != raw, "two fieldless bumps are distinct objects");
    let hdr = crate::nextgen::HDR as u64;
    assert!(raw2 >= raw + hdr, "no overlap: {raw2:#x} < {raw:#x}+{hdr}");
}

/// OVERFLOW → SAFEPOINT → RETRY: the inline bump's nursery-full arm. The compiled
/// loop allocates a one-field object per iteration, kept live in local 3 across the
/// loop body, against a nursery driven nearly full BEFORE entry — so the FIRST
/// inline bump already overflows and the loop can only make progress through the
/// designed slow path: spill roots → `rava_arena_safepoint` (native Cheney collect;
/// `tlab_collect` resets `young.top` and republishes the cursor to the freed
/// nursery) → reload + retry via the machine back-edge. The test buffer is
/// registered with the collector exactly as `Vm::jit_enter` registers a
/// non-self-registering body's frame (`frames::cells_push`), so the rooted local is
/// FORWARDED by the collection rather than left stale. Asserts: the loop terminates
/// with the right sum (the bounded-exit guard rules out the spin), the safepoint
/// fired at least once (the overflow arm was really taken), the `new` callback never
/// fired (overflow is served by the safepoint, NOT by falling back to `rava_new`),
/// and the surviving rooted object was moved (the cursor's new frontier is below the
/// pre-collect top — the nursery was actually reclaimed).
#[cfg(feature = "next-gen-gc")]
#[test]
fn jit_inline_arena_new_overflow_safepoint_retry_forwards_roots() {
    use std::sync::Arc;
    let name: Arc<str> = Arc::from("Ov");
    let layout: Arc<[(u32, u8)]> = vec![(0u32, 0u8)].into();
    let cid = crate::nextgen::register_class(&name, &layout);

    // static long f(int n){ long sum=0; Ov o=null;
    //   for (int i=0;i<n;i++){ o=new Ov(); o.f=i; sum+=o.f; } return sum; }
    // Locals: 0=n, 1..2=sum (long), 3=o (ref), 4=i. Field "f" interns to fid 0 → slot 0.
    let pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("Ov".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("f".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    let bc: Vec<u8> = vec![
        0x0e, // lconst_0            (pc0)
        0x40, // lstore_1            (pc1)
        0x01, // aconst_null          (pc2)
        0x4e, // astore_3             (pc3)
        0x03, // iconst_0            (pc4)
        0x36, 0x04, // istore 4        (pc5)
        // L (pc7):
        0x15, 0x04, // iload 4         (pc7)
        0x15, 0x00, // iload 0         (pc9)
        0xa2, 0x00, 0x1b, // if_icmpge EXIT (pc11→pc38)
        0xbb, 0x00, 0x02, // new #2 — INLINE BUMP (pc14)
        0x59, // dup                  (pc17)
        0x4e, // astore_3 — root in local 3 (pc18)
        0x15, 0x04, // iload 4         (pc19)
        0xb5, 0x00, 0x06, // putfield #6 (pc21)
        0x2d, // aload_3              (pc24)
        0xb4, 0x00, 0x06, // getfield #6 (pc25)
        0x85, // i2l                  (pc28)
        0x1f, // lload_1              (pc29)
        0x61, // ladd                 (pc30)
        0x40, // lstore_1             (pc31)
        0x84, 0x04, 0x01, // iinc 4, 1 (pc32)
        0xa7, 0xff, 0xe4, // goto L (pc35→pc7, rel −28)
        // EXIT (pc38):
        0x1f, // lload_1              (pc38)
        0xad, // lreturn              (pc39)
    ];
    let c = code(bc, 5);
    let mut new_arena: std::collections::HashMap<u16, (u32, u32)> =
        std::collections::HashMap::new();
    new_arena.insert(2, (cid, 1));
    let f = compile_with_static_intrinsics(
        &c,
        "(I)J",
        &pool,
        0,
        // Field "f" → (interned id, Some(slot)): slot present + verify off
        // ⇒ putfield/getfield emit the bare inline arena store/load.
        &mut |owner: &str, field: &str| {
            assert_eq!((owner, field), ("Ov", "f"), "only the Ov.f field site is asked");
            (0u32, Some(0u32))
        },
        &std::collections::HashMap::new(),
        &no_inline(),
        &no_native(),
        &no_statics(),
        &new_arena,
        &no_ctor_inlines(),
        &no_concat_shapes(),
        &no_hof_fusions(),
        &no_getter_inlines(),
        None,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        empty_prim_arrays(),
    )
    .expect("overflow-arm loop compiles");

    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let desc_cell = vm.heap.array_desc_cell_ptr();
    let prev = set_current_vm(&mut vm as *mut crate::Vm);
    // Drive the nursery nearly full so the FIRST compiled bump overflows: leave
    // exactly two objects' worth of room, then let iteration 3 take the overflow arm.
    let (_base, _top0, cap) = crate::nextgen::young_extent();
    let one = crate::nextgen::obj_size(1);
    crate::nextgen::young_advance_top(cap - 2 * one);
    let (base, top_full, _cap) = crate::nextgen::young_extent();
    let mut locals: Vec<i64> = vec![0; 16];
    locals[0] = 500; // n = 500 iterations: several overflows, each forwarded + retried
    let before_new = tl_cb(3);
    let before_sp = tl_cb(20); // 20 = rava_arena_safepoint
    // Register the frame buffer with the collector exactly as `Vm::jit_enter` does
    // for a non-self-registering body — the loop's rooted local 3 is forwarded by
    // the safepoint collection only because this range is published.
    crate::frames::cells_push((locals.as_ptr(), locals.len()));
    let t0 = std::time::Instant::now();
    let sum = unsafe { f.call_desc(&mut locals, desc_cell) };
    let dt = t0.elapsed();
    crate::frames::cells_pop();
    let after_new = tl_cb(3);
    let after_sp = tl_cb(20);
    set_current_vm(prev);

    assert_eq!(sum, (0..500i64).sum(), "loop computes Σi through the overflow arm");
    assert_eq!(
        after_new, before_new,
        "new callback must not fire even on nursery-full — the safepoint owns overflow"
    );
    assert!(after_sp > before_sp, "the safepoint fired — the overflow arm was taken");
    assert!(dt.as_secs() < 30, "bounded: no safepoint spin (took {dt:?})");
    // The nursery was actually reclaimed: the frontier is back below the pre-run top.
    let (_b2, top_after, _c2) = crate::nextgen::young_extent();
    let _ = base;
    assert!(
        top_after < top_full,
        "collector reclaimed the nursery (top {top_after} < {top_full})"
    );
}
