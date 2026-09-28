//! Constants, loads and stores: the opcodes that only move a value between a local slot and the operand stack.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn consts(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let vars = o.vars;
    let bc = o.bc;
    let pool = o.pool;
    let env = o.env;
    let locals_ptr = o.locals_ptr;
    #[cfg(feature = "next-gen-gc")]
    let (arena_proof, licm) = (o.arena_proof, o.licm);
    match op {
        0x00 => {}
        0x01 => {
            let v = b.ins().iconst(I64, 0);
            push(stack, v, JT::Ref);
        } // aconst_null

        // aload / aload_0..3 (reference local) + astore / astore_0..3
        0x19 => {
            #[allow(unused_mut)]
            let mut sv = load_local(b, vars, bc[pc + 1] as usize, JT::Ref);
            #[cfg(feature = "next-gen-gc")]
            {
                let local = bc[pc + 1] as usize;
                sv.src = licm.and_then(|l| l.alias.get(local).copied().flatten());
                sv.arena = arena_proof.get(local).copied().unwrap_or(false);
            }
            stack.push(sv);
        }
        0x2a..=0x2d => {
            #[allow(unused_mut)]
            let mut sv = load_local(b, vars, (op - 0x2a) as usize, JT::Ref);
            #[cfg(feature = "next-gen-gc")]
            {
                let local = (op - 0x2a) as usize;
                sv.src = licm.and_then(|l| l.alias.get(local).copied().flatten());
                sv.arena = arena_proof.get(local).copied().unwrap_or(false);
            }
            stack.push(sv);
        }
        0x3a => {
            let sv = stack.pop()?;
            store_local(b, vars, bc[pc + 1] as usize, sv);
        }
        0x4b..=0x4e => {
            let sv = stack.pop()?;
            store_local(b, vars, (op - 0x4b) as usize, sv);
        }

        // getfield — primitive fields read natively (scan the flat descriptor);
        // reference fields stay on the read-only callback (box/null decode).
        0x12 => {
            let idx = bc[pc + 1] as u16;
            emit_ldc_ref(b, stack, pool, env, vars, locals_ptr, idx)?;
        }
        0x13 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            emit_ldc_ref(b, stack, pool, env, vars, locals_ptr, idx)?;
        }
        // ldc2_w — always long/double.
        0x14 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            ldc_const(b, stack, pool, idx)?;
        }
        0x02..=0x08 => {
            let v = b.ins().iconst(I32, (op as i64) - 0x03);
            push(stack, v, JT::Int);
        }
        0x09 | 0x0a => {
            let v = b.ins().iconst(I64, (op as i64) - 0x09);
            push(stack, v, JT::Long);
        }
        0x0b..=0x0d => {
            let v = b.ins().f32const((op - 0x0b) as f32);
            push(stack, v, JT::Float);
        }
        0x0e | 0x0f => {
            let v = b.ins().f64const((op - 0x0e) as f64);
            push(stack, v, JT::Double);
        }
        0x10 => {
            let v = b.ins().iconst(I32, bc[pc + 1] as i8 as i64);
            push(stack, v, JT::Int);
        }
        0x11 => {
            let v = b.ins().iconst(I32, s16(bc, pc + 1) as i64);
            push(stack, v, JT::Int);
        }

        0x15 => {
            let sv = load_local(b, vars, bc[pc + 1] as usize, JT::Int);
            stack.push(sv);
        }
        0x16 => {
            let sv = load_local(b, vars, bc[pc + 1] as usize, JT::Long);
            stack.push(sv);
        }
        0x17 => {
            let sv = load_local(b, vars, bc[pc + 1] as usize, JT::Float);
            stack.push(sv);
        }
        0x18 => {
            let sv = load_local(b, vars, bc[pc + 1] as usize, JT::Double);
            stack.push(sv);
        }
        0x1a..=0x1d => {
            let sv = load_local(b, vars, (op - 0x1a) as usize, JT::Int);
            stack.push(sv);
        }
        0x1e..=0x21 => {
            let sv = load_local(b, vars, (op - 0x1e) as usize, JT::Long);
            stack.push(sv);
        }
        0x22..=0x25 => {
            let sv = load_local(b, vars, (op - 0x22) as usize, JT::Float);
            stack.push(sv);
        }
        0x26..=0x29 => {
            let sv = load_local(b, vars, (op - 0x26) as usize, JT::Double);
            stack.push(sv);
        }

        0x36..=0x39 => {
            let sv = stack.pop()?;
            store_local(b, vars, bc[pc + 1] as usize, sv);
        }
        0x3b..=0x3e => {
            let sv = stack.pop()?;
            store_local(b, vars, (op - 0x3b) as usize, sv);
        }
        0x3f..=0x42 => {
            let sv = stack.pop()?;
            store_local(b, vars, (op - 0x3f) as usize, sv);
        }
        0x43..=0x46 => {
            let sv = stack.pop()?;
            store_local(b, vars, (op - 0x43) as usize, sv);
        }
        0x47..=0x4a => {
            let sv = stack.pop()?;
            store_local(b, vars, (op - 0x47) as usize, sv);
        }

        // integer arithmetic / logic
        _ => return None,
    }
    Some(())
}
