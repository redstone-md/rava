use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static THROW: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
fn result() -> i64 {
    CALLS.fetch_add(1, Ordering::Relaxed);
    if THROW.load(Ordering::Relaxed) {
        crate::jit::vm_ref_worker_sanctioned().unwrap()
            .jit_pending_set(crate::Value::Str("callback fault".into()));
    }
    42
}
extern "C" fn two(_: i64, _: i64) -> i64 { result() }
extern "C" fn three(_: i64, _: i64, _: i64) -> i64 { result() }
extern "C" fn four(_: i64, _: i64, _: i64, _: i64) -> i64 { result() }

#[test]
fn aot_callback_fault_exits_without_losing_the_exception() {
    let mut cases = Vec::new();
    for op in 0x2e..=0x35 {
        cases.push(vec![0x2a, 0x03, op, if matches!(op, 0x2f | 0x31) { 0x58 } else { 0x57 }]);
    }
    for op in 0x4f..=0x56 {
        let value = match op { 0x50 => 0x0a, 0x51 => 0x0c, 0x52 => 0x0f, 0x53 => 0x01, _ => 0x04 };
        cases.push(vec![0x2a, 0x03, value, op]);
    }
    cases.extend([
        vec![0xbb, 0, 1, 0x57], vec![0x04, 0xbc, 10, 0x57],
        vec![0x04, 0xbd, 0, 1, 0x57], vec![0x12, 1, 0x57],
        vec![0xb2, 0, 1, 0x57], vec![0x2a, 0xc0, 0, 1, 0x57],
        vec![0x2a, 0xc1, 0, 1, 0x57],
    ]);
    for operation in cases {
        let mut bytes = vec![0x10, 6, 0x3e];
        bytes.extend(&operation);
        bytes.extend([0x10, 7, 0x3c, 0x10, 9, 0xac]);
        let (code, relocs, constants) = compile_method(&bytes, "CallbackFault", 4,
            &|_| Some(("CallbackFault".into(), "field".into(), FieldKind::Int)),
            &|_| None, &|_| None, &|_| Some(LdcConst::Class)).unwrap();
        let (function, values) = CompiledObj::from_full(code, relocs, constants)
            .link_full(&|index| {
                let (name, address) = crate::jit::aot_symtab()[index as usize];
                match name {
                    "rava_ldc" | "rava_new" | "rava_newarray" => two as usize,
                    "rava_aload" | "rava_aastore" | "rava_anewarray" |
                    "rava_getstatic" | "rava_instanceof" | "rava_aot_checkcast" => three as usize,
                    "rava_aot_primitive_store" => four as usize,
                    _ => address,
                }
            }, &mut |constant| match constant {
                crate::cnp::ConstDesc::ClassPtr(_) => Some(1),
                crate::cnp::ConstDesc::Imm(value) => Some(*value),
                _ => None,
            }).unwrap();
        for throwing in [false, true] {
            let mut vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
            let mut locals = [1, 5, 0, 0, 0, 0, 0, 0];
            THROW.store(throwing, Ordering::Relaxed);
            CALLS.store(0, Ordering::Relaxed);
            let previous = crate::jit::set_current_vm(&mut vm as *mut _);
            let output = unsafe { function.run_const(locals.as_mut_ptr(), values.as_ptr()) };
            crate::jit::set_current_vm(previous);
            let exception = vm.jit_pending_take();
            assert_eq!(matches!(exception, Some(crate::Value::Str(ref s)) if s == "callback fault"), throwing);
            assert_eq!(CALLS.load(Ordering::Relaxed), 1);
            assert_eq!(locals[3], 6);
            assert_eq!(output, if throwing { 0 } else { 9 }, "operation {operation:x?}");
            assert_eq!(locals[1], if throwing { 5 } else { 7 });
        }
    }
}
