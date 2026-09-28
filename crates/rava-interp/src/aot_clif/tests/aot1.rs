//! Part 1 of the aot tests.

use super::*;

/// `int f(int n){ int s=0; for(int i=0;i<n;i++) s+=i; return s; }` — local 0=n,
/// 1=s, 2=i. Sum of 0..n-1. Compiled via Context, AOT-roundtripped, run.
#[test]
fn numeric_sum_loop_aot_roundtrip() {
    // 0: iconst_0 ; 1: istore_1 (s=0)
    // 2: iconst_0 ; 3: istore_2 (i=0)
    // 4: iload_2  ; 5: iload_0 ; 6: if_icmpge 19
    // 9: iload_1  ; 10: iload_2 ; 11: iadd ; 12: istore_1
    // 13: iinc 2,1
    // 16: goto 4
    // 19: iload_1 ; 20: ireturn
    let bc: Vec<u8> = vec![
        0x03, 0x3c, // [0]iconst_0, [1]istore_1
        0x03, 0x3d, // [2]iconst_0, [3]istore_2
        0x1c, 0x1a, 0xa2, 0x00, 0x0d, // [4]iload_2, [5]iload_0, [6]if_icmpge +13(→pc19)
        0x1b, 0x1c, 0x60, 0x3c, // [9]iload_1, [10]iload_2, [11]iadd, [12]istore_1
        0x84, 0x02, 0x01, // [13]iinc 2,1
        0xa7, 0xff, 0xf4, // [16]goto -12 (→pc4)
        0x1b, 0xac, // [19]iload_1, [20]ireturn
    ];
    assert!(numeric_eligible(&bc), "eligible");
    let obj = compile_numeric(&bc).expect("compiles to relocatable code");
    // AOT roundtrip: serialize → deserialize → link, exactly like the on-disk cache.
    let bytes = obj.to_bytes();
    let obj2 = CompiledObj::from_bytes(&bytes).expect("decodes");
    let f = obj2.link().expect("links");
    for n in [0i64, 1, 5, 10, 100] {
        let mut locals = [n, 0, 0, 0];
        let expect: i64 = (0..n).sum();
        let got = unsafe { f.run_numeric(locals.as_mut_ptr()) };
        assert_eq!(got, expect, "sum(0..{n})");
    }
}


/// The relocatable AOT path: compile a method calling symbol 0, persist WITH its
/// Abs8 reloc, reload, link_with a resolver mapping symbol 0 → sym_triple, run.
/// Proves callback-carrying code survives the serialize→process-boundary→link cycle.
#[test]
fn relocatable_call_aot_roundtrip() {
    let obj = compile_call_local0(0).expect("compiles with a reloc");
    assert_eq!(obj.relocs().len(), 1, "one Abs8 reloc for the symbolic call");
    let bytes = obj.to_bytes();
    let obj2 = CompiledObj::from_bytes(&bytes).expect("decodes incl. reloc section");
    assert_eq!(obj2.relocs().len(), 1, "reloc survives serialization");
    let resolve = |s: u32| -> usize {
        match s {
            0 => sym_triple as extern "C" fn(i64) -> i64 as usize,
            _ => 0,
        }
    };
    let f = obj2.link_with(&resolve).expect("links with symbol resolution");
    for n in [0i64, 7, -4, 1000] {
        let mut locals = [n, 0, 0, 0];
        assert_eq!(unsafe { f.run_numeric(locals.as_mut_ptr()) }, n * 3, "triple({n})");
    }
}

/// The constant-table path: compile a method reading consts[2], AOT-roundtrip,
/// then link + run with a per-process constant array — proving runtime constants
/// survive serialization as table SLOTS filled at link (not baked in the code).
#[test]
fn constant_table_aot_roundtrip() {
    let obj = compile_const_plus_local0(2).expect("compiles");
    assert!(obj.relocs().is_empty(), "loads-from-args carry no relocs");
    let bytes = obj.to_bytes();
    let f = CompiledObj::from_bytes(&bytes).expect("decodes").link().expect("links");
    // Linker fills the constant table per process (here: slot 2 = a resolved id).
    let consts = [11i64, 22, 100, 33];
    for n in [0i64, 5, -7, 1000] {
        let mut locals = [n, 0, 0, 0];
        let got = unsafe { f.run_const(locals.as_mut_ptr(), consts.as_ptr()) };
        assert_eq!(got, n + 100, "consts[2]+local0 for n={n}");
    }
}

