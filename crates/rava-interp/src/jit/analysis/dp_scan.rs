//! Prefix sum, predicate count and peak search.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Recognise the rigid whole-method prefix-scan shape above (long arrays, `acc = op(acc,
/// in[i]); out[i] = acc`, seed == identity). `None` on any deviation.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_scan(bc: &[u8]) -> Option<ScanLoop> {
    let mut pc = 0usize;
    // Prologue: acc = <const>; i = c0.
    let (acc_init, l1, init_long) = decode_acc_init(bc, pc)?;
    let long = init_long; // long[] (8-byte) vs int[] (4-byte) scan
    pc += l1;
    let acc_store_ok = if long { matches!(bc[pc], 0x37 | 0x3f..=0x42) } else { matches!(bc[pc], 0x36 | 0x3b..=0x3e) };
    if !acc_store_ok {
        return None; // (l/i)store acc
    }
    let (acc_local, l2) = decode_store(bc, pc)?;
    pc += l2;
    let (c0, l3) = decode_iconst(bc, pc)?;
    pc += l3;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) {
        return None; // istore i
    }
    let (iv_local, l4) = decode_store(bc, pc)?;
    if iv_local == acc_local {
        return None;
    }
    pc += l4;
    // Header: iload iv; (iload LIM | iconst LIM); if_icmpge exit.
    let header = pc;
    let (h_iv, _, lh1) = decode_load(bc, pc)?;
    if !matches!(bc[pc], 0x15 | 0x1a..=0x1d) || h_iv != iv_local {
        return None;
    }
    pc += lh1;
    let (limit_local, lim_const, lh2) = if matches!(bc[pc], 0x15 | 0x1a..=0x1d) {
        let (l, _, ln) = decode_load(bc, pc)?;
        (Some(l), 0, ln)
    } else if let Some((c, cn)) = decode_iconst(bc, pc) {
        (None, c, cn)
    } else {
        return None;
    };
    pc += lh2;
    if *bc.get(pc)? != 0xa2 {
        return None;
    }
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;

    // Body: lload acc; aload in; iload iv; laload; <long fold>; lstore acc;
    //       aload out; iload iv; lload acc; lastore.
    let lload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x16 => Some(*bc.get(p + 1)? as usize), op @ 0x1e..=0x21 => Some((op - 0x1e) as usize), _ => None }
    };
    let iload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x15 => Some(*bc.get(p + 1)? as usize), op @ 0x1a..=0x1d => Some((op - 0x1a) as usize), _ => None }
    };
    let aload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x19 => Some(*bc.get(p + 1)? as usize), op @ 0x2a..=0x2d => Some((op - 0x2a) as usize), _ => None }
    };
    // Width-typed acc load: long→lload, int→iload.
    let accload = |p: usize| -> Option<usize> { if long { lload_local(p) } else { iload_local(p) } };
    let mut q = pc;
    if accload(q)? != acc_local { return None; }
    q += insn_len_at(bc, q)?;
    let in_local = aload_local(q)?;
    q += insn_len_at(bc, q)?;
    if iload_local(q)? != iv_local { return None; }
    q += insn_len_at(bc, q)?;
    let load_ok = if long { *bc.get(q)? == 0x2f } else { *bc.get(q)? == 0x2e }; // laload / iaload
    if !load_ok { return None; }
    q += 1;
    let op = crate::parallel_pool::ReduceOp::from_fold_opcode(*bc.get(q)?)?;
    let fold_ok = if long {
        matches!(bc[q], 0x61 | 0x69 | 0x83 | 0x7f | 0x81) // ladd/lmul/land/lor/lxor
    } else {
        matches!(bc[q], 0x60 | 0x68 | 0x82 | 0x7e | 0x80) // iadd/imul/iand/ior/ixor
    };
    if !fold_ok { return None; }
    q += 1;
    let acc_st_ok = if long { matches!(bc[q], 0x37 | 0x3f..=0x42) } else { matches!(bc[q], 0x36 | 0x3b..=0x3e) };
    if !acc_st_ok { return None; } // (l/i)store acc
    let (s_acc, sl) = decode_store(bc, q)?;
    if s_acc != acc_local { return None; }
    q += sl;
    let out_local = aload_local(q)?;
    q += insn_len_at(bc, q)?;
    if iload_local(q)? != iv_local { return None; }
    q += insn_len_at(bc, q)?;
    if accload(q)? != acc_local { return None; }
    q += insn_len_at(bc, q)?;
    let store_ok = if long { *bc.get(q)? == 0x50 } else { *bc.get(q)? == 0x4f }; // lastore / iastore
    if !store_ok { return None; }
    q += 1;
    // iinc iv, 1 ; goto header ; exit == return (void).
    if *bc.get(q)? != 0x84 || *bc.get(q + 1)? as usize != iv_local || *bc.get(q + 2)? as i8 as i32 != 1 {
        return None;
    }
    q += 3;
    if *bc.get(q)? != 0xa7 || (q as i32 + s16(bc, q + 1)) as usize != header {
        return None;
    }
    q += 3;
    if q != exit_pc || *bc.get(exit_pc)? != 0xb1 || exit_pc + 1 != bc.len() {
        return None;
    }
    // The parallel scan seeds each partial with op.identity(); require the guest's seed to
    // match (else the fused prefix would be off by op(acc_init, …)).
    if acc_init != op.identity() {
        return None;
    }
    if in_local == out_local || in_local == iv_local || out_local == iv_local || c0 < 0 {
        return None;
    }
    if limit_local.is_none() {
        let trip = (lim_const - c0) as i64;
        if trip < MIN_PARALLEL_TRIP {
            return None;
        }
    }
    Some(ScanLoop { iv_local, c0, limit_local, lim_const, in_local, out_local, op, elem_bytes: if long { 8 } else { 4 } })
}

