//! Non-moving, generational mark-sweep collector (design: `docs/core/README.md`).
//!
//! Allocation bumps through per-thread runs of free cells (one owned page per size class); a
//! collection stops the world at safepoints, marks from conservative stack roots and precise VM
//! roots, and sweeps by copying mark bitmaps into allocation bitmaps (`space.rs`). Objects never
//! move, so raw references held anywhere stay valid.
//!
//! Generations use sticky mark bits: objects that survived a collection keep their mark and are
//! old. A young collection traces only from roots and from old objects on cards the write
//! barrier dirtied (`cards.rs`), frees the young objects nothing reached, and promotes the rest.
//! A full collection clears every mark first; it runs once the old generation has doubled.

pub mod cards;
mod large;
mod mark;
mod prefault;
pub(crate) mod recent;
pub mod safepoint;
mod space;

use crate::thread::Thread;
use space::{class_of, Space, CLASSES, CLASS_SIZES, LARGE_THRESHOLD, PAGE_SIZE};
use std::sync::Mutex;

/// A thread's allocation cache for one size class: a zeroed bump range (a run of free cells) in
/// an owned page, `page` being that page's index + 1 (0 = none yet), and where the run started
/// (what was bumped since is counted as allocated when the run is retired).
#[derive(Clone, Copy, Default)]
pub struct ClassCache {
    bump: usize,
    end: usize,
    page: usize,
    start: usize,
}

pub struct AllocCache {
    classes: [ClassCache; CLASSES],
    /// Bytes of retired runs not yet added to `Heap::small_allocated` (flushed per MiB and
    /// when taking a page, so refills within a page touch no shared counter).
    pending: usize,
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
        AllocCache { classes: [ClassCache::default(); CLASSES], pending: 0 }
    }
}

pub struct Heap {
    space: Mutex<Space>,
    min_trigger: usize,
    max_heap: usize,
    /// Bytes bumped through retired small-object runs since the last collection.
    small_allocated: std::sync::atomic::AtomicUsize,
    /// The space's page metadata (a fixed-size table), for runs on pages a thread owns.
    pages: *mut Option<space::PageMeta>,
    base: usize,
}

// The space is only touched under its lock (or with the world stopped).
unsafe impl Sync for Heap {}
unsafe impl Send for Heap {}

pub struct Stats {
    pub collections: u64,
    pub used: usize,
}

