//! AOT Cranelift emission tests.

use super::*;

fn run_ref_across_invoke(bc: &[u8]) -> i64 {
    use crate::cnp::{CompiledObj, ConstDesc};
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    // Data class: 1 int field = 42. Container class C: 1 ref field → the Data obj.
    let dfid = vm.resolve_field_id("Data", "v");
    let dname: std::sync::Arc<str> = std::sync::Arc::from("Data");
    let dlayout: std::sync::Arc<[(u32, u8)]> = vec![(dfid, 0u8)].into();
    let dcid = crate::nextgen::register_class(&dname, &dlayout);
    let dobj = crate::nextgen::tlab_try_alloc(dcid, 1).unwrap();
    unsafe { crate::nextgen::set_cell(dobj.as_ptr(), 0, 42) };

    let cfid = vm.resolve_field_id("C", "field");
    let cname: std::sync::Arc<str> = std::sync::Arc::from("C");
    let clayout: std::sync::Arc<[(u32, u8)]> = vec![(cfid, 6u8)].into(); // kind 6 = ref
    let ccid = crate::nextgen::register_class(&cname, &clayout);
    let cobj = crate::nextgen::tlab_try_alloc(ccid, 1).unwrap();
    // Store the Data ptr as a raw arena cell (encode_arena_cell tags it).
    crate::opcodes::arena_putfield(cobj, cfid, &crate::Value::Obj(dobj));

    let field_of = |cp: u16| if cp == 0 { Some(("C".to_string(), "field".to_string(), crate::aot_clif::FieldKind::Ref)) } else { None };
    let method_of = |cp: u16| if cp == 1 { Some(InvokeInfo { numeric_leaf: None, object_leaf: None, direct_leaf: None, ncells: 1, kind: 0, ret_void: true, ret_ref: false, ret_wide: false }) } else { None };
    let (code, relocs, consts) = compile_method(bc, "C", 4, &field_of, &method_of, &(|_: u16| None), &(|_: u16| Some(crate::aot_clif::LdcConst::Str("x".into())))).expect("compiles");
    let obj = CompiledObj::from_bytes(&CompiledObj::from_full(code, relocs, consts).to_bytes()).unwrap();
    let inv_idx = crate::jit::aot_symtab().iter().position(|&(n, _)| n == "rava_invoke").unwrap();
    let gf_idx = crate::jit::aot_symtab().iter().position(|&(n, _)| n == "rava_getfield").unwrap();
    let rs = |i: u32| -> usize {
        if i as usize == inv_idx { stub_invoke_gc as extern "C" fn(i64, i64, i64, *const i64, i64) -> i64 as usize }
        else if i as usize == gf_idx { crate::jit::aot_symbol_addr(gf_idx as u32) }
        else { crate::jit::aot_symbol_addr(i) }
    };
    let mut rc = |d: &ConstDesc| match d {
        ConstDesc::FieldId(o, n) if o == "C" => Some(vm.resolve_field_id(o, n) as i64),
        ConstDesc::ClassPtr(_) => Some(0),
        ConstDesc::Imm(v) => Some(*v),
        _ => None,
    };
    let (f, cvals) = obj.link_full(&rs, &mut rc).unwrap();
    // locals[0] = the container `this` (raw arena ptr); cells sized max_locals+max_stack.
    let prev = crate::jit::set_current_vm(&mut vm as *mut _);
    crate::frames::register();
    let mut cells = vec![cobj.as_ptr() as i64, 0, 0, 0, 0, 0, 0, 0];
    crate::frames::cells_push((cells.as_ptr(), cells.len()));
    let ret = unsafe { f.run_const(cells.as_mut_ptr(), cvals.as_ptr()) };
    crate::frames::cells_pop();
    crate::frames::unregister();
    crate::jit::set_current_vm(prev);
    assert_ne!(ret, dobj.as_ptr() as i64, "the collection must move the result");
    // The returned ref must point at the (forwarded) Data object with v == 42.
    let p = unsafe { crate::arena::ArenaPtr::from_addr(ret as u64) };
    unsafe { crate::nextgen::cell(p.as_ptr(), 0) }
}

/// Invoke RETURNS a ref (fresh arena obj) → the AOT method must decode+return it
/// intact. Tests the result round-trip (rava_invoke result cell → push → areturn →
/// aot decode), the path the stub-only rooting tests don't cover.
extern "C" fn stub_invoke_ret_ref(_c: i64, _cp: i64, _k: i64, _b: *const i64, _n: i64) -> i64 {
    let vm = crate::jit::vm_ref().unwrap();
    let fid = vm.resolve_field_id("Data", "v");
    let name: std::sync::Arc<str> = std::sync::Arc::from("Data");
    let layout: std::sync::Arc<[(u32, u8)]> = vec![(fid, 0u8)].into();
    let cid = crate::nextgen::register_class(&name, &layout);
    let d = crate::nextgen::tlab_try_alloc(cid, 1).unwrap();
    unsafe { crate::nextgen::set_cell(d.as_ptr(), 0, 99) };
    d.as_ptr() as i64 // raw arena ptr = a Value::Obj cell
}

extern "C" fn sym_triple(x: i64) -> i64 {
    x * 3
}
/// Stub standing in for rava_getfield: `obj*1_000_000 + field_id + is_ref*7`, so the
/// test can assert the emitted call threaded (recv, field_id, is_ref) in the right
/// order — without a live VM.
extern "C" fn stub_getfield(obj: i64, field_id: i64, is_ref: i64) -> i64 {
    obj * 1_000_000 + field_id + is_ref * 7
}
/// Stub for rava_invoke: reads the args buffer + cp, so the test asserts args were
/// marshalled into the buffer and the const-table cp/caller_ptr threaded.
extern "C" fn stub_invoke(caller: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    assert_eq!(n, 2, "arg count");
    assert_eq!(caller, 0xC1A55, "caller_ptr from ClassPtr const");
    let (a0, a1) = unsafe { (*buf, *buf.add(1)) };
    a0 * 1_000_000 + a1 * 1000 + cp + kind
}
extern "C" fn stub_putfield(obj: i64, fid: i64, val: i64, kind: i64) -> i64 {
    obj * 1_000_000 + fid * 1000 + val * 10 + kind
}
extern "C" fn stub_new(caller: i64, cp: i64) -> i64 {
    caller * 100 + cp
}
extern "C" fn stub_getfield_const(_obj: i64, fid: i64, _is_ref: i64) -> i64 {
    // A field whose "value" is fid+1 — lets the test predict the sum.
    fid + 1
}
extern "C" fn stub_invoke_sum(_caller: i64, cp: i64, _kind: i64, buf: *const i64, n: i64) -> i64 {
    // Sum the marshalled args + cp — lets the test verify args threaded.
    let mut s = cp;
    for i in 0..n {
        s += unsafe { *buf.add(i as usize) };
    }
    s
}
/// A2 invoke ROOTING repro (deterministic, no boot): a ref held across an invoke
/// that triggers a moving GC must survive + be forwarded. Two shapes — held in a
/// LOCAL vs held on the OPERAND STACK — to isolate which rooting path is broken.
/// The stub rava_invoke forces an arena collection, moving the ref's object.
extern "C" fn stub_invoke_gc(_caller: i64, _cp: i64, _kind: i64, _buf: *const i64, _n: i64) -> i64 {
    let vm = crate::jit::vm_ref().unwrap();
    vm.arena_collect_jit(); // move young objects; the rooted cells buffer forwards them
    0
}

mod aot1;
mod aot2;
