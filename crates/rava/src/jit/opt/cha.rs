//! The one implementation a virtual or interface method has among the classes that have
//! instances so far (C2's class hierarchy analysis for a megamorphic site): many receiver classes
//! that all inherit one method (an interface implemented only through an abstract base class) are
//! one target. Nothing here is relied on for correctness: the call is inlined behind a test of
//! the receiver's vtable entry (and, for an interface, of its superclass), which a later class
//! fails over to the dispatch.

use crate::klass::{InitState, Klass, KlassKind};
use crate::method::Method;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Answers per method, with the number of classes they were computed over.
fn cache() -> &'static Mutex<HashMap<usize, (usize, Option<&'static Method>)>> {
    static C: OnceLock<Mutex<HashMap<usize, (usize, Option<&'static Method>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The implementation every initialized concrete class implementing `m`'s holder selects for
/// `m`, if they agree on one.
pub(super) fn unique_target(m: &'static Method) -> Option<&'static Method> {
    let classes = crate::vm::vm().all_classes.lock().unwrap().len();
    let key = m as *const Method as usize;
    if let Some(&(n, t)) = cache().lock().unwrap().get(&key) {
        // A stale answer only picks the target: the guard still decides.
        if classes - n < (n / 50).max(64) {
            return t;
        }
    }
    let t = compute(m);
    cache().lock().unwrap().insert(key, (classes, t));
    t
}

fn compute(m: &'static Method) -> Option<&'static Method> {
    let holder = m.holder;
    let all = crate::vm::vm().all_classes.lock().unwrap().clone();
    let mut target: Option<&'static Method> = None;
    for k in all {
        // Only classes with instances can be receivers.
        if !matches!(k.kind, KlassKind::Instance) || k.is_interface() || k.access & crate::klass::ACC_ABSTRACT != 0 {
            continue;
        }
        if !matches!(k.init.state(), InitState::Initialized | InitState::BeingInitialized) || !k.is_subtype_of(holder) {
            continue;
        }
        let t = crate::link::select_method(k, m)?;
        if t.is_abstract() {
            return None;
        }
        match target {
            Some(x) if !std::ptr::eq(x, t) => return None,
            _ => target = Some(t),
        }
    }
    target
}

/// The test an inlined `target` of an interface method needs: the receiver's class is a
/// subclass of `target`'s (by the primary supers display) whose vtable entry is `target`.
pub(super) fn interface_guard(target: &'static Method) -> Option<(&'static Klass, i32)> {
    let c = target.holder;
    let i = target.vtable_index();
    (i >= 0 && !c.is_interface() && crate::jit::emit_obj::primary_depth(c).is_some()).then_some((c, i))
}
