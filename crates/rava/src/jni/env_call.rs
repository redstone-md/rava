//! Calls from native code into Java: method IDs (`jmethodID` is the `Method`'s address), the
//! `Call<Type>Method`, `CallNonvirtual<Type>Method` and `CallStatic<Type>Method` families and
//! `NewObject`, each in its three forms. The `A` form reads a `jvalue` array, the `V` form a
//! `va_list`; the variadic form is an assembly stub that saves the argument registers,
//! builds a `va_list` over them and the caller's stack, and calls the `V` form (stable Rust
//! cannot define C-variadic functions).

use super::env::{c_str, klass, or_pending, SIZE};
use super::{enter, refs, Env, VaList};
use crate::heap::{obj, Oop};
use crate::method::Method;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use std::ffi::c_char;

/// Where a call's arguments come from.
enum Args {
    Va(*mut VaList),
    Values(*const u64),
}

impl Args {
    /// The next argument, of type `ty`, as an interpreter slot value.
    ///
    /// # Safety
    /// The caller passed one of that type.
    unsafe fn next(&mut self, ty: BasicType) -> u64 {
        match self {
            Args::Va(ap) => {
                let ap = &mut **ap;
                match ty {
                    BasicType::Float => (f64::from_bits(ap.double()) as f32).to_bits() as u64,
                    BasicType::Double => ap.double(),
                    _ => normalize(ty, ap.int()),
                }
            }
            Args::Values(p) => {
                let v = **p;
                *p = p.add(1);
                match ty {
                    BasicType::Float => v as u32 as u64,
                    _ => normalize(ty, v),
                }
            }
        }
    }
}

