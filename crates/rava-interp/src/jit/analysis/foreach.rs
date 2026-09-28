//! Iterator-shaped loops: the offloadable foreach, its split form, and the coordinate map.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Recognise the object-building for-each map skeleton above. Pure over the bytecode (no pool,
/// no VM) so it is unit-testable; `None` on any deviation. DORMANT — the fork codegen (F3) is
/// the next increment; until then nothing calls this on a hot path, so the build is byte-exact.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn analyze_offloadable_foreach(bc: &[u8]) -> Option<ForkEachLoop> {
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
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) {
        return None; // istore i
    }
    let (iv_local, l4) = decode_store(bc, pc)?;
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
        return None; // if_icmpge (runs while i < LIM)
    }
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;

    // Body: aload OUT ; iload iv ; aload SRC ; iload iv ; aaload ; [checkcast]? ; invoke f ;
    //       [checkcast]? ; aastore ; iinc iv,step ; goto header. Decode the exact sequence.
    let body_start = pc;
    let aload_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x19 => Some(*bc.get(p + 1)? as usize),
            op @ 0x2a..=0x2d => Some((op - 0x2a) as usize),
            _ => None,
        }
    };
    let iload_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x15 => Some(*bc.get(p + 1)? as usize),
            op @ 0x1a..=0x1d => Some((op - 0x1a) as usize),
            _ => None,
        }
    };
    let mut q = body_start;
    let out_local = aload_at(q)?;
    q += insn_len_at(bc, q)?;
    if iload_at(q)? != iv_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    let src_local = aload_at(q)?;
    q += insn_len_at(bc, q)?;
    if iload_at(q)? != iv_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? != 0x32 {
        return None; // aaload (element = src[iv])
    }
    q += 1;
    if *bc.get(q)? == 0xc0 {
        q += insn_len_at(bc, q)?; // optional checkcast of the element
    }
    let call_pc = q;
    let call_op = *bc.get(q)?;
    if !matches!(call_op, 0xb6 | 0xb8 | 0xb9) {
        return None; // per-element invoke (virtual / static / interface)
    }
    let call_cp = ((*bc.get(q + 1)? as u16) << 8) | *bc.get(q + 2)? as u16;
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? == 0xc0 {
        q += insn_len_at(bc, q)?; // optional checkcast of the result
    }
    if *bc.get(q)? != 0x53 {
        return None; // aastore (out[iv] = result)
    }
    let body_end = q;
    q += 1;
    if *bc.get(q)? != 0x84 || *bc.get(q + 1)? as usize != iv_local {
        return None; // iinc iv, step
    }
    let step = *bc.get(q + 2)? as i8 as i32;
    if step == 0 {
        return None;
    }
    let iinc_pc = q;
    let goto_pc = iinc_pc + 3;
    if *bc.get(goto_pc)? != 0xa7 || (goto_pc as i32 + s16(bc, goto_pc + 1)) as usize != header {
        return None;
    }
    if goto_pc + 3 != exit_pc || *bc.get(exit_pc)? != 0xb1 || exit_pc + 1 != bc.len() {
        return None; // exit is a bare `return` (void), nothing trailing
    }

    // out / src / counter must be distinct locals: an in-place `out == src` write races the
    // read on other schemes, and a counter alias breaks the disjoint-index guarantee.
    if out_local == iv_local || src_local == iv_local || out_local == src_local {
        return None;
    }

    // Compile-time trip gate (a runtime limit is guarded by the forker instead).
    if limit_local.is_none() {
        let trip = (((lim_const - c0) as i64) + step as i64 - 1) / step as i64;
        if trip < MIN_PARALLEL_TRIP {
            return None;
        }
    }

    Some(ForkEachLoop {
        iv_local,
        c0,
        step,
        limit_local,
        lim_const,
        limit_from_array: limit_local.and_then(|ll| hoisted.iter().find(|(ln, _)| *ln == ll).map(|(_, arr)| *arr)),
        src_local,
        out_local,
        call_cp,
        call_op,
        call_pc,
        body_range: (body_start, body_end),
    })
}

