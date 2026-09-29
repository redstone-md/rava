//! `java.lang.reflect.Array` natives.

use super::{ret_int, ret_ref, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::interp::{read_field, write_field};
use crate::klass::{Klass, KlassKind};
use crate::runtime::{self as rt, throw_new};
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use crate::vm::vm;

const A: &str = "java/lang/reflect/Array";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(A, "newArray", "(Ljava/lang/Class;I)Ljava/lang/Object;", |t, a| {
        let k = array_class_for(t, a[0] as Oop)?;
        ret_ref(rt::alloc_array(t, k, a[1] as i32)?)
    });
    r.add(A, "multiNewArray", "(Ljava/lang/Class;[I)Ljava/lang/Object;", |t, a| {
        // SAFETY: int[] argument.
        let dims: Vec<i32> = unsafe { obj::array_slice::<i32>(a[1] as Oop) }.to_vec();
        if dims.is_empty() {
            return Err(throw_new(t, "java/lang/IllegalArgumentException", "empty dimensions"));
        }
        let mut k = array_class_for(t, a[0] as Oop)?;
        for _ in 1..dims.len() {
            k = vm().array_class_of(k).map_err(|e| rt::link_error(t, e.into()))?;
        }
        ret_ref(build(t, k, &dims)?)
    });
    r.add(A, "getLength", "(Ljava/lang/Object;)I", |t, a| ret_int(array_len(t, a[0] as Oop)?));
    r.add(A, "get", "(Ljava/lang/Object;I)Ljava/lang/Object;", |t, a| {
        let (arr, i, elem) = element(t, a[0] as Oop, a[1] as i32)?;
        let v = unsafe { read_field(arr, elem_offset(elem, i), elem) };
        ret_ref(rt::box_value(t, elem, v)?)
    });
    r.add(A, "set", "(Ljava/lang/Object;ILjava/lang/Object;)V", |t, a| {
        let (arr, i, elem) = element(t, a[0] as Oop, a[1] as i32)?;
        let value = a[2] as Oop;
        if elem == BasicType::Reference {
            // SAFETY: object array.
            let KlassKind::ObjArray { elem: ek } = unsafe { obj::klass(arr) }.kind else { unreachable!() };
            if !value.is_null() && !unsafe { obj::klass(value) }.is_subtype_of(ek) {
                return Err(throw_new(t, "java/lang/IllegalArgumentException", "array element type mismatch"));
            }
        }
        let v = rt::unbox_value(t, value, elem)?;
        unsafe { write_field(arr, elem_offset(elem, i), elem, v) };
        Ok(0)
    });
}

/// The array class whose component is the given mirror.
fn array_class_for(t: &mut Thread, component: Oop) -> JResult<&'static Klass> {
    if component.is_null() {
        return Err(rt::null_pointer(t));
    }
    match rt::klass_of_mirror(component) {
        Some(k) => vm().array_class_of(k).map_err(|e| rt::link_error(t, e.into())),
        None => match rt::primitive_type_of_mirror(component) {
            Some(BasicType::Void) | None => Err(throw_new(t, "java/lang/IllegalArgumentException", "void")),
            Some(p) => vm().primitive_array_class(p).map_err(|e| rt::link_error(t, e.into())),
        },
    }
}

fn build(t: &mut Thread, k: &'static Klass, dims: &[i32]) -> JResult<Oop> {
    let arr = rt::alloc_array(t, k, dims[0])?;
    if let (KlassKind::ObjArray { elem }, true) = (&k.kind, dims.len() > 1) {
        for i in 0..dims[0] as usize {
            let sub = build(t, elem, &dims[1..])?;
            // SAFETY: `arr` has dims[0] reference elements.
            unsafe { *obj::array_data::<Oop>(arr).add(i) = sub };
        }
    }
    Ok(arr)
}

fn array_len(t: &mut Thread, arr: Oop) -> JResult<i32> {
    if arr.is_null() {
        return Err(rt::null_pointer(t));
    }
    // SAFETY: live object.
    if !unsafe { obj::klass(arr) }.is_array() {
        return Err(throw_new(t, "java/lang/IllegalArgumentException", "Argument is not an array"));
    }
    Ok(unsafe { obj::array_length(arr) })
}

fn element(t: &mut Thread, arr: Oop, i: i32) -> JResult<(Oop, i32, BasicType)> {
    let len = array_len(t, arr)?;
    if i < 0 || i >= len {
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", &format!("Index {i} out of bounds for length {len}")));
    }
    // SAFETY: live array.
    let elem = match unsafe { obj::klass(arr) }.kind {
        KlassKind::TypeArray { elem } => elem,
        _ => BasicType::Reference,
    };
    Ok((arr, i, elem))
}

fn elem_offset(elem: BasicType, i: i32) -> u32 {
    crate::heap::ARRAY_BASE_OFFSET + elem.size() * i as u32
}