/// Step A1: every AOT symbol resolves to a real (non-null) callback address, and
/// indices are stable/dense (resolve_symbol(i) == symtab[i].1).
#[test]
fn aot_symbol_table_resolves() {
    let t = crate::jit::aot_symtab();
    assert!(!t.is_empty());
    for (i, &(name, addr)) in t.iter().enumerate() {
        assert_ne!(addr, 0, "{name} address is null");
        assert_eq!(resolve_symbol(i as u32), addr, "index {i} ({name}) resolves");
    }
    // Out-of-range → 0 (a bad reloc can't silently bind to a stray address).
    assert_eq!(resolve_symbol(t.len() as u32), 0);
}

/// Step A2: a method carrying a SYMBOLIC constant descriptor (FieldId) round-trips
/// and `link_full` resolves it per-process into the constant array — proving runtime
/// constants persist as descriptors, not baked values. The code reads consts[0]
/// (compile_const_plus_local0), which the resolved descriptor fills.
#[test]
fn symbolic_const_table_link_full() {
    use crate::cnp::{CompiledObj, ConstDesc};
    let base = compile_const_plus_local0(0).expect("compiles");
    let obj = CompiledObj::from_full(
        base.code_vec(),
        Vec::new(),
        vec![ConstDesc::FieldId("net/minecraft/Foo".into(), "bar".into())],
    );
    let bytes = obj.to_bytes();
    let obj2 = CompiledObj::from_bytes(&bytes).expect("decodes incl consts");
    assert_eq!(obj2.consts().len(), 1);
    assert_eq!(obj2.consts()[0], ConstDesc::FieldId("net/minecraft/Foo".into(), "bar".into()));
    // Link, resolving the FieldId descriptor to a per-process id (here fixed 4242).
    let mut resolve_const = |d: &ConstDesc| -> Option<i64> {
        match d {
            ConstDesc::FieldId(o, n) if o == "net/minecraft/Foo" && n == "bar" => Some(4242),
            _ => None,
        }
    };
    let (f, consts) = obj2
        .link_full(&|i| resolve_symbol(i), &mut resolve_const)
        .expect("links + resolves consts");
    assert_eq!(consts, vec![4242]);
    let mut locals = [58i64, 0, 0, 0];
    assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), consts.as_ptr()) }, 58 + 4242);
}


/// Step A3: the `getfield` opcode compiled with BOTH mechanisms — a const-table
/// field_id + an Abs8 call reloc — AOT-roundtripped and linked against a stub
/// symbol. Proves the per-opcode emit threads (recv, resolved field_id, is_ref)
/// correctly through the serialize→link→run cycle. (Runtime correctness against the
/// real rava_getfield is the in-VM differential test at A4.)
#[test]
fn getfield_opcode_aot_roundtrip() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // Emit getfield for a ref field; symbol index 0 (a stub here).
    let (code, relocs) = compile_getfield(0, true).expect("emits getfield");
    assert_eq!(relocs.len(), 1, "one call reloc to the getfield symbol");
    let obj = CompiledObj::from_full(
        code,
        relocs,
        vec![ConstDesc::FieldId("Foo".into(), "bar".into())],
    );
    // Full persist → reload → link_full (stub symbol + FieldId resolved to id 4242).
    let bytes = obj.to_bytes();
    let obj2 = CompiledObj::from_bytes(&bytes).expect("decodes");
    let resolve_sym = |i: u32| -> usize {
        if i == 0 { stub_getfield as extern "C" fn(i64, i64, i64) -> i64 as usize } else { 0 }
    };
    let mut resolve_const = |d: &ConstDesc| -> Option<i64> {
        match d {
            ConstDesc::FieldId(o, n) if o == "Foo" && n == "bar" => Some(4242),
            _ => None,
        }
    };
    let (f, consts) = obj2.link_full(&resolve_sym, &mut resolve_const).expect("links");
    assert_eq!(consts, vec![4242]);
    // recv = locals[0] = 5 → stub(5, 4242, 1) = 5_000_000 + 4242 + 7.
    let mut locals = [5i64, 0, 0, 0];
    let got = unsafe { f.run_const(locals.as_mut_ptr(), consts.as_ptr()) };
    assert_eq!(got, 5_000_000 + 4242 + 7, "recv, field_id, is_ref threaded correctly");
}


