//! Counted loops: the induction variable, the back edge, and what stays invariant across it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Can this method's constant-slot field loads be marked `readonly` (so the
/// egraph hoists loop-invariant ones — field-LICM)? True iff the body performs
/// NO heap mutation and NO callback that could GC/realloc/mutate during the
/// call: no putfield/putstatic, no allocation, and every invoke is INLINED (an
/// inlinable callee is pure-reader by `is_inlinable` — it forbids putfield/
/// invoke/new). A real call or any write disqualifies. `readonly` requires the
/// memory be immutable for the whole call, which these conditions guarantee
/// (single-threaded under the GIL; no callback ⇒ no GC/compaction mid-call).
pub(crate) fn field_loads_invariant(
    bc: &[u8],
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            0xb3 | 0xb5 => return false,        // putstatic / putfield — heap write
            0xbb | 0xbc | 0xbd => return false, // new / newarray / anewarray
            // ⚠**TWO MORE REAL CALLS THAT WERE FALLING THROUGH THE `_` ARM.** The tightening
            // below states the standard: membership must mean no GC-capable, heap-mutating call
            // survives. Two opcodes emit exactly such a call and were never listed, so a body
            // containing one could be stamped `readonly` and have a field load hoisted across it:
            //
            //  * `0xba invokedynamic` — mints the lambda object (`emit/lambda_mint.rs`). That is
            //    an ALLOCATION, the same hazard `0xbb new` is rejected for one line above, plus a
            //    real call when the site is not fused. Nothing distinguishes it from `new` here.
            //  * `0xc1 instanceof` — the emitted arm is a spill/call/`reload_locals_after_gc`
            //    sequence (`emit/ops/typecheck.rs`); the callback resolves the target class,
            //    which can LOAD it and run its `<clinit>`, i.e. arbitrary guest code that writes
            //    fields and collects. `0xc0 checkcast` may allocate an exception as well.
            //
            // Cost of rejecting them: a loop whose body lambdas or type-tests stops claiming
            // invariance — the coverage it had before those opcodes were compiled at all.
            0xba | 0xc0 | 0xc1 | 0xbf => return false, // athrow null constructs an exception
            0xb6 | 0xb7 | 0xb9 => {
                let idx = if pc + 2 < bc.len() {
                    ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16
                } else {
                    return false;
                };
                // ⚠**MEMBERSHIP IN `inline_targets` NO LONGER MEANS "NO REAL CALL SURVIVES".**
                //
                // This predicate stamps `readonly` on the method's arena field loads
                // (`fields_arena`, `fields_ref`, `fields_prim`), which licenses Cranelift to hoist
                // a field read across everything between it and its use. That was sound while an
                // entry in `inline_targets` meant the callee is a leaf spliced in whole — nothing
                // survived that could write the heap or collect.
                //
                // It stopped being true when guarded devirtualisation landed: an entry carrying
                // `guard_class`/`guard_ref_word` emits a class-id compare with the inlined body on
                // the HIT arm and a **real `emit_dispatch_pic`/`emit_invoke` on the miss arm** —
                // GC-capable and heap-mutating. Tier-0 partial maps have the same exposure. So a
                // method containing one guarded site could be marked invariant and then have a
                // field load hoisted across that call, reading a value the callee had overwritten
                // or a pointer it had moved.
                //
                // Requiring the entry to be UNGUARDED restores the original meaning of membership.
                // The cost is coverage, not correctness: a method with a guarded site simply stops
                // claiming invariance, which is what it did before the guard existed.
                // ⭐The condition this used to spell out inline now has a NAME and one
                // definition (`InlineCallee::leaves_gc_point`), because this round added a THIRD
                // shape of entry that leaves a real GC point live: a ROOTED SPLICE, whose body
                // allocates and/or still calls. Same verdict, same cost — a method containing one
                // stops claiming invariance — reached through the predicate instead of a
                // re-transcription that the next shape would have had to find and update.
                match inline_targets.get(&idx) {
                    Some(t) if !t.leaves_gc_point() => {}
                    _ => return false,
                }
            }
            0xb8 => {
                let idx = if pc + 2 < bc.len() {
                    ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16
                } else {
                    return false;
                };
                // Same rule for the static site: an entry that leaves a GC point live is not a
                // fold, whatever opcode reaches it.
                let folded = matches!(inline_targets.get(&idx), Some(t) if !t.leaves_gc_point());
                if !folded && !static_intrinsics.contains_key(&idx) {
                    return false;
                }
            }
            _ => {}
        }
        pc += len;
    }
    true
}