/// Recognise the count-where-predicate template. `None` on any deviation.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_count(bc: &[u8]) -> Option<CountLoop> {
    let mut pc = 0usize;
    if !matches!(bc[pc], 0x03) { return None; } // iconst_0 (c=0)
    pc += 1;
    if !matches!(bc[pc], 0x3b..=0x3e | 0x36) { return None; } // istore c
    let (c_local, cl0) = decode_store(bc, pc)?;
    pc += cl0;
    let (c0, cl) = decode_iconst(bc, pc)?;
    pc += cl;
    if !matches!(bc[pc], 0x3b..=0x3e | 0x36) { return None; } // istore i
    let (iv_local, il) = decode_store(bc, pc)?;
    if iv_local == c_local { return None; }
    pc += il;
    let header = pc;
    let (h_iv, _, lh1) = decode_load(bc, pc)?;
    if !matches!(bc[pc], 0x15 | 0x1a..=0x1d) || h_iv != iv_local { return None; }
    pc += lh1;
    let (limit_local, lim_const, lh2) = if matches!(bc[pc], 0x15 | 0x1a..=0x1d) {
        let (l, _, ln) = decode_load(bc, pc)?;
        (Some(l), 0, ln)
    } else if let Some((c, cn)) = decode_iconst(bc, pc) {
        (None, c, cn)
    } else {
        return None;
    };
    pc += lh2;
    if *bc.get(pc)? != 0xa2 { return None; }
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;

    let aload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x19 => Some(*bc.get(p + 1)? as usize), op @ 0x2a..=0x2d => Some((op - 0x2a) as usize), _ => None }
    };
    let iload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x15 => Some(*bc.get(p + 1)? as usize), op @ 0x1a..=0x1d => Some((op - 0x1a) as usize), _ => None }
    };
    let mut q = pc;
    let in_local = aload_local(q)?;
    q += insn_len_at(bc, q)?;
    if iload_local(q)? != iv_local { return None; }
    q += insn_len_at(bc, q)?;
    let long = match *bc.get(q)? { 0x2f => true, 0x2e => false, _ => return None };
    q += 1;
    let (pred_cp, pred_const, pred_keep, skip_pc) = if long {
        if *bc.get(q)? != 0x14 { return None; }
        let cp = ((*bc.get(q + 1)? as u16) << 8) | *bc.get(q + 2)? as u16;
        q += 3;
        if *bc.get(q)? != 0x94 { return None; } // lcmp
        q += 1;
        let keep = match *bc.get(q)? {
            0x9e => IntCC::SignedGreaterThan,
            0x9b => IntCC::SignedGreaterThanOrEqual,
            0x9c => IntCC::SignedLessThan,
            0x9d => IntCC::SignedLessThanOrEqual,
            0x99 => IntCC::NotEqual,
            0x9a => IntCC::Equal,
            _ => return None,
        };
        let sp = (q as i32 + s16(bc, q + 1)) as usize;
        q += 3;
        (cp, 0i64, keep, sp)
    } else {
        let (c, cl) = decode_iconst(bc, q)?;
        q += cl;
        let keep = match *bc.get(q)? {
            0xa4 => IntCC::SignedGreaterThan,
            0xa1 => IntCC::SignedGreaterThanOrEqual,
            0xa2 => IntCC::SignedLessThan,
            0xa3 => IntCC::SignedLessThanOrEqual,
            0x9f => IntCC::NotEqual,
            0xa0 => IntCC::Equal,
            _ => return None,
        };
        let sp = (q as i32 + s16(bc, q + 1)) as usize;
        q += 3;
        (0u16, c as i64, keep, sp)
    };
    // Kept body: iinc c,1 only.
    if *bc.get(q)? != 0x84 || *bc.get(q + 1)? as usize != c_local || *bc.get(q + 2)? as i8 as i32 != 1 {
        return None;
    }
    q += 3;
    if q != skip_pc { return None; }
    if *bc.get(q)? != 0x84 || *bc.get(q + 1)? as usize != iv_local || *bc.get(q + 2)? as i8 as i32 != 1 {
        return None;
    }
    q += 3;
    if *bc.get(q)? != 0xa7 || (q as i32 + s16(bc, q + 1)) as usize != header { return None; }
    q += 3;
    if q != exit_pc { return None; }
    if iload_local(q)? != c_local { return None; }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? != 0xac || q + 1 != bc.len() { return None; } // ireturn
    if in_local == iv_local || c0 < 0 { return None; }
    if limit_local.is_none() && ((lim_const - c0) as i64) < MIN_PARALLEL_TRIP { return None; }
    Some(CountLoop {
        iv_local,
        c_local,
        c0,
        limit_local,
        lim_const,
        in_local,
        pred_keep,
        pred_cp,
        pred_const,
        elem_bytes: if long { 8 } else { 4 },
    })
}

