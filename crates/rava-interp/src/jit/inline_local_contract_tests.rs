//! Dependency contract required before pooling inline-local Variable identities.

use cranelift::codegen::ir::{types::I64, AbiParam, ExtFuncData, ExternalName, Function,
    InstBuilder, Signature, UserExternalName};
use cranelift::codegen::isa::CallConv;
use cranelift::frontend::{FunctionBuilder, FunctionBuilderContext};

fn crossing(reuse: bool, explicit_value_root: bool) -> String {
    let mut function = Function::new();
    function.signature = Signature::new(CallConv::SystemV);
    function.signature.params.push(AbiParam::new(I64));
    function.signature.returns.push(AbiParam::new(I64));
    let mut context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut function, &mut context);
    let name = builder.func.declare_imported_user_function(UserExternalName {
        namespace: 0, index: 0,
    });
    let signature = builder.func.import_signature(Signature::new(CallConv::SystemV));
    let callback = builder.import_function(ExtFuncData {
        name: ExternalName::user(name), signature, colocated: false, patchable: false,
    });
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let first = builder.declare_var(I64);
    builder.declare_var_needs_stack_map(first);
    let input = builder.block_params(entry)[0];
    builder.def_var(first, input);
    // A completed splice returns this SSA value onto its caller's operand stack.
    let saved = builder.use_var(first);
    if explicit_value_root {
        builder.declare_value_needs_stack_map(saved);
    }
    // The next splice initializes its local in the same basic block.
    let second = if reuse { first } else {
        let variable = builder.declare_var(I64);
        builder.declare_var_needs_stack_map(variable);
        variable
    };
    let zero = builder.ins().iconst(I64, 0);
    builder.def_var(second, zero);
    builder.ins().call(callback, &[]);
    builder.ins().return_(&[saved]);
    builder.seal_all_blocks();
    builder.finalize();
    function.display().to_string()
}

#[test]
fn separate_local_identities_keep_the_saved_reference_rooted() {
    let ir = crossing(false, false);
    assert!(ir.contains("stack_map=["), "{ir}");
}

#[test]
fn naive_same_block_reuse_loses_the_old_variable_definition() {
    // This pins the 0.132.1 hazard, not an acceptable emitter implementation.
    // Revisit the pooling proof if a dependency update changes this contract.
    let ir = crossing(true, false);
    assert!(!ir.contains("stack_map=["), "dependency behavior changed:\n{ir}");
}

#[test]
fn explicit_ssa_value_root_survives_local_identity_reuse() {
    let ir = crossing(true, true);
    assert!(ir.contains("stack_map=["), "{ir}");
}
