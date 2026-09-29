//! Deoptimization: compiled code has written the interpreter state of every scope at the failing
//! point into the frame (the method's own and the inlined callees' frames at the arguments of
//! their calls). The interpreter finishes the innermost scope, then each caller from after its
//! call (or from the call, rethrowing), up to the method itself.

use super::regalloc::{Loc, XMMS};
use crate::jit::analysis::insn_len;
use crate::jit::asm::R;
use crate::method::Method;
use crate::thread::{header, Thread, Throw};

/// Registers the common deoptimization stub pushes, in push order (an even count keeps the
/// stack aligned), above the SSE registers it stores (`XMMS` order, 8 bytes each); the runtime
/// finds the spill slots right above them.
pub const SAVED: [R; 10] = [R::Rbx, R::Rbp, R::R14, R::R15, R::Rsi, R::Rdi, R::R8, R::R9, R::R10, R::R11];

/// A deoptimization point: its scopes and where each live frame slot's value is.
pub struct Desc {
    /// Innermost first.
    pub frames: Vec<Frame>,
    /// (frame slot, location) of every value to write back.
    pub slots: Vec<(i32, Loc)>,
}

/// One scope of a deoptimization point.
pub struct Frame {
    pub method: &'static Method,
    /// Frame slot of its local 0.
    pub frame: i32,
    pub bci: u32,
    /// Operand-stack depth at `bci`.
    pub depth: u32,
}

/// Writes the state of deoptimization point `desc` into the compiled frame at `base` (register
/// values from `saved`, as the common stub pushed them), then resumes its scopes innermost
/// first, with the exception a call left pending rethrown at the innermost one. Returns the
/// method's result; an exception it throws is left pending.
pub extern "sysv64" fn deopt(t: *mut Thread, base: *mut u64, desc: *const Desc, saved: *const u64) -> u64 {
    // SAFETY: called by compiled code with its own thread, frame, descriptor and saved registers.
    let (t, desc) = unsafe { (&mut *t, &*desc) };
    let gprs = XMMS.len();
    for &(slot, loc) in &desc.slots {
        // SAFETY: the stub stored the SSE registers, and above them pushed `SAVED` last to first,
        // below the spill slots.
        let v = unsafe {
            match loc {
                Loc::Xmm(x) => *saved.add(XMMS.iter().position(|&s| s == x).expect("an allocatable SSE register")),
                Loc::Reg(r) => *saved.add(gprs + SAVED.len() - 1 - SAVED.iter().position(|&s| s == r).expect("an allocatable register")),
                Loc::Stack(k) => *saved.add(gprs + SAVED.len() + k as usize),
                Loc::Const(c) => c as u64,
                Loc::None => continue,
            }
        };
        // SAFETY: a slot of the compiled frame.
        unsafe { *base.add(slot as usize) = v };
    }
    let (frames, n) = (&desc.frames[..], desc.frames.len());
    if let Some(s) = crate::jit::stats::get() {
        s.deopts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    super::note_deopt(frames[n - 1].method);
    let mut outcome: Result<u64, Throw> = match t.take_pending() {
        Some(e) => Err(e),
        None => Ok(0),
    };
    if std::env::var_os("RAVA_CORE_DEOPT_LOG").is_some() {
        let scopes: Vec<String> = frames.iter().map(|f| format!("{}@{} depth {} frame {}", f.method, f.bci, f.depth, f.frame)).collect();
        let pending = match &outcome {
            Err(e) => crate::runtime::class_name_of(e.0),
            Ok(_) => "-".into(),
        };
        eprintln!("[deopt] {} pending={pending}", scopes.join(" <- "));
    }
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
