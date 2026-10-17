//! Shared libraries loaded through `System.load`/`loadLibrary`, and the binding of native
//! methods to their symbols (JNI spec, "Resolving Native Method Names"): `Java_` and the
//! mangled class and method names, then, for an overloaded method, `__` and the mangled
//! argument descriptor. `RegisterNatives` binds a method directly.

use crate::klass::Klass;
use crate::method::Method;
use crate::thread::{JResult, Thread};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

/// The libraries loaded so far, in load order: each handle with the class that loaded it,
/// which names the class loader whose native methods it serves (the loader itself moves with
/// the heap; the class's reference to it is kept current).
fn loaded() -> &'static Mutex<Vec<(Option<&'static Klass>, usize)>> {
    static L: Mutex<Vec<(Option<&'static Klass>, usize)>> = Mutex::new(Vec::new());
    &L
}

/// Opens the library at `path` for the class loader of `from` and runs its `JNI_OnLoad`.
/// Returns the handle and the JNI version it asked for, or the loader's error.
pub(crate) fn load(t: &mut Thread, path: &str, from: Option<&'static Klass>) -> Result<(usize, i32), String> {
    // The handle is kept for the life of the VM.
    let h = crate::os::library_open(path)?;
    {
        let mut l = loaded().lock().unwrap();
        let seen = l.iter().any(|&(_, x)| x == h);
        let loader = from.map_or(std::ptr::null_mut(), |k| k.loader());
        if !l.iter().any(|&(k, x)| x == h && k.map_or(std::ptr::null_mut(), |k| k.loader()) == loader) {
            l.push((from, h));
        }
        if seen {
            return Ok((h, super::VERSION));
        }
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
    crate::os::library_symbol(h, name)
}

/// The native code bound to `m`: registered, else found by its JNI names in a loaded library.
pub(crate) fn entry(m: &'static Method) -> Option<usize> {
    let bound = m.jit_if().map_or(0, |j| j.jni_entry.load(Ordering::Acquire));
    if bound != 0 {
        return Some(bound);
    }
    let short = format!("Java_{}_{}", mangle(m.holder.name.as_str()), mangle(m.name.as_str()));
    let args = m.desc.as_str().strip_prefix('(').and_then(|d| d.split(')').next()).unwrap_or("");
    let long = format!("{short}__{}", mangle(args));
    // The libraries of the method's own class loader first (JNI: a class loader's libraries
    // serve its classes; two loaders' copies of a library each bind to their own), then any.
    let loader = m.holder.loader();
    let libs: Vec<(bool, usize)> = loaded().lock().unwrap().iter().map(|&(k, h)| (k.map_or(std::ptr::null_mut(), |k| k.loader()) == loader, h)).collect();
    let find = |own: bool| libs.iter().filter(|l| l.0 == own).find_map(|&(_, h)| [symbol(h, &short), symbol(h, &long)].into_iter().find(|&a| a != 0));
    let f = find(true).or_else(|| find(false))?;
    m.jit().jni_entry.store(f, Ordering::Release);
    super::stub::bind(m, f);
    Some(f)
}

/// Binds `m` to `f` (`RegisterNatives`), or unbinds it with 0.
pub(crate) fn register(m: &'static Method, f: usize) {
    m.jit().jni_entry.store(f, Ordering::Release);
    super::stub::bind(m, f);
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
pub(crate) fn load_for_java(t: &mut Thread, path: &str, from: Option<&'static Klass>, throw: bool) -> JResult<Option<(usize, i32)>> {
    match load(t, path, from) {
        Ok(r) => Ok(Some(r)),
        Err(msg) if throw => Err(crate::runtime::throw_new(t, "java/lang/UnsatisfiedLinkError", &format!("Can't load library: {path}: {msg}"))),
        Err(_) => Ok(None),
    }
}
