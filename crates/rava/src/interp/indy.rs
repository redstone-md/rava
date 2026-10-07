//! `invokedynamic` and signature-polymorphic calls (JVMS 6.5, 2.9.3), linked as HotSpot links
//! them: the JDK's `MethodHandleNatives.linkCallSite` / `linkMethod` return an invoker method
//! plus an appendix argument; the VM itself executes the `invokeBasic` / `linkTo*` intrinsics.

use super::support::Frame;
use crate::classfile::Cp;
use crate::heap::{obj, Oop};
use crate::klass::ACC_FINAL;
use crate::link::select_method;
use crate::method::Method;
use crate::natives::mh::member_target;
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::types::{BasicType, MethodSig};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

vm_field!(mh_form, "java/lang/invoke/MethodHandle", "form", "Ljava/lang/invoke/LambdaForm;");
vm_field!(lf_vmentry, "java/lang/invoke/LambdaForm", "vmentry", "Ljava/lang/invoke/MemberName;");

const MHN: &str = "java/lang/invoke/MethodHandleNatives";

/// A linked call site: the invoker method and its trailing appendix argument (may be null).
#[derive(Clone, Copy)]
struct Linkage {
    invoker: &'static Method,
    appendix: usize,
}

/// Linked sites keyed by (method, bytecode index): each `invokedynamic` instruction is its own
/// call site; signature-polymorphic sites are cached the same way.
fn sites() -> &'static Mutex<HashMap<(usize, usize), Linkage>> {
    static S: OnceLock<Mutex<HashMap<(usize, usize), Linkage>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Appendix objects of linked call sites (collector roots).
pub(crate) fn for_each_root(f: &mut dyn FnMut(Oop)) {
    for l in sites().lock().unwrap().values() {
        f(l.appendix as Oop);
    }
}

/// The invoker and appendix of an already linked call site (for compiled code).
pub(crate) fn linked(m: &'static Method, bci: usize) -> Option<(&'static Method, usize)> {
    sites().lock().unwrap().get(&(m as *const Method as usize, bci)).map(|l| (l.invoker, l.appendix))
}

/// Call-site signatures, parsed once per descriptor.
pub(crate) fn site_sig(desc: Symbol) -> &'static MethodSig {
    static SIGS: OnceLock<Mutex<HashMap<Symbol, &'static MethodSig>>> = OnceLock::new();
    let mut map = SIGS.get_or_init(Default::default).lock().unwrap();
    map.entry(desc).or_insert_with(|| Box::leak(Box::new(MethodSig::parse(desc))))
}

fn object_array(t: &mut Thread, len: i32) -> JResult<Oop> {
    let object = crate::loader::boot_class!("java/lang/Object").expect("Object");
    rt::alloc_array(t, crate::vm::vm().array_class_of(object).expect("Object[]"), len)
}

/// Unpacks the `MemberName` + `appendixResult[0]` an up-call produced.
fn linkage_from(t: &mut Thread, mn: Oop, appendix_box: Oop) -> JResult<Linkage> {
    let invoker = member_target(mn).ok_or_else(|| throw_new(t, "java/lang/InternalError", "unresolved invoker"))?;
    // SAFETY: the one-element Object[] passed to the up-call.
    let appendix = unsafe { obj::load_elem(appendix_box, 0) } as usize;
    Ok(Linkage { invoker, appendix })
}

pub(super) fn invokedynamic(t: &mut Thread, f: &mut Frame) -> JResult<()> {
    let k = f.method.holder;
    let cf = k.cf.as_ref().unwrap();
    let Cp::InvokeDynamic { bsm, nat } = cf.pool.at(f.u2(f.pc + 1) as usize) else {
        return Err(throw_new(t, "java/lang/IncompatibleClassChangeError", "not an InvokeDynamic constant"));
    };
    let (name, desc) = cf.name_and_type(nat).unwrap();
    let key = (f.method as *const Method as usize, f.pc);
    let cached = sites().lock().unwrap().get(&key).copied();
    let link = match cached {
        Some(l) => l,
        None => {
            let (bsm_mh, args) = rt::bootstrap_specifier(t, k, bsm)?;
            let caller = rt::mirror_of(t, k)?;
            let jname = rt::intern_str(t, name.as_str())?;
            let mtype = rt::method_type(t, k, desc.as_str())?;
            let appendix = object_array(t, 1)?;
            let mn = crate::boot::call_static(
                t,
                MHN,
                "linkCallSite",
                "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/invoke/MemberName;",
                &[caller as u64, bsm_mh as u64, jname as u64, mtype as u64, args as u64, appendix as u64],
            )? as Oop;
            let l = linkage_from(t, mn, appendix)?;
            *sites().lock().unwrap().entry(key).or_insert(l)
        }
    };
    call_linked(t, f, link, site_sig(desc), 0)
}

