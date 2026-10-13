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
vm_field!(name_offset, "java/lang/Thread", "name", "Ljava/lang/String;");
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
    // SAFETY: a live java.lang.Thread (held by the caller's stack, so it stays put); the field
    // is a volatile boolean.
    let field = unsafe { &*((jt as usize + interrupted_offset() as usize) as *const std::sync::atomic::AtomicBool) };
    field.load(std::sync::atomic::Ordering::SeqCst) && (!clear || field.swap(false, std::sync::atomic::Ordering::SeqCst))
}

/// `JavaThreadStatus` values stored in `Thread.FieldHolder.threadStatus` (JVMTI state bits:
/// alive, runnable, waiting, with or without timeout, sleeping, in `Object.wait`, parked,
/// blocked on a monitor), which `Thread.getState` and `ThreadInfo` decode.
pub const STATUS_RUNNABLE: i32 = 0x0005;
pub const STATUS_TERMINATED: i32 = 0x0002;
pub const STATUS_SLEEPING: i32 = 0x00e1;
pub const STATUS_IN_OBJECT_WAIT: i32 = 0x0191;
pub const STATUS_IN_OBJECT_WAIT_TIMED: i32 = 0x01a1;
pub const STATUS_PARKED: i32 = 0x0291;
pub const STATUS_PARKED_TIMED: i32 = 0x02a1;
pub const STATUS_BLOCKED_ON_MONITOR_ENTER: i32 = 0x0401;

/// Records what `t` is doing (a `STATUS_*` value) in its Java thread, for `Thread.getState`.
pub(crate) fn set_status(t: &Thread, status: i32) {
    let jt = t.java_thread;
    if jt.is_null() {
        return;
    }
    // SAFETY: a live java.lang.Thread whose holder exists once it is constructed.
    unsafe {
        let holder = obj::read_ref(jt, holder_offset());
        if !holder.is_null() {
            obj::write::<i32>(holder, status_offset(), status);
        }
    }
}

/// Marks `t` blocked entering the monitor of `o` (`Thread.getState` BLOCKED, `ThreadInfo`'s lock).
pub(crate) fn block_on(t: &Thread, o: Oop) {
    set_status(t, STATUS_BLOCKED_ON_MONITOR_ENTER);
    t.gc_record.inspect(|r| r.blocked_on.store(o as usize, std::sync::atomic::Ordering::SeqCst));
}

/// Undoes `block_on` once the monitor is entered.
pub(crate) fn unblock(t: &Thread) {
    t.gc_record.inspect(|r| r.blocked_on.store(0, std::sync::atomic::Ordering::SeqCst));
    set_status(t, STATUS_RUNNABLE);
}

