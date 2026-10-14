//! Calling a JNI native: the interpreter's argument slots become a System V call (`JNIEnv*`, the
//! receiver or class, then the parameters: integers and references in general registers,
//! `float` and `double` in vector registers, the rest on the stack in order), made with the
//! thread in the native state so collections proceed while native code runs.

use super::refs;
use crate::method::Method;
use crate::thread::{JResult, Thread, Throw};
use crate::types::BasicType;

/// Arguments of one C call, sorted into the registers and stack words that carry them.
#[cfg(not(windows))]
#[derive(Default)]
pub(super) struct CArgs {
    ints: [u64; 6],
    n_ints: usize,
    fps: [u64; 8],
    n_fps: usize,
    stack: Vec<u64>,
}

#[cfg(not(windows))]
impl CArgs {
    pub(super) fn int(&mut self, v: u64) {
        if self.n_ints < 6 {
            self.ints[self.n_ints] = v;
            self.n_ints += 1;
        } else {
            self.stack.push(v);
        }
    }

    /// A `float` (its bits in the low half) or a `double`.
    pub(super) fn fp(&mut self, bits: u64) {
        if self.n_fps < 8 {
            self.fps[self.n_fps] = bits;
            self.n_fps += 1;
        } else {
            self.stack.push(bits);
        }
    }

    /// Calls `f` with the System V convention; returns rax and xmm0.
    ///
    /// # Safety
    /// `f` must be a C function taking these arguments.
    pub(super) unsafe fn call(&self, f: usize) -> (u64, u64) {
        let (rax, xmm0): (u64, u64);
        std::arch::asm!(
            "mov r12, rsp",
            "lea rax, [r15 * 8 + 15]",
            "sub rsp, rax",
            "and rsp, -16",
            // Stack arguments, if any (`rep movsq` costs dozens of cycles even for none).
            "mov rcx, r15",
            "test rcx, rcx",
            "jz 3f",
            "2:",
            "mov rax, [r14 + rcx * 8 - 8]",
            "mov [rsp + rcx * 8 - 8], rax",
            "dec rcx",
            "jnz 2b",
            "3:",
            "movq xmm0, [r10]", "movq xmm1, [r10 + 8]", "movq xmm2, [r10 + 16]", "movq xmm3, [r10 + 24]",
            "movq xmm4, [r10 + 32]", "movq xmm5, [r10 + 40]", "movq xmm6, [r10 + 48]", "movq xmm7, [r10 + 56]",
            "mov rdi, [r13]", "mov rsi, [r13 + 8]", "mov rdx, [r13 + 16]",
            "mov rcx, [r13 + 24]", "mov r8, [r13 + 32]", "mov r9, [r13 + 40]",
            "mov eax, 8",
            "call r11",
            "mov rsp, r12",
            "movq rdx, xmm0",
            in("r11") f,
            in("r13") self.ints.as_ptr(),
            in("r10") self.fps.as_ptr(),
            in("r14") self.stack.as_ptr(),
            in("r15") self.stack.len(),
            out("r12") _,
            lateout("rax") rax,
            lateout("rdx") xmm0,
            clobber_abi("C"),
        );
        (rax, xmm0)
    }
}

/// Arguments of one C call in the Win64 convention: by position, the first four in a register
/// (an integer one or a vector one, by type: both are loaded, the callee reads its own) and the
/// rest on the stack above the 32 bytes of shadow space.
#[cfg(windows)]
#[derive(Default)]
pub(super) struct CArgs {
    regs: [u64; 4],
    n: usize,
    stack: Vec<u64>,
}

#[cfg(windows)]
impl CArgs {
    pub(super) fn int(&mut self, v: u64) {
        if self.n < 4 {
            self.regs[self.n] = v;
        } else {
            self.stack.push(v);
        }
        self.n += 1;
    }

    /// A `float` (its bits in the low half) or a `double`.
    pub(super) fn fp(&mut self, bits: u64) {
        self.int(bits);
    }

