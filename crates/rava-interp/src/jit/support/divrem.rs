//! Division, remainder, shifts and three-way compare: the JVM rules that need explicit guards.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// 20.5 STATIC LIVENESS AVOIDANCE — is a non-leaf callee GC-SAFE to PARTIAL-inline (splice its body
/// while leaving invokes NOT in `inlined` as real jit_calls)? A mid-body real call can trigger GC, and
/// the callee's live refs sit in Cranelift SSA vars that are NOT in the flat `cells` root set the GC
/// scans — so it is safe ONLY IF no reference is live across such a call. Rather than a bug-prone
/// full stack-type simulator, use a SOUND structural check valid for verified bytecode:
///   • straight-line (no branch/switch) — a single linear path, so no loop-carried live ref;
///   • EXACTLY ONE real call (0 ⇒ fully collapsed ⇒ trivially safe; >1 ⇒ an earlier ref result can be
///     live across a later call ⇒ refuse);
///   • that call is in TAIL position: after it, only checkcast / numeric-conversions / a return — so
///     NOTHING reads a local or the heap after the call (no local ref live-across), and stack balance
///     (guaranteed by verification) means the operand stack at the call is exactly the args (no ref
///     below them). Covers the transit delegations `return helper(this.x)` that dominate DFU/Codec.
/// Any unmodelled shape ⇒ false (conservative: fewer inlines, never a GC crash).
/// V4.2: EXACTLY the `java/lang/Math` calls that `math_intrinsic` (emit_op's 0xb8 arm)
/// lowers to a native `smax`/`smin` — no call is emitted, so `partial_inline_gc_safe` must
/// NOT count them as GC-capable real calls. Integer max/min ONLY (exact; FP max/min omitted
/// to stay conservative). MUST stay in lock-step with `math_intrinsic`: recognizing a call
/// here that emit_op does NOT actually intrinsic would leave an UNCOUNTED real call →
/// unsound (GC hazard). Generic — `java/lang/Math` is a JDK class, not an app crutch.
pub(crate) fn math_minmax_noncall(pool: &ConstantPool, cp: u16) -> bool {
    match pool.member_ref(cp) {
        Some(m) => {
            m.owner == "java/lang/Math"
                && matches!(
                    (&*m.name, &*m.descriptor),
                    ("max", "(II)I") | ("max", "(JJ)J") | ("min", "(II)I") | ("min", "(JJ)J")
                )
        }
        None => false,
    }
}

pub(crate) fn adjacent_i32_divisor(
    bc: &[u8],
    pc: usize,
    blocks: &std::collections::HashMap<usize, cranelift::prelude::Block>,
) -> Option<i32> {
    // If another CFG edge can enter the div/rem directly, its operand stack did
    // not necessarily execute the adjacent constant producer.
    if blocks.contains_key(&pc) {
        return None;
    }
    adjacent_i32_divisor_value(bc, pc)
}

pub(crate) fn adjacent_i32_divisor_value(bc: &[u8], pc: usize) -> Option<i32> {
    let producer = previous_insn_start(bc, pc)?;
    let divisor = match *bc.get(producer)? {
        0x02 => -1,
        op @ 0x03..=0x08 => i32::from(op - 0x03),
        0x10 => i32::from(*bc.get(producer + 1)? as i8),
        0x11 => i32::from(i16::from_be_bytes([
            *bc.get(producer + 1)?,
            *bc.get(producer + 2)?,
        ])),
        _ => return None,
    };
    (divisor != 0).then_some(divisor)
}

pub(crate) fn fast_i32_divisor(divisor: i32) -> bool {
    divisor == -1
        || divisor
            .checked_abs()
            .is_some_and(|n| (n as u32).is_power_of_two())
}

/// The div's dividend is a local loaded only after falling through a same-local
/// sign guard. Any explicit branch/switch edge into the guard, dividend load,
/// divisor producer, or div invalidates the fact: otherwise another predecessor
/// could bypass part of the proof sequence. This is a bytecode/CFG range proof,
/// not a profile or method-identity heuristic.
pub(crate) fn guarded_nonnegative_i32_dividend(bc: &[u8], div_pc: usize) -> bool {
    let Some(divisor_pc) = previous_insn_start(bc, div_pc) else {
        return false;
    };
    let Some(load_pc) = previous_insn_start(bc, divisor_pc) else {
        return false;
    };
    let Some(local) = iload_local(bc, load_pc) else {
        return false;
    };
    let Some(guard_pc) = previous_insn_start(bc, load_pc) else {
        return false;
    };
    if !matches!(bc.get(guard_pc), Some(0x9b | 0x9e))
        || guard_pc + 3 != load_pc
    {
        return false;
    }
    let Some(guard_load_pc) = previous_insn_start(bc, guard_pc) else {
        return false;
    };
    if iload_local(bc, guard_load_pc) != Some(local) {
        return false;
    }

    let protected = [guard_pc, load_pc, divisor_pc, div_pc];
    let mut pc = 0usize;
    while pc < bc.len() {
        let Some(len) = insn_len_at(bc, pc) else {
            return false;
        };
        let op = bc[pc];
        if matches!(op, 0xa8 | 0xa9 | 0xc4 | 0xc9) {
            return false; // legacy/dynamic subroutine flow is not range-proven
        } else if matches!(op, 0x99..=0xa7 | 0xc6 | 0xc7) {
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            if protected.contains(&target) {
                return false;
            }
        } else if op == 0xc8 {
            let offset =
                i32::from_be_bytes([bc[pc + 1], bc[pc + 2], bc[pc + 3], bc[pc + 4]]);
            let target = (pc as i32 + offset) as usize;
            if protected.contains(&target) {
                return false;
            }
        } else if matches!(op, 0xaa | 0xab) {
            let Some((default_target, cases)) = switch_targets(bc, pc) else {
                return false;
            };
            if protected.contains(&default_target)
                || cases
                    .iter()
                    .any(|(_, target)| protected.contains(target))
            {
                return false;
            }
        }
        pc += len;
    }
    true
}