pub(crate) fn find_leaders(bc: &[u8]) -> Option<Vec<usize>> {
    let mut leaders = std::collections::BTreeSet::new();
    let mut starts = std::collections::BTreeSet::new();
    leaders.insert(0usize);
    let mut pc = 0;
    while pc < bc.len() {
        starts.insert(pc);
        let op = bc[pc];
        if is_excluded(op) {
            return None;
        }
        let len = insn_len_at(bc, pc)?;
        if len == 0 || pc.checked_add(len)? > bc.len() {
            return None;
        }
        if matches!(op, 0x99..=0x9e | 0x9f..=0xa6 | 0xa7 | 0xc6 | 0xc7) {
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            leaders.insert(target);
            // The fall-through block starts at pc+len — but ONLY if it exists. A branch that is
            // the method's LAST instruction (e.g. a `goto` loop back-edge, gson JsonReader.
            // nextQuotedValue ends `293: goto 7`) has pc+len == bc.len(); inserting that as a
            // leader is a phantom (no instruction there) that failed the start-set check below and
            // wrongly rejected the whole method. Guard it — a real correctness fix in the CFG builder.
            if pc + len < bc.len() {
                leaders.insert(pc + len);
            }
        } else if op == 0xaa || op == 0xab {
            // tableswitch / lookupswitch: the default + every case target is a leader.
            let (default_t, cases) = switch_targets(bc, pc)?;
            leaders.insert(default_t);
            for (_, t) in &cases {
                leaders.insert(*t);
            }
            if pc + len < bc.len() {
                leaders.insert(pc + len);
            }
        }
        pc += len;
    }
    // Every leader must be a real instruction start (reject phantom targets).
    if leaders.iter().any(|l| !starts.contains(l)) {
        return None;
    }
    Some(leaders.into_iter().collect())
}

/// The int constant an instruction pushes, if it pushes one with no operands of its own.
/// `iconst_m1..5`, `bipush`, `sipush` — everything the induction-variable proof needs to read a
/// literal out of the byte stream without the constant pool.
fn const_int_at(bc: &[u8], p: usize) -> Option<i32> {
    match *bc.get(p)? {
        op @ 0x02..=0x08 => Some(op as i32 - 0x03), // iconst_m1..iconst_5
        0x10 => Some(*bc.get(p + 1)? as i8 as i32), // bipush
        0x11 => Some(s16(bc, p + 1)),               // sipush
        _ => None,
    }
}

/// Does the instruction at `p` leave a value on the stack that is PROVABLY ≥ 0?
///
/// `q` is the instruction linearly before `p` — the producer of `p`'s SECOND operand for the
/// binary forms. `widen` is the round's kill switch: with it off only the three original constant
/// pushes count, so the extra facts can be bisected away without touching the shape of the walk.
///
/// Each arm is a JVMS guarantee, not an inference:
///  * `arraylength` — an array's length is a non-negative int by construction; no array can be
///    created with a negative size (`newarray` throws `NegativeArraySizeException` first).
///  * `i2c` — truncates to 16 bits and ZERO-extends, so the result is in `[0, 65535]`. Its
///    siblings `i2b` (0x91) and `i2s` (0x93) SIGN-extend and are deliberately absent.
///  * `iand` with a non-negative constant — `a & c` clears the sign bit whenever `c`'s is clear,
///    so the result is in `[0, c]`. This is the `x & MASK` index shape.
///  * `iushr` by a constant with `(c & 31) != 0` — the shift amount is masked to 5 bits, so a
///    non-zero one shifts a zero into the sign bit. A shift of 0 is the identity and is refused.
fn pushes_nonneg_int(bc: &[u8], p: usize, q: Option<usize>, widen: bool) -> bool {
    match bc.get(p).copied() {
        Some(0x03..=0x08) => true,                                        // iconst_0..5
        Some(0x10) => bc.get(p + 1).map(|&n| (n as i8) >= 0).unwrap_or(false), // bipush N
        Some(0x11) => s16(bc, p + 1) >= 0,                                // sipush N
        _ if !widen => false,
        Some(0xbe) => true, // arraylength
        Some(0x92) => true, // i2c
        Some(0x7e) => q.and_then(|qq| const_int_at(bc, qq)).map_or(false, |c| c >= 0), // iand
        Some(0x7c) => q.and_then(|qq| const_int_at(bc, qq)).map_or(false, |c| c & 31 != 0), // iushr
        _ => false,
    }
}

