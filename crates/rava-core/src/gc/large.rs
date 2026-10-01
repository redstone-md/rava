//! The large-object area: the upper part of the heap reservation, handed out in 4 KiB-aligned
//! blocks (first fit over a free list, else fresh memory). Freed blocks give their memory back
//! to the kernel, which zeroes it for the next use. Keeping large objects inside the reservation
//! lets the card table cover them.

use std::collections::BTreeMap;

pub const BLOCK: usize = 4096;

pub struct LargeArea {
    pub base: usize,
    pub end: usize,
    /// Next never-used address.
    pub fresh: usize,
    /// Free blocks: address -> size, coalesced.
    free: BTreeMap<usize, usize>,
}

impl LargeArea {
    pub fn new(base: usize, end: usize) -> LargeArea {
        LargeArea { base, end, fresh: base, free: BTreeMap::new() }
    }

    /// A zeroed block of at least `bytes`: `(address, size)`.
    pub fn alloc(&mut self, bytes: usize) -> Option<(usize, usize)> {
        let size = (bytes + BLOCK - 1) & !(BLOCK - 1);
        let fit = self.free.iter().find(|(_, &s)| s >= size).map(|(&a, &s)| (a, s));
        if let Some((a, s)) = fit {
            self.free.remove(&a);
            if s > size {
                self.free.insert(a + size, s - size);
            }
            return Some((a, size));
        }
        if self.fresh + size > self.end {
            return None;
        }
        self.fresh += size;
        super::prefault::advanced(super::prefault::Region::Large, self.fresh);
        Some((self.fresh - size, size))
    }

    /// Returns a block; its memory goes back to the kernel.
    pub fn free(&mut self, addr: usize, size: usize) {
        // SAFETY: a block of the area no object occupies any more.
        unsafe { libc::madvise(addr as *mut libc::c_void, size, libc::MADV_DONTNEED) };
        let (mut a, mut s) = (addr, size);
        if let Some((&next, &ns)) = self.free.range(a + s..).next() {
            if next == a + s {
                self.free.remove(&next);
                s += ns;
            }
        }
        if let Some((&prev, &ps)) = self.free.range(..a).next_back() {
            if prev + ps == a {
                self.free.remove(&prev);
                a = prev;
                s += ps;
            }
        }
        self.free.insert(a, s);
    }
}
