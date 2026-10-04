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
    let Some(Resolved::Class(owner)) = m.holder.resolved[class as usize].get() else { return None };
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

/// A folded value `v` of type `ty`, about to become a constant of compiled code. A young object
/// may move at the next young collection unless a root holds it then; noted as a new root
/// (`gc::recent`), the collection keeps it in place, old from then on.
fn embedded(v: i64, ty: BasicType) -> i64 {
    if ty == BasicType::Reference {
        crate::gc::recent::note(v as crate::heap::Oop);
    }
    v
}

/// Reads a value of type `ty` at `addr` in operand-slot encoding: a heap object's field or
/// element (`heap`, where references may be compressed) or a static slot.
///
/// # Safety
/// `addr` must point at a live field, element or static slot of type `ty`.
unsafe fn read_slot(addr: usize, ty: BasicType, heap: bool) -> i64 {
    unsafe {
        match ty {
            BasicType::Reference if heap => obj::load_ref_at(addr) as i64,
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
    let _heap = crate::gc::safepoint::heap_access();
    let (holder, f) = field_decl(m, index)?;
    if o == 0 || f.is_static() {
        return None;
    }
    let final_trusted = f.access & ACC_FINAL != 0 && trusts_instance_finals(holder);
    // SAFETY: `o` is a live instance of (a subclass of) `holder` by verification.
    let v = unsafe { read_slot(o + f.offset as usize, f.ty, true) };
    (final_trusted || (v != 0 && is_stable(holder, f))).then(|| embedded(v, f.ty))
}

/// `getstatic index`, if the field is constant.
pub(super) fn static_field(m: &'static Method, index: u16) -> Option<i64> {
    let _heap = crate::gc::safepoint::heap_access();
    let (holder, f) = field_decl(m, index)?;
    if !f.is_static() || holder.init.state() != InitState::Initialized || holder.name == "java/lang/System" {
        return None;
    }
    let slot = &holder.statics[f.offset as usize] as *const _ as usize;
    // SAFETY: a static slot of an initialized class.
    let v = unsafe { read_slot(slot, f.ty, false) };
    (f.access & ACC_FINAL != 0 || (v != 0 && is_stable(holder, f))).then(|| embedded(v, f.ty))
}

/// The array dimensions of field reference `index` whose elements are constant once set: those
/// of a `@Stable` array field (HotSpot's stable dimension), else 0.
pub(super) fn stable_dims(m: &'static Method, index: u16) -> u8 {
    let Some((holder, f)) = field_decl(m, index) else { return 0 };
    let dims = f.desc.as_str().bytes().take_while(|&b| b == b'[').count() as u8;
    if dims > 0 && is_stable(holder, f) { dims } else { 0 }
}

/// Element `i` of the constant array `a` of a stable dimension, when within bounds and set
/// (non-default), in operand-slot encoding.
pub(super) fn stable_element(a: usize, i: i64, ty: BasicType) -> Option<i64> {
    let _heap = crate::gc::safepoint::heap_access();
    if a == 0 {
        return None;
    }
    let o = a as crate::heap::Oop;
    // SAFETY: a live array of `ty` elements.
    let len = unsafe { obj::array_length(o) } as i64;
    if !(0..len).contains(&i) {
        return None;
    }
    // SAFETY: an element within bounds.
    let v = unsafe { read_slot(obj::array_data::<u8>(o) as usize + i as usize * ty.size() as usize, ty, true) };
    (v != 0).then(|| embedded(v, ty))
}

/// The class of the constant object `o`.
pub(super) fn class_of(o: usize) -> &'static Klass {
    // SAFETY: a live object's header holds its immortal class.
    unsafe { obj::klass(o as crate::heap::Oop) }
}

/// The method a constant `MemberName` names.
pub(super) fn member_target(mn: usize) -> Option<&'static Method> {
    let _heap = crate::gc::safepoint::heap_access();
    member_target_unguarded(mn)
}

/// `member_target`, the caller holding `heap_access`.
fn member_target_unguarded(mn: usize) -> Option<&'static Method> {
    if mn == 0 {
        return None;
    }
    // SAFETY: a live MemberName; its ResolvedMethodName holds a leaked Method pointer.
    unsafe {
        let rmn = obj::read_ref(mn as crate::heap::Oop, mh::mn_method());
        if rmn.is_null() {
            return None;
        }
        let raw = obj::read::<u64>(rmn, mh::rmn_vmtarget());
        (raw != 0).then(|| &*(raw as *const Method))
    }
}

crate::runtime::vm_field!(counting_target, "java/lang/invoke/MethodHandleImpl$CountingWrapper", "target", "Ljava/lang/invoke/MethodHandle;");

/// The constant handle `h` as compiled code calls it: through `MethodHandleImpl.CountingWrapper`s
/// to the handle they wrap. A wrapper (a `guardWithTest` branch) counts invocations through a
/// `@DontInline` form until it switches to one that just delegates; the count is a profiling
/// heuristic with no other effect, so code compiled while it still counts calls the target.
pub(super) fn through_counting(mut h: usize) -> usize {
    let _heap = crate::gc::safepoint::heap_access();
    for _ in 0..8 {
        if h == 0 || class_of(h).name.as_str() != "java/lang/invoke/MethodHandleImpl$CountingWrapper" {
            break;
        }
        // SAFETY: a live CountingWrapper; `target` is a final field.
        h = unsafe { obj::read_ref(h as crate::heap::Oop, counting_target()) } as usize;
    }
    embedded(h as i64, BasicType::Reference) as usize
}

/// The method `invokeBasic` on the constant handle `h` enters: `h.form.vmentry`'s target.
pub(super) fn handle_target(h: usize) -> Option<&'static Method> {
    let _heap = crate::gc::safepoint::heap_access();
    if h == 0 {
        return None;
    }
    // SAFETY: a live MethodHandle and its LambdaForm.
    unsafe {
        let form = obj::read_ref(h as crate::heap::Oop, crate::interp::indy::mh_form());
        if form.is_null() {
            return None;
        }
        let entry = obj::read_ref(form, crate::interp::indy::lf_vmentry());
        member_target_unguarded(entry as usize)
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
