//! Tier-0 interpreter over untyped 64-bit slots.
//!
//! A frame's locals start where the caller's outgoing arguments already sit on the value stack
//! (no copy), followed by the operand stack. Category-2 values (long/double) take two slots,
//! value in the first, so `dup2`/`pop2`/locals indexing follow the JVMS exactly.
//! Slot encodings: int-like values sign-extended, float bits in the low half, raw long/double
//! bits, references as addresses.

mod ops;
mod support;

pub(crate) use support::{read_field, write_field};

use crate::method::Method;
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};
use crate::vm::vm;

/// Deepest Java call nesting before StackOverflowError (Rust-stack bound for the recursive
/// interpreter; the thread running Java code is created with a large native stack).
const MAX_DEPTH: u32 = 6000;

/// Invokes `m` whose parameter slots start at `locals`.
pub fn invoke(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<u64> {
    if m.is_native() {
        return call_native(t, m, locals);
    }
    let Some(code) = m.code else {
        return Err(throw_new(t, "java/lang/AbstractMethodError", &m.to_string()));
    };
    let frame_slots = code.max_locals as usize + code.max_stack as usize + 2;
    // SAFETY: pointer arithmetic within the thread's value stack, bounds-checked below.
    let frame_end = unsafe { locals.add(frame_slots) };
    if frame_end > t.stack_limit() || t.depth >= MAX_DEPTH {
        return Err(throw_new(t, "java/lang/StackOverflowError", ""));
    }
    let lock = if m.is_synchronized() { Some(sync_target(t, m, locals)?) } else { None };
    let saved_sp = t.sp;
    t.sp = frame_end;
    t.depth += 1;
    t.frames.push((m, std::ptr::null()));
    let r = ops::execute(t, m, code, locals);
    if let Some(o) = lock {
        crate::sync::exit(o);
    }
    t.frames.pop();
    t.depth -= 1;
    t.sp = saved_sp;
    r
}

/// Acquires the monitor of a synchronized method: the receiver, or the class for statics.
fn sync_target(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<crate::heap::Oop> {
    let o = if m.is_static() {
        crate::runtime::mirror_of(t, m.holder)?
    } else {
        // SAFETY: slot 0 holds the receiver.
        unsafe { *locals as crate::heap::Oop }
    };
    crate::sync::enter(o);
    Ok(o)
}

fn call_native(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<u64> {
    let f = match m.native.get() {
        Some(f) => *f,
        None => match vm().natives.lookup(m) {
            Some(f) => *m.native.get_or_init(|| f),
            None => {
                if std::env::var_os("RAVA_CORE_TRACE_ULE").is_some() {
                    eprintln!("[ULE] {m}");
                }
                return Err(throw_new(t, "java/lang/UnsatisfiedLinkError", &m.to_string()));
            }
        },
    };
    // SAFETY: the caller placed `param_slots` argument slots at `locals`.
    let args = unsafe { std::slice::from_raw_parts(locals, m.param_slots() as usize) };
    let lock = if m.is_synchronized() { Some(sync_target(t, m, locals)?) } else { None };
    let saved_sp = t.sp;
    // SAFETY: natives may call back into Java; keep their frames above the arguments.
    t.sp = unsafe { locals.add(m.param_slots() as usize) }.max(t.sp);
    t.frames.push((m, std::ptr::null()));
    let r = f(t, args);
    if let Some(o) = lock {
        crate::sync::exit(o);
    }
    t.frames.pop();
    t.sp = saved_sp;
    r
}
