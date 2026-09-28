use super::{ClassCompiler, object_leaf::ObjectLeaf};
use crate::cnp::ConstDesc;
use rava_classfile::{ClassFile, Constant, ConstantPool};
use std::cell::Cell;

thread_local! {
    static INVOKES: Cell<usize> = const { Cell::new(0) };
    static WRITES: Cell<usize> = const { Cell::new(0) };
    static INVOKE_KIND: Cell<i64> = const { Cell::new(-1) };
    static NULL_FAULTS: Cell<usize> = const { Cell::new(0) };
}

fn fixture() -> ClassFile {
    let mut class = super::class_compiler_tests::fixture();
    let desc = "(LOfflineFixture;LOfflineFixture;Ljava/lang/Object;)Ljava/lang/Object;";
    let mut entries: Vec<_> = (0..class.pool.len())
        .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
    entries.extend([
        Constant::Utf8("leaf".into()), Constant::Utf8(desc.into()), // 11,12
        Constant::NameAndType { name_index: 11, desc_index: 12 },
        Constant::MethodRef { class_index: 2, nat_index: 13 }, // 14
        Constant::Utf8("value".into()), Constant::Utf8("Ljava/lang/Object;".into()),
        Constant::NameAndType { name_index: 15, desc_index: 16 },
        Constant::FieldRef { class_index: 2, nat_index: 17 }, // 18
    ]);
    class.pool = ConstantPool::from_entries(entries);
    let mut field = class.methods[0].clone();
    field.name = "value".into(); field.descriptor = "Ljava/lang/Object;".into();
    field.access_flags = 1; field.code = None;
    class.fields.push(field);
    let leaf = &mut class.methods[0];
    leaf.name = "leaf".into(); leaf.descriptor = desc.into();
    let code = leaf.code.as_mut().unwrap();
    code.max_locals = 3; code.max_stack = 2;
    code.bytecode = vec![0x2a, 0x2c, 0xb5, 0, 18, 0x2b, 0xb4, 0, 18, 0xb0];
    let mut caller = leaf.clone();
    caller.name = "caller".into();
    caller.code.as_mut().unwrap().max_stack = 3;
    caller.code.as_mut().unwrap().bytecode = vec![0x2a, 0x2b, 0x2c, 0xb8, 0, 14, 0xb0];
    class.methods.push(caller);
    class
}

extern "C" fn get(receiver: i64, _field: i64, _reference: i64) -> i64 {
    unsafe { *(receiver as *const i64) }
}
extern "C" fn put(receiver: i64, _field: i64, value: i64, _kind: i64) -> i64 {
    WRITES.with(|n| n.set(n.get() + 1));
    unsafe { *(receiver as *mut i64) = value; }
    0
}
extern "C" fn fallback(_caller: i64, _cp: i64, kind: i64, args: *const i64, _n: i64) -> i64 {
    INVOKES.with(|n| n.set(n.get() + 1));
    INVOKE_KIND.with(|n| n.set(kind));
    let args = unsafe { std::slice::from_raw_parts(args, 3) };
    // Model the original method's write before its second receiver faults.
    if args[0] != 0 { unsafe { *(args[0] as *mut i64) = args[2]; } }
    99 // A fault path must return from the caller, never use this sentinel.
}

extern "C" fn null_fault(_caller: i64, _pc: i64, _op: i64, _depth: i64) -> i64 {
    NULL_FAULTS.with(|n| n.set(n.get() + 1));
    0
}

#[test]
fn object_leaf_preserves_aliasing_and_null_fallback_before_effects() {
    check_aliasing_and_null(fixture());
}

fn check_aliasing_and_null(class: ClassFile) {
    let (function, constants) = link_leaf(&class);
    let expected_kind = match class.methods[2].code.as_ref().unwrap().bytecode[3] {
        0xb6 => 1, 0xb7 => 2, _ => 0,
    };
    for case in 0..4 {
        let (mut first, mut second) = (11i64, 22i64);
        let a = &mut first as *mut i64 as i64;
        let c = &mut second as *mut i64 as i64;
        let (left, right) = match case { 0 => (a, c), 1 => (a, a), 2 => (a, 0), _ => (0, c) };
        let mut locals = [left, right, 77, 0, 0, 0];
        INVOKES.with(|n| n.set(0)); WRITES.with(|n| n.set(0));
        NULL_FAULTS.with(|n| n.set(0));
        let result = unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) };
        assert_eq!(result, [22, 77, 0, 0][case]);
        assert_eq!(first, if case == 3 { 11 } else { 77 });
        let outer_null = case == 3 && expected_kind != 0;
        assert_eq!(INVOKES.with(Cell::get), usize::from(case >= 2 && !outer_null));
        assert_eq!(NULL_FAULTS.with(Cell::get), usize::from(outer_null));
        assert_eq!(WRITES.with(Cell::get), usize::from(case < 2));
        if case >= 2 && !outer_null { assert_eq!(INVOKE_KIND.with(Cell::get), expected_kind); }
    }
}

