//! Deoptimization: compiled code has written the interpreter state of every scope at the failing
//! point into the frame (the method's own and the inlined callees' frames at the arguments of
//! their calls). The interpreter finishes the innermost scope, then each caller from after its
//! call (or from the call, rethrowing), up to the method itself.

use crate::jit::analysis::insn_len;
use crate::method::Method;
use crate::thread::{header, Thread, Throw};

/// One scope of a deoptimization point.
pub struct Frame {
    pub method: &'static Method,
    /// Frame slot of its local 0.
    pub frame: i32,
    pub bci: u32,
    /// Operand-stack depth at `bci`.
    pub depth: u32,
}

/// Resumes the scopes `frames[..n]` (innermost first) of the compiled frame at `base`, with the
/// exception a call left pending rethrown at the innermost one. Returns the method's result; an
/// exception it throws is left pending.
pub extern "sysv64" fn deopt(t: *mut Thread, base: *mut u64, frames: *const Frame, n: usize) -> u64 {
    // SAFETY: called by compiled code with its own thread, frame and deopt descriptor.
    let (t, frames) = unsafe { (&mut *t, std::slice::from_raw_parts(frames, n)) };
    if let Some(s) = crate::jit::stats::get() {
        s.deopts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    super::note_deopt(frames[n - 1].method);
    let mut outcome: Result<u64, Throw> = match t.take_pending() {
        Some(e) => Err(e),
        None => Ok(0),
    };
    for (i, f) in frames.iter().enumerate() {
        let code = f.method.code.expect("compiled scope has code");
        // SAFETY: the scope's frame lies within the compiled frame.
        let locals = unsafe { base.add(f.frame as usize) };
        let (bci, depth, pending) = match (i, outcome) {
            (0, Ok(_)) => (f.bci as usize, f.depth as usize, None),
            (_, Err(e)) => (f.bci as usize, f.depth as usize, Some(e)),
            // The callee returned: push its result and continue after the call.
            (_, Ok(v)) => {
                let slots = frames[i - 1].method.sig.ret.slots() as usize;
                // SAFETY: operand slot `depth` of this scope (the callee's first local).
                unsafe { *locals.add(code.max_locals as usize + header::SLOTS + f.depth as usize) = v };
                (f.bci as usize + insn_len(code.code, f.bci as usize), f.depth as usize + slots, None)
            }
        };
        outcome = if i + 1 == frames.len() {
            crate::interp::resume(t, f.method, locals, bci, depth, pending)
        } else {
            // An inlined scope gets its own linked frame for the interpreter.
            // SAFETY: header and operand stack of this scope's frame, within the compiled frame.
            unsafe {
                let hdr = locals.add(code.max_locals as usize);
                t.push_frame(hdr, f.method, 0);
                t.sp = hdr.add(header::SLOTS + code.max_stack as usize + 2);
                let r = crate::interp::resume(t, f.method, locals, bci, depth, pending);
                t.pop_frame(hdr);
                r
            }
        };
    }
    match outcome {
        Ok(v) => v,
        Err(e) => {
            t.pending = e.0;
            0
        }
    }
}
