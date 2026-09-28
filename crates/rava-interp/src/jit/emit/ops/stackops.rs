//! Pure operand-stack shuffles: dup, swap, pop, and iinc.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn stackops(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let vars = o.vars;
    let bc = o.bc;
    let _ = op;
    match op {
        0x57 => {
            stack.pop()?;
        }
        0x58 => {
            let a = stack.pop()?;
            if a.t != JT::Long && a.t != JT::Double {
                stack.pop()?;
            }
        }
        0x59 => {
            let a = *stack.last()?;
            stack.push(a);
        }
        0x5a => {
            // dup_x1: [..., v2, v1] -> [..., v1, v2, v1]. JVMS: both category-1
            // (no long/double). The duplicated entry shares v1's SSA value (values
            // are immutable, so using it twice is correct). Unlocks the common
            // "cache-and-return" idiom (e.g. ChunkProviderServer$1.get caches the
            // looked-up chunk into lastChunkByPos AND returns it — 21.8M calls/run).
            let n = stack.len();
            if n < 2 {
                return None;
            }
            let v1 = stack[n - 1];
            let v2 = stack[n - 2];
            if matches!(v1.t, JT::Long | JT::Double) || matches!(v2.t, JT::Long | JT::Double) {
                return None;
            }
            stack.insert(n - 2, v1);
        }
        0x5c => {
            let n = stack.len();
            let a = *stack.last()?;
            if a.t == JT::Long || a.t == JT::Double {
                stack.push(a);
            } else {
                if n < 2 {
                    return None;
                }
                let btm = stack[n - 2];
                stack.push(btm);
                stack.push(a);
            }
        }
        0x5f => {
            let n = stack.len();
            if n < 2 {
                return None;
            }
            stack.swap(n - 1, n - 2);
        }

        0x84 => {
            // iinc
            let idx = bc[pc + 1] as usize;
            let inc = bc[pc + 2] as i8 as i64;
            let cur = b.use_var(vars[idx]);
            let cur32 = b.ins().ireduce(I32, cur);
            // ⭐**THE INCREMENT WAS AN SSA VALUE; IT IS AN IMMEDIATE FIELD.** `inc` is a signed
            // BYTE of the instruction stream (−128..127), so it always fits `iadd_imm`'s
            // immediate and the emitted add is the same `add r, imm8`. `iinc` is the loop-counter
            // opcode, so this is one fewer value per LOOP BODY, not merely per method.
            //
            // ⛔**AND THE `ireduce`/`sextend` PAIR AROUND IT STAYS, DELIBERATELY.** Folding the
            // add onto the i64 cell directly would be one value cheaper still and WRONG: the
            // cell holds a sign-extended i32, and JVMS `iinc` wraps at 32 bits. `i32::MAX + 1`
            // must become `sextend(i32::MIN)` = 0xFFFF_FFFF_8000_0000; an i64 add produces
            // 0x0000_0000_8000_0000, and every later `ireduce` would agree while every `icmp` on
            // the cell would not.
            let sum = if callseq_lean() {
                b.ins().iadd_imm(cur32, inc)
            } else {
                let c = b.ins().iconst(I32, inc);
                b.ins().iadd(cur32, c)
            };
            let cell = b.ins().sextend(I64, sum);
            b.def_var(vars[idx], cell);
        }
        _ => return None,
    }
    Some(())
}
