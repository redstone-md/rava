use super::*;
use crate::jit::*;
use crate::jit::exception_state::{owned::tests::class, OwnedExceptionSites};
use crate::{Value as GuestValue, Vm, opcodes::RunExit};
use std::sync::Arc;

#[test]
fn abandoned_compile_releases_all_staged_class_pins() {
    let class = class();
    let weak = Arc::downgrade(&class);
    let factory = OwnedExceptionSites::new(class, 0).unwrap();
    let mut pending = PendingExceptionSites::default();
    for _ in 0..16 { pending.insert(factory.site(0).unwrap()); }
    drop(factory);
    assert!(weak.upgrade().is_some());
    drop(pending);
    assert!(weak.upgrade().is_none());
}

// Test-only ABI shim: one int local and int return. It is deliberately not the
// general production exit ABI, whose nonterminal/error protocol remains pending.
extern "C" fn int_exit(vm: *mut Vm, site: *const OwnedExceptionSite, cells: *const i64) -> i64 {
    let (vm, site, cells) = unsafe { (&mut *vm, &*site, std::slice::from_raw_parts(cells, 1)) };
    let mut frame = site.frame();
    match site.resume_pending(vm, &mut frame, cells) {
        Some(Ok(RunExit::Normal(Some(GuestValue::Int(value))))) => value as i64,
        _ => i64::MIN,
    }
}

#[test]
fn generated_exit_address_survives_batch_growth_and_module_drop() {
    let class = class();
    let weak = Arc::downgrade(&class);
    let factory = OwnedExceptionSites::new(class, 0).unwrap();
    let mut pending = PendingExceptionSites::default();
    let address = pending.insert(factory.site(0).unwrap());
    for _ in 0..16 { pending.insert(factory.site(0).unwrap()); }
    drop(factory);

    let mut module = build_shared_module();
    let mut signature = module.make_signature();
    signature.params.extend([AbiParam::new(I64), AbiParam::new(I64)]);
    signature.returns.push(AbiParam::new(I64));
    let mut callback = module.make_signature();
    callback.params.extend([AbiParam::new(I64), AbiParam::new(I64), AbiParam::new(I64)]);
    callback.returns.push(AbiParam::new(I64));
    let mut ctx = module.make_context();
    ctx.func.signature = signature.clone();
    let mut fb = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let vm = b.block_params(entry)[0];
        let cells = b.block_params(entry)[1];
        let site = b.ins().iconst(I64, address as usize as i64);
        let target = b.ins().iconst(I64, int_exit as *const () as usize as i64);
        let sig = b.import_signature(callback);
        let call = b.ins().call_indirect(sig, target, &[vm, site, cells]);
        let result = b.inst_results(call)[0];
        b.ins().return_(&[result]);
        b.finalize();
    }
    let id = module.declare_function("owned_exception_exit_test", Linkage::Local, &signature).unwrap();
    module.define_function(id, &mut ctx).unwrap();
    module.finalize_definitions().unwrap();
    let code = module.get_finalized_function(id);
    assert_eq!(pending.publish(), 17);
    drop(module); // Cranelift documents that this does NOT invalidate code.
    assert!(weak.upgrade().is_some());

    let call: extern "C" fn(*mut Vm, *const i64) -> i64 = unsafe { std::mem::transmute(code) };
    let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
    for value in [i32::MIN, -9, 0, 71, i32::MAX] {
        let exception = vm.make_exception("java/lang/ArithmeticException", "native exit");
        vm.jit_pending_set(exception);
        vm.heap.set_jit_fault();
        assert_eq!(call(&mut vm, [value as i64].as_ptr()), value as i64);
        assert!(!vm.jit_pending_is_set());
        assert_eq!(crate::heap::active_jit_fault_for_test(&vm.heap.jit_ctx), 0);
    }
}
