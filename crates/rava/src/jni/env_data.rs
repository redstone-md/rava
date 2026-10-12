//! Strings and arrays for native code. Element pointers (`Get<Type>ArrayElements`,
//! `GetPrimitiveArrayCritical`) point into the array itself, which a local reference keeps in
//! place; UTF-8 and UTF-16 copies of strings are `malloc`ed and freed on release.

use super::env::{or_pending, SIZE};
use super::{enter, refs, Env};
use crate::heap::{obj, Oop};
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use std::ffi::c_char;

fn link(t: &mut Thread, r: crate::loader::LoadResult) -> JResult<&'static crate::klass::Klass> {
    r.map_err(|e| crate::runtime::link_error(t, e.into()))
}

/// `s` in modified UTF-8 (JVMS 4.4.7): NUL and supplementary characters' surrogates encoded
/// as Java's class files do.
fn modified_utf8(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len());
    for &u in units {
        match u {
            0x0001..=0x007f => out.push(u as u8),
            0x0000 | 0x0080..=0x07ff => out.extend([0xc0 | (u >> 6) as u8, 0x80 | (u & 0x3f) as u8]),
            _ => out.extend([0xe0 | (u >> 12) as u8, 0x80 | ((u >> 6) & 0x3f) as u8, 0x80 | (u & 0x3f) as u8]),
        }
    }
    out
}

/// A `malloc`ed copy of `bytes` followed by `nul` zero bytes.
fn c_copy<T: Copy>(data: &[T], nul: usize) -> *mut T {
    let size = std::mem::size_of::<T>();
    // SAFETY: a fresh allocation of the copied length plus the terminator.
    unsafe {
        let p = libc::malloc((data.len() + nul) * size + 1) as *mut T;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        std::ptr::write_bytes((p as *mut u8).add(data.len() * size), 0, nul * size);
        p
    }
}

extern "C" fn new_string(env: *mut Env, chars: *const u16, len: i32) -> Oop {
    let (t, _g) = enter(env);
    // SAFETY: the caller's `len` UTF-16 units.
    let units = unsafe { std::slice::from_raw_parts(chars, len.max(0) as usize) };
    let r = crate::runtime::new_string_utf16(t, units);
    let s = or_pending(t, r);
    refs::local(t, s)
}

extern "C" fn get_string_length(_env: *mut Env, s: Oop) -> i32 {
    crate::runtime::string_units(s).len() as i32
}

extern "C" fn get_string_chars(_env: *mut Env, s: Oop, is_copy: *mut u8) -> *const u16 {
    if !is_copy.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *is_copy = 1 };
    }
    c_copy(&crate::runtime::string_units(s), 1)
}

extern "C" fn release_chars(_env: *mut Env, _s: Oop, p: *mut libc::c_void) {
    // SAFETY: a copy `get_string_chars`/`get_string_utf_chars` malloc'ed.
    unsafe { libc::free(p) };
}

extern "C" fn new_string_utf(env: *mut Env, bytes: *const c_char) -> Oop {
    let (t, _g) = enter(env);
    if bytes.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a NUL-terminated modified UTF-8 string.
    let b = unsafe { std::ffi::CStr::from_ptr(bytes) }.to_bytes();
    let units = crate::symbol::decode_modified_utf8(b);
    let r = crate::runtime::new_string_utf16(t, &units);
    let s = or_pending(t, r);
    refs::local(t, s)
}

extern "C" fn get_string_utf_length(_env: *mut Env, s: Oop) -> i32 {
    modified_utf8(&crate::runtime::string_units(s)).len() as i32
}

extern "C" fn get_string_utf_chars(_env: *mut Env, s: Oop, is_copy: *mut u8) -> *const c_char {
    if !is_copy.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *is_copy = 1 };
    }
    c_copy(&modified_utf8(&crate::runtime::string_units(s)), 1) as *const c_char
}

extern "C" fn get_string_region(_env: *mut Env, s: Oop, start: i32, len: i32, buf: *mut u16) {
    let u = crate::runtime::string_units(s);
    let (a, n) = (start.max(0) as usize, len.max(0) as usize);
    if let Some(src) = u.get(a..a + n) {
        // SAFETY: the caller's buffer of `len` units.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), buf, n) };
    }
}

extern "C" fn get_string_utf_region(_env: *mut Env, s: Oop, start: i32, len: i32, buf: *mut u8) {
    let u = crate::runtime::string_units(s);
    let (a, n) = (start.max(0) as usize, len.max(0) as usize);
    if let Some(src) = u.get(a..a + n) {
        let b = modified_utf8(src);
        // SAFETY: the caller's buffer, large enough for the region's encoding and a NUL.
        unsafe {
            std::ptr::copy_nonoverlapping(b.as_ptr(), buf, b.len());
            *buf.add(b.len()) = 0;
        }
    }
}

extern "C" fn get_array_length(_env: *mut Env, a: Oop) -> i32 {
    // SAFETY: a live array.
    unsafe { obj::array_length(a) }
}

extern "C" fn new_object_array(env: *mut Env, n: i32, elem: Oop, init: Oop) -> Oop {
    let (t, _g) = enter(env);
    let r = (|| {
        let Some(k) = super::env::klass(elem) else { return Ok(std::ptr::null_mut()) };
        let ak = link(t, crate::vm::vm().array_class_of(k))?;
        let a = crate::runtime::alloc_array(t, ak, n)?;
        if !init.is_null() {
            // SAFETY: the new array of `n` references.
            (0..n as usize).for_each(|i| unsafe { obj::store_elem(a, i, init) });
        }
        Ok(a)
    })();
    let a = or_pending(t, r);
    refs::local(t, a)
}