/// Recognise the iterator for-each skeleton in `bc` beginning at `iter_call_pc` (the pc of the
/// `iterator()` invoke). Structural only — decodes the exact instruction chain. `None` if it is
/// not the canonical single-statement `e.m()` loop.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn match_iterator_foreach(bc: &[u8], iter_call_pc: usize) -> Option<IterForEach> {
    let aload_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x19 => Some(*bc.get(p + 1)? as usize),
            op @ 0x2a..=0x2d => Some((op - 0x2a) as usize),
            _ => None,
        }
    };
    let astore_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x3a => Some(*bc.get(p + 1)? as usize),
            op @ 0x4b..=0x4e => Some((op - 0x4b) as usize),
            _ => None,
        }
    };
    // iterator() invoke (virtual/interface), then `astore it`.
    if !matches!(*bc.get(iter_call_pc)?, 0xb6 | 0xb9) {
        return None;
    }
    let mut q = iter_call_pc + insn_len_at(bc, iter_call_pc)?;
    let it_local = astore_at(q)?;
    q += insn_len_at(bc, q)?;
    // Loop head: aload it ; invoke hasNext ; ifeq END.
    let head = q;
    if aload_at(q)? != it_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    if !matches!(*bc.get(q)?, 0xb6 | 0xb9) {
        return None; // hasNext()
    }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? != 0x99 {
        return None; // ifeq END (exits when hasNext == false)
    }
    let loop_end_pc = (q as i32 + s16(bc, q + 1)) as usize;
    q += 3;
    // Body: aload it ; invoke next ; [checkcast]? ; astore e ; aload e ; invoke m ; [pop]? ; goto head.
    if aload_at(q)? != it_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    if !matches!(*bc.get(q)?, 0xb6 | 0xb9) {
        return None; // next()
    }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? == 0xc0 {
        q += insn_len_at(bc, q)?; // checkcast E
    }
    let e_local = astore_at(q)?;
    q += insn_len_at(bc, q)?;
    if aload_at(q)? != e_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    let elem_op = *bc.get(q)?;
    if !matches!(elem_op, 0xb6 | 0xb9) {
        return None; // e.m()
    }
    let elem_cp = ((*bc.get(q + 1)? as u16) << 8) | *bc.get(q + 2)? as u16;
    q += insn_len_at(bc, q)?;
    // Optional pop of a non-void result (the body is a single statement `e.m();`).
    let elem_result_popped = matches!(*bc.get(q)?, 0x57 | 0x58);
    if elem_result_popped {
        q += 1;
    }
    // goto back to the loop head, and the head must be the ifeq exit's fallthrough origin.
    if *bc.get(q)? != 0xa7 || (q as i32 + s16(bc, q + 1)) as usize != head {
        return None;
    }
    if it_local == e_local {
        return None;
    }
    Some(IterForEach { it_local, e_local, elem_op, elem_cp, loop_end_pc, elem_result_popped })
}