pub(crate) fn int_divrem_constant(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    is_rem: bool,
    divisor: i32,
    nonnegative: bool,
) -> Option<()> {
    let _y = stack.pop()?;
    let x = stack.pop()?;
    let result = if divisor == -1 {
        if is_rem {
            b.ins().iconst(I32, 0)
        } else {
            b.ins().ineg(x.v)
        }
    } else {
        let magnitude = divisor.unsigned_abs();
        if magnitude.is_power_of_two() {
            let positive_q = if nonnegative {
                b.ins()
                    .sshr_imm(x.v, i64::from(magnitude.trailing_zeros()))
            } else {
                let sign = b.ins().sshr_imm(x.v, 31);
                let bias = b.ins().band_imm(sign, i64::from(magnitude - 1));
                let biased = b.ins().iadd(x.v, bias);
                b.ins()
                    .sshr_imm(biased, i64::from(magnitude.trailing_zeros()))
            };
            let quotient = if divisor < 0 { b.ins().ineg(positive_q) } else { positive_q };
            if is_rem {
                let product = b.ins().imul_imm(quotient, i64::from(divisor));
                b.ins().isub(x.v, product)
            } else {
                quotient
            }
        } else {
            // Non-power-of-2 constant divisor: emit a BARE srem/sdiv with NO
            // divisor==0 guard (the constant is non-zero, and != -1 here, so
            // Cranelift's trap can never fire). srem/sdiv match Java semantics
            // (remainder follows the dividend's sign; division truncates toward
            // zero). Removes the guard branch + /0 callback the general path adds.
            let d = b.ins().iconst(I32, i64::from(divisor));
            if is_rem {
                b.ins().srem(x.v, d)
            } else {
                b.ins().sdiv(x.v, d)
            }
        }
    };
    push(stack, result, JT::Int);
    Some(())
}

pub(crate) fn int_divrem(b: &mut FunctionBuilder, stack: &mut Vec<SV>, env: &Env, is_rem: bool) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    // NATIVE idiv/irem (next-gen-gc): inline `srem`/`sdiv` with a cold divisor==0
    // guard + the INT_MIN/-1 overflow special-case (Cranelift's sdiv/srem TRAP on
    // both). Only `/ by zero` reaches the callback. Kills the per-op divrem callback
    // (e.g. Dispatch's `i % shapes.length` = 26M `rava_i32_divrem` → 0).
    #[cfg(feature = "next-gen-gc")]
    {
        let dbz = b.create_block();
        let ok = b.create_block();
        let neg1 = b.create_block();
        let divb = b.create_block();
        let merge = b.create_block();
        b.append_block_param(merge, I32);

        let is_zero = b.ins().icmp_imm(IntCC::Equal, y.v, 0);
        b.ins().brif(is_zero, dbz, &[], ok, &[]);

        // Cold: divisor 0 → reuse the callback (sets ArithmeticException pending), 0.
        b.switch_to_block(dbz);
        b.seal_block(dbz);
        let lhs = b.ins().sextend(I64, x.v);
        let rhs = b.ins().sextend(I64, y.v);
        let rem = b.ins().iconst(I64, i64::from(is_rem));
        let call = b.ins().call(env.i32_divrem, &[lhs, rhs, rem]);
        let res = b.inst_results(call)[0];
        let z = b.ins().ireduce(I32, res);
        b.ins().jump(merge, &[z.into()]);

        b.switch_to_block(ok);
        b.seal_block(ok);
        let is_neg1 = b.ins().icmp_imm(IntCC::Equal, y.v, -1);
        b.ins().brif(is_neg1, neg1, &[], divb, &[]);

        // divisor -1: `x / -1` = wrapping negate (INT_MIN/-1 → INT_MIN); `x % -1` = 0.
        b.switch_to_block(neg1);
        b.seal_block(neg1);
        let r1 = if is_rem { b.ins().iconst(I32, 0) } else { b.ins().ineg(x.v) };
        b.ins().jump(merge, &[r1.into()]);

        // safe native path (divisor ∉ {0, -1}).
        b.switch_to_block(divb);
        b.seal_block(divb);
        let r2 = if is_rem { b.ins().srem(x.v, y.v) } else { b.ins().sdiv(x.v, y.v) };
        b.ins().jump(merge, &[r2.into()]);

        b.switch_to_block(merge);
        b.seal_block(merge);
        let v = b.block_params(merge)[0];
        push(stack, v, JT::Int);
        return Some(());
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let lhs = b.ins().sextend(I64, x.v);
        let rhs = b.ins().sextend(I64, y.v);
        let rem = b.ins().iconst(I64, i64::from(is_rem));
        let call = b.ins().call(env.i32_divrem, &[lhs, rhs, rem]);
        let res = b.inst_results(call)[0];
        let v = b.ins().ireduce(I32, res);
        push(stack, v, JT::Int);
        Some(())
    }
}

