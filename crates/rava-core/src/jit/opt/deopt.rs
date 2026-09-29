//! Deoptimization: compiled code has written the interpreter state of the failing point into
//! its frame; the interpreter finishes the method from there (its loops may enter compiled code
//! again through on-stack replacement).

use crate::method::Method;
use crate::thread::Thread;

/// Resumes `m` in the interpreter at `bci` with an operand stack `depth` deep, rethrowing the
/// pending exception (a call that threw) at that instruction. Returns the method's result; an
/// exception it throws is left pending.
pub extern "sysv64" fn deopt(t: *mut Thread, m: *const Method, locals: *mut u64, bci_depth: u64) -> u64 {
    // SAFETY: called by compiled code of `m` with its own thread and frame.
    let (t, m): (&mut Thread, &'static Method) = unsafe { (&mut *t, &*m) };
    let (bci, depth) = ((bci_depth & 0xffff_ffff) as usize, (bci_depth >> 32) as usize);
    if let Some(s) = crate::jit::stats::get() {
        s.deopts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let pending = t.take_pending();
    match crate::interp::resume(t, m, locals, bci, depth, pending) {
        Ok(v) => v,
        Err(e) => {
            t.pending = e.0;
            0
        }
    }
}
