//! Exact heap-BitSet operations emitted directly into compiled guest code.

use crate::jit::*;

/// Inline `BitSet.isEmpty()Z` on the exact heap representation. The callback remains the
/// semantic fallback for subclasses, arena receivers and any unexpected backing shape.
pub(crate) fn emit_bitset_isempty_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
) -> Option<()> {
    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls = b.create_block();
    let read = b.create_block();
    let load = b.create_block();
    let loop_head = b.create_block();
    b.append_block_param(loop_head, I32);
    let loop_body = b.create_block();
    let advance = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }
    let pass: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();

    // Only a real heap Ref may be dereferenced by `emit_heap_ref_class_key`.
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls, &[], slow, &[]);

    // The class word is runtime-published from a live BitSet receiver. An unset word or any
    // subclass takes the original callback, preserving virtual-dispatch semantics.
    b.switch_to_block(cls);
    b.seal_block(cls);
    let key_addr = b.ins().iconst(
        I64,
        (&BITSET_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let expected = b
        .ins()
        .load(I64, MemFlags::trusted(), key_addr, Offset32::new(0));
    let actual = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let same = b.ins().icmp(IntCC::Equal, actual, expected);
    let ready = b.ins().icmp_imm(IntCC::NotEqual, expected, 0);
    let exact = b.ins().band(same, ready);
    b.ins().brif(exact, read, &[], slow, &[]);

    // Exact BitSet with no `$bits` array is empty, matching `jit_bitset_isempty`.
    b.switch_to_block(read);
    b.seal_block(read);
    let bits = native_getfield_ref(b, env, array_desc_cell, recv, env.bits_fid);
    let bits_hi = b.ins().ushr_imm(bits, 32);
    let is_array = b.ins().icmp_imm(IntCC::Equal, bits_hi, ARRAY_TAG_HI as i64);
    let yes = b.ins().iconst(I32, 1);
    let mut empty_args: Vec<cranelift::codegen::ir::BlockArg> = vec![yes.into()];
    empty_args.extend_from_slice(&pass);
    b.ins().brif(is_array, load, &[], merge, &empty_args);

    // Scan the packed long[] in machine code. No call or safepoint exists in this loop, so the
    // descriptor data pointer and the carried references cannot move before the merge.
    b.switch_to_block(load);
    b.seal_block(load);
    let (data, len) = load_array_desc(b, array_desc_cell, bits);
    let zero = b.ins().iconst(I32, 0);
    b.ins().jump(loop_head, &[zero.into()]);

    b.switch_to_block(loop_head);
    let index = b.block_params(loop_head)[0];
    let done = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    b.ins().brif(done, merge, &empty_args, loop_body, &[]);

    b.switch_to_block(loop_body);
    b.seal_block(loop_body);
    let index64 = b.ins().uextend(I64, index);
    let offset = b.ins().ishl_imm(index64, 3);
    let address = b.ins().iadd(data, offset);
    let word = b
        .ins()
        .load(I64, MemFlags::trusted(), address, Offset32::new(0));
    let nonzero = b.ins().icmp_imm(IntCC::NotEqual, word, 0);
    let no = b.ins().iconst(I32, 0);
    let mut nonempty_args: Vec<cranelift::codegen::ir::BlockArg> = vec![no.into()];
    nonempty_args.extend_from_slice(&pass);
    b.ins().brif(nonzero, merge, &nonempty_args, advance, &[]);

    b.switch_to_block(advance);
    b.seal_block(advance);
    let next = b.ins().iadd_imm(index, 1);
    b.ins().jump(loop_head, &[next.into()]);
    b.seal_block(loop_head);

    // Full semantic oracle, including subclass dispatch and alternate receiver encodings.
    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let class = b.ins().iconst(I64, env.class_ptr);
    let cp = b.ins().iconst(I64, cp as i64);
    let kind = b.ins().iconst(I64, 1);
    let argc = b.ins().iconst(I64, 1);
    let call = b
        .ins()
        .call(env.bitset_isempty, &[class, cp, kind, buf, argc]);
    let result = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let result = b.ins().ireduce(I32, result);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![result.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let params: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = params[k + 1];
    }
    stack.push(SV {
        v: params[0],
        t: JT::Int,
        src: None,
        #[cfg(feature = "next-gen-gc")]
        arena: false,
    });
    Some(())
}

/// Default-on kill switch used for same-binary product A/B.
pub(crate) fn bitset_isempty_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_BITSET_ISEMPTY_INLINE").is_none())
}
