//! The Java heap and the object model.
//!
//! Every object starts with a 16-byte header: the `Klass` pointer and a mark word (identity hash
//! and, later, lock/GC bits). Instance fields follow at fixed offsets. Arrays store their length
//! as an `i32` at offset 16 and elements from offset 24. A reference is a raw pointer to the
//! header; null is 0.
//!
//! Allocation is a bump in one reserved region. The collector (non-moving mark-sweep with
//! conservative stack roots, see `docs/core/README.md`) is a later tracer bullet; this interface
//! is what it will sit behind.

use crate::klass::Klass;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const HEADER_SIZE: u32 = 16;
pub const ARRAY_LENGTH_OFFSET: u32 = 16;
pub const ARRAY_BASE_OFFSET: u32 = 24;

#[repr(C)]
pub struct ObjHeader {
    klass: *const Klass,
    mark: AtomicU64,
}

/// A Java reference. Copyable raw pointer; validity is the collector's contract.
pub type Oop = *mut ObjHeader;

/// Typed access to an object's memory. All functions require a live, non-null `obj` and an
/// offset inside it that was produced by the class's field layout or the array layout.
pub mod obj {
    use super::*;

    #[inline(always)]
    pub unsafe fn klass(obj: Oop) -> &'static Klass {
        &*(*obj).klass
    }

    #[inline(always)]
    pub unsafe fn mark(obj: Oop) -> &'static AtomicU64 {
        &(*obj).mark
    }

    #[inline(always)]
    pub unsafe fn read<T: Copy>(obj: Oop, offset: u32) -> T {
        std::ptr::read((obj as *const u8).add(offset as usize) as *const T)
    }

    #[inline(always)]
    pub unsafe fn write<T: Copy>(obj: Oop, offset: u32, value: T) {
        std::ptr::write((obj as *mut u8).add(offset as usize) as *mut T, value)
    }

    #[inline(always)]
    pub unsafe fn array_length(array: Oop) -> i32 {
        read::<i32>(array, ARRAY_LENGTH_OFFSET)
    }

    /// Pointer to element 0 of an array.
    #[inline(always)]
    pub unsafe fn array_data<T>(array: Oop) -> *mut T {
        (array as *mut u8).add(ARRAY_BASE_OFFSET as usize) as *mut T
    }

    /// Element slice of an array whose element type is `T`.
    #[inline(always)]
    pub unsafe fn array_slice<'a, T>(array: Oop) -> &'a mut [T] {
        std::slice::from_raw_parts_mut(array_data::<T>(array), array_length(array) as usize)
    }
}

/// Reserved address range, bump-allocated. Memory comes zeroed from the kernel.
pub struct Heap {
    base: usize,
    end: usize,
    top: AtomicUsize,
}

// The heap hands out disjoint ranges atomically.
unsafe impl Send for Heap {}
unsafe impl Sync for Heap {}

impl Heap {
    pub fn new(reserve_bytes: usize) -> Heap {
        // SAFETY: anonymous, private, lazily committed reservation.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                reserve_bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "cannot reserve {reserve_bytes} bytes of heap");
        let base = base as usize;
        Heap { base, end: base + reserve_bytes, top: AtomicUsize::new(base) }
    }

    /// `bytes` of zeroed, 8-aligned memory, or `None` when the heap is exhausted.
    #[inline]
    pub fn allocate(&self, bytes: usize) -> Option<*mut u8> {
        let size = (bytes + 7) & !7;
        let at = self.top.fetch_add(size, Ordering::Relaxed);
        (at + size <= self.end).then_some(at as *mut u8)
    }

    /// Allocates an instance of `klass` with its header written.
    pub fn alloc_instance(&self, klass: &'static Klass) -> Option<Oop> {
        let obj = self.allocate(klass.instance_size() as usize)? as Oop;
        // SAFETY: fresh, zeroed, large enough for the header.
        unsafe { std::ptr::write(obj as *mut *const Klass, klass) };
        Some(obj)
    }

    /// Allocates an array of `length` elements of `elem_size` bytes described by `klass`.
    pub fn alloc_array(&self, klass: &'static Klass, elem_size: u32, length: i32) -> Option<Oop> {
        let bytes = ARRAY_BASE_OFFSET as usize + elem_size as usize * length as usize;
        let obj = self.allocate(bytes)? as Oop;
        // SAFETY: fresh, zeroed, large enough for the array header.
        unsafe {
            std::ptr::write(obj as *mut *const Klass, klass);
            obj::write::<i32>(obj, ARRAY_LENGTH_OFFSET, length);
        }
        Some(obj)
    }

    pub fn used_bytes(&self) -> usize {
        self.top.load(Ordering::Relaxed) - self.base
    }

    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.top.load(Ordering::Relaxed)
    }
}
