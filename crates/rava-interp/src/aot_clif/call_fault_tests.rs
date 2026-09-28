use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static THROW: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn invoke(_: i64, _: i64, _: i64, _: i64, _: i64) -> i64 {
    call_result()
}
extern "C" fn indy(_: i64, _: i64, _: i64, _: i64) -> i64 {
    call_result()
}
fn call_result() -> i64 {
    CALLS.fetch_add(1, Ordering::Relaxed);
    if THROW.load(Ordering::Relaxed) {
        crate::jit::vm_ref_worker_sanctioned().unwrap()
            .jit_pending_set(crate::Value::Str("call fault".into()));
    }
    42
}

#[test]
fn aot_call_fault_preserves_prior_effects_and_stops_following_bytecode() {
    for op in [0xb6, 0xb7, 0xb8, 0xb9, 0xba] {
        let receiver = matches!(op, 0xb6 | 0xb7 | 0xb9);
        let mut bytes = vec![0x10, 6, 0x3e]; // Local 3 changes before the call.
        if receiver { bytes.push(0x2a); }
        bytes.extend([op, 0, 1]);
        if op == 0xb9 { bytes.extend([1, 0]); }
        if op == 0xba { bytes.extend([0, 0]); }
        bytes.extend([0x3c, 0x10, 9, 0xac]); // Store returned int into local 1.
        let info = |cells| InvokeInfo { numeric_leaf: None, object_leaf: None,
            direct_leaf: None, ncells: cells, kind: 0, ret_void: false, ret_ref: false, ret_wide: false };
        let (code, relocs, constants) = compile_method(&bytes, "CallFault", 4,
            &|_| None, &|_| Some(info(1)), &|_| Some(info(0)), &|_| None).unwrap();
        let object = CompiledObj::from_full(code, relocs, constants);
        let (function, values) = object.link_full(&|index| {
            let (name, address) = crate::jit::aot_symtab()[index as usize];
            match name {
                "rava_invoke" => invoke as usize,
                "rava_indy" => indy as usize,
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
            let result = unsafe { function.run_const(locals.as_mut_ptr(), values.as_ptr()) };
            crate::jit::set_current_vm(previous);
            let exception = vm.jit_pending_take();
            assert_eq!(matches!(exception, Some(crate::Value::Str(ref s)) if s == "call fault"), throwing);
            assert_eq!(CALLS.load(Ordering::Relaxed), 1);
            assert_eq!(locals[3], 6);
            assert_eq!(result, if throwing { 0 } else { 9 }, "opcode {op:x}");
            assert_eq!(locals[1], if throwing { 5 } else { 42 });
        }
    }
}
