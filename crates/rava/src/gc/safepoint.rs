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
    /// The object whose monitor this thread is blocked entering, or 0 (for `ThreadInfo`).
    pub blocked_on: AtomicUsize,
    /// The OS thread, for setting its priority from another thread (`os::set_java_priority`).
    pub os_thread: usize,
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
    crate::os::current_stack()
}

thread_local!(static ATTACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// Excludes collections while held, on a thread that is not a Java thread (a compiler reading
/// the heap: a young collection moves objects, `evacuate.rs`); None on a Java thread, which no
/// collection runs past anyway (it stops only at polls, never inside the caller).
pub fn heap_access() -> Option<HeapAccess> {
    if ATTACHED.with(|a| a.get()) {
        return None;
    }
    let guard = registry().collecting.lock().unwrap_or_else(|e| e.into_inner());
    READER.store(thread_label().as_ptr() as *mut u8, SeqCst);
    Some(HeapAccess { _guard: guard })
}

/// A non-Java thread's exclusion of collections (`heap_access`).
pub struct HeapAccess {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for HeapAccess {
    fn drop(&mut self) {
        READER.store(std::ptr::null_mut(), SeqCst);
    }
}

/// The calling OS thread's name, kept for diagnostics (NUL-terminated).
fn thread_label() -> &'static str {
    thread_local!(static LABEL: &'static str = Box::leak(format!("{}\0", std::thread::current().name().unwrap_or("unnamed")).into_boxed_str()));
    LABEL.with(|l| *l)
}

/// What holds the world, for diagnostics: the collecting thread's record, the step it is at
/// (an index into `STEPS`) and since when (milliseconds from the first collection), and the
/// non-Java thread reading the heap (`heap_access`), if any.
static COLLECTOR: AtomicPtr<Record> = AtomicPtr::new(std::ptr::null_mut());
static STEP: AtomicUsize = AtomicUsize::new(0);
static SINCE: AtomicUsize = AtomicUsize::new(0);
static READER: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

/// The steps of a collection, as diagnostics name them.
pub const STEPS: [&str; 7] = ["idle", "stopping threads", "world stopped", "taking the heap lock", "marking", "sweeping", "returning memory"];
pub const STEP_LOCK: usize = 3;
pub const STEP_MARK: usize = 4;
pub const STEP_SWEEP: usize = 5;
pub const STEP_RELEASE: usize = 6;

fn epoch() -> std::time::Instant {
    static E: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *E.get_or_init(std::time::Instant::now)
}

/// Records the step a collection is at.
pub fn set_step(step: usize) {
    STEP.store(step, SeqCst);
    SINCE.store(epoch().elapsed().as_millis() as usize, SeqCst);
}

/// Who holds the world, as a dump reports it when it cannot stop the world itself.
pub fn holder() -> String {
    let rec = COLLECTOR.load(SeqCst);
    if !rec.is_null() || STEP.load(SeqCst) != 0 {
        let secs = (epoch().elapsed().as_millis() as usize).saturating_sub(SINCE.load(SeqCst)) as f64 / 1e3;
        // SAFETY: a registered thread's record lives as long as the process.
        let name = if rec.is_null() { "a service thread".to_string() } else { format!("\"{}\"", crate::dump::thread_name(unsafe { &*rec })) };
        return format!("{name} is collecting, at step \"{}\" for {secs:.1} s", STEPS[STEP.load(SeqCst)]);
    }
    let reader = READER.load(SeqCst);
    if !reader.is_null() {
        // SAFETY: a leaked NUL-terminated label.
        let label = unsafe { std::ffi::CStr::from_ptr(reader as *const std::ffi::c_char) };
        return format!("\"{}\" is reading the heap (a compiler thread)", label.to_string_lossy());
    }
    "nothing recorded holds it".to_string()
}

/// Whether a stop is requested (the frames of stopped and native threads hold still).
pub fn requested() -> bool {
    REQUESTED.load(SeqCst)
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
        blocked_on: AtomicUsize::new(0),
        os_thread: crate::os::current_thread_handle(),
    }));
    t.gc_record = Some(rec);
    registry().threads.lock().unwrap().push(rec);
    crate::sampler::attach_current();
    // Armed if a collection started before `arm` could see this thread.
    t.poll.store(REQUESTED.load(SeqCst) as u8, SeqCst);
    // A collection may be in progress: do not start running Java code under it.
    poll(t);
}

