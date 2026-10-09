//! `java.lang.invoke.MethodHandleNatives`: member resolution for method handles. A resolved
//! `MemberName` points at its `Method` through a `ResolvedMethodName` (injected `vmtarget`);
//! fields record their offset (instance) or absolute slot address (static) in `vmindex`.

use super::{ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::klass::{Klass, ACC_FINAL};
use crate::link::{find_field, lookup_method};
use crate::method::Method;
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::types::BasicType;

vm_field!(mn_clazz, "java/lang/invoke/MemberName", "clazz", "Ljava/lang/Class;");
vm_field!(mn_name, "java/lang/invoke/MemberName", "name", "Ljava/lang/String;");
vm_field!(mn_type, "java/lang/invoke/MemberName", "type", "Ljava/lang/Object;");
vm_field!(mn_flags, "java/lang/invoke/MemberName", "flags", "I");
vm_field!(mn_method, "java/lang/invoke/MemberName", "method", "Ljava/lang/invoke/ResolvedMethodName;");
vm_field!(mn_vmindex, "java/lang/invoke/MemberName", "vmindex", "J");
vm_field!(rmn_vmtarget, "java/lang/invoke/ResolvedMethodName", "vmtarget", "J");
vm_field!(rmn_vmholder, "java/lang/invoke/ResolvedMethodName", "vmholder", "Ljava/lang/Class;");
vm_field!(mt_rtype, "java/lang/invoke/MethodType", "rtype", "Ljava/lang/Class;");
vm_field!(mt_ptypes, "java/lang/invoke/MethodType", "ptypes", "[Ljava/lang/Class;");
vm_field!(callsite_target, "java/lang/invoke/CallSite", "target", "Ljava/lang/invoke/MethodHandle;");

const IS_METHOD: i32 = 0x0001_0000;
const IS_CONSTRUCTOR: i32 = 0x0002_0000;
const IS_FIELD: i32 = 0x0004_0000;
const CALLER_SENSITIVE: i32 = 0x0010_0000;
const TRUSTED_FINAL: i32 = 0x0020_0000;
const KIND_SHIFT: i32 = 24;
const REF_INVOKE_VIRTUAL: i32 = 5;
const REF_INVOKE_STATIC: i32 = 6;
const REF_INVOKE_SPECIAL: i32 = 7;
const REF_NEW_INVOKE_SPECIAL: i32 = 8;
const REF_INVOKE_INTERFACE: i32 = 9;

const MHN: &str = "java/lang/invoke/MethodHandleNatives";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(MHN, "registerNatives", "()V", |_, _| ret_void());
    r.add(MHN, "getNamedCon", "(I[Ljava/lang/Object;)I", |_, _| Ok(0));
    r.add(MHN, "resolve", "(Ljava/lang/invoke/MemberName;Ljava/lang/Class;IZ)Ljava/lang/invoke/MemberName;", resolve);
    r.add(MHN, "init", "(Ljava/lang/invoke/MemberName;Ljava/lang/Object;)V", init);
    r.add(MHN, "expand", "(Ljava/lang/invoke/MemberName;)V", expand);
    r.add(MHN, "objectFieldOffset", "(Ljava/lang/invoke/MemberName;)J", |_, a| ret_long(vmindex(a[0] as Oop)));
    r.add(MHN, "staticFieldOffset", "(Ljava/lang/invoke/MemberName;)J", |_, a| ret_long(vmindex(a[0] as Oop)));
    r.add(MHN, "staticFieldBase", "(Ljava/lang/invoke/MemberName;)Ljava/lang/Object;", |_, _| ret_ref(std::ptr::null_mut()));
    r.add(MHN, "getMemberVMInfo", "(Ljava/lang/invoke/MemberName;)Ljava/lang/Object;", |t, a| {
        let index = rt::box_value(t, BasicType::Long, vmindex(a[0] as Oop) as u64)?;
        // SAFETY: live MemberName.
        let clazz = unsafe { obj::read_ref(a[0] as Oop, mn_clazz()) };
        let object = crate::loader::boot_class!("java/lang/Object").expect("Object");
        let arr = rt::alloc_array(t, crate::vm::vm().array_class_of(object).expect("Object[]"), 2)?;
        unsafe {
            obj::store_elem(arr, 0, index);
            obj::store_elem(arr, 1, clazz);
        }
        ret_ref(arr)
    });
    for name in ["setCallSiteTargetNormal", "setCallSiteTargetVolatile"] {
        r.add(MHN, name, "(Ljava/lang/invoke/CallSite;Ljava/lang/invoke/MethodHandle;)V", |_, a| {
            // SAFETY: live CallSite.
            unsafe { obj::write_ref(a[0] as Oop, callsite_target(), a[1] as Oop) };
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            ret_void()
        });
    }
}

