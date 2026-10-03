//! The low 2 GiB of the address space (`MAP_32BIT`), for VM structures generated code names by
//! a 32-bit immediate or an absolute displacement instead of a 64-bit constant loaded into a
//! register: classes (compared in type guards and stored in object headers) and the class
//! hierarchy flags of methods (tested where calls were inlined), as HotSpot keeps classes in
//! its compressed class space. Nothing here is freed; without low memory left, values go to the
//! ordinary heap.

use std::sync::Mutex;

/// Moves `value` into low memory where possible, for the rest of the run.
pub fn leak<T>(value: T) -> &'static T {
    const CHUNK: usize = 4 << 20;
    static ARENA: Mutex<(usize, usize)> = Mutex::new((0, 0));
    let (size, align) = (std::mem::size_of::<T>(), std::mem::align_of::<T>());
    let mut arena = ARENA.lock().unwrap();
    let mut at = arena.0.next_multiple_of(align);
    if arena.0 == 0 || at + size > arena.1 {
        // SAFETY: a fresh anonymous mapping.
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), CHUNK, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT, -1, 0)
        };
        if p == libc::MAP_FAILED {
            return Box::leak(Box::new(value));
        }
        *arena = (p as usize, p as usize + CHUNK);
        at = arena.0;
    }
    arena.0 = at + size;
    // SAFETY: an aligned, unused slot of the arena, never freed.
    unsafe {
        std::ptr::write(at as *mut T, value);
        &*(at as *const T)
    }
}

/// Whether `addr` fits a sign-extended 32-bit immediate or displacement.
pub fn is_low(addr: usize) -> bool {
    addr <= i32::MAX as usize
}
