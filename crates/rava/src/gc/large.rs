//! The large-object area: the upper part of the heap reservation, handed out in 4 KiB-aligned
//! blocks (first fit over a free list, else fresh memory). Freed blocks keep their memory and
//! are cleared when handed out again, as a JVM clears reused heap: giving every freed block back
//! to the kernel (`MADV_DONTNEED`) made each reuse fault it in and clear it there again, after a
//! TLB shootdown across the process's threads. Only free memory beyond a retained margin is given
//! back after a collection (`trim`), the largest blocks first: early bursts of large arrays (a
//! launcher reading whole jars) otherwise stay resident for the rest of the run. Keeping large
//! objects inside the reservation lets the card table cover them.

use std::collections::BTreeMap;

pub const BLOCK: usize = 4096;

/// A free block: its size, and whether it may hold non-zero bytes (released and never-used
/// memory reads as zeros and needs no clearing).
#[derive(Clone, Copy)]
struct Free {
    size: usize,
    dirty: bool,
}

pub struct LargeArea {
    pub base: usize,
    pub end: usize,
    /// Next never-used address.
    pub fresh: usize,
    /// Free blocks by address, coalesced.
    free: BTreeMap<usize, Free>,
    /// Bytes of dirty free blocks (resident memory nothing uses).
    dirty_bytes: usize,
}

impl LargeArea {
    pub fn new(base: usize, end: usize) -> LargeArea {
        LargeArea { base, end, fresh: base, free: BTreeMap::new(), dirty_bytes: 0 }
    }

    /// A zeroed block of at least `bytes`: `(address, size)`.
    pub fn alloc(&mut self, bytes: usize) -> Option<(usize, usize)> {
        let size = (bytes + BLOCK - 1) & !(BLOCK - 1);
        let fit = self.free.iter().find(|(_, f)| f.size >= size).map(|(&a, &f)| (a, f));
        if let Some((a, f)) = fit {
            self.free.remove(&a);
            if f.size > size {
                self.free.insert(a + size, Free { size: f.size - size, dirty: f.dirty });
            }
            if f.dirty {
                self.dirty_bytes -= size;
                // SAFETY: a free block of the area, no longer read by anything.
                unsafe { std::ptr::write_bytes(a as *mut u8, 0, size) };
            }
            return Some((a, size));
        }
        if self.fresh + size > self.end {
            return None;
        }
        crate::os::commit(self.fresh, size);
        self.fresh += size;
        super::prefault::advanced(super::prefault::Region::Large, self.fresh);
        Some((self.fresh - size, size))
    }

    /// Returns a block (cleared when handed out again).
    pub fn free(&mut self, addr: usize, size: usize) {
        let (mut a, mut f) = (addr, Free { size, dirty: true });
        self.dirty_bytes += size;
        if let Some((&next, &n)) = self.free.range(a + f.size..).next() {
            if next == a + f.size {
                self.free.remove(&next);
                self.merge(&mut f, n);
            }
        }
        if let Some((&prev, &p)) = self.free.range(..a).next_back() {
            if prev + p.size == a {
                self.free.remove(&prev);
                a = prev;
                self.merge(&mut f, p);
            }
        }
        self.free.insert(a, f);
    }

    /// Joins free neighbour `n` into `f`. A clean block joined to a dirty one counts as dirty.
    fn merge(&mut self, f: &mut Free, n: Free) {
        if !n.dirty {
            self.dirty_bytes += n.size;
        }
        f.size += n.size;
    }

    /// Gives dirty free memory beyond `keep` bytes back to the kernel, the largest blocks first.
    pub fn trim(&mut self, keep: usize) {
        if self.dirty_bytes <= keep {
            return;
        }
        let mut dirty: Vec<(usize, usize)> = self.free.iter().filter(|(_, f)| f.dirty).map(|(&a, f)| (a, f.size)).collect();
        dirty.sort_unstable_by_key(|&(_, s)| std::cmp::Reverse(s));
        for (a, size) in dirty {
            if self.dirty_bytes <= keep {
                break;
            }
            // A free block of the area; nothing reads it until it is handed out again.
            if crate::os::discard(a, size) {
                self.free.get_mut(&a).expect("free block").dirty = false;
                self.dirty_bytes -= size;
            }
        }
    }
}
