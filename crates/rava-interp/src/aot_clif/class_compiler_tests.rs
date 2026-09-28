use super::ClassCompiler;
use crate::cnp::{CompiledObj, ConstDesc};
use rava_classfile::{ClassFile, ExceptionHandler};

pub(super) fn fixture() -> ClassFile {
    fn u16be(out: &mut Vec<u8>, value: u16) { out.extend(value.to_be_bytes()); }
    fn utf8(out: &mut Vec<u8>, value: &str) {
        out.push(1);
        u16be(out, value.len() as u16);
        out.extend(value.as_bytes());
    }
    let mut bytes = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52];
    u16be(&mut bytes, 11);
    utf8(&mut bytes, "OfflineFixture"); // #1
    bytes.extend([7, 0, 1]); // #2
    utf8(&mut bytes, "java/lang/Object"); // #3
    bytes.extend([7, 0, 3]); // #4
    utf8(&mut bytes, "run"); // #5
    utf8(&mut bytes, "()I"); // #6
    utf8(&mut bytes, "Code"); // #7
    bytes.push(3); // #8
    bytes.extend(100_000i32.to_be_bytes());
    utf8(&mut bytes, "<clinit>"); // #9
    utf8(&mut bytes, "()V"); // #10
    for value in [0x21, 2, 4, 0, 0, 2] { u16be(&mut bytes, value); }
    // The initializer never terminates if executed. Compilation must only read it.
    for (flags, name, desc, stack, code) in [
        (9, 5, 6, 1, [0x12, 8, 0xac]),
        (8, 9, 10, 0, [0xa7, 0, 0]),
    ] {
        for value in [flags, name, desc, 1, 7] { u16be(&mut bytes, value); }
        bytes.extend(15u32.to_be_bytes());
        u16be(&mut bytes, stack);
        u16be(&mut bytes, 0);
        bytes.extend(3u32.to_be_bytes());
        bytes.extend(code);
        u16be(&mut bytes, 0);
        u16be(&mut bytes, 0);
    }
    u16be(&mut bytes, 0);
    ClassFile::parse(&bytes).unwrap()
}

#[test]
fn direct_leaf_identity_tracks_runtime_admission_flags() {
    let mut class = fixture();
    let before = ClassCompiler::new(&class).direct_leaf_key(0).unwrap();
    class.access_flags &= !0x0001;
    assert_ne!(
        before,
        ClassCompiler::new(&class).direct_leaf_key(0).unwrap()
    );
    class.access_flags |= 0x0001;
    class.methods[0].access_flags &= !0x0001;
    assert_ne!(
        before,
        ClassCompiler::new(&class).direct_leaf_key(0).unwrap()
    );
}

#[test]
fn direct_leaf_preparer_matches_public_runtime_gate() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().bytecode = vec![0x04, 0xac];
    assert!(ClassCompiler::new(&class)
        .compile_direct_leaf_method(0)
        .is_some());

    class.methods[0].access_flags &= !0x0001;
    assert!(ClassCompiler::new(&class)
        .compile_direct_leaf_method(0)
        .is_none());
    class.methods[0].access_flags |= 0x0001;
    class.access_flags &= !0x0001;
    assert!(ClassCompiler::new(&class)
        .compile_direct_leaf_method(0)
        .is_none());
}

#[test]
fn serialized_code_runs_without_creating_a_vm() {
    let class = fixture();
    let compiler = ClassCompiler::new(&class);
    let obj = compiler.compile_method(0).expect("numeric method compiles");
    let decoded = CompiledObj::from_bytes(&obj.to_bytes()).unwrap();
    let (function, constants) = decoded.link_full(&super::resolve_symbol, &mut |d| match d {
        ConstDesc::ClassPtr(_) => Some(0), // This numeric method never reads the class.
        _ => None,
    }).unwrap();
    let mut locals = [0i64; 1];
    assert_eq!(unsafe { function.run_const(locals.as_mut_ptr(), constants.as_ptr()) }, 100_000);
    assert!(compiler.compile_method(usize::MAX).is_none());
}

#[test]
fn exception_handlers_are_refused_without_a_vm() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().exceptions.push(ExceptionHandler {
        start_pc: 0, end_pc: 2, handler_pc: 2, catch_type: 0,
    });
    assert!(ClassCompiler::new(&class).compile_method(0).is_none());
}

#[test]
fn class_literals_and_missing_code_are_refused() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().bytecode = vec![0x12, 2, 0xb0];
    assert!(ClassCompiler::new(&class).compile_method(0).is_none());
    class.methods[0].code = None;
    assert!(ClassCompiler::new(&class).compile_method(0).is_none());
}
