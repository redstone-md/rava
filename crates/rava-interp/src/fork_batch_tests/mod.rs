//! GAP-S01 focused tests: the iterator-fork batch may target only receivers whose
//! exact runtime dispatch — the same `resolve_method` walk serial execution uses —
//! selects ONE identical bytecode method for every element. Heterogeneous overrides,
//! distinct interface implementations, abstract, native and unresolved targets all
//! keep the serial path (`None`). Deduplication by receiver class never drops a
//! class from proof coverage, and admission contains no name-based routing:
//! renaming every synthetic class must not change any verdict.

use super::*;
use rava_classfile::{ClassFile, Code, Constant, ConstantPool, MemberInfo};

fn pool() -> ConstantPool {
    ConstantPool::from_entries(vec![Constant::Unusable])
}

/// A minimal distinct method body. `pad` nops make each body's `Code` unique in
/// content (bodies are compared by pointer identity, but distinct shapes keep the
/// test honest about which method resolved).
fn body(pad: usize) -> Code {
    let mut bc = vec![0x00; pad]; // nop*pad
    bc.push(0xb1); // return
    Code { max_stack: 1, max_locals: 1, bytecode: bc, exceptions: Vec::new(), ..Default::default() }
}

fn method(name: &str, desc: &str, flags: u16, code: Option<Code>) -> MemberInfo {
    MemberInfo { access_flags: flags, name: name.into(), descriptor: desc.into(), code }
}

fn define_flags(vm: &mut Vm, name: &str, superc: Option<&str>, ifaces: &[&str], methods: Vec<MemberInfo>, flags: u16) {
    let file = ClassFile::synthesize(
        pool(),
        flags,
        name.to_string(),
        superc.map(str::to_string),
        ifaces.iter().map(|s| s.to_string()).collect(),
        methods,
    );
    vm.loader.define_synthetic(file);
}

fn define(vm: &mut Vm, name: &str, superc: Option<&str>, ifaces: &[&str], methods: Vec<MemberInfo>) {
    define_flags(vm, name, superc, ifaces, methods, 0x0021); // ACC_PUBLIC | ACC_SUPER
}

fn obj(vm: &mut Vm, class: &str) -> Value {
    Value::Ref(vm.heap.alloc(class))
}

/// The exact `Code` pointer `resolve_method` selects for `class.m()V` (must exist).
fn resolved_code_key(vm: &mut Vm, class: &str) -> usize {
    let decl = vm
        .resolve_method(class, "m", "()V")
        .expect("resolve ok")
        .expect("target exists");
    let m = decl.file.method("m", "()V").expect("method present");
    m.code.as_ref().expect("bytecode present") as *const Code as usize
}

/// Pool: #1 = MethodRef `owner.name desc`, #7 = String "boom".
fn mref_pool(owner: &str, name: &str, desc: &str) -> ConstantPool {
    ConstantPool::from_entries(vec![
        Constant::Unusable,                                     // 0
        Constant::MethodRef { class_index: 2, nat_index: 3 },   // 1
        Constant::Class { name_index: 4 },                      // 2
        Constant::NameAndType { name_index: 5, desc_index: 6 }, // 3
        Constant::Utf8(owner.into()),                           // 4
        Constant::Utf8(name.into()),                            // 5
        Constant::Utf8(desc.into()),                            // 6
        Constant::String { utf8_index: 8 },                     // 7
        Constant::Utf8("boom".into()),                          // 8
    ])
}

/// `new #2; dup; ldc "boom"; invokespecial #1 (owner.<init>(String)V); athrow` —
/// the exact throw-path shape the canonical drill bottoms out in
/// (StateHolder.getValue's IllegalArgumentException error path).
fn fresh_throw_body() -> Code {
    Code {
        max_stack: 4,
        max_locals: 1,
        bytecode: vec![0xbb, 0x00, 0x02, 0x59, 0x12, 0x07, 0xb7, 0x00, 0x01, 0xbf],
        exceptions: Vec::new(),
     ..Default::default() }
}

fn define_with_pool(vm: &mut Vm, name: &str, superc: Option<&str>, pool: ConstantPool, methods: Vec<MemberInfo>) {
    let file = ClassFile::synthesize(pool, 0x0021, name.to_string(), superc.map(str::to_string), vec![], methods);
    vm.loader.define_synthetic(file);
}

mod fork1;
mod fork2;
