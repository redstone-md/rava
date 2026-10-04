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
    let (bci, depth) = ((bci_depth & 0xffff) as usize, (bci_depth >> 16 & 0xffff) as usize);
    if let Some(stats) = super::stats::get() {
        stats.deferred(m, bci);
    }
    let mut f = Frame::new(m, m.code.expect("compiled method has code"), locals);
    f.pc = bci;
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
    f.pc = bci as usize;
    t.pending = std::ptr::null_mut();
    let handler = find_handler(t, &f, ex);
    let Some(handler) = handler else {
        t.pending = ex;
        return 0;
    };
    let address = m.baseline().and_then(|cm| cm.address_of(handler as u32));
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

/// A monomorphic inline cache: the last receiver class a site saw (`klass`, 0 when empty) and
/// the target for it, stored inline so a hit is one compare against the cell and one load, with
/// no pointer to chase. The writer stores `target` before `klass` (x86 keeps store order), so a
/// reader that sees a class also sees its matching target. After `IC_MAX_UPDATES` misses the
/// site stops re-caching (megamorphic) and every miss dispatches through `select`.
#[repr(C)]
pub struct IcCell {
    klass: std::sync::atomic::AtomicUsize,
    target: std::sync::atomic::AtomicUsize,
    misses: std::sync::atomic::AtomicU32,
    /// The first two receiver classes the site saw, and whether it saw more (type profile).
    seen: [std::sync::atomic::AtomicUsize; 2],
    more: std::sync::atomic::AtomicBool,
    /// Calls tier-1 code made with each of `seen` as the receiver's class, and with others
    /// (HotSpot's receiver type counts, for its major-receiver inlining).
    counts: [std::sync::atomic::AtomicU32; 2],
    others: std::sync::atomic::AtomicU32,
}

pub(super) const IC_MAX_UPDATES: u32 = 4;
/// Counted calls before a site's major receiver is trusted.
const MAJOR_MIN_CALLS: u64 = 1000;
/// Share of a megamorphic site's calls a recorded receiver class needs to be checked for first.
const FREQUENT_PERCENT: u64 = 25;

impl IcCell {
    /// The cached receiver class while the site has seen no other.
    pub(super) fn observed(&self) -> Option<usize> {
        use std::sync::atomic::Ordering;
        let k = self.klass.load(Ordering::Acquire);
        (k != 0 && self.misses.load(Ordering::Relaxed) <= 1).then_some(k)
    }

    /// The receiver classes seen, when there were at most two.
    pub(super) fn receivers(&self) -> Option<Vec<usize>> {
        use std::sync::atomic::Ordering;
        if self.more.load(Ordering::Relaxed) {
            return None;
        }
        Some(self.seen.iter().map(|s| s.load(Ordering::Relaxed)).filter(|&k| k != 0).collect())
    }

    pub(super) fn note(&self, k: usize) {
        use std::sync::atomic::Ordering;
        for s in &self.seen {
            match s.compare_exchange(0, k, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return,
                Err(cur) if cur == k => return,
                Err(_) => {}
            }
        }
        self.more.store(true, Ordering::Relaxed);
    }

    /// The class of at least 90% of the receivers counted (C2's TypeProfileMajorReceiverPercent),
    /// once enough calls were.
    pub(super) fn major(&self) -> Option<usize> {
        use std::sync::atomic::Ordering;
        let c = [self.counts[0].load(Ordering::Relaxed) as u64, self.counts[1].load(Ordering::Relaxed) as u64];
        let total = c[0] + c[1] + self.others.load(Ordering::Relaxed) as u64;
        if total < MAJOR_MIN_CALLS {
            return None;
        }
        (0..2).map(|i| (self.seen[i].load(Ordering::Relaxed), c[i])).find(|&(k, n)| k != 0 && n * 10 >= total * 9).map(|(k, _)| k)
    }

    /// The recorded receiver classes that each took at least `FREQUENT_PERCENT` of the calls
    /// counted, most frequent first, once enough calls were: at a megamorphic site these are
    /// still worth a class check and an inlined body ahead of the generic dispatch (as C2
    /// type-checks the profiled receivers of a polymorphic site before its virtual call).
    pub(super) fn frequent(&self) -> Vec<usize> {
        use std::sync::atomic::Ordering;
        let c = [self.counts[0].load(Ordering::Relaxed) as u64, self.counts[1].load(Ordering::Relaxed) as u64];
        let total = c[0] + c[1] + self.others.load(Ordering::Relaxed) as u64;
        if total < MAJOR_MIN_CALLS {
            return Vec::new();
        }
        let mut out: Vec<(usize, u64)> = (0..2).map(|i| (self.seen[i].load(Ordering::Relaxed), c[i])).filter(|&(k, n)| k != 0 && n * 100 >= total * FREQUENT_PERCENT).collect();
        out.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        out.into_iter().map(|(k, _)| k).collect()
    }

