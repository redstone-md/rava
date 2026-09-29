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
    /// (`ptr | 1`) to their live bytecode index.
    pub const BCI: usize = 1;
    pub const CALLER: usize = 2;
    pub const SAVED_SP: usize = 3;
}

/// Field offsets of `Thread` used by generated code.
pub mod offsets {
    pub const SP: i32 = 0;
    pub const LIMIT: i32 = 8;
    pub const TOP: i32 = 16;
    pub const PENDING: i32 = 24;
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
    stack: Box<[u64]>,
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
}

impl Thread {
    pub fn new() -> Thread {
        let mut stack = vec![0u64; STACK_SLOTS].into_boxed_slice();
        let sp = stack.as_mut_ptr();
        // SAFETY: inside the owned slice.
        let limit = unsafe { sp.add(stack.len() - RESERVE_SLOTS) };
        Thread {
            sp,
            limit,
            top: std::ptr::null_mut(),
            pending: std::ptr::null_mut(),
            stack,
            java_thread: std::ptr::null_mut(),
            scoped_value_cache: std::ptr::null_mut(),
            depth: 0,
            alloc: Default::default(),
            gc_record: None,
            handles: Vec::new(),
            lock_token: crate::sync::thread_token(),
        }
    }

    /// Registers this thread (at its final address) with the collector.
    pub fn attach(&mut self) {
        crate::gc::safepoint::attach(self);
    }

    pub fn detach(&mut self) {
        crate::gc::safepoint::detach(self);
    }

    pub(crate) fn stack_limit(&self) -> *mut u64 {
        self.limit
    }

    /// Runs `f` with the reserved stack zone available (to build a StackOverflowError).
    pub(crate) fn with_reserve<T>(&mut self, f: impl FnOnce(&mut Thread) -> T) -> T {
        let saved = self.limit;
        // SAFETY: one past the end of the owned slice.
        self.limit = unsafe { (self.stack.as_ptr() as *mut u64).add(self.stack.len()) };
        let r = f(self);
        self.limit = saved;
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
        FrameIter { hdr: self.top }
    }

    /// Takes the exception compiled code left pending.
    pub(crate) fn take_pending(&mut self) -> Option<Throw> {
        let p = std::mem::replace(&mut self.pending, std::ptr::null_mut());
        (!p.is_null()).then_some(Throw(p))
    }
}

pub struct FrameIter {
    hdr: *mut u64,
}

impl Iterator for FrameIter {
    type Item = (&'static Method, i32);

    fn next(&mut self) -> Option<Self::Item> {
        if self.hdr.is_null() {
            return None;
        }
        // SAFETY: every linked header was written by `push_frame` and outlives its frame.
        unsafe {
            let m = &*(*self.hdr.add(header::METHOD) as *const Method);
            let raw = *self.hdr.add(header::BCI);
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
