//! The Java heap and the object model.
//!
//! Every object starts with a 16-byte header: the `Klass` pointer and a mark word (identity hash
//! and, later, lock/GC bits). Instance fields follow at fixed offsets. Arrays store their length
//! as an `i32` at offset 16 and elements from offset 24. A reference is a raw pointer to the
//! header; null is 0.
//!
//! Memory comes from the collector (`gc`), which never moves objects.

use crate::klass::Klass;
use std::sync::atomic::AtomicU64;

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

    /// Writes a field. A pointer-sized write may store a reference, so it marks the card
    /// (the generational write barrier); smaller ones cannot.
    #[inline(always)]
    pub unsafe fn write<T: Copy>(obj: Oop, offset: u32, value: T) {
        let at = (obj as *mut u8).add(offset as usize);
        std::ptr::write(at as *mut T, value);
        if std::mem::size_of::<T>() == 8 {
            crate::gc::cards::dirty(at as usize);
        }
    }

    /// Stores reference element `i` of an object array, with the write barrier.
    #[inline(always)]
    pub unsafe fn store_elem(array: Oop, i: usize, value: Oop) {
        let at = array_data::<Oop>(array).add(i);
        *at = value;
        crate::gc::cards::dirty(at as usize);
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

/// Writes the header of a new instance into zeroed memory `mem`.
///
/// # Safety
/// `mem` must be zeroed, 8-aligned and at least `klass.instance_size()` bytes.
pub unsafe fn init_instance(mem: *mut u8, klass: &'static Klass) -> Oop {
    std::ptr::write(mem as *mut *const Klass, klass);
    mem as Oop
}

/// Writes the header of a new array into zeroed memory `mem`.
///
/// # Safety
/// `mem` must be zeroed, 8-aligned and large enough for `length` elements.
pub unsafe fn init_array(mem: *mut u8, klass: &'static Klass, length: i32) -> Oop {
    std::ptr::write(mem as *mut *const Klass, klass);
    obj::write::<i32>(mem as Oop, ARRAY_LENGTH_OFFSET, length);
    mem as Oop
}

/// Bytes of an array with `length` elements of `elem_size` bytes.
pub fn array_bytes(elem_size: u32, length: i32) -> usize {
    ARRAY_BASE_OFFSET as usize + elem_size as usize * length as usize
}
