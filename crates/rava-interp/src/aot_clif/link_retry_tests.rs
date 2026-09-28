use crate::{class_loader::ClassLoader, Value, Vm};
use rava_classfile::{ClassFile, Constant, ConstantPool};
use std::sync::atomic::Ordering;

#[test]
fn aot_failed_field_binding_recovers_after_class_definition() {
    let mut vm = Vm::new(ClassLoader::new());
    vm.jit_threshold = 1;
    let mut file = super::class_compiler_tests::fixture();
    file.this_class = "LinkRetryCaller".into();
    file.super_class = None;
    file.pool = ConstantPool::from_entries(vec![
        Constant::Unusable,
        Constant::Utf8("LinkRetryOwner".into()),
        Constant::Class { name_index: 1 },
        Constant::Utf8("value".into()),
        Constant::Utf8("I".into()),
        Constant::NameAndType { name_index: 3, desc_index: 4 },
        Constant::FieldRef { class_index: 2, nat_index: 5 },
    ]);
    file.methods.truncate(1);
    let method = &mut file.methods[0];
    method.descriptor = "(ILLinkRetryOwner;)I".into();
    let code = method.code.as_mut().unwrap();
    code.max_locals = 2;
    code.max_stack = 1;
    // The zero argument avoids dereferencing the null receiver, but binding still
    // requires the field owner's metadata. No exception or guest allocation needed.
    code.bytecode = vec![0x1a, 0x99, 0, 8, 0x2b, 0xb4, 0, 6, 0xac, 0x10, 7, 0xac];
    let mut field = method.clone();
    field.name = "value".into();
    field.descriptor = "I".into();
    field.access_flags = 1;
    field.code = None;
    assert!(super::ClassCompiler::new(&file).compile_method(0).is_some());
    let caller = vm.loader.define_synthetic(file);
    caller.clinit_done.store(true, Ordering::Relaxed);
    let method = &caller.file.methods[0];
    let code = method.code.as_ref().unwrap();
    let args = [Value::Int(0), Value::Null];
    assert!(vm.aot_run(code, &method.name, &method.descriptor, true, &caller, &args).is_none());
    assert!(!vm.aot_full_cache.contains_key(&(code as *const _ as usize)),
        "failed metadata binding must not cache interpreter eligibility");

    let mut owner = ClassFile::synthesize(ConstantPool::from_entries(vec![Constant::Unusable]),
        0x21, "LinkRetryOwner".into(), None, Vec::new(), Vec::new());
    owner.fields.push(field);
    vm.loader.define_synthetic(owner);
    let result = vm.aot_run(code, &method.name, &method.descriptor, true, &caller, &args)
        .expect("same method must become executable after metadata appears").unwrap();
    assert!(matches!(result, Some(Value::Int(7))));
    assert!(vm.aot_full_cache.get(&(code as *const _ as usize)).unwrap().is_some());
}
