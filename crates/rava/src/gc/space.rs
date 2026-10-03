//! Heap memory: 256 KiB pages of equal-size cells (segregated size classes) for small objects
//! in the lower three quarters of the reservation, and blocks of the large-object area
//! (`large.rs`) above them. Nothing moves.
//!
//! Each page keeps two bitmaps: `alloc` (cells handed out since the page was last swept, or that
//! survived it) and `marks` (reached by the current collection). A cell holds an object iff its
//! `alloc` bit is set and its first word (the class pointer) is non-zero. Sweeping only swaps and
//! counts bitmaps; dead cells are never touched until they are handed out again, as part of a
//! run of free cells that is zeroed then and bump-allocated (so compiled code allocates from any
//! run the same way). Fresh and recycled pages are handed out whole.

use super::large::LargeArea;
use std::collections::BTreeMap;

pub const PAGE_SHIFT: usize = 18;
/// Where the heap is reserved with compressed references, and the end of the range they reach.
const NARROW_BASE: usize = 4 << 30;
const NARROW_END: usize = 1 << (32 + crate::heap::NARROW_SHIFT);
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
/// Objects above this size get their own mapping.
pub const LARGE_THRESHOLD: usize = 32 << 10;

/// The shortest hole worth a bump run in a reused page (`PageMeta::next_free_run`). 2 KiB was no
/// faster on the server start (A/B within noise) and left pages of small cells with scattered
/// holes unreused: the heap faulted in 0.3-0.6 GiB more fresh memory (peak RSS 1.9 -> 2.5 GiB).
const MIN_RUN: usize = 256;

pub const CLASS_SIZES: [u32; 59] = [
    16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, 128, 136, 144, 152, 160, 168, 176, 184, 192, 200, //
    208, 216, 224, 232, 240, 248, 256, //
    320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, //
    2560, 3072, 3584, 4096, 5120, 6144, 7168, 8192, //
    10240, 12288, 14336, 16384, 20480, 24576, 28672, 32768,
];
pub const CLASSES: usize = CLASS_SIZES.len();

/// Size class for an allocation of `bytes` (≤ LARGE_THRESHOLD).
#[inline]
pub fn class_of(bytes: usize) -> usize {
    if bytes <= 256 {
        return (bytes.max(16) + 7) / 8 - 2;
    }
    CLASS_SIZES.iter().position(|&s| s as usize >= bytes).expect("small object")
}

/// Side metadata of a page in use.
pub struct PageMeta {
    pub class: u8,
    pub cell_size: u32,
    /// `ceil(2^40 / cell_size)`: `offset * recip >> 40` is the cell index of any page offset
    /// (exact for offsets below 2^18), without a division on the marking path.
    pub recip: u64,
    pub alloc: Box<[u64]>,
    pub marks: Box<[u64]>,
    /// Next cell to look for a free run from.
    pub cursor: usize,
    /// Free cells are known to be zero (a fresh or recycled page), so runs need no clearing.
    pub zeroed: bool,
    /// Owned by a thread's allocation cache.
    pub owned: bool,
}

impl PageMeta {
    pub fn cells(&self) -> usize {
        PAGE_SIZE / self.cell_size as usize
    }

    #[inline]
    pub fn cell_of(&self, offset: usize) -> usize {
        ((offset as u64 * self.recip) >> 40) as usize
    }

    fn allocated(&self, cell: usize) -> bool {
        self.alloc[cell / 64] & (1 << (cell % 64)) != 0
    }

    /// The first cell at or after `from` (below `cells`) whose allocation bit equals `set`.
    fn find_bit(&self, from: usize, cells: usize, set: bool) -> Option<usize> {
        let word = |w: usize| self.alloc.get(w).map(|&x| if set { x } else { !x });
        let mut w = from / 64;
        let mut bits = word(w)? & (!0u64 << (from % 64));
        loop {
            if bits != 0 {
                let i = w * 64 + bits.trailing_zeros() as usize;
                return (i < cells).then_some(i);
            }
            w += 1;
            bits = word(w)?;
        }
    }