extern "C" fn get_object_array_element(env: *mut Env, a: Oop, i: i32) -> Oop {
    let (t, _g) = enter(env);
    // SAFETY: a live object array.
    if i < 0 || i >= unsafe { obj::array_length(a) } {
        t.jni.exception = crate::runtime::throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &i.to_string()).0;
        return std::ptr::null_mut();
    }
    // SAFETY: an index within bounds.
    let o = unsafe { obj::load_elem(a, i as usize) };
    refs::local(t, o)
}

extern "C" fn set_object_array_element(env: *mut Env, a: Oop, i: i32, v: Oop) {
    let (t, _g) = enter(env);
    // SAFETY: a live object array.
    if i < 0 || i >= unsafe { obj::array_length(a) } {
        t.jni.exception = crate::runtime::throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &i.to_string()).0;
        return;
    }
    // SAFETY: an index within bounds.
    unsafe { obj::store_elem(a, i as usize, v) };
}

fn new_array(env: *mut Env, ty: BasicType, n: i32) -> Oop {
    let (t, _g) = enter(env);
    let r = (|| {
        let k = link(t, crate::vm::vm().primitive_array_class(ty))?;
        crate::runtime::alloc_array(t, k, n)
    })();
    let a = or_pending(t, r);
    refs::local(t, a)
}

/// Element pointer of a primitive array, kept in place by a local reference while held.
extern "C" fn get_elements(env: *mut Env, a: Oop, is_copy: *mut u8) -> *mut u8 {
    let (t, _g) = enter(env);
    if !is_copy.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *is_copy = 0 };
    }
    refs::local(t, a);
    // SAFETY: a live array.
    unsafe { obj::array_data::<u8>(a) }
}

extern "C" fn release_elements(_env: *mut Env, _a: Oop, _p: *mut u8, _mode: i32) {}

/// `Get<Type>ArrayRegion` / `Set<Type>ArrayRegion` for elements of `size` bytes.
fn region(env: *mut Env, a: Oop, start: i32, len: i32, buf: *mut u8, size: usize, set: bool) {
    let (t, _g) = enter(env);
    // SAFETY: a live array.
    let n = unsafe { obj::array_length(a) };
    if start < 0 || len < 0 || start as i64 + len as i64 > n as i64 {
        t.jni.exception = crate::runtime::throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &format!("{start}+{len} of {n}")).0;
        return;
    }
    // SAFETY: a range within the array and the caller's buffer of `len` elements.
    unsafe {
        let at = obj::array_data::<u8>(a).add(start as usize * size);
        if set {
            std::ptr::copy_nonoverlapping(buf, at, len as usize * size);
        } else {
            std::ptr::copy_nonoverlapping(at, buf, len as usize * size);
        }
    }
}

macro_rules! prim_array {
    ($new:ident, $get:ident, $set:ident, $ty:expr, $size:literal) => {
        extern "C" fn $new(env: *mut Env, n: i32) -> Oop {
            new_array(env, $ty, n)
        }
        extern "C" fn $get(env: *mut Env, a: Oop, start: i32, len: i32, buf: *mut u8) {
            region(env, a, start, len, buf, $size, false)
        }
        extern "C" fn $set(env: *mut Env, a: Oop, start: i32, len: i32, buf: *mut u8) {
            region(env, a, start, len, buf, $size, true)
        }
    };
}

prim_array!(new_z, get_z, set_z, BasicType::Boolean, 1);
prim_array!(new_b, get_b, set_b, BasicType::Byte, 1);
prim_array!(new_c, get_c, set_c, BasicType::Char, 2);
prim_array!(new_s, get_s, set_s, BasicType::Short, 2);
prim_array!(new_i, get_i, set_i, BasicType::Int, 4);
prim_array!(new_j, get_j, set_j, BasicType::Long, 8);
prim_array!(new_f, get_f, set_f, BasicType::Float, 4);
prim_array!(new_d, get_d, set_d, BasicType::Double, 8);

pub(super) fn fill(t: &mut [usize; SIZE]) {
    t[163] = new_string as *const () as usize;
    t[164] = get_string_length as *const () as usize;
    t[165] = get_string_chars as *const () as usize;
    t[166] = release_chars as *const () as usize;
    t[167] = new_string_utf as *const () as usize;
    t[168] = get_string_utf_length as *const () as usize;
    t[169] = get_string_utf_chars as *const () as usize;
    t[170] = release_chars as *const () as usize;
    t[171] = get_array_length as *const () as usize;
    t[172] = new_object_array as *const () as usize;
    t[173] = get_object_array_element as *const () as usize;
    t[174] = set_object_array_element as *const () as usize;
    t[175..183].copy_from_slice(&[new_z as *const () as usize, new_b as *const () as usize, new_c as *const () as usize, new_s as *const () as usize, new_i as *const () as usize, new_j as *const () as usize, new_f as *const () as usize, new_d as *const () as usize]);
    t[183..191].fill(get_elements as *const () as usize);
    t[191..199].fill(release_elements as *const () as usize);
    t[199..207].copy_from_slice(&[get_z as *const () as usize, get_b as *const () as usize, get_c as *const () as usize, get_s as *const () as usize, get_i as *const () as usize, get_j as *const () as usize, get_f as *const () as usize, get_d as *const () as usize]);
    t[207..215].copy_from_slice(&[set_z as *const () as usize, set_b as *const () as usize, set_c as *const () as usize, set_s as *const () as usize, set_i as *const () as usize, set_j as *const () as usize, set_f as *const () as usize, set_d as *const () as usize]);
    t[220] = get_string_region as *const () as usize;
    t[221] = get_string_utf_region as *const () as usize;
    t[222] = get_elements as *const () as usize;
    t[223] = release_elements as *const () as usize;
    t[224] = get_string_chars as *const () as usize;
    t[225] = release_chars as *const () as usize;
}
