//! wide-prefixed operands and multianewarray.

use super::{guard, throw_exc};
use crate::opcodes::*;

/// `None` = keep interpreting, `Some(exit)` = leave `run`.
#[inline(always)]
/// ⚠**`#[inline(never)]` DELIBERATELY** — row 397: inlined into `opcodes::run`, this body's
/// locals are reserved by `run`'s prologue, which every interpreted call pays. `run` measured
/// a 2136-byte frame; the frame itself costs 160 ns against HotSpot's 7.2.
#[inline(never)]
pub(crate) fn wide(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    code: &Code,
    bc: &[u8],
    pc: usize,
    op: u8,
) -> Result<Option<RunExit>, VmError> {
    let _ = (bc, pc, op);
    match op {
        0xc4 => { // wide: 16-bit local index for a load/store/iinc/ret
            let sub = bc[pc + 1];
            let idx = ((bc[pc + 2] as usize) << 8) | bc[pc + 3] as usize;
            match sub {
                0x15..=0x19 => { frame.push(frame.locals()[idx].clone()); frame.pc += 4; } // *load
                0x36..=0x3a => { let v = frame.pop()?; frame.locals_mut()[idx] = v; frame.pc += 4; } // *store
                0x84 => { // iinc
                    let inc = (((bc[pc + 4] as i16) << 8) | bc[pc + 5] as i16) as i32;
                    if let Value::Int(v) = &mut frame.locals_mut()[idx] { *v = v.wrapping_add(inc); }
                    frame.pc += 6;
                }
                0xa9 => { frame.pc += 4; } // ret — subroutines unused by modern javac
                _ => return Err(VmError::UnknownOpcode(0xc4)),
            }
        }
        0xc5 => { // multianewarray: class index (u16), dimension count (u8)
            let desc = class_name_at(&frame.class, u16_operand(bc, pc))?;
            let dims = bc[pc + 3] as usize;
            let mut signed = vec![0i32; dims];
            // Sizes are pushed outermost-first, so the innermost is on top.
            for i in (0..dims).rev() { signed[i] = frame.pop_int()?; }
            // JVMS 6.5.multianewarray: EVERY dimension is validated before any
            // allocation, and a negative one raises NegativeArraySizeException
            // naming that size. Clamping with `.max(0)` instead silently built a
            // zero-length dimension, so a guest loop over it just did nothing —
            // the wrong answer with no exception to explain it.
            if let Some(neg) = signed.iter().copied().find(|&c| c < 0) {
                throw_exc!(vm, frame, code, vm.make_exception_shared(
                    "java/lang/NegativeArraySizeException", &neg.to_string()));
            }
            let counts: Vec<usize> = signed.iter().map(|&c| c as usize).collect();
            let h = alloc_multi_array(vm, &desc, &counts);
            frame.push(Value::Array(h));
            frame.pc += 4;
        }

        other => return Err(VmError::UnknownOpcode(other)),
        _ => return Err(VmError::UnknownOpcode(op)),
    }
    Ok(None)
}
