//! Loadable constants as objects (JVMS 5.4.3.5): the values `ldc` pushes and bootstrap methods
//! receive. Method types, method handles and dynamic constants are produced by up-calls into
//! `java.lang.invoke.MethodHandleNatives`, exactly as HotSpot links them.

use super::{box_value, intern_literal, link_error, mirror_of, primitive_mirror};
use crate::classfile::Cp;
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::link::Resolved;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::types::{skip_field_desc, BasicType};
use crate::vm::vm;

const MHN: &str = "java/lang/invoke/MethodHandleNatives";

fn call_mhn(t: &mut Thread, name: &str, desc: &str, args: &[u64]) -> JResult<Oop> {
    Ok(crate::boot::call_static(t, MHN, name, desc, args)? as Oop)
}

/// The class mirror a field descriptor denotes, loaded through `k`'s loader.
pub fn mirror_for_desc(t: &mut Thread, k: &'static Klass, desc: &str) -> JResult<Oop> {
    let c = desc.as_bytes()[0];
    if c != b'L' && c != b'[' {
        return primitive_mirror(t, BasicType::from_desc(c));
    }
    let name = if c == b'L' { &desc[1..desc.len() - 1] } else { desc };
    let target = vm().load_class(t, k.loader(), Symbol::intern(name)).map_err(|e| link_error(t, e))?;
    mirror_of(t, target)
}

/// The `java.lang.invoke.MethodType` for a method descriptor, as seen from `k`.
pub fn method_type(t: &mut Thread, k: &'static Klass, desc: &str) -> JResult<Oop> {
    let b = desc.as_bytes();
    let mut params = Vec::new();
    let mut i = 1;
    while b[i] != b')' {
        let end = skip_field_desc(b, i);
        params.push(mirror_for_desc(t, k, &desc[i..end])?);
        i = end;
    }
    let ret = mirror_for_desc(t, k, &desc[i + 1..])?;
    let class_class = crate::loader::load("java/lang/Class").expect("Class");
    let arr = super::alloc_array(t, vm().array_class_of(class_class).expect("Class[]"), params.len() as i32)?;
    // SAFETY: fresh Class[] of the same length.
    unsafe { obj::array_slice::<Oop>(arr).copy_from_slice(&params) };
    call_mhn(t, "findMethodHandleType", "(Ljava/lang/Class;[Ljava/lang/Class;)Ljava/lang/invoke/MethodType;", &[ret as u64, arr as u64])
}

/// Constant `index` of `k`'s pool as an object; primitives come back boxed.
pub fn resolve_constant(t: &mut Thread, k: &'static Klass, index: u16) -> JResult<Oop> {
    if let Some(Resolved::Constant(o)) = k.resolved[index as usize].get() {
        return Ok(*o as Oop);
    }
    let cf = k.cf.as_ref().unwrap();
    let o = match cf.pool[index as usize] {
        Cp::Int(v) => box_value(t, BasicType::Int, v as i64 as u64)?,
        Cp::Float(v) => box_value(t, BasicType::Float, v.to_bits() as u64)?,
        Cp::Long(v) => box_value(t, BasicType::Long, v as u64)?,
        Cp::Double(v) => box_value(t, BasicType::Double, v.to_bits())?,
        Cp::String(s) => return intern_literal(t, cf.utf8_bytes(s).unwrap_or(&[])),
        Cp::Class(_) => {
            let c = k.resolve_class(t, index).map_err(|e| link_error(t, e))?;
            return mirror_of(t, c);
        }
        Cp::MethodType(d) => {
            let desc = cf.utf8(d).map_err(|_| bad(t))?;
            method_type(t, k, desc.as_str())?
        }
        Cp::MethodHandle { kind, index: member } => method_handle(t, k, kind, member)?,
        Cp::Dynamic { bsm, nat } => dynamic_constant(t, k, bsm, nat)?,
        _ => return Err(bad(t)),
    };
    // Every thread resolving the same entry must see the same object (JVMS 5.4.3).
    let r = k.resolved[index as usize].get_or_init(|| Resolved::Constant(o as usize));
    let Resolved::Constant(winner) = r else { unreachable!() };
    Ok(*winner as Oop)
}

fn bad(t: &mut Thread) -> crate::thread::Throw {
    super::throw_new(t, "java/lang/ClassFormatError", "unexpected constant-pool entry")
}

fn method_handle(t: &mut Thread, k: &'static Klass, kind: u8, member: u16) -> JResult<Oop> {
    let cf = k.cf.as_ref().unwrap();
    let (_, name, desc) = cf.member_ref(member).ok_or_else(|| bad(t))?;
    let class_index = match cf.pool[member as usize] {
        Cp::Field { class, .. } | Cp::Method { class, .. } | Cp::IMethod { class, .. } => class,
        _ => return Err(bad(t)),
    };
    let defc = k.resolve_class(t, class_index).map_err(|e| link_error(t, e))?;
    let defc_mirror = mirror_of(t, defc)?;
    let caller = mirror_of(t, k)?;
    let jname = super::intern_str(t, name.as_str())?;
    let ty = if kind <= 4 { mirror_for_desc(t, k, desc.as_str())? } else { method_type(t, k, desc.as_str())? };
    call_mhn(
        t,
        "linkMethodHandleConstant",
        "(Ljava/lang/Class;ILjava/lang/Class;Ljava/lang/String;Ljava/lang/Object;)Ljava/lang/invoke/MethodHandle;",
        &[caller as u64, kind as u64, defc_mirror as u64, jname as u64, ty as u64],
    )
}

/// The bootstrap method handle and an `Object[]` of its static arguments.
pub fn bootstrap_specifier(t: &mut Thread, k: &'static Klass, bsm: u16) -> JResult<(Oop, Oop)> {
    let cf = k.cf.as_ref().unwrap();
    let (handle, args) = cf.bootstrap_method(bsm).ok_or_else(|| bad(t))?;
    let mh = resolve_constant(t, k, handle)?;
    let object = crate::loader::load("java/lang/Object").expect("Object");
    let arr = super::alloc_array(t, vm().array_class_of(object).expect("Object[]"), args.len() as i32)?;
    for (i, &a) in args.iter().enumerate() {
        let v = resolve_constant(t, k, a)?;
        // SAFETY: `arr` has args.len() elements.
        unsafe { *obj::array_data::<Oop>(arr).add(i) = v };
    }
    Ok((mh, arr))
}

fn dynamic_constant(t: &mut Thread, k: &'static Klass, bsm: u16, nat: u16) -> JResult<Oop> {
    let cf = k.cf.as_ref().unwrap();
    let (name, desc) = cf.name_and_type(nat).ok_or_else(|| bad(t))?;
    let (mh, args) = bootstrap_specifier(t, k, bsm)?;
    let caller = mirror_of(t, k)?;
    let jname = super::intern_str(t, name.as_str())?;
    let ty = mirror_for_desc(t, k, desc.as_str())?;
    call_mhn(
        t,
        "linkDynamicConstant",
        "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        &[caller as u64, mh as u64, jname as u64, ty as u64, args as u64],
    )
}