/// Recognise the rigid long-array argmax template above. `None` on any deviation.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_argmax(bc: &[u8]) -> Option<ArgmaxLoop> {
    let aload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x19 => Some(*bc.get(p + 1)? as usize), op @ 0x2a..=0x2d => Some((op - 0x2a) as usize), _ => None }
    };
    let iload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x15 => Some(*bc.get(p + 1)? as usize), op @ 0x1a..=0x1d => Some((op - 0x1a) as usize), _ => None }
    };
    let lload_local = |p: usize| -> Option<usize> {
        match bc[p] { 0x16 => Some(*bc.get(p + 1)? as usize), op @ 0x1e..=0x21 => Some((op - 0x1e) as usize), _ => None }
    };
    let maxload = |long: bool, p: usize| -> Option<usize> { if long { lload_local(p) } else { iload_local(p) } };
    let mut pc = 0usize;
    // Prologue: max = a[0].  The element load width (laload/iaload) picks long vs int.
    let arr_local = aload_local(pc)?;
    pc += insn_len_at(bc, pc)?;
    if *bc.get(pc)? != 0x03 { return None; } // iconst_0
    pc += 1;
    let long = match *bc.get(pc)? { 0x2f => true, 0x2e => false, _ => return None }; // laload/iaload
    pc += 1;
    let max_st_ok = if long { matches!(bc[pc], 0x37 | 0x3f..=0x42) } else { matches!(bc[pc], 0x36 | 0x3b..=0x3e) };
    if !max_st_ok { return None; } // (l/i)store max
    let (max_local, l) = decode_store(bc, pc)?;
    pc += l;
    // best = 0.
    if *bc.get(pc)? != 0x03 { return None; } // iconst_0
    pc += 1;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) { return None; } // istore best
    let (best_local, l) = decode_store(bc, pc)?;
    pc += l;
    // i = 1.
    if *bc.get(pc)? != 0x04 { return None; } // iconst_1
    pc += 1;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) { return None; } // istore i
    let (iv_local, l) = decode_store(bc, pc)?;
    if iv_local == best_local || iv_local == max_local {
        return None;
    }
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
    if *bc.get(pc)? != 0xa2 { return None; } // if_icmpge
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;
    // Predicate: aload arr; iload i; (l/i)aload; (l/i)load max; then long → lcmp; ifle/ifge,
    // int → if_icmple/if_icmpge directly. ifle/if_icmple → argmax, ifge/if_icmpge → argmin.
    if aload_local(pc)? != arr_local { return None; }
    pc += insn_len_at(bc, pc)?;
    if iload_local(pc)? != iv_local { return None; }
    pc += insn_len_at(bc, pc)?;
    let ld_ok = if long { *bc.get(pc)? == 0x2f } else { *bc.get(pc)? == 0x2e };
    if !ld_ok { return None; }
    pc += 1;
    if maxload(long, pc)? != max_local { return None; }
    pc += insn_len_at(bc, pc)?;
    let want_max = if long {
        if *bc.get(pc)? != 0x94 { return None; } // lcmp
        pc += 1;
        match *bc.get(pc)? { 0x9e => true, 0x9c => false, _ => return None }
    } else {
        match *bc.get(pc)? { 0xa4 => true, 0xa2 => false, _ => return None }
    };
    let skip_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;
    // Update: max = a[i].
    if aload_local(pc)? != arr_local { return None; }
    pc += insn_len_at(bc, pc)?;
    if iload_local(pc)? != iv_local { return None; }
    pc += insn_len_at(bc, pc)?;
    let ld2_ok = if long { *bc.get(pc)? == 0x2f } else { *bc.get(pc)? == 0x2e };
    if !ld2_ok { return None; }
    pc += 1;
    let max_st2 = if long { matches!(bc[pc], 0x37 | 0x3f..=0x42) } else { matches!(bc[pc], 0x36 | 0x3b..=0x3e) };
    if !max_st2 { return None; } // (l/i)store max
    let (m2, l) = decode_store(bc, pc)?;
    if m2 != max_local { return None; }
    pc += l;
    // best = i.
    if iload_local(pc)? != iv_local { return None; }
    pc += insn_len_at(bc, pc)?;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) { return None; } // istore best
    let (b2, l) = decode_store(bc, pc)?;
    if b2 != best_local { return None; }
    pc += l;
    if pc != skip_pc { return None; } // the ifle skips exactly past the update
    // iinc i,1 ; goto header ; exit = iload best; ireturn.
    if *bc.get(pc)? != 0x84 || *bc.get(pc + 1)? as usize != iv_local || *bc.get(pc + 2)? as i8 as i32 != 1 {
        return None;
    }
    pc += 3;
    if *bc.get(pc)? != 0xa7 || (pc as i32 + s16(bc, pc + 1)) as usize != header { return None; }
    pc += 3;
    if pc != exit_pc { return None; }
    if iload_local(pc)? != best_local { return None; }
    pc += insn_len_at(bc, pc)?;
    if *bc.get(pc)? != 0xac || pc + 1 != bc.len() { return None; } // ireturn
    if arr_local == iv_local {
        return None;
    }
    if limit_local.is_none() && (lim_const as i64) < MIN_PARALLEL_TRIP {
        return None;
    }
    Some(ArgmaxLoop {
        arr_local,
        iv_local,
        max_local,
        best_local,
        limit_local,
        lim_const,
        elem_bytes: if long { 8 } else { 4 },
        want_max,
    })
}