    /// Counted calls per recorded receiver class and for all others (`RAVA_INLINE_LOG`).
    pub(super) fn shares(&self) -> (u32, u32, u32) {
        use std::sync::atomic::Ordering;
        (self.counts[0].load(Ordering::Relaxed), self.counts[1].load(Ordering::Relaxed), self.others.load(Ordering::Relaxed))
    }

    pub(super) fn counts_offset() -> i32 {
        std::mem::offset_of!(IcCell, counts) as i32
    }

    pub(super) fn others_offset() -> i32 {
        std::mem::offset_of!(IcCell, others) as i32
    }

    pub(super) fn seen_offset() -> i32 {
        std::mem::offset_of!(IcCell, seen) as i32
    }

    pub(super) fn more_offset() -> i32 {
        std::mem::offset_of!(IcCell, more) as i32
    }

    pub(super) fn misses_offset() -> i32 {
        std::mem::offset_of!(IcCell, misses) as i32
    }

    /// Offset of the cached receiver class (compared on the hit path).
    pub(super) fn klass_offset() -> i32 {
        std::mem::offset_of!(IcCell, klass) as i32
    }

    /// Offset of the cached target (loaded on the hit path).
    pub(super) fn target_offset() -> i32 {
        std::mem::offset_of!(IcCell, target) as i32
    }

    /// A cell in low memory (`crate::low`): optimized code addresses it absolutely.
    pub fn new() -> &'static IcCell {
        crate::low::leak(IcCell {
            klass: Default::default(),
            target: Default::default(),
            misses: std::sync::atomic::AtomicU32::new(0),
            seen: Default::default(),
            more: Default::default(),
            counts: Default::default(),
            others: Default::default(),
        })
    }
}

/// Inline-cache miss: dispatches and, while the site is not megamorphic, caches the result.
pub extern "sysv64" fn ic_miss(k: *const Klass, m: *const Method, cell: *const IcCell) -> *const Method {
    let target = select(k, m);
    // SAFETY: the cell is leaked and owned by one call site.
    let cell = unsafe { &*cell };
    cell.note(k as usize);
    use std::sync::atomic::Ordering;
    // Megamorphic sites stop counting, so they do not keep writing a shared line.
    if !target.is_null() && cell.misses.load(Ordering::Relaxed) < IC_MAX_UPDATES && cell.misses.fetch_add(1, Ordering::Relaxed) < IC_MAX_UPDATES {
        // Publish the target before the class: a reader that sees this class (store order is
        // kept on x86) therefore sees its matching target.
        cell.target.store(target as usize, Ordering::Release);
        cell.klass.store(k as usize, Ordering::Release);
    }
    target
}

/// The identity hash of a non-null object, assigning it (the slow path of the intrinsic).
pub extern "sysv64" fn identity_hash(o: crate::heap::Oop) -> u64 {
    crate::natives::identity_hash(o) as u32 as u64
}

