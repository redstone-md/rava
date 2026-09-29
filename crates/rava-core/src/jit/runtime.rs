//! Rust entry points called from compiled code (System V ABI).

use crate::interp::{exec_single, find_handler, Frame};
use crate::klass::Klass;
use crate::method::Method;
use crate::thread::{header, Thread};

/// Executes the instruction at `bci` in the interpreter on the compiled frame at `locals`, with
/// the operand stack `depth` deep. Returns non-zero when it threw (left in `Thread::pending`).
pub extern "sysv64" fn interp_one(t: *mut Thread, m: *const Method, locals: *mut u64, bci_depth: u64) -> u64 {
    // SAFETY: called by compiled code of `m` with its own thread and frame.
    let (t, m) = unsafe { (&mut *t, &*m) };
    let (bci, depth) = ((bci_depth & 0xffff_ffff) as usize, (bci_depth >> 32) as usize);
    if let Some(stats) = super::stats::get() {
        stats.deferred(m.code.unwrap().code[bci]);
    }
    let mut f = Frame::new(m, m.code.expect("compiled method has code"), locals);
    f.pc = bci;
    f.op_pc = bci;
    // SAFETY: the operand stack of this frame holds `depth` slots.
    f.sp = unsafe { f.stack_base.add(depth) };
    unsafe { *f.hdr.add(header::BCI) = (bci as u64) << 1 };
    match exec_single(t, &mut f) {
        Ok(()) => 0,
        Err(e) => {
            t.pending = e.0;
            1
        }
    }
}

/// `interp_one` for a site compiled before its constant was resolved: counts toward
/// recompiling the method with the (now resolved) fast path.
pub extern "sysv64" fn interp_one_unresolved(t: *mut Thread, m: *const Method, locals: *mut u64, bci_depth: u64) -> u64 {
    let r = interp_one(t, m, locals, bci_depth);
    // SAFETY: a live method.
    super::note_unresolved_deferral(unsafe { &*m });
    r
}

/// Finds the handler for `Thread::pending` in the innermost (compiled) frame. Returns the native
/// address to resume at, with the exception stored in operand slot 0, or 0 to unwind.
pub extern "sysv64" fn handle_exception(t: *mut Thread, locals: *mut u64) -> usize {
    // SAFETY: called by compiled code whose frame is the innermost one.
    let t = unsafe { &mut *t };
    let (m, bci) = t.frames().next().expect("compiled frame");
    let ex = t.pending;
    let mut f = Frame::new(m, m.code.expect("code"), locals);
    f.op_pc = bci as usize;
    t.pending = std::ptr::null_mut();
    let handler = find_handler(t, &f, ex);
    let Some(handler) = handler else {
        t.pending = ex;
        return 0;
    };
    let address = m.compiled().and_then(|cm| cm.address_of(handler as u32));
    match address {
        Some(a) => {
            // SAFETY: operand slot 0 of this frame.
            unsafe { *f.stack_base = ex as u64 };
            a
        }
        None => {
            t.pending = ex;
            0
        }
    }
}

/// Interface or default-method dispatch: the implementation of `m` for `k`, or null.
pub extern "sysv64" fn select(k: *const Klass, m: *const Method) -> *const Method {
    // SAFETY: a live (immortal) receiver class and a resolved method.
    let (k, m): (&'static Klass, &'static Method) = unsafe { (&*k, &*m) };
    crate::link::select_method(k, m).map_or(std::ptr::null(), |x| x as *const Method)
}

/// A monomorphic inline cache: an immutable (receiver class, target) pair published through one
/// pointer, so readers never see a torn pair. After `IC_MAX_UPDATES` misses the site stops
/// re-caching (megamorphic) and every miss dispatches through `select`.
#[repr(C)]
pub struct IcEntry {
    pub klass: usize,
    pub target: usize,
}

#[repr(C)]
pub struct IcCell {
    pub entry: std::sync::atomic::AtomicPtr<IcEntry>,
    misses: std::sync::atomic::AtomicU32,
}

const IC_MAX_UPDATES: u32 = 4;

impl IcCell {
    pub fn new() -> &'static IcCell {
        static EMPTY: IcEntry = IcEntry { klass: 0, target: 0 };
        Box::leak(Box::new(IcCell {
            entry: std::sync::atomic::AtomicPtr::new(&EMPTY as *const IcEntry as *mut IcEntry),
            misses: std::sync::atomic::AtomicU32::new(0),
        }))
    }
}

