//! The `JNIEnv` function table (JNI spec, "JNI Functions", by index) and its functions on
//! classes, exceptions, references, monitors and natives. Calls, fields, strings and arrays are
//! in `env_call`, `env_field` and `env_data`. A function not provided yet stops the process
//! naming its index, so what an application needs shows at once.

use super::{enter, refs, Env};
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use std::ffi::{c_char, CStr};

/// Entries of a JNI 21 function table (`IsVirtualThread` is the last).
pub(super) const SIZE: usize = 235;

macro_rules! missing {
    ($($i:literal)*) => {
        [$({
            extern "C" fn f() -> ! {
                eprintln!("[rava] JNI function #{} is not implemented", $i);
                std::process::abort();
            }
            f as *const () as usize
        }),*]
    };
}

/// The table every thread's `JNIEnv` points at.
pub(super) fn table() -> *const usize {
    static T: std::sync::OnceLock<Box<[usize; SIZE]>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t: Box<[usize; SIZE]> = Box::new(missing!(
            0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39
            40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79
            80 81 82 83 84 85 86 87 88 89 90 91 92 93 94 95 96 97 98 99 100 101 102 103 104 105 106 107 108 109 110 111 112 113 114 115 116 117 118 119
            120 121 122 123 124 125 126 127 128 129 130 131 132 133 134 135 136 137 138 139 140 141 142 143 144 145 146 147 148 149 150 151 152 153 154 155 156 157 158 159
            160 161 162 163 164 165 166 167 168 169 170 171 172 173 174 175 176 177 178 179 180 181 182 183 184 185 186 187 188 189 190 191 192 193 194 195 196 197 198 199
            200 201 202 203 204 205 206 207 208 209 210 211 212 213 214 215 216 217 218 219 220 221 222 223 224 225 226 227 228 229 230 231 232 233 234
        ));
        t[0] = 0;
        t[1] = 0;
        t[2] = 0;
        t[3] = 0;
        t[4] = get_version as *const () as usize;
        t[6] = find_class as *const () as usize;
        t[10] = get_superclass as *const () as usize;
        t[11] = is_assignable_from as *const () as usize;
        t[13] = throw as *const () as usize;
        t[14] = throw_new as *const () as usize;
        t[15] = exception_occurred as *const () as usize;
        t[16] = exception_describe as *const () as usize;
        t[17] = exception_clear as *const () as usize;
        t[18] = fatal_error as *const () as usize;
        t[19] = push_local_frame as *const () as usize;
        t[20] = pop_local_frame as *const () as usize;
        t[21] = new_global_ref as *const () as usize;
        t[22] = delete_global_ref as *const () as usize;
        t[23] = delete_local_ref as *const () as usize;
        t[24] = is_same_object as *const () as usize;
        t[25] = new_local_ref as *const () as usize;
        t[26] = ensure_local_capacity as *const () as usize;
        t[27] = alloc_object as *const () as usize;
        t[31] = get_object_class as *const () as usize;
        t[32] = is_instance_of as *const () as usize;
        t[215] = register_natives as *const () as usize;
        t[216] = unregister_natives as *const () as usize;
        t[217] = monitor_enter as *const () as usize;
        t[218] = monitor_exit as *const () as usize;
        t[219] = get_java_vm as *const () as usize;
        t[226] = new_global_ref as *const () as usize;
        t[227] = delete_global_ref as *const () as usize;
        t[228] = exception_check as *const () as usize;
        t[229] = new_direct_byte_buffer as *const () as usize;
        t[230] = get_direct_buffer_address as *const () as usize;
        t[231] = get_direct_buffer_capacity as *const () as usize;
        t[232] = get_object_ref_type as *const () as usize;
        t[234] = is_virtual_thread as *const () as usize;
        super::env_call::fill(&mut t);
        super::env_field::fill(&mut t);
        super::env_data::fill(&mut t);
        t
    })
    .as_ptr()
}

/// Records `r`'s exception as pending for native code; the value, or zero.
pub(super) fn or_pending<T: Default>(t: &mut Thread, r: JResult<T>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            t.jni.exception = e.0;
            T::default()
        }
    }
}

/// The class a `jclass` names.
pub(super) fn klass(c: Oop) -> Option<&'static Klass> {
    (!c.is_null()).then(|| crate::runtime::klass_of_mirror(c)).flatten()
}

/// A `jclass` (local reference) for `k`.
pub(super) fn jclass(t: &mut Thread, k: &'static Klass) -> JResult<Oop> {
    let m = crate::runtime::mirror_of(t, k)?;
    Ok(refs::local(t, m))
}

/// The class loader native code's `FindClass` resolves with: that of the innermost frame of a
/// class not loaded by the boot loader (the native's class, or the caller of
/// `System.loadLibrary` in `JNI_OnLoad`), else the boot loader.
fn context_loader(t: &Thread) -> Oop {
    t.frames().map(|(m, _)| m.holder.loader()).find(|l| !l.is_null()).unwrap_or(std::ptr::null_mut())
}

