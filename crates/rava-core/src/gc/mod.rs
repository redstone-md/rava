//! Non-moving mark-sweep collector (design: `docs/core/README.md`).
//!
//! Allocation goes through per-thread caches (one page per size class); a collection stops the
//! world at safepoints, marks from conservative stack roots and precise VM roots, and sweeps
//! pages into free lists. Objects never move, so raw references held anywhere stay valid.

mod mark;
pub mod safepoint;
mod space;

use crate::thread::Thread;
use space::{class_of, Space, CLASSES, CLASS_SIZES, LARGE_THRESHOLD, PAGE_SIZE};
use std::sync::Mutex;

/// A thread's allocation cache for one size class: a free list and a bump range in an owned page.
#[derive(Clone, Copy, Default)]
pub struct ClassCache {
    free: usize,
    bump: usize,
    end: usize,
}

pub struct AllocCache {
    classes: [ClassCache; CLASSES],
}

/// For inline allocation in compiled code: the size of a cell for an object of `bytes` (at
/// most `LARGE_THRESHOLD`), and the offsets within `AllocCache` of that size class's bump
/// pointer and bump limit. Bump memory is already zeroed.
pub(crate) fn bump_layout(bytes: usize) -> Option<(usize, usize, usize)> {
    if bytes > LARGE_THRESHOLD {
        return None;
    }
    let class = class_of(bytes);
    let base = std::mem::offset_of!(AllocCache, classes) + class * std::mem::size_of::<ClassCache>();
    Some((CLASS_SIZES[class] as usize, base + std::mem::offset_of!(ClassCache, bump), base + std::mem::offset_of!(ClassCache, end)))
}

impl Default for AllocCache {
    fn default() -> Self {
        AllocCache { classes: [ClassCache::default(); CLASSES] }
    }
}

pub struct Heap {
    space: Mutex<Space>,
    min_trigger: usize,
}

// The space is only touched under its lock (or with the world stopped).
unsafe impl Sync for Heap {}
unsafe impl Send for Heap {}

pub struct Stats {
    pub collections: u64,
    pub used: usize,
}

impl Heap {
    pub fn new(reserve: usize) -> Heap {
        let min_trigger = std::env::var("RAVA_CORE_GC_MB").ok().and_then(|v| v.parse::<usize>().ok()).map_or(64 << 20, |mb| mb << 20);
        Heap { space: Mutex::new(Space::new(reserve)), min_trigger }
    }

    /// `bytes` of zeroed, 8-aligned memory for `t`, collecting when the heap has grown enough.
    #[inline]
    pub fn allocate(&self, t: &mut Thread, bytes: usize) -> Option<*mut u8> {
        if bytes > LARGE_THRESHOLD {
            return self.allocate_large(t, bytes);
        }
        let class = class_of(bytes);
        let size = CLASS_SIZES[class] as usize;
        loop {
            let c = &mut t.alloc.classes[class];
            if c.free != 0 {
                let cell = c.free;
                // SAFETY: a free cell of this class: word 1 links the next free cell.
                unsafe {
                    c.free = *((cell + 8) as *const usize);
                    std::ptr::write_bytes(cell as *mut u8, 0, size);
                }
                return Some(cell as *mut u8);
            }
            if c.bump + size <= c.end {
                let cell = c.bump;
                c.bump += size;
                return Some(cell as *mut u8);
            }
            self.refill(t, class)?;
        }
    }

    #[cold]
    fn refill(&self, t: &mut Thread, class: usize) -> Option<()> {
        self.maybe_collect(t);
        let mut space = self.space.lock().unwrap();
        let (_, free, bump, end) = match space.take_page(class) {
            Some(p) => p,
            None => {
                drop(space);
                self.collect(t);
                self.space.lock().unwrap().take_page(class)?
            }
        };
        t.alloc.classes[class] = ClassCache { free, bump, end };
        Some(())
    }

