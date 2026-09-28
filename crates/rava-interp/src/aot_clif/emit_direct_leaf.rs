//! The Tracer 1 direct-leaf invokestatic arm: the guarded
//! fptr-branch/call_indirect lowering for sites carrying a `DirectLeafCall`
//! compile-time candidate, plus the fallback-close merge into the shared
//! resume block.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::aot_clif::*;
use crate::cnp::ConstDesc;

// ═══════════════ Tracer 1: the direct-leaf invokestatic arm ═══════════════
//
// `emit_direct_leaf_arm` lowers an `invokestatic` site carrying a
// `DirectLeafCall` compile-time candidate and, when it takes the arm, hands
// back the shared resume block both branches merge into. The site lowering is
//
//   ┌ entry ─┐ load ConstDesc::AotLeaf(owner,name,desc) fptr from the const
//   │        │ table, then brif(fptr == 0):
//   ▼        ▼
//  direct   fallback (the CALLER emits the old arms + rava_invoke tail here,
//  call_    bit-identical to the pre-Tracer lowering, and closes it with
//  indirect finish_direct_leaf_fallback → jump resume)
//   │  ▲
//   │  └──────────────┐
//   ▼                 ▼
//  resume (one i64 block param per post-invoke stack cell — both arms carry
//  the same shape: the merged result + the cells below the operands).
//
// The DIRECT branch passes the hidden consts pointer as a literal 0 (a
// verified leaf's const table is empty and link-validated, so 0 fails loudly
// on any accidental deref), the proven primitive operands as i64 cells in the
// ordinary System V integer registers, and does NOT touch argbuf, spill,
// reload, call_fault, rava_invoke or aot_run_input: a classifier-verified
// no-GC leaf cannot throw, allocate, or safepoint.
//
// Fail-closed contract: on any unsupported shape (arity mismatch against the
// authoritative `signature.params`, more than five user args, a ref operand,
// a wide mismatch) the function emits NOTHING beyond the const slot and
// returns `Ok(None)` — the caller falls through to the pre-existing arms and
// never refuses the whole method.
//
// Per the shared contract, the direct-leaf arm runs BEFORE the
// numeric/object-leaf arms so a native-ABI target is never shadowed by
// same-class inlining.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_direct_leaf_arm(
    b: &mut FunctionBuilder,
    stack: &mut Vec<(Value, bool, bool)>,
    consts_ptr: Value,
    consts: &mut Vec<crate::cnp::ConstDesc>,
    leaf: &crate::aot_clif::direct_leaf::DirectLeafCall,
    ncells: u32,
    info: &InvokeInfo,
) -> Option<Option<cranelift::codegen::ir::Block>> {
    use cranelift::codegen::ir::{condcodes::IntCC, types::I64, AbiParam, InstBuilder, Signature};
    use cranelift::codegen::isa::CallConv;

    // RESULT-SHAPE GUARD: a void/ref return can never be an admitted candidate
    // (the classifier admits I/J/Z returns only), and the typed-stack width
    // must agree with the descriptor — a PrimKind::Long return must be tagged
    // wide, an I/Z return narrow. Any mismatch → the old path, fail closed.
    let ret_long = leaf.signature.ret == crate::aot_clif::direct_leaf::PrimKind::Long;
    if info.ret_void || info.ret_ref || info.ret_wide != ret_long {
        return Some(None);
    }

    // Arity: the descriptor is authoritative — exactly one PrimKind per user
    // arg. The call's operand cells must match it, and it must fit the SysV
    // integer-register budget (RDI is the hidden consts pointer; RSI..R9 are
    // the five user registers). Above five → the old path.
    if ncells as usize != leaf.signature.params.len() || ncells > 5 {
        return Some(None);
    }

    // EMIT-TIME ARG PROOF: each operand cell must be non-ref, and its
    // wide-ness must match the descriptor's per-position kind (J → one WIDE
    // i64 cell; I/Z → one narrow sign-extended cell; Long counts as ONE cell).
    let base = stack.len() - ncells as usize;
    for (k, (_, is_ref, is_wide)) in stack[base..].iter().enumerate() {
        let param_long = leaf.signature.params[k] == crate::aot_clif::direct_leaf::PrimKind::Long;
        if *is_ref || *is_wide != param_long {
            return Some(None);
        }
    }
    let args: Vec<Value> = stack[base..].iter().map(|(v, _, _)| *v).collect();

    // Symbolic leaf pointer, interned ONCE per site. The LINK resolves it to
    // the target's compiled-leaf function pointer or 0 (never None), so the
    // zero arm is always the reachable fallback at runtime.
    let slot = intern_const(
        consts,
        ConstDesc::AotLeaf(
            leaf.owner.clone(),
            leaf.name.clone(),
            leaf.descriptor.clone(),
        ),
    );
    let fptr = load_const(b, slot, consts_ptr);

    let direct = b.create_block();
    let fallback = b.create_block();
    let resume = b.create_block();
    let is_zero = b.ins().icmp_imm(IntCC::Equal, fptr, 0);
    b.ins().brif(is_zero, fallback, &[], direct, &[]);
    b.seal_block(direct);
    b.seal_block(fallback);

    // ── DIRECT branch: System V call_indirect, spill-free ──
    b.switch_to_block(direct);
    // The direct ABI is System V BY CONTRACT — never inherit the enclosing
    // method's call convention.
    let mut dsig = Signature::new(CallConv::SystemV);
    dsig.params.push(AbiParam::new(I64)); // hidden consts pointer (RDI)
    for _ in 0..ncells {
        dsig.params.push(AbiParam::new(I64)); // one i64 cell per user arg
    }
    dsig.returns.push(AbiParam::new(I64));
    let sref = b.import_signature(dsig);
    let mut call_args: Vec<Value> = Vec::with_capacity(ncells as usize + 1);
    call_args.push(b.ins().iconst(I64, 0)); // hidden consts ptr: literal NULL
    call_args.extend_from_slice(&args);
    let call = b.ins().call_indirect(sref, fptr, &call_args);
    let result = b.inst_results(call)[0];
    // Post-invoke stack: the cells BELOW the operands keep their identity (they
    // dominate `direct`), the result cell replaces the operand cells. The
    // caller's operand Vec is NOT touched here — the fallback arms must still
    // see the PRE-invoke stack to run verbatim; the resume-block params
    // rewrite the caller's stack after the fallback closes (see
    // `finish_direct_leaf_fallback`).
    let post_len = base + 1;
    // Append the resume block params FIRST (a block's params must exist before
    // any edge references them), then jump from the direct branch.
    for _ in 0..post_len {
        b.append_block_param(resume, I64);
    }
    let mut direct_edge: Vec<cranelift::codegen::ir::BlockArg> =
        stack[..base].iter().map(|(v, _, _)| (*v).into()).collect();
    direct_edge.push(result.into());
    b.ins().jump(resume, &direct_edge);

    // Leave the builder on the FALLBACK block for the caller's old arms.
    b.switch_to_block(fallback);
    Some(Some(resume))
}

