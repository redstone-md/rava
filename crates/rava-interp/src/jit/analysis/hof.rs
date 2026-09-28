//! Higher-order shapes: the wrapper that forwards to a SAM and the consumer that calls one.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Strict consumer body admitted by adjacent higher-order fusion. The only call
/// is the proven SAM interface site; observable closure uses, allocation, heap
/// mutation, monitors, and exception machinery are excluded. Ordinary inlining
/// deliberately continues to reject invokeinterface.
///
/// The 96-byte budget here is NOT the leaf budget widened in `admission.rs` and deliberately stays
/// where it is: this body keeps a real `invokeinterface` mid-splice, which spills through the
/// CALLER's frame buffer, so the callee's frame size has to stay small relative to its caller's.
/// The wide leaf band earns its width precisely by proving it never spills at all.
pub(crate) fn is_hof_consumer_inlinable(code: &Code, descriptor: &str, sam_pc: usize) -> bool {
    if !code.exceptions.is_empty()
        || code.bytecode.is_empty()
        || code.bytecode.len() > 96
        || ret_jt(descriptor).is_none()
    {
        return false;
    }
    let bc = &code.bytecode;
    let Some(leaders) = find_leaders(bc) else {
        return false;
    };
    let mut pc = 0usize;
    let mut saw_sam = false;
    while pc < bc.len() {
        let Some(len) = insn_len_at(bc, pc) else {
            return false;
        };
        match bc[pc] {
            // Same landmine as `is_inlinable`: this body is spliced by `emit_inline`, which walks
            // with `insn_len(op)` and has no arm for the variable-length switches — the `None` it
            // returns aborts the CALLER's compile, not just the fusion. `insn_len_at` above decodes
            // them fine, so nothing else here would catch it. Kill switch shared with the inliner.
            0xaa | 0xab if wide_inline_enabled() => return false,
            0xb9 if pc == sam_pc && !saw_sam => saw_sam = true,
            0x6c | 0x70
                if !leaders.contains(&pc)
                    && adjacent_i32_divisor_value(bc, pc)
                        .is_some_and(fast_i32_divisor) => {}
            // Calls, allocation, class initialization, writes, identity tests,
            // resolution, monitors, throws, and fault-capable operations are
            // outside tracer-1's proof. Reference captures remain live until
            // the SAM site, so no preceding operation may allocate or move them.
            0x12..=0x14
            | 0x2e..=0x35
            | 0x4f..=0x56
            | 0x6c
            | 0x6d
            | 0x70..=0x73
            | 0xa5
            | 0xa6
            | 0xb2..=0xb8
            | 0xb9
            | 0xba..=0xbe
            | 0xbf
            | 0xc0..=0xc5
            => return false,
            _ => {}
        }
        pc += len;
    }
    saw_sam
}

/// Implementation body admitted by structural HOF fusion. Parameters and return
/// types are separately restricted to primitives / stable primitive-array
/// handles. Reject every opcode that can materialize a moving reference before a
/// fault callback; ordinary leaf inlining remains broader.
pub(crate) fn is_hof_implementation_inlinable(code: &Code, descriptor: &str) -> bool {
    if !is_inlinable(code, descriptor, false) {
        return false;
    }
    let mut pc = 0usize;
    while pc < code.bytecode.len() {
        let op = code.bytecode[pc];
        let Some(len) = insn_len_at(&code.bytecode, pc) else {
            return false;
        };
        if matches!(
            op,
            0x12..=0x14
                | 0x32
                | 0x6c
                | 0x6d
                | 0x70..=0x73
                | 0xb2..=0xb5
                | 0xbe
                | 0xbf
                | 0xc0..=0xc3
                | 0xc5
        ) {
            return false;
        }
        pc += len;
    }
    true
}

/// Admit a small wrapper only when every dynamic/static call in it belongs to
/// an already-proven adjacent HOF pair. This does not recognize method names or
/// broaden ordinary inlining: missing, extra, or ambiguous calls fail closed.
pub(crate) fn is_hof_wrapper_inlinable(
    code: &Code,
    descriptor: &str,
    plans: &std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
) -> bool {
    let pairs: std::collections::HashMap<usize, usize> = plans
        .iter()
        .map(|(&indy_pc, plan)| (indy_pc, plan.consumer_pc))
        .collect();
    is_hof_wrapper_shape(code, descriptor, &pairs)
}

pub(crate) fn is_hof_wrapper_shape(
    code: &Code,
    descriptor: &str,
    pairs: &std::collections::HashMap<usize, usize>,
) -> bool {
    if pairs.is_empty()
        || !code.exceptions.is_empty()
        || code.bytecode.is_empty()
        || code.bytecode.len() > INLINE_MAX_BYTECODE
        || ret_jt(descriptor).is_none()
    {
        return false;
    }
    let consumers: std::collections::HashSet<usize> = pairs.values().copied().collect();
    if consumers.len() != pairs.len() {
        return false;
    }
    let bc = &code.bytecode;
    let mut seen = 0usize;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        if is_excluded(op) {
            return false;
        }
        let Some(len) = insn_len_at(bc, pc) else {
            return false;
        };
        match op {
            0xba => {
                let Some(&consumer_pc) = pairs.get(&pc) else {
                    return false;
                };
                if consumer_pc != pc + len || bc.get(consumer_pc) != Some(&0xb8) {
                    return false;
                }
                seen += 1;
            }
            0xb8 if consumers.contains(&pc) => {}
            // Spliced by `emit_inline`, whose `insn_len(op)` cannot decode a switch and aborts the
            // caller's whole compile on the `None`. See `is_inlinable`.
            0xaa | 0xab if wide_inline_enabled() => return false,
            0xb3 | 0xb5..=0xbd | 0xc1 | 0x4f..=0x56 => return false,
            _ => {}
        }
        pc += len;
    }
    seen == pairs.len() && find_leaders(bc).is_some()
}