/// Is `target` the destination of ANY branch or switch edge in this method?
///
/// ⚠**THE PROOF BELOW READS THE LINEARLY PRECEDING INSTRUCTION AS "THE PRODUCER", AND A MERGE
/// POINT BREAKS THAT.** javac compiles `int i = c ? n : 0;` to
/// `… ifeq L ; iload n ; goto E ; L: iconst_0 ; E: istore i` — the store's linear predecessor is
/// `iconst_0`, and the OTHER path stores `n`, which may be negative. Believing the literal there
/// would hand `safe_bce_loads` a false `iv ≥ 0`, and a false one is not a missed optimisation: it
/// deletes a bounds check that was load-bearing, so `arr[iv]` reads at a negative offset from the
/// element base. So an `istore iv` that anything can jump TO is refused outright.
///
/// Fails CLOSED — an undecodable opcode answers `true` ("assume it is a target"), which rejects
/// the store and turns loop-BCE off for the method. Called once per `istore iv` (normally one per
/// method, since a second assignment to the same slot is what makes a variable not an induction
/// variable), so this stays a linear scan with no side table to keep.
fn is_branch_target(bc: &[u8], target: usize) -> bool {
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let len = match insn_len_at(bc, pc) {
            Some(l) => l,
            None => return true,
        };
        match op {
            0x99..=0xa6 | 0xa7 | 0xc6 | 0xc7 => {
                // `s16` indexes unguarded; a truncated branch at the tail of a malformed body
                // would panic, so bound it here and fail closed on the short read.
                if pc + 3 > bc.len() {
                    return true;
                }
                if (pc as i32 + s16(bc, pc + 1)) as usize == target {
                    return true;
                }
            }
            0xaa | 0xab => match switch_targets(bc, pc) {
                Some((default_t, cases)) => {
                    if default_t == target || cases.iter().any(|&(_, t)| t == target) {
                        return true;
                    }
                }
                None => return true,
            },
            _ => {}
        }
        pc += len;
    }
    false
}

