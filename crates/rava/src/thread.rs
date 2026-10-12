//! A Java thread's execution state.
//!
//! Every frame — interpreted, compiled or native — lives on the thread's value stack as
//! `locals[max_locals] | header[4] | operand stack[max_stack]`. The header holds the method,
//! the current bytecode index, the caller's header and the caller's `sp`; frames form a linked
//! list from `Thread::top`. Compiled code reads and writes these fields at fixed offsets, so the
//! first fields of `Thread` are `#[repr(C)]`.

use crate::heap::Oop;
use crate::method::Method;

/// A thrown Java exception propagating through Rust frames.
#[derive(Clone, Copy, Debug)]
pub struct Throw(pub Oop);

pub type JResult<T> = Result<T, Throw>;

/// Default value-stack size in 8-byte slots (64 MiB, committed lazily by the kernel).
const STACK_SLOTS: usize = 8 << 20;
/// Slots kept back from `limit` so a StackOverflowError can still be constructed.
const RESERVE_SLOTS: usize = 64 << 10;

/// Frame header layout, in slots after the locals.
pub mod header {
    pub const SLOTS: usize = 4;
    pub const METHOD: usize = 0;
    /// Compiled and native frames store `bci << 1`; interpreted frames store a tagged pointer
    /// (`ptr | 1`) to their live bytecode index; optimized code calling from inlined bodies
    /// stores a pointer to the call site's `InlinedSite` (aligned, above `SITE_MIN`).
    pub const BCI: usize = 1;
    /// Site pointers are user-space addresses in `SITE_MIN..SITE_END`; `bci << 1` of a
    /// negative bci (natives) lies above.
    pub const SITE_MIN: u64 = 1 << 20;
    pub const SITE_END: u64 = 1 << 47;
    pub const CALLER: usize = 2;
    pub const SAVED_SP: usize = 3;
}

