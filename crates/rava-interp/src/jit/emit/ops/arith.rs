//! Arithmetic, conversions and the three-way compares.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn arith(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let bc = o.bc;
    let blocks = o.blocks;
    let env = o.env;
    match op {
        0x60 => bin(b, stack, JT::Int, |b, x, y| b.ins().iadd(x, y))?,
        0x64 => bin(b, stack, JT::Int, |b, x, y| b.ins().isub(x, y))?,
        0x68 => bin(b, stack, JT::Int, |b, x, y| b.ins().imul(x, y))?,
        0x6c | 0x70 => {
            let is_rem = op == 0x70;
            // Any NON-ZERO compile-time-constant divisor takes the guard-free
            // constant path: a constant divisor can never be 0 (no /0 exception
            // possible) and, when != -1, can never hit the INT_MIN/-1 overflow —
            // so the runtime divisor==0 branch + callback the general int_divrem
            // emits is DEAD WORK. Power-of-2 → shift; other constants → a bare
            // srem/sdiv (no guard). This removes a per-iteration branch+callback
            // from tight loops like `i % 7` (C2 emits no guard for a constant).
            if let Some(divisor) =
                adjacent_i32_divisor(bc, pc, blocks).filter(|&d| d != 0)
            {
                let nonnegative =
                    positive_div_enabled() && guarded_nonnegative_i32_dividend(bc, pc);
                int_divrem_constant(b, stack, is_rem, divisor, nonnegative)?;
            } else {
                int_divrem(b, stack, env, is_rem)?;
            }
        }
        0x74 => {
            let a = stack.pop()?;
            let v = b.ins().ineg(a.v);
            push(stack, v, JT::Int);
        }
        0x78 => bin(b, stack, JT::Int, |b, x, y| b.ins().ishl(x, y))?,
        0x7a => bin(b, stack, JT::Int, |b, x, y| b.ins().sshr(x, y))?,
        0x7c => bin(b, stack, JT::Int, |b, x, y| b.ins().ushr(x, y))?,
        0x7e => bin(b, stack, JT::Int, |b, x, y| b.ins().band(x, y))?,
        0x80 => bin(b, stack, JT::Int, |b, x, y| b.ins().bor(x, y))?,
        0x82 => bin(b, stack, JT::Int, |b, x, y| b.ins().bxor(x, y))?,

        // long arithmetic / logic
        0x61 => bin(b, stack, JT::Long, |b, x, y| b.ins().iadd(x, y))?,
        0x65 => bin(b, stack, JT::Long, |b, x, y| b.ins().isub(x, y))?,
        0x69 => bin(b, stack, JT::Long, |b, x, y| b.ins().imul(x, y))?,
        0x6d => long_divrem(b, stack, env, false)?,
        0x71 => long_divrem(b, stack, env, true)?,
        0x75 => {
            let a = stack.pop()?;
            let v = b.ins().ineg(a.v);
            push(stack, v, JT::Long);
        }
        0x79 => long_shift(b, stack, |b, x, y| b.ins().ishl(x, y))?,
        0x7b => long_shift(b, stack, |b, x, y| b.ins().sshr(x, y))?,
        0x7d => long_shift(b, stack, |b, x, y| b.ins().ushr(x, y))?,
        0x7f => bin(b, stack, JT::Long, |b, x, y| b.ins().band(x, y))?,
        0x81 => bin(b, stack, JT::Long, |b, x, y| b.ins().bor(x, y))?,
        0x83 => bin(b, stack, JT::Long, |b, x, y| b.ins().bxor(x, y))?,

        // float arithmetic (div is IEEE, non-trapping → supported)
        0x62 => bin(b, stack, JT::Float, |b, x, y| b.ins().fadd(x, y))?,
        0x66 => bin(b, stack, JT::Float, |b, x, y| b.ins().fsub(x, y))?,
        0x6a => bin(b, stack, JT::Float, |b, x, y| b.ins().fmul(x, y))?,
        0x6e => bin(b, stack, JT::Float, |b, x, y| b.ins().fdiv(x, y))?,
        0x72 => float_rem(b, stack, env, false)?,
        0x76 => {
            let a = stack.pop()?;
            let v = b.ins().fneg(a.v);
            push(stack, v, JT::Float);
        }

        // double arithmetic
        0x63 => bin(b, stack, JT::Double, |b, x, y| b.ins().fadd(x, y))?,
        0x67 => bin(b, stack, JT::Double, |b, x, y| b.ins().fsub(x, y))?,
        0x6b => bin(b, stack, JT::Double, |b, x, y| b.ins().fmul(x, y))?,
        0x6f => bin(b, stack, JT::Double, |b, x, y| b.ins().fdiv(x, y))?,
        0x73 => float_rem(b, stack, env, true)?,
        0x77 => {
            let a = stack.pop()?;
            let v = b.ins().fneg(a.v);
            push(stack, v, JT::Double);
        }

        // conversions
        0x85 => {
            let a = stack.pop()?;
            let v = b.ins().sextend(I64, a.v);
            push(stack, v, JT::Long);
        }
        0x86 => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_from_sint(F32, a.v);
            push(stack, v, JT::Float);
        }
        0x87 => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_from_sint(F64, a.v);
            push(stack, v, JT::Double);
        }
        0x88 => {
            let a = stack.pop()?;
            let v = b.ins().ireduce(I32, a.v);
            push(stack, v, JT::Int);
        }
        0x89 => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_from_sint(F32, a.v);
            push(stack, v, JT::Float);
        }
        0x8a => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_from_sint(F64, a.v);
            push(stack, v, JT::Double);
        }
        0x8b => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_to_sint_sat(I32, a.v);
            push(stack, v, JT::Int);
        }
        0x8c => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_to_sint_sat(I64, a.v);
            push(stack, v, JT::Long);
        }
        0x8d => {
            let a = stack.pop()?;
            let v = b.ins().fpromote(F64, a.v);
            push(stack, v, JT::Double);
        }
        0x8e => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_to_sint_sat(I32, a.v);
            push(stack, v, JT::Int);
        }
        0x8f => {
            let a = stack.pop()?;
            let v = b.ins().fcvt_to_sint_sat(I64, a.v);
            push(stack, v, JT::Long);
        }
        0x90 => {
            let a = stack.pop()?;
            let v = b.ins().fdemote(F32, a.v);
            push(stack, v, JT::Float);
        }
        0x91 => {
            let a = stack.pop()?;
            let t = b.ins().ireduce(types::I8, a.v);
            let v = b.ins().sextend(I32, t);
            push(stack, v, JT::Int);
        }
        0x92 => {
            let a = stack.pop()?;
            let t = b.ins().ireduce(types::I16, a.v);
            let v = b.ins().uextend(I32, t);
            push(stack, v, JT::Int);
        }
        0x93 => {
            let a = stack.pop()?;
            let t = b.ins().ireduce(types::I16, a.v);
            let v = b.ins().sextend(I32, t);
            push(stack, v, JT::Int);
        }

        0x94 => cmp3(b, stack, JT::Long, false)?,
        0x95..=0x98 => {
            let branch_pc = pc + 1;
            let branch = bc.get(branch_pc).copied();
            // A compare immediately consumed by its matching sign branch does
            // not need the materialized -1/0/+1 value. If the branch is not an
            // alternate CFG entry, emit one ordered compare whose selected
            // sign preserves exactly that next branch, including JVMS NaN.
            let fused = !blocks.contains_key(&branch_pc)
                && match (op, branch) {
                    (0x96 | 0x98, Some(0x9b | 0x9c)) => {
                        cmp3_branch_sign(b, stack, matches!(op, 0x98), true)?;
                        true
                    }
                    (0x95 | 0x97, Some(0x9d | 0x9e)) => {
                        cmp3_branch_sign(b, stack, matches!(op, 0x97), false)?;
                        true
                    }
                    _ => false,
                };
            if !fused {
                let t = if matches!(op, 0x95 | 0x96) { JT::Float } else { JT::Double };
                cmp3(b, stack, t, matches!(op, 0x96 | 0x98))?;
            }
        }

        // stack manipulation (trickier dup_x* / dup2* bail)
        _ => return None,
    }
    Some(())
}