/// An argument's slot value from a register or `jvalue` holding it in its low bytes.
fn normalize(ty: BasicType, v: u64) -> u64 {
    match ty {
        BasicType::Boolean => (v as u8 != 0) as u64,
        BasicType::Byte => v as i8 as i64 as u64,
        BasicType::Char => v as u16 as u64,
        BasicType::Short => v as i16 as i64 as u64,
        BasicType::Int => v as i32 as i64 as u64,
        _ => v,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Virtual,
    Nonvirtual,
    Static,
}

/// Calls `m` (selected by the receiver's class for a virtual call) with arguments from `args`;
/// a reference result becomes a local reference.
fn call(t: &mut Thread, mode: Mode, recv: Oop, m: *const Method, mut args: Args) -> JResult<u64> {
    // SAFETY: a method ID is a live method's address.
    let m: &'static Method = unsafe { &*m };
    let mut slots: Vec<u64> = Vec::with_capacity(m.sig.arg_slots as usize + 1);
    let target = match mode {
        Mode::Static => {
            crate::runtime::initialize(t, m.holder)?;
            m
        }
        _ if recv.is_null() => return Err(crate::runtime::throw_new(t, "java/lang/NullPointerException", "")),
        Mode::Nonvirtual => m,
        Mode::Virtual if m.is_private() || m.is_static() => m,
        // SAFETY: a live object.
        Mode::Virtual => crate::link::select_method(unsafe { obj::klass(recv) }, m).unwrap_or(m),
    };
    if mode != Mode::Static {
        slots.push(recv as u64);
    }
    for &p in m.sig.params.iter() {
        // SAFETY: the native passed arguments of the method's parameter types.
        slots.push(unsafe { args.next(p) });
        if p.slots() == 2 {
            slots.push(0);
        }
    }
    let v = t.call(target, &slots)?;
    Ok(if m.sig.ret == BasicType::Reference { refs::local(t, v as Oop) as u64 } else { v })
}

fn run(env: *mut Env, mode: Mode, recv: Oop, m: *const Method, args: Args) -> u64 {
    let (t, _g) = enter(env);
    let r = call(t, mode, recv, m, args);
    or_pending(t, r)
}

fn new_object(env: *mut Env, c: Oop, m: *const Method, args: Args) -> Oop {
    let (t, _g) = enter(env);
    let r = (|| {
        let Some(k) = klass(c) else { return Ok(std::ptr::null_mut()) };
        crate::runtime::initialize(t, k)?;
        let o = crate::runtime::alloc_instance(t, k)?;
        let o = refs::local(t, o);
        call(t, Mode::Nonvirtual, o, m, args)?;
        Ok(o)
    })();
    or_pending(t, r)
}

fn method_id(env: *mut Env, c: Oop, name: *const c_char, sig: *const c_char, statik: bool) -> *const Method {
    let (t, _g) = enter(env);
    let Some(k) = klass(c) else { return std::ptr::null() };
    let r = (|| {
        crate::runtime::initialize(t, k)?;
        let (n, s) = (Symbol::intern(c_str(name)), Symbol::intern(c_str(sig)));
        let found = if n.as_str() == "<init>" { k.declared_method(n, s) } else { crate::link::lookup_method(k, n, s) };
        match found.filter(|m| m.is_static() == statik) {
            Some(m) => Ok(m as *const Method),
            None => Err(crate::runtime::throw_new(t, "java/lang/NoSuchMethodError", &format!("{}.{}{}", k.name, n.as_str(), s.as_str()))),
        }
    })();
    match r {
        Ok(m) => m,
        Err(e) => {
            t.jni.exception = e.0;
            std::ptr::null()
        }
    }
}

extern "C" fn get_method_id(env: *mut Env, c: Oop, name: *const c_char, sig: *const c_char) -> *const Method {
    method_id(env, c, name, sig, false)
}

extern "C" fn get_static_method_id(env: *mut Env, c: Oop, name: *const c_char, sig: *const c_char) -> *const Method {
    method_id(env, c, name, sig, true)
}

extern "C" fn new_object_v(env: *mut Env, c: Oop, m: *const Method, ap: *mut VaList) -> Oop {
    new_object(env, c, m, Args::Va(ap))
}

extern "C" fn new_object_a(env: *mut Env, c: Oop, m: *const Method, a: *const u64) -> Oop {
    new_object(env, c, m, Args::Values(a))
}

/// A variadic JNI function: saves the six argument registers and eight vector registers,
/// builds a `va_list` past the `gp` fixed integer arguments (whose last register then carries
/// it, `vreg`) and calls `target`, the `V` form, whose result returns as it is.
macro_rules! variadic {
    ($stub:ident, $gp:literal, $vreg:literal, $target:path) => {
        // ELF and Mach-O differ in the section's name and in the underscore C symbols carry.
        #[cfg(not(target_os = "macos"))]
        std::arch::global_asm!(
            ".pushsection .text",
            ".p2align 4",
            concat!(".globl ", stringify!($stub)),
            concat!(stringify!($stub), ":"),
            "sub rsp, 216",
            "mov [rsp], rdi", "mov [rsp + 8], rsi", "mov [rsp + 16], rdx",
            "mov [rsp + 24], rcx", "mov [rsp + 32], r8", "mov [rsp + 40], r9",
            "movaps [rsp + 48], xmm0", "movaps [rsp + 64], xmm1", "movaps [rsp + 80], xmm2", "movaps [rsp + 96], xmm3",
            "movaps [rsp + 112], xmm4", "movaps [rsp + 128], xmm5", "movaps [rsp + 144], xmm6", "movaps [rsp + 160], xmm7",
            concat!("mov dword ptr [rsp + 176], ", $gp),
            "mov dword ptr [rsp + 180], 48",
            "lea rax, [rsp + 224]",
            "mov [rsp + 184], rax",
            "mov [rsp + 192], rsp",
            concat!("lea ", $vreg, ", [rsp + 176]"),
            "call {target}",
            "add rsp, 216",
            "ret",
            ".popsection",
            target = sym $target,
        );
        #[cfg(target_os = "macos")]
        std::arch::global_asm!(
            ".pushsection __TEXT,__text",
            ".p2align 4",
            concat!(".globl _", stringify!($stub)),
            concat!("_", stringify!($stub), ":"),
            "sub rsp, 216",
            "mov [rsp], rdi", "mov [rsp + 8], rsi", "mov [rsp + 16], rdx",
            "mov [rsp + 24], rcx", "mov [rsp + 32], r8", "mov [rsp + 40], r9",
            "movaps [rsp + 48], xmm0", "movaps [rsp + 64], xmm1", "movaps [rsp + 80], xmm2", "movaps [rsp + 96], xmm3",
            "movaps [rsp + 112], xmm4", "movaps [rsp + 128], xmm5", "movaps [rsp + 144], xmm6", "movaps [rsp + 160], xmm7",
            concat!("mov dword ptr [rsp + 176], ", $gp),
            "mov dword ptr [rsp + 180], 48",
            "lea rax, [rsp + 224]",
            "mov [rsp + 184], rax",
            "mov [rsp + 192], rsp",
            concat!("lea ", $vreg, ", [rsp + 176]"),
            "call {target}",
            "add rsp, 216",
            "ret",
            ".popsection",
            target = sym $target,
        );
        extern "C" {
            fn $stub();
        }
    };
}

variadic!(rava_jni_new_object, 24, "rcx", new_object_v);

/// The three families for one return type: `(table offset, V and A forms of each, stubs)`.
macro_rules! typed_calls {
    ($ty:ty, $conv:expr, $off:literal,
     $vv:ident, $va:ident, $nv:ident, $na:ident, $sv:ident, $sa:ident,
     $svv:ident, $snv:ident, $ssv:ident) => {
        extern "C" fn $vv(env: *mut Env, o: Oop, m: *const Method, ap: *mut VaList) -> $ty {
            $conv(run(env, Mode::Virtual, o, m, Args::Va(ap)))
        }
        extern "C" fn $va(env: *mut Env, o: Oop, m: *const Method, a: *const u64) -> $ty {
            $conv(run(env, Mode::Virtual, o, m, Args::Values(a)))
        }
        extern "C" fn $nv(env: *mut Env, o: Oop, _c: Oop, m: *const Method, ap: *mut VaList) -> $ty {
            $conv(run(env, Mode::Nonvirtual, o, m, Args::Va(ap)))
        }
        extern "C" fn $na(env: *mut Env, o: Oop, _c: Oop, m: *const Method, a: *const u64) -> $ty {
            $conv(run(env, Mode::Nonvirtual, o, m, Args::Values(a)))
        }
        extern "C" fn $sv(env: *mut Env, c: Oop, m: *const Method, ap: *mut VaList) -> $ty {
            $conv(run(env, Mode::Static, c, m, Args::Va(ap)))
        }
        extern "C" fn $sa(env: *mut Env, c: Oop, m: *const Method, a: *const u64) -> $ty {
            $conv(run(env, Mode::Static, c, m, Args::Values(a)))
        }
        variadic!($svv, 24, "rcx", $vv);
        variadic!($snv, 32, "r8", $nv);
        variadic!($ssv, 24, "rcx", $sv);
    };
}

typed_calls!(Oop, |v: u64| v as Oop, 0, cv_o, ca_o, nv_o, na_o, sv_o, sa_o, rava_jni_call_o, rava_jni_ncall_o, rava_jni_scall_o);
typed_calls!(u8, |v: u64| v as u8, 1, cv_z, ca_z, nv_z, na_z, sv_z, sa_z, rava_jni_call_z, rava_jni_ncall_z, rava_jni_scall_z);
typed_calls!(i8, |v: u64| v as i8, 2, cv_b, ca_b, nv_b, na_b, sv_b, sa_b, rava_jni_call_b, rava_jni_ncall_b, rava_jni_scall_b);
typed_calls!(u16, |v: u64| v as u16, 3, cv_c, ca_c, nv_c, na_c, sv_c, sa_c, rava_jni_call_c, rava_jni_ncall_c, rava_jni_scall_c);
typed_calls!(i16, |v: u64| v as i16, 4, cv_s, ca_s, nv_s, na_s, sv_s, sa_s, rava_jni_call_s, rava_jni_ncall_s, rava_jni_scall_s);
typed_calls!(i32, |v: u64| v as i32, 5, cv_i, ca_i, nv_i, na_i, sv_i, sa_i, rava_jni_call_i, rava_jni_ncall_i, rava_jni_scall_i);
typed_calls!(i64, |v: u64| v as i64, 6, cv_j, ca_j, nv_j, na_j, sv_j, sa_j, rava_jni_call_j, rava_jni_ncall_j, rava_jni_scall_j);
typed_calls!(f32, |v: u64| f32::from_bits(v as u32), 7, cv_f, ca_f, nv_f, na_f, sv_f, sa_f, rava_jni_call_f, rava_jni_ncall_f, rava_jni_scall_f);
typed_calls!(f64, |v: u64| f64::from_bits(v), 8, cv_d, ca_d, nv_d, na_d, sv_d, sa_d, rava_jni_call_d, rava_jni_ncall_d, rava_jni_scall_d);
typed_calls!((), |_: u64| (), 9, cv_v, ca_v, nv_v, na_v, sv_v, sa_v, rava_jni_call_v, rava_jni_ncall_v, rava_jni_scall_v);

/// `FromReflectedMethod`: the ID of a `java.lang.reflect.Method` or `Constructor`.
extern "C" fn from_reflected_method(_env: *mut Env, m: Oop) -> *const Method {
    crate::natives::reflect::executable_of(m)
}

pub(super) fn fill(t: &mut [usize; SIZE]) {
    t[7] = from_reflected_method as *const () as usize;
    t[28] = rava_jni_new_object as *const () as usize;
    t[29] = new_object_v as *const () as usize;
    t[30] = new_object_a as *const () as usize;
    t[33] = get_method_id as *const () as usize;
    t[113] = get_static_method_id as *const () as usize;
    // Object, Boolean, Byte, Char, Short, Int, Long, Float, Double, Void: three entries each.
    let families: [[usize; 9]; 10] = [
        [rava_jni_call_o as *const () as usize, cv_o as *const () as usize, ca_o as *const () as usize, rava_jni_ncall_o as *const () as usize, nv_o as *const () as usize, na_o as *const () as usize, rava_jni_scall_o as *const () as usize, sv_o as *const () as usize, sa_o as *const () as usize],
        [rava_jni_call_z as *const () as usize, cv_z as *const () as usize, ca_z as *const () as usize, rava_jni_ncall_z as *const () as usize, nv_z as *const () as usize, na_z as *const () as usize, rava_jni_scall_z as *const () as usize, sv_z as *const () as usize, sa_z as *const () as usize],
        [rava_jni_call_b as *const () as usize, cv_b as *const () as usize, ca_b as *const () as usize, rava_jni_ncall_b as *const () as usize, nv_b as *const () as usize, na_b as *const () as usize, rava_jni_scall_b as *const () as usize, sv_b as *const () as usize, sa_b as *const () as usize],
        [rava_jni_call_c as *const () as usize, cv_c as *const () as usize, ca_c as *const () as usize, rava_jni_ncall_c as *const () as usize, nv_c as *const () as usize, na_c as *const () as usize, rava_jni_scall_c as *const () as usize, sv_c as *const () as usize, sa_c as *const () as usize],
        [rava_jni_call_s as *const () as usize, cv_s as *const () as usize, ca_s as *const () as usize, rava_jni_ncall_s as *const () as usize, nv_s as *const () as usize, na_s as *const () as usize, rava_jni_scall_s as *const () as usize, sv_s as *const () as usize, sa_s as *const () as usize],
        [rava_jni_call_i as *const () as usize, cv_i as *const () as usize, ca_i as *const () as usize, rava_jni_ncall_i as *const () as usize, nv_i as *const () as usize, na_i as *const () as usize, rava_jni_scall_i as *const () as usize, sv_i as *const () as usize, sa_i as *const () as usize],
        [rava_jni_call_j as *const () as usize, cv_j as *const () as usize, ca_j as *const () as usize, rava_jni_ncall_j as *const () as usize, nv_j as *const () as usize, na_j as *const () as usize, rava_jni_scall_j as *const () as usize, sv_j as *const () as usize, sa_j as *const () as usize],
        [rava_jni_call_f as *const () as usize, cv_f as *const () as usize, ca_f as *const () as usize, rava_jni_ncall_f as *const () as usize, nv_f as *const () as usize, na_f as *const () as usize, rava_jni_scall_f as *const () as usize, sv_f as *const () as usize, sa_f as *const () as usize],
        [rava_jni_call_d as *const () as usize, cv_d as *const () as usize, ca_d as *const () as usize, rava_jni_ncall_d as *const () as usize, nv_d as *const () as usize, na_d as *const () as usize, rava_jni_scall_d as *const () as usize, sv_d as *const () as usize, sa_d as *const () as usize],
        [rava_jni_call_v as *const () as usize, cv_v as *const () as usize, ca_v as *const () as usize, rava_jni_ncall_v as *const () as usize, nv_v as *const () as usize, na_v as *const () as usize, rava_jni_scall_v as *const () as usize, sv_v as *const () as usize, sa_v as *const () as usize],
    ];
    for (i, f) in families.iter().enumerate() {
        t[34 + 3 * i..34 + 3 * i + 3].copy_from_slice(&f[0..3]);
        t[64 + 3 * i..64 + 3 * i + 3].copy_from_slice(&f[3..6]);
        t[114 + 3 * i..114 + 3 * i + 3].copy_from_slice(&f[6..9]);
    }
}
