//! Class initialization (JVMS 5.5).

use super::{alloc_instance, class_name_of, throw_new, vm_field};
use crate::classfile::Cp;
use crate::heap::{obj, Oop};
use crate::klass::{InitState, Klass};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread, Throw};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

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
                let ncdfe = throw_new(t, "java/lang/NoClassDefFoundError", &msg);
                if let Some(cause) = saved_error(k) {
                    // SAFETY: a fresh NoClassDefFoundError, whose cause is still unset.
                    unsafe { obj::write_ref(ncdfe.0, cause_offset(), cause) };
                }
                return Err(ncdfe);
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
    if let Err(e) = &result {
        save_error(t, k, e.0);
    }
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
    crate::natives::jfr_register_on_init(t, k)?;
    if let Some(clinit) = k.declared_method(Symbol::intern("<clinit>"), Symbol::intern("()V")) {
        t.call(clinit, &[])?;
    }
    Ok(())
}

/// Static fields with a `ConstantValue` attribute get their value before `<clinit>` runs.
fn assign_constant_values(t: &mut Thread, k: &'static Klass) -> JResult<()> {
    let cf = k.cf.as_ref().unwrap();
    for f in k.fields.iter().filter(|f| f.is_static() && f.constant_value != 0) {
        let v = match cf.pool.at(f.constant_value as usize) {
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

vm_field!(cause_offset, "java/lang/Throwable", "cause", "Ljava/lang/Throwable;");
vm_field!(detail_offset, "java/lang/Throwable", "detailMessage", "Ljava/lang/String;");
vm_field!(stack_trace_offset, "java/lang/Throwable", "stackTrace", "[Ljava/lang/StackTraceElement;");
vm_field!(backtrace_offset, "java/lang/Throwable", "backtrace", "Ljava/lang/Object;");
vm_field!(thread_name_offset, "java/lang/Thread", "name", "Ljava/lang/String;");

/// The errors classes failed to initialize with, by class: global references, kept for good
/// (a class whose initialization failed stays unusable).
fn saved_errors() -> &'static Mutex<HashMap<usize, usize>> {
    static ERRORS: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    ERRORS.get_or_init(Default::default)
}

fn saved_error(k: &'static Klass) -> Option<Oop> {
    saved_errors().lock().unwrap().get(&(k as *const Klass as usize)).map(|&o| o as Oop)
}

/// Saves why `k` failed to initialize, as HotSpot does since JDK 18: not the exception itself
/// (it would keep everything its stack references alive) but a new
/// `ExceptionInInitializerError("Exception <class>: <message> [in thread "<name>"]")` with the
/// exception's stack trace. Every later `NoClassDefFoundError` for `k` carries it as its cause.
/// Failing to make it (out of memory, say) leaves those errors without a cause.
fn save_error(t: &mut Thread, k: &'static Klass, e: Oop) {
    let e = crate::jni::new_global(e);
    if let Ok(saved) = make_saved_error(t, e) {
        saved_errors().lock().unwrap().insert(k as *const Klass as usize, saved as usize);
    }
    crate::jni::delete_global(e);
}

fn make_saved_error(t: &mut Thread, e: Oop) -> JResult<Oop> {
    // SAFETY: a live Throwable and the live current Thread.
    let (detail, thread) = unsafe { (obj::read_ref(e, detail_offset()), obj::read_ref(t.java_thread, thread_name_offset())) };
    let detail = if detail.is_null() { String::new() } else { format!(": {}", super::java_string_to_rust(detail)) };
    let thread = if thread.is_null() { String::new() } else { super::java_string_to_rust(thread) };
    let text = format!("Exception {}{detail} [in thread \"{thread}\"]", class_name_of(e));
    let k = crate::loader::boot_class!("java/lang/ExceptionInInitializerError").expect("EIIE");
    initialize(t, k)?;
    let message = super::new_string(t, &text)?;
    let message = crate::jni::new_global(message);
    let saved = alloc_instance(t, k).map(crate::jni::new_global);
    let made = saved.and_then(|w| {
        let ctor = k.declared_method(Symbol::intern("<init>"), Symbol::intern("(Ljava/lang/String;)V")).expect("EIIE(String)");
        t.call(ctor, &[w as u64, message as u64])?;
        // SAFETY: a live Throwable.
        let get = crate::link::lookup_method(unsafe { obj::klass(e) }, Symbol::intern("getStackTrace"), Symbol::intern("()[Ljava/lang/StackTraceElement;")).expect("Throwable.getStackTrace");
        let trace = t.call(get, &[e as u64])? as Oop;
        // SAFETY: the new error's own fields: the symbolic trace replaces the backtrace.
        unsafe {
            obj::write_ref(w, stack_trace_offset(), trace);
            obj::write_ref(w, backtrace_offset(), std::ptr::null_mut());
        }
        Ok(w)
    });
    crate::jni::delete_global(message);
    if let (Err(_), Ok(w)) = (&made, &saved) {
        crate::jni::delete_global(*w);
    }
    made
}

/// JVMS 5.5 step 11: a non-`Error` exception from `<clinit>` becomes ExceptionInInitializerError.
fn wrap_in_initializer_error(t: &mut Thread, e: Throw) -> Throw {
    let error = crate::loader::boot_class!("java/lang/Error").expect("java/lang/Error");
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
    let k = crate::loader::boot_class!("java/lang/ExceptionInInitializerError").expect("EIIE");
    initialize(t, k)?;
    let w = alloc_instance(t, k)?;
    let ctor = k
        .declared_method(Symbol::intern("<init>"), Symbol::intern("(Ljava/lang/Throwable;)V"))
        .ok_or_else(|| super::fatal(&format!("EIIE ctor for {}", class_name_of(cause))))?;
    t.call(ctor, &[w as u64, cause as u64])?;
    Ok(w)
}