/// The `STATUS_*` value recorded for Java thread `jt`.
pub(crate) fn status_of(jt: Oop) -> i32 {
    // SAFETY: a live java.lang.Thread; its holder exists once it is constructed.
    unsafe {
        let holder = obj::read_ref(jt, holder_offset());
        if holder.is_null() { 0 } else { obj::read::<i32>(holder, status_offset()) }
    }
}

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
    // An instance method, called on the current thread: a[0] is the thread, a[1] the name.
    r.add(T, "setNativeName", "(Ljava/lang/String;)V", |_, a| {
        set_os_thread_name(&rt::java_string_to_rust(a[1] as Oop));
        ret_void()
    });
    r.add(T, "interrupt0", "()V", |_, a| {
        // The Java caller has set `interrupted`; wake whatever the target is blocked in.
        if let Some(rec) = record_of(a[0] as Oop) {
            rec.parker.unpark();
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
    // JDK 25's name for it (also nanoseconds).
    r.add(T, "sleepNanos0", "(J)V", sleep0);
    r.add(T, "getThreads", "()[Ljava/lang/Thread;", get_threads);
    // Stacks of other threads are not walked here: each thread reports an empty trace.
    r.add(T, "dumpThreads", "([Ljava/lang/Thread;)[[Ljava/lang/StackTraceElement;", |t, a| {
        // SAFETY: a live Thread[] when non-null.
        let n = if a[0] == 0 { 0 } else { unsafe { obj::array_length(a[0] as Oop) } };
        let ste = crate::loader::boot_class!("java/lang/StackTraceElement").expect("StackTraceElement");
        let row = crate::vm::vm().array_class_of(ste).expect("StackTraceElement[]");
        let table = crate::vm::vm().array_class_of(row).expect("StackTraceElement[][]");
        let out = rt::alloc_array(t, table, n)?;
        for i in 0..n as usize {
            let empty = rt::alloc_array(t, row, 0)?;
            // SAFETY: `out` has n reference elements.
            unsafe { obj::store_elem(out, i, empty) };
        }
        ret_ref(out)
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
    r.add("java/lang/Object", "wait0", "(J)V", |t, a| {
        let jt = t.java_thread;
        set_status(t, if a[1] as i64 > 0 { STATUS_IN_OBJECT_WAIT_TIMED } else { STATUS_IN_OBJECT_WAIT });
        let waited = sync::wait(t, a[0] as Oop, a[1] as i64, &|| is_interrupted(jt, true));
        set_status(t, STATUS_RUNNABLE);
        match waited {
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
    set_status(t, STATUS_SLEEPING);
    let interrupted = match t.gc_record {
        None => {
            crate::gc::safepoint::blocking(t, || std::thread::sleep(deadline - std::time::Instant::now()));
            false
        }
        Some(rec) => crate::gc::safepoint::blocking(t, || rec.parker.sleep(deadline, &|| is_interrupted(jt, true))),
    };
    set_status(t, STATUS_RUNNABLE);
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
        let holder = obj::read_ref(jt, holder_offset());
        if !holder.is_null() {
            obj::write::<i32>(holder, status_offset(), STATUS_RUNNABLE);
        }
    }
}

fn mark_terminated(jt: Oop) {
    // SAFETY: as above.
    unsafe {
        let holder = obj::read_ref(jt, holder_offset());
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
    let daemon = unsafe { obj::read::<bool>(obj::read_ref(jt, holder_offset()), daemon_offset()) };
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

/// Names the current OS thread (as HotSpot does), for profilers and debuggers; Linux keeps
/// the first 15 bytes.
fn set_os_thread_name(name: &str) {
    let mut end = name.len().min(15);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    if let Ok(c) = std::ffi::CString::new(&name[..end]) {
        crate::os::set_thread_name(&c);
    }
}

/// Ends Java thread `jt` (run on `t`, its own): `Thread.exit()` (thread-local cleanup, such as
/// freeing cached temporary direct buffers), then terminated, its joiners notified.
pub(crate) fn terminate(t: &mut Thread, jt: Oop) {
    let thread_class = crate::loader::boot_class!("java/lang/Thread").expect("Thread");
    if let Some(exit) = thread_class.declared_method(Symbol::intern("exit"), Symbol::intern("()V")) {
        let _ = t.call(exit, &[jt as u64]);
    }
    sync::enter(t, jt);
    mark_terminated(jt);
    let _ = sync::notify(jt, true);
    sync::exit(jt);
}

fn run_thread(jt: Oop, daemon: bool) {
    let mut t = Thread::new();
    t.java_thread = jt;
    t.attach();
    // Attached, the thread keeps its own object alive.
    starting().lock().unwrap().retain(|&o| o != jt as usize);
    mark_started(&t, jt);
    // SAFETY: the attached thread's own object; its name is a live String (or null).
    let name = unsafe { obj::read_ref(jt, name_offset()) };
    if !name.is_null() {
        set_os_thread_name(&rt::java_string_to_rust(name));
    }
    // SAFETY: live receiver.
    let k = unsafe { obj::klass(jt) };
    let thread_class = crate::loader::boot_class!("java/lang/Thread").expect("Thread");
    let run = thread_class.declared_method(Symbol::intern("run"), Symbol::intern("()V")).expect("Thread.run");
    let target = select_method(k, run).unwrap_or(run);
    if let Err(ex) = t.call(target, &[jt as u64]) {
        let dispatch = thread_class
            .declared_method(Symbol::intern("dispatchUncaughtException"), Symbol::intern("(Ljava/lang/Throwable;)V"));
        if let Some(d) = dispatch {
            let _ = t.call(d, &[jt as u64, ex.0 as u64]);
        }
    }
    terminate(&mut t, jt);
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

/// `Thread.getThreads`: the started, not yet terminated platform threads.
pub(super) fn get_threads(t: &mut Thread, _: &[u64]) -> JResult<u64> {
    let thread_class = crate::loader::boot_class!("java/lang/Thread").expect("Thread");
    let array_class = crate::vm::vm().array_class_of(thread_class).expect("Thread[]");
    let count = crate::gc::safepoint::all_threads().len() as i32;
    let out = rt::alloc_array(t, array_class, count)?;
    let mut n = 0;
    for rec in crate::gc::safepoint::all_threads().into_iter().take(count as usize) {
        // SAFETY: a registered thread's record points at its live `Thread`; its Java thread
        // object is set once when it attaches.
        let jt = unsafe { (*rec.thread.load(std::sync::atomic::Ordering::Acquire)).java_thread };
        if !jt.is_null() {
            // SAFETY: `out` has `count` reference elements and n < count.
            unsafe { obj::store_elem(out, n, jt) };
            n += 1;
        }
    }
    if n == count as usize {
        return ret_ref(out);
    }
    // Some record had no Java thread yet: return the filled prefix.
    let trimmed = rt::alloc_array(t, array_class, n as i32)?;
    for i in 0..n {
        // SAFETY: both arrays have at least n reference elements.
        unsafe { obj::store_elem(trimmed, i, obj::load_elem(out, i)) };
    }
    ret_ref(trimmed)
}
