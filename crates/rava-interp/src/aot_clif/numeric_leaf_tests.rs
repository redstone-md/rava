use super::{ClassCompiler, numeric_leaf::NumericLeaf};
use rava_classfile::{ClassFile, Constant, ConstantPool};

fn fixture() -> ClassFile {
    let mut class = super::class_compiler_tests::fixture();
    let mut entries: Vec<_> = (0..class.pool.len())
        .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
    entries.push(Constant::NameAndType { name_index: 5, desc_index: 6 }); // 11
    entries.push(Constant::MethodRef { class_index: 2, nat_index: 11 }); // 12
    class.pool = ConstantPool::from_entries(entries);
    let mut caller = class.methods[0].clone();
    caller.name = "caller".into();
    caller.code.as_mut().unwrap().bytecode = vec![0xb8, 0, 12, 0xac];
    class.methods.push(caller);
    class
}

#[test]
fn same_class_leaf_executes_without_an_invoke_callback() {
    let class = fixture();
    assert!(NumericLeaf::resolve(&class, 12).is_some());
    let object = ClassCompiler::new(&class).compile_method(2).unwrap();
    let (function, constants) = object.link_full(&|_| 0, &mut |_| Some(0)).unwrap();
    let mut locals = [0i64; 2];
    assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) }, 100_000);
}

#[test]
fn callee_changes_invalidate_unchanged_caller_artifact() {
    let mut class = fixture();
    let before = ClassCompiler::new(&class).method_key(2).unwrap();
    class.methods[0].code.as_mut().unwrap().bytecode = vec![0x04, 0xac];
    assert_ne!(before, ClassCompiler::new(&class).method_key(2).unwrap());
    let before_flags = ClassCompiler::new(&class).method_key(2).unwrap();
    class.methods[0].access_flags |= 0x20;
    assert_ne!(before_flags, ClassCompiler::new(&class).method_key(2).unwrap());
}

#[test]
fn synchronized_instance_recursive_and_faulting_leaves_are_refused() {
    for flags in [0x29, 0x1, 0x109, 0x409] {
        let mut class = fixture();
        class.methods[0].access_flags = flags;
        assert!(NumericLeaf::resolve(&class, 12).is_none());
    }
    for bytecode in [vec![0xb8, 0, 12, 0xac], vec![0x04, 0x03, 0x6c, 0xac],
        vec![0x15, 0, 0xac], vec![0x04, 0xac, 0x00]] {
        let mut class = fixture();
        class.methods[0].code.as_mut().unwrap().bytecode = bytecode;
        assert!(NumericLeaf::resolve(&class, 12).is_none());
    }
}

fn append_twice(class: &mut ClassFile, target: u16, name: &str) -> u16 {
    let mut entries: Vec<_> = (0..class.pool.len())
        .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
    let name_index = entries.len() as u16;
    entries.extend([
        Constant::Utf8(name.into()),
        Constant::NameAndType { name_index, desc_index: 6 },
        Constant::MethodRef { class_index: 2, nat_index: name_index + 1 },
    ]);
    class.pool = ConstantPool::from_entries(entries);
    let mut method = class.methods[0].clone();
    method.name = name.into();
    let [hi, lo] = target.to_be_bytes();
    let code = method.code.as_mut().unwrap();
    code.max_stack = 2;
    code.bytecode = vec![0xb8, hi, lo, 0xb8, hi, lo, 0x60, 0xac];
    class.methods.push(method);
    name_index + 2
}

#[test]
fn aot_nested_numeric_calls_execute_without_callbacks_and_preserve_overflow() {
    for (input, expected) in [(100_000, 400_000), (i32::MAX, -4)] {
        let mut class = fixture();
        let mut entries: Vec<_> = (0..class.pool.len())
            .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
        entries[8] = Constant::Integer(input);
        class.pool = ConstantPool::from_entries(entries);
        let bridge = append_twice(&mut class, 12, "bridge");
        append_twice(&mut class, bridge, "outer");
        assert!(NumericLeaf::resolve(&class, bridge).is_some());
        let object = ClassCompiler::new(&class).compile_method(class.methods.len() - 1).unwrap();
        // A callback relocation would resolve to null and fail execution. The
        // complete two-level computation must therefore execute as native code.
        let (function, constants) = object.link_full(&|_| 0, &mut |_| Some(0)).unwrap();
        let mut locals = [0i64; 4];
        assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) }, expected);
    }
}

#[test]
fn aot_numeric_expansion_charges_repeated_calls_to_one_budget() {
    let mut class = fixture();
    let mut target = 12;
    for depth in 1..=5 {
        target = append_twice(&mut class, target, &format!("level{depth}"));
        assert_eq!(NumericLeaf::resolve(&class, target).is_some(), depth < 5,
            "small individual bodies must not bypass the total expansion budget");
    }
}

#[test]
fn aot_nested_numeric_calls_preserve_argument_order_and_signed_cells() {
    let mut class = fixture();
    let mut entries: Vec<_> = (0..class.pool.len())
        .map(|i| class.pool.get(i as u16).unwrap().clone()).collect();
    entries[6] = Constant::Utf8("(II)I".into());
    entries.extend([
        Constant::Utf8("subtract".into()),
        Constant::NameAndType { name_index: 13, desc_index: 6 },
        Constant::MethodRef { class_index: 2, nat_index: 14 },
    ]);
    class.pool = ConstantPool::from_entries(entries);
    for index in [0, 2] {
        let method = &mut class.methods[index];
        method.descriptor = "(II)I".into();
        let code = method.code.as_mut().unwrap();
        code.max_locals = 2;
        code.max_stack = 2;
        code.bytecode = if index == 0 { vec![0x1b, 0x1a, 0xb8, 0, 15, 0xac] }
            else { vec![0x1a, 0x1b, 0xb8, 0, 12, 0xac] };
    }
    let mut subtract = class.methods[0].clone();
    subtract.name = "subtract".into();
    subtract.code.as_mut().unwrap().bytecode = vec![0x1a, 0x1b, 0x64, 0xac];
    class.methods.push(subtract);
    let object = ClassCompiler::new(&class).compile_method(2).unwrap();
    let (function, constants) = object.link_full(&|_| 0, &mut |_| Some(0)).unwrap();
    for (a, c) in [(1i32, 7i32), (-5, -13), (i32::MIN, i32::MAX)] {
        let mut locals = [a as i64, c as i64, 0, 0];
        assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) },
            c.wrapping_sub(a) as i64);
    }
}