/// The Java frames a call from optimized code stands for, innermost first, the physical
/// frame's method last (HotSpot's debug info at a call's return address): inlined bodies write
/// no headers of their own.
pub struct InlinedSite {
    pub frames: Box<[(&'static Method, i32)]>,
}

/// Field offsets of `Thread` used by generated code.
pub mod offsets {
    pub const SP: i32 = 0;
    pub const LIMIT: i32 = 8;
    pub const TOP: i32 = 16;
    pub const PENDING: i32 = 24;
    /// The card table biased by the heap base, the heap base and span, and the table itself
    /// (`gc::cards::layout`), for the write barrier of compiled code.
    pub const CARDS: i32 = 32;
    pub const HEAP_BASE: i32 = 40;
    pub const HEAP_SPAN: i32 = 48;
    pub const CARD_TABLE: i32 = 56;
    /// Non-zero while a collection wants this thread to stop (`gc::safepoint::arm`): compiled
    /// code polls it with one compare against the thread register.
    pub const POLL: i32 = 64;
}

#[repr(C)]
pub struct Thread {
    /// First free slot of the value stack.
    pub(crate) sp: *mut u64,
    /// One past the last usable slot.
    pub(crate) limit: *mut u64,
    /// Header of the innermost frame (null when no Java frame is active).
    pub(crate) top: *mut u64,
    /// Exception thrown by compiled code, awaiting its handler (null otherwise).
    pub(crate) pending: Oop,
    /// Write-barrier constants (see `offsets::CARDS`).
    cards: [usize; 4],
    /// See `offsets::POLL`; written by the collecting thread.
    pub(crate) poll: std::sync::atomic::AtomicU8,
    stack: ValueStack,
    pub java_thread: Oop,
    /// `Thread.scopedValueCache` of the current thread.
    pub(crate) scoped_value_cache: Oop,
    /// Interpreter call depth, bounded to turn runaway recursion into StackOverflowError.
    pub(crate) depth: u32,
    /// Allocation caches of this thread (reset by each collection).
    pub(crate) alloc: crate::gc::AllocCache,
    /// Registration with the collector's safepoint protocol (after `attach`).
    pub(crate) gc_record: Option<&'static crate::gc::safepoint::Record>,
    /// References natives keep outside the stacks (collector roots), e.g. while building arrays.
    pub(crate) handles: Vec<Oop>,
    /// `sync::thread_token()` of the OS thread running this thread (read by compiled locking).
    pub(crate) lock_token: u64,
    /// Below this native stack address, calls into Java from the VM throw StackOverflowError
    /// (0: unchecked). The value stack bounds Java frames, but frames the VM runs between them
    /// (an interpreter resuming deoptimized code, which calls on) use the native stack only.
    pub(crate) native_limit: usize,
    /// A StackOverflowError is being constructed (`runtime::stack_overflow`).
    pub(crate) overflowing: bool,
    /// The thread's `JNIEnv` and JNI state (`jni`).
    pub(crate) jni: crate::jni::JniThread,
}

const _: () = assert!(std::mem::offset_of!(Thread, poll) == offsets::POLL as usize);

/// A thread's value stack: its own lazily committed mapping, so only the slots a thread
/// reaches take memory (whatever the allocator does with large zeroed blocks), and unmapped
/// when the thread ends.
struct ValueStack(*mut u64);

impl ValueStack {
    fn new() -> ValueStack {
        let p = crate::os::reserve(STACK_SLOTS * 8, None, false).expect("cannot map a value stack");
        ValueStack(p as *mut u64)
    }

    fn as_ptr(&self) -> *const u64 {
        self.0
    }

    fn len(&self) -> usize {
        STACK_SLOTS
    }
}

impl Drop for ValueStack {
    fn drop(&mut self) {
        // The mapping `new` made, no longer used.
        crate::os::release(self.0 as usize, STACK_SLOTS * 8);
    }
}

// The mapping belongs to its thread's `Thread`, which moves between threads only whole.
unsafe impl Send for ValueStack {}
unsafe impl Sync for ValueStack {}

impl Thread {
    pub fn new() -> Thread {
        let stack = ValueStack::new();
        let sp = stack.as_ptr() as *mut u64;
        // SAFETY: inside the owned slice.
        let limit = unsafe { sp.add(stack.len() - RESERVE_SLOTS) };
        Thread {
            sp,
            limit,
            top: std::ptr::null_mut(),
            pending: std::ptr::null_mut(),
            cards: {
                let (biased, base, span, table) = crate::gc::cards::layout();
                [biased, base, span, table]
            },
            poll: std::sync::atomic::AtomicU8::new(0),
            stack,
            java_thread: std::ptr::null_mut(),
            scoped_value_cache: std::ptr::null_mut(),
            depth: 0,
            alloc: Default::default(),
            gc_record: None,
            handles: Vec::new(),
            lock_token: crate::sync::thread_token(),
            native_limit: 0,
            overflowing: false,
            jni: crate::jni::JniThread::new(),
        }
    }

    /// Registers this thread (at its final address) with the collector.
    pub fn attach(&mut self) {
        crate::gc::safepoint::attach(self);
        crate::jni::set_current(self);
        // What constructing the error and the VM's own calls need stays free (4 MiB, at most an
        // eighth of a small stack).
        let (low, size) = crate::gc::safepoint::current_stack();
        self.native_limit = low + (4 << 20).min(size / 8);
    }

    /// Whether the native stack is down to its reserve (`native_limit`).
    #[inline]
    pub(crate) fn native_low(&self) -> bool {
        let probe = 0u8;
        (std::ptr::addr_of!(probe) as usize) < self.native_limit
    }

    pub fn detach(&mut self) {
        crate::gc::safepoint::detach(self);
    }

    pub(crate) fn stack_limit(&self) -> *mut u64 {
        self.limit
    }

    /// Runs `f` with the reserved stack zone available (to build a StackOverflowError).
    pub(crate) fn with_reserve<T>(&mut self, f: impl FnOnce(&mut Thread) -> T) -> T {
        let (saved, native) = (self.limit, self.native_limit);
        // SAFETY: one past the end of the owned slice.
        self.limit = unsafe { (self.stack.as_ptr() as *mut u64).add(self.stack.len()) };
        self.native_limit = 0;
        let r = f(self);
        (self.limit, self.native_limit) = (saved, native);
        r
    }

    pub fn stack_range(&self) -> (*const u64, *const u64) {
        (self.stack.as_ptr(), self.sp as *const u64)
    }

    /// Calls `m` with `args` (receiver first; longs/doubles take two slots, value in the first).
    pub fn call(&mut self, m: &'static Method, args: &[u64]) -> JResult<u64> {
        let base = self.sp;
        // SAFETY: the value stack is sized far beyond any argument list.
        unsafe { std::ptr::copy_nonoverlapping(args.as_ptr(), base, args.len()) };
        let r = crate::interp::invoke(self, m, base);
        self.sp = base;
        r
    }

    /// Links a frame whose header starts at `hdr` and makes it the innermost frame.
    ///
    /// # Safety
    /// `hdr` must point at `header::SLOTS` writable slots of this thread's value stack.
    #[inline(always)]
    pub(crate) unsafe fn push_frame(&mut self, hdr: *mut u64, m: &'static Method, bci: u64) {
        *hdr.add(header::METHOD) = m as *const Method as u64;
        *hdr.add(header::BCI) = bci;
        *hdr.add(header::CALLER) = self.top as u64;
        *hdr.add(header::SAVED_SP) = self.sp as u64;
        self.top = hdr;
    }

    /// Unlinks the innermost frame, restoring the caller's `sp`.
    ///
    /// # Safety
    /// `hdr` must be the innermost frame pushed by `push_frame`.
    #[inline(always)]
    pub(crate) unsafe fn pop_frame(&mut self, hdr: *mut u64) {
        self.top = *hdr.add(header::CALLER) as *mut u64;
        self.sp = *hdr.add(header::SAVED_SP) as *mut u64;
    }

    /// Active frames, innermost first: the method and its current bytecode index (-1 for
    /// natives).
    pub fn frames(&self) -> FrameIter {
        FrameIter { hdr: self.top, inlined: &[] }
    }

    /// Takes the exception compiled code left pending.
    pub(crate) fn take_pending(&mut self) -> Option<Throw> {
        let p = std::mem::replace(&mut self.pending, std::ptr::null_mut());
        (!p.is_null()).then_some(Throw(p))
    }
}

pub struct FrameIter {
    hdr: *mut u64,
    /// Frames of an `InlinedSite` still to yield before moving to the caller.
    inlined: &'static [(&'static Method, i32)],
}

impl Iterator for FrameIter {
    type Item = (&'static Method, i32);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((&first, rest)) = self.inlined.split_first() {
            self.inlined = rest;
            if rest.is_empty() {
                // SAFETY: as below; the site's frames all belong to this header.
                self.hdr = unsafe { *self.hdr.add(header::CALLER) as *mut u64 };
            }
            return Some(first);
        }
        if self.hdr.is_null() {
            return None;
        }
        // SAFETY: every linked header was written by `push_frame` or compiled code and outlives
        // its frame; site pointers point at leaked `InlinedSite`s.
        unsafe {
            let raw = *self.hdr.add(header::BCI);
            if raw & 1 == 0 && (header::SITE_MIN..header::SITE_END).contains(&raw) {
                self.inlined = &(*(raw as *const InlinedSite)).frames;
                return self.next();
            }
            let m = &*(*self.hdr.add(header::METHOD) as *const Method);
            let bci = if raw & 1 == 1 { *((raw & !1) as *const usize) as i32 } else { (raw as i64 >> 1) as i32 };
            self.hdr = *self.hdr.add(header::CALLER) as *mut u64;
            Some((m, bci))
        }
    }
}

impl Default for Thread {
    fn default() -> Self {
        Self::new()
    }
}
