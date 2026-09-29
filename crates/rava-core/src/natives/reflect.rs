//! Core reflection natives (`jdk.internal.reflect`, `java.lang.reflect` identity).

use super::{ret_int, ret_ref, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};

vm_field!(field_clazz, "java/lang/reflect/Field", "clazz", "Ljava/lang/Class;");
vm_field!(field_slot, "java/lang/reflect/Field", "slot", "I");

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("jdk/internal/reflect/Reflection", "getCallerClass", "()Ljava/lang/Class;", caller_class);
    r.add("jdk/internal/reflect/Reflection", "getClassAccessFlags", "(Ljava/lang/Class;)I", |_, a| {
        ret_int(rt::klass_of_mirror(a[0] as Oop).map_or(0x0411, |k| k.access as i32))
    });
}

/// The declaring class and index into `Klass::fields` of a `java.lang.reflect.Field`.
pub(crate) fn field_of(field: Oop) -> (&'static Klass, usize) {
    // SAFETY: live Field object created by the VM's reflection natives.
    unsafe {
        let clazz = obj::read::<Oop>(field, field_clazz());
        let k = rt::klass_of_mirror(clazz).expect("Field.clazz of a primitive");
        (k, obj::read::<i32>(field, field_slot()) as usize)
    }
}

/// `Reflection.getCallerClass`: the class of the method that called the caller-sensitive method
/// (frame 0 is this native, 1 the @CallerSensitive method, 2 its caller), skipping reflection
/// and method-handle plumbing frames.
fn caller_class(t: &mut Thread, _: &[u64]) -> JResult<u64> {
    let caller = (2..t.frame_count())
        .filter_map(|n| t.caller_frame(n))
        .map(|(m, _)| m)
        .find(|m| !is_reflection_frame(m.holder));
    match caller {
        Some(m) => ret_ref(rt::mirror_of(t, m.holder)?),
        None => ret_ref(std::ptr::null_mut()),
    }
}

fn is_reflection_frame(k: &Klass) -> bool {
    let n = k.name.as_str();
    n == "java/lang/reflect/Method" || n.starts_with("jdk/internal/reflect/") || n.starts_with("java/lang/invoke/LambdaForm$")
}