    /// Sets (or clears) the allocation bits of cells `first..end`.
    fn set_alloc(&mut self, first: usize, end: usize, set: bool) {
        let mut i = first;
        while i < end {
            let (w, b) = (i / 64, i % 64);
            let n = (end - i).min(64 - b);
            let mask = (!0u64 >> (64 - n)) << b;
            if set {
                self.alloc[w] |= mask;
            } else {
                self.alloc[w] &= !mask;
            }
            i += n;
        }
    }

    /// The next run of free cells at or after the cursor, marked allocated: `(first, end)`.
    /// Holes under `MIN_RUN` bytes are passed over (as Immix skips partly used lines): each run
    /// costs compiled code a trip through the runtime, and reused pages are full of one- and
    /// two-cell holes. They stay free for a later pass, once their neighbours have died too.
    fn next_free_run(&mut self) -> Option<(usize, usize)> {
        let cells = self.cells();
        let min = (MIN_RUN / self.cell_size as usize).max(1);
        let (first, end) = loop {
            let Some(first) = self.find_bit(self.cursor, cells, false) else {
                self.cursor = cells;
                return None;
            };
            let end = self.find_bit(first, cells, true).unwrap_or(cells);
            if end - first >= min {
                break (first, end);
            }
            self.cursor = end;
        };
        self.set_alloc(first, end, true);
        self.cursor = end;
        Some((first, end))
    }
}

/// How much of a run of reused cells is cleared at a time, just before it is bump-allocated:
/// clearing a whole run (up to a page) at once streams it out of the cache, and allocation
/// then misses on every line again.
const ZERO_CHUNK: usize = 4096;

/// The next run of free cells of the page at `page` (metadata `meta`): `(start, end, stop)`,
/// the cells `start..stop` handed to the caller, of which `start..end` are zeroed and ready to
/// bump-allocate. On a reused page only a first chunk is zeroed; the cells after it are left
/// out of the allocation bitmap until `next_chunk` zeroes them (a cell marked allocated always
/// holds an object or zeros, which the collector's conservative scan relies on).
pub fn next_run_in(meta: &mut PageMeta, page: usize) -> Option<(usize, usize, usize)> {
    let (first, last) = meta.next_free_run()?;
    let cell = meta.cell_size as usize;
    let (start, stop) = (page + first * cell, page + last * cell);
    if meta.zeroed {
        return Some((start, stop, stop));
    }
    let end = chunk_end(start, stop, cell);
    meta.set_alloc((end - page) / cell, last, false);
    // SAFETY: free cells of a page in use, handed to the caller only now.
    unsafe { zero(start, end) };
    Some((start, end, stop))
}

/// Zeroes the next chunk of a run from `from` (a cell boundary below `stop`) and marks its
/// cells allocated; returns the chunk's end.
pub fn next_chunk(meta: &mut PageMeta, page: usize, from: usize, stop: usize) -> usize {
    let cell = meta.cell_size as usize;
    let end = chunk_end(from, stop, cell);
    // SAFETY: cells of a run this thread owns, not yet handed to the allocator.
    unsafe { zero(from, end) };
    meta.set_alloc((from - page) / cell, (end - page) / cell, true);
    end
}

/// The end of the chunk of a run of `cell`-sized cells from `start`: whole cells, `ZERO_CHUNK`
/// bytes or at least one cell, within `stop`.
fn chunk_end(start: usize, stop: usize, cell: usize) -> usize {
    (start + (ZERO_CHUNK / cell).max(1) * cell).min(stop)
}

/// # Safety
/// `start..stop` must be heap memory no object occupies.
unsafe fn zero(start: usize, stop: usize) {
    if stop - start <= 256 {
        // Short runs are common in reused pages: clear them without a call (volatile, so the
        // loop is not turned back into one).
        let mut p = start;
        while p < stop {
            (p as *mut u64).write_volatile(0);
            p += 8;
        }
    } else {
        std::ptr::write_bytes(start as *mut u8, 0, stop - start);
    }
}

