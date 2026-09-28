//! The interpreter's opcode families, and the two macros every one of them needs.
//!
//! `guard!` runs a call that may throw and lands the throw in this frame's handler; `throw_exc!`
//! raises a value the interpreter itself constructed. Both used to `continue` the interpreter
//! loop. Nothing follows the match inside that loop, so returning `Ok(None)` is the same jump.
//!
//! They take the context explicitly because macro_rules is hygienic for local bindings: a `vm`
//! written in the macro body would resolve at the DEFINITION site, not the call site.
//!
//! Only COLD opcodes live here. Moving the cheap ones cost 12.7% interpreted (245.5 -> 275.5 ns
//! per call, no overlap) and 3.9% even forced inline -- a second match is not affordable at one
//! or two instructions per opcode. athrow, allocation, the invokes and the wide prefix each do
//! far more work than a dispatch, so the same move is free for them.

use crate::opcodes::*;

/// Run a call that may throw. On a throw, look for a handler in THIS frame: found means the
/// stack is cleared, the exception pushed, pc moved, and interpretation continues; not found
/// means the throw propagates to the caller.
macro_rules! guard {
    ($vm:expr, $frame:expr, $code:expr, $call:expr) => {
        match $call {
            Ok(x) => x,
            Err(VmError::Throw(v)) => match find_handler($vm, $frame, $code, $frame.pc, &v)? {
                Some(h) => {
                    $frame.clear_stack();
                    $frame.push(v);
                    $frame.pc = h;
                    return Ok(None);
                }
                None => return Err(VmError::Throw(v)),
            },
            Err(e) => return Err(e),
        }
    };
}

/// Raise a Java exception value: catch it in this frame or propagate.
macro_rules! throw_exc {
    ($vm:expr, $frame:expr, $code:expr, $v:expr) => {{
        let v = $v;
        match find_handler($vm, $frame, $code, $frame.pc, &v)? {
            Some(h) => {
                $frame.clear_stack();
                $frame.push(v);
                $frame.pc = h;
                return Ok(None);
            }
            None => return Err(VmError::Throw(v)),
        }
    }};
}

pub(crate) use guard;
pub(crate) use throw_exc;

mod athrow;
pub(crate) use athrow::athrow;
mod objects;
pub(crate) use objects::objects;
mod invoke;
pub(crate) use invoke::invoke;
mod wide;
pub(crate) use wide::wide;
