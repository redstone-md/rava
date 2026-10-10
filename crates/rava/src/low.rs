//! The low 2 GiB of the address space (`MAP_32BIT`), for VM structures generated code names by
//! a 32-bit immediate or an absolute displacement instead of a 64-bit constant loaded into a
//! register: classes (compared in type guards and stored in object headers) and the class
//! hierarchy flags of methods (tested where calls were inlined), and methods (stored in frames
//! and passed to calls), as HotSpot keeps classes in
//! its compressed class space. Nothing here is freed; without low memory left, values go to the
//! ordinary heap. The arena is on transparent huge pages: dispatch and type checks read the
//! metadata of thousands of classes per tick, which 4 KiB pages spread over as many TLB entries.

use std::sync::Mutex;

const CHUNK: usize = 32 << 20;

/// `size` bytes aligned to `align` in low memory, or None (none left, or too large).
fn reserve(size: usize, align: usize) -> Option<usize> {
    static ARENA: Mutex<(usize, usize)> = Mutex::new((0, 0));
    if size > CHUNK / 4 {
        // (A class's vtable tail stays far below this.)
        return None;
    }
    let mut arena = ARENA.lock().unwrap();
    let mut at = arena.0.next_multiple_of(align);
    if arena.0 == 0 || at + size > arena.1 {
        // SAFETY: a fresh anonymous mapping.
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), CHUNK, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT, -1, 0)
        };
        if p == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the mapping just made.
        unsafe { libc::madvise(p, CHUNK, libc::MADV_HUGEPAGE) };
        *arena = (p as usize, p as usize + CHUNK);
        at = arena.0;
    }
    arena.0 = at + size;
    Some(at)
}

/// Moves `value` into low memory where possible, for the rest of the run.
pub fn leak<T>(value: T) -> &'static T {
    let Some(at) = reserve(std::mem::size_of::<T>(), std::mem::align_of::<T>()) else { return Box::leak(Box::new(value)) };
    // SAFETY: an aligned, unused slot of the arena, never freed.
    unsafe {
        std::ptr::write(at as *mut T, value);
        &*(at as *const T)
    }
}

/// Moves `value` into low memory followed by `tail` zeroed bytes (8-aligned, at
/// `size_of::<T>()` rounded up to 8), or None when none is left.
pub fn leak_with_tail<T>(value: T, tail: usize) -> Option<&'static T> {
    let head = std::mem::size_of::<T>().next_multiple_of(8);
    let at = reserve(head + tail, std::mem::align_of::<T>().max(8))?;
    // SAFETY: an aligned, unused run of the arena (fresh mappings are zeroed), never freed.
    unsafe {
        std::ptr::write(at as *mut T, value);
        Some(&*(at as *const T))
    }
}

/// Moves `values` into low memory where possible, for the rest of the run.
pub fn leak_slice<T>(values: Vec<T>) -> &'static [T] {
    let n = values.len();
    let Some(at) = reserve(std::mem::size_of::<T>() * n, std::mem::align_of::<T>()) else { return Box::leak(values.into_boxed_slice()) };
    // SAFETY: an aligned, unused run of the arena for `n` values, never freed.
    unsafe {
        for (i, v) in values.into_iter().enumerate() {
            std::ptr::write((at as *mut T).add(i), v);
        }
        std::slice::from_raw_parts(at as *const T, n)
    }
}

/// Whether `addr` fits a sign-extended 32-bit immediate or displacement.
pub fn is_low(addr: usize) -> bool {
    addr <= i32::MAX as usize
}
