//! Class initialization (JVMS 5.5).

use super::{alloc_instance, class_name_of, throw_new};
use crate::classfile::Cp;
use crate::heap::{obj, Oop};
use crate::klass::{InitState, Klass};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread, Throw};
use std::sync::atomic::Ordering;

/// Ensures `k` is initialized, running `<clinit>` (and its supertypes') on first use.
#[inline]
pub fn initialize(t: &mut Thread, k: &'static Klass) -> JResult<()> {
    if k.init.state() == InitState::Initialized {
        return Ok(());
    }
    initialize_slow(t, k)
}

fn initialize_slow(t: &mut Thread, k: &'static Klass) -> JResult<()> {
    let me = std::thread::current().id();
    loop {
        let mut owner = k.init.owner.lock().unwrap();
        match k.init.state() {
            InitState::Initialized => return Ok(()),
            InitState::BeingInitialized if *owner == Some(me) => return Ok(()),
            InitState::BeingInitialized => {
                // Wait in the native state, and without the lock once back: a thread stopped at a
                // safepoint must not hold a lock a running thread could be waiting for.
                drop(owner);
                crate::gc::safepoint::blocking(t, || {
                    let mut owner = k.init.owner.lock().unwrap();
                    while k.init.state() == InitState::BeingInitialized && *owner != Some(me) {
                        owner = k.init.cv.wait(owner).unwrap();
                    }
                });
            }
            InitState::Erroneous => {
                let msg = format!("Could not initialize class {}", k.name.as_str().replace('/', "."));
                drop(owner);
                return Err(throw_new(t, "java/lang/NoClassDefFoundError", &msg));
            }
            InitState::Linked => {
                k.init.set_initializer(t as *const Thread as usize);
                k.init.set(InitState::BeingInitialized);
                *owner = Some(me);
                break;
            }
        }
    }
    let result = run_initializers(t, k);
    let mut owner = k.init.owner.lock().unwrap();
    *owner = None;
    k.init.set_initializer(0);
    k.init.set(if result.is_ok() { InitState::Initialized } else { InitState::Erroneous });
    k.init.cv.notify_all();
    drop(owner);
    result.map_err(|e| wrap_in_initializer_error(t, e))
}

fn run_initializers(t: &mut Thread, k: &'static Klass) -> JResult<()> {
    if !k.is_interface() {
        if let Some(s) = k.super_klass {
            initialize(t, s)?;
        }
        // Superinterfaces declaring non-abstract, non-static methods are initialized too.
        for &i in k.all_interfaces.iter() {
            if i.methods().iter().any(|m| m.is_default_candidate()) {
                initialize(t, i)?;
            }
        }
    }
    assign_constant_values(t, k)?;
    if let Some(clinit) = k.declared_method(Symbol::intern("<clinit>"), Symbol::intern("()V")) {
        t.call(clinit, &[])?;
    }
    Ok(())
}

/// Static fields with a `ConstantValue` attribute get their value before `<clinit>` runs.
fn assign_constant_values(t: &mut Thread, k: &'static Klass) -> JResult<()> {
    let cf = k.cf.as_ref().unwrap();
    for f in k.fields.iter().filter(|f| f.is_static() && f.constant_value != 0) {
        let v = match cf.pool[f.constant_value as usize] {
            Cp::Int(i) => i as i64 as u64,
            Cp::Float(x) => x.to_bits() as u64,
            Cp::Long(l) => l as u64,
            Cp::Double(d) => d.to_bits(),
            Cp::String(s) => {
                let bytes = cf.utf8_bytes(s).unwrap_or(&[]);
                super::strings::intern_modified_utf8(t, bytes)? as u64
            }
            _ => continue,
        };
        k.statics[f.offset as usize].store(v, Ordering::Relaxed);
    }
    Ok(())
}

/// JVMS 5.5 step 11: a non-`Error` exception from `<clinit>` becomes ExceptionInInitializerError.
fn wrap_in_initializer_error(t: &mut Thread, e: Throw) -> Throw {
    let error = crate::loader::load("java/lang/Error").expect("java/lang/Error");
    // SAFETY: the thrown object is live.
    if unsafe { obj::klass(e.0) }.is_subtype_of(error) {
        return e;
    }
    match make_eiie(t, e.0) {
        Ok(w) => Throw(w),
        Err(inner) => inner,
    }
}

fn make_eiie(t: &mut Thread, cause: Oop) -> JResult<Oop> {
    let k = crate::loader::load("java/lang/ExceptionInInitializerError").expect("EIIE");
    initialize(t, k)?;
    let w = alloc_instance(t, k)?;
    let ctor = k
        .declared_method(Symbol::intern("<init>"), Symbol::intern("(Ljava/lang/Throwable;)V"))
        .ok_or_else(|| super::fatal(&format!("EIIE ctor for {}", class_name_of(cause))))?;
    t.call(ctor, &[w as u64, cause as u64])?;
    Ok(w)
}
