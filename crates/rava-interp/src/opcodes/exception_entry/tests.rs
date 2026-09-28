use super::*;
use rava_classfile::{ClassFile, ExceptionHandler};

fn handler(start: u16, end: u16, target: u16, catch_type: u16) -> ExceptionHandler {
    ExceptionHandler { start_pc: start, end_pc: end, handler_pc: target, catch_type }
}

pub(crate) fn fixture(bytecode: Vec<u8>, exceptions: Vec<ExceptionHandler>, locals: Vec<Value>)
    -> (Vm, Box<Code>, Frame)
{
    // Minimal class, with named catches in its real constant pool.
    let mut bytes = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52, 0, 7];
    for (index, name) in [(1u16, "ExceptionEntry"), (3, "java/lang/Throwable"),
        (5, "java/lang/ArithmeticException")]
    {
        bytes.push(1);
        bytes.extend((name.len() as u16).to_be_bytes());
        bytes.extend(name.as_bytes());
        bytes.push(7);
        bytes.extend(index.to_be_bytes());
    }
    for v in [0x21u16, 2, 0, 0, 0, 0, 0] { bytes.extend(v.to_be_bytes()); }
    let vm = Vm::new(crate::class_loader::ClassLoader::new());
    let class = vm.loader.define_synthetic(ClassFile::parse(&bytes).unwrap());
    let code = Box::new(Code {
        max_stack: 4, max_locals: locals.len() as u16, bytecode, exceptions,
        ..Default::default()
    });
    let frame = Frame::from_parts(&*code, locals, code.max_locals as usize, class);
    (vm, code, frame)
}

fn roots() -> Vec<usize> {
    let mut roots = Vec::new();
    crate::frames::local(&mut roots);
    roots
}

#[test]
fn ordered_catch_uses_original_pc_and_preserves_updated_local() {
    // Replaying from entry would overwrite the already updated local with zero.
    let (mut vm, code, mut frame) = fixture(
        vec![0x03, 0x3b, 0xbf, 0x57, 0x1a, 0xac, 0x57, 0x05, 0xac],
        vec![handler(0, 3, 3, 6), handler(0, 3, 6, 4)], vec![Value::Int(73)],
    );
    frame.push(Value::Int(999)); // stale compiled operand stack must disappear
    let exc = vm.make_exception("java/lang/ArithmeticException", "test");
    let before = roots();
    let result = resume_exception(&mut vm, &code, &mut frame, 2, VmError::Throw(exc));
    assert!(matches!(result, Ok(RunExit::Normal(Some(Value::Int(73))))));
    assert_eq!(roots(), before);
}

#[test]
fn range_end_is_exclusive_and_uncaught_identity_survives() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0, 0, 0xb1],
        vec![handler(0, 2, 3, 0)], vec![Value::Int(73)]);
    let exc = vm.make_exception("java/lang/ArithmeticException", "identity");
    let Value::Ref(expected) = exc else { panic!("expected heap exception") };
    let before = roots();
    let result = resume_exception(&mut vm, &code, &mut frame, 2, VmError::Throw(Value::Ref(expected)));
    assert!(matches!(result, Err(VmError::Throw(Value::Ref(got))) if got == expected));
    assert!(matches!(frame.buf[0], Value::Int(73)));
    assert_eq!(roots(), before);
}

#[test]
fn wide_and_reference_locals_survive_catch_all_entry() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0x57, 0x1e, 0xad],
        vec![handler(0, 1, 1, 0)],
        vec![Value::Long(i64::MIN + 7), Value::Null, Value::Double(1.25), Value::Null]);
    let reference = vm.make_exception("java/lang/ArithmeticException", "local");
    frame.buf[3] = reference.clone();
    let result = resume_exception(&mut vm, &code, &mut frame, 0, VmError::Throw(reference.clone()));
    assert!(matches!(result, Ok(RunExit::Normal(Some(Value::Long(v)))) if v == i64::MIN + 7));
    assert!(matches!(frame.buf[2], Value::Double(v) if v == 1.25));
    assert!(matches!((&frame.buf[3], reference), (Value::Ref(a), Value::Ref(b)) if *a == b));
}

#[test]
fn handler_rethrow_preserves_identity_and_void_return_is_distinct() {
    for (opcode, returns) in [(0xbf, false), (0xb1, true)] {
        let (mut vm, code, mut frame) = fixture(vec![0, opcode],
            vec![handler(0, 1, 1, 0)], vec![]);
        let Value::Ref(expected) = vm.make_exception("java/lang/ArithmeticException", "test")
            else { panic!("expected heap exception") };
        let before = roots();
        let result = resume_exception(&mut vm, &code, &mut frame, 0, VmError::Throw(Value::Ref(expected)));
        if returns {
            assert!(matches!(result, Ok(RunExit::Normal(None))));
        } else {
            assert!(matches!(result, Err(VmError::Throw(Value::Ref(got))) if got == expected));
        }
        assert_eq!(roots(), before);
    }
}

#[test]
fn host_error_bypasses_catch_without_mutating_frame() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0xb1],
        vec![handler(0, 1, 1, 0)], vec![Value::Int(17)]);
    frame.pc = 1;
    frame.push(Value::Int(23));
    let before = roots();
    let result = resume_exception(&mut vm, &code, &mut frame, 0, VmError::StackUnderflow);
    assert!(matches!(result, Err(VmError::StackUnderflow)));
    assert_eq!(frame.pc, 1);
    assert!(matches!(frame.buf.as_slice(), [Value::Int(17), Value::Int(23)]));
    assert_eq!(roots(), before);
    #[cfg(feature = "next-gen-gc")]
    assert!(matches!(resume_exception(&mut vm, &code, &mut frame, 0, VmError::RegionAbort),
        Err(VmError::RegionAbort)));
}

#[test]
fn nonmatching_typed_catch_falls_through_to_catch_all() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0x57, 0x04, 0xac, 0x57, 0x05, 0xac],
        vec![handler(0, 1, 1, 6), handler(0, 1, 4, 0)], vec![]);
    let exc = vm.make_exception("java/lang/NullPointerException", "test");
    let result = resume_exception(&mut vm, &code, &mut frame, 0, VmError::Throw(exc));
    assert!(matches!(result, Ok(RunExit::Normal(Some(Value::Int(2))))));
}

#[test]
fn handler_rethrow_enters_enclosing_handler_with_same_exception() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0xbf, 0xb0],
        vec![handler(0, 1, 1, 0), handler(1, 2, 2, 0)], vec![]);
    let Value::Ref(expected) = vm.make_exception("java/lang/ArithmeticException", "test")
        else { panic!("expected heap exception") };
    let before = roots();
    let result = resume_exception(&mut vm, &code, &mut frame, 0, VmError::Throw(Value::Ref(expected)));
    assert!(matches!(result, Ok(RunExit::Normal(Some(Value::Ref(got)))) if got == expected));
    assert_eq!(roots(), before);
}

#[test]
fn handler_execution_error_releases_registered_roots() {
    let (mut vm, code, mut frame) = fixture(vec![0, 0xff],
        vec![handler(0, 1, 1, 0)], vec![]);
    let exc = vm.make_exception("java/lang/ArithmeticException", "test");
    let before = roots();
    let result = resume_exception(&mut vm, &code, &mut frame, 0, VmError::Throw(exc));
    assert!(matches!(result, Err(VmError::UnknownOpcode(0xff))));
    assert_eq!(roots(), before);
}
