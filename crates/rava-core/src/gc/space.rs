//! Heap memory: 256 KiB pages of equal-size cells (segregated size classes) for small objects,
//! and individual mappings for large ones. Nothing moves.
//!
//! A cell holds an object iff its first word (the class pointer) is non-zero; a free cell has a
//! zero first word and the next free cell in its second word. Fresh pages come zeroed from the
//! kernel; recycled cells are zeroed when handed out.

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
    pub marks: Box<[u64]>,
    /// Head of the free-cell list built by the last sweep (0 = none).
    pub free: usize,
    /// Owned by a thread's allocation cache.
    pub owned: bool,
}

impl PageMeta {
    pub fn cells(&self) -> usize {
        PAGE_SIZE / self.cell_size as usize
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
    /// Bytes handed to allocation caches or large objects since the last collection.
    pub allocated_since_gc: usize,
    pub live_after_gc: usize,
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
        Space {
            base,
            end: base + reserve,
            fresh: base,
            pages: (0..reserve / PAGE_SIZE).map(|_| None).collect(),
            empty: Vec::new(),
            partial: std::array::from_fn(|_| Vec::new()),
            large: BTreeMap::new(),
            allocated_since_gc: 0,
            live_after_gc: 0,
        }
    }

    pub fn page_index(&self, addr: usize) -> Option<usize> {
        (addr >= self.base && addr < self.end).then(|| (addr - self.base) >> PAGE_SHIFT)
    }

    pub fn page_addr(&self, index: usize) -> usize {
        self.base + (index << PAGE_SHIFT)
    }

    /// A page for size class `class` with its free list and bump range, owned by the caller.
    pub fn take_page(&mut self, class: usize) -> Option<(usize, usize, usize, usize)> {
        let cell = CLASS_SIZES[class] as usize;
        while let Some(p) = self.partial[class].pop() {
            let meta = self.pages[p].as_mut().expect("partial page");
            if meta.owned {
                continue;
            }
            meta.owned = true;
            let free = std::mem::replace(&mut meta.free, 0);
            self.allocated_since_gc += PAGE_SIZE / 2;
            let addr = self.page_addr(p);
            return Some((p, free, addr, addr));
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
            class: class as u8,
            cell_size: cell as u32,
            marks: vec![0u64; cells.div_ceil(64)].into_boxed_slice(),
            free: 0,
            owned: true,
        });
        self.allocated_since_gc += PAGE_SIZE;
        // Bump through the whole page; only whole cells are handed out.
        Some((index, 0, addr, addr + cells * cell))
    }

    pub fn alloc_large(&mut self, bytes: usize) -> Option<usize> {
        let size = (bytes + 4095) & !4095;
        // SAFETY: an anonymous private mapping, zero-filled.
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if p == libc::MAP_FAILED {
            return None;
        }
        self.large.insert(p as usize, Large { size, marked: false });
        self.allocated_since_gc += size;
        Some(p as usize)
    }

    /// The object (cell or large block) containing `addr`, if `addr` points into a live one.
    pub fn find_object(&self, addr: usize) -> Option<usize> {
        if let Some(index) = self.page_index(addr) {
            let meta = self.pages[index].as_ref()?;
            let page = self.page_addr(index);
            let cell = (addr - page) / meta.cell_size as usize;
            if cell >= meta.cells() {
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
