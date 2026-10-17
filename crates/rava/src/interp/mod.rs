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

pub(crate) use ops::{exec_single, resume};
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
    if t.native_low() {
        return Err(crate::runtime::stack_overflow(t));
    }
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

#[inline(always)]
pub(crate) fn call_native(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<u64> {
    if m.jit_if().is_some_and(|j| j.blocks.load(std::sync::atomic::Ordering::Relaxed)) {
        clear_stack();
    }
    call_native_framed(t, m, locals)
}

/// Zeroes the stack below the caller's frame before a native that may block runs there. The
/// collector scans a blocked thread's native stack conservatively, and the frames that native
/// pushes keep, in slots they never write, whatever deeper calls left there: a parked fork-join
/// worker of All the Mods 10 kept the task it had finished, and through it 12.9 million objects
/// of datapack loading (0.6 GiB), alive for good. (Boehm's collector clears its stack likewise.)
#[inline(never)]
fn clear_stack() {
    let mut words = [0usize; 1024];
    std::hint::black_box(&mut words);
}

/// Notes that the native `t` runs has blocked (`safepoint::blocking`): its later calls first clear
/// the stack its frames will occupy (`clear_stack`).
pub(crate) fn note_blocking(t: &Thread) {
    let hdr = t.top;
    // SAFETY: the innermost frame's header, written by `push_frame`.
    if hdr.is_null() || unsafe { *hdr.add(header::BCI) } != NATIVE_BCI {
        return;
    }
    // SAFETY: as above; a native's frame names its method.
    let m = unsafe { &*(*hdr.add(header::METHOD) as *const Method) };
    if let Some(j) = m.jit_if() {
        j.blocks.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Runs the native `m` in a frame of its own, with its arguments at `locals`.
#[inline(never)]
fn call_native_framed(t: &mut Thread, m: &'static Method, locals: *mut u64) -> JResult<u64> {
    // The VM's own body, else native code a loaded library binds to it (JNI).
    let bound = m.jit_if().map_or(0, |j| j.jni_entry.load(std::sync::atomic::Ordering::Acquire));
    let f = match m.jit().native.get() {
        Some(f) => Ok(*f),
        None if bound != 0 => Err(bound),
        None => match vm().natives.lookup(m) {
            Some(f) => Ok(*m.jit().native.get_or_init(|| f)),
            None => match crate::jni::entry(m) {
                Some(addr) => Err(addr),
                None => {
                    if std::env::var_os("RAVA_TRACE_ULE").is_some() {
                        eprintln!("[ULE] {m}");
                    }
                    return Err(throw_new(t, "java/lang/UnsatisfiedLinkError", &m.to_string()));
                }
            },
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
    let r = match f {
        Ok(f) => f(t, args),
        Err(addr) => crate::jni::invoke(t, m, addr, args),
    };
    if let Some(o) = lock {
        crate::sync::exit(o);
    }
    unsafe { t.pop_frame(hdr) };
    r
}
