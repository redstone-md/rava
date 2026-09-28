//! Object shape: new, checkcast, instanceof, and array creation.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn objects(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let vars = o.vars;
    let bc = o.bc;
    let env = o.env;
    let locals_ptr = o.locals_ptr;
    #[cfg(feature = "next-gen-gc")]
    let cursor_var = o.cursor_var;
    let _ = op;
    match op {
        0xc0 => {
            let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
            let value = stack.last()?.v;
            let cls = b.ins().iconst(I64, env.class_ptr);
            let cpv = b.ins().iconst(I64, idx as i64);
            // Keep the operand on the rooted stack; rejection allocates an exception.
            spill_for_gc(b, vars, stack, locals_ptr);
            let call = b.ins().call(env.checkcast, &[cls, cpv, value]);
            let accepted = b.inst_results(call)[0];
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
            let success = b.create_block();
            let failure = b.create_block();
            b.ins().brif(accepted, success, &[], failure, &[]);
            b.switch_to_block(failure);
            b.seal_block(failure);
            let zero = b.ins().iconst(I64, 0);
            #[cfg(feature = "next-gen-gc")]
            let zero = emit_deep_epilogue(b, env, o.dv, zero);
            let zero = abi_ret_from_cell(b, zero);
            b.ins().return_(&[zero]);
            b.switch_to_block(success);
            b.seal_block(success);
        }

        // instanceof — hierarchy walk via callback; pushes 0/1.
        0xc1 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            // INLINE TYPE TEST (`RAVA_NO_EMIT_TYPETEST=1` disables, and then this whole block
            // emits nothing so the codegen below is byte-identical). Probes this site's 4-entry
            // (class key → answer) cache in machine code and reaches the callback only on a
            // miss, which then fills the entry. The callback was the ONE AND ONLY 0xc1 fallback:
            // every compiled `instanceof` crossed into Rust for an answer the host already had
            // memoised. See `ops::typecheck` for what the key proves and what it refuses.
            #[cfg(feature = "next-gen-gc")]
            if super::typecheck::emit_instanceof(
                b,
                env,
                stack,
                vars,
                locals_ptr,
                o.array_desc_cell,
                idx,
            )
            .is_some()
            {
                return Some(());
            }
            let r = stack.pop()?;
            let cls = b.ins().iconst(I64, env.class_ptr);
            let cpv = b.ins().iconst(I64, idx as i64);
            spill_for_gc(b, vars, stack, locals_ptr);
            let call = b.ins().call(env.instanceof, &[cls, cpv, r.v]);
            let res = b.inst_results(call)[0];
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
            let v = b.ins().ireduce(I32, res);
            push(stack, v, JT::Int);
        }

        // new — INLINE TLAB bump for an arena-eligible class (zero callback); every
        // other class allocates via the real interpreter handler (callback).
        0xbb => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            // Arena-eligible + compiled cursor available → inline bump, no callback.
            #[cfg(feature = "next-gen-gc")]
            if let (Some(cv), Some(&(class_id, nfields))) =
                (cursor_var, env.new_arena.get(&idx))
            {
                emit_inline_arena_new(
                    b,
                    vars,
                    stack,
                    cv,
                    class_id,
                    nfields,
                    env.arena_safepoint_fn,
                    env.arena_deopt_alloc_fn,
                    locals_ptr,
                );
                return Some(());
            }
            // LATE-BOUND INLINE BUMP (`RAVA_NO_INLINE_NEW=1` disables). The site was not
            // arena-eligible AT EMIT TIME — and `build_new_arena`'s predicate has exactly one
            // non-permanent term, `initialized`. A method compiles at its threshold, routinely
            // before a cold `new` inside it has run its target's `<clinit>`, and the site was
            // then stuck on the callback for the rest of the process because nothing re-asked.
            // So bake only the ADDRESS of a per-site shape word: `rava_new` publishes the shape
            // it OBSERVES on the first execution, and the emitted guard reads it on every one
            // after. `mi_malloc_aligned` plus the four `new` callback rows are the top of the
            // compiled-code profile; this is the arm that takes them off it.
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled()
                && (cursor_var.is_some() || inline_new_site_cursor_enabled())
            {
                if let Some(slot) = new_site_slot(env.class_ptr as usize, idx) {
                    if emit_inline_arena_new_late(
                        b, env, vars, stack, cursor_var, slot, idx, locals_ptr,
                    )
                    .is_some()
                    {
                        return Some(());
                    }
                }
            }
            let cls = b.ins().iconst(I64, env.class_ptr);
            let cpv = b.ins().iconst(I64, idx as i64);
            spill_for_gc(b, vars, stack, locals_ptr);
            let call = b.ins().call(env.new_obj, &[cls, cpv]);
            let res = b.inst_results(call)[0];
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
            push(stack, res, JT::Ref);
        }

        // newarray (primitive) — atype operand, pop count, push fresh array ref.
        0xbc => {
            let atype_raw = bc[pc + 1];
            let atype = b.ins().iconst(I64, atype_raw as i64);
            let count = stack.pop()?;
            let count = b.ins().sextend(I64, count.v);
            // The structural plan proves this exact site/atype remains method-confined.
            // The shared consumer gate is compile-time only: when it is off, UNSET emits
            // the historical callback path below with no extra runtime branch.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::arena_primitive_arrays_enabled()
                && env.prim_arrays.contains(pc, atype_raw)
            {
                if let Some(cursor) = cursor_var {
                    emit_inline_primitive_newarray(
                        b,
                        env,
                        vars,
                        stack,
                        cursor,
                        locals_ptr,
                        atype_raw,
                        atype,
                        count,
                    )?;
                    return Some(());
                }
            }
            // rava_newarray ALLOCATES → can trigger a moving collection. Spill live
            // roots before and reload after (as 0xbb/0xbd do) — otherwise an arena
            // pointer held in a register/stack slot across this call (e.g. an
            // Identifier receiver live while a JSON parse builds a primitive array
            // during recipe reload) is left pointing at the pre-move location and the
            // next getfield panics with a stale `class None` receiver (Wall #19).
            spill_for_gc(b, vars, stack, locals_ptr);
            let call = b.ins().call(env.newarray, &[atype, count]);
            let res = b.inst_results(call)[0];
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
            push(stack, res, JT::Ref);
        }

        // anewarray (reference) — class cp operand, pop count, push fresh array ref.
        0xbd => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let count = stack.pop()?;
            let count = b.ins().sextend(I64, count.v);
            #[cfg(feature = "next-gen-gc")]
            if let (Some(cursor), Some(&class_id)) = (cursor_var, env.ref_arrays.get(&pc)) {
                emit_inline_reference_anewarray(
                    b,
                    env,
                    vars,
                    stack,
                    cursor,
                    locals_ptr,
                    idx,
                    class_id,
                    count,
                )?;
                return Some(());
            }
            let cls = b.ins().iconst(I64, env.class_ptr);
            let cpv = b.ins().iconst(I64, idx as i64);
            spill_for_gc(b, vars, stack, locals_ptr);
            let call = b.ins().call(env.anewarray, &[cls, cpv, count]);
            let res = b.inst_results(call)[0];
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
            push(stack, res, JT::Ref);
        }

        // Primitive array loads — NATIVE: inline bounds-checked indexed load, no
        // per-element callback. iaload/baload/caload/saload → int; laload → long;
        // faload → float; daload → double. Element width/extension from the opcode.
        _ => return None,
    }
    Some(())
}
