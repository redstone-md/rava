//! Heap memory: 256 KiB pages of equal-size cells (segregated size classes) for small objects
//! in the lower three quarters of the reservation, and blocks of the large-object area
//! (`large.rs`) above them. Allocation pages take new objects; old pages only the objects
//! collections move (`evacuate.rs`).
//!
//! Each page keeps two bitmaps: `alloc` (cells handed out since the page was last swept, or that
//! survived it) and `marks` (reached by the current collection). A cell holds an object iff its
//! `alloc` bit is set and its first word (the class pointer) is non-zero. Sweeping only swaps and
//! counts bitmaps; dead cells are never touched until they are handed out again, as part of a
//! run of free cells that is bump-allocated, each object cleared as it is made (so compiled code
//! allocates from any run the same way). Fresh and recycled pages are handed out whole.

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
    /// Owned by a thread's allocation cache.
    pub owned: bool,
    /// Holds objects a young collection moved out of allocation pages (`mark.rs`): only
    /// collections allocate here, so allocation pages hold young objects and few old ones.
    pub old: bool,
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
    pub(super) fn set_alloc(&mut self, first: usize, end: usize, set: bool) {
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
        self.next_free_run_of((MIN_RUN / self.cell_size as usize).max(1))
    }

    /// The next run of at least `min` free cells at or after the cursor, marked allocated.
    fn next_free_run_of(&mut self, min: usize) -> Option<(usize, usize)> {
        let cells = self.cells();
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

/// The next run of free cells of the page at `page` (metadata `meta`): `(start, stop)`, marked
/// allocated and handed to the caller as they are. Allocation clears each object as it makes
/// it (HotSpot's TLAB allocation), so the cells are written once, as their objects are
/// initialized; clearing whole runs ahead of allocation stalled on every line of memory
/// last touched a collection ago. Cells a run hands out but no object took yet are released
/// before the collector's conservative scan could take them for objects (`Heap::collect_as`).
pub fn next_run_in(meta: &mut PageMeta, page: usize) -> Option<(usize, usize)> {
    let (first, last) = meta.next_free_run()?;
    let cell = meta.cell_size as usize;
    Some((page + first * cell, page + last * cell))
}

pub struct Large {
    pub size: usize,
    pub marked: bool,
}

pub struct Space {
    pub base: usize,
    pub end: usize,
    /// Bytes reserved for the heap (pages and large objects).
    pub reserved: usize,
    /// Next never-used page.
    pub fresh: usize,
    pub pages: Vec<Option<PageMeta>>,
    /// Released pages, ready for any size class.
    pub empty: Vec<usize>,
    /// Empty pages given back to the kernel (zero when touched again), taken after `empty`.
    pub released: Vec<usize>,
    /// Free bytes of the allocation pages in `partial` (as the last sweep left them).
    pub partial_free: usize,
    /// Allocation pages with free cells, per class.
    pub partial: [Vec<usize>; CLASSES],
    /// Old pages with free cells, per class, for objects collections move.
    pub old_partial: [Vec<usize>; CLASSES],
    pub large: BTreeMap<usize, Large>,
    /// Bytes of the objects in `large` (kept with it, for `Heap::used_bytes`).
    pub large_bytes: usize,
    pub large_area: LargeArea,
    /// Bytes handed to allocation caches or large objects since the last collection.
    pub allocated_since_gc: usize,
    pub live_after_gc: usize,
    /// Live bytes after the last full collection.
    pub live_after_full: usize,
    /// Cells whose objects young collections promoted lately, with how many young collections
    /// they have survived (sorted, disjoint ranges): they count as young again in the next one,
    /// until they have survived `mark::TENURE` (`mark.rs`).
    pub aging: Vec<(usize, usize, u8)>,
    /// Young collections a promotion stays young again for (`mark::tenure_after`).
    pub tenure: u8,
}

/// The smallest heap reservation `Space::new` falls back to.
const MIN_RESERVE: usize = 256 << 20;

impl Space {
    pub fn new(reserve: usize) -> Space {
        let reserve = reserve.min(if crate::heap::COMPRESSED { NARROW_END - NARROW_BASE - PAGE_SIZE } else { usize::MAX }) & !(PAGE_SIZE - 1);
        // Compressed references need the heap below 32 GiB (zero-based, `heap::NARROW_SHIFT`):
        // above the low 4 GiB (classes and code, `crate::low`), as HotSpot places it.
        // The first free range from 4 GiB up in steps of 1 GiB (the binary itself may sit there).
        // Where libraries or stacks break that range up (address space layout randomization),
        // a smaller reservation, by eighths, down to 256 MiB.
        let mut reserve = reserve;
        let raw = loop {
            let raw = if crate::heap::COMPRESSED {
                (NARROW_BASE..=NARROW_END.saturating_sub(reserve + PAGE_SIZE)).step_by(1 << 30).find_map(|at| crate::os::reserve_uncommitted(reserve + PAGE_SIZE, Some(at)))
            } else {
                crate::os::reserve_uncommitted(reserve + PAGE_SIZE, None)
            };
            match raw {
                Some(raw) => break raw,
                None if reserve > MIN_RESERVE => reserve = (reserve / 8 * 7).max(MIN_RESERVE) & !(PAGE_SIZE - 1),
                None => panic!("cannot reserve {reserve} bytes of heap"),
            }
        };
        assert!(!crate::heap::COMPRESSED || raw as usize + reserve + PAGE_SIZE <= NARROW_END, "heap reservation above 32 GiB");
        let base = (raw as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let pages_end = base + (reserve / 4 * 3 & !(PAGE_SIZE - 1));
        // Transparent huge pages where the system offers them on request: object graphs spread
        // over gigabytes then miss the TLB far less (released pages split them again).
        crate::os::huge_pages(base, reserve);
        super::cards::init(base, reserve);
        super::prefault::start((base, pages_end), (pages_end, base + reserve));
        Space {
            base,
            reserved: reserve,
            end: pages_end,
            fresh: base,
            pages: (0..(pages_end - base) / PAGE_SIZE).map(|_| None).collect(),
            empty: Vec::new(),
            released: Vec::new(),
            partial_free: 0,
            partial: std::array::from_fn(|_| Vec::new()),
            old_partial: std::array::from_fn(|_| Vec::new()),
            large: BTreeMap::new(),
            large_bytes: 0,
            large_area: LargeArea::new(pages_end, base + reserve),
            allocated_since_gc: 0,
            live_after_gc: 0,
            live_after_full: 0,
            aging: Vec::new(),
            tenure: super::mark::TENURE,
        }
    }

    pub fn page_index(&self, addr: usize) -> Option<usize> {
        (addr >= self.base && addr < self.end).then(|| (addr - self.base) >> PAGE_SHIFT)
    }

    pub fn page_addr(&self, index: usize) -> usize {
        self.base + (index << PAGE_SHIFT)
    }

    /// A page of size class `class` with a free run, owned by the caller from now on, and that
    /// run (`next_run_in`): `(page, start, end)`. A reused page with only short holes left goes back
    /// unowned until the next collection sweeps it again.
    /// Bytes of pages in use (not empty, not released) and of live large blocks.
    pub fn in_use(&self) -> usize {
        (self.fresh - self.base) - (self.empty.len() + self.released.len()) * PAGE_SIZE + self.large_bytes
    }

    pub fn take_run(&mut self, class: usize) -> Option<(usize, usize, usize)> {
        while let Some(p) = self.partial[class].pop() {
            let addr = self.page_addr(p);
            let meta = self.pages[p].as_mut().expect("partial page");
            if meta.owned {
                continue;
            }
            if let Some((start, end)) = next_run_in(meta, addr) {
                meta.owned = true;
                return Some((p, start, end));
            }
        }
        let p = self.take_page(class, false)?;
        let (start, end) = self.next_run(p).expect("a fresh or empty page has free cells");
        Some((p, start, end))
    }

    /// A run of free cells of class `class` in an old page, for objects a collection moves:
    /// `(page, start, end)`. Its cells are not cleared (each is overwritten whole) and stay free
    /// unless marked. Any hole serves: the collector, not compiled code, fills it.
    pub fn old_run(&mut self, class: usize) -> Option<(usize, usize, usize)> {
        loop {
            let p = match self.old_partial[class].last() {
                Some(&p) => p,
                None => {
                    let p = self.take_page(class, true)?;
                    self.old_partial[class].push(p);
                    p
                }
            };
            let page = self.page_addr(p);
            let meta = self.pages[p].as_mut().expect("old page");
            if let Some((first, end)) = meta.next_free_run_of(1) {
                let cell = meta.cell_size as usize;
                return Some((p, page + first * cell, page + end * cell));
            }
            self.old_partial[class].pop();
        }
    }

    /// An empty or fresh page of size class `class`, owned by the caller from now on (an old
    /// page, for `old_run`, if `old`).
    fn take_page(&mut self, class: usize, old: bool) -> Option<usize> {
        let cell = CLASS_SIZES[class] as usize;
        // A recycled page keeps its memory committed (cheaper to clear than to fault in again).
        let addr = match self.empty.pop() {
            Some(a) => a,
            None if !self.released.is_empty() => {
                let a = self.released.pop().unwrap();
                // Discarded: committed again before use.
                crate::os::commit(a, PAGE_SIZE);
                a
            }
            None if self.fresh + PAGE_SIZE <= self.end => {
                crate::os::commit(self.fresh, PAGE_SIZE);
                self.fresh += PAGE_SIZE;
                super::prefault::advanced(super::prefault::Region::Small, self.fresh);
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
            owned: !old,
            old,
        });
        Some(index)
    }

    /// Gives empty pages back to the kernel once the free space of allocation pages and the
    /// committed empty pages exceeds `keep` bytes (what the next allocations need) by more than
    /// `RELEASE_SLACK` twice over, down to that slack: releasing a little after every collection
    /// only made the next allocations fault it in again.
    pub fn release_surplus(&mut self, keep: usize) {
        const RELEASE_SLACK: usize = 32 << 20;
        if self.partial_free + self.empty.len() * PAGE_SIZE <= keep + 2 * RELEASE_SLACK {
            return;
        }
        while self.partial_free + self.empty.len() * PAGE_SIZE > keep + RELEASE_SLACK {
            let Some(page) = self.empty.pop() else { break };
            // An empty page of the reservation; nothing references it.
            crate::os::discard(page, PAGE_SIZE);
            self.released.push(page);
        }
    }

    /// The next run of free cells of owned page `index` (`next_run_in`).
    pub fn next_run(&mut self, index: usize) -> Option<(usize, usize)> {
        let page = self.page_addr(index);
        next_run_in(self.pages[index].as_mut().expect("owned page"), page)
    }



    pub fn alloc_large(&mut self, bytes: usize) -> Option<usize> {
        let (p, size) = self.large_area.alloc(bytes)?;
        self.large.insert(p, Large { size, marked: false });
        self.large_bytes += size;
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
            owned: true,
            old: false,
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
                owned: false,
                old: false,
            };
            for offset in (0..PAGE_SIZE).step_by(8) {
                assert_eq!(meta.cell_of(offset), offset / size as usize, "size {size} offset {offset}");
            }
        }
    }
}