/// Inline-cache miss: dispatches and, while the site is not megamorphic, caches the result.
pub extern "sysv64" fn ic_miss(k: *const Klass, m: *const Method, cell: *const IcCell) -> *const Method {
    let target = select(k, m);
    // SAFETY: the cell is leaked and owned by one call site.
    let cell = unsafe { &*cell };
    use std::sync::atomic::Ordering;
    // Megamorphic sites stop counting, so they do not keep writing a shared line.
    if !target.is_null() && cell.misses.load(Ordering::Relaxed) < IC_MAX_UPDATES && cell.misses.fetch_add(1, Ordering::Relaxed) < IC_MAX_UPDATES {
        let e = Box::leak(Box::new(IcEntry { klass: k as usize, target: target as usize }));
        cell.entry.store(e, Ordering::Release);
    }
    target
}

/// A one-entry type-check cache: the last class that passed `subtype` against `target`.
pub extern "sysv64" fn subtype_cached(k: *const Klass, target: *const Klass, cell: *const std::sync::atomic::AtomicUsize) -> u64 {
    let ok = subtype(k, target);
    if ok == 1 {
        // SAFETY: a leaked per-site cell.
        unsafe { &*cell }.store(k as usize, std::sync::atomic::Ordering::Relaxed);
    }
    ok
}

/// `linkToVirtual` / `linkToInterface`: private and final targets bind directly.
pub extern "sysv64" fn select_linked(k: *const Klass, m: *const Method) -> *const Method {
    // SAFETY: immortal metadata.
    let target: &'static Method = unsafe { &*m };
    if target.is_private() || target.access & crate::klass::ACC_FINAL != 0 {
        return target;
    }
    select(k, m)
}

/// Raises StackOverflowError for a frame that did not fit.
pub extern "sysv64" fn stack_overflow(t: *mut Thread) {
    // SAFETY: the current thread.
    let t = unsafe { &mut *t };
    let e = crate::runtime::stack_overflow(t);
    t.pending = e.0;
}

// JLS 5.1.3 narrowing (NaN → 0, saturation): Rust's `as` casts implement exactly this.
pub extern "sysv64" fn f2i(bits: u64) -> u64 {
    f32::from_bits(bits as u32) as i32 as i64 as u64
}
pub extern "sysv64" fn f2l(bits: u64) -> u64 {
    f32::from_bits(bits as u32) as i64 as u64
}
pub extern "sysv64" fn d2i(bits: u64) -> u64 {
    f64::from_bits(bits) as i32 as i64 as u64
}
pub extern "sysv64" fn d2l(bits: u64) -> u64 {
    f64::from_bits(bits) as i64 as u64
}
pub extern "sysv64" fn frem(a: u64, b: u64) -> u64 {
    (f32::from_bits(a as u32) % f32::from_bits(b as u32)).to_bits() as u64
}
pub extern "sysv64" fn drem(a: u64, b: u64) -> u64 {
    (f64::from_bits(a) % f64::from_bits(b)).to_bits()
}


/// `k` is assignable to `target` (1) or not (0).
pub extern "sysv64" fn subtype(k: *const Klass, target: *const Klass) -> u64 {
    // SAFETY: immortal class metadata.
    let (k, target): (&'static Klass, &'static Klass) = unsafe { (&*k, &*target) };
    k.is_subtype_of(target) as u64
}

/// Allocates an instance of an initialized class; null with an exception pending on failure.
pub extern "sysv64" fn new_instance(t: *mut Thread, k: *const Klass) -> u64 {
    // SAFETY: the current thread and an immortal class.
    let (t, k): (&mut Thread, &'static Klass) = unsafe { (&mut *t, &*k) };
    match crate::runtime::alloc_instance(t, k) {
        Ok(o) => o as u64,
        Err(e) => {
            t.pending = e.0;
            0
        }
    }
}

/// Allocates an array of class `k`; null with an exception pending on failure.
pub extern "sysv64" fn new_array(t: *mut Thread, k: *const Klass, length: u64) -> u64 {
    // SAFETY: the current thread and an immortal class.
    let (t, k): (&mut Thread, &'static Klass) = unsafe { (&mut *t, &*k) };
    match crate::runtime::alloc_array(t, k, length as i32) {
        Ok(o) => o as u64,
        Err(e) => {
            t.pending = e.0;
            0
        }
    }
}

/// Safepoint poll slow path: stops for a collection in progress.
pub extern "sysv64" fn safepoint(t: *mut Thread) {
    // SAFETY: the current thread.
    crate::gc::safepoint::poll(unsafe { &*t });
}