/// Step A3.2: invokestatic with argument marshalling — the args buffer + const-table
/// caller_ptr/cp + call reloc composed. AOT-roundtripped + linked against a stub that
/// reads the buffer back, proving the whole invoke shape threads correctly.
#[test]
fn invokestatic_opcode_aot_roundtrip() {
    use crate::cnp::{CompiledObj, ConstDesc};
    let (code, relocs) = compile_invokestatic_2(0, 0 /*static*/).expect("emits invoke");
    assert_eq!(relocs.len(), 1);
    let obj = CompiledObj::from_full(
        code,
        relocs,
        vec![ConstDesc::ClassPtr("Foo".into()), ConstDesc::Imm(99)],
    );
    let bytes = obj.to_bytes();
    let obj2 = CompiledObj::from_bytes(&bytes).expect("decodes");
    let resolve_sym = |i: u32| -> usize {
        if i == 0 { stub_invoke as extern "C" fn(i64, i64, i64, *const i64, i64) -> i64 as usize } else { 0 }
    };
    let mut resolve_const = |d: &ConstDesc| -> Option<i64> {
        match d {
            ConstDesc::ClassPtr(c) if c == "Foo" => Some(0xC1A55),
            ConstDesc::Imm(v) => Some(*v),
            _ => None,
        }
    };
    let (f, consts) = obj2.link_full(&resolve_sym, &mut resolve_const).expect("links");
    assert_eq!(consts, vec![0xC1A55, 99]);
    // args = locals[0..2] = (3, 4) → stub(caller, cp=99, kind=0, [3,4], 2)
    //      = 3_000_000 + 4000 + 99 + 0.
    let mut locals = [3i64, 4, 0, 0];
    let got = unsafe { f.run_const(locals.as_mut_ptr(), consts.as_ptr()) };
    assert_eq!(got, 3_000_000 + 4000 + 99, "args marshalled + cp threaded");
}


/// Step A3.3: the shared call-shape template covers putfield (obj=Local, fid=Const,
/// val=Local, kind=Imm) and new (caller=Const, cp=Const) uniformly — each AOT-
/// roundtripped + linked against a stub, proving the arg composition threads.
#[test]
fn call_shape_putfield_and_new() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // putfield this.f = v : rava_putfield(obj=local0, fid=const0, val=local1, kind=Imm(4))
    {
        let (code, relocs) = compile_call_shape(
            0,
            &[ArgSrc::Local(0), ArgSrc::Const(0), ArgSrc::Local(1), ArgSrc::Imm(4)],
        )
        .unwrap();
        let obj = CompiledObj::from_full(code, relocs, vec![ConstDesc::FieldId("Foo".into(), "f".into())]);
        let o2 = CompiledObj::from_bytes(&obj.to_bytes()).unwrap();
        let rs = |i: u32| if i == 0 { stub_putfield as extern "C" fn(i64, i64, i64, i64) -> i64 as usize } else { 0 };
        let mut rc = |d: &ConstDesc| match d { ConstDesc::FieldId(_, _) => Some(77), _ => None };
        let (f, consts) = o2.link_full(&rs, &mut rc).unwrap();
        let mut locals = [8i64, 9, 0, 0]; // obj=8, val=9
        assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), consts.as_ptr()) }, 8_000_000 + 77_000 + 90 + 4);
    }
    // new Foo : rava_new(caller=const0, cp=const1)
    {
        let (code, relocs) = compile_call_shape(0, &[ArgSrc::Const(0), ArgSrc::Const(1)]).unwrap();
        let obj = CompiledObj::from_full(code, relocs, vec![ConstDesc::ClassPtr("Foo".into()), ConstDesc::Imm(12)]);
        let o2 = CompiledObj::from_bytes(&obj.to_bytes()).unwrap();
        let rs = |i: u32| if i == 0 { stub_new as extern "C" fn(i64, i64) -> i64 as usize } else { 0 };
        let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(5), ConstDesc::Imm(v) => Some(*v), _ => None };
        let (f, consts) = o2.link_full(&rs, &mut rc).unwrap();
        let mut locals = [0i64; 4];
        assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), consts.as_ptr()) }, 5 * 100 + 12);
    }
}


