//! Generated entries of JNI natives. Bound to its library function, a native method gets a stub
//! with the compiled-code ABI (`jit::EntryFn`) that does what `call::invoke` does for this one
//! signature: it pushes the native's frame, moves the argument slots straight into the C
//! argument registers, switches the thread to the native state around the call with plain
//! stores (one store-load fence on the way back), and drops the call's local references only
//! when it made any. Whatever it does not cover (synchronized natives, a class without its
//! mirror yet, a short stack) goes to the generic entry.

use super::{refs, JniThread, Scope};
use crate::gc::safepoint::{self, Record, NATIVE, RUNNING};
use crate::jit::asm::{mem, Alu, Asm, Cond, X, R};
use crate::method::Method;
use crate::thread::{header, offsets, Thread};
use crate::types::BasicType;
use std::mem::offset_of;
use std::sync::atomic::Ordering;

/// `bci << 1` of a native frame (bci -1), as `interp::call_native` writes it.
const NATIVE_BCI: i64 = -1 << 1;
/// Integer and vector argument registers of the System V ABI.
const INTS: [R; 6] = [R::Rdi, R::Rsi, R::Rdx, R::Rcx, R::R8, R::R9];
const FPS: usize = 8;

/// Where one C argument comes from.
#[derive(Clone, Copy)]
enum Src {
    Env,
    /// The class mirror, stored in the frame's extra slot.
    Mirror,
    /// Argument slot `i`: a float or double (`Some(double)`), else an integer or reference.
    Slot(usize, Option<bool>),
}

/// Binds `m` to the library function `f` (0: unbound): its entry becomes a stub calling `f`,
/// or the generic native entry.
pub(super) fn bind(m: &'static Method, f: usize) {
    let entry = if f != 0 && eligible(m) { generate(m, f) } else { crate::jit::native_entry() };
    m.entry.store(entry, Ordering::Release);
    crate::jit::code_cache::repoint(m);
}

/// Natives the stub serves: unsynchronized, without a VM body (which takes precedence), and
/// with no invocation statistics to keep.
fn eligible(m: &'static Method) -> bool {
    !m.is_synchronized() && m.native.get().is_none() && crate::vm::vm().natives.lookup(m).is_none() && crate::jit::stats::get().is_none()
}

/// The C arguments of `m`'s native in order: the environment, the receiver or class, then the
/// parameters.
fn sources(m: &Method) -> Vec<Src> {
    let mut out = vec![Src::Env];
    let mut slot = 0;
    if m.is_static() {
        out.push(Src::Mirror);
    } else {
        out.push(Src::Slot(0, None));
        slot = 1;
    }
    for &p in m.sig.params.iter() {
        let fp = match p {
            BasicType::Float => Some(false),
            BasicType::Double => Some(true),
            _ => None,
        };
        out.push(Src::Slot(slot, fp));
        slot += p.slots() as usize;
    }
    out
}

/// Loads argument `s` into `r` (an integer) or `x` (a float or double).
fn load(a: &mut Asm, s: Src, r: R) {
    match s {
        Src::Env => a.lea(r, mem(R::Rbx, (offset_of!(Thread, jni) + offset_of!(JniThread, env)) as i32)),
        Src::Mirror => a.load(true, r, mem(R::R12, (header::SLOTS * 8) as i32)),
        Src::Slot(i, _) => a.load(true, r, mem(R::Rbp, i as i32 * 8)),
    }
}