pub(crate) fn long_divrem(b: &mut FunctionBuilder, stack: &mut Vec<SV>, env: &Env, is_rem: bool) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    // CONSTANT DIVISOR ⇒ NO CALLBACK. This callback exists because Cranelift's raw
    // `sdiv`/`srem` TRAP on `/ by zero` and on `MIN / -1`, while Java needs the first
    // as a pending ArithmeticException and wrapping for the second. Neither can happen
    // when the divisor is a compile-time constant that is neither 0 nor -1 — so emit
    // the instruction and skip the host crossing entirely.
    //
    // Measured worth: `rava_i64_divrem` fires 1,545,513 times on a default boot and
    // 9,508,179 with RAVA_ARENA_LAMBDA, and the caller is `Climate$RTree
    // .lambda$comparator$3` dividing by the CONSTANT 2 — the entire +8.5 s that flag
    // costs in the serial prefix is this missing fold (ledger row 48).
    if let Some(k) = const_i64_of(b, y.v) {
        if k != 0 && k != -1 {
            let res = if is_rem { b.ins().srem_imm(x.v, k) } else { b.ins().sdiv_imm(x.v, k) };
            push(stack, res, JT::Long);
            return Some(());
        }
    }
    let rem = b.ins().iconst(I64, i64::from(is_rem));
    let call = b.ins().call(env.i64_divrem, &[x.v, y.v, rem]);
    let res = b.inst_results(call)[0];
    push(stack, res, JT::Long);
    Some(())
}

/// JVM frem/drem via the floating-remainder libcall.
pub(crate) fn float_rem(b: &mut FunctionBuilder, stack: &mut Vec<SV>, env: &Env, dbl: bool) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    let fref = if dbl { env.f64_rem } else { env.f32_rem };
    let call = b.ins().call(fref, &[x.v, y.v]);
    let res = b.inst_results(call)[0];
    push(stack, res, if dbl { JT::Double } else { JT::Float });
    Some(())
}

/// long shift: the count is an int; widen to i64 for the shift amount.
pub(crate) fn long_shift(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    f: impl FnOnce(&mut FunctionBuilder, Value, Value) -> Value,
) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    let yc = b.ins().sextend(I64, y.v);
    let v = f(b, x.v, yc);
    push(stack, v, JT::Long);
    Some(())
}

/// lcmp/fcmp/dcmp: push -1/0/1. For lcmp (integer) no NaN. For float/double the
/// `g` flag selects fcmpg (NaN -> 1) vs fcmpl (NaN -> -1), matching JVMS.
pub(crate) fn cmp3_branch_sign(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    double: bool,
    less: bool,
) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    let ordered = b.ins().fcmp(
        if less { FloatCC::LessThan } else { FloatCC::GreaterThan },
        x.v,
        y.v,
    );
    let zero = b.ins().iconst(I32, 0);
    let signed = b.ins().iconst(I32, if less { -1 } else { 1 });
    let result = b.ins().select(ordered, signed, zero);
    debug_assert_eq!(x.t, if double { JT::Double } else { JT::Float });
    debug_assert_eq!(y.t, x.t);
    push(stack, result, JT::Int);
    Some(())
}

pub(crate) fn cmp3(b: &mut FunctionBuilder, stack: &mut Vec<SV>, t: JT, g: bool) -> Option<()> {
    let y = stack.pop()?;
    let x = stack.pop()?;
    let one = b.ins().iconst(I32, 1);
    let neg1 = b.ins().iconst(I32, -1);
    let zero = b.ins().iconst(I32, 0);
    let r = match t {
        JT::Long => {
            let gt = b.ins().icmp(IntCC::SignedGreaterThan, x.v, y.v);
            let lt = b.ins().icmp(IntCC::SignedLessThan, x.v, y.v);
            let base = b.ins().select(lt, neg1, zero);
            b.ins().select(gt, one, base)
        }
        _ => {
            // Ordered compares are false for NaN; Unordered is true iff NaN.
            let gt = b.ins().fcmp(FloatCC::GreaterThan, x.v, y.v);
            let lt = b.ins().fcmp(FloatCC::LessThan, x.v, y.v);
            let uno = b.ins().fcmp(FloatCC::Unordered, x.v, y.v);
            let base = b.ins().select(lt, neg1, zero);
            let base = b.ins().select(gt, one, base);
            let nanval = if g { one } else { neg1 };
            b.ins().select(uno, nanval, base)
        }
    };
    push(stack, r, JT::Int);
    Some(())
}
