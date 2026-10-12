//! Shared libraries loaded through `System.load`/`loadLibrary`, and the binding of native
//! methods to their symbols (JNI spec, "Resolving Native Method Names"): `Java_` and the
//! mangled class and method names, then, for an overloaded method, `__` and the mangled
//! argument descriptor. `RegisterNatives` binds a method directly.

use crate::method::Method;
use crate::thread::{JResult, Thread};
use std::ffi::CString;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

/// Handles of the libraries loaded so far, in load order.
fn loaded() -> &'static Mutex<Vec<usize>> {
    static L: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    &L
}

/// Opens the library at `path` and runs its `JNI_OnLoad`. Returns the handle and the JNI
/// version it asked for, or the loader's error.
pub(crate) fn load(t: &mut Thread, path: &str) -> Result<(usize, i32), String> {
    let c = CString::new(path).map_err(|e| e.to_string())?;
    // SAFETY: a NUL-terminated path; the handle is kept for the life of the VM.
    let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY) } as usize;
    if h == 0 {
        // SAFETY: dlerror's message after a failed dlopen.
        let msg = unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) };
        return Err(msg.to_string_lossy().into_owned());
    }
    {
        let mut l = loaded().lock().unwrap();
        if l.contains(&h) {
            return Ok((h, super::VERSION));
        }
        l.push(h);
    }
    let on_load = symbol(h, "JNI_OnLoad");
    if on_load == 0 {
        return Ok((h, 0x0001_0001));
    }
    let mut c = super::call::CArgs::default();
    c.int(super::vm_iface::java_vm() as u64);
    c.int(0);
    // SAFETY: `jint JNI_OnLoad(JavaVM *, void *)`.
    let (rax, _) = crate::gc::safepoint::blocking(t, || unsafe { c.call(on_load) });
    if let Some(e) = super::refs::take_exception(t) {
        t.jni.exception = e;
    }
    Ok((h, rax as i32))
}

/// The address of `name` in library `h` (0 if absent).
pub(crate) fn symbol(h: usize, name: &str) -> usize {
    let Ok(c) = CString::new(name) else { return 0 };
    // SAFETY: a handle from dlopen and a NUL-terminated name.
    unsafe { libc::dlsym(h as *mut libc::c_void, c.as_ptr()) as usize }
}

/// The native code bound to `m`: registered, else found by its JNI names in a loaded library.
pub(crate) fn entry(m: &'static Method) -> Option<usize> {
    let bound = m.jni_entry.load(Ordering::Acquire);
    if bound != 0 {
        return Some(bound);
    }
    let short = format!("Java_{}_{}", mangle(m.holder.name.as_str()), mangle(m.name.as_str()));
    let args = m.desc.as_str().strip_prefix('(').and_then(|d| d.split(')').next()).unwrap_or("");
    let long = format!("{short}__{}", mangle(args));
    let libs = loaded().lock().unwrap().clone();
    let f = libs.iter().find_map(|&h| [symbol(h, &short), symbol(h, &long)].into_iter().find(|&a| a != 0))?;
    m.jni_entry.store(f, Ordering::Release);
    Some(f)
}

/// Binds `m` to `f` (`RegisterNatives`), or unbinds it with 0.
pub(crate) fn register(m: &'static Method, f: usize) {
    m.jni_entry.store(f, Ordering::Release);
}

/// JNI name mangling: `/` becomes `_`, and `_`, `;`, `[` and non-ASCII characters escapes.
fn mangle(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '/' => out.push('_'),
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("_0{:04x}", u));
                }
            }
        }
    }
    out
}

/// `NativeLibraries.load` for a library the VM does not supply: loads it, recording its
/// handle and JNI version in the `NativeLibraryImpl`; false (or UnsatisfiedLinkError, when
/// asked to throw) if it cannot be opened.
pub(crate) fn load_for_java(t: &mut Thread, path: &str, throw: bool) -> JResult<Option<(usize, i32)>> {
    match load(t, path) {
        Ok(r) => Ok(Some(r)),
        Err(msg) if throw => Err(crate::runtime::throw_new(t, "java/lang/UnsatisfiedLinkError", &format!("Can't load library: {path}: {msg}"))),
        Err(_) => Ok(None),
    }
}
