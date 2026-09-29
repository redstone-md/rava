//! Core reflection natives (`jdk.internal.reflect`, `java.lang.reflect` identity).

use super::{ret_int, ret_ref, NativeRegistry};
use crate::classfile::Cp;
use crate::method::Method;
use crate::runtime::throw_new;
use crate::types::BasicType;
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};

vm_field!(field_clazz, "java/lang/reflect/Field", "clazz", "Ljava/lang/Class;");
vm_field!(field_slot, "java/lang/reflect/Field", "slot", "I");

vm_field!(method_clazz, "java/lang/reflect/Method", "clazz", "Ljava/lang/Class;");
vm_field!(method_slot, "java/lang/reflect/Method", "slot", "I");
vm_field!(ctor_clazz, "java/lang/reflect/Constructor", "clazz", "Ljava/lang/Class;");
vm_field!(ctor_slot, "java/lang/reflect/Constructor", "slot", "I");

pub(super) fn register(r: &mut NativeRegistry) {
    for c in ["jdk/internal/reflect/NativeMethodAccessorImpl", "jdk/internal/reflect/DirectMethodHandleAccessor$NativeAccessor"] {
        r.add(c, "invoke0", "(Ljava/lang/reflect/Method;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;", invoke0);
    }
    for c in ["jdk/internal/reflect/NativeConstructorAccessorImpl", "jdk/internal/reflect/DirectConstructorHandleAccessor$NativeAccessor"] {
        r.add(c, "newInstance0", "(Ljava/lang/reflect/Constructor;[Ljava/lang/Object;)Ljava/lang/Object;", new_instance0);
    }
    r.add("jdk/internal/misc/PreviewFeatures", "isPreviewEnabled", "()Z", |_, _| Ok(0));
    r.add("jdk/internal/reflect/Reflection", "areNestMates", "(Ljava/lang/Class;Ljava/lang/Class;)Z", |t, a| {
        let (x, y) = (rt::klass_of_mirror(a[0] as Oop), rt::klass_of_mirror(a[1] as Oop));
        Ok(match (x, y) {
            (Some(x), Some(y)) => std::ptr::eq(x.nest_host(t), y.nest_host(t)),
            _ => false,
        } as u64)
    });
    const CP: &str = "jdk/internal/reflect/ConstantPool";
    r.add(CP, "getSize0", "(Ljava/lang/Object;)I", |_, a| ret_int(cp_class(a).cf.as_ref().map_or(0, |cf| cf.pool.len()) as i32));
    r.add(CP, "getIntAt0", "(Ljava/lang/Object;I)I", |_, a| match pool_entry(a) { Cp::Int(v) => ret_int(v), _ => ret_int(0) });
    r.add(CP, "getLongAt0", "(Ljava/lang/Object;I)J", |_, a| match pool_entry(a) { Cp::Long(v) => Ok(v as u64), _ => Ok(0) });
    r.add(CP, "getFloatAt0", "(Ljava/lang/Object;I)F", |_, a| match pool_entry(a) { Cp::Float(v) => Ok(v.to_bits() as u64), _ => Ok(0) });
    r.add(CP, "getDoubleAt0", "(Ljava/lang/Object;I)D", |_, a| match pool_entry(a) { Cp::Double(v) => Ok(v.to_bits()), _ => Ok(0) });
    r.add(CP, "getUTF8At0", "(Ljava/lang/Object;I)Ljava/lang/String;", |t, a| {
        let cf = cp_class(a).cf.as_ref().unwrap();
        let bytes = cf.utf8_bytes(a[2] as u16).unwrap_or(&[]);
        ret_ref(rt::new_string_utf16(t, &crate::symbol::decode_modified_utf8(bytes))?)
    });
    r.add(CP, "getStringAt0", "(Ljava/lang/Object;I)Ljava/lang/String;", |t, a| {
        let cf = cp_class(a).cf.as_ref().unwrap();
        let Cp::String(u) = pool_entry(a) else { return ret_ref(std::ptr::null_mut()) };
        ret_ref(rt::intern_literal(t, cf.utf8_bytes(u).unwrap_or(&[]))?)
    });
    r.add(CP, "getClassAt0", "(Ljava/lang/Object;I)Ljava/lang/Class;", |t, a| {
        let k = cp_class(a).resolve_class(t, a[2] as u16).map_err(|e| rt::link_error(t, e))?;
        ret_ref(rt::mirror_of(t, k)?)
    });
    r.add(CP, "getTagAt0", "(Ljava/lang/Object;I)B", |_, a| {
        let tag = match pool_entry(a) {
            Cp::Utf8 { .. } => 1, Cp::Int(_) => 3, Cp::Float(_) => 4, Cp::Long(_) => 5, Cp::Double(_) => 6,
            Cp::Class(_) => 7, Cp::String(_) => 8, Cp::Field { .. } => 9, Cp::Method { .. } => 10,
            Cp::IMethod { .. } => 11, Cp::NameAndType { .. } => 12, Cp::MethodHandle { .. } => 15,
            Cp::MethodType(_) => 16, Cp::Dynamic { .. } => 17, Cp::InvokeDynamic { .. } => 18, _ => 0,
        };
        ret_int(tag)
    });
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

fn cp_class(a: &[u64]) -> &'static Klass {
    rt::klass_of_mirror(a[1] as Oop).expect("constant pool of a primitive class")
}