fn link_leaf(class: &ClassFile) -> (crate::cnp::CompiledFn, Vec<i64>) {
    assert!(ObjectLeaf::resolve(&class, 14).is_some());
    let object = ClassCompiler::new(&class).compile_method(2).unwrap();
    object.link_full(&|index| {
        match crate::jit::aot_symtab()[index as usize].0 {
            "rava_getfield" => get as usize,
            "rava_putfield" => put as usize,
            "rava_invoke" => fallback as usize,
            "rava_aot_null_fault" => null_fault as usize,
            _ => 0,
        }
    }, &mut |desc| match desc {
        ConstDesc::ClassPtr(_) | ConstDesc::FieldId(_, _) => Some(1),
        ConstDesc::Imm(v) => Some(*v), _ => None,
    }).unwrap()
}

fn instance_fixture(flags: u16, final_class: bool, opcode: u8) -> ClassFile {
    let mut class = fixture();
    let desc = "(LOfflineFixture;Ljava/lang/Object;)Ljava/lang/Object;";
    let mut entries: Vec<_> = (0..class.pool.len())
        .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
    entries[12] = Constant::Utf8(desc.into());
    class.pool = ConstantPool::from_entries(entries);
    class.methods[0].descriptor = desc.into();
    class.methods[0].access_flags = flags;
    if final_class { class.access_flags |= 0x10; }
    class.methods[2].code.as_mut().unwrap().bytecode[3] = opcode;
    class
}

#[test]
fn aot_instance_object_leaves_preserve_dispatch_and_null_effects() {
    for (flags, final_class, opcode) in [(0x11, false, 0xb6), (2, false, 0xb7),
        (2, false, 0xb6), (1, true, 0xb6)] {
        let class = instance_fixture(flags, final_class, opcode);
        let leaf = ObjectLeaf::resolve(&class, 14).unwrap();
        assert!(leaf.supports_opcode(opcode));
        assert!(!leaf.supports_opcode(0xb8));
        assert!(!leaf.supports_opcode(0xb9));
        check_aliasing_and_null(class);
    }
    assert!(ObjectLeaf::resolve(&instance_fixture(1, false, 0xb6), 14).is_none());
    assert!(ObjectLeaf::resolve(&instance_fixture(0x31, true, 0xb6), 14).is_none());
    let mut class = instance_fixture(1, false, 0xb6);
    let before = ClassCompiler::new(&class).method_key(2).unwrap();
    class.access_flags |= 0x10;
    assert_ne!(before, ClassCompiler::new(&class).method_key(2).unwrap());
}

#[test]
fn aot_instance_leaf_checks_null_this_even_when_body_ignores_it() {
    let mut class = instance_fixture(0x11, false, 0xb6);
    class.methods[0].code.as_mut().unwrap().bytecode = vec![0x2b, 0xb4, 0, 18, 0xb0];
    let (function, constants) = link_leaf(&class);
    let mut second = 22i64;
    let mut locals = [0, &mut second as *mut i64 as i64, 77, 0, 0, 0];
    INVOKES.with(|n| n.set(0));
    NULL_FAULTS.with(|n| n.set(0));
    assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) }, 0);
    assert_eq!(INVOKES.with(Cell::get), 0);
    assert_eq!(NULL_FAULTS.with(Cell::get), 1);
    assert_eq!(second, 22);
}

