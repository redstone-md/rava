//! Branches, switches, gotos and returns.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

fn poll_backedge(b: &mut FunctionBuilder, env: &Env<'_>, pc: usize, target: usize) {
    if env.poll_backedges && target <= pc {
        let safepoint = env.safepoint.expect("enabled JIT poll imports its callback");
        b.ins().call(safepoint, &[]);
    }
}

pub(crate) fn branch(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let block_shapes = &mut *o.block_shapes;
    let cur_open = &mut *o.cur_open;
    let bc = o.bc;
    let blocks = o.blocks;
    let env = o.env;
    #[cfg(feature = "next-gen-gc")]
    let (licm, dv) = (o.licm, o.dv);
    match op {
        0x99..=0x9e => {
            let a = stack.pop()?;
            let cc = match op {
                0x99 => IntCC::Equal,
                0x9a => IntCC::NotEqual,
                0x9b => IntCC::SignedLessThan,
                0x9c => IntCC::SignedGreaterThanOrEqual,
                0x9d => IntCC::SignedGreaterThan,
                _ => IntCC::SignedLessThanOrEqual,
            };
            // ⭐**THE COMMONEST BRANCH IN JAVA BYTECODE CARRIED ITS ZERO AS A SEPARATE SSA
            // VALUE.** `icmp_imm` puts the constant in the instruction, which is what the x86
            // lowering wants anyway (`cmp r, 0`), so the emitted comparison is the same and the
            // `iconst` simply never exists — one less value for the egraph to hash, one less
            // node for `regalloc2`'s interference walk, per conditional branch, per body, on
            // the serial compile path.
            //
            // THE `JT::Int` TEST IS WHAT MAKES THIS SAFE, and it only ever REJECTS. `icmp`
            // demands both operands share a type, so today a stack top that is not an `i32`
            // produces IR the Cranelift verifier rejects and `define_function` refuses the
            // method. `icmp_imm` has no such constraint — it would adapt to whatever width the
            // operand happens to be and COMPILE a method that is refused today. The JVM verifier
            // forbids `if<cond>` on anything but an int, so this arm changes nothing for real
            // class files; for anything else the historic `iconst`+`icmp` is emitted and
            // rejected exactly as before. `SV::t` proves the Cranelift type because every push
            // types its value with `jt_ty` (`JT::Int` ⇒ `I32`).
            let lean = callseq_lean() && a.t == JT::Int;
            let cond = if lean {
                b.ins().icmp_imm(cc, a.v, 0)
            } else {
                let zero = b.ins().iconst(I32, 0);
                b.ins().icmp(cc, a.v, zero)
            };
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            poll_backedge(b, env, pc, target);
            brif_to(b, blocks, block_shapes, cond, target, pc + 3, stack)?;
            *cur_open = false;
        }
        0x9f..=0xa4 => {
            let y = stack.pop()?;
            let x = stack.pop()?;
            let cc = match op {
                0x9f => IntCC::Equal,
                0xa0 => IntCC::NotEqual,
                0xa1 => IntCC::SignedLessThan,
                0xa2 => IntCC::SignedGreaterThanOrEqual,
                0xa3 => IntCC::SignedGreaterThan,
                _ => IntCC::SignedLessThanOrEqual,
            };
            let cond = b.ins().icmp(cc, x.v, y.v);
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            poll_backedge(b, env, pc, target);
            brif_to(b, blocks, block_shapes, cond, target, pc + 3, stack)?;
            *cur_open = false;
        }
        0xa5 | 0xa6 => {
            // if_acmpeq / if_acmpne — reference identity, INLINE for the common case.
            // Ref/Array/null cells compare by raw i64 equality; only BOX_TAG cells
            // (Str / boxed primitive, where ref_equal does content/value equality)
            // need the callback. So: native compare, and fall to rava_acmp only when
            // the bits differ AND an operand is box-tagged.
            let y = stack.pop()?;
            let x = stack.pop()?;
            // FOUR CONSTANTS FOR ONE TEST. The box-tag classify below built a shift amount and a
            // tag constant as SSA values and fed them to two-operand `ushr`/`icmp` — and the
            // callback arm then built two more zeros for its own comparisons. All four are
            // immediate operands of the instruction that consumes them, so the `*_imm` forms
            // emit the same comparison with the value never created. `BOX_TAG` is `1 << 41`, so
            // the folded immediate is 512 — nowhere near an encoding edge.
            //
            // Guarded on BOTH operands being `JT::Ref`, for the same reason (and with the same
            // reject-only effect) as the `if<cond>` arm above: `ushr`'s amount and `icmp`'s
            // operands are type-checked today, `*_imm` is not, so a mistyped stack must keep the
            // historic sequence and stay refused. `if_acmp*` on anything but two references is
            // unverifiable bytecode.
            let lean = callseq_lean() && x.t == JT::Ref && y.t == JT::Ref;
            let eq_fast = b.ins().icmp(IntCC::Equal, x.v, y.v);
            let neq = b.ins().icmp(IntCC::NotEqual, x.v, y.v);
            // A reference cell is box-tagged iff its high 32 bits == BOX_TAG >> 32.
            let box_hi_imm = (BOX_TAG as u64 >> 32) as i64;
            // The cold arm materialises BOTH constants first, in their historic order, so
            // `RAVA_NO_CALLSEQ=1` reproduces the previous instruction sequence exactly.
            let consts = if lean {
                None
            } else {
                let shift = b.ins().iconst(I64, 32);
                let box_hi = b.ins().iconst(I64, box_hi_imm);
                Some((shift, box_hi))
            };
            let xhi = match consts {
                None => b.ins().ushr_imm(x.v, 32),
                Some((shift, _)) => b.ins().ushr(x.v, shift),
            };
            let yhi = match consts {
                None => b.ins().ushr_imm(y.v, 32),
                Some((shift, _)) => b.ins().ushr(y.v, shift),
            };
            let xbox = match consts {
                None => b.ins().icmp_imm(IntCC::Equal, xhi, box_hi_imm),
                Some((_, box_hi)) => b.ins().icmp(IntCC::Equal, xhi, box_hi),
            };
            let ybox = match consts {
                None => b.ins().icmp_imm(IntCC::Equal, yhi, box_hi_imm),
                Some((_, box_hi)) => b.ins().icmp(IntCC::Equal, yhi, box_hi),
            };
            let either_box = b.ins().bor(xbox, ybox);
            let need_cb = b.ins().band(either_box, neq);
            // Diamond: skip the callback unless need_cb. merge carries the I8 result.
            let cb_block = b.create_block();
            let merge = b.create_block();
            b.append_block_param(merge, types::I8);
            b.ins()
                .brif(need_cb, cb_block, &[], merge, &[eq_fast.into()]);
            b.switch_to_block(cb_block);
            b.seal_block(cb_block);
            let call = b.ins().call(env.acmp, &[x.v, y.v]);
            let cb = b.inst_results(call)[0];
            // `cb` is the callback's `i64` result and `eq_b` (below) is the merge block's `I8`
            // parameter — both types are fixed by this emitter, not by the operand stack, so
            // these two folds need no shape test at all.
            let cb_b = if lean {
                b.ins().icmp_imm(IntCC::NotEqual, cb, 0)
            } else {
                let zero = b.ins().iconst(I64, 0);
                b.ins().icmp(IntCC::NotEqual, cb, zero)
            };
            b.ins().jump(merge, &[cb_b.into()]);
            b.switch_to_block(merge);
            b.seal_block(merge);
            let eq_b = b.block_params(merge)[0];
            // acmpeq takes the branch when equal; acmpne when not.
            let cond = if op == 0xa5 {
                eq_b
            } else if lean {
                b.ins().icmp_imm(IntCC::Equal, eq_b, 0)
            } else {
                let z = b.ins().iconst(types::I8, 0);
                b.ins().icmp(IntCC::Equal, eq_b, z)
            };
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            poll_backedge(b, env, pc, target);
            brif_to(b, blocks, block_shapes, cond, target, pc + 3, stack)?;
            *cur_open = false;
        }
        0xa7 => {
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            poll_backedge(b, env, pc, target);
            jump_to(b, blocks, block_shapes, target, stack)?;
            *cur_open = false;
        }
        // tableswitch / lookupswitch — pop the int index, branch via Cranelift's
        // `Switch` (jump table / binary search, O(1)/O(log n) — NOT an icmp chain,
        // which previously regressed boot). Only supported when the index is the
        // SOLE operand: `Switch` can't pass block args, so target blocks must start
        // with an empty operand stack (the common shape: switch on a local/field).
        0xaa | 0xab => {
            let sv = stack.pop()?;
            if sv.t != JT::Int {
                return None; // switch index must be an int
            }
            let idx = sv.v; // already I32 (the operand stack holds ints as i32)
            let (default_t, cases) = switch_targets(bc, pc)?;
            if env.poll_backedges
                && (default_t <= pc || cases.iter().any(|(_, target)| *target <= pc))
            {
                let safepoint = env.safepoint.expect("enabled JIT poll imports its callback");
                b.ins().call(safepoint, &[]);
            }
            if stack.is_empty() {
                // Common shape (switch on a local/field, empty residual stack):
                // Cranelift `Switch` = O(1) jump table / O(log n). Target blocks
                // start empty so no block args are needed.
                ensure_block_shape(b, blocks, block_shapes, default_t, stack)?;
                for (_, t) in &cases {
                    ensure_block_shape(b, blocks, block_shapes, *t, stack)?;
                }
                let mut sw = cranelift::frontend::Switch::new();
                for (m, t) in &cases {
                    // Unsigned key reinterpretation of the i32 match — `Switch` compares
                    // unsigned, so the runtime i32 value and the key must use the same
                    // 32-bit wraparound (handles negative case labels correctly).
                    sw.set_entry((*m as u32) as u128, blocks[t]);
                }
                sw.emit(b, idx, blocks[&default_t]);
            } else {
                // Residual operand value(s) live UNDER the index — e.g. a ref pushed
                // before the switch and consumed at the post-switch merge (StairBlock
                // .getShape's Map-valued exhaustive switch). Cranelift `Switch` can't
                // pass block args, so lower as an icmp/brif chain that threads the
                // residual stack through each target's block params (the exact
                // mechanism brif_to uses for if_* branches). O(n) compares — taken
                // only for this rare non-empty-residual shape; empty-residual switches
                // keep the O(1) jump table above. Bit-identical for existing methods
                // (they all hit the empty branch); this only unblocks methods that
                // previously bailed to the interpreter.
                let args = stack_args(stack); // typed residual values, shape order
                for (m, t) in &cases {
                    ensure_block_shape(b, blocks, block_shapes, *t, stack)?;
                    // Equality on the raw 32-bit pattern (same wraparound as the
                    // jump-table key) → correct for negative case labels too.
                    let key = b.ins().iconst(I32, (*m as u32) as i64);
                    let hit = b.ins().icmp(IntCC::Equal, idx, key);
                    let next = b.create_block(); // fresh fallthrough (not a leader)
                    b.ins().brif(hit, blocks[t], &args, next, &[]);
                    b.switch_to_block(next);
                    b.seal_block(next); // single predecessor (this brif), already emitted
                }
                // Every non-matching index falls through to the default target.
                ensure_block_shape(b, blocks, block_shapes, default_t, stack)?;
                b.ins().jump(blocks[&default_t], &args);
            }
            *cur_open = false;
        }
        0xc6 | 0xc7 => {
            let a = stack.pop()?;
            let cc = if op == 0xc6 {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            };
            // The null test against the i64 reference cell — same fold, same reject-only shape
            // test as the `if<cond>` arm (`JT::Ref` ⇒ `I64`; anything else keeps the historic
            // `iconst`+`icmp` and stays refused by the verifier).
            //
            // Note the cold arm's `iconst` MOVES from before the `cc` match to inside the else,
            // which is the same instruction stream: `cc` is a Rust-level `IntCC`, not IR, so no
            // instruction was ever emitted between the two.
            let cond = if callseq_lean() && a.t == JT::Ref {
                b.ins().icmp_imm(cc, a.v, 0)
            } else {
                let zero = b.ins().iconst(I64, 0);
                b.ins().icmp(cc, a.v, zero)
            };
            let target = (pc as i32 + s16(bc, pc + 1)) as usize;
            poll_backedge(b, env, pc, target);
            brif_to(b, blocks, block_shapes, cond, target, pc + 3, stack)?;
            *cur_open = false;
        }

        0xac | 0xad | 0xae | 0xaf | 0xb0 => {
            // i/l/f/d/areturn. For areturn the stack top is a JT::Ref whose
            // `to_cell` is the tagged handle / box-index cell verbatim; the
            // caller decodes it via `unbox_ref`.
            // PROMO: write dirty field registers back first (no-op when the
            // method isn't promoted / has no written pair). Inline-callee
            // returns never reach here (emit_inline jumps to `cont`).
            #[cfg(feature = "next-gen-gc")]
            if let Some(l) = licm {
                emit_promo_flush(b, &l);
            }
            let a = stack.pop()?;
            // Register-ABI arc: return the raw primitive matching the register signature.
            // Route the i64 cell through the deep epilogue FIRST (it unregisters the self-reg
            // buffer + trims the box table — a non-leaf register entry needs this; a leaf's dv
            // is None so it is a no-op), then convert the cell back to the raw primitive. The
            // epilogue leaves a primitive cell's bits unchanged exactly as the memory ABI
            // relies on for a double-returning method.
            if cur_reg_abi().is_some() {
                let cell = to_cell(b, a);
                #[cfg(feature = "next-gen-gc")]
                let cell = emit_deep_epilogue(b, env, dv, cell);
                // A reference return IS the cell (post-epilogue, so a BOX-tagged result
                // has already been re-homed onto the caller's box base).
                let rv = abi_ret_from_cell(b, cell);
                b.ins().return_(&[rv]);
            } else {
                let cell = to_cell(b, a);
                #[cfg(feature = "next-gen-gc")]
                let cell = emit_deep_epilogue(b, env, dv, cell);
                b.ins().return_(&[cell]);
            }
            *cur_open = false;
        }
        0xb1 => {
            #[cfg(feature = "next-gen-gc")]
            if let Some(l) = licm {
                emit_promo_flush(b, &l);
            }
            let zero = b.ins().iconst(I64, 0);
            #[cfg(feature = "next-gen-gc")]
            let zero = emit_deep_epilogue(b, env, dv, zero);
            let zero = abi_ret_from_cell(b, zero);
            b.ins().return_(&[zero]);
            *cur_open = false;
        }
        _ => return None,
    }
    Some(())
}