/// Invokes a signature-polymorphic method `m` at the current `invokevirtual`.
pub(super) fn invoke_polymorphic(t: &mut Thread, f: &mut Frame, m: &'static Method) -> JResult<()> {
    let cf = f.method.holder.cf.as_ref().unwrap();
    let (_, _, desc) = cf.member_ref(f.u2(f.pc + 1)).unwrap();
    let sig = site_sig(desc);
    let slots = sig.arg_slots as usize + 1;
    // SAFETY: the operand stack holds receiver + arguments.
    let args = unsafe { f.sp.sub(slots) };
    if m.name == "invokeBasic" {
        let ret = invoke_basic(t, args)?;
        return finish(f, args, sig, ret);
    }
    let key = (f.method as *const Method as usize, f.pc);
    let cached = sites().lock().unwrap().get(&key).copied();
    let link = match cached {
        Some(l) => l,
        None => {
            let caller = rt::mirror_of(t, f.method.holder)?;
            let defc = rt::mirror_of(t, m.holder)?;
            let jname = rt::intern_str(t, m.name.as_str())?;
            let mtype = rt::method_type(t, f.method.holder, desc.as_str())?;
            let appendix = object_array(t, 1)?;
            let mn = crate::boot::call_static(
                t,
                MHN,
                "linkMethod",
                "(Ljava/lang/Class;ILjava/lang/Class;Ljava/lang/String;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/invoke/MemberName;",
                &[caller as u64, 5, defc as u64, jname as u64, mtype as u64, appendix as u64],
            )? as Oop;
            let l = linkage_from(t, mn, appendix)?;
            *sites().lock().unwrap().entry(key).or_insert(l)
        }
    };
    call_linked(t, f, link, sig, 1)
}

/// Calls a linked invoker with the site's arguments plus the appendix.
fn call_linked(t: &mut Thread, f: &mut Frame, link: Linkage, sig: &'static MethodSig, receiver: usize) -> JResult<()> {
    let slots = sig.arg_slots as usize + receiver;
    // SAFETY: arguments are on the operand stack; the frame reserves slack for the appendix.
    let args = unsafe { f.sp.sub(slots) };
    if link.appendix != 0 {
        unsafe { *f.sp = link.appendix as u64 };
    }
    let saved = t.sp;
    t.sp = t.sp.max(unsafe { f.sp.add(1) });
    let ret = super::invoke(t, link.invoker, args);
    t.sp = saved;
    finish(f, args, sig, ret?)
}

fn finish(f: &mut Frame, args: *mut u64, sig: &MethodSig, ret: u64) -> JResult<()> {
    f.sp = args;
    if sig.ret != BasicType::Void {
        f.push(ret);
        if sig.ret.slots() == 2 {
            f.push(0);
        }
    }
    Ok(())
}

/// `MethodHandle.invokeBasic`: jump to the receiver's LambdaForm entry with the same arguments.
fn invoke_basic(t: &mut Thread, args: *mut u64) -> JResult<u64> {
    // SAFETY: slot 0 is the receiver MethodHandle.
    let mh = unsafe { *args } as Oop;
    if mh.is_null() {
        return Err(rt::null_pointer(t));
    }
    // SAFETY: live MethodHandle with a LambdaForm whose vmentry was resolved by Java code.
    let target = unsafe {
        let form = obj::read_ref(mh, mh_form());
        member_target(obj::read_ref(form, lf_vmentry()))
    };
    let target = target.ok_or_else(|| throw_new(t, "java/lang/InternalError", "LambdaForm without vmentry"))?;
    super::invoke(t, target, args)
}

/// `invokestatic MethodHandle.linkTo*(args..., MemberName)` from LambdaForm bytecode: the call
/// site's descriptor gives the argument count; the trailing MemberName names the target.
pub(super) fn invoke_link_to(t: &mut Thread, f: &mut Frame, m: &'static Method) -> JResult<()> {
    let cf = f.method.holder.cf.as_ref().unwrap();
    let (_, _, desc) = cf.member_ref(f.u2(f.pc + 1)).unwrap();
    let sig = site_sig(desc);
    let slots = sig.arg_slots as usize;
    // SAFETY: the operand stack holds the arguments, MemberName last.
    let args = unsafe { f.sp.sub(slots) };
    let mn = unsafe { *args.add(slots - 1) } as Oop;
    let target = member_target(mn).ok_or_else(|| throw_new(t, "java/lang/InternalError", "unresolved MemberName"))?;
    let target = match m.name.as_str() {
        "linkToVirtual" | "linkToInterface" => {
            let recv = unsafe { *args } as Oop;
            if recv.is_null() {
                return Err(rt::null_pointer(t));
            }
            if target.is_private() || target.access & ACC_FINAL != 0 {
                target
            } else {
                // SAFETY: live receiver.
                select_method(unsafe { obj::klass(recv) }, target)
                    .ok_or_else(|| throw_new(t, "java/lang/AbstractMethodError", &target.to_string()))?
            }
        }
        "linkToSpecial" => {
            if unsafe { *args } == 0 {
                return Err(rt::null_pointer(t));
            }
            target
        }
        _ => target,
    };
    let ret = super::invoke(t, target, args)?;
    finish(f, args, sig, ret)
}