fn generate(m: &'static Method, f: usize) -> usize {
    let srcs = sources(m);
    let (mut ints, mut fps, mut stack) = (Vec::new(), Vec::new(), Vec::new());
    for &s in &srcs {
        match s {
            Src::Slot(_, Some(d)) if fps.len() < FPS => fps.push((s, d)),
            Src::Slot(_, Some(_)) => stack.push(s),
            _ if ints.len() < INTS.len() => ints.push(s),
            _ => stack.push(s),
        }
    }
    let params = m.param_slots() as i32;
    let extra = m.is_static() as usize;
    let jni = offset_of!(Thread, jni);
    let scope = (jni + offset_of!(JniThread, scope)) as i32;
    let exception = (jni + offset_of!(JniThread, exception)) as i32;
    let rec_rsp = offset_of!(Record, rsp) as i32;
    let rec_state = offset_of!(Record, state) as i32;
    let mut a = Asm::default();
    let generic = a.new_label();

    // Entered with rdi = thread, rsi = argument slots, rsp 8 mod 16. Checks first, with the
    // registers untouched for the generic entry: the native stack, the class mirror, and room
    // for the frame (`hdr`, in r11) above the arguments and the published stack end.
    a.alu_rm(true, Alu::Cmp, R::Rsp, mem(R::Rdi, offset_of!(Thread, native_limit) as i32));
    a.jcc(Cond::B, generic);
    if m.is_static() {
        a.mov_ri(R::Rax, offset_of!(crate::klass::Klass, mirror) as i64 + m.holder as *const _ as i64);
        a.alu_mi(true, Alu::Cmp, mem(R::Rax, 0), 0);
        a.jcc(Cond::E, generic);
    }
    a.lea(R::R11, mem(R::Rsi, params * 8));
    a.load(true, R::Rax, mem(R::Rdi, offsets::SP));
    a.alu_rr(true, Alu::Cmp, R::R11, R::Rax);
    a.cmov(true, Cond::B, R::R11, R::Rax);
    a.lea(R::Rax, mem(R::R11, ((header::SLOTS + extra) * 8) as i32));
    a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::Rdi, offsets::LIMIT));
    a.jcc(Cond::A, generic);

    // rbx = thread, rbp = argument slots, r12 = frame header, r14 = safepoint record; the
    // enclosing call's local-reference scope is saved at [rsp] (6 pushes + 24 bytes: aligned).
    for r in [R::Rbx, R::Rbp, R::R12, R::R13, R::R14, R::R15] {
        a.push(r);
    }
    a.alu_ri(true, Alu::Sub, R::Rsp, 24);
    a.mov_rr(true, R::Rbx, R::Rdi);
    a.mov_rr(true, R::Rbp, R::Rsi);
    a.mov_rr(true, R::R12, R::R11);
    a.mov_ri(R::Rax, m as *const Method as i64);
    a.store(true, mem(R::R12, header::METHOD as i32 * 8), R::Rax);
    a.store_imm(mem(R::R12, header::BCI as i32 * 8), NATIVE_BCI as i32);
    a.load(true, R::Rax, mem(R::Rbx, offsets::TOP));
    a.store(true, mem(R::R12, header::CALLER as i32 * 8), R::Rax);
    a.load(true, R::Rax, mem(R::Rbx, offsets::SP));
    a.store(true, mem(R::R12, header::SAVED_SP as i32 * 8), R::Rax);
    a.store(true, mem(R::Rbx, offsets::TOP), R::R12);
    if m.is_static() {
        // The mirror in a value-stack slot, which the collector scans and pins.
        a.mov_ri(R::Rax, offset_of!(crate::klass::Klass, mirror) as i64 + m.holder as *const _ as i64);
        a.load(true, R::Rax, mem(R::Rax, 0));
        a.store(true, mem(R::R12, (header::SLOTS * 8) as i32), R::Rax);
    }
    a.lea(R::Rax, mem(R::R12, ((header::SLOTS + extra) * 8) as i32));
    a.store(true, mem(R::Rbx, offsets::SP), R::Rax);
    for i in 0..3 {
        a.load(true, R::Rax, mem(R::Rbx, scope + i * 8));
        a.store(true, mem(R::Rsp, i * 8), R::Rax);
    }
    a.store_imm(mem(R::Rbx, scope + offset_of!(Scope, touched) as i32), 0);
    a.load(true, R::R14, mem(R::Rbx, offset_of!(Thread, gc_record) as i32));

    // Arguments: stack words (pushed last to first, padded to keep the call aligned), then
    // vector and integer registers.
    let pad = stack.len() % 2 == 1;
    if pad {
        a.alu_ri(true, Alu::Sub, R::Rsp, 8);
    }
    for &s in stack.iter().rev() {
        load(&mut a, s, R::Rax);
        a.push(R::Rax);
    }
    for (i, &(s, d)) in fps.iter().enumerate() {
        let Src::Slot(slot, _) = s else { unreachable!() };
        a.fload(d, X(i as u8), mem(R::Rbp, slot as i32 * 8));
    }
    for (i, &s) in ints.iter().enumerate() {
        load(&mut a, s, INTS[i]);
    }

    // The call, in the native state (plain stores suffice: a collector reading the state late
    // only waits longer); back in Java, a collection requested meanwhile is waited out.
    let running = a.new_label();
    a.test_rr(true, R::R14, R::R14);
    let no_rec = a.new_label();
    a.jcc(Cond::E, no_rec);
    a.store(true, mem(R::R14, rec_rsp), R::Rsp);
    a.store8_imm(mem(R::R14, rec_state), NATIVE);
    a.bind(no_rec);
    a.mov_ri(R::R11, f as i64);
    a.call_r(R::R11);
    let popped = (stack.len() + pad as usize) * 8;
    if popped > 0 {
        a.alu_ri(true, Alu::Add, R::Rsp, popped as i32);
    }
    a.mov_rr(true, R::R13, R::Rax);
    a.mov_rx(true, R::R15, X(0));
    a.test_rr(true, R::R14, R::R14);
    a.jcc(Cond::E, running);
    a.store8_imm(mem(R::R14, rec_state), RUNNING);
    a.storeload();
    a.mov_ri(R::Rax, &safepoint::REQUESTED as *const _ as i64);
    a.cmp8_mi(mem(R::Rax, 0), 0);
    a.jcc(Cond::E, running);
    a.mov_rr(true, R::Rdi, R::R14);
    a.call_abs(leave_native as *const () as usize);
    a.bind(running);

    // Local references, if the call made any, then the enclosing call's scope.
    let released = a.new_label();
    a.alu_mi(true, Alu::Cmp, mem(R::Rbx, scope + offset_of!(Scope, touched) as i32), 0);
    a.jcc(Cond::E, released);
    a.mov_rr(true, R::Rdi, R::Rbx);
    a.call_abs(release as *const () as usize);
    a.bind(released);
    for i in 0..3 {
        a.load(true, R::Rax, mem(R::Rsp, i * 8));
        a.store(true, mem(R::Rbx, scope + i * 8), R::Rax);
    }

    // An exception the native raised becomes the pending one, with a zero result.
    let no_exc = a.new_label();
    a.load(true, R::Rax, mem(R::Rbx, exception));
    a.test_rr(true, R::Rax, R::Rax);
    a.jcc(Cond::E, no_exc);
    a.store(true, mem(R::Rbx, offsets::PENDING), R::Rax);
    a.store_imm(mem(R::Rbx, exception), 0);
    a.alu_rr(true, Alu::Xor, R::R13, R::R13);
    a.alu_rr(true, Alu::Xor, R::R15, R::R15);
    a.bind(no_exc);

    a.load(true, R::Rax, mem(R::R12, header::CALLER as i32 * 8));
    a.store(true, mem(R::Rbx, offsets::TOP), R::Rax);
    a.load(true, R::Rax, mem(R::R12, header::SAVED_SP as i32 * 8));
    a.store(true, mem(R::Rbx, offsets::SP), R::Rax);

    // The result as a slot value in rax, and in xmm0 too (`jit::float_result_stub`).
    match m.sig.ret {
        BasicType::Boolean => {
            // Only the low byte of a C `jboolean` is defined.
            a.movzx8(R::Rax, R::R13);
            a.test_rr(false, R::Rax, R::Rax);
            a.setcc(Cond::Ne, R::Rax);
            a.movzx8(R::Rax, R::Rax);
        }
        BasicType::Byte => a.movsx_rr(1, R::Rax, R::R13),
        BasicType::Char => a.movzx16_rr(R::Rax, R::R13),
        BasicType::Short => a.movsx_rr(2, R::Rax, R::R13),
        BasicType::Int => a.movsxd(R::Rax, R::R13),
        BasicType::Float => a.mov_rr(false, R::Rax, R::R15),
        BasicType::Double => a.mov_rr(true, R::Rax, R::R15),
        BasicType::Void => a.alu_rr(false, Alu::Xor, R::Rax, R::Rax),
        _ => a.mov_rr(true, R::Rax, R::R13),
    }
    a.mov_xr(true, X(0), R::Rax);
    a.alu_ri(true, Alu::Add, R::Rsp, 24);
    for r in [R::R15, R::R14, R::R13, R::R12, R::Rbp, R::Rbx] {
        a.pop(r);
    }
    a.ret();

    a.bind(generic);
    a.jmp_abs(crate::jit::native_entry());
    crate::jit::code_cache::install(&a.finish())
}

extern "sysv64" fn leave_native(rec: &'static Record) {
    safepoint::leave_native(rec);
}

extern "sysv64" fn release(t: *mut Thread) {
    // SAFETY: the stub passes its current thread.
    refs::release(unsafe { &mut *t });
}
