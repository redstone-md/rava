//! `java.lang.Class` mirrors. Each `Klass` gets one mirror, created on first request; the mirror
//! points back through the injected `klass` field. Primitive mirrors (`int.class`) have a null
//! `klass` and record their type in the injected `primitive_type` field.

use super::{alloc_instance, initialize, vm_field};
use crate::heap::{obj, Oop};
use crate::klass::{Klass, KlassKind};
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

vm_field!(klass_offset, "java/lang/Class", "klass", "J");
vm_field!(primitive_offset, "java/lang/Class", "primitive_type", "I");
vm_field!(loader_offset, "java/lang/Class", "classLoader", "Ljava/lang/ClassLoader;");
vm_field!(component_offset, "java/lang/Class", "componentType", "Ljava/lang/Class;");

pub fn mirror_of(t: &mut Thread, k: &'static Klass) -> JResult<Oop> {
    let existing = k.mirror();
    if !existing.is_null() {
        return Ok(existing);
    }
    let class_class = crate::loader::load("java/lang/Class").expect("java/lang/Class");
    initialize(t, class_class)?;
    let component = match k.kind {
        KlassKind::ObjArray { elem } => mirror_of(t, elem)?,
        KlassKind::TypeArray { elem } => primitive_mirror(t, elem)?,
        KlassKind::Instance => std::ptr::null_mut(),
    };
    let m = alloc_instance(t, class_class)?;
    // SAFETY: fresh Class instance; offsets from its layout (with injected fields).
    unsafe {
        obj::write::<u64>(m, klass_offset(), k as *const Klass as u64);
        obj::write_ref(m, loader_offset(), k.loader());
        obj::write_ref(m, component_offset(), component);
    }
    crate::natives::set_mirror_module(m, k);
    match k.mirror.compare_exchange(std::ptr::null_mut(), m, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            crate::gc::recent::note(m);
            Ok(m)
        }
        Err(winner) => Ok(winner),
    }
}

/// The `Klass` behind a mirror; `None` for primitive mirrors.
pub fn klass_of_mirror(mirror: Oop) -> Option<&'static Klass> {
    // SAFETY: `mirror` is a live java.lang.Class.
    let raw = unsafe { obj::read::<u64>(mirror, klass_offset()) };
    // SAFETY: the field only ever holds a leaked `&'static Klass` or 0.
    (raw != 0).then(|| unsafe { &*(raw as *const Klass) })
}

pub fn primitive_type_of_mirror(mirror: Oop) -> Option<BasicType> {
    if klass_of_mirror(mirror).is_some() {
        return None;
    }
    // SAFETY: live Class; the field stores `BasicType as i32 + 1`.
    let code = unsafe { obj::read::<i32>(mirror, primitive_offset()) };
    PRIMITIVES.iter().copied().find(|&b| b as i32 + 1 == code)
}

const PRIMITIVES: [BasicType; 9] = [
    BasicType::Boolean,
    BasicType::Char,
    BasicType::Float,
    BasicType::Double,
    BasicType::Byte,
    BasicType::Short,
    BasicType::Int,
    BasicType::Long,
    BasicType::Void,
];

static PRIMITIVE_MIRRORS: Mutex<[usize; 10]> = Mutex::new([0; 10]);

/// Every primitive mirror created so far.
pub fn primitive_mirrors() -> Vec<Oop> {
    PRIMITIVE_MIRRORS.lock().unwrap().iter().filter(|&&m| m != 0).map(|&m| m as Oop).collect()
}

pub fn primitive_mirror(t: &mut Thread, ty: BasicType) -> JResult<Oop> {
    const TABLE: &Mutex<[usize; 10]> = &PRIMITIVE_MIRRORS;
    if let Some(&m) = TABLE.lock().unwrap().get(ty as usize).filter(|&&m| m != 0) {
        return Ok(m as Oop);
    }
    let class_class = crate::loader::load("java/lang/Class").expect("java/lang/Class");
    initialize(t, class_class)?;
    let m = alloc_instance(t, class_class)?;
    // SAFETY: fresh Class instance.
    unsafe { obj::write::<i32>(m, primitive_offset(), ty as i32 + 1) };
    crate::natives::set_primitive_mirror_module(m);
    let mut table = TABLE.lock().unwrap();
    if table[ty as usize] == 0 {
        table[ty as usize] = m as usize;
    }
    Ok(table[ty as usize] as Oop)
}

/// Primitive type named as `Class.getPrimitiveClass` receives it (`"int"`, `"void"`, ...).
pub fn primitive_by_name(name: &str) -> Option<BasicType> {
    Some(match name {
        "boolean" => BasicType::Boolean,
        "char" => BasicType::Char,
        "float" => BasicType::Float,
        "double" => BasicType::Double,
        "byte" => BasicType::Byte,
        "short" => BasicType::Short,
        "int" => BasicType::Int,
        "long" => BasicType::Long,
        "void" => BasicType::Void,
        _ => return None,
    })
}
