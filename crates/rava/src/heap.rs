//! The Java heap and the object model.
//!
//! Every object starts with a 16-byte header: the `Klass` pointer in 32 bits (classes live in
//! low memory, `crate::low`), 4 bytes the object's own data may use, and at offset 8 the mark
//! word (identity hash, lock bits). An array stores its length in those 4 bytes and elements
//! from offset 16; an instance's first field of at most 4 bytes goes there, the others from
//! offset 16 (`loader::FieldLayout`). A reference is a raw pointer to the header; null is 0.
//!
//! Inside the heap (reference fields, `Object[]` elements) a reference may be stored compressed
//! (`COMPRESSED`): as a 32-bit value, the address shifted right by `NARROW_SHIFT`, as HotSpot's
//! zero-based compressed oops (the heap lies below 32 GiB, `gc::space`). Registers, frames,
//! statics and native code hold full pointers; `obj::read_ref`/`write_ref`/`load_elem`/
//! `store_elem` convert.
//!
//! Memory comes from the collector (`gc`), which never moves an object a root (a stack, a
//! native, the VM) holds.

use crate::klass::Klass;
use std::sync::atomic::AtomicU64;

pub const HEADER_SIZE: u32 = 16;
/// The 4 bytes of the header after the class pointer, free for the object's data.
pub const HEADER_GAP: u32 = 4;
pub const ARRAY_LENGTH_OFFSET: u32 = HEADER_GAP;
pub const ARRAY_BASE_OFFSET: u32 = 16;

/// Whether heap references are stored in 32 bits.
pub const COMPRESSED: bool = true;
/// A compressed reference is the address shifted right by this.
pub const NARROW_SHIFT: u32 = 3;
/// Bytes of a reference stored in the heap.
pub const REF_SIZE: u32 = if COMPRESSED { 4 } else { 8 };

/// The stored form of reference `o` (null stays 0).
#[inline(always)]
pub fn encode(o: Oop) -> u64 {
    if COMPRESSED {
        o as u64 >> NARROW_SHIFT
    } else {
        o as u64
    }
}

/// The reference stored as `raw`.
#[inline(always)]
pub fn decode(raw: u64) -> Oop {
    if COMPRESSED {
        (raw << NARROW_SHIFT) as Oop
    } else {
        raw as Oop
    }
}

#[repr(C)]
pub struct ObjHeader {
    /// The class's address (below 4 GiB).
    klass: u32,
    /// Data of the object (`HEADER_GAP`).
    gap: u32,
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
        &*((*obj).klass as usize as *const Klass)
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

    /// The reference stored at `at` (a field or element of a heap object).
    #[inline(always)]
    pub unsafe fn load_ref_at(at: usize) -> Oop {
        if COMPRESSED {
            decode(std::ptr::read(at as *const u32) as u64)
        } else {
            std::ptr::read(at as *const Oop)
        }
    }

    /// Stores reference `value` at `at` (a field or element of a heap object), with the write
    /// barrier.
    #[inline(always)]
    pub unsafe fn store_ref_at(at: usize, value: Oop) {
        if COMPRESSED {
            std::ptr::write(at as *mut u32, encode(value) as u32);
        } else {
            std::ptr::write(at as *mut Oop, value);
        }
        crate::gc::cards::dirty(at);
    }

    /// Stores reference `value` at `at` without the write barrier (the collector, rewriting a
    /// reference to a moved object).
    #[inline(always)]
    pub unsafe fn store_ref_raw(at: usize, value: Oop) {
        if COMPRESSED {
            std::ptr::write(at as *mut u32, encode(value) as u32);
        } else {
            std::ptr::write(at as *mut Oop, value);
        }
    }

    /// Reference field at `offset`.
    #[inline(always)]
    pub unsafe fn read_ref(obj: Oop, offset: u32) -> Oop {
        load_ref_at(obj as usize + offset as usize)
    }

    /// Writes reference field at `offset`, with the write barrier.
    #[inline(always)]
    pub unsafe fn write_ref(obj: Oop, offset: u32, value: Oop) {
        store_ref_at(obj as usize + offset as usize, value)
    }

    /// Address of element `i` of an object array.
    #[inline(always)]
    pub fn elem_addr(array: Oop, i: usize) -> usize {
        array as usize + ARRAY_BASE_OFFSET as usize + i * REF_SIZE as usize
    }

    /// Reference element `i` of an object array.
    #[inline(always)]
    pub unsafe fn load_elem(array: Oop, i: usize) -> Oop {
        load_ref_at(elem_addr(array, i))
    }

    /// Stores reference element `i` of an object array, with the write barrier.
    #[inline(always)]
    pub unsafe fn store_elem(array: Oop, i: usize, value: Oop) {
        store_ref_at(elem_addr(array, i), value)
    }

    /// The elements of an object array.
    pub unsafe fn ref_elems(array: Oop) -> Vec<Oop> {
        (0..array_length(array) as usize).map(|i| load_elem(array, i)).collect()
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

/// The header word naming `klass`.
#[inline(always)]
pub fn klass_word(klass: &'static Klass) -> u32 {
    let at = klass as *const Klass as usize;
    debug_assert!(at < 1 << 32, "classes live in low memory");
    at as u32
}

/// Writes the header of a new instance into zeroed memory `mem`.
///
/// # Safety
/// `mem` must be zeroed, 8-aligned and at least `klass.instance_size()` bytes.
pub unsafe fn init_instance(mem: *mut u8, klass: &'static Klass) -> Oop {
    std::ptr::write(mem as *mut u32, klass_word(klass));
    mem as Oop
}

/// Writes the header of a new array into zeroed memory `mem`.
///
/// # Safety
/// `mem` must be zeroed, 8-aligned and large enough for `length` elements.
pub unsafe fn init_array(mem: *mut u8, klass: &'static Klass, length: i32) -> Oop {
    std::ptr::write(mem as *mut u32, klass_word(klass));
    obj::write::<i32>(mem as Oop, ARRAY_LENGTH_OFFSET, length);
    mem as Oop
}

/// Bytes of an array with `length` elements of `elem_size` bytes.
pub fn array_bytes(elem_size: u32, length: i32) -> usize {
    ARRAY_BASE_OFFSET as usize + elem_size as usize * length as usize
}