/// Step A4: the unified opcode loop on a real method mixing getfield in a LOOP with
/// arithmetic + control flow:
///   int f(this, n) { int s=0; for(i=0;i<n;i++) s += this.field; return s; }
/// local 0=this(ref), 1=n(int), 2=s, 3=i. Compiled in one pass, AOT-roundtripped,
/// linked against a stub getfield → proves numeric + callback + control-flow compose.
#[test]
fn compile_method_getfield_loop() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // 0: iconst_0 3c? no — locals: 0=this,1=n,2=s,3=i. istore_2 = 0x3d.
    // 0:iconst_0 1:istore_2  2:iconst_0 3:istore_3
    // 4:iload_3 5:iload_1 6:if_icmpge +? →pc23
    // 9:iload_2 10:aload_0 11:getfield #0 14:iadd 15:istore_2
    // 16:iinc 3,1  19:goto 4
    // 23:iload_2 24:ireturn
    let bc: Vec<u8> = vec![
        0x03, 0x3d,             // [0]iconst_0 [1]istore_2 (s=0)
        0x03, 0x3e,             // [2]iconst_0 [3]istore_3 (i=0)
        0x1d, 0x1b, 0xa2, 0x00, 0x10, // [4]iload_3 [5]iload_1 [6]if_icmpge +16(→pc22)
        0x1c, 0x2a, 0xb4, 0x00, 0x00, // [9]iload_2 [10]aload_0 [11]getfield#0
        0x60, 0x3d,             // [14]iadd [15]istore_2
        0x84, 0x03, 0x01,       // [16]iinc 3,1
        0xa7, 0xff, 0xf1,       // [19]goto -15(→pc4)
        0x1c, 0xac,             // [22]iload_2 [23]ireturn
    ];
    let field_of = |cp: u16| -> Option<(String, String, crate::aot_clif::FieldKind)> {
        if cp == 0 { Some(("Foo".into(), "field".into(), crate::aot_clif::FieldKind::Int)) } else { None }
    };
    let method_of = |_cp: u16| -> Option<InvokeInfo> { None }; // no invokes here
    let (code, relocs, consts) = compile_method(&bc, "Bar", 8, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).expect("compiles");
    // consts[0] is the reserved caller ClassPtr; the FieldId follows.
    assert_eq!(consts[0], ConstDesc::ClassPtr("Bar".into()));
    assert!(consts.contains(&ConstDesc::FieldId("Foo".into(), "field".into())));
    let obj = CompiledObj::from_full(code, relocs, consts);
    let o2 = CompiledObj::from_bytes(&obj.to_bytes()).unwrap();
    let rs = |i: u32| if i as usize == crate::jit::aot_symtab().iter().position(|&(n,_)| n=="rava_getfield").unwrap()
        { stub_getfield_const as extern "C" fn(i64,i64,i64)->i64 as usize } else { 0 };
    let mut rc = |d: &ConstDesc| match d {
        ConstDesc::FieldId(_, _) => Some(41),
        ConstDesc::ClassPtr(_) => Some(0), // reserved caller ptr, unused by getfield
        _ => None,
    };
    let (f, cvals) = o2.link_full(&rs, &mut rc).unwrap();
    // field "value" = fid+1 = 42; sum over n iters = 42*n.
    for n in [0i64, 1, 3, 10] {
        let mut locals = [1i64, n, 0, 0]; // non-null receiver; stub ignores its identity
        let got = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) };
        assert_eq!(got, 42 * n, "sum this.field ({n} iters)");
    }
}


