//! Thread registry and stop-the-world coordination.
//!
//! A thread is `RUNNING` (must stop at the next poll), `NATIVE` (blocked outside Java; the
//! collector may run) or `STOPPED` (parked at a poll). Before a thread stops or blocks it spills
//! its callee-saved registers and records its stack pointer, so the collector can scan
//! `[rsp, stack_top)` conservatively. Transitions use sequentially consistent operations
//! (Dekker-style with `REQUESTED`).

use crate::thread::Thread;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering::SeqCst};
use std::sync::{Condvar, Mutex};

pub const RUNNING: u8 = 0;
pub const NATIVE: u8 = 1;
pub const STOPPED: u8 = 2;

/// Set while a collection wants (or holds) the world stopped. Read by every poll.
pub static REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct Record {
    pub thread: AtomicPtr<Thread>,
    pub stack_top: usize,
    pub rsp: AtomicUsize,
    pub state: AtomicU8,
    /// `LockSupport.park` permit and its wakeup (also used by `sleep` and interrupts).
    pub parker: crate::parker::Parker,
    /// The object whose monitor this thread is `wait`ing on, or 0 (for `Thread.interrupt`).
    pub waiting_on: AtomicUsize,
}

struct Registry {
    threads: Mutex<Vec<&'static Record>>,
    /// Collection epoch; waiters sleep on `resumed` until it advances.
    epoch: Mutex<u64>,
    resumed: Condvar,
    collecting: Mutex<()>,
}

fn registry() -> &'static Registry {
    static R: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    R.get_or_init(|| Registry { threads: Mutex::new(Vec::new()), epoch: Mutex::new(0), resumed: Condvar::new(), collecting: Mutex::new(()) })
}

/// The top (highest address) of the calling OS thread's stack.
fn current_stack_top() -> usize {
    let (low, size) = current_stack();
    low + size
}

/// The lowest address and size of the calling OS thread's stack.
pub fn current_stack() -> (usize, usize) {
    // SAFETY: queries the attributes of the calling thread.
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        libc::pthread_getattr_np(libc::pthread_self(), &mut attr);
        let (mut addr, mut size) = (std::ptr::null_mut(), 0usize);
        libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        (addr as usize, size)
    }
}

thread_local!(static ATTACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// Excludes collections while held, on a thread that is not a Java thread (a compiler reading
/// the heap: a young collection moves objects, `evacuate.rs`); None on a Java thread, which no
/// collection runs past anyway (it stops only at polls, never inside the caller).
pub fn heap_access() -> Option<std::sync::MutexGuard<'static, ()>> {
    if ATTACHED.with(|a| a.get()) {
        return None;
    }
    Some(registry().collecting.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Registers `t` (at its final address) as a Java thread of this OS thread.
pub fn attach(t: &mut Thread) {
    ATTACHED.with(|a| a.set(true));
    let rec: &'static Record = Box::leak(Box::new(Record {
        thread: AtomicPtr::new(t),
        stack_top: current_stack_top(),
        rsp: AtomicUsize::new(0),
        state: AtomicU8::new(RUNNING),
        parker: crate::parker::Parker::default(),
        waiting_on: AtomicUsize::new(0),
    }));
    t.gc_record = Some(rec);
    registry().threads.lock().unwrap().push(rec);
    // Armed if a collection started before `arm` could see this thread.
    t.poll.store(REQUESTED.load(SeqCst) as u8, SeqCst);
    // A collection may be in progress: do not start running Java code under it.
    poll(t);
}

pub fn detach(t: &mut Thread) {
    ATTACHED.with(|a| a.set(false));
    if let Some(rec) = t.gc_record.take() {
        // Still in Java: no collection runs meanwhile to reset the cache itself.
        crate::vm::vm().heap.retire_cache(t);
        enter_native(rec);
        registry().threads.lock().unwrap().retain(|r| !std::ptr::eq(*r, rec));
    }
}

pub fn all_threads() -> Vec<&'static Record> {
    registry().threads.lock().unwrap().clone()
}

/// Calls `f` with the stack pointer after spilling callee-saved registers onto the stack, so a
/// conservative scan from that pointer sees every register-held reference of outer frames.
#[inline(never)]
pub fn with_spilled_registers<R>(f: impl FnOnce(usize) -> R) -> R {
    let mut regs = [0usize; 6];
    // SAFETY: stores registers into a local array.
    unsafe {
        std::arch::asm!(
            "mov [{0}], rbx", "mov [{0} + 8], rbp", "mov [{0} + 16], r12",
            "mov [{0} + 24], r13", "mov [{0} + 32], r14", "mov [{0} + 40], r15",
            in(reg) regs.as_mut_ptr(),
        )
    };
    let r = f(regs.as_ptr() as usize);
    std::hint::black_box(&regs);
    r
}

