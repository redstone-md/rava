//! Element-independent map and predicate compaction.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Recognise the whole-method MAP shape above (sibling of `analyze_data_parallel_loop`).
/// Returns `None` (→ serial) on any deviation. Reuses the same rigid prologue/header/iinc/
/// goto template; the differences are: NO accumulator init, the body ends in the store
/// triple `aload out; iload iv; <value>; (i/l)astore`, and the method exit is a bare `return`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_map(bc: &[u8]) -> Option<MapLoop> {
    let mut pc = 0usize;
    // Prologue: optional `int n = arr.length;` length-hoist groups, then `i = c0`.
    let mut hoisted: Vec<(usize, usize)> = Vec::new();
    loop {
        let is_aload = matches!(*bc.get(pc)?, 0x19 | 0x2a..=0x2d);
        if !is_aload {
            break;
        }
        let arr_local = if bc[pc] == 0x19 { *bc.get(pc + 1)? as usize } else { (bc[pc] - 0x2a) as usize };
        let alen = if bc[pc] == 0x19 { 2 } else { 1 };
        if *bc.get(pc + alen)? == 0xbe {
            let store_pc = pc + alen + 1;
            if let Some((ln, _)) = decode_store_kind(bc, store_pc).filter(|_| matches!(bc[store_pc], 0x36 | 0x3b..=0x3e)) {
                hoisted.push((ln, arr_local));
                pc = store_pc + insn_len_at(bc, store_pc)?;
                continue;
            }
        }
        break;
    }
    let (c0, l3) = decode_iconst(bc, pc)?;
    pc += l3;
    let (iv_local, l4) = decode_store(bc, pc)?;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) {
        return None;
    }
    pc += l4;

    // Header: iload iv ; (iload LIM | iconst LIM) ; if_icmpge exit.
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

    // Body: aload out ; iload iv ; <value> ; (i/l)astore ; iinc iv,step ; goto header.
    let body_start = pc;
    let mut starts: Vec<usize> = Vec::new();
    let mut iinc_pc = None;
    let mut q = body_start;
    while q < bc.len() {
        if bc[q] == 0x84 {
            if bc.get(q + 1).copied()? as usize != iv_local {
                return None;
            }
            iinc_pc = Some(q);
            break;
        }
        starts.push(q);
        q += insn_len_at(bc, q)?;
    }
    let iinc_pc = iinc_pc?;
    let step = bc.get(iinc_pc + 2).copied()? as i8 as i32;
    if step == 0 {
        return None;
    }

    // Suffix: the instruction before `iinc` is the terminal `(i/l)astore`.
    let astore_pc = *starts.last()?;
    let out_is_long = match bc[astore_pc] {
        0x4f => false,        // iastore (4-byte int elements)
        0x50 | 0x52 => true,  // lastore / dastore (8-byte long/double elements)
        _ => return None,
    };
    if starts.len() < 3 {
        return None; // need at least: aload out, iload iv, astore
    }
    let aload_local = |p: usize| -> Option<usize> {
        match bc[p] {
            0x19 => Some(bc[p + 1] as usize),
            op @ 0x2a..=0x2d => Some((op - 0x2a) as usize),
            _ => None,
        }
    };
    let iload_local = |p: usize| -> Option<usize> {
        match bc[p] {
            0x15 => Some(bc[p + 1] as usize),
            op @ 0x1a..=0x1d => Some((op - 0x1a) as usize),
            _ => None,
        }
    };
    let body_end = astore_pc;
    // Locate the store statement `aload out; iload iv; <value>` by a stack-depth walk (item
    // model): `out[i] = value` begins at the LAST depth-0 boundary before the astore — the
    // scratch statements before it (e.g. `long x = in[i];`) are stack-neutral. This matches
    // both the inlined shape (target opens the body) and an explicit temp (target mid-body).
    let mut depth = 0i32;
    let mut stmt_start = body_start;
    for &p in &starts {
        if p == astore_pc {
            break;
        }
        if depth == 0 {
            stmt_start = p;
        }
        depth += map_stack_delta(bc, p)?;
    }
    let ti = starts.iter().position(|&p| p == stmt_start)?;
    let out_pc = starts[ti];
    let idx_pc = *starts.get(ti + 1)?;
    let out_local = aload_local(out_pc)?;
    if iload_local(idx_pc)? != iv_local || out_local == iv_local {
        return None; // the store target must be `aload out ; iload iv`
    }
    let store_skip = vec![out_pc, idx_pc];

    // SAFETY GATE + CAPTURE/SCRATCH/ARRAY passes over the body EXCEPT the store target
    // (mirror the reduce analyzer). No shared write (other than the sanctioned terminal
    // out[i] store), no call/branch/alloc; a store targets a scratch temp; a non-counter,
    // non-output load is a capture.
    let val_starts: Vec<usize> = starts
        .iter()
        .copied()
        .filter(|&p| p < body_end && p != out_pc && p != idx_pc)
        .collect();
    if val_starts.is_empty() {
        return None;
    }
    let mut scratch: Vec<(usize, bool)> = Vec::new();
    let mut reads: Vec<(usize, bool)> = Vec::new();
    let mut static_calls: Vec<(usize, u16)> = Vec::new();
    let mut const_calls: Vec<(usize, u16)> = Vec::new();
    // A local READ before it is WRITTEN in the body carries state across iterations (an
    // accumulator / scan). If such a local is also a scratch (stored later), the map is
    // NOT element-independent → reject (it belongs to the reduce/scan analyzers).
    let mut seen_write: Vec<usize> = Vec::new();
    let mut loop_carried: Vec<usize> = Vec::new();
    for &p in &val_starts {
        let op = bc[p];
        if !is_parallel_body_op(op) {
            return None;
        }
        if op == 0xb8 {
            // invokestatic in the value → the dispatch checks it's Math.max/min.
            let cp = ((bc.get(p + 1).copied()? as u16) << 8) | bc.get(p + 2).copied()? as u16;
            static_calls.push((p, cp));
        }
        if op == 0x14 {
            // ldc2_w long/double literal → the dispatch resolves the value.
            let cp = ((bc.get(p + 1).copied()? as u16) << 8) | bc.get(p + 2).copied()? as u16;
            const_calls.push((p, cp));
        }
        if let Some((local, is_long)) = decode_store_kind(bc, p) {
            if local == iv_local || local == out_local {
                return None;
            }
            if !seen_write.contains(&local) {
                seen_write.push(local);
            }
            if !scratch.iter().any(|(l, _)| *l == local) {
                scratch.push((local, is_long));
            }
        } else {
            let load = match op {
                0x15 | 0x1a..=0x1d => Some((decode_load(bc, p)?.0, false)),
                0x16 | 0x1e..=0x21 => Some((decode_load(bc, p)?.0, true)),
                0x18 => Some((*bc.get(p + 1)? as usize, true)), // dload <idx> (captured double)
                0x26..=0x29 => Some(((op - 0x26) as usize, true)), // dload_0..3
                _ => None,
            };
            if let Some((local, is_long)) = load {
                if local != iv_local
                    && local != out_local
                    && !seen_write.contains(&local)
                    && !loop_carried.contains(&local)
                {
                    loop_carried.push(local); // read before any write → carries state
                }
                if local != iv_local && local != out_local && !reads.iter().any(|(l, _)| *l == local) {
                    reads.push((local, is_long));
                }
            }
        }
    }
    // A scratch local read before it is written is a loop-carried accumulator (scan /
    // reduction) — the map's per-iteration re-init would compute the wrong value.
    if scratch.iter().any(|(l, _)| loop_carried.contains(l)) {
        return None;
    }
    let captures: Vec<(usize, bool)> = reads
        .into_iter()
        .filter(|(l, _)| !scratch.iter().any(|(s, _)| s == l))
        .collect();

    // ARRAY-CAPTURE PASS: input `in[i+off]` reads. A primitive-array element load is
    // recognised ONLY as `aload L; iload iv; (i/l/d)aload` (offset 0) or the STENCIL shape
    // `aload L; iload iv; <const k>; iadd|isub; (i/l/d)aload` (offset ±k). Any other index
    // shape → an (i/l/d)aload not matched here → serial. `read_off_lo/hi` accumulate the
    // offset range for the fork's bounds gate.
    let mut array_captures: Vec<(usize, bool)> = Vec::new();
    let mut read_off_lo = 0i32;
    let mut read_off_hi = 0i32;
    for (j, &p) in val_starts.iter().enumerate() {
        if !matches!(bc[p], 0x2e | 0x2f | 0x31) {
            continue;
        }
        let is_long = matches!(bc[p], 0x2f | 0x31); // laload / daload = 8-byte element
        let (arr, off) = if j >= 2
            && iload_local(val_starts[j - 1]) == Some(iv_local)
            && aload_local(val_starts[j - 2]).is_some()
        {
            (aload_local(val_starts[j - 2])?, 0i32)
        } else if j >= 4
            && matches!(bc[val_starts[j - 1]], 0x60 | 0x64) // iadd / isub
            && iload_local(val_starts[j - 3]) == Some(iv_local)
            && aload_local(val_starts[j - 4]).is_some()
        {
            let (k, _) = decode_iconst(bc, val_starts[j - 2])?;
            let off = if bc[val_starts[j - 1]] == 0x60 { k } else { -k };
            (aload_local(val_starts[j - 4])?, off)
        } else {
            return None; // an element load not in the counter±const shape → serial
        };
        if arr == iv_local {
            return None;
        }
        read_off_lo = read_off_lo.min(off);
        read_off_hi = read_off_hi.max(off);
        if !array_captures.iter().any(|(l, _)| *l == arr) {
            array_captures.push((arr, is_long));
        }
    }
    // The low end of the input range: `c0 + read_off_lo` must be >= 0 (a compile-time c0 is
    // checked here; the fork gate re-checks with the runtime limit for the high end).
    if c0 as i32 + read_off_lo < 0 {
        return None;
    }

    // goto header right after iinc; exit == the if_icmpge target == a bare `return`.
    let goto_pc = iinc_pc + 3;
    if *bc.get(goto_pc)? != 0xa7 || (goto_pc as i32 + s16(bc, goto_pc + 1)) as usize != header {
        return None;
    }
    if goto_pc + 3 != exit_pc || *bc.get(exit_pc)? != 0xb1 || exit_pc + 1 != bc.len() {
        return None; // exit is `return` (void), nothing trailing
    }

    if limit_local.is_none() {
        let trip = (((lim_const - c0) as i64) + step as i64 - 1) / step as i64;
        if trip < MIN_PARALLEL_TRIP {
            return None;
        }
    }

    Some(MapLoop {
        iv_local,
        c0,
        step,
        limit_local,
        lim_const,
        limit_from_array: limit_local.and_then(|ll| hoisted.iter().find(|(ln, _)| *ln == ll).map(|(_, arr)| *arr)),
        out_local,
        out_is_long,
        body_range: (body_start, body_end),
        store_skip,
        captures,
        scratch,
        array_captures,
        static_calls,
        math_ops: Vec::new(),
        read_off_lo,
        read_off_hi,
        const_calls,
        const_ops: Vec::new(),
    })
}