impl Heap {
    pub fn new(reserve: usize, max_heap: usize) -> Heap {
        let min_trigger = std::env::var("RAVA_CORE_GC_MB").ok().and_then(|v| v.parse::<usize>().ok()).map_or(128 << 20, |mb| mb << 20);
        let mut space = Space::new(reserve);
        let (pages, base) = (space.pages.as_mut_ptr(), space.base);
        Heap { space: Mutex::new(space), min_trigger, max_heap, small_allocated: Default::default(), pages, base }
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
            if c.bump + size <= c.end {
                let cell = c.bump;
                c.bump += size;
                return Some(cell as *mut u8);
            }
            self.refill(t, class)?;
        }
    }

    /// The next run of free cells for `t`'s `class` cache: the rest of its page, else a new page.
    #[cold]
    fn refill(&self, t: &mut Thread, class: usize) -> Option<()> {
        let retired = t.alloc.classes[class];
        t.alloc.pending += retired.bump - retired.start;
        if t.alloc.pending >= 1 << 20 {
            self.flush_pending(t);
        }
        if retired.page != 0 {
            let index = retired.page - 1;
            // SAFETY: the page is owned by this thread's cache, so outside collections (which
            // cannot run while this thread allocates) only this thread touches its metadata.
            let meta = unsafe { (*self.pages.add(index)).as_mut().expect("owned page") };
            if let Some((bump, end)) = space::next_run_in(meta, self.base + (index << space::PAGE_SHIFT)) {
                t.alloc.classes[class] = ClassCache { bump, end, page: retired.page, start: bump };
                return Some(());
            }
        }
        t.alloc.classes[class] = ClassCache::default();
        self.flush_pending(t);
        self.maybe_collect(t);
        let mut space = self.space.lock().unwrap();
        let page = match space.take_page(class) {
            Some(p) => p,
            None => {
                drop(space);
                self.collect(t);
                space = self.space.lock().unwrap();
                space.take_page(class)?
            }
        };
        let (bump, end) = space.next_run(page).expect("a taken page has free cells");
        t.alloc.classes[class] = ClassCache { bump, end, page: page + 1, start: bump };
        Some(())
    }

    fn flush_pending(&self, t: &mut Thread) {
        self.small_allocated.fetch_add(std::mem::take(&mut t.alloc.pending), std::sync::atomic::Ordering::Relaxed);
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
            let allocated = s.allocated_since_gc + self.small_allocated.load(std::sync::atomic::Ordering::Relaxed);
            (allocated > self.young_budget(s.live_after_gc)).then(|| self.full_due(&s))
        };
        if let Some(full) = due {
            self.collect_as(t, full);
        }
    }

    /// How much may be allocated between collections that leave `live` bytes (the young
    /// generation's size): half the live data, within the maximum heap size.
    fn young_budget(&self, live: usize) -> usize {
        (live / 2).min(self.max_heap.saturating_sub(live)).max(self.min_trigger)
    }

    /// A full collection is due once the old generation has doubled since the last one (or
    /// fills the heap), or when generations are off (`RAVA_CORE_GC_GEN=0`).
    fn full_due(&self, s: &Space) -> bool {
        static GENERATIONAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let generational = *GENERATIONAL.get_or_init(|| std::env::var("RAVA_CORE_GC_GEN").map_or(true, |v| v != "0"));
        !generational || s.live_after_gc > (2 * s.live_after_full).max(self.min_trigger) || s.live_after_gc + self.min_trigger > self.max_heap
    }

    pub fn max_bytes(&self) -> usize {
        self.max_heap
    }

    /// A full stop-the-world collection.
    pub fn collect(&self, t: &mut Thread) {
        self.collect_as(t, true)
    }

    /// A stop-the-world collection, `full` or young.
    fn collect_as(&self, t: &mut Thread, full: bool) {
        if std::env::var_os("RAVA_CORE_NO_GC").is_some() {
            return;
        }
        let started = std::time::Instant::now();
        let mut phases = [0f64; 3];
        let done = safepoint::stop_the_world(t, |_| {
            let stopped = std::time::Instant::now();
            phases[0] = (stopped - started).as_secs_f64() * 1e3;
            let mut space = self.space.lock().unwrap();
            for rec in safepoint::all_threads() {
                // SAFETY: every registered thread is stopped (or is us); its cache is ours to reset.
                let th = unsafe { &mut *rec.thread.load(std::sync::atomic::Ordering::SeqCst) };
                th.alloc = AllocCache::default();
            }
            for m in space.pages.iter_mut().flatten() {
                m.owned = false;
            }
            if full {
                clear_marks(&mut space);
            }
            let before_mark = std::time::Instant::now();
            let marked = mark::mark(&mut space, !full);
            if !full && std::env::var_os("RAVA_CORE_GC_VERIFY").is_some() {
                verify_young(&mut space);
            }
            if full {
                let (b, f, lb, lf) = (space.base, space.fresh, space.large_area.base, space.large_area.fresh);
                cards::clean(b, f);
                cards::clean(lb, lf);
            }
            let before_sweep = std::time::Instant::now();
            let live = sweep(&mut space);
            if full {
                space.live_after_full = live;
            }
            phases[1] = (before_sweep - before_mark).as_secs_f64() * 1e3;
            phases[2] = before_sweep.elapsed().as_secs_f64() * 1e3;
            space.live_after_gc = live;
            space.allocated_since_gc = 0;
            self.small_allocated.store(0, std::sync::atomic::Ordering::Relaxed);
            (marked, live)
        });
        if let (Some((marked, live)), true) = (done, std::env::var_os("RAVA_CORE_GC_LOG").is_some()) {
            eprintln!(
                "[gc] {} marked {marked} objects, {} MiB live, {:.1} ms (stop {:.1}, mark {:.1}, sweep {:.1})",
                if full { "full" } else { "young" },
                live >> 20,
                started.elapsed().as_secs_f64() * 1e3,
                phases[0],
                phases[1],
                phases[2]
            );
        }
    }

    pub fn used_bytes(&self) -> usize {
        let s = self.space.lock().unwrap();
        (s.fresh - s.base) - s.empty.len() * PAGE_SIZE + s.large.values().map(|l| l.size).sum::<usize>()
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.space.lock().unwrap().find_object(addr).is_some()
    }

    /// Whether `addr` is the start of an object (not a pointer into one).
    pub fn is_object(&self, addr: usize) -> bool {
        self.space.lock().unwrap().find_object(addr) == Some(addr)
    }
}