/// Whole-method proof that induction var `iv` (a local slot) is ALWAYS ≥ 0.
///
/// Every write to `iv` must be either (a) an `istore iv` that NOTHING BRANCHES TO and whose
/// immediately-preceding instruction leaves a provably non-negative int (`pushes_nonneg_int`), or
/// (b) an `iinc iv, delta` with `delta ≥ 0`. Any other write — istore from an unprovable
/// producer, a negative `iinc`, or ANY `wide`-prefixed istore/iinc targeting `iv` — makes it
/// unprovable → false. (Starts ≥ 0, only ever grows ⇒ ≥ 0 everywhere.)
///
/// **WHAT IT REMOVES FROM THE EMITTED CODE.** This is the soundness gate on `safe_bce_loads`, so
/// every `arr[iv]` it unlocks inside a canonical counted loop drops its per-element
/// `arraylength` load, the `icmp uge`, and the `brif` to the AIOOBE block — the array descriptor
/// read DCEs out with them (`emit/arrays.rs`, `skip_bounds`). A single `false` here turns that
/// off for the whole method.
///
/// ⭐**TWO WIDENINGS, EACH A JVMS GUARANTEE** (kill switch `RAVA_NO_FAST_TYPES=1`):
///  * `iinc` was pinned to `delta == 1`, which rejected the ENTIRE METHOD for a `i += 2` /
///    `i += 3` stride — the shape a loop walking packed triples or a bitset word-at-a-time uses.
///    Non-negativity needs only `delta ≥ 0`, and the consumer never reads the step: it marks
///    loads BEFORE the body's first `iinc`, where the header compare has already proven
///    `iv < arr.length` whatever the stride is.
///  * the initialiser producer set (`pushes_nonneg_int`). This matters out of proportion because
///    the gate is WHOLE-METHOD: one unrelated `iv = h & 0xff` or `iv = a.length` elsewhere in the
///    body used to disqualify the counted loop that shares the slot.
pub(crate) fn iv_nonneg_monotonic(bc: &[u8], iv: usize) -> bool {
    // `widen` is `RAVA_NO_FAST_TYPES`'s inverse — the round's extra facts. Named apart from the
    // `wide` PREFIX opcode (0xc4) handled at the bottom of this loop, which is a different thing.
    let widen = super::localtypes::fast_types();
    let mut pc = 0usize;
    let mut prev: Option<usize> = None; // linear-preceding instruction PC
    let mut prev2: Option<usize> = None; // the one before that (the binary forms' 2nd operand)
    while pc < bc.len() {
        let op = bc[pc];
        let len = match insn_len_at(bc, pc) {
            Some(l) => l,
            None => return false,
        };
        // istore iv (0x36 / istore_0..3 0x3b..=0x3e)
        let istore_target: Option<usize> = match op {
            0x36 => bc.get(pc + 1).map(|&x| x as usize),
            0x3b..=0x3e => Some((op - 0x3b) as usize),
            _ => None,
        };
        if istore_target == Some(iv) {
            let ok = match prev {
                Some(pp) => pushes_nonneg_int(bc, pp, prev2, widen) && !is_branch_target(bc, pc),
                None => false,
            };
            if !ok {
                return false;
            }
        }
        // iinc iv, delta
        if op == 0x84 {
            let idx = match bc.get(pc + 1) {
                Some(&x) => x as usize,
                None => return false,
            };
            if idx == iv {
                let delta = match bc.get(pc + 2) {
                    Some(&x) => x as i8 as i32,
                    None => return false,
                };
                let ok = if widen { delta >= 0 } else { delta == 1 };
                if !ok {
                    return false;
                }
            }
        }
        // wide-prefixed istore / iinc targeting iv → unprovable.
        if op == 0xc4 {
            if let Some(&wop) = bc.get(pc + 1) {
                if wop == 0x36 || wop == 0x84 {
                    let widx = ((*bc.get(pc + 2).unwrap_or(&0) as usize) << 8)
                        | *bc.get(pc + 3).unwrap_or(&0) as usize;
                    if widx == iv {
                        return false;
                    }
                }
            }
        }
        prev2 = prev;
        prev = Some(pc);
        pc += len;
    }
    true
}

/// Every opcode allowed inside the folded loop's INVARIANT expression: it must
/// compute a single `int` from getstatic (primitive) + int constants + pure int
/// arithmetic — NEVER load a local (so it cannot depend on `i` or `acc`), never
/// allocate, never store/branch. This is what makes `INV` provably loop-invariant.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn is_invariant_expr_op(op: u8) -> bool {
    matches!(op,
        0xb2                       // getstatic
        | 0x02..=0x08              // iconst_m1..5
        | 0x10 | 0x11              // bipush / sipush
        | 0x60 | 0x64 | 0x68       // iadd / isub / imul
        | 0x74                     // ineg
        | 0x7e | 0x80 | 0x82       // iand / ior / ixor
        | 0x78 | 0x7a | 0x7c       // ishl / ishr / iushr
        | 0x59                     // dup (cheap stack juggling, no effect)
    )
}

/// Every opcode permitted inside a DATA-PARALLEL loop body expression: it must
/// compute an integer from the counter / parameter locals + constants via PURE
/// integer arithmetic — never store, branch, call, allocate, or touch a field/array
/// (any of which could carry state across iterations or mutate shared memory). This
/// is what proves the iterations independent + side-effect-free.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn is_parallel_body_op(op: u8) -> bool {
    matches!(op,
        0x02..=0x08              // iconst_m1..5
        | 0x09 | 0x0a            // lconst_0..1
        | 0x10 | 0x11            // bipush / sipush
        | 0x15 | 0x16            // iload / lload <idx>
        | 0x1a..=0x1d            // iload_0..3
        | 0x1e..=0x21            // lload_0..3
        | 0x36 | 0x37            // istore / lstore <idx>  (scratch locals)
        | 0x3b..=0x42            // istore_0..3 / lstore_0..3
        | 0x60..=0x83            // i/l add,sub,mul,div,rem,neg,sh*,and,or,xor
        | 0x85 | 0x88            // i2l / l2i
        | 0x91..=0x93            // i2b / i2c / i2s
        | 0x57 | 0x58 | 0x59     // pop / pop2 / dup
        | 0x5a..=0x5e            // dup_x1..dup2_x2
        | 0x5f                   // swap
        // SROA: a CONFINED short-lived object (new + trivial ctor + own field
        // access + ref-local moves) is scalar-replaced by the body emitter — no
        // allocation. Any escaping consumer of the reference isn't in this set, so
        // the emitter (fail-closed) bails to the serial path.
        | 0x19 | 0x2a..=0x2d     // aload / aload_0..3 (a virtual-object / captured-array ref)
        | 0x2e | 0x2f | 0x31     // iaload / laload / daload (read of a captured primitive array)
        | 0x3a | 0x4b..=0x4e     // astore / astore_0..3
        // double value ops (MAP only — the reduce fold gate rejects double folds). Emitted
        // bits-based (a double rides an i64 cell; the ops bitcast in/out).
        | 0x0e | 0x0f            // dconst_0..1
        | 0x18 | 0x26..=0x29     // dload / dload_0..3
        | 0x39 | 0x47..=0x4a     // dstore / dstore_0..3
        | 0x87 | 0x8a            // i2d / l2d
        | 0x14                   // ldc2_w (long/double literal — dispatch resolves the value)
        | 0xbb                   // new
        | 0xb7                   // invokespecial <init>
        | 0xb4 | 0xb5            // getfield / putfield (own object)
        | 0xb6                   // invokevirtual (trivial getter — inlined, else bail)
        | 0xb8                   // invokestatic (only Math.max/min — the emitter gates it)
    )
}