pub struct Large {
    pub size: usize,
    pub marked: bool,
}

pub struct Space {
    pub base: usize,
    pub end: usize,
    /// Next never-used page.
    pub fresh: usize,
    pub pages: Vec<Option<PageMeta>>,
    /// Released pages, ready for any size class.
    pub empty: Vec<usize>,
    /// Pages with free cells, per class.
    pub partial: [Vec<usize>; CLASSES],
    pub large: BTreeMap<usize, Large>,
    pub large_area: LargeArea,
    /// Bytes handed to allocation caches or large objects since the last collection.
    pub allocated_since_gc: usize,
    pub live_after_gc: usize,
    /// Live bytes after the last full collection.
    pub live_after_full: usize,
}

impl Space {
    pub fn new(reserve: usize) -> Space {
        let reserve = reserve.min(if crate::heap::COMPRESSED { NARROW_END - NARROW_BASE - PAGE_SIZE } else { usize::MAX }) & !(PAGE_SIZE - 1);
        let reserve_at = |hint: usize, flags: libc::c_int| {
            // SAFETY: an anonymous, lazily committed reservation (at `hint` only if that range is
            // free: MAP_FIXED_NOREPLACE never replaces a mapping).
            unsafe {
                libc::mmap(
                    hint as *mut libc::c_void,
                    reserve + PAGE_SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | flags,
                    -1,
                    0,
                )
            }
        };
        // Compressed references need the heap below 32 GiB (zero-based, `heap::NARROW_SHIFT`):
        // above the low 4 GiB (classes and code, `crate::low`), as HotSpot places it.
        let raw = if crate::heap::COMPRESSED { reserve_at(NARROW_BASE, libc::MAP_FIXED_NOREPLACE) } else { reserve_at(0, 0) };
        assert!(raw != libc::MAP_FAILED, "cannot reserve {reserve} bytes of heap");
        assert!(!crate::heap::COMPRESSED || raw as usize + reserve + PAGE_SIZE <= NARROW_END, "heap reservation above 32 GiB");
        let base = (raw as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let pages_end = base + (reserve / 4 * 3 & !(PAGE_SIZE - 1));
        super::cards::init(base, reserve);
        super::prefault::start((base, pages_end), (pages_end, base + reserve));
        Space {
            base,
            end: pages_end,
            fresh: base,
            pages: (0..(pages_end - base) / PAGE_SIZE).map(|_| None).collect(),
            empty: Vec::new(),
            partial: std::array::from_fn(|_| Vec::new()),
            large: BTreeMap::new(),
            large_area: LargeArea::new(pages_end, base + reserve),
            allocated_since_gc: 0,
            live_after_gc: 0,
            live_after_full: 0,
        }
    }

    pub fn page_index(&self, addr: usize) -> Option<usize> {
        (addr >= self.base && addr < self.end).then(|| (addr - self.base) >> PAGE_SHIFT)
    }

    pub fn page_addr(&self, index: usize) -> usize {
        self.base + (index << PAGE_SHIFT)
    }

    /// A page of size class `class` with a free run, owned by the caller from now on, and that
    /// run (`next_run_in`): `(page, start, end, stop)`. A reused page with only short holes left goes back
    /// unowned until the next collection sweeps it again.
    pub fn take_run(&mut self, class: usize) -> Option<(usize, usize, usize, usize)> {
        while let Some(p) = self.partial[class].pop() {
            let addr = self.page_addr(p);
            let meta = self.pages[p].as_mut().expect("partial page");
            if meta.owned {
                continue;
            }
            if let Some((start, end, stop)) = next_run_in(meta, addr) {
                meta.owned = true;
                return Some((p, start, end, stop));
            }
        }
        let p = self.take_page(class)?;
        let (start, end, stop) = self.next_run(p).expect("a fresh or empty page has free cells");
        Some((p, start, end, stop))
    }

    /// An empty or fresh page of size class `class`, owned by the caller from now on.
    fn take_page(&mut self, class: usize) -> Option<usize> {
        let cell = CLASS_SIZES[class] as usize;
        // A recycled page keeps its memory committed (cheaper to clear than to fault in again)
        // and is cleared chunk by chunk as it is allocated; a fresh one is zero already.
        let (addr, zeroed) = match self.empty.pop() {
            Some(a) => (a, false),
            None if self.fresh + PAGE_SIZE <= self.end => {
                self.fresh += PAGE_SIZE;
                super::prefault::advanced(super::prefault::Region::Small, self.fresh);
                (self.fresh - PAGE_SIZE, true)
            }
            None => return None,
        };
        let index = self.page_index(addr).unwrap();
        let cells = PAGE_SIZE / cell;
        self.pages[index] = Some(PageMeta {
            recip: ((1u64 << 40) + cell as u64 - 1) / cell as u64,
            class: class as u8,
            cell_size: cell as u32,
            alloc: vec![0u64; cells.div_ceil(64)].into_boxed_slice(),
            marks: vec![0u64; cells.div_ceil(64)].into_boxed_slice(),
            cursor: 0,
            zeroed,
            owned: true,
        });
        Some(index)
    }

    /// The next run of free cells of owned page `index` (`next_run_in`).
    pub fn next_run(&mut self, index: usize) -> Option<(usize, usize, usize)> {
        let page = self.page_addr(index);
        next_run_in(self.pages[index].as_mut().expect("owned page"), page)
    }



    pub fn alloc_large(&mut self, bytes: usize) -> Option<usize> {
        let (p, size) = self.large_area.alloc(bytes)?;
        self.large.insert(p, Large { size, marked: false });
        self.allocated_since_gc += size;
        Some(p)
    }

    /// The object (cell or large block) containing `addr`, if `addr` points into a live one.
    pub fn find_object(&self, addr: usize) -> Option<usize> {
        if let Some(index) = self.page_index(addr) {
            let meta = self.pages[index].as_ref()?;
            let page = self.page_addr(index);
            let cell = meta.cell_of(addr - page);
            if cell >= meta.cells() {
                return None;
            }
            if !meta.allocated(cell) {
                return None;
            }
            let start = page + cell * meta.cell_size as usize;
            // SAFETY: inside a page in use.
            let klass = unsafe { *(start as *const u32) };
            return (klass != 0).then_some(start);
        }
        let (&start, l) = self.large.range(..=addr).next_back()?;
        (addr < start + l.size).then_some(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_runs_skip_allocated_cells() {
        // 128 cells of 2 KiB: cells 0, 1, 5 and 64..=69 allocated.
        let mut meta = PageMeta {
            recip: 0,
            class: 0,
            cell_size: 2048,
            alloc: vec![0b10_0011, 0b11_1111].into_boxed_slice(),
            marks: vec![0, 0].into_boxed_slice(),
            cursor: 0,
            zeroed: false,
            owned: true,
        };
        assert_eq!(meta.next_free_run(), Some((2, 5)));
        assert_eq!(meta.next_free_run(), Some((6, 64)));
        assert_eq!(meta.next_free_run(), Some((70, 128)));
        assert_eq!(meta.next_free_run(), None);
        assert!(meta.alloc.iter().all(|&w| w == !0));
    }

    #[test]
    fn reciprocal_cell_index_is_exact() {
        for &size in CLASS_SIZES.iter() {
            let meta = PageMeta {
                recip: ((1u64 << 40) + size as u64 - 1) / size as u64,
                class: 0,
                cell_size: size,
                alloc: Box::new([]),
                marks: Box::new([]),
                cursor: 0,
                zeroed: true,
                owned: false,
            };
            for offset in (0..PAGE_SIZE).step_by(8) {
                assert_eq!(meta.cell_of(offset), offset / size as usize, "size {size} offset {offset}");
            }
        }
    }
}