    #[cold]
    fn allocate_large(&self, t: &mut Thread, bytes: usize) -> Option<*mut u8> {
        self.maybe_collect(t);
        let addr = self.space.lock().unwrap().alloc_large(bytes);
        addr.map(|a| a as *mut u8)
    }

    fn maybe_collect(&self, t: &mut Thread) {
        let due = {
            let s = self.space.lock().unwrap();
            s.allocated_since_gc > self.min_trigger.max(s.live_after_gc)
        };
        if due {
            self.collect(t);
        }
    }

    /// A full stop-the-world collection.
    pub fn collect(&self, t: &mut Thread) {
        if std::env::var_os("RAVA_CORE_NO_GC").is_some() {
            return;
        }
        let started = std::time::Instant::now();
        let done = safepoint::stop_the_world(t, |_| {
            let mut space = self.space.lock().unwrap();
            for rec in safepoint::all_threads() {
                // SAFETY: every registered thread is stopped (or is us); its cache is ours to reset.
                let th = unsafe { &mut *rec.thread.load(std::sync::atomic::Ordering::SeqCst) };
                th.alloc = AllocCache::default();
            }
            for m in space.pages.iter_mut().flatten() {
                m.owned = false;
                m.marks.iter_mut().for_each(|w| *w = 0);
            }
            space.large.values_mut().for_each(|l| l.marked = false);
            let marked = mark::mark(&mut space);
            let live = sweep(&mut space);
            space.live_after_gc = live;
            space.allocated_since_gc = 0;
            (marked, live)
        });
        if let (Some((marked, live)), true) = (done, std::env::var_os("RAVA_CORE_GC_LOG").is_some()) {
            eprintln!("[gc] marked {marked} objects, {} MiB live, {:.1} ms", live >> 20, started.elapsed().as_secs_f64() * 1e3);
        }
    }

    pub fn used_bytes(&self) -> usize {
        let s = self.space.lock().unwrap();
        (s.fresh - s.base) - s.empty.len() * PAGE_SIZE + s.large.values().map(|l| l.size).sum::<usize>()
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.space.lock().unwrap().find_object(addr).is_some()
    }
}

/// Frees unmarked objects; rebuilds page free lists; returns live bytes (page granularity for
/// partially live pages).
fn sweep(space: &mut Space) -> usize {
    let mut live = 0;
    for list in space.partial.iter_mut() {
        list.clear();
    }
    for index in 0..space.pages.len() {
        let page = space.page_addr(index);
        let Some(meta) = space.pages[index].as_mut() else { continue };
        let cell = meta.cell_size as usize;
        let mut free_head = 0usize;
        let mut live_cells = 0;
        for i in (0..meta.cells()).rev() {
            let addr = page + i * cell;
            // SAFETY: cells of a page in use.
            let klass = unsafe { *(addr as *const usize) };
            let marked = meta.marks[i / 64] & (1 << (i % 64)) != 0;
            if klass != 0 && marked {
                live_cells += 1;
                continue;
            }
            // SAFETY: a dead or free cell: clear the class word, link it into the free list.
            unsafe {
                *(addr as *mut usize) = 0;
                *((addr + 8) as *mut usize) = free_head;
            }
            free_head = addr;
        }
        if live_cells == 0 {
            space.pages[index] = None;
            space.empty.push(page);
            continue;
        }
        live += live_cells * cell;
        meta.free = free_head;
        if free_head != 0 {
            let class = meta.class as usize;
            space.partial[class].push(index);
        }
    }
    let dead: Vec<usize> = space.large.iter().filter(|(_, l)| !l.marked).map(|(&a, _)| a).collect();
    for a in dead {
        let l = space.large.remove(&a).unwrap();
        // SAFETY: unmaps a dead large object.
        unsafe { libc::munmap(a as *mut libc::c_void, l.size) };
    }
    live + space.large.values().map(|l| l.size).sum::<usize>()
}
