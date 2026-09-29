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
vm_field!(interrupted_offset, "java/lang/Thread", "interrupted", "Z");
vm_field!(daemon_offset, "java/lang/Thread$FieldHolder", "daemon", "Z");

/// The started non-daemon threads still running: the VM ends only when none is left.
fn non_daemon() -> &'static (std::sync::Mutex<usize>, std::sync::Condvar) {
    static LIVE: (std::sync::Mutex<usize>, std::sync::Condvar) = (std::sync::Mutex::new(0), std::sync::Condvar::new());
    &LIVE
}

fn non_daemon_ended() {
    let (live, ended) = non_daemon();
    *live.lock().unwrap() -= 1;
    ended.notify_all();
}

/// Blocks until every non-daemon thread has finished, as the launcher's `DestroyJavaVM` does.
pub fn await_non_daemon(t: &Thread) {
    let (live, ended) = non_daemon();
    crate::gc::safepoint::blocking(t, || {
        let mut n = live.lock().unwrap();
        while *n > 0 {
            n = ended.wait(n).unwrap();
        }
    });
}

/// Reads `jt`'s interrupt status; when set and `clear`, resets it (HotSpot `is_interrupted`).
pub(crate) fn is_interrupted(jt: Oop, clear: bool) -> bool {
    if jt.is_null() {
        return false;
    }
    // SAFETY: a live java.lang.Thread (objects never move); the field is a volatile boolean.
    let field = unsafe { &*((jt as usize + interrupted_offset() as usize) as *const std::sync::atomic::AtomicBool) };
    field.load(std::sync::atomic::Ordering::SeqCst) && (!clear || field.swap(false, std::sync::atomic::Ordering::SeqCst))
}

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
    r.add(T, "interrupt0", "()V", |_, a| {
        // The Java caller has set `interrupted`; wake whatever the target is blocked in.
        if let Some(rec) = record_of(a[0] as Oop) {
            *rec.permit.lock().unwrap() = true;
            rec.unparked.notify_all();
            let o = rec.waiting_on.load(std::sync::atomic::Ordering::SeqCst);
            if o != 0 {
                sync::wake_waiters(o);
            }
        }
        ret_void()
    });
    r.add(T, "clearInterruptEvent", "()V", |_, _| ret_void());
    r.add(T, "yield0", "()V", |_, _| {
        std::thread::yield_now();
        ret_void()
    });
    r.add(T, "sleep0", "(J)V", sleep0);
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
    r.add("java/lang/Object", "wait0", "(J)V", |t, a| {
        let jt = t.java_thread;
        match sync::wait(t, a[0] as Oop, a[1] as i64, &|| is_interrupted(jt, true)) {
            Ok(()) => ret_void(),
            Err(sync::WaitError::Interrupted) => Err(throw_new(t, "java/lang/InterruptedException", "")),
            Err(sync::WaitError::NotOwner) => Err(throw_new(t, "java/lang/IllegalMonitorStateException", "current thread is not owner")),
        }
    });
    r.add("java/lang/Object", "notify", "()V", |t, a| notify(t, a[0] as Oop, false));
    r.add("java/lang/Object", "notifyAll", "()V", |t, a| notify(t, a[0] as Oop, true));
}

/// `Thread.sleep`: waits on the thread's park event so `interrupt` can cut it short.
fn sleep0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let jt = t.java_thread;
    let deadline = std::time::Instant::now() + std::time::Duration::from_nanos(a[0].min(i64::MAX as u64));
    let interrupted = match t.gc_record {
        None => {
            crate::gc::safepoint::blocking(t, || std::thread::sleep(deadline - std::time::Instant::now()));
            false
        }
        Some(rec) => crate::gc::safepoint::blocking(t, || {
            let mut permit = rec.permit.lock().unwrap();
            loop {
                if is_interrupted(jt, true) {
                    return true;
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    return false;
                }
                permit = rec.unparked.wait_timeout(permit, deadline - now).unwrap().0;
            }
        }),
    };
    if interrupted {
        return Err(throw_new(t, "java/lang/InterruptedException", "sleep interrupted"));
    }
    ret_void()
}

fn notify(t: &mut Thread, o: Oop, all: bool) -> JResult<u64> {
    match sync::notify(o, all) {
        Ok(()) => ret_void(),
        Err(_) => Err(throw_new(t, "java/lang/IllegalMonitorStateException", "current thread is not owner")),
    }
}

/// Marks `jt` as alive and running: `eetop` holds the thread's collector record (used by
/// `unpark`), status RUNNABLE.
pub(crate) fn mark_started(t: &Thread, jt: Oop) {
    let eetop = t.gc_record.map_or(sync::thread_token() as i64, |r| r as *const _ as i64);
    // SAFETY: live java.lang.Thread whose holder exists after construction.
    unsafe {
        obj::write::<i64>(jt, eetop_offset(), eetop);
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

/// `java.lang.Thread` objects between `start0` and their thread attaching: nothing else may
/// reference them meanwhile (a virtual thread's carrier, say), so they are collector roots.
fn starting() -> &'static std::sync::Mutex<Vec<usize>> {
    static STARTING: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
    &STARTING
}

pub(crate) fn for_each_starting(f: &mut dyn FnMut(Oop)) {
    starting().lock().unwrap().iter().for_each(|&o| f(o as Oop));
}

fn start0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let jt = a[0] as Oop;
    // SAFETY: live receiver.
    unsafe { obj::write::<i64>(jt, eetop_offset(), -1) };
    // SAFETY: a started thread has its field holder.
    let daemon = unsafe { obj::read::<bool>(obj::read::<Oop>(jt, holder_offset()), daemon_offset()) };
    if !daemon {
        *non_daemon().0.lock().unwrap() += 1;
    }
    let addr = jt as usize;
    starting().lock().unwrap().push(addr);
    let spawned = std::thread::Builder::new().stack_size(512 << 20).spawn(move || run_thread(addr as Oop, daemon));
    if spawned.is_err() {
        if !daemon {
            non_daemon_ended();
        }
        starting().lock().unwrap().retain(|&o| o != addr);
        mark_terminated(jt);
        return Err(throw_new(t, "java/lang/OutOfMemoryError", "unable to create native thread"));
    }
    ret_void()
}

fn run_thread(jt: Oop, daemon: bool) {
    let mut t = Thread::new();
    t.java_thread = jt;
    t.attach();
    // Attached, the thread keeps its own object alive.
    starting().lock().unwrap().retain(|&o| o != jt as usize);
    mark_started(&t, jt);
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
    sync::enter(&t, jt);
    mark_terminated(jt);
    let _ = sync::notify(jt, true);
    sync::exit(jt);
    t.detach();
    if !daemon {
        non_daemon_ended();
    }
}

/// The collector record of a started, attached thread (from `eetop`).
pub(crate) fn record_of(jt: Oop) -> Option<&'static crate::gc::safepoint::Record> {
    if jt.is_null() {
        return None;
    }
    // SAFETY: live java.lang.Thread; eetop is 0, -1 (starting) or a leaked record address.
    let eetop = unsafe { obj::read::<i64>(jt, eetop_offset()) };
    crate::gc::safepoint::all_threads().into_iter().find(|r| *r as *const _ as i64 == eetop)
}