/// Loads (and initializes) the class named by `name` (internal form, or a descriptor for an
/// array) as seen from `loader`.
pub(super) fn load(t: &mut Thread, loader: Oop, name: &str) -> JResult<&'static Klass> {
    let k = crate::vm::vm().load_class(t, loader, Symbol::intern(name)).map_err(|e| crate::runtime::link_error(t, e))?;
    crate::runtime::initialize(t, k)?;
    Ok(k)
}

/// A C string argument.
pub(super) fn c_str<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    // SAFETY: JNI string arguments are NUL-terminated modified UTF-8 (ASCII for names).
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
}

extern "C" fn get_version(_env: *mut Env) -> i32 {
    super::VERSION
}

extern "C" fn find_class(env: *mut Env, name: *const c_char) -> Oop {
    let (t, _g) = enter(env);
    let loader = context_loader(t);
    let r = load(t, loader, c_str(name)).and_then(|k| jclass(t, k));
    or_pending(t, r)
}

extern "C" fn get_superclass(env: *mut Env, c: Oop) -> Oop {
    let (t, _g) = enter(env);
    let r = match klass(c).filter(|k| !k.is_interface()).and_then(|k| k.super_klass) {
        Some(s) => jclass(t, s),
        None => Ok(std::ptr::null_mut()),
    };
    or_pending(t, r)
}

extern "C" fn is_assignable_from(_env: *mut Env, sub: Oop, sup: Oop) -> u8 {
    match (klass(sub), klass(sup)) {
        (Some(a), Some(b)) => a.is_subtype_of(b) as u8,
        _ => 0,
    }
}

extern "C" fn throw(env: *mut Env, e: Oop) -> i32 {
    let (t, _g) = enter(env);
    t.jni.exception = e;
    0
}

extern "C" fn throw_new(env: *mut Env, c: Oop, msg: *const c_char) -> i32 {
    let (t, _g) = enter(env);
    let Some(k) = klass(c) else { return -1 };
    let e = crate::runtime::throw_new(t, k.name.as_str(), c_str(msg));
    t.jni.exception = e.0;
    0
}

extern "C" fn exception_occurred(env: *mut Env) -> Oop {
    let (t, _g) = enter(env);
    let e = t.jni.exception;
    refs::local(t, e)
}

extern "C" fn exception_describe(env: *mut Env) {
    let (t, _g) = enter(env);
    let e = std::mem::replace(&mut t.jni.exception, std::ptr::null_mut());
    if !e.is_null() {
        eprintln!("Exception in native code: {}", crate::natives::describe_throwable(e));
    }
}

extern "C" fn exception_clear(env: *mut Env) {
    let (t, _g) = enter(env);
    t.jni.exception = std::ptr::null_mut();
}

extern "C" fn exception_check(env: *mut Env) -> u8 {
    let (t, _g) = enter(env);
    (!t.jni.exception.is_null()) as u8
}

extern "C" fn fatal_error(_env: *mut Env, msg: *const c_char) -> ! {
    eprintln!("FATAL ERROR in native method: {}", c_str(msg));
    std::process::abort();
}

extern "C" fn push_local_frame(env: *mut Env, _capacity: i32) -> i32 {
    let (t, _g) = enter(env);
    refs::touch(t);
    let mark = t.handles.len();
    t.jni.frames.push(mark);
    0
}

extern "C" fn pop_local_frame(env: *mut Env, result: Oop) -> Oop {
    let (t, _g) = enter(env);
    if let Some(mark) = t.jni.frames.pop() {
        t.handles.truncate(mark);
    }
    refs::local(t, result)
}

extern "C" fn new_global_ref(_env: *mut Env, o: Oop) -> Oop {
    refs::new_global(o)
}

extern "C" fn delete_global_ref(_env: *mut Env, o: Oop) {
    refs::delete_global(o);
}

extern "C" fn delete_local_ref(env: *mut Env, o: Oop) {
    let (t, _g) = enter(env);
    // The latest reference to the object within the current local frame.
    let floor = refs::floor(t);
    if let Some(i) = t.handles[floor.min(t.handles.len())..].iter().rposition(|&h| h == o) {
        t.handles.remove(floor + i);
    }
}

extern "C" fn is_same_object(_env: *mut Env, a: Oop, b: Oop) -> u8 {
    (a == b) as u8
}

extern "C" fn new_local_ref(env: *mut Env, o: Oop) -> Oop {
    let (t, _g) = enter(env);
    refs::local(t, o)
}

extern "C" fn ensure_local_capacity(_env: *mut Env, _capacity: i32) -> i32 {
    0
}

extern "C" fn alloc_object(env: *mut Env, c: Oop) -> Oop {
    let (t, _g) = enter(env);
    let r = match klass(c) {
        Some(k) => crate::runtime::initialize(t, k).and_then(|_| crate::runtime::alloc_instance(t, k)),
        None => Ok(std::ptr::null_mut()),
    };
    let o = or_pending(t, r);
    refs::local(t, o)
}

