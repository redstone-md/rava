//! Reduction recognition: the counter-dependent fold and the two-accumulator variant.
//!
//! Re-exported from the parent module, so no call site outside it changed.


use crate::jit::*;

/// DATA-PARALLEL LOOP analyzer (next-gen-gc, Kit 3). Match a whole method against the
/// counted associative-accumulator template and extract its `DataParallelLoop`, or
/// `None`. Accepts `int`/`long` accumulators, `+`/`*`/`^` folds, constant step, and a
/// body of pure counter-dependent integer arithmetic with NO shared writes; rejects
/// a compile-time trip count below `MIN_PARALLEL_TRIP`. Deliberately rigid — a match
/// guarantees the fork's reduction equals the serial fold.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_data_parallel_loop(bc: &[u8], ret: Option<JT>) -> Option<DataParallelLoop> {
    let mut pc = 0usize;
    // Prologue: acc init (const → store acc) + iv init (iconst → istore iv). A `ldc2_w` seed
    // (e.g. `long m = Long.MIN_VALUE` for a Math.max reduce) needs the pool → capture its CP
    // and let the dispatch resolve the value (like the invokestatic fold).
    let (acc_init, l1, init_long, acc_init_cp) = if bc[pc] == 0x14 {
        let cp = ((bc.get(pc + 1).copied()? as u16) << 8) | bc.get(pc + 2).copied()? as u16;
        (0i64, 3usize, true, Some(cp))
    } else {
        let (v, l, il) = decode_acc_init(bc, pc)?;
        (v, l, il, None)
    };
    pc += l1;
    let (acc_local, l2) = decode_store(bc, pc)?;
    let acc_is_long = matches!(bc[pc], 0x37 | 0x3f..=0x42);
    let acc_is_int = matches!(bc[pc], 0x36 | 0x3b..=0x3e);
    if acc_is_long != init_long || (!acc_is_long && !acc_is_int) {
        return None; // init width must match the accumulator store width
    }
    pc += l2;
    // Optional LENGTH-HOIST prologue: `int n = arr.length;` compiles to
    // `aload arr; arraylength; istore n` between the accumulator init and the counter
    // init. javac emits it for the idiomatic `for (i = 0; i < n; i++) … arr[i]` reduction.
    // Absorb any number of such groups (one per hoisted array length) so the rigid
    // template still matches. CRITICAL: the parallel codegen replaces the whole method,
    // so it never runs this prologue — the limit local would read as 0. We therefore
    // record `(limit_local -> array_local)` and, when the loop's limit IS a hoisted
    // length, the fork computes the trip from that array's runtime length instead.
    let mut hoisted: Vec<(usize, usize)> = Vec::new();
    loop {
        let op = *bc.get(pc)?;
        // Compute arr_local/alen ONLY for a real aload opcode — `op - 0x2a` underflows (u8) for any
        // op < 0x2a, and the eager form panicked here on the first non-aload byte (a plain iconst
        // limit). aload_0..3 = 0x2a..=0x2d, wide aload = 0x19.
        if matches!(op, 0x19 | 0x2a..=0x2d) {
            let arr_local = if op == 0x19 { *bc.get(pc + 1)? as usize } else { (op - 0x2a) as usize };
            let alen = if op == 0x19 { 2 } else { 1 };
            if *bc.get(pc + alen)? == 0xbe {
                let store_pc = pc + alen + 1;
                if let Some((ln, _)) = decode_store_kind(bc, store_pc).filter(|_| matches!(bc[store_pc], 0x36 | 0x3b..=0x3e)) {
                    hoisted.push((ln, arr_local));
                    pc = store_pc + insn_len_at(bc, store_pc)?;
                    continue;
                }
            }
        }
        break;
    }
    let (c0, l3) = decode_iconst(bc, pc)?;
    pc += l3;
    let (iv_local, l4) = decode_store(bc, pc)?;
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) || iv_local == acc_local {
        return None; // iv is a distinct int local
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
        return None; // if_icmpge (runs while i < LIM)
    }
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;

    // Body: [scratch stores]? ; load acc ; <fold value> ; (i2l)? ; <fold> ; store acc ;
    // iinc iv,step ; goto header. The `load acc` (the fold's lhs) may be PRECEDED by
    // javac scratch-temp assignments, so we don't require it first — it's located
    // within the body below and skipped by the body emitter.
    let body_start = pc;

    // Locate the loop increment `iinc iv, step`: scan instruction boundaries from
    // `body_start` (a branch/return mid-body would desync the stream and is rejected
    // when we validate the body-expr opcodes below).
    let mut starts: Vec<usize> = Vec::new();
    let mut iinc_pc = None;
    let mut q = body_start;
    while q < bc.len() {
        if bc[q] == 0x84 {
            // iinc <index>, <const>
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
    let step = bc.get(iinc_pc + 2).copied()? as i8 as i32; // iinc: index, signed const
    if step == 0 {
        return None;
    }

    // Suffix peel: the instructions right before `iinc` are `<fold> <store acc>`, with
    // an optional `i2l` before the fold (long acc folding an int body expr).
    // store_acc = last recorded start; fold = previous.
    let s_pc = *starts.last()?;
    if !(matches!(bc[s_pc], 0x37 | 0x3f..=0x42) || matches!(bc[s_pc], 0x36 | 0x3b..=0x3e)) {
        return None;
    }
    let (s_local, _) = decode_store(bc, s_pc)?;
    let s_is_long = matches!(bc[s_pc], 0x37 | 0x3f..=0x42);
    if s_local != acc_local || s_is_long != acc_is_long {
        return None; // must `store acc` of the right width
    }
    if starts.len() < 2 {
        return None;
    }
    let fold_pc = *starts.get(starts.len() - 2)?;
    // The fold is either a bytecode op (iadd/ladd/ixor/…) OR `invokestatic Math.max/min`
    // (stack effect JJ→J / II→I = pop-2-push-1, exactly a fold). The invokestatic form is
    // resolved by the dispatch (which has the constant pool); its width matches the acc.
    let (op, fold_static_cp) = if bc[fold_pc] == 0xb8 {
        let cp = ((bc.get(fold_pc + 1).copied()? as u16) << 8) | bc.get(fold_pc + 2).copied()? as u16;
        (crate::parallel_pool::ReduceOp::Add, Some(cp)) // placeholder; dispatch sets Max/Min
    } else {
        let o = crate::parallel_pool::ReduceOp::from_fold_opcode(bc[fold_pc])?;
        // Fold width must match the accumulator (iadd/imul/ixor int; ladd/lmul/lxor long).
        if matches!(bc[fold_pc], 0x61 | 0x69 | 0x83) != acc_is_long {
            return None;
        }
        (o, None)
    };
    // Optional `i2l` (long acc folding an int body expr) sits before the fold.
    let mut body_end = fold_pc;
    if acc_is_long {
        if let Some(&maybe_i2l) = starts.len().checked_sub(3).and_then(|i| starts.get(i)) {
            if bc[maybe_i2l] == 0x85 {
                body_end = maybe_i2l;
            }
        }
    }
    if body_end <= body_start {
        return None; // empty body expression
    }

    // Locate the single `load acc` (the fold's lhs) inside the body — scratch
    // assignments may precede it. The body emitter SKIPS this instruction (acc is
    // the loop-carried accumulator, supplied by the fold, not the body value).
    let mut acc_load_pc: Option<usize> = None;
    for &p in &starts {
        if p >= body_end {
            break;
        }
        let is_acc_load = match bc[p] {
            0x15 | 0x1a..=0x1d if !acc_is_long => {
                decode_load(bc, p).map(|(l, _, _)| l) == Some(acc_local)
            }
            0x16 | 0x1e..=0x21 if acc_is_long => {
                decode_load(bc, p).map(|(l, _, _)| l) == Some(acc_local)
            }
            _ => false,
        };
        if is_acc_load {
            if acc_load_pc.is_some() {
                return None; // accumulator loaded more than once → not our shape
            }
            acc_load_pc = Some(p);
        }
    }
    let acc_load_pc = acc_load_pc?; // the fold's `load acc` must be present

    // SAFETY GATE + CAPTURE PASS: every body instruction must be pure
    // counter-dependent integer arithmetic — no shared write (putfield/putstatic/
    // array-store), no call, no alloc, no branch. A local LOAD other than the counter
    // is an invariant external param → record it as a capture (the body can't store,
    // so any local it reads is loop-invariant). (`starts` past `body_end` are the
    // peeled fold/store/i2l, excluded.)
    // A store to a body-local marks it a SCRATCH slot (a javac temp like `int a=...`);
    // a load of a non-counter, non-scratch local is a captured invariant param. A
    // scratch store must NOT target the counter or the accumulator (that would carry
    // loop state the fork can't replicate).
    let mut scratch: Vec<(usize, bool)> = Vec::new();
    let mut reads: Vec<(usize, bool)> = Vec::new();
    for &p in &starts {
        if p >= body_end {
            break;
        }
        if p == acc_load_pc {
            continue; // the fold's `load acc` — not a scratch/capture
        }
        let op = bc[p];
        if !is_parallel_body_op(op) {
            return None;
        }
        if let Some((local, is_long)) = decode_store_kind(bc, p) {
            if local == iv_local || local == acc_local {
                return None; // a store into the counter/accumulator breaks independence
            }
            if !scratch.iter().any(|(l, _)| *l == local) {
                scratch.push((local, is_long));
            }
        } else {
            let load = match op {
                0x15 | 0x1a..=0x1d => Some((decode_load(bc, p)?.0, false)), // iload
                0x16 | 0x1e..=0x21 => Some((decode_load(bc, p)?.0, true)),  // lload
                _ => None,
            };
            if let Some((local, is_long)) = load {
                if local != iv_local && !reads.iter().any(|(l, _)| *l == local) {
                    reads.push((local, is_long));
                }
            }
        }
    }
    // Captures = locals READ but never WRITTEN in the body (true external invariants).
    let captures: Vec<(usize, bool)> = reads
        .into_iter()
        .filter(|(l, _)| !scratch.iter().any(|(s, _)| s == l))
        .collect();

    // ARRAY-CAPTURE PASS: detect `arr[i]` reads. A primitive-array element load is
    // SAFE in a detached worker ONLY as the exact triple `aload L; iload iv; (i/l)aload`
    // — the index must be the bare counter so the fork's `limit <= length` gate proves
    // every access in-bounds (no AIOOBE possible, which the worker couldn't throw). Any
    // other indexing shape isn't recognised here, so its `iaload` reaches the emitter
    // with a non-ArrRef stack top and bails to the serial path. `c0 >= 0` is required
    // below so the low end of the index range is in-bounds too.
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
    let mut array_captures: Vec<(usize, bool)> = Vec::new();
    for w in starts.windows(3) {
        let (p0, p1, p2) = (w[0], w[1], w[2]);
        if p2 >= body_end {
            break;
        }
        if matches!(bc[p2], 0x2e | 0x2f) {
            // The element load must consume `aload L` + `iload iv` directly.
            let (Some(arr), Some(ix)) = (aload_local(p0), iload_local(p1)) else {
                return None; // an iaload/laload not in the canonical triple → unsafe
            };
            if ix != iv_local || arr == iv_local || arr == acc_local {
                return None;
            }
            let is_long = bc[p2] == 0x2f;
            if !array_captures.iter().any(|(l, _)| *l == arr) {
                array_captures.push((arr, is_long));
            }
        }
    }
    if !array_captures.is_empty() && c0 < 0 {
        return None; // a negative start index could read arr[-1] → out of bounds
    }

    // goto header right after iinc.
    let goto_pc = iinc_pc + 3;
    if *bc.get(goto_pc)? != 0xa7 || (goto_pc as i32 + s16(bc, goto_pc + 1)) as usize != header {
        return None;
    }

    // Exit (== the if_icmpge target): load acc ; (i/l)return — nothing trailing.
    if goto_pc + 3 != exit_pc {
        return None;
    }
    let (e_acc, _, le) = decode_load(bc, exit_pc)?;
    if e_acc != acc_local {
        return None;
    }
    let ret_pc = exit_pc + le;
    let want_ret = if acc_is_long { 0xad } else { 0xac }; // lreturn / ireturn
    if *bc.get(ret_pc)? != want_ret || ret_pc + 1 != bc.len() {
        return None;
    }
    let _ = ret; // return type is implied by the accumulator width

    // Compile-time trip-count gate (a runtime limit is guarded by the forker instead).
    if limit_local.is_none() {
        let trip = (((lim_const - c0) as i64) + step as i64 - 1) / step as i64;
        if trip < MIN_PARALLEL_TRIP {
            return None;
        }
    }

    Some(DataParallelLoop {
        iv_local,
        acc_local,
        acc_is_long,
        acc_init,
        c0,
        step,
        limit_local,
        lim_const,
        limit_from_array: limit_local.and_then(|ll| hoisted.iter().find(|(ln, _)| *ln == ll).map(|(_, arr)| *arr)),
        op,
        body_range: (body_start, body_end),
        captures,
        scratch,
        array_captures,
        acc_load_pc,
        fold_static_cp,
        acc_init_cp,
    })
}
