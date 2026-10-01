//! `System.arraycopy`, with HotSpot's checks in HotSpot's order and its exception messages
//! (`TypeArrayKlass::copy_array` / `ObjArrayKlass::copy_array`), which programs can observe.

use super::lang::primitive_name;
use super::ret_void;
use crate::heap::{obj, Oop};
use crate::klass::{Klass, KlassKind};
use crate::runtime::{null_pointer, throw_new};
use crate::thread::{JResult, Thread};

/// How a message names an array's elements: `int` for primitive arrays, `object array` otherwise.
fn elements(k: &Klass) -> String {
    match k.kind {
        KlassKind::TypeArray { elem } => primitive_name(Some(elem)),
        _ => "object array".into(),
    }
}

fn store_error(t: &mut Thread, msg: String) -> crate::thread::Throw {
    throw_new(t, "java/lang/ArrayStoreException", &msg)
}

fn bounds_error(t: &mut Thread, msg: String) -> crate::thread::Throw {
    throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &msg)
}

pub(super) fn arraycopy(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (src, sp, dst, dp, len) = (a[0] as Oop, a[1] as i32, a[2] as Oop, a[3] as i32, a[4] as i32);
    if src.is_null() || dst.is_null() {
        return Err(null_pointer(t));
    }
    // SAFETY: both are live objects.
    let (sk, dk) = unsafe { (obj::klass(src), obj::klass(dst)) };
    let is_array = |k: &Klass| matches!(k.kind, KlassKind::TypeArray { .. } | KlassKind::ObjArray { .. });
    if !is_array(sk) {
        return Err(store_error(t, format!("arraycopy: source type {} is not an array", sk.java_name())));
    }
    if !is_array(dk) {
        return Err(store_error(t, format!("arraycopy: destination type {} is not an array", dk.java_name())));
    }
    let same_kind = match (&sk.kind, &dk.kind) {
        (KlassKind::TypeArray { elem: x }, KlassKind::TypeArray { elem: y }) => x == y,
        (KlassKind::ObjArray { .. }, KlassKind::ObjArray { .. }) => true,
        _ => false,
    };
    if !same_kind {
        return Err(store_error(t, format!("arraycopy: type mismatch: can not copy {}[] into {}[]", elements(sk), elements(dk))));
    }
    // SAFETY: both are arrays.
    let (sl, dl) = unsafe { (obj::array_length(src), obj::array_length(dst)) };
    if sp < 0 {
        return Err(bounds_error(t, format!("arraycopy: source index {sp} out of bounds for {}[{sl}]", elements(sk))));
    }
    if dp < 0 {
        return Err(bounds_error(t, format!("arraycopy: destination index {dp} out of bounds for {}[{dl}]", elements(dk))));
    }
    if len < 0 {
        return Err(bounds_error(t, format!("arraycopy: length {len} is negative")));
    }
    if sp as u32 + len as u32 > sl as u32 {
        return Err(bounds_error(t, format!("arraycopy: last source index {} out of bounds for {}[{sl}]", sp as u32 + len as u32, elements(sk))));
    }
    if dp as u32 + len as u32 > dl as u32 {
        return Err(bounds_error(t, format!("arraycopy: last destination index {} out of bounds for {}[{dl}]", dp as u32 + len as u32, elements(dk))));
    }
    if let (KlassKind::ObjArray { elem: se }, KlassKind::ObjArray { elem: de }) = (&sk.kind, &dk.kind) {
        if !sk.is_subtype_of(dk) {
            // Element-wise store check; copies the prefix that fits, then throws.
            for i in 0..len as usize {
                // SAFETY: indices within the checked bounds.
                let v = unsafe { *obj::array_data::<Oop>(src).add(sp as usize + i) };
                if !v.is_null() && !unsafe { obj::klass(v) }.is_subtype_of(de) {
                    let msg = format!(
                        "arraycopy: element type mismatch: can not cast one of the elements of {}[] to the type of the destination array, {}",
                        se.java_name(),
                        de.java_name()
                    );
                    return Err(store_error(t, msg));
                }
                // SAFETY: as above.
                unsafe { obj::store_elem(dst, dp as usize + i, v) };
            }
            return ret_void();
        }
    }
    let es = sk.elem_size() as usize;
    // SAFETY: checked bounds; `copy` handles overlap (same array).
    unsafe {
        std::ptr::copy(
            obj::array_data::<u8>(src).add(sp as usize * es),
            obj::array_data::<u8>(dst).add(dp as usize * es),
            len as usize * es,
        )
    };
    if matches!(dk.kind, KlassKind::ObjArray { .. }) {
        // SAFETY: as above.
        crate::gc::cards::dirty_range(unsafe { obj::array_data::<u8>(dst).add(dp as usize * es) } as usize, len as usize * es);
    }
    ret_void()
}
