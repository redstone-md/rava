//! A Java thread's execution state: the value stack interpreter frames live on, and the
//! identity of the `java.lang.Thread` object.

use crate::heap::Oop;
use crate::method::Method;

/// A thrown Java exception propagating through Rust frames.
#[derive(Clone, Copy, Debug)]
pub struct Throw(pub Oop);

pub type JResult<T> = Result<T, Throw>;

/// Default value-stack size in 8-byte slots (64 MiB, committed lazily by the kernel).
const STACK_SLOTS: usize = 8 << 20;

pub struct Thread {
    stack: Box<[u64]>,
    /// First free slot of the value stack.
    pub(crate) sp: *mut u64,
    pub java_thread: Oop,
    /// Interpreter call depth, bounded to turn runaway recursion into StackOverflowError.
    pub(crate) depth: u32,
    /// Active frames, innermost last: the method and, for interpreted frames, a pointer to the
    /// frame's current bytecode index (null for natives). Used for stack traces and
    /// caller-sensitive natives.
    pub(crate) frames: Vec<(&'static Method, *const usize)>,
}

impl Thread {
    pub fn new() -> Thread {
        let mut stack = vec![0u64; STACK_SLOTS].into_boxed_slice();
        let sp = stack.as_mut_ptr();
        Thread { stack, sp, java_thread: std::ptr::null_mut(), depth: 0, frames: Vec::with_capacity(256) }
    }

    /// One past the last usable slot.
    pub(crate) fn stack_limit(&self) -> *mut u64 {
        // SAFETY: one-past-the-end pointer of the owned slice.
        unsafe { (self.stack.as_ptr() as *mut u64).add(self.stack.len()) }
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

    /// The method of the `n`th active frame counting outwards from the innermost (0).
    pub fn caller_frame(&self, n: usize) -> Option<(&'static Method, i32)> {
        let &(m, bci) = self.frames.iter().rev().nth(n)?;
        // SAFETY: a non-null bci pointer targets the live interpreter frame of that entry.
        Some((m, if bci.is_null() { -1 } else { unsafe { *bci as i32 } }))
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl Default for Thread {
    fn default() -> Self {
        Self::new()
    }
}