/// Sets (or clears) the poll word of every registered thread, which compiled code reads. Under
/// the registry lock, so no thread detaches (and frees its `Thread`) meanwhile.
fn arm(on: bool) {
    for r in registry().threads.lock().unwrap().iter() {
        let t = r.thread.load(SeqCst);
        // SAFETY: a registered thread's `Thread` lives until it detaches, under this lock.
        unsafe { (*t).poll.store(on as u8, SeqCst) };
    }
}

/// Compiled code's poll slow path: stops for a collection in progress, then resynchronizes the
/// thread's poll word with the request (it may have been armed after the collection ended).
pub fn poll_compiled(t: &Thread) {
    poll(t);
    t.poll.store(REQUESTED.load(SeqCst) as u8, SeqCst);
}

#[inline(always)]
pub fn poll(t: &Thread) {
    if REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
        stop_at_poll(t);
    }
}

#[cold]
#[inline(never)]
fn stop_at_poll(t: &Thread) {
    let Some(rec) = t.gc_record else { return };
    with_spilled_registers(|rsp| {
        rec.rsp.store(rsp, SeqCst);
        rec.state.store(STOPPED, SeqCst);
        wait_for_resume();
        rec.state.store(RUNNING, SeqCst);
    });
    // Another collection may have started between the wakeup and the state change.
    poll(t);
}

fn wait_for_resume() {
    let reg = registry();
    let mut epoch = reg.epoch.lock().unwrap();
    while REQUESTED.load(SeqCst) {
        epoch = reg.resumed.wait(epoch).unwrap();
    }
}

/// Marks the thread as blocked outside Java (the collector may run meanwhile).
pub fn enter_native(rec: &'static Record) {
    with_spilled_registers(|rsp| {
        rec.rsp.store(rsp, SeqCst);
        rec.state.store(NATIVE, SeqCst);
    });
}

/// Returns to Java after `enter_native`, waiting out any collection in progress.
pub fn leave_native(rec: &'static Record) {
    loop {
        rec.state.store(RUNNING, SeqCst);
        if !REQUESTED.load(SeqCst) {
            return;
        }
        rec.state.store(NATIVE, SeqCst);
        wait_for_resume();
    }
}

/// Runs native code (a JNI call) with the thread in the native state, as `blocking` does but
/// with one fence: entering publishes the stack pointer and state with a release store (a
/// collector that reads the state late only waits longer), and only returning needs the
/// store-load order against the collector's request (`leave_native`).
pub fn native_call<R>(t: &Thread, f: impl FnOnce() -> R) -> R {
    let Some(rec) = t.gc_record else { return f() };
    with_spilled_registers(|rsp| {
        rec.rsp.store(rsp, std::sync::atomic::Ordering::Release);
        rec.state.store(NATIVE, std::sync::atomic::Ordering::Release);
        let r = f();
        leave_native(rec);
        r
    })
}

/// Runs a blocking operation with the thread in the native state. `f` runs below the recorded
/// stack pointer, so it must not be the only holder of a reference it needs afterwards.
pub fn blocking<R>(t: &Thread, f: impl FnOnce() -> R) -> R {
    let Some(rec) = t.gc_record else { return f() };
    with_spilled_registers(|rsp| {
        rec.rsp.store(rsp, SeqCst);
        rec.state.store(NATIVE, SeqCst);
        let r = f();
        leave_native(rec);
        r
    })
}

/// Stops every other Java thread and runs `f` (with the initiator's own spilled stack pointer).
/// Returns `None` when another thread was collecting; the caller stopped for that collection
/// instead and should retry whatever needed memory.
pub fn stop_the_world<R>(t: &Thread, f: impl FnOnce(usize) -> R) -> Option<R> {
    let reg = registry();
    let guard = loop {
        if let Ok(g) = reg.collecting.try_lock() {
            break g;
        }
        if REQUESTED.load(SeqCst) {
            poll(t);
            return None;
        }
        std::thread::yield_now();
    };
    REQUESTED.store(true, SeqCst);
    arm(true);
    let me = t.gc_record.map(|r| r as *const Record);
    while all_threads().into_iter().any(|r| Some(r as *const Record) != me && r.state.load(SeqCst) == RUNNING) {
        std::thread::yield_now();
    }
    let result = with_spilled_registers(|rsp| {
        if let Some(rec) = t.gc_record {
            rec.rsp.store(rsp, SeqCst);
        }
        f(rsp)
    });
    let mut epoch = reg.epoch.lock().unwrap();
    *epoch += 1;
    arm(false);
    REQUESTED.store(false, SeqCst);
    reg.resumed.notify_all();
    drop(epoch);
    drop(guard);
    Some(result)
}
