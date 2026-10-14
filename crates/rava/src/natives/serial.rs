//! `java.io.ObjectStreamClass` natives, which serialization needs before its first object.

use super::{ret_bool, ret_void, NativeRegistry};
use crate::heap::Oop;
use crate::runtime as rt;
use crate::symbol::Symbol;

const C: &str = "java/io/ObjectStreamClass";

pub(super) fn register(r: &mut NativeRegistry) {
    // HotSpot caches method ids here; nothing to cache.
    r.add(C, "initNative", "()V", |_, _| ret_void());
    // Whether the class itself declares a static initializer, which the default
    // serialVersionUID hashes in.
    r.add(C, "hasStaticInitializer", "(Ljava/lang/Class;)Z", |_, a| {
        let declares = rt::klass_of_mirror(a[0] as Oop).is_some_and(|k| k.declared_method(Symbol::intern("<clinit>"), Symbol::intern("()V")).is_some());
        ret_bool(declares)
    });
}
