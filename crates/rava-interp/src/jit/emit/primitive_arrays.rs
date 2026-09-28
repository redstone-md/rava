//! Inline producer for structurally confined primitive arrays.
//!
//! Admission is computed from bytecode and CFG facts before emission. This module only
//! materializes the selected representation; it never inspects class or method names.

use crate::jit::*;

/// Diagnostic-only dynamic allocation census. The emitted callback is absent unless
/// explicitly armed, so performance A/B runs contain no counter branch or host crossing.
#[cfg(feature = "next-gen-gc")]
fn primitive_array_stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARENA_PRIM_ARRAY_STATS").is_some())
}

/// Emit a primitive `newarray` as an inline arena bump.
///
/// A negative length and a twice-full nursery take the historical `rava_newarray`
/// callback, preserving its tagged-handle result and exception behavior. The first
/// full-nursery result takes one Cheney safepoint and one retry. Every callback edge
/// spills and reloads the live frame, and the continuation receives forwarded
/// operand-stack references as block parameters.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_inline_primitive_newarray(
    b: &mut FunctionBuilder,
    env: &Env,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    cursor_var: Variable,
    locals_ptr: Value,
    atype: u8,
    atype_value: Value,
    count: Value,
) -> Option<()> {
    let class_id = crate::nextgen::primitive_array_cid(atype)?;
    let cell_bytes = crate::nextgen::PRIMITIVE_ARRAY_CELL_BYTES as i64;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter_map(|(i, sv)| (sv.t == JT::Ref).then_some(i))
        .collect();
    let nref = ref_slots.len();

    let attempt = b.create_block();
    let overflow = b.create_block();
    let retry = b.create_block();
    let fallback = b.create_block();
    let initialized = b.create_block();
    let zero_loop = b.create_block();
    let cont = b.create_block();
    for block in [attempt, overflow, retry, fallback] {
        for _ in 0..nref {
            b.append_block_param(block, I64);
        }
    }
    b.append_block_param(initialized, I64);
    for _ in 0..nref {
        b.append_block_param(initialized, I64);
    }
    b.append_block_param(zero_loop, I64);
    b.append_block_param(cont, I64);
    for _ in 0..nref {
        b.append_block_param(cont, I64);
    }

    let initial_refs: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();
    let negative = b.ins().icmp_imm(IntCC::SignedLessThan, count, 0);
    b.ins()
        .brif(negative, fallback, &initial_refs, attempt, &initial_refs);

    // First bump attempt. A fit publishes through `initialized`; a miss gets exactly
    // one safepoint before the retry block.
    b.switch_to_block(attempt);
    b.seal_block(attempt);
    let attempt_refs: Vec<Value> = b.block_params(attempt).to_vec();
    let cursor = b.use_var(cursor_var);
    let next = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(0));
    let end = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(8));
    let bytes = b.ins().imul_imm(count, cell_bytes);
    let size = b.ins().iadd_imm(bytes, crate::nextgen::HDR as i64);
    let new_top = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, new_top, end);
    let mut success_args: Vec<cranelift::codegen::ir::BlockArg> = vec![next.into()];
    success_args.extend(
        attempt_refs
            .iter()
            .copied()
            .map(|value| -> cranelift::codegen::ir::BlockArg { value.into() }),
    );
    let overflow_args: Vec<cranelift::codegen::ir::BlockArg> =
        attempt_refs.iter().copied().map(Into::into).collect();
    b.ins()
        .brif(fits, initialized, &success_args, overflow, &overflow_args);

    // Nursery full: root the frame, let the collector republish the cursor, and carry
    // every forwarded stack reference into a single retry.
    b.switch_to_block(overflow);
    b.seal_block(overflow);
    let overflow_refs: Vec<Value> = b.block_params(overflow).to_vec();
    adopt_ref_params(stack, &ref_slots, &overflow_refs);
    spill_for_gc(b, vars, stack, locals_ptr);
    let _ = b.ins().call(env.arena_safepoint_fn, &[]);
    reload_locals_after_gc(b, vars, locals_ptr);
    let retry_args = reloaded_ref_args(b, vars.len(), stack, &ref_slots, locals_ptr);
    b.ins().jump(retry, &retry_args);

    // The one retry. A second miss falls into the ordinary callback rather than a
    // scratch allocation, so the observable value remains a real tagged array handle.
    b.switch_to_block(retry);
    b.seal_block(retry);
    let retry_refs: Vec<Value> = b.block_params(retry).to_vec();
    let cursor = b.use_var(cursor_var);
    let next = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(0));
    let end = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(8));
    let new_top = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, new_top, end);
    let mut retry_success: Vec<cranelift::codegen::ir::BlockArg> = vec![next.into()];
    retry_success.extend(
        retry_refs
            .iter()
            .copied()
            .map(|value| -> cranelift::codegen::ir::BlockArg { value.into() }),
    );
    let retry_fallback: Vec<cranelift::codegen::ir::BlockArg> =
        retry_refs.iter().copied().map(Into::into).collect();
    b.ins()
        .brif(fits, initialized, &retry_success, fallback, &retry_fallback);
    b.seal_block(initialized);
    b.seal_block(fallback);

    // The historical allocation edge serves both negative lengths and a second TLAB miss.
    b.switch_to_block(fallback);
    let fallback_refs: Vec<Value> = b.block_params(fallback).to_vec();
    adopt_ref_params(stack, &ref_slots, &fallback_refs);
    spill_for_gc(b, vars, stack, locals_ptr);
    let call = b.ins().call(env.newarray, &[atype_value, count]);
    let result = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut fallback_cont = vec![result.into()];
    fallback_cont.extend(reloaded_ref_args(
        b,
        vars.len(),
        stack,
        &ref_slots,
        locals_ptr,
    ));
    b.ins().jump(cont, &fallback_cont);

    // Successful arena allocation: publish the top, write the dynamic header, and clear
    // every eight-byte cell. The zero-length edge must not execute a body store.
    b.switch_to_block(initialized);
    let initialized_params: Vec<Value> = b.block_params(initialized).to_vec();
    let object = initialized_params[0];
    let cursor = b.use_var(cursor_var);
    let new_top = b.ins().iadd(object, size);
    b.ins()
        .store(MemFlags::trusted(), new_top, cursor, Offset32::new(0));
    let count_word = b.ins().ishl_imm(count, 32);
    let cid = b.ins().iconst(I64, class_id as i64);
    let header = b.ins().bor(count_word, cid);
    b.ins()
        .store(MemFlags::trusted(), header, object, Offset32::new(0));
    // Reuse the existing raw-aware arraylength callback as an opt-in census tick.
    // Its callback ordinal is visible in `[CB]`; it only reads the header and cannot GC.
    if primitive_array_stats_enabled() {
        b.ins().call(env.arrlen, &[object]);
    }
    let zero = b.ins().iconst(I64, 0);
    b.ins()
        .store(MemFlags::trusted(), zero, object, Offset32::new(8));
    if crate::nextgen::HDR > 16 {
        b.ins()
            .store(MemFlags::trusted(), zero, object, Offset32::new(16));
    }
    let cells = b.ins().iadd_imm(object, crate::nextgen::HDR as i64);
    let empty = b.ins().icmp_imm(IntCC::Equal, count, 0);
    let initialized_args: Vec<cranelift::codegen::ir::BlockArg> =
        initialized_params.iter().copied().map(Into::into).collect();
    b.ins()
        .brif(empty, cont, &initialized_args, zero_loop, &[cells.into()]);

    b.switch_to_block(zero_loop);
    let cell = b.block_params(zero_loop)[0];
    b.ins()
        .store(MemFlags::trusted(), zero, cell, Offset32::new(0));
    let next_cell = b.ins().iadd_imm(cell, cell_bytes);
    let object_end = b.ins().iadd(object, size);
    let more = b.ins().icmp(IntCC::UnsignedLessThan, next_cell, object_end);
    b.ins().brif(
        more,
        zero_loop,
        &[next_cell.into()],
        cont,
        &initialized_args,
    );
    b.seal_block(zero_loop);
    b.seal_block(cont);

    b.switch_to_block(cont);
    let cont_params: Vec<Value> = b.block_params(cont).to_vec();
    for (k, &slot) in ref_slots.iter().enumerate() {
        stack[slot].v = cont_params[k + 1];
    }
    push(stack, cont_params[0], JT::Ref);
    Some(())
}

#[cfg(feature = "next-gen-gc")]
fn adopt_ref_params(stack: &mut [SV], slots: &[usize], params: &[Value]) {
    for (k, &slot) in slots.iter().enumerate() {
        stack[slot].v = params[k];
    }
}

#[cfg(feature = "next-gen-gc")]
fn reloaded_ref_args(
    b: &mut FunctionBuilder,
    local_count: usize,
    stack: &[SV],
    slots: &[usize],
    locals_ptr: Value,
) -> Vec<cranelift::codegen::ir::BlockArg> {
    slots
        .iter()
        .map(|&slot| reload_ref_slot(b, local_count, slot, stack[slot].v, locals_ptr).into())
        .collect()
}
