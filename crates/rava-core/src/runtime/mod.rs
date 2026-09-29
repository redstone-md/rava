//! Runtime services shared by the interpreter, natives and (later) compiled code.

mod init;
mod mirror;
mod strings;

pub use init::initialize;
pub use mirror::{klass_of_mirror, mirror_of, primitive_by_name, primitive_mirror, primitive_type_of_mirror};
pub use strings::{intern, intern_modified_utf8 as intern_literal, java_string_to_rust, new_string, new_string_utf16, string_units};

use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::link::LinkError;
use crate::loader::LoadError;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread, Throw};
use crate::vm::vm;
use std::sync::OnceLock;

/// Byte offset of instance field `name:desc` declared by `class` (or a superclass), resolved once.
pub(crate) fn field_offset(cache: &OnceLock<u32>, class: &str, name: &str, desc: &str) -> u32 {
    *cache.get_or_init(|| {
        let k = crate::loader::load(class).unwrap_or_else(|e| panic!("VM class {class}: {e:?}"));
        let (_, f) = crate::link::find_field(k, Symbol::intern(name), Symbol::intern(desc))
            .unwrap_or_else(|| panic!("VM field {class}.{name}:{desc}"));
        f.offset
    })
}

/// Declares a cached accessor for a VM-known instance field offset.
macro_rules! vm_field {
    ($fn:ident, $class:literal, $name:literal, $desc:literal) => {
        pub(crate) fn $fn() -> u32 {
            static CELL: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            $crate::runtime::field_offset(&CELL, $class, $name, $desc)
        }
    };
}
pub(crate) use vm_field;

pub fn alloc_instance(t: &mut Thread, k: &'static Klass) -> JResult<Oop> {
    match vm().heap.alloc_instance(k) {
        Some(o) => Ok(o),
        None => Err(out_of_memory(t)),
    }
}

pub fn alloc_array(t: &mut Thread, k: &'static Klass, length: i32) -> JResult<Oop> {
    if length < 0 {
        return Err(throw_new(t, "java/lang/NegativeArraySizeException", &length.to_string()));
    }
    match vm().heap.alloc_array(k, k.elem_size(), length) {
        Some(o) => Ok(o),
        None => Err(out_of_memory(t)),
    }
}

fn out_of_memory(t: &mut Thread) -> Throw {
    throw_new(t, "java/lang/OutOfMemoryError", "Java heap space")
}

/// Creates and returns a new exception of `class` with `message` (JVMS "throws X").
/// If constructing it throws, that exception propagates instead.
pub fn throw_new(t: &mut Thread, class: &str, message: &str) -> Throw {
    match construct_throwable(t, class, message) {
        Ok(ex) => Throw(ex),
        Err(e) => e,
    }
}

fn construct_throwable(t: &mut Thread, class: &str, message: &str) -> JResult<Oop> {
    let k = crate::loader::load(class).map_err(|e| fatal(&format!("cannot load {class}: {e:?}")))?;
    initialize(t, k)?;
    let ex = alloc_instance(t, k)?;
    let msg = if message.is_empty() { std::ptr::null_mut() } else { new_string(t, message)? };
    let ctor = k
        .declared_method(Symbol::intern("<init>"), Symbol::intern("(Ljava/lang/String;)V"))
        .ok_or_else(|| fatal(&format!("{class} has no (String) constructor")))?;
    t.call(ctor, &[ex as u64, msg as u64])?;
    Ok(ex)
}

/// A VM invariant broke while the Java world cannot report it; abort loudly.
pub fn fatal(msg: &str) -> Throw {
    panic!("rava-core fatal: {msg}")
}

pub fn link_error(t: &mut Thread, e: LinkError) -> Throw {
    match e {
        LinkError::Load(LoadError { kind, message }) => throw_new(t, kind, &message),
        LinkError::Error(kind, message) => throw_new(t, kind, &message),
    }
}

pub fn null_pointer(t: &mut Thread) -> Throw {
    throw_new(t, "java/lang/NullPointerException", "")
}

/// Class name of an object, dotted (for messages).
pub fn class_name_of(o: Oop) -> String {
    // SAFETY: callers pass live non-null objects.
    unsafe { obj::klass(o) }.name.as_str().replace('/', ".")
}