fn pool_entry(a: &[u64]) -> Cp {
    cp_class(a).cf.as_ref().and_then(|cf| cf.pool.get(a[2] as usize).copied()).unwrap_or(Cp::Empty)
}

/// The `Method` behind a `java.lang.reflect.Method` or `Constructor`.
pub(crate) fn executable_of(r: Oop) -> &'static Method {
    // SAFETY: live reflection object.
    if unsafe { obj::klass(r) }.name == "java/lang/reflect/Constructor" {
        executable(r, ctor_clazz(), ctor_slot())
    } else {
        executable(r, method_clazz(), method_slot())
    }
}

/// The `Method` a reflective `Method`/`Constructor` object denotes.
fn executable(obj_ref: Oop, clazz: u32, slot: u32) -> &'static Method {
    // SAFETY: live reflection object created by the VM.
    unsafe {
        let k = rt::klass_of_mirror(obj::read::<Oop>(obj_ref, clazz)).expect("declaring class");
        &k.methods()[obj::read::<i32>(obj_ref, slot) as usize]
    }
}

/// Unboxes the `Object[]` arguments into call slots after `prefix`.
fn build_args(t: &mut Thread, m: &'static Method, prefix: Option<u64>, args: Oop) -> JResult<Vec<u64>> {
    let given: &[Oop] = if args.is_null() { &[] } else { unsafe { obj::array_slice::<Oop>(args) } };
    if given.len() != m.sig.params.len() {
        return Err(throw_new(t, "java/lang/IllegalArgumentException", "wrong number of arguments"));
    }
    let mut slots: Vec<u64> = prefix.into_iter().collect();
    for (i, &ty) in m.sig.params.iter().enumerate() {
        slots.push(rt::unbox_value(t, given[i], ty)?);
        if ty.slots() == 2 {
            slots.push(0);
        }
    }
    Ok(slots)
}

/// Wraps an exception thrown by the invoked code in InvocationTargetException.
fn wrap_target_exception(t: &mut Thread, ex: crate::thread::Throw) -> crate::thread::Throw {
    match crate::boot::construct(t, "java/lang/reflect/InvocationTargetException", "(Ljava/lang/Throwable;)V", &[ex.0 as u64]) {
        Ok(ite) => crate::thread::Throw(ite),
        Err(e) => e,
    }
}

fn invoke0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let m = executable(a[0] as Oop, method_clazz(), method_slot());
    let (recv, args) = (a[1] as Oop, a[2] as Oop);
    let target = if m.is_static() {
        rt::initialize(t, m.holder)?;
        m
    } else {
        if recv.is_null() {
            return Err(rt::null_pointer(t));
        }
        // SAFETY: live receiver.
        let rk = unsafe { obj::klass(recv) };
        if !rk.is_subtype_of(m.holder) {
            return Err(throw_new(t, "java/lang/IllegalArgumentException", "object is not an instance of declaring class"));
        }
        if m.is_private() { m } else { crate::link::select_method(rk, m).unwrap_or(m) }
    };
    let slots = build_args(t, m, (!m.is_static()).then_some(recv as u64), args)?;
    let v = t.call(target, &slots).map_err(|e| wrap_target_exception(t, e))?;
    ret_ref(rt::box_value(t, m.sig.ret, v)?)
}

fn new_instance0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let m = executable(a[0] as Oop, ctor_clazz(), ctor_slot());
    if m.holder.access & (crate::klass::ACC_ABSTRACT | crate::klass::ACC_INTERFACE) != 0 {
        return Err(throw_new(t, "java/lang/InstantiationException", m.holder.name.as_str()));
    }
    rt::initialize(t, m.holder)?;
    let o = rt::alloc_instance(t, m.holder)?;
    let slots = build_args(t, m, Some(o as u64), a[1] as Oop)?;
    t.call(m, &slots).map_err(|e| wrap_target_exception(t, e))?;
    let _ = BasicType::Void;
    ret_ref(o)
}