extern "C" fn get_object_class(env: *mut Env, o: Oop) -> Oop {
    let (t, _g) = enter(env);
    if o.is_null() {
        return o;
    }
    // SAFETY: a live object.
    let k = unsafe { obj::klass(o) };
    let r = jclass(t, k);
    or_pending(t, r)
}

extern "C" fn is_instance_of(_env: *mut Env, o: Oop, c: Oop) -> u8 {
    match klass(c) {
        // SAFETY: a live object.
        Some(k) => (o.is_null() || unsafe { obj::klass(o) }.is_subtype_of(k)) as u8,
        None => 0,
    }
}

#[repr(C)]
struct NativeMethod {
    name: *const c_char,
    signature: *const c_char,
    f: usize,
}

extern "C" fn register_natives(env: *mut Env, c: Oop, methods: *const NativeMethod, n: i32) -> i32 {
    let (t, _g) = enter(env);
    let Some(k) = klass(c) else { return -1 };
    for i in 0..n.max(0) as usize {
        // SAFETY: the caller's array of `n` entries.
        let e = unsafe { &*methods.add(i) };
        let (name, sig) = (Symbol::intern(c_str(e.name)), Symbol::intern(c_str(e.signature)));
        match k.declared_method(name, sig).filter(|m| m.is_native()) {
            Some(m) => super::libs::register(m, e.f),
            None => {
                let msg = format!("{}.{}{}", k.name, name.as_str(), sig.as_str());
                t.jni.exception = crate::runtime::throw_new(t, "java/lang/NoSuchMethodError", &msg).0;
                return -1;
            }
        }
    }
    0
}

extern "C" fn unregister_natives(_env: *mut Env, c: Oop) -> i32 {
    if let Some(k) = klass(c) {
        k.methods.get().into_iter().flat_map(|ms| ms.iter()).filter(|m| m.is_native()).for_each(|m| super::libs::register(m, 0));
    }
    0
}

extern "C" fn monitor_enter(env: *mut Env, o: Oop) -> i32 {
    let (t, _g) = enter(env);
    crate::sync::enter(t, o);
    0
}

extern "C" fn monitor_exit(env: *mut Env, o: Oop) -> i32 {
    let (_t, _g) = enter(env);
    if crate::sync::exit(o) {
        0
    } else {
        -1
    }
}

extern "C" fn get_java_vm(_env: *mut Env, vm: *mut *const std::ffi::c_void) -> i32 {
    // SAFETY: the caller's out-parameter.
    unsafe { *vm = super::vm_iface::java_vm() };
    0
}

/// `java.nio.Buffer.address` and `capacity` of a direct buffer (`None` for a heap buffer).
fn direct_buffer(o: Oop) -> Option<(i64, i32)> {
    if o.is_null() {
        return None;
    }
    // SAFETY: a live object; the fields are java.nio.Buffer's.
    let k = unsafe { obj::klass(o) };
    let (_, addr) = crate::link::find_field(k, Symbol::intern("address"), Symbol::intern("J"))?;
    let (_, cap) = crate::link::find_field(k, Symbol::intern("capacity"), Symbol::intern("I"))?;
    // SAFETY: instance fields of `o`.
    let (a, c) = unsafe { (obj::read::<i64>(o, addr.offset), obj::read::<i32>(o, cap.offset)) };
    (a != 0).then_some((a, c))
}

/// A `java.nio.DirectByteBuffer` over native memory, through the constructor the JDK keeps
/// for JNI (`DirectByteBuffer(long address, long capacity)`).
extern "C" fn new_direct_byte_buffer(env: *mut Env, address: usize, capacity: i64) -> Oop {
    let (t, _g) = enter(env);
    let r = (|| {
        let k = load(t, std::ptr::null_mut(), "java/nio/DirectByteBuffer")?;
        let init = k.declared_method(Symbol::intern("<init>"), Symbol::intern("(JJ)V")).expect("DirectByteBuffer(long, long)");
        let o = crate::runtime::alloc_instance(t, k)?;
        let o = refs::local(t, o);
        t.call(init, &[o as u64, address as u64, 0, capacity as u64, 0])?;
        Ok(o)
    })();
    or_pending(t, r)
}

extern "C" fn get_direct_buffer_address(_env: *mut Env, buf: Oop) -> usize {
    direct_buffer(buf).map_or(0, |(a, _)| a as usize)
}

extern "C" fn get_direct_buffer_capacity(_env: *mut Env, buf: Oop) -> i64 {
    direct_buffer(buf).map_or(-1, |(_, c)| c as i64)
}

/// `JNILocalRefType` (1) for any non-null reference.
extern "C" fn get_object_ref_type(_env: *mut Env, o: Oop) -> i32 {
    (!o.is_null()) as i32
}

extern "C" fn is_virtual_thread(_env: *mut Env, _o: Oop) -> u8 {
    0
}