fn clear_marks(space: &mut Space) {
    for m in space.pages.iter_mut().flatten() {
        m.marks.iter_mut().for_each(|w| *w = 0);
    }
    space.large.values_mut().for_each(|l| l.marked = false);
}

/// `RAVA_CORE_GC_VERIFY`: after a young mark, marks again from scratch and reports every
/// object the full trace reaches that the young one missed (a missing write barrier). Leaves
/// the full marks in place.
fn verify_young(space: &mut Space) {
    let young: Vec<Option<Box<[u64]>>> = space.pages.iter().map(|m| m.as_ref().map(|m| m.marks.clone())).collect();
    let young_large: Vec<(usize, bool)> = space.large.iter().map(|(&a, l)| (a, l.marked)).collect();
    clear_marks(space);
    mark::mark(space, false);
    let mut missed = 0;
    for (index, (meta, before)) in space.pages.iter().zip(young.iter()).enumerate() {
        let (Some(meta), Some(before)) = (meta, before) else { continue };
        for (w, (&now, &was)) in meta.marks.iter().zip(before.iter()).enumerate() {
            let mut lost = now & !was;
            while lost != 0 {
                let cell = w * 64 + lost.trailing_zeros() as usize;
                lost &= lost - 1;
                let start = space.page_addr(index) + cell * meta.cell_size as usize;
                if missed < 20 {
                    // SAFETY: a live object's class.
                    eprintln!("[gc-verify] young collection missed {:#x} ({})", start, unsafe { crate::heap::obj::klass(start as crate::heap::Oop) }.name);
                }
                missed += 1;
            }
        }
    }
    for (a, was) in young_large {
        if space.large.get(&a).is_some_and(|l| l.marked && !was) {
            eprintln!("[gc-verify] young collection missed large {a:#x}");
            missed += 1;
        }
    }
    if missed > 0 {
        eprintln!("[gc-verify] {missed} reachable objects missed");
    }
}

/// Frees unmarked objects by bitmap alone: the marks become the pages' allocation bitmaps (dead
/// cells are left untouched until handed out again). Pages without survivors are released
/// whole. Returns live bytes.
fn sweep(space: &mut Space) -> usize {
    let mut live = 0;
    for list in space.partial.iter_mut() {
        list.clear();
    }
    for index in 0..space.pages.len() {
        let page = space.page_addr(index);
        let Some(meta) = space.pages[index].as_mut() else { continue };
        let live_cells: usize = meta.marks.iter().map(|w| w.count_ones() as usize).sum();
        if live_cells == 0 {
            // Without metadata no reference resolves into the page; it is zeroed when reused.
            space.pages[index] = None;
            space.empty.push(page);
            continue;
        }
        // The marks stay: survivors are old from now on.
        meta.alloc.copy_from_slice(&meta.marks);
        meta.cursor = 0;
        meta.zeroed = false;
        live += live_cells * meta.cell_size as usize;
        // Allocation reuses pages at most half full: sparser free cells would hand out runs
        // too short to amortize a refill. Fuller pages wait until more of them dies.
        if live_cells * 2 <= meta.cells() {
            let class = meta.class as usize;
            space.partial[class].push(index);
        }
    }
    let dead: Vec<usize> = space.large.iter().filter(|(_, l)| !l.marked).map(|(&a, _)| a).collect();
    for a in dead {
        let l = space.large.remove(&a).unwrap();
        space.large_area.free(a, l.size);
    }
    live + space.large.values().map(|l| l.size).sum::<usize>()
}