pub fn detach(t: &mut Thread) {
    ATTACHED.with(|a| a.set(false));
    crate::sampler::detach_current();
    if let Some(rec) = t.gc_record.take() {
        // Still in Java: no collection runs meanwhile to reset the cache itself.
        crate::vm::vm().heap.retire_cache(t);
        // Unregistered while still running, never published as native: a collection either
        // waits for the record to go (its wait rereads the registry) or never sees this
        // thread, whose `Thread` and value stack are freed right after this returns. Published
        // as native first, a collection could take the record as stopped and scan the frames
        // after they were freed.
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
    // The callee-saved registers, where callers further up may keep references: rbx, rbp and
    // r12-r15, and on Windows rdi and rsi too.
    let mut regs = [0usize; 8];
    // SAFETY: stores registers into a local array.
    unsafe {
        std::arch::asm!(
            "mov [{0}], rbx", "mov [{0} + 8], rbp", "mov [{0} + 16], r12",
            "mov [{0} + 24], r13", "mov [{0} + 32], r14", "mov [{0} + 40], r15",
            "mov [{0} + 48], rdi", "mov [{0} + 56], rsi",
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

/// Whether the collector makes every thread of the process execute a full barrier after it
/// requests a stop (Linux `membarrier`, HotSpot's `UseSystemMemoryBarrier`). A thread returning
/// from native code then needs no fence of its own between publishing that it runs Java and
/// reading the request: whichever comes first, the collector's barrier orders the two.
pub fn asymmetric() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(crate::os::register_process_barrier)
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
    COLLECTOR.store(t.gc_record.map_or(std::ptr::null_mut(), |r| r as *const Record as *mut Record), SeqCst);
    let r = stopped(t.gc_record, guard, None, f);
    COLLECTOR.store(std::ptr::null_mut(), SeqCst);
    Some(r)
}

/// Stops every Java thread from a thread that is not one (a diagnostic service thread) and runs
/// `f` meanwhile; None when a collection or a heap reader holds the world for longer than
/// `patience`. Threads still running Java after `patience` are left running: `f` must not read
/// their frames.
pub fn try_with_world_stopped<R>(patience: std::time::Duration, f: impl FnOnce() -> R) -> Option<R> {
    let end = std::time::Instant::now() + patience;
    let guard = loop {
        match registry().collecting.try_lock() {
            Ok(g) => break g,
            Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) if std::time::Instant::now() < end => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => return None,
        }
    };
    Some(stopped(None, guard, Some(std::time::Instant::now() + patience), |_| f()))
}

/// With the collection lock held: requests the stop, waits until no other thread runs Java (or
/// the deadline passes), runs `f` and resumes the world.
fn stopped<R>(me: Option<&'static Record>, guard: std::sync::MutexGuard<'_, ()>, deadline: Option<std::time::Instant>, f: impl FnOnce(usize) -> R) -> R {
    let reg = registry();
    REQUESTED.store(true, SeqCst);
    if asymmetric() {
        crate::os::process_barrier();
    }
    arm(true);
    let me = me.map(|r| r as *const Record);
    set_step(1);
    let (started, mut warn_at) = (std::time::Instant::now(), 10u64);
    while all_threads().into_iter().any(|r| Some(r as *const Record) != me && r.state.load(SeqCst) == RUNNING) {
        if deadline.is_some_and(|d| std::time::Instant::now() > d) {
            break;
        }
        // A thread that never reaches a safepoint (blocked in a system call it entered without
        // leaving the Java state) stops everything: name it.
        if started.elapsed().as_secs() >= warn_at {
            let running: Vec<String> = all_threads().into_iter().filter(|r| Some(*r as *const Record) != me && r.state.load(SeqCst) == RUNNING).map(|r| format!("\"{}\"", crate::dump::thread_name(r))).collect();
            eprintln!("rava: waited {warn_at} s for threads to reach a safepoint: {}", running.join(", "));
            warn_at *= 3;
        }
        std::thread::yield_now();
    }
    set_step(2);
    let result = with_spilled_registers(|rsp| {
        if let Some(rec) = me {
            // SAFETY: the initiator's own record.
            unsafe { (*rec).rsp.store(rsp, SeqCst) };
        }
        f(rsp)
    });
    set_step(0);
    let mut epoch = reg.epoch.lock().unwrap();
    *epoch += 1;
    arm(false);
    REQUESTED.store(false, SeqCst);
    reg.resumed.notify_all();
    drop(epoch);
    drop(guard);
    result
}