    /// Calls `f` with the Win64 convention; returns rax and xmm0.
    ///
    /// # Safety
    /// `f` must be a C function taking these arguments.
    pub(super) unsafe fn call(&self, f: usize) -> (u64, u64) {
        let (rax, xmm0): (u64, u64);
        std::arch::asm!(
            "mov r12, rsp",
            "lea rax, [r15 * 8 + 32 + 15]",
            "sub rsp, rax",
            "and rsp, -16",
            "mov rcx, r15",
            "test rcx, rcx",
            "jz 3f",
            "2:",
            "mov rax, [r14 + rcx * 8 - 8]",
            "mov [rsp + rcx * 8 + 24], rax",
            "dec rcx",
            "jnz 2b",
            "3:",
            "mov rcx, [r13]", "mov rdx, [r13 + 8]", "mov r8, [r13 + 16]", "mov r9, [r13 + 24]",
            "movq xmm0, rcx", "movq xmm1, rdx", "movq xmm2, r8", "movq xmm3, r9",
            "call r11",
            "mov rsp, r12",
            "movq rdx, xmm0",
            in("r11") f,
            in("r13") self.regs.as_ptr(),
            in("r14") self.stack.as_ptr(),
            in("r15") self.stack.len(),
            out("r12") _,
            lateout("rax") rax,
            lateout("rdx") xmm0,
            clobber_abi("C"),
        );
        (rax, xmm0)
    }
}

/// Runs native `m` at `f` on the argument slots `args` (receiver first unless static).
pub(crate) fn invoke(t: &mut Thread, m: &'static Method, f: usize, args: &[u64]) -> JResult<u64> {
    let mirror = if m.is_static() { crate::runtime::mirror_of(t, m.holder)? } else { std::ptr::null_mut() };
    let outer = refs::open(t);
    let mut c = CArgs::default();
    c.int(super::env_of(t) as u64);
    let mut slot = 0;
    if m.is_static() {
        c.int(refs::local(t, mirror) as u64);
    } else {
        c.int(args[0]);
        slot = 1;
    }
    for &p in m.sig.params.iter() {
        let v = args[slot];
        match p {
            BasicType::Float => c.fp(v as u32 as u64),
            BasicType::Double => c.fp(v),
            _ => c.int(v),
        }
        slot += p.slots() as usize;
    }
    // `RAVA_TRACE_JNI=<text>`: each call of a native whose name contains it, with its argument
    // slots and result (the generic path only: with `RAVA_NO_JNI_STUBS` every call takes it).
    let traced = std::env::var("RAVA_TRACE_JNI").ok().filter(|f| m.to_string().contains(f.as_str()));
    // SAFETY: the symbol a library exported (or RegisterNatives bound) for this method.
    let (rax, xmm0) = crate::gc::safepoint::native_call(t, || unsafe { c.call(f) });
    if traced.is_some() {
        let hex: Vec<String> = args.iter().map(|a| format!("{a:#x}")).collect();
        let loader = m.holder.loader();
        let loader = if loader.is_null() { "boot".to_string() } else { format!("{}@{:#x}", crate::runtime::class_name_of(loader), loader as usize) };
        eprintln!("[jni] {m} [{loader}] ({}) -> {rax:#x}", hex.join(", "));
    }
    refs::close(t, outer);
    if let Some(e) = refs::take_exception(t) {
        return Err(Throw(e));
    }
    Ok(match m.sig.ret {
        BasicType::Boolean => (rax as u8 != 0) as u64,
        BasicType::Byte => rax as i8 as i64 as u64,
        BasicType::Char => rax as u16 as u64,
        BasicType::Short => rax as i16 as i64 as u64,
        BasicType::Int => rax as i32 as i64 as u64,
        BasicType::Float => xmm0 as u32 as u64,
        BasicType::Double => xmm0,
        BasicType::Void => 0,
        _ => rax,
    })
}
