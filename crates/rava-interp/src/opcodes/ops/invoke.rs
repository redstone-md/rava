//! The five invoke opcodes and the allocation that follows a new.

use super::{guard, throw_exc};
use crate::opcodes::*;

/// `None` = keep interpreting, `Some(exit)` = leave `run`.
#[inline(always)]
pub(crate) fn invoke(
    vm: &mut Vm,
    frame: &mut Frame,
    code: &Code,
    bc: &[u8],
    pc: usize,
    op: u8,
) -> Result<Option<RunExit>, VmError> {
    let _ = (bc, pc, op);
    match op {
        0xb6 => {
            // 11.11 READ-VISIBILITY diag: a null `Holder` receiver into `Holder.value()` on a pool
            // worker. Decode the instruction that PUSHED the null receiver (the op ending at this
            // invoke's pc) to name the source — getstatic (which static field) vs getfield (which
            // field) vs aload (which local). Worker-gated + narrowed to the confirmed culprit ⇒ no
            // flood, no production cost. Names WHERE the worker read null where main reads a Holder.
            #[cfg(feature = "next-gen-gc")]
            if crate::gil::is_worker() && matches!(frame.stack().last(), Some(Value::Null)) {
                if let Some(m) = frame.class.file.pool.member_ref(u16_operand(bc, pc)) {
                    if &*m.owner == "net/minecraft/core/Holder" && &*m.name == "value" {
                        let mut p = 0usize;
                        let mut src = None;
                        while p < pc {
                            let l = match crate::jit::insn_len_at(bc, p) { Some(l) => l, None => break };
                            if p + l == pc { src = Some(p); break; }
                            if p + l > pc { break; }
                            p += l;
                        }
                        let fref = |q: usize| {
                            let cp = ((bc[q + 1] as u16) << 8) | bc[q + 2] as u16;
                            frame.class.file.pool.member_ref(cp)
                                .map(|f| format!("{}.{}", f.owner, f.name))
                                .unwrap_or_else(|| "?".into())
                        };
                        match src {
                            Some(s) => match bc[s] {
                                0xb2 => eprintln!("[RAVA-READ-DIAG] SOURCE: getstatic -> {} is NULL on worker!", fref(s)),
                                0xb4 => eprintln!("[RAVA-READ-DIAG] SOURCE: getfield -> field {} is NULL on worker!", fref(s)),
                                0x19 => eprintln!("[RAVA-READ-DIAG] SOURCE: aload local{} is NULL on worker!", bc[s + 1]),
                                0x2a..=0x2d => eprintln!("[RAVA-READ-DIAG] SOURCE: aload_{} (local) is NULL on worker!", bc[s] - 0x2a),
                                op => eprintln!("[RAVA-READ-DIAG] SOURCE: op 0x{op:02x} @pc{s} pushed NULL Holder on worker!"),
                            },
                            None => eprintln!("[RAVA-READ-DIAG] SOURCE: receiver-push not located before Holder.value @pc{pc}"),
                        }
                    }
                }
            }
            // M3 F3.3: fork a `for (e : coll) { <prefix(e)>; e.m(); }` iterator loop at the
            // iterator() call (replay prefix + apply e.m(), skip the loop). Gated; None →
            // normal virtual dispatch.
            #[cfg(feature = "next-gen-gc")]
            if fork_route_foreach() || fork_target_scan() {
                if let Some(end) = guard!(vm, frame, code, vm.try_fork_iterator_loop(frame, code, pc)) { frame.pc = end; return Ok(None); }
            }
            guard!(vm, frame, code, invokevirtual(vm, frame, u16_operand(bc, pc))); frame.pc += 3;
        } // invokevirtual
        0xb9 => {
            #[cfg(feature = "next-gen-gc")]
            if fork_route_foreach() || fork_target_scan() {
                if let Some(end) = guard!(vm, frame, code, vm.try_fork_iterator_loop(frame, code, pc)) { frame.pc = end; return Ok(None); }
            }
            guard!(vm, frame, code, invokevirtual(vm, frame, u16_operand(bc, pc))); frame.pc += 5;
        } // invokeinterface (index + count + 0)
        0xb7 => { guard!(vm, frame, code, invokespecial(vm, frame, u16_operand(bc, pc))); frame.pc += 3; } // invokespecial
        0xb8 => { guard!(vm, frame, code, invokestatic(vm, frame, u16_operand(bc, pc))); frame.pc += 3; } // invokestatic
        0xba => { guard!(vm, frame, code, invokedynamic(vm, frame, u16_operand(bc, pc))); frame.pc += 5; } // invokedynamic (2 index + 2 zero bytes)
        0xbb => match guard!(vm, frame, code, new_object_value(vm, &frame.class, u16_operand(bc, pc))) {
            NewSignal::Value(v) => { frame.push(v); frame.pc += 3; } // new
            // Exit run with PC UNCHANGED → the driver handles the signal at the
            // loop safepoint, then this exact `new` re-executes.
            #[cfg(feature = "next-gen-gc")]
            NewSignal::NurseryFull => return Ok(Some(RunExit::NurseryFull)),
            #[cfg(feature = "next-gen-gc")]
            NewSignal::ClinitPending(c) => return Ok(Some(RunExit::ClinitPending(c))),
        },
        _ => return Err(VmError::UnknownOpcode(op)),
    }
    Ok(None)
}
