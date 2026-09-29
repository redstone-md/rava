//! What the compiler may treat as constant, following HotSpot's rules (`ciField::is_constant`,
//! `ciMethodHandle`):
//! - **Static finals** of initialized classes, except `System.in/out/err` (which `System.setOut`
//!   and friends reassign).
//! - **Instance finals** of *trusted* holders: `java.lang.invoke`, `sun.invoke`, hidden classes
//!   and records, whose final fields cannot be changed through reflection.
//! - **`@Stable`** fields, once they hold a non-default value.
//! - **Method handles**: a constant handle's `form.vmentry` and a constant `MemberName`'s target.
//!   A handle may later get an equivalent form (customization), so the old target stays correct.
//!
//! Objects never move and folded values are reachable from the constant they were read from,
//! which is itself reachable (a class's statics, a linked call site or a literal).

use crate::classfile::Cp;
use crate::heap::obj;
use crate::klass::{FieldDesc, InitState, Klass, ACC_FINAL};
use crate::link::{find_field, Resolved};
use crate::method::Method;
use crate::natives::mh;
use crate::types::BasicType;

/// The declaration of field reference `index` in `m`'s class, once it is resolved.
fn field_decl(m: &'static Method, index: u16) -> Option<(&'static Klass, &'static FieldDesc)> {
    let cf = m.holder.cf.as_ref()?;
    let (_, name, desc) = cf.member_ref(index)?;
    let Cp::Field { class, .. } = cf.pool[index as usize] else { return None };
    let Some(Resolved::Class(owner)) = m.holder.resolved[class as usize].get().copied() else { return None };
    find_field(owner, name, desc)
}

fn is_stable(holder: &'static Klass, f: &'static FieldDesc) -> bool {
    let Some(cf) = holder.cf.as_ref() else { return false };
    let Some(info) = cf.fields.iter().find(|i| i.name == f.name && i.desc == f.desc) else { return false };
    info.attrs.iter().filter(|a| a.name == "RuntimeVisibleAnnotations").any(|a| {
        crate::classfile::annotation_types(cf.attr_bytes(*a)).into_iter().any(|i| cf.utf8(i).is_ok_and(|s| s == "Ljdk/internal/vm/annotation/Stable;"))
    })
}

fn trusts_instance_finals(holder: &'static Klass) -> bool {
    let n = holder.name.as_str();
    n.starts_with("java/lang/invoke/") || n.starts_with("sun/invoke/") || holder.is_hidden() || holder.super_klass.is_some_and(|s| s.name == "java/lang/Record")
}

/// Reads a value of type `ty` at `addr` in operand-slot encoding.
///
/// # Safety
/// `addr` must point at a live field or static slot of type `ty`.
unsafe fn read_slot(addr: usize, ty: BasicType) -> i64 {
    unsafe {
        match ty {
            BasicType::Boolean | BasicType::Byte => *(addr as *const i8) as i64,
            BasicType::Char => *(addr as *const u16) as i64,
            BasicType::Short => *(addr as *const i16) as i64,
            BasicType::Int => *(addr as *const i32) as i64,
            BasicType::Float => *(addr as *const u32) as i64,
            _ => *(addr as *const i64),
        }
    }
}

/// `getfield index` on the constant object `o`, if the field is constant.
pub(super) fn instance_field(m: &'static Method, index: u16, o: usize) -> Option<i64> {
    let (holder, f) = field_decl(m, index)?;
    if o == 0 || f.is_static() {
        return None;
    }
    let final_trusted = f.access & ACC_FINAL != 0 && trusts_instance_finals(holder);
    // SAFETY: `o` is a live instance of (a subclass of) `holder` by verification.
    let v = unsafe { read_slot(o + f.offset as usize, f.ty) };
    (final_trusted || (v != 0 && is_stable(holder, f))).then_some(v)
}

/// `getstatic index`, if the field is constant.
pub(super) fn static_field(m: &'static Method, index: u16) -> Option<i64> {
    let (holder, f) = field_decl(m, index)?;
    if !f.is_static() || holder.init.state() != InitState::Initialized || holder.name == "java/lang/System" {
        return None;
    }
    let slot = &holder.statics[f.offset as usize] as *const _ as usize;
    // SAFETY: a static slot of an initialized class.
    let v = unsafe { read_slot(slot, f.ty) };
    (f.access & ACC_FINAL != 0 || (v != 0 && is_stable(holder, f))).then_some(v)
}

/// The class of the constant object `o`.
pub(super) fn class_of(o: usize) -> &'static Klass {
    // SAFETY: a live object's header holds its immortal class.
    unsafe { obj::klass(o as crate::heap::Oop) }
}

/// The method a constant `MemberName` names.
pub(super) fn member_target(mn: usize) -> Option<&'static Method> {
    if mn == 0 {
        return None;
    }
    // SAFETY: a live MemberName; its ResolvedMethodName holds a leaked Method pointer.
    unsafe {
        let rmn = obj::read::<crate::heap::Oop>(mn as crate::heap::Oop, mh::mn_method());
        if rmn.is_null() {
            return None;
        }
        let raw = obj::read::<u64>(rmn, mh::rmn_vmtarget());
        (raw != 0).then(|| &*(raw as *const Method))
    }
}

/// The method `invokeBasic` on the constant handle `h` enters: `h.form.vmentry`'s target.
pub(super) fn handle_target(h: usize) -> Option<&'static Method> {
    if h == 0 {
        return None;
    }
    // SAFETY: a live MethodHandle and its LambdaForm.
    unsafe {
        let form = obj::read::<crate::heap::Oop>(h as crate::heap::Oop, crate::interp::indy::mh_form());
        if form.is_null() {
            return None;
        }
        let entry = obj::read::<crate::heap::Oop>(form, crate::interp::indy::lf_vmentry());
        member_target(entry as usize)
    }
}

/// The locals `m` never writes (their initial values stay constant through the body).
pub(super) fn unwritten_locals(m: &Method) -> Vec<bool> {
    let Some(info) = m.code else { return Vec::new() };
    let mut out = vec![true; info.max_locals as usize];
    let code = info.code;
    let mut pc = 0;
    while pc < code.len() {
        let op = code[pc];
        let written = match op {
            0x36..=0x3a | 0x84 => Some(code[pc + 1] as usize),
            0x3b..=0x4e => Some(((op - 0x3b) % 4) as usize),
            0xc4 if matches!(code[pc + 1], 0x36..=0x3a | 0x84) => Some(u16::from_be_bytes([code[pc + 2], code[pc + 3]]) as usize),
            _ => None,
        };
        if let Some(n) = written {
            out.iter_mut().skip(n).take(2).for_each(|w| *w = false);
        }
        pc += super::analysis::insn_len(code, pc);
    }
    out
}