fn vmindex(mn: Oop) -> i64 {
    // SAFETY: live MemberName.
    unsafe { obj::read::<i64>(mn, mn_vmindex()) }
}

/// The method a resolved `MemberName` denotes.
pub(crate) fn member_target(mn: Oop) -> Option<&'static Method> {
    // SAFETY: live MemberName; vmtarget holds a leaked `&'static Method` or 0.
    unsafe {
        let rmn = obj::read_ref(mn, mn_method());
        if rmn.is_null() {
            return None;
        }
        let raw = obj::read::<u64>(rmn, rmn_vmtarget());
        (raw != 0).then(|| &*(raw as *const Method))
    }
}

/// Method descriptor of a `MethodType`.
pub(crate) fn method_type_descriptor(mt: Oop) -> String {
    // SAFETY: live MethodType.
    let (rtype, ptypes) = unsafe { (obj::read_ref(mt, mt_rtype()), obj::read_ref(mt, mt_ptypes())) };
    let mut d = String::from("(");
    for &p in unsafe { obj::ref_elems(ptypes) }.iter() {
        d += &class_descriptor(p);
    }
    d.push(')');
    d += &class_descriptor(rtype);
    d
}

/// Field descriptor of a class mirror.
pub(crate) fn class_descriptor(mirror: Oop) -> String {
    match rt::klass_of_mirror(mirror) {
        Some(k) if k.is_array() => k.name.as_str().to_string(),
        Some(k) => format!("L{};", k.name),
        None => rt::primitive_type_of_mirror(mirror).map_or('V', |t| t.desc_char()).to_string(),
    }
}

fn is_caller_sensitive(m: &'static Method) -> bool {
    let Some(cf) = m.holder.cf.as_ref() else { return false };
    let info = &cf.methods[m.index()];
    info.attrs.iter().filter(|a| a.name == "RuntimeVisibleAnnotations").any(|a| {
        crate::classfile::annotation_types(cf.attr_bytes(*a))
            .into_iter()
            .any(|i| cf.utf8(i).is_ok_and(|s| s == "Ljdk/internal/reflect/CallerSensitive;"))
    })
}

/// The `ResolvedMethodName` naming method `m`: one per method, a root for good once made (so it
/// never moves), as HotSpot interns them.
pub(super) fn resolved_method_name(t: &mut Thread, m: &'static Method) -> JResult<Oop> {
    use std::sync::atomic::Ordering;
    let cached = m.resolved_name.load(Ordering::Acquire);
    if cached != 0 {
        return Ok(cached as Oop);
    }
    let rmn = new_resolved_method_name(t, m)?;
    crate::gc::recent::embed(rmn);
    match m.resolved_name.compare_exchange(0, rmn as usize, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Ok(rmn),
        Err(won) => Ok(won as Oop),
    }
}