/// Recognise the rigid long-array filter template above. `None` on any deviation.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_filter(bc: &[u8]) -> Option<FilterLoop> {
    let mut pc = 0usize;
    // Prologue: k = 0; i = c0.  (both int)
    if !matches!(bc[pc], 0x03) { return None; } // iconst_0 (k=0)
    pc += 1;
    if !matches!(bc[pc], 0x3b..=0x3e | 0x36) { return None; } // istore k
    let (k_local, kl) = decode_store(bc, pc)?;
    pc += kl;
    let (c0, cl) = decode_iconst(bc, pc)?;
    pc += cl;
    if !matches!(bc[pc], 0x3b..=0x3e | 0x36) { return None; } // istore i
    let (iv_local, il) = decode_store(bc, pc)?;
    if iv_local == k_local { return None; }
    pc += il;
    // Header: iload iv; (iload LIM | iconst LIM); if_icmpge exit.
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
    // Predicate: aload in; iload iv; (laload C ldc2_w lcmp ifXX) | (iaload <push C> if_icmpXX).
    let mut q = pc;
    let in_local = aload_local(q)?;
    q += insn_len_at(bc, q)?;
    if iload_local(q)? != iv_local { return None; }
    q += insn_len_at(bc, q)?;
    let long = match *bc.get(q)? {
        0x2f => true,  // laload → long[]
        0x2e => false, // iaload → int[]
        _ => return None,
    };
    q += 1;
    // Extract the predicate constant + the KEEP comparison (branch-NOT-taken condition).
    let (pred_cp, pred_const, pred_keep, skip_pc) = if long {
        if *bc.get(q)? != 0x14 { return None; } // ldc2_w C
        let cp = ((*bc.get(q + 1)? as u16) << 8) | *bc.get(q + 2)? as u16;
        q += 3;
        if *bc.get(q)? != 0x94 { return None; } // lcmp
        q += 1;
        let keep = match *bc.get(q)? {
            0x9e => IntCC::SignedGreaterThan,        // ifle skip → keep >
            0x9b => IntCC::SignedGreaterThanOrEqual, // iflt → keep >=
            0x9c => IntCC::SignedLessThan,           // ifge → keep <
            0x9d => IntCC::SignedLessThanOrEqual,    // ifgt → keep <=
            0x99 => IntCC::NotEqual,                 // ifeq → keep !=
            0x9a => IntCC::Equal,                    // ifne → keep ==
            _ => return None,
        };
        let sp = (q as i32 + s16(bc, q + 1)) as usize;
        q += 3;
        (cp, 0i64, keep, sp)
    } else {
        let (c, cl) = decode_iconst(bc, q)?; // iconst / bipush / sipush C
        q += cl;
        let keep = match *bc.get(q)? {
            0xa4 => IntCC::SignedGreaterThan,        // if_icmple skip → keep >
            0xa1 => IntCC::SignedGreaterThanOrEqual, // if_icmplt → keep >=
            0xa2 => IntCC::SignedLessThan,           // if_icmpge → keep <
            0xa3 => IntCC::SignedLessThanOrEqual,    // if_icmpgt → keep <=
            0x9f => IntCC::NotEqual,                 // if_icmpeq → keep !=
            0xa0 => IntCC::Equal,                    // if_icmpne → keep ==
            _ => return None,
        };
        let sp = (q as i32 + s16(bc, q + 1)) as usize;
        q += 3;
        (0u16, c as i64, keep, sp)
    };
    // Kept body: aload out; iload k; aload in; iload iv; (l/i)aload; (l/i)astore; iinc k,1.
    let out_local = aload_local(q)?;
    q += insn_len_at(bc, q)?;
    if iload_local(q)? != k_local { return None; }
    q += insn_len_at(bc, q)?;
    if aload_local(q)? != in_local { return None; }
    q += insn_len_at(bc, q)?;
    if iload_local(q)? != iv_local { return None; }
    q += insn_len_at(bc, q)?;
    let ld_ok = if long { *bc.get(q)? == 0x2f } else { *bc.get(q)? == 0x2e };
    if !ld_ok { return None; } // laload / iaload
    q += 1;
    let st_ok = if long { *bc.get(q)? == 0x50 } else { *bc.get(q)? == 0x4f };
    if !st_ok { return None; } // lastore / iastore
    q += 1;
    if *bc.get(q)? != 0x84 || *bc.get(q + 1)? as usize != k_local || *bc.get(q + 2)? as i8 as i32 != 1 {
        return None; // iinc k,1
    }
    q += 3;
    if q != skip_pc { return None; } // the ifXX skips exactly past the kept body
    // iinc iv,1 ; goto header ; exit = iload k; ireturn.
    if *bc.get(q)? != 0x84 || *bc.get(q + 1)? as usize != iv_local || *bc.get(q + 2)? as i8 as i32 != 1 {
        return None;
    }
    q += 3;
    if *bc.get(q)? != 0xa7 || (q as i32 + s16(bc, q + 1)) as usize != header { return None; }
    q += 3;
    if q != exit_pc { return None; }
    if iload_local(q)? != k_local { return None; }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? != 0xac || q + 1 != bc.len() { return None; } // ireturn
    if in_local == out_local || in_local == iv_local || out_local == iv_local || out_local == k_local || c0 < 0 {
        return None;
    }
    if limit_local.is_none() && ((lim_const - c0) as i64) < MIN_PARALLEL_TRIP {
        return None;
    }
    Some(FilterLoop {
        iv_local,
        k_local,
        c0,
        limit_local,
        lim_const,
        in_local,
        out_local,
        pred_keep,
        pred_cp,
        pred_const,
        elem_bytes: if long { 8 } else { 4 },
    })
}
