//! `java.lang.Thread` and `Object` monitor natives. Every Java thread is an OS thread with its
//! own interpreter `Thread`.

use super::{ret_bool, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::link::select_method;
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::symbol::Symbol;
use crate::sync;
use crate::thread::{JResult, Thread};
use std::sync::atomic::AtomicI64;

vm_field!(eetop_offset, "java/lang/Thread", "eetop", "J");
vm_field!(holder_offset, "java/lang/Thread", "holder", "Ljava/lang/Thread$FieldHolder;");
vm_field!(status_offset, "java/lang/Thread$FieldHolder", "threadStatus", "I");

/// `JavaThreadStatus` values stored in `Thread.FieldHolder.threadStatus`.
pub const STATUS_RUNNABLE: i32 = 0x0005;
pub const STATUS_TERMINATED: i32 = 0x0002;

const T: &str = "java/lang/Thread";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(T, "registerNatives", "()V", |_, _| ret_void());
    r.add(T, "currentThread", "()Ljava/lang/Thread;", |t, _| ret_ref(t.java_thread));
    r.add(T, "currentCarrierThread", "()Ljava/lang/Thread;", |t, _| ret_ref(t.java_thread));
    r.add(T, "scopedValueCache", "()[Ljava/lang/Object;", |t, _| ret_ref(t.scoped_value_cache));
    r.add(T, "setScopedValueCache", "([Ljava/lang/Object;)V", |t, a| {
        t.scoped_value_cache = a[0] as Oop;
        ret_void()
    });
    r.add(T, "ensureMaterializedForStackWalk", "(Ljava/lang/Object;)V", |_, _| ret_void());
    r.add(T, "setPriority0", "(I)V", |_, _| ret_void());
    r.add(T, "setNativeName", "(Ljava/lang/String;)V", |_, _| ret_void());
    r.add(T, "interrupt0", "()V", |_, _| ret_void());
    r.add(T, "clearInterruptEvent", "()V", |_, _| ret_void());
    r.add(T, "yield0", "()V", |_, _| {
        std::thread::yield_now();
        ret_void()
    });
    r.add(T, "sleep0", "(J)V", |_, a| {
        std::thread::sleep(std::time::Duration::from_nanos(a[0].min(i64::MAX as u64)));
        ret_void()
    });
    r.add(T, "holdsLock", "(Ljava/lang/Object;)Z", |t, a| {
        if a[0] == 0 {
            return Err(rt::null_pointer(t));
        }
        ret_bool(sync::holds_lock(a[0] as Oop))
    });
    r.add(T, "getNextThreadIdOffset", "()J", |_, _| {
        static NEXT_TID: AtomicI64 = AtomicI64::new(1);
        ret_long(&NEXT_TID as *const AtomicI64 as i64)
    });
    r.add(T, "start0", "()V", start0);
    r.add("java/lang/Object", "wait0", "(J)V", |t, a| match sync::wait(a[0] as Oop, a[1] as i64) {
        Ok(()) => ret_void(),
        Err(_) => Err(throw_new(t, "java/lang/IllegalMonitorStateException", "current thread is not owner")),
    });
    r.add("java/lang/Object", "notify", "()V", |t, a| notify(t, a[0] as Oop, false));
    r.add("java/lang/Object", "notifyAll", "()V", |t, a| notify(t, a[0] as Oop, true));
}

fn notify(t: &mut Thread, o: Oop, all: bool) -> JResult<u64> {
    match sync::notify(o, all) {
        Ok(()) => ret_void(),
        Err(_) => Err(throw_new(t, "java/lang/IllegalMonitorStateException", "current thread is not owner")),
    }
}

/// Marks `jt` as alive and running (eetop non-zero, status RUNNABLE).
pub(crate) fn mark_started(jt: Oop) {
    // SAFETY: live java.lang.Thread whose holder exists after construction.
    unsafe {
        obj::write::<i64>(jt, eetop_offset(), sync::thread_token() as i64);
        let holder = obj::read::<Oop>(jt, holder_offset());
        if !holder.is_null() {
            obj::write::<i32>(holder, status_offset(), STATUS_RUNNABLE);
        }
    }
}

fn mark_terminated(jt: Oop) {
    // SAFETY: as above.
    unsafe {
        let holder = obj::read::<Oop>(jt, holder_offset());
        if !holder.is_null() {
            obj::write::<i32>(holder, status_offset(), STATUS_TERMINATED);
        }
        obj::write::<i64>(jt, eetop_offset(), 0);
    }
}

fn start0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let jt = a[0] as Oop;
    // SAFETY: live receiver.
    unsafe { obj::write::<i64>(jt, eetop_offset(), -1) };
    let addr = jt as usize;
    let spawned = std::thread::Builder::new().stack_size(512 << 20).spawn(move || run_thread(addr as Oop));
    if spawned.is_err() {
        mark_terminated(jt);
        return Err(throw_new(t, "java/lang/OutOfMemoryError", "unable to create native thread"));
    }
    ret_void()
}

fn run_thread(jt: Oop) {
    let mut t = Thread::new();
    t.java_thread = jt;
    mark_started(jt);
    // SAFETY: live receiver.
    let k = unsafe { obj::klass(jt) };
    let thread_class = crate::loader::load("java/lang/Thread").expect("Thread");
    let run = thread_class.declared_method(Symbol::intern("run"), Symbol::intern("()V")).expect("Thread.run");
    let target = select_method(k, run).unwrap_or(run);
    if let Err(ex) = t.call(target, &[jt as u64]) {
        let dispatch = thread_class
            .declared_method(Symbol::intern("dispatchUncaughtException"), Symbol::intern("(Ljava/lang/Throwable;)V"));
        if let Some(d) = dispatch {
            let _ = t.call(d, &[jt as u64, ex.0 as u64]);
        }
    }
    if let Some(exit) = thread_class.declared_method(Symbol::intern("exit"), Symbol::intern("()V")) {
        let _ = t.call(exit, &[jt as u64]);
    }
    sync::enter(jt);
    mark_terminated(jt);
    let _ = sync::notify(jt, true);
    sync::exit(jt);
}