/// Step A4-extend: a method with an invoke — `int f(a, b) { return g(a, b); }` where
/// g is invokestatic #5, 2 args, int return. Compiled by the unified loop (invoke
/// branch), AOT-roundtripped, linked against a stub rava_invoke that reads the args
/// buffer. Proves invoke marshalling composes in compile_method.
#[test]
fn compile_method_invoke() {
    use crate::cnp::{CompiledObj, ConstDesc};
    // 0:iload_0 1:iload_1 2:invokestatic #5 5:ireturn
    let bc: Vec<u8> = vec![0x1a, 0x1b, 0xb8, 0x00, 0x05, 0xac];
    let field_of = |_cp: u16| None;
    // Convention: method_of returns count_args+1 (phantom receiver); the invoke
    // branch drops it for invokestatic. So a 2-arg static → ncells 3 → 2 popped.
    let method_of = |cp: u16| -> Option<InvokeInfo> {
        if cp == 5 { Some(InvokeInfo { numeric_leaf: None, object_leaf: None, direct_leaf: None, ncells: 3, kind: 0, ret_void: false, ret_ref: false, ret_wide: false }) } else { None }
    };
    let (code, relocs, consts) = compile_method(&bc, "Bar", 8, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).expect("compiles");
    // consts: [0]=ClassPtr(Bar), then Imm(5) for the cp.
    assert_eq!(consts[0], ConstDesc::ClassPtr("Bar".into()));
    assert!(consts.contains(&ConstDesc::Imm(5)));
    let obj = CompiledObj::from_full(code, relocs, consts);
    let o2 = CompiledObj::from_bytes(&obj.to_bytes()).unwrap();
    let inv_idx = crate::jit::aot_symtab().iter().position(|&(n, _)| n == "rava_invoke").unwrap();
    let rs = |i: u32| if i as usize == inv_idx { stub_invoke_sum as extern "C" fn(i64,i64,i64,*const i64,i64)->i64 as usize } else { crate::jit::aot_symbol_addr(i) };
    let mut rc = |d: &ConstDesc| match d { ConstDesc::ClassPtr(_) => Some(0), ConstDesc::Imm(v) => Some(*v), _ => None };
    let (f, cvals) = o2.link_full(&rs, &mut rc).unwrap();
    // f(a=7, b=9): stub sums cp(5) + buf[7,9] = 5+7+9 = 21.
    let mut locals = [7i64, 9, 0, 0];
    assert_eq!(unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) }, 21);
}

/// putfield in the unified loop: `void f(this, v){ this.field = v; }` compiles (pop
/// val, pop recv, rava_putfield) — proves the write opcode threads via compile_method.
#[test]
fn compile_method_putfield() {
    use crate::cnp::ConstDesc;
    // 0:aload_0 1:iload_1 2:putfield#0 5:return (void — now covered)
    let bc: Vec<u8> = vec![0x2a, 0x1b, 0xb5, 0x00, 0x00, 0xb1];
    let field_of = |cp: u16| if cp == 0 { Some(("Foo".to_string(), "field".to_string(), crate::aot_clif::FieldKind::Int)) } else { None };
    let method_of = |_cp: u16| None;
    // Void setter `this.field = v` now compiles (putfield + return-void covered).
    let (_, _, consts) = compile_method(&bc, "Foo", 8, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).expect("compiles void setter");
    assert!(consts.contains(&ConstDesc::FieldId("Foo".into(), "field".into())));
    // An unresolvable field still fails-closed.
    let bad = |_cp: u16| None;
    assert!(compile_method(&bc, "Foo", 8, &bad, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).is_none(), "unresolved field → None");
}