#[test]
fn object_leaf_refuses_unsupported_effects_and_invalid_receiver_proofs() {
    for flags in [1, 0x29, 0x109, 0x409] {
        let mut class = fixture(); class.methods[0].access_flags = flags;
        assert!(ObjectLeaf::resolve(&class, 14).is_none());
    }
    for flags in [0x9, 0x41, 0x11] {
        let mut class = fixture(); class.fields[0].access_flags = flags;
        assert!(ObjectLeaf::resolve(&class, 14).is_none());
    }
    for bytes in [vec![0x2a, 0xb4, 0, 18, 0xb4, 0, 18, 0xb0],
        vec![0x2a, 0xb8, 0, 14, 0xb0], vec![0xbb, 0, 2, 0xb0]] {
        let mut class = fixture(); class.methods[0].code.as_mut().unwrap().bytecode = bytes;
        assert!(ObjectLeaf::resolve(&class, 14).is_none());
    }
    let mut class = fixture();
    let before = ClassCompiler::new(&class).method_key(2).unwrap();
    class.methods[0].code.as_mut().unwrap().bytecode.swap(0, 5);
    // Both loads were different argument opcodes, so this changes the inlined effects.
    assert_ne!(before, ClassCompiler::new(&class).method_key(2).unwrap());
    let before = ClassCompiler::new(&class).method_key(2).unwrap();
    class.fields[0].access_flags |= 0x40;
    assert_ne!(before, ClassCompiler::new(&class).method_key(2).unwrap());
}

extern "C" fn collect(_caller: i64, _cp: i64, _kind: i64, _args: *const i64, _n: i64) -> i64 {
    crate::jit::vm_ref().unwrap().arena_collect_jit();
    0
}

#[test]
fn inlined_reference_result_is_forwarded_across_a_following_collection() {
    for instance in [false, true] { check_reference_forwarding(instance); }
}

fn check_reference_forwarding(instance: bool) {
    let mut class = if instance { instance_fixture(0x11, false, 0xb6) } else { fixture() };
    let mut entries: Vec<_> = (0..class.pool.len())
        .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
    entries.extend([
        Constant::Utf8("collect".into()), Constant::Utf8("()V".into()),
        Constant::NameAndType { name_index: 19, desc_index: 20 },
        Constant::MethodRef { class_index: 4, nat_index: 21 },
    ]);
    class.pool = ConstantPool::from_entries(entries);
    class.methods[2].code.as_mut().unwrap().bytecode =
        vec![0x2a, 0x2b, 0x2c, if instance { 0xb6 } else { 0xb8 }, 0, 14, 0xb8, 0, 22, 0xb0];
    let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
    let fid = vm.resolve_field_id("OfflineFixture", "value");
    let data_fid = vm.resolve_field_id("ObjectLeafData", "number");
    let cid = crate::nextgen::register_class(&std::sync::Arc::from("OfflineFixture"),
        &vec![(fid, 6u8)].into());
    let data_cid = crate::nextgen::register_class(&std::sync::Arc::from("ObjectLeafData"),
        &vec![(data_fid, 0u8)].into());
    let receiver = crate::nextgen::tlab_try_alloc(cid, 1).unwrap();
    let data = crate::nextgen::tlab_try_alloc(data_cid, 1).unwrap();
    unsafe { crate::nextgen::set_cell(data.as_ptr(), 0, 4242); }
    let object = ClassCompiler::new(&class).compile_method(2).unwrap();
    let (function, constants) = object.link_full(&|index| {
        if crate::jit::aot_symtab()[index as usize].0 == "rava_invoke" { collect as usize }
        else { super::resolve_symbol(index) }
    }, &mut |desc| match desc {
        ConstDesc::ClassPtr(_) => Some(0),
        ConstDesc::FieldId(_, _) => Some(fid as i64),
        ConstDesc::Imm(v) => Some(*v), _ => None,
    }).unwrap();
    let previous = crate::jit::set_current_vm(&mut vm as *mut _);
    crate::frames::register();
    let mut locals = [receiver.as_ptr() as i64, receiver.as_ptr() as i64,
        data.as_ptr() as i64, 0, 0, 0];
    crate::frames::cells_push((locals.as_ptr(), locals.len()));
    let result = unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) };
    crate::frames::cells_pop();
    crate::frames::unregister();
    crate::jit::set_current_vm(previous);
    assert_ne!(result, data.as_ptr() as i64, "collection must actually move the result");
    let forwarded = unsafe { crate::arena::ArenaPtr::from_addr(result as u64) };
    assert_eq!(unsafe { crate::nextgen::cell(forwarded.as_ptr(), 0) }, 4242);
}
