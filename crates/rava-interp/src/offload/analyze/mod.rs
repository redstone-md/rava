//! Whether a call tree may leave the GIL, and why an arithmetic shape is refused.
//!
//! Re-exported from the parent module, so no call site outside it changed.

mod arith;
use crate::offload::*;
pub(crate) use arith::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    relaxed: bool,
    txn: bool,
    is_init: bool,
    receiver_private: bool,
    self_class: Option<&str>,
    iv_local: Option<usize>,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> Result<(), OffReject> {
    // `this` (local 0) is a writable-private slot when this is a constructor (the object is
    // under construction) OR the fork context guarantees a distinct per-iteration receiver.
    let this_private = is_init || receiver_private;
    // A method with exception handlers can resume mid-side-effect on unwinding —
    // keep it off the parallel path for now (matches the JIT's own caution).
    if !code.exceptions.is_empty() {
        return Err(OffReject::Handler);
    }
    // FieldTxn (write-only transaction) mode: a constructor body is never a
    // txn bytecode unit — the runtime splices constructors, it does not
    // journal them, so the proof refuses the shape outright.
    if txn && is_init {
        return Err(OffReject::Ctor);
    }
    let bc = &code.bytecode;
    let Some(targets) = branch_targets(bc) else {
        return Err(OffReject::Malformed); // unanalysable control flow → reject
    };

    // DURABLE-FRESH refinement (loop support): a local assigned by EXACTLY ONE `astore` and never
    // by any other store / iinc is single-assignment — once it holds a fresh value that freshness
    // is STABLE (no reassignment can make it shared), so it must survive the conservative join
    // reset. This lets an array/object allocated BEFORE a loop stay fresh through the loop body —
    // the shape the linear join-reset otherwise rejects (e.g. `long[] bits = new long[n]; for(...)
    // bits[i]=…;` in a VoxelShape/BitSet build). Pre-scan the store opcodes to find such locals.
    let nloc = code.max_locals as usize;
    let mut astore_count = vec![0u32; nloc];
    let mut other_store = vec![false; nloc];
    {
        let mut p = 0usize;
        while p < bc.len() {
            let op = bc[p];
            let mark_astore = |v: &mut [u32], i: usize| {
                if let Some(c) = v.get_mut(i) {
                    *c += 1;
                }
            };
            let mark_other = |v: &mut [bool], i: usize| {
                if let Some(s) = v.get_mut(i) {
                    *s = true;
                }
            };
            match op {
                0x3a => mark_astore(&mut astore_count, *bc.get(p + 1).unwrap_or(&0) as usize),
                0x4b..=0x4e => mark_astore(&mut astore_count, (op - 0x4b) as usize),
                0x36 | 0x38 => mark_other(&mut other_store, *bc.get(p + 1).unwrap_or(&0) as usize),
                0x37 | 0x39 => mark_other(&mut other_store, *bc.get(p + 1).unwrap_or(&0) as usize),
                0x3b..=0x3e => mark_other(&mut other_store, (op - 0x3b) as usize),
                0x3f..=0x42 => mark_other(&mut other_store, (op - 0x3f) as usize),
                0x43..=0x46 => mark_other(&mut other_store, (op - 0x43) as usize),
                0x47..=0x4a => mark_other(&mut other_store, (op - 0x47) as usize),
                0x84 => mark_other(&mut other_store, *bc.get(p + 1).unwrap_or(&0) as usize), // iinc
                _ => {}
            }
            p += insn_len(bc, p).map_err(|()| OffReject::Malformed)?;
        }
    }
    let durable: Vec<bool> = (0..nloc)
        .map(|i| astore_count[i] == 1 && !other_store[i])
        .collect();
    // `frozen[i]` = this durable local has been assigned a fresh value → keep it fresh at joins.
    let mut frozen = vec![false; nloc];

    // Operand-stack freshness (one bool per JVM stack SLOT — a long/double occupies
    // two slots, both Shared). Locals freshness by slot.
    let mut stack: Vec<F> = Vec::new();
    let mut locals: Vec<F> = vec![F::Shared; code.max_locals as usize];
    if this_private {
        if let Some(l0) = locals.first_mut() {
            *l0 = F::This; // `this` (ctor-fresh or the distinct fork element): writable + typed
        }
    }
    if let Some(iv) = iv_local {
        if let Some(slot) = locals.get_mut(iv) {
            *slot = F::Iv; // counted-loop induction var: a per-iteration-distinct array index
        }
    }

    macro_rules! pop {
        () => {
            match stack.pop() {
                Some(f) => f,
                None => return Err(OffReject::Malformed), // stack underflow → reject
            }
        };
    }
    macro_rules! popn {
        ($n:expr) => {{
            let mut last = F::Shared;
            for _ in 0..$n {
                last = pop!();
            }
            last
        }};
    }
    // Local read/write with a bounds guard (out of range → reject the method).
    macro_rules! lget {
        ($i:expr) => {
            match locals.get($i) {
                Some(&f) => f,
                None => return Err(OffReject::Malformed),
            }
        };
    }
    macro_rules! lset {
        ($i:expr, $f:expr) => {
            match locals.get_mut($i) {
                Some(slot) => *slot = $f,
                None => return Err(OffReject::Malformed),
            }
        };
    }

    let mut pc = 0usize;
    while pc < bc.len() {
        // Join point: reset freshness to the conservative bottom (a value is
        // must-fresh here only if fresh on EVERY path; non-fresh is always safe).
        // `this` in an <init> is fresh on all paths, so it survives the reset.
        if targets.contains(&pc) {
            for s in stack.iter_mut() {
                *s = F::Shared;
            }
            for (i, l) in locals.iter_mut().enumerate() {
                if frozen[i] {
                    // durable single-assign fresh local — its freshness is stable, keep it.
                } else if iv_local == Some(i) {
                    *l = F::Iv; // induction var is an index on every path (iinc keeps it an index)
                } else {
                    *l = if this_private && i == 0 {
                        F::This
                    } else {
                        F::Shared
                    };
                }
            }
        }
        let op = bc[pc];
        match op {
            0x00 => {}                                           // nop
            0x01 => stack.push(F::Shared),                       // aconst_null
            0x02..=0x08 => stack.push(F::Shared),                // iconst_m1..5
            0x09 | 0x0a => stack.extend([F::Shared, F::Shared]), // lconst_0/1
            0x0b..=0x0d => stack.push(F::Shared),                // fconst_0..2
            0x0e | 0x0f => stack.extend([F::Shared, F::Shared]), // dconst_0/1
            0x10 | 0x11 => stack.push(F::Shared),                // bipush / sipush
            0x12 | 0x13 => {
                if txn {
                    return Err(OffReject::Opcode(pc, op));
                }
                stack.push(F::Shared);
            } // ldc / ldc_w may resolve shared constants
            0x14 => {
                if txn {
                    return Err(OffReject::Opcode(pc, op));
                }
                stack.extend([F::Shared, F::Shared]);
            } // ldc2_w
            // typed loads: prim narrow push 1 Shared; wide (long/double) push 2;
            // aload pushes the LOCAL's freshness.
            0x17 => stack.push(F::Shared), // fload
            0x15 => {
                // iload: an `iload iv` pushes the induction-var index tag (for `arr[iv]`).
                let idx = *bc.get(pc + 1).unwrap_or(&0) as usize;
                stack.push(if iv_local == Some(idx) {
                    F::Iv
                } else {
                    F::Shared
                });
            }
            0x16 | 0x18 => stack.extend([F::Shared, F::Shared]), // lload / dload
            0x19 => stack.push(lget!(*bc.get(pc + 1).unwrap_or(&0) as usize)), // aload
            0x1a..=0x1d => stack.push(if iv_local == Some((op - 0x1a) as usize) {
                F::Iv
            } else {
                F::Shared
            }), // iload_0..3
            0x1e..=0x21 => stack.extend([F::Shared, F::Shared]), // lload_0..3
            0x22..=0x25 => stack.push(F::Shared),                // fload_0..3
            0x26..=0x29 => stack.extend([F::Shared, F::Shared]), // dload_0..3
            0x2a..=0x2d => stack.push(lget!((op - 0x2a) as usize)), // aload_0..3
            // array LOADS: pop index + arrayref, push element (from a shared array
            // OR a fresh one — the element itself is not tracked as fresh).
            // BUG FIX: `baload` (0x33) was missing from this list even though the
            // comment claimed it, so every method that reads a `byte[]`/`boolean[]`
            // fell through to `arith_or_reject` and was refused as UNMODELLED — a pure
            // READ rejected. That single omission refused the whole vanilla noise leaf
            // (`ImprovedNoise.p` → `sampleAndLerp` → `noise` → `PerlinNoise.getValue`
            // → `NormalNoise.getValue`), i.e. the hottest provably-pure code in the
            // boot's ring phase.
            // FieldTxn mode: a load from a SHARED array is a shared read the
            // runtime cannot journal — only a proven-fresh (private) array may
            // be read (its elements are invisible to other threads by the same
            // freshness proof the writes rely on).
            0x2e | 0x30 | 0x32 | 0x33 | 0x34 | 0x35 => {
                let arr = popn!(2);
                if txn && !matches!(arr, F::Fresh(_) | F::This) {
                    return Err(OffReject::SharedRead(pc, op));
                }
                stack.push(F::Shared);
            } // iaload/faload/aaload/baload/caload/saload
            0x2f | 0x31 => {
                let arr = popn!(2);
                if txn && !matches!(arr, F::Fresh(_) | F::This) {
                    return Err(OffReject::SharedRead(pc, op));
                }
                stack.extend([F::Shared, F::Shared]);
            } // laload / daload
            // typed stores to a local.
            0x36 | 0x38 => {
                pop!();
            } // istore / fstore
            0x37 | 0x39 => {
                popn!(2);
            } // lstore / dstore
            0x3a => {
                let f = pop!();
                let idx = *bc.get(pc + 1).unwrap_or(&0) as usize;
                lset!(idx, f);
                if durable.get(idx).copied().unwrap_or(false) && matches!(f, F::Fresh(_) | F::This)
                {
                    if let Some(fr) = frozen.get_mut(idx) {
                        *fr = true;
                    }
                }
            } // astore
            0x3b..=0x3e => {
                pop!();
            } // istore_0..3
            0x3f..=0x42 => {
                popn!(2);
            } // lstore_0..3
            0x43..=0x46 => {
                pop!();
            } // fstore_0..3
            0x47..=0x4a => {
                popn!(2);
            } // dstore_0..3
            0x4b..=0x4e => {
                let f = pop!();
                let idx = (op - 0x4b) as usize;
                lset!(idx, f);
                if durable.get(idx).copied().unwrap_or(false) && matches!(f, F::Fresh(_) | F::This)
                {
                    if let Some(fr) = frozen.get_mut(idx) {
                        *fr = true;
                    }
                }
            } // astore_0..3
            // array STORES: [arrayref, index, value] — a shared-array write unless
            // the array is FRESH. narrow value pops 1 slot, wide pops 2.
            0x4f | 0x51 | 0x53 | 0x54 | 0x55 | 0x56 => {
                pop!(); // value
                let idx = pop!(); // index
                let arr = pop!();
                // An `arr[iv]` write is index-partitioned — distinct across the counted loop's
                // distinct iv values → disjoint → safe even on a SHARED array. Otherwise the
                // array must be private (Fresh/This).
                if !relaxed && !matches!(idx, F::Iv) && !matches!(arr, F::Fresh(_) | F::This) {
                    return Err(OffReject::SharedWrite(pc, op));
                }
            } // iastore/fastore/aastore/bastore/castore/sastore
            0x50 | 0x52 => {
                popn!(2); // wide value
                let idx = pop!(); // index
                let arr = pop!();
                if !relaxed && !matches!(idx, F::Iv) && !matches!(arr, F::Fresh(_) | F::This) {
                    return Err(OffReject::SharedWrite(pc, op));
                }
            } // lastore / dastore
            0x57 => {
                pop!();
            } // pop
            0x58 => {
                popn!(2);
            } // pop2
            0x59 => {
                let t = pop!();
                stack.push(t);
                stack.push(t);
            } // dup
            0x5a => {
                // dup_x1: [.., b, a] -> [.., a, b, a]
                let a = pop!();
                let b = pop!();
                stack.push(a);
                stack.push(b);
                stack.push(a);
            }
            0x5c => {
                // dup2 (form 1: two 1-slot values): [.., b, a] -> [.., b, a, b, a]
                let a = pop!();
                let b = pop!();
                stack.push(b);
                stack.push(a);
                stack.push(b);
                stack.push(a);
            }
            0x5f => {
                // swap
                let a = pop!();
                let b = pop!();
                stack.push(a);
                stack.push(b);
            }
            // Everything else (arithmetic, conversions, compares, branches, returns,
            // fields, invokes, allocation) is modelled by `arith_or_reject`, which
            // also rejects any unmodelled opcode.
            _ => {
                arith_or_reject(
                    op,
                    bc,
                    pc,
                    pool,
                    bsm,
                    &mut stack,
                    relaxed,
                    txn,
                    self_class,
                    callee_offloadable,
                )?;
            }
        }
        pc += insn_len(bc, pc).map_err(|()| OffReject::Malformed)?;
    }
    Ok(())
}