/// Recognise the bottom-test counted loop anchored at `goto_pc` (the leading `goto COND`):
/// ```text
///   goto_pc: goto COND
///   BODY(=goto_pc+3): <body ...>
///            iinc iv, step        (body_end)
///   COND:    iload iv ; <push bound> ; if_icmplt/if_icmple BODY
///   EXIT:    ...
/// ```
/// Structural only. `None` if not this exact shape.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn match_numeric_range_loop(bc: &[u8], goto_pc: usize) -> Option<NumericRangeLoop> {
    let iload_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x15 => Some(*bc.get(p + 1)? as usize),
            op @ 0x1a..=0x1d => Some((op - 0x1a) as usize),
            _ => None,
        }
    };
    // Leading `goto COND`.
    if *bc.get(goto_pc)? != 0xa7 {
        return None;
    }
    let cond_pc = (goto_pc as i32 + s16(bc, goto_pc + 1)) as usize;
    let body_start_pc = goto_pc + 3;
    if cond_pc <= body_start_pc {
        return None; // cond must be forward of the body (a real forward loop)
    }
    // The `iinc iv, step` must be the last instruction before COND (3 bytes: 0x84 iv step).
    let body_end_pc = cond_pc.checked_sub(3)?;
    if body_end_pc < body_start_pc || *bc.get(body_end_pc)? != 0x84 {
        return None;
    }
    let iv_local = *bc.get(body_end_pc + 1)? as usize;
    let step = *bc.get(body_end_pc + 2)? as i8 as i32;
    if step <= 0 {
        return None; // only ascending counted loops
    }
    // COND: iload iv ; <push bound> ; if_icmplt/if_icmple BODY.
    let mut q = cond_pc;
    if iload_at(q)? != iv_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    let bound = match *bc.get(q)? {
        0x15 | 0x1a..=0x1d => RangeBound::Local(iload_at(q)?),
        0x02..=0x08 => RangeBound::Const(*bc.get(q)? as i32 - 0x03), // iconst_m1..5
        0x10 => RangeBound::Const(*bc.get(q + 1)? as i8 as i32),     // bipush
        0x11 => RangeBound::Const(s16(bc, q + 1)),                   // sipush
        _ => return None,
    };
    q += insn_len_at(bc, q)?;
    let cmp = *bc.get(q)?;
    let inclusive = match cmp {
        0xa1 => false, // if_icmplt → `<`
        0xa4 => true,  // if_icmple → `<=`
        _ => return None,
    };
    // Back-edge must jump to the body start.
    if (q as i32 + s16(bc, q + 1)) as usize != body_start_pc {
        return None;
    }
    let loop_exit_pc = q + 3;
    Some(NumericRangeLoop {
        iv_local,
        bound,
        inclusive,
        step,
        body_start_pc,
        body_end_pc,
        cond_pc,
        loop_exit_pc,
    })
}