/// A receiver class new to a virtual call's profile (first-tier code checks the known ones).
pub extern "sysv64" fn note_receiver(k: *const Klass, cell: *const IcCell) {
    // SAFETY: a leaked profile cell.
    unsafe { &*cell }.note(k as usize);
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

/// An interface call whose receiver class has no implementation: AbstractMethodError, as the
/// interpreter raises it.
pub extern "sysv64" fn no_target(t: *mut Thread, m: *const Method) {
    // SAFETY: the current thread and a live method.
    let (t, m) = unsafe { (&mut *t, &*m) };
    let e = crate::runtime::throw_new(t, "java/lang/AbstractMethodError", &m.to_string());
    t.pending = e.0;
}

/// `monitorenter` beyond the inline thin-lock path (may block).
pub extern "sysv64" fn monitor_enter(t: *mut Thread, o: crate::heap::Oop) {
    // SAFETY: the current thread and a live, non-null object.
    crate::sync::enter(unsafe { &*t }, o);
}

/// `monitorexit` beyond the inline thin-lock path; 0 with IllegalMonitorStateException pending
/// when this thread does not hold the monitor.
pub extern "sysv64" fn monitor_exit(t: *mut Thread, o: crate::heap::Oop) -> u64 {
    if crate::sync::exit(o) {
        return 1;
    }
    // SAFETY: the current thread.
    let t = unsafe { &mut *t };
    let e = crate::runtime::throw_new(t, "java/lang/IllegalMonitorStateException", "");
    t.pending = e.0;
    0
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

/// `multianewarray` of two dimensions (`counts`: the first in the low half, the second in the
/// high half); null with an exception pending on failure.
pub extern "sysv64" fn new_multi2(t: *mut Thread, k: *const Klass, counts: u64) -> u64 {
    // SAFETY: the current thread and an immortal class.
    let (t, k): (&mut Thread, &'static Klass) = unsafe { (&mut *t, &*k) };
    let counts = [counts as u32 as i32, (counts >> 32) as u32 as i32];
    let made = match counts.iter().find(|&&c| c < 0) {
        Some(bad) => Err(crate::runtime::throw_new(t, "java/lang/NegativeArraySizeException", &bad.to_string())),
        None => crate::interp::support::build_multi(t, k, &counts),
    };
    match made {
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
    crate::gc::safepoint::poll_compiled(unsafe { &*t });
}

/// `System.arraycopy` for optimized code: copies and returns 1 when every check passes (non-null
/// arrays of the same kind, the range within both, for reference arrays a source class that
/// is a subtype of the destination's, so no element needs a store check); otherwise returns 0
/// having changed nothing, and the caller deoptimizes for the interpreter to throw (or copy
/// element by element). Never collects.
pub extern "sysv64" fn arraycopy_fast(src: crate::heap::Oop, sp: u64, dst: crate::heap::Oop, dp: u64, len: u64) -> u64 {
    use crate::heap::obj;
    use crate::klass::KlassKind;
    let (sp, dp, len) = (sp as i32, dp as i32, len as i32);
    if src.is_null() || dst.is_null() || sp < 0 || dp < 0 || len < 0 {
        return 0;
    }
    // SAFETY: both are live objects.
    let (sk, dk) = unsafe { (obj::klass(src), obj::klass(dst)) };
    let refs = match (&sk.kind, &dk.kind) {
        (KlassKind::TypeArray { elem: x }, KlassKind::TypeArray { elem: y }) if x == y => false,
        (KlassKind::ObjArray { .. }, KlassKind::ObjArray { .. }) if std::ptr::eq(sk, dk) || sk.is_subtype_of(dk) => true,
        _ => return 0,
    };
    // SAFETY: both are arrays.
    let (sl, dl) = unsafe { (obj::array_length(src), obj::array_length(dst)) };
    if sp as u32 + len as u32 > sl as u32 || dp as u32 + len as u32 > dl as u32 {
        return 0;
    }
    let es = sk.elem_size() as usize;
    let n = len as usize * es;
    // SAFETY: checked bounds; `copy` handles overlap (same array).
    unsafe {
        let (from, to) = (obj::array_data::<u8>(src).add(sp as usize * es), obj::array_data::<u8>(dst).add(dp as usize * es));
        std::ptr::copy(from, to, n);
        if refs {
            crate::gc::cards::dirty_range(to as usize, n);
        }
    }
    1
}

/// Calls a site compiled without a receiver profile records before its method is recompiled.
pub(super) const REPROFILE_CALLS: u32 = 2000;
/// Calls of a site compiled as cold before it counts as hot.
pub(super) const HOT_COLD_CALLS: u32 = 10000;

/// Records a receiver class at a call compiled without a profile (`ic_cell` is the site's
/// profile); returns 1 when `left` (the site's remaining count, already decremented) ran out.
pub extern "sysv64" fn profile_call(k: *const Klass, cell: *const IcCell, left: *const std::sync::atomic::AtomicU32) -> u64 {
    use std::sync::atomic::Ordering;
    // SAFETY: a leaked profile cell and a leaked per-site counter.
    let (cell, left) = unsafe { (&*cell, &*left) };
    cell.note(k as usize);
    for (i, s) in cell.seen.iter().enumerate() {
        if s.load(Ordering::Relaxed) == k as usize {
            cell.counts[i].fetch_add(1, Ordering::Relaxed);
            return (left.load(Ordering::Relaxed) == 0) as u64;
        }
    }
    cell.others.fetch_add(1, Ordering::Relaxed);
    (left.load(Ordering::Relaxed) == 0) as u64
}

/// A site of `m`'s optimized code compiled without a profile turned hot: recompile `m` with
/// what the site recorded (a few times at most per method).
pub extern "sysv64" fn request_reprofile(m: *const Method, site: *const Method, bci: u64) {
    use std::sync::atomic::Ordering;
    // SAFETY: live methods (`site` null unless a cold site turned hot).
    let m: &'static Method = unsafe { &*m };
    if let Some(site) = unsafe { site.as_ref() } {
        super::profile::mark_hot_site(site, bci as usize);
    }
    if m.reprofiles.fetch_add(1, Ordering::Relaxed) < MAX_REPROFILES {
        super::queue::submit(super::queue::Job::Reprofile(m));
    }
}

/// Recompilations of one method for sites that turned hot after it was compiled.
const MAX_REPROFILES: u32 = 4;
