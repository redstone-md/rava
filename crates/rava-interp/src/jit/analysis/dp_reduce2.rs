//! Two-accumulator reduction: sum and sum-of-squares folded in one pass.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Recognise the dual-accumulator template above (long accs seeded to op identity). `None` on
/// any deviation.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_reduce2(bc: &[u8]) -> Option<Reduce2Loop> {
    use crate::parallel_pool::ReduceOp;
    let lload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x16 => Some(*bc.get(p + 1)? as usize), op @ 0x1e..=0x21 => Some((op - 0x1e) as usize), _ => None }
    };
    let iload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x15 => Some(*bc.get(p + 1)? as usize), op @ 0x1a..=0x1d => Some((op - 0x1a) as usize), _ => None }
    };
    let mut pc = 0usize;
    // Prologue: acc1 = 0; acc2 = 0; i = c0 (both accs same width, seeded to op identity).
    let (init1, l1, lg1) = decode_acc_init(bc, pc)?;
    let acc_is_long = lg1;
    // Accumulator store: long → (l)store, int → (i)store — width from the seed.
    let acc_store = |p: usize| -> bool {
        if acc_is_long { matches!(bc[p], 0x37 | 0x3f..=0x42) } else { matches!(bc[p], 0x36 | 0x3b..=0x3e) }
    };
    pc += l1;
    if !acc_store(pc) { return None; }
    let (acc1_local, l) = decode_store(bc, pc)?;
    pc += l;
    let (init2, l2, lg2) = decode_acc_init(bc, pc)?;
    if lg2 != acc_is_long { return None; } // both accumulators must be the same width
    pc += l2;
    if !acc_store(pc) { return None; }
    let (acc2_local, l) = decode_store(bc, pc)?;
    if acc2_local == acc1_local { return None; }
    pc += l;
    let (c0, l) = decode_iconst(bc, pc)?;
    pc += l;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) { return None; } // istore i
    let (iv_local, l) = decode_store(bc, pc)?;
    if iv_local == acc1_local || iv_local == acc2_local { return None; }
    pc += l;
    // Header: iload i; (iload LIM | iconst LIM); if_icmpge exit.
    let header = pc;
    if iload_local(pc)? != iv_local { return None; }
    pc += insn_len_at(bc, pc)?;
    let (limit_local, lim_const) = if let Some(l) = iload_local(pc) {
        (Some(l), 0)
    } else if let Some((c, _)) = decode_iconst(bc, pc) {
        (None, c)
    } else {
        return None;
    };
    pc += insn_len_at(bc, pc)?;
    if *bc.get(pc)? != 0xa2 { return None; }
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;
    // Two fold statements: `lload accK; <valK>; <opK>; lstore accK`. The value ends at the
    // fold op immediately before the matching lstore (the value has no stores of its own).
    let fold_stmt = |acc: usize, mut q: usize| -> Option<(usize, usize, ReduceOp, usize)> {
        // returns (val_start, val_end (= fold pc), op, next pc after the acc store).
        let accload = if acc_is_long { lload_local(q) } else { iload_local(q) };
        if accload? != acc { return None; }
        q += insn_len_at(bc, q)?;
        let val_start = q;
        // walk to the store of this acc; the instruction before it is the fold.
        let mut prev = q;
        loop {
            if q >= bc.len() { return None; }
            if acc_store(q) {
                let (a, _) = decode_store(bc, q)?;
                if a != acc { return None; }
                let op = ReduceOp::from_fold_opcode(bc[prev])?;
                let fold_ok = if acc_is_long {
                    matches!(bc[prev], 0x61 | 0x69 | 0x83 | 0x7f | 0x81) // ladd/lmul/land/lor/lxor
                } else {
                    matches!(bc[prev], 0x60 | 0x68 | 0x82 | 0x7e | 0x80) // iadd/imul/iand/ior/ixor
                };
                if !fold_ok { return None; }
                return Some((val_start, prev, op, q + insn_len_at(bc, q)?));
            }
            prev = q;
            q += insn_len_at(bc, q)?;
        }
    };
    let (v1s, v1e, op1, q) = fold_stmt(acc1_local, pc)?;
    let (v2s, v2e, op2, q) = fold_stmt(acc2_local, q)?;
    // Each accumulator's seed must equal its fold's identity (else the parallel partials,
    // combined from the op identity, would be off by the seed).
    if init1 != op1.identity() || init2 != op2.identity() {
        return None;
    }
    // iinc i,1 ; goto header ; exit = <return expr over acc1/acc2> ; lreturn.
    let mut pc = q;
    if *bc.get(pc)? != 0x84 || *bc.get(pc + 1)? as usize != iv_local || *bc.get(pc + 2)? as i8 as i32 != 1 {
        return None;
    }
    pc += 3;
    if *bc.get(pc)? != 0xa7 || (pc as i32 + s16(bc, pc + 1)) as usize != header { return None; }
    pc += 3;
    if pc != exit_pc { return None; }
    // Return expression [exit_pc, (l/i)return). Must read only acc1/acc2 at the acc width (no
    // other locals / wrong-width loads / array access), and end with the matching return.
    let ret_op = if acc_is_long { 0xad } else { 0xac }; // lreturn / ireturn
    let ret_start = pc;
    let mut r = pc;
    while r < bc.len() && *bc.get(r)? != ret_op {
        let accld = if acc_is_long { lload_local(r) } else { iload_local(r) };
        if let Some(l) = accld {
            if l != acc1_local && l != acc2_local { return None; }
        } else {
            let wrong_width = if acc_is_long { iload_local(r).is_some() } else { lload_local(r).is_some() };
            if wrong_width || aload_local_at(bc, r).is_some() {
                return None;
            }
        }
        r += insn_len_at(bc, r)?;
    }
    if r >= bc.len() || *bc.get(r)? != ret_op || r + 1 != bc.len() { return None; }
    let ret_range = (ret_start, r);
    if limit_local.is_none() && (lim_const as i64 - c0 as i64) < MIN_PARALLEL_TRIP { return None; }
    if c0 < 0 { return None; }
    // Collect array captures (`aload L; iload iv; (l/i)aload`) + scalar captures across both
    // value ranges. Any aload NOT in the canonical array-read shape → unsupported.
    let mut array_captures: Vec<(usize, bool)> = Vec::new();
    let mut captures: Vec<(usize, bool)> = Vec::new();
    for &(vs, ve) in &[(v1s, v1e), (v2s, v2e)] {
        let mut p = vs;
        while p < ve {
            if let Some(arr) = aload_local_at(bc, p) {
                let n1 = p + insn_len_at(bc, p)?;
                let n2 = n1 + insn_len_at(bc, n1)?;
                if iload_local(n1) != Some(iv_local) || !matches!(bc[n2], 0x2e | 0x2f | 0x31) {
                    return None; // aload not in a canonical a[i] read → unsupported
                }
                let is_long = matches!(bc[n2], 0x2f | 0x31);
                if arr == iv_local {
                    return None;
                }
                if !array_captures.iter().any(|(l, _)| *l == arr) {
                    array_captures.push((arr, is_long));
                }
                p = n2 + 1;
                continue;
            }
            if let Some(l) = iload_local(p) {
                if l != iv_local && !captures.iter().any(|(x, _)| *x == l) {
                    captures.push((l, false));
                }
            } else if let Some(l) = lload_local(p) {
                if !captures.iter().any(|(x, _)| *x == l) {
                    captures.push((l, true));
                }
            }
            p += insn_len_at(bc, p)?;
        }
    }
    Some(Reduce2Loop {
        acc1_local,
        acc2_local,
        iv_local,
        c0,
        limit_local,
        lim_const,
        op1,
        op2,
        val1_range: (v1s, v1e),
        val2_range: (v2s, v2e),
        ret_range,
        array_captures,
        captures,
        acc_is_long,
    })
}