/// Recognise the TOP-test counted loop — the shape javac ACTUALLY emits for `for (int i = lo;
/// i < hi; i++)` (the bottom-test form above is rarer). Anchored at the trailing BACKWARD `goto
/// COND`:
/// ```text
///   COND: iload iv ; <push bound> ; if_icmpge/if_icmpgt END
///         <body ...>
///         iinc iv, step        (body_end, immediately before the goto)
///   goto_pc: goto COND         (backward)
///   END:  ...
/// ```
/// `if_icmpge` (exit on `iv >= bound`) ⇒ the loop runs `iv < bound` (exclusive); `if_icmpgt` ⇒
/// `iv <= bound` (inclusive). Structural only. `None` if not this exact shape.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn match_numeric_range_loop_toptest(bc: &[u8], goto_pc: usize) -> Option<NumericRangeLoop> {
    let iload_at = |p: usize| -> Option<usize> {
        match *bc.get(p)? {
            0x15 => Some(*bc.get(p + 1)? as usize),
            op @ 0x1a..=0x1d => Some((op - 0x1a) as usize),
            _ => None,
        }
    };
    // Trailing BACKWARD `goto COND`.
    if *bc.get(goto_pc)? != 0xa7 {
        return None;
    }
    let cond_pc = (goto_pc as i32 + s16(bc, goto_pc + 1)) as usize;
    if cond_pc >= goto_pc {
        return None; // must jump backward to the condition
    }
    // The `iinc iv, step` sits immediately before the goto (3 bytes).
    let body_end_pc = goto_pc.checked_sub(3)?;
    if *bc.get(body_end_pc)? != 0x84 {
        return None;
    }
    let iv_local = *bc.get(body_end_pc + 1)? as usize;
    let step = *bc.get(body_end_pc + 2)? as i8 as i32;
    if step <= 0 {
        return None;
    }
    // COND: iload iv ; <push bound> ; if_icmpge/if_icmpgt END.
    let mut q = cond_pc;
    if iload_at(q)? != iv_local {
        return None;
    }
    q += insn_len_at(bc, q)?;
    let bound = match *bc.get(q)? {
        0x15 | 0x1a..=0x1d => RangeBound::Local(iload_at(q)?),
        0x02..=0x08 => RangeBound::Const(*bc.get(q)? as i32 - 0x03),
        0x10 => RangeBound::Const(*bc.get(q + 1)? as i8 as i32),
        0x11 => RangeBound::Const(s16(bc, q + 1)),
        _ => return None,
    };
    q += insn_len_at(bc, q)?;
    let inclusive = match *bc.get(q)? {
        0xa2 => false, // if_icmpge → exits on iv>=bound → loop is iv<bound
        0xa3 => true,  // if_icmpgt → exits on iv>bound  → loop is iv<=bound
        _ => return None,
    };
    let loop_exit_pc = (q as i32 + s16(bc, q + 1)) as usize; // END = the compare's exit target
    let body_start_pc = q + 3;
    if body_start_pc > body_end_pc {
        return None; // body must be non-empty (the iinc is at body_end)
    }
    Some(NumericRangeLoop {
        iv_local,
        bound,
        inclusive,
        step,
        body_start_pc,
        body_end_pc,
        cond_pc,
        loop_exit_pc,
    })
}

/// Largest range-loop whose back-edge `goto` lies within `[lo, hi)` (the direct child loop of an
/// enclosing body). Scans for backward gotos, matches each, returns the one with the widest body.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inner_range_loop(bc: &[u8], lo: usize, hi: usize) -> Option<NumericRangeLoop> {
    let mut best: Option<NumericRangeLoop> = None;
    let mut p = lo;
    while p < hi {
        if *bc.get(p)? == 0xa7 {
            let tgt = (p as i32 + s16(bc, p + 1)) as usize;
            if tgt < p && tgt >= lo {
                if let Some(m) = match_numeric_range_loop_toptest(bc, p) {
                    let span = m.body_end_pc.saturating_sub(m.body_start_pc);
                    if best.as_ref().map_or(true, |b| span > b.body_end_pc.saturating_sub(b.body_start_pc)) {
                        best = Some(m);
                    }
                }
            }
        }
        p += insn_len_at(bc, p)?;
    }
    best
}

pub(crate) fn has_back_edge(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        if matches!(op, 0x99..=0xa6 | 0xa7 | 0xc6 | 0xc7) {
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            if target <= pc {
                return true;
            }
        }
        pc += len;
    }
    false
}
