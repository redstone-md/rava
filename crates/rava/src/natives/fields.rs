//! Writing a Java object's field by its class and name, for natives that fill result objects
//! the class library passes in (file attributes, volume information).

use crate::heap::{obj, Oop};
use crate::symbol::Symbol;

fn offset(class: &str, field: &str, desc: &str) -> u32 {
    let k = crate::loader::load(class).expect("a class library class");
    k.declared_field(Symbol::intern(field), Symbol::intern(desc)).unwrap_or_else(|| panic!("{class}.{field}")).offset
}

pub(crate) fn set_long(o: Oop, class: &str, field: &str, v: i64) {
    // SAFETY: a live instance of `class`.
    unsafe { obj::write::<i64>(o, offset(class, field, "J"), v) };
}

pub(crate) fn set_int(o: Oop, class: &str, field: &str, v: i32) {
    // SAFETY: a live instance of `class`.
    unsafe { obj::write::<i32>(o, offset(class, field, "I"), v) };
}

/// A `String` field (`v` may be null).
#[cfg(windows)]
pub(crate) fn set_string(o: Oop, class: &str, field: &str, v: Oop) {
    // SAFETY: a live instance of `class`.
    unsafe { obj::write_ref(o, offset(class, field, "Ljava/lang/String;"), v) };
}