/// Recognise the split iterator for-each skeleton (prefix + `e.m()`). Pure/structural. `None` if
/// the loop is not `while(it.hasNext()){ e=(E)it.next(); <branch-free prefix>; e.m(); }`.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn match_iterator_foreach_split(bc: &[u8], iter_call_pc: usize) -> Option<IterForEachSplit> {
    let aload_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x19 => Some(*bc.get(p + 1)? as usize),
            op @ 0x2a..=0x2d => Some((op - 0x2a) as usize),
            _ => None,
        }
    };
    let astore_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x3a => Some(*bc.get(p + 1)? as usize),
            op @ 0x4b..=0x4e => Some((op - 0x4b) as usize),
            _ => None,
        }
    };
    // Header is identical to the strict matcher, up to `astore e`.
    if !matches!(*bc.get(iter_call_pc)?, 0xb6 | 0xb9) {
        return None;
    }
    let mut q = iter_call_pc + insn_len_at(bc, iter_call_pc)?;
    let it_local = astore_at(q)?;
    q += insn_len_at(bc, q)?;
    let head = q;
    if aload_at(q)? != it_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    if !matches!(*bc.get(q)?, 0xb6 | 0xb9) {
        return None; // hasNext()
    }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? != 0x99 {
        return None;
    }
    let loop_end_pc = (q as i32 + s16(bc, q + 1)) as usize;
    q += 3;
    if aload_at(q)? != it_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    if !matches!(*bc.get(q)?, 0xb6 | 0xb9) {
        return None; // next()
    }
    q += insn_len_at(bc, q)?;
    if *bc.get(q)? == 0xc0 {
        q += insn_len_at(bc, q)?; // checkcast E
    }
    let e_local = astore_at(q)?;
    if it_local == e_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    // Collect the body instruction boundaries from here up to the trailing `goto head`.
    let prefix_start = q;
    let mut starts: Vec<usize> = Vec::new();
    while *bc.get(q)? != 0xa7 {
        // Prefix statements must be straight-line (no branches/switches/returns) so they can be
        // replayed per element in order; and they must not overwrite the it/e locals.
        let op = *bc.get(q)?;
        if matches!(op, 0x99..=0xa8 | 0xaa | 0xab | 0xac..=0xb1 | 0xbf | 0xc6..=0xc9) {
            return None;
        }
        if let Some((l, _)) = decode_store_kind(bc, q) {
            if l == it_local || l == e_local {
                return None;
            }
        }
        if matches!(op, 0x3a | 0x4b..=0x4e) && matches!(astore_at(q), Some(l) if l == it_local || l == e_local) {
            return None; // astore into it/e (ref store)
        }
        starts.push(q);
        q += insn_len_at(bc, q)?;
    }
    // Trailing `goto head`.
    let goto_pc = q;
    if (goto_pc as i32 + s16(bc, goto_pc + 1)) as usize != head {
        return None;
    }
    // Peel the final statement `aload e ; invoke m ; [pop]?` off the end of `starts`.
    let mut idx = starts.len();
    let elem_result_popped = idx > 0 && matches!(*bc.get(starts[idx - 1])?, 0x57 | 0x58);
    if elem_result_popped {
        idx -= 1;
    }
    if idx < 2 {
        return None; // need at least `aload e ; invoke m`
    }
    let elem_pc = starts[idx - 1];
    let elem_op = *bc.get(elem_pc)?;
    if !matches!(elem_op, 0xb6 | 0xb9) {
        return None;
    }
    let elem_cp = ((*bc.get(elem_pc + 1)? as u16) << 8) | *bc.get(elem_pc + 2)? as u16;
    let aload_e_pc = starts[idx - 2];
    if aload_at(aload_e_pc) != Some(e_local) {
        return None; // the final call's receiver must be `e`
    }
    let prefix_end = aload_e_pc;
    Some(IterForEachSplit {
        it_local,
        e_local,
        prefix_range: (prefix_start, prefix_end),
        elem_op,
        elem_cp,
        loop_end_pc,
        elem_result_popped,
    })
}

