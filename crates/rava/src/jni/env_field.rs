//! Field access from native code: a `jfieldID` names a resolved field (its holder, type and
//! offset, or its static slot), and `Get<Type>Field`/`Set<Type>Field` and their static forms
//! read and write it as `getfield`/`putfield` would.

use super::env::{c_str, klass, SIZE};
use super::{enter, refs, Env};
use crate::heap::{obj, Oop};
use crate::symbol::Symbol;
use crate::types::BasicType;
use std::ffi::c_char;
use std::sync::atomic::{AtomicU64, Ordering};

struct FieldId {
    ty: BasicType,
    offset: u32,
    slot: Option<&'static AtomicU64>,
}

/// The ID of field `f` declared by `holder`.
fn id_of(holder: &'static crate::klass::Klass, f: &crate::klass::FieldDesc) -> *const FieldId {
    let slot: Option<&'static AtomicU64> = f.is_static().then(|| &holder.statics[f.offset as usize]);
    Box::leak(Box::new(FieldId { ty: f.ty, offset: f.offset, slot }))
}

fn field_id(env: *mut Env, c: Oop, name: *const c_char, sig: *const c_char, statik: bool) -> *const FieldId {
    let (t, _g) = enter(env);
    let Some(k) = klass(c) else { return std::ptr::null() };
    if let Err(e) = crate::runtime::initialize(t, k) {
        t.jni.exception = e.0;
        return std::ptr::null();
    }
    let (n, s) = (Symbol::intern(c_str(name)), Symbol::intern(c_str(sig)));
    match crate::link::find_field(k, n, s).filter(|(_, f)| f.is_static() == statik) {
        Some((holder, f)) => id_of(holder, f),
        None => {
            t.jni.exception = crate::runtime::throw_new(t, "java/lang/NoSuchFieldError", n.as_str()).0;
            std::ptr::null()
        }
    }
}

extern "C" fn get_field_id(env: *mut Env, c: Oop, name: *const c_char, sig: *const c_char) -> *const FieldId {
    field_id(env, c, name, sig, false)
}

extern "C" fn get_static_field_id(env: *mut Env, c: Oop, name: *const c_char, sig: *const c_char) -> *const FieldId {
    field_id(env, c, name, sig, true)
}

/// The value of field `f` (of `o`, or static) as a slot value.
fn read(o: Oop, f: &FieldId) -> u64 {
    if let Some(slot) = f.slot {
        return slot.load(Ordering::SeqCst);
    }
    // SAFETY: a live object holding the field at its resolved offset.
    unsafe {
        match f.ty {
            BasicType::Reference => obj::read_ref(o, f.offset) as u64,
            BasicType::Boolean | BasicType::Byte => obj::read::<i8>(o, f.offset) as i64 as u64,
            BasicType::Char => obj::read::<u16>(o, f.offset) as u64,
            BasicType::Short => obj::read::<i16>(o, f.offset) as i64 as u64,
            BasicType::Int | BasicType::Float => obj::read::<u32>(o, f.offset) as u64,
            _ => obj::read::<u64>(o, f.offset),
        }
    }
}

fn write(o: Oop, f: &FieldId, v: u64) {
    if let Some(slot) = f.slot {
        return slot.store(v, Ordering::SeqCst);
    }
    // SAFETY: as `read`.
    unsafe {
        match f.ty {
            BasicType::Reference => obj::write_ref(o, f.offset, v as Oop),
            BasicType::Boolean | BasicType::Byte => obj::write::<u8>(o, f.offset, v as u8),
            BasicType::Char | BasicType::Short => obj::write::<u16>(o, f.offset, v as u16),
            BasicType::Int | BasicType::Float => obj::write::<u32>(o, f.offset, v as u32),
            _ => obj::write::<u64>(o, f.offset, v),
        }
    }
}

extern "C" fn get_object(env: *mut Env, o: Oop, f: *const FieldId) -> Oop {
    let (t, _g) = enter(env);
    // SAFETY: a field ID handed out by `field_id`.
    let v = read(o, unsafe { &*f }) as Oop;
    refs::local(t, v)
}

extern "C" fn set_object(_env: *mut Env, o: Oop, f: *const FieldId, v: Oop) {
    // SAFETY: as `get_object`.
    write(o, unsafe { &*f }, v as u64);
}

macro_rules! prim {
    ($get:ident, $set:ident, $ty:ty, $from:expr, $to:expr) => {
        extern "C" fn $get(_env: *mut Env, o: Oop, f: *const FieldId) -> $ty {
            // SAFETY: a field ID handed out by `field_id`.
            $from(read(o, unsafe { &*f }))
        }
        extern "C" fn $set(_env: *mut Env, o: Oop, f: *const FieldId, v: $ty) {
            // SAFETY: as above.
            write(o, unsafe { &*f }, $to(v))
        }
    };
}

prim!(get_z, set_z, u8, |v: u64| v as u8, |v: u8| (v != 0) as u64);
prim!(get_b, set_b, i8, |v: u64| v as i8, |v: i8| v as i64 as u64);
prim!(get_c, set_c, u16, |v: u64| v as u16, |v: u16| v as u64);
prim!(get_s, set_s, i16, |v: u64| v as i16, |v: i16| v as i64 as u64);
prim!(get_i, set_i, i32, |v: u64| v as i32, |v: i32| v as i64 as u64);
prim!(get_j, set_j, i64, |v: u64| v as i64, |v: i64| v as u64);
prim!(get_f, set_f, f32, |v: u64| f32::from_bits(v as u32), |v: f32| v.to_bits() as u64);
prim!(get_d, set_d, f64, f64::from_bits, f64::to_bits);

/// `FromReflectedField`: the ID of a `java.lang.reflect.Field`.
extern "C" fn from_reflected_field(_env: *mut Env, field: Oop) -> *const FieldId {
    let (k, i) = crate::natives::reflect::field_of(field);
    id_of(k, &k.fields[i])
}

pub(super) fn fill(t: &mut [usize; SIZE]) {
    t[8] = from_reflected_field as *const () as usize;
    t[94] = get_field_id as *const () as usize;
    t[144] = get_static_field_id as *const () as usize;
    let gets = [get_object as *const () as usize, get_z as *const () as usize, get_b as *const () as usize, get_c as *const () as usize, get_s as *const () as usize, get_i as *const () as usize, get_j as *const () as usize, get_f as *const () as usize, get_d as *const () as usize];
    let sets = [set_object as *const () as usize, set_z as *const () as usize, set_b as *const () as usize, set_c as *const () as usize, set_s as *const () as usize, set_i as *const () as usize, set_j as *const () as usize, set_f as *const () as usize, set_d as *const () as usize];
    // The static forms take the class where the instance forms take the object: the same
    // functions serve (a static field ID carries its slot).
    t[95..104].copy_from_slice(&gets);
    t[104..113].copy_from_slice(&sets);
    t[145..154].copy_from_slice(&gets);
    t[154..163].copy_from_slice(&sets);
}