fn new_resolved_method_name(t: &mut Thread, m: &'static Method) -> JResult<Oop> {
    let rmn_class = crate::loader::boot_class!("java/lang/invoke/ResolvedMethodName").expect("ResolvedMethodName");
    rt::initialize(t, rmn_class)?;
    let rmn = rt::alloc_instance(t, rmn_class)?;
    let holder = rt::mirror_of(t, m.holder)?;
    // SAFETY: a fresh ResolvedMethodName.
    unsafe {
        obj::write::<u64>(rmn, rmn_vmtarget(), m as *const Method as u64);
        obj::write_ref(rmn, rmn_vmholder(), holder);
    }
    Ok(rmn)
}

/// The method a `ResolvedMethodName` names.
pub(super) fn method_of_resolved_name(rmn: Oop) -> Option<&'static Method> {
    // SAFETY: a live ResolvedMethodName; vmtarget holds a leaked `&'static Method` or 0.
    let raw = unsafe { obj::read::<u64>(rmn, rmn_vmtarget()) };
    (raw != 0).then(|| unsafe { &*(raw as *const Method) })
}

/// `MemberName.flags` as stack frames report them (JDK 25's `ClassFrameInfo.flags`): access
/// flags, method or constructor, caller-sensitive and hidden-member bits.
pub(super) fn frame_member_flags(m: &'static Method) -> i32 {
    const HIDDEN_MEMBER: i32 = 0x0040_0000;
    let shape = if m.name == "<init>" { IS_CONSTRUCTOR } else { IS_METHOD };
    let sensitive = if is_caller_sensitive(m) { CALLER_SENSITIVE } else { 0 };
    let hidden = if m.is_hidden_frame() { HIDDEN_MEMBER } else { 0 };
    (m.access as i32 & 0xffff) | shape | sensitive | hidden
}

/// Writes the resolution of method `m` into `mn` with reference kind `kind`.
fn fill_method(t: &mut Thread, mn: Oop, m: &'static Method, requested: i32) -> JResult<()> {
    let rmn = resolved_method_name(t, m)?;
    let holder = rt::mirror_of(t, m.holder)?;
    let (kind, shape) = if m.name == "<init>" {
        (REF_NEW_INVOKE_SPECIAL, IS_CONSTRUCTOR)
    } else if m.is_static() {
        (REF_INVOKE_STATIC, IS_METHOD)
    } else if requested == REF_INVOKE_SPECIAL || m.is_private() {
        (REF_INVOKE_SPECIAL, IS_METHOD)
    } else if m.holder.is_interface() && requested == REF_INVOKE_INTERFACE {
        (REF_INVOKE_INTERFACE, IS_METHOD)
    } else {
        (REF_INVOKE_VIRTUAL, IS_METHOD)
    };
    let sensitive = if is_caller_sensitive(m) { CALLER_SENSITIVE } else { 0 };
    let flags = (m.access as i32 & 0xffff) | shape | sensitive | (kind << KIND_SHIFT);
    // SAFETY: live objects; offsets from their layouts.
    unsafe {
        obj::write_ref(mn, mn_method(), rmn);
        obj::write::<i32>(mn, mn_flags(), flags);
        obj::write_ref(mn, mn_clazz(), holder);
        obj::write::<i64>(mn, mn_vmindex(), m.vtable_index() as i64);
    }
    Ok(())
}

fn fill_field(t: &mut Thread, mn: Oop, holder: &'static Klass, index: usize, kind: i32) -> JResult<()> {
    let f = &holder.fields[index];
    let trusted = f.access & ACC_FINAL != 0
        && (f.is_static() || holder.is_hidden() || holder.super_klass.is_some_and(|s| s.name == "java/lang/Record"));
    let flags = (f.access as i32 & 0xffff) | IS_FIELD | if trusted { TRUSTED_FINAL } else { 0 } | (kind << KIND_SHIFT);
    let vmindex = if f.is_static() { super::unsafe_mem::static_slot_address(holder, index) as i64 } else { f.offset as i64 };
    let mirror = rt::mirror_of(t, holder)?;
    // SAFETY: live MemberName.
    unsafe {
        obj::write::<i32>(mn, mn_flags(), flags);
        obj::write_ref(mn, mn_clazz(), mirror);
        obj::write::<i64>(mn, mn_vmindex(), vmindex);
    }
    Ok(())
}