/// Recognise the 3-nest coordinate map at outermost back-edge `goto_pc`. Structural only — returns the
/// cp indices for the caller to confirm `CTX`/`compute()D` names via the constant pool. Rejects any body
/// with a shared write (putstatic/putfield/array-store) — proving iteration independence structurally.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn match_coord_map_loop(bc: &[u8], goto_pc: usize) -> Option<CoordMapLoop> {
    let mx = match_numeric_range_loop_toptest(bc, goto_pc)?; // X (outermost)
    let mz = inner_range_loop(bc, mx.body_start_pc, mx.body_end_pc)?; // Z
    let my = inner_range_loop(bc, mz.body_start_pc, mz.body_end_pc)?; // Y (innermost)
    // Innermost body must be the density-fill payload + reduction, and shared-write-free.
    let (b0, b1) = (my.body_start_pc, my.body_end_pc);
    let (mut ctx_cp, mut compute_cp) = (None, None);
    let mut ctx_ivloads: Vec<usize> = Vec::new();
    let mut acc_local: Option<usize> = None;
    let mut recv_local: Option<usize> = None; // first aload = invariant receiver (density)
    let mut reduce_mul_cp: Option<u16> = None; // ldc2_w before lmul
    let mut compute_end_pc: Option<usize> = None;
    let mut saw_d2l = false; // doubleToRawLongBits (invokestatic) in the reduction
    let mut p = b0;
    while p < b1 {
        let op = *bc.get(p)?;
        match op {
            0xb3 | 0xb5 | 0x4f..=0x56 => return None, // putstatic / putfield / *astore → shared mutation
            0x19 if recv_local.is_none() => recv_local = Some(*bc.get(p + 1)? as usize), // aload recv
            0x2a..=0x2d if recv_local.is_none() => recv_local = Some((op - 0x2a) as usize),
            0xbb => ctx_cp = Some(((*bc.get(p + 1)? as u16) << 8) | *bc.get(p + 2)? as u16), // new CTX
            0x15 => ctx_ivloads.push(*bc.get(p + 1)? as usize),
            0x1a..=0x1d => ctx_ivloads.push((op - 0x1a) as usize),
            0xb9 => {
                compute_cp = Some(((*bc.get(p + 1)? as u16) << 8) | *bc.get(p + 2)? as u16);
                compute_end_pc = Some(p + insn_len_at(bc, p)?); // reduction starts right after
            }
            0x14 => reduce_mul_cp = Some(((*bc.get(p + 1)? as u16) << 8) | *bc.get(p + 2)? as u16), // ldc2_w C
            0xb8 => saw_d2l = true, // invokestatic (doubleToRawLongBits — name-confirmed by the caller)
            0x37 | 0x3f..=0x42 => acc_local = Some(decode_store(bc, p)?.0), // lstore acc (reduction)
            _ => {}
        }
        p += insn_len_at(bc, p)?;
    }
    let ctx_cp = ctx_cp?;
    let compute_cp = compute_cp?;
    let acc_local = acc_local?;
    let recv_local = recv_local?;
    let reduce_mul_cp = reduce_mul_cp?;
    let compute_end_pc = compute_end_pc?;
    if !saw_d2l {
        return None; // reduction must run the double→long widening
    }
    // The context must be built from exactly the 3 nest counters (order-independent set membership).
    let ivs = [mx.iv_local, mz.iv_local, my.iv_local];
    if !ivs.iter().all(|iv| ctx_ivloads.contains(iv)) {
        return None;
    }
    // Y-loop init source (yMin): the `<load L>; istore y_iv` in the Z-body before the Y-cond.
    let ymin_local = {
        let mut ym = None;
        let mut q = mz.body_start_pc;
        while q < my.cond_pc {
            let src = match *bc.get(q)? {
                0x15 => Some(*bc.get(q + 1)? as usize),
                op @ 0x1a..=0x1d => Some((op - 0x1a) as usize),
                _ => None,
            };
            if let Some(src) = src {
                let nx = q + insn_len_at(bc, q)?;
                if matches!(*bc.get(nx)?, 0x36 | 0x3b..=0x3e) && decode_store(bc, nx)?.0 == my.iv_local {
                    ym = Some(src);
                }
            }
            q += insn_len_at(bc, q)?;
        }
        ym?
    };
    Some(CoordMapLoop {
        x_iv: mx.iv_local, z_iv: mz.iv_local, y_iv: my.iv_local,
        x_bound: mx.bound, z_bound: mz.bound, y_bound: my.bound,
        ctx_cp, compute_cp, acc_local,
        body_start_pc: b0, body_end_pc: b1, loop_exit_pc: mx.loop_exit_pc,
        recv_local, ymin_local, reduce_mul_cp, compute_end_pc,
    })
}