/// A4-ROUTE in-VM gate (read path): compile_method's getfield output, run in a REAL
/// VM against the REAL rava_getfield callback + a REAL arena object, returns the
/// object's field value — proving the AOT translator's callback path is correct
/// in-process (not just against stubs). Pure getfield (no side effects), so safe.
#[test]
fn getfield_runs_in_vm_against_real_callback() {
    use crate::cnp::{CompiledObj, ConstDesc};
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    // Field id for TestObj#val, and a 1-field arena class whose slot 0 holds it.
    let fid = vm.resolve_field_id("AotGfTest", "val");
    let name: std::sync::Arc<str> = std::sync::Arc::from("AotGfTest");
    let layout: std::sync::Arc<[(u32, u8)]> = vec![(fid, 0u8)].into();
    let cid = crate::nextgen::register_class(&name, &layout);
    let p = crate::nextgen::tlab_try_alloc(cid, 1).expect("arena alloc");
    unsafe { crate::nextgen::set_cell(p.as_ptr(), 0, 12345) };

    // `int f(this){ return this.val; }` : aload_0, getfield #0, ireturn.
    let bc: Vec<u8> = vec![0x2a, 0xb4, 0x00, 0x00, 0xac];
    let field_of = |cp: u16| if cp == 0 { Some(("AotGfTest".to_string(), "val".to_string(), crate::aot_clif::FieldKind::Int)) } else { None };
    let method_of = |_cp: u16| None;
    let (code, relocs, consts) = compile_method(&bc, "AotGfTest", 8, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).expect("compiles");
    let obj = CompiledObj::from_full(code, relocs, consts);
    let mut rc = |d: &ConstDesc| match d {
        ConstDesc::FieldId(o, n) if o == "AotGfTest" && n == "val" => Some(fid as i64),
        ConstDesc::ClassPtr(_) => Some(0),
        _ => None,
    };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).expect("links");

    // Run with the VM current (rava_getfield uses vm_ref()) + the arena obj as
    // locals[0] (packed as its raw pointer, the JIT's Value::Obj cell form).
    let prev = crate::jit::set_current_vm(&mut vm as *mut _);
    let mut locals = [p.as_ptr() as i64, 0, 0, 0];
    let got = unsafe { f.run_const(locals.as_mut_ptr(), cvals.as_ptr()) };
    crate::jit::set_current_vm(prev);
    assert_eq!(got, 12345, "AOT getfield read the real arena field via real rava_getfield");
}


/// Numeric ldc must emit INLINE (rava_ldc is reference-only). `int f(int x){ return
/// 1_000_000 + x; }` via ldc_w of an Integer constant. No callbacks → reloc-free.
/// String ldc must load the INTERNED perm-pool address from the const table (like the
/// JIT), not a fresh alloc per call. `String f(){ return "hello"; }` via ldc_w →
/// StrLit const → intern_arena_string → a stable ArenaString with the right content.
/// Two runs return the SAME address (identity — the whole point of interning).
#[test]
fn string_ldc_interned() {
    use crate::cnp::{CompiledObj, ConstDesc};
    let bc = vec![0x13u8, 0x00, 0x00, 0xb0]; // ldc_w #0, areturn
    let field_of = |_cp: u16| None;
    let method_of = |_cp: u16| None;
    let ldc_of = |cp: u16| if cp == 0 { Some(LdcConst::Str("hello".to_string())) } else { None };
    let (code, relocs, consts) = compile_method(&bc, "S", 4, &field_of, &method_of, &(|_: u16| None), &ldc_of).expect("compiles");
    assert!(consts.contains(&ConstDesc::StrLit("hello".into())), "string ldc → StrLit const");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let mut rc = |d: &ConstDesc| match d {
        ConstDesc::ClassPtr(_) => Some(0),
        ConstDesc::StrLit(s) => Some(crate::nextgen::intern_arena_string(s)),
        _ => None,
    };
    let (f, cvals) = obj.link_full(&|i| resolve_symbol(i), &mut rc).unwrap();
    let mut l1 = [0i64; 4];
    let r1 = unsafe { f.run_const(l1.as_mut_ptr(), cvals.as_ptr()) };
    let mut l2 = [0i64; 4];
    let r2 = unsafe { f.run_const(l2.as_mut_ptr(), cvals.as_ptr()) };
    assert_eq!(r1, r2, "interned string ldc must return the SAME address each call");
    let p = unsafe { crate::arena::ArenaPtr::from_addr(r1 as u64) };
    assert!(crate::nextgen::is_arena_string(p), "returned an ArenaString");
    assert_eq!(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(p)), "hello");
}