fn resolve(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (mn, speculative) = (a[0] as Oop, a[3] != 0);
    // SAFETY: live MemberName with clazz, name, type and flags filled by Java code.
    let (clazz, name, ty, flags) = unsafe {
        (obj::read_ref(mn, mn_clazz()), obj::read_ref(mn, mn_name()), obj::read_ref(mn, mn_type()), obj::read::<i32>(mn, mn_flags()))
    };
    let Some(defc) = rt::klass_of_mirror(clazz) else {
        return Err(throw_new(t, "java/lang/IllegalArgumentException", "primitive class"));
    };
    let name_s = Symbol::intern(&rt::java_string_to_rust(name));
    let kind = (flags >> KIND_SHIFT) & 0xf;
    // SAFETY: live `type` object.
    let desc = match unsafe { obj::klass(ty) }.name.as_str() {
        "java/lang/String" => rt::java_string_to_rust(ty),
        "java/lang/Class" => class_descriptor(ty),
        _ => method_type_descriptor(ty),
    };
    let desc_s = Symbol::intern(&desc);
    let found = if flags & IS_FIELD != 0 {
        match find_field(defc, name_s, desc_s) {
            Some((holder, f)) => {
                let index = holder.fields.iter().position(|x| std::ptr::eq(x, f)).unwrap();
                fill_field(t, mn, holder, index, kind)?;
                true
            }
            None => false,
        }
    } else {
        match lookup_method(defc, name_s, desc_s) {
            Some(m) => {
                fill_method(t, mn, m, kind)?;
                true
            }
            None => false,
        }
    };
    if found {
        return ret_ref(mn);
    }
    if speculative {
        return ret_ref(std::ptr::null_mut());
    }
    let (err, what) = if flags & IS_FIELD != 0 { ("java/lang/NoSuchFieldError", "field") } else { ("java/lang/NoSuchMethodError", "method") };
    Err(throw_new(t, err, &format!("no such {what}: {}.{name_s}{desc}", defc.name.as_str().replace('/', "."))))
}

/// Initializes `mn` to denote `m` (for stack walking).
pub(crate) fn init_member_name(t: &mut Thread, mn: Oop, m: &'static Method) -> JResult<()> {
    let kind = if m.is_static() { REF_INVOKE_STATIC } else if m.holder.is_interface() { REF_INVOKE_INTERFACE } else { REF_INVOKE_VIRTUAL };
    fill_method(t, mn, m, kind)
}

/// `init(MemberName, reflective member)`.
fn init(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (mn, r) = (a[0] as Oop, a[1] as Oop);
    // SAFETY: live reflection object.
    let rk = unsafe { obj::klass(r) };
    match rk.name.as_str() {
        "java/lang/reflect/Field" => {
            let (holder, index) = super::reflect::field_of(r);
            let kind = if holder.fields[index].is_static() { 2 } else { 1 };
            fill_field(t, mn, holder, index, kind)?;
        }
        _ => {
            let m = super::reflect::executable_of(r);
            let kind = if m.is_static() { REF_INVOKE_STATIC } else if m.holder.is_interface() { REF_INVOKE_INTERFACE } else { REF_INVOKE_VIRTUAL };
            fill_method(t, mn, m, kind)?;
        }
    }
    ret_void()
}

/// `expand(MemberName)`: fills in name and type from the resolved member.
fn expand(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let mn = a[0] as Oop;
    if let Some(m) = member_target(mn) {
        let name = rt::intern_str(t, m.name.as_str())?;
        let ty = rt::intern_str(t, m.desc.as_str())?;
        // SAFETY: live MemberName.
        unsafe {
            if obj::read_ref(mn, mn_name()).is_null() {
                obj::write_ref(mn, mn_name(), name);
            }
            if obj::read_ref(mn, mn_type()).is_null() {
                obj::write_ref(mn, mn_type(), ty);
            }
        }
    }
    ret_void()
}