/// Close the direct arm's FALLBACK block after the caller has run the old
/// arms (numeric/object leaves + the unchanged rava_invoke tail) verbatim:
/// jump into the shared resume block with the fallback's post-invoke stack —
/// the same shape the direct branch jumped with — then continue the method
/// there.
///
/// CRITICAL merge semantics: the fallback's SSA values (its own result cell,
/// the below-stack cells possibly rewritten by the GC spill/reload) become
/// the resume block's params; the DIRECT branch's values joined the same
/// params earlier. The CALLER rewrites its operand stack to the resume block
/// params afterwards (it knows the post-invoke tagging), so the method's
/// continuation references dominating values.
pub(crate) fn finish_direct_leaf_fallback(
    b: &mut FunctionBuilder,
    stack: &[(Value, bool, bool)],
    resume: cranelift::codegen::ir::Block,
    post_len: usize,
) {
    use cranelift::codegen::ir::InstBuilder;
    let edge: Vec<cranelift::codegen::ir::BlockArg> =
        stack.iter().map(|(v, _, _)| (*v).into()).collect();
    debug_assert_eq!(
        edge.len(),
        post_len,
        "fallback edge arity must match the direct arm's"
    );
    b.ins().jump(resume, &edge);
    b.seal_block(resume);
    b.switch_to_block(resume);
}
