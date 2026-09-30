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
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
/// Objects above this size get their own mapping.
pub const LARGE_THRESHOLD: usize = 32 << 10;

pub const CLASS_SIZES: [u32; 44] = [
    16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 256, //
    320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, //
    2560, 3072, 3584, 4096, 5120, 6144, 7168, 8192, //
    10240, 12288, 14336, 16384, 20480, 24576, 28672, 32768,
];
pub const CLASSES: usize = CLASS_SIZES.len();

/// Size class for an allocation of `bytes` (≤ LARGE_THRESHOLD).
#[inline]
pub fn class_of(bytes: usize) -> usize {
    if bytes <= 256 {
        return (bytes.max(1) + 15) / 16 - 1;
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

    /// The next run of free cells at or after the cursor, marked allocated: `(first, end)`.
    fn next_free_run(&mut self) -> Option<(usize, usize)> {
        let cells = self.cells();
        let Some(first) = self.find_bit(self.cursor, cells, false) else {
            self.cursor = cells;
            return None;
        };
        let end = self.find_bit(first, cells, true).unwrap_or(cells);
        for i in first..end {
            self.alloc[i / 64] |= 1 << (i % 64);
        }
        self.cursor = end;
        Some((first, end))
    }
}

/// The next run of free cells of the page at `page` (metadata `meta`) as a zeroed bump range
/// `(start, end)`.
pub fn next_run_in(meta: &mut PageMeta, page: usize) -> Option<(usize, usize)> {
    let (first, end) = meta.next_free_run()?;
    let cell = meta.cell_size as usize;
    let (start, stop) = (page + first * cell, page + end * cell);
    if !meta.zeroed {
        // SAFETY: free cells of a page in use, handed to the caller only now.
        unsafe { std::ptr::write_bytes(start as *mut u8, 0, stop - start) };
    }
    Some((start, stop))
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
        let reserve = reserve & !(PAGE_SIZE - 1);
        // SAFETY: an anonymous, lazily committed reservation, aligned to the page size below.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                reserve + PAGE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert!(raw != libc::MAP_FAILED, "cannot reserve {reserve} bytes of heap");
        let base = (raw as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let pages_end = base + (reserve / 4 * 3 & !(PAGE_SIZE - 1));
        super::cards::init(base, reserve);
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

    /// A page of size class `class` with free cells, owned by the caller from now on.
    pub fn take_page(&mut self, class: usize) -> Option<usize> {
        let cell = CLASS_SIZES[class] as usize;
        while let Some(p) = self.partial[class].pop() {
            let meta = self.pages[p].as_mut().expect("partial page");
            if meta.owned {
                continue;
            }
            meta.owned = true;
            return Some(p);
        }
        let addr = match self.empty.pop() {
            Some(a) => {
                // A recycled page keeps its memory committed (cheaper to clear than to fault in
                // again); bump allocation relies on zeroed memory.
                // SAFETY: an unused page of the heap.
                unsafe { std::ptr::write_bytes(a as *mut u8, 0, PAGE_SIZE) };
                a
            }
            None if self.fresh + PAGE_SIZE <= self.end => {
                self.fresh += PAGE_SIZE;
                self.fresh - PAGE_SIZE
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
            zeroed: true,
            owned: true,
        });
        Some(index)
    }

    /// The next run of free cells of owned page `index` as a zeroed bump range `(start, end)`.
    pub fn next_run(&mut self, index: usize) -> Option<(usize, usize)> {
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
            let klass = unsafe { *(start as *const usize) };
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
