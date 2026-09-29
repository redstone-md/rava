//! Tier-0 interpreter over untyped 64-bit slots.
//!
//! A frame's locals start where the caller's outgoing arguments already sit on the value stack
//! (no copy), followed by the operand stack. Category-2 values (long/double) take two slots,
//! value in the first, so `dup2`/`pop2`/locals indexing follow the JVMS exactly.
//! Slot encodings: int-like values sign-extended, float bits in the low half, raw long/double
//! bits, references as addresses.

pub(crate) mod indy;
pub(crate) mod ops;
pub(crate) mod support;

pub(crate) use ops::exec_single;
pub(crate) use support::{find_handler, read_field, special_target, write_field, Frame};

use crate::method::Method;
use crate::runtime::throw_new;
use crate::thread::{header, JResult, Thread};
use crate::vm::vm;

/// Deepest Java call nesting before StackOverflowError (Rust-stack bound for the recursive
/// interpreter; the thread running Java code is created with a large native stack).
const MAX_DEPTH: u32 = 6000;

/// `bci` header value of a native frame.
const NATIVE_BCI: u64 = (-1i64 << 1) as u64;

/// Invokes `m` whose parameter slots start at `locals`, in compiled code when available.
pub fn invoke(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<u64> {
    crate::gc::safepoint::poll(t);
    if let Some(cm) = crate::jit::on_invoke(m) {
        return crate::jit::call(t, m, cm, locals);
    }
    invoke_interpreted(t, m, locals)
}

/// Invokes `m` in the interpreter (or its native body).
pub fn invoke_interpreted(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<u64> {
    if m.is_native() {
        return call_native(t, m, locals);
    }
    let Some(code) = m.code else {
        return Err(throw_new(t, "java/lang/AbstractMethodError", &m.to_string()));
    };
    let frame_slots = code.max_locals as usize + header::SLOTS + code.max_stack as usize + 2;
    // SAFETY: pointer arithmetic within the thread's value stack, bounds-checked below.
    let frame_end = unsafe { locals.add(frame_slots) };
    if frame_end > t.stack_limit() || t.depth >= MAX_DEPTH {
        return Err(crate::runtime::stack_overflow(t));
    }
    let lock = if m.is_synchronized() { Some(sync_target(t, m, locals)?) } else { None };
    // SAFETY: the header slots follow the locals inside the checked frame.
    let hdr = unsafe { locals.add(code.max_locals as usize) };
    unsafe { t.push_frame(hdr, m, NATIVE_BCI) };
    t.sp = frame_end;
    t.depth += 1;
    let r = ops::execute(t, m, code, locals);
    if let Some(o) = lock {
        crate::sync::exit(o);
    }
    t.depth -= 1;
    unsafe { t.pop_frame(hdr) };
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
    crate::sync::enter(t, o);
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
    let params = m.param_slots() as usize;
    // SAFETY: the caller placed `param_slots` argument slots at `locals`.
    let args = unsafe { std::slice::from_raw_parts(locals, params) };
    let lock = if m.is_synchronized() { Some(sync_target(t, m, locals)?) } else { None };
    // Natives may call back into Java: their header and callees go above the arguments.
    let hdr = unsafe { locals.add(params) }.max(t.sp);
    unsafe { t.push_frame(hdr, m, NATIVE_BCI) };
    t.sp = unsafe { hdr.add(header::SLOTS) };
    let r = f(t, args);
    if let Some(o) = lock {
        crate::sync::exit(o);
    }
    unsafe { t.pop_frame(hdr) };
    r
}
