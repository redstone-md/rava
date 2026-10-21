//! Generational mark-sweep collector, mostly copying (design: `docs/README.md`).
//!
//! Allocation bumps through per-thread runs of free cells (one owned page per size class); a
//! collection stops the world at safepoints, marks from conservative stack roots and precise VM
//! roots, and sweeps by copying mark bitmaps into allocation bitmaps (`space.rs`). What a root
//! holds never moves, so raw references in stacks, natives and the VM stay valid; objects of
//! allocation pages reached only through the heap or a static are moved into packed old pages
//! (`evacuate.rs`).
//!
//! Generations use sticky mark bits: objects that survived a collection keep their mark and are
//! old. A young collection traces only from roots and from old objects on cards the write
//! barrier dirtied (`cards.rs`), frees the young objects nothing reached, and promotes the rest.
//! A full collection clears every mark first; it runs once the old generation has doubled.

pub mod cards;
mod evacuate;
mod histo;
pub mod idle;
mod large;
mod aging;
mod mark;
mod mixed;
mod pool;
mod prefault;
pub(crate) mod recent;
pub mod safepoint;
mod space;
mod statics;
mod sweep;
mod why;

use crate::thread::Thread;
use space::{class_of, Space, CLASSES, CLASS_SIZES, LARGE_THRESHOLD};
use std::sync::Mutex;

/// A thread's allocation cache for one size class: a bump range (a run of free cells) in
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
/// pointer and bump limit. Bump memory is not cleared: the allocation clears what it takes.
pub(crate) fn bump_layout(bytes: usize) -> Option<(usize, usize, usize)> {
    if bytes > LARGE_THRESHOLD {
        return None;
    }
    let class = class_of(bytes);
    let base = std::mem::offset_of!(AllocCache, classes) + class * std::mem::size_of::<ClassCache>();
    Some((CLASS_SIZES[class] as usize, base + std::mem::offset_of!(ClassCache, bump), base + std::mem::offset_of!(ClassCache, end)))
}

/// Sizes up to this have a class per 8 bytes from 16 (`class_of`): compiled code finds the class
/// of a variable-length array from its rounded size. (Classes per 16 bytes rounded 8% of the
/// live heap away.)
pub(crate) const SMALL_STEP_MAX: usize = 256;
const _: () = assert!(std::mem::size_of::<ClassCache>() == 32 && CLASS_SIZES[SMALL_STEP_MAX / 8 - 2] as usize == SMALL_STEP_MAX);

/// Offsets within `AllocCache` of the bump pointer and limit of the class of cell size `size`
/// (a multiple of 8 from 16, at most `SMALL_STEP_MAX`), less `4 * size - 64`: the class's cache
/// is at `classes + (size / 8 - 2) * 32`.
pub(crate) fn step_cache_offsets() -> (usize, usize) {
    let base = std::mem::offset_of!(AllocCache, classes);
    (base + std::mem::offset_of!(ClassCache, bump), base + std::mem::offset_of!(ClassCache, end))
}

impl Default for AllocCache {
    fn default() -> Self {
        AllocCache { classes: [ClassCache::default(); CLASSES], pending: 0 }
    }
}

thread_local! {
    /// Set while this thread allocates past `-Xmx` (`beyond_limit`).
    static BEYOND_LIMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with this thread's allocations exempt from `-Xmx`: for the `OutOfMemoryError` that
/// reports a full heap (HotSpot preallocates it) and for objects deoptimization must rebuild.
pub fn beyond_limit<R>(f: impl FnOnce() -> R) -> R {
    let was = BEYOND_LIMIT.with(|b| b.replace(true));
    let r = f();
    BEYOND_LIMIT.with(|b| b.set(was));
    r
}

/// Time without a collection after which a program allocating little counts as idle.
const IDLE: std::time::Duration = std::time::Duration::from_secs(10);

pub struct Heap {
    space: Mutex<Space>,
    min_trigger: usize,
    max_heap: usize,
    /// Bytes bumped through retired small-object runs since the last collection.
    small_allocated: std::sync::atomic::AtomicUsize,
    /// The space's page metadata (a fixed-size table), for runs on pages a thread owns.
    pages: *mut Option<space::PageMeta>,
    base: usize,
    /// When the heap was made (the VM's start), for the log's timestamps.
    created: std::time::Instant,
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
        let min_trigger = std::env::var("RAVA_GC_MB").ok().and_then(|v| v.parse::<usize>().ok()).map_or(128 << 20, |mb| mb << 20);
        let mut space = Space::new(reserve);
        let (pages, base, max_heap) = (space.pages.as_mut_ptr(), space.base, max_heap.min(space.reserved));
        Heap { space: Mutex::new(space), min_trigger, max_heap, small_allocated: Default::default(), pages, base, created: std::time::Instant::now() }
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
                // SAFETY: the cell just taken from this thread's run.
                unsafe { std::ptr::write_bytes(cell as *mut u8, 0, bytes) };
                return Some(cell as *mut u8);
            }
            self.refill(t, class)?;
        }
    }

    /// The next run of free cells for `t`'s `class` cache: the rest of its page, else a new page.
    #[cold]
    fn refill(&self, t: &mut Thread, class: usize) -> Option<()> {
        let retired = t.alloc.classes[class];
        t.alloc.pending += retired.bump - retired.start;        crate::profile::allocated(t, retired.bump - retired.start);
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
        if self.over_limit(&space, CLASS_SIZES[class] as usize) {
            drop(space);
            self.collect(t);
            space = self.space.lock().unwrap();
            if self.over_limit(&space, CLASS_SIZES[class] as usize) {
                return None;
            }
        }
        let (page, bump, end) = match space.take_run(class) {
            Some(r) => r,
            None => {
                drop(space);
                self.collect(t);
                space = self.space.lock().unwrap();
                space.take_run(class)?
            }
        };
        t.alloc.classes[class] = ClassCache { bump, end, page: page + 1, start: bump };
        Some(())
    }

    /// Gives back what `t`'s allocation cache holds unused, as the thread stops taking part in
    /// collections (before a collection could reset the cache itself).
    pub fn retire_cache(&self, t: &mut Thread) {
        release_unused(&mut self.space.lock().unwrap(), &t.alloc);
        self.flush_pending(t);
        t.alloc = AllocCache::default();
    }

    fn flush_pending(&self, t: &mut Thread) {
        self.small_allocated.fetch_add(std::mem::take(&mut t.alloc.pending), std::sync::atomic::Ordering::Relaxed);
    }

    #[cold]
    fn allocate_large(&self, t: &mut Thread, bytes: usize) -> Option<*mut u8> {
        crate::profile::allocated(t, bytes);
        self.maybe_collect(t);
        if self.over_limit(&self.space.lock().unwrap(), bytes) {
            self.collect(t);
            if self.over_limit(&self.space.lock().unwrap(), bytes) {
                return None;
            }
        }
        let addr = self.space.lock().unwrap().alloc_large(bytes);
        addr.map(|a| a as *mut u8)
    }

    /// Whether `bytes` more would take the heap's data past `-Xmx`: what the last collection left
    /// live and what was allocated since (HotSpot's limit, which a full collection decides). Not
    /// for what this thread must allocate regardless (`beyond_limit`).
    fn over_limit(&self, s: &Space, bytes: usize) -> bool {
        let data = s.live_after_gc + s.allocated_since_gc + self.small_allocated.load(std::sync::atomic::Ordering::Relaxed);
        data + bytes > self.max_heap && !BEYOND_LIMIT.with(|b| b.get())
    }

    fn maybe_collect(&self, t: &mut Thread) {
        let due = {
            let s = self.space.lock().unwrap();
            let allocated = s.allocated_since_gc + self.small_allocated.load(std::sync::atomic::Ordering::Relaxed);
            (allocated > self.young_budget(&s, s.live_after_gc)).then(|| self.full_due(&s))
        };
        if let Some(full) = due {
            self.collect_as(t, full);
        }
    }

    /// One full collection that gives the heap back down to its live data, when the program
    /// has been idle since the last collection (at least `IDLE`, at a rate that would take
    /// longer than that to fill its young generation), as ZGC and Shenandoah uncommit an idle
    /// heap: a server waiting for players otherwise keeps what its start left. Once per idle
    /// spell: a collection an allocation budget asks for arms the next. Called by the idle
    /// collector's thread.
    pub fn collect_if_idle(&self, t: &mut Thread) {
        {
            let mut s = self.space.lock().unwrap();
            let Some(since) = s.last_gc.map(|at| at.elapsed()) else { return };
            if s.idle_collected || since < IDLE {
                return;
            }
            let allocated = s.allocated_since_gc + self.small_allocated.load(std::sync::atomic::Ordering::Relaxed);
            // At this rate the young generation would take longer than `IDLE` to fill.
            let young = self.young_budget(&s, s.live_after_gc);
            if allocated as f64 >= young as f64 * since.as_secs_f64() / IDLE.as_secs_f64() {
                return;
            }
            (s.idle_collected, s.idle_next) = (true, true);
        }
        self.collect_as(t, true);
    }

    /// How much may be allocated between collections that leave `live` bytes (the young
    /// generation's size): a third of the live data, at least `RAVA_GC_MB` (128 MiB), within
    /// the maximum heap size; but no more than a second's allocations at the recent rate (at
    /// least `MIN_YOUNG`), as G1 sizes its eden by its pause and interval: a program that has
    /// gone quiet (a server between bursts) then keeps a small young generation, and what the
    /// next full collection releases leaves its heap near its live data.
    fn young_budget(&self, s: &Space, live: usize) -> usize {
        const MIN_YOUNG: usize = 32 << 20;
        let full = (live / 3).max(self.min_trigger).min(self.max_heap.saturating_sub(live));
        if s.alloc_rate <= 0.0 {
            return full;
        }
        full.min((s.alloc_rate as usize).max(MIN_YOUNG))
    }

    /// A full collection is due once the old generation has grown by half since the last one
    /// (or fills the heap), or when generations are off (`RAVA_GC_GEN=0`). Waiting for it to
    /// double let dead old objects pile up to twice the live data: All the Mods 10 held an
    /// 8.4 GiB heap for 3.8 GiB of live objects, its full collection costing 0.37 s.
    ///
    /// One is due as well once promotion has settled (`QUIET` young collections in a row each
    /// promoted under 1/512 of the live data) after the old generation grew by an eighth: what
    /// a program's start promoted and dropped is collected then, not when growth resumes. A
    /// full collection timed by growth alone can land at the peak of the start's transient
    /// data: All the Mods 10 kept 3.4 GiB live for 2.4 GiB in use, and a lower growth
    /// threshold (1.25) moved that collection, not the waste.
    fn full_due(&self, s: &Space) -> bool {
        static GENERATIONAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let generational = *GENERATIONAL.get_or_init(|| std::env::var("RAVA_GC_GEN").map_or(true, |v| v != "0"));
        let grown = |by: usize| s.live_after_gc > (s.live_after_full + s.live_after_full / by).max(self.min_trigger);
        !generational || grown(2) || (s.quiet >= QUIET && grown(8)) || s.live_after_gc + self.min_trigger > self.max_heap
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
        if std::env::var_os("RAVA_NO_GC").is_some() {
            return;
        }
        let started = std::time::Instant::now();
        let faults = minor_faults();
        let mut phases = [0f64; 3];
        let done = safepoint::stop_the_world(t, |_| {
            let stopped = std::time::Instant::now();
            phases[0] = (stopped - started).as_secs_f64() * 1e3;
            safepoint::set_step(safepoint::STEP_LOCK);
            let mut space = self.space.lock().unwrap();
            for rec in safepoint::all_threads() {
                // SAFETY: every registered thread is stopped (or is us); its cache is ours to reset.
                let th = unsafe { &mut *rec.thread.load(std::sync::atomic::Ordering::SeqCst) };
                release_unused(&mut space, &th.alloc);
                th.alloc = AllocCache::default();
            }
            for m in space.pages.iter_mut().flatten() {
                m.owned = false;
            }
            if full {
                space.select_sparse();
                clear_marks(&mut space);
            }
            let before_mark = std::time::Instant::now();
            safepoint::set_step(safepoint::STEP_MARK);
            let marked = mark::mark(&mut space, !full);
            if full && std::env::var_os("RAVA_GC_HISTO").is_some() {
                histo::report(&space);
            }
            if let (true, Ok(class)) = (full, std::env::var("RAVA_GC_WHY")) {
                why::report(&mut space, &class);
            }
            if !full && std::env::var_os("RAVA_GC_VERIFY").is_some() {
                verify_young(&mut space);
            }
            if full {
                let (b, f, lb, lf) = (space.base, space.fresh, space.large_area.base, space.large_area.fresh);
                cards::clean(b, f);
                cards::clean(lb, lf);
            }
            let before_sweep = std::time::Instant::now();
            safepoint::set_step(safepoint::STEP_SWEEP);
            let live = sweep::sweep(&mut space, mark::workers());
            if full {
                space.live_after_full = live;
            }
            let promoted = live.saturating_sub(space.live_after_gc);
            space.quiet = if !full && promoted < live / 512 { space.quiet + 1 } else { 0 };
            phases[1] = (before_sweep - before_mark).as_secs_f64() * 1e3;
            phases[2] = before_sweep.elapsed().as_secs_f64() * 1e3;
            space.live_after_gc = live;
            // A program gone quiet (its young generation down to under half the full size)
            // allocates little before the next collection: its surplus pages go back after a
            // young collection too, and few fault in again.
            let quiet_program = self.young_budget(&space, live) * 2 < (live / 3).max(self.min_trigger);
            if full || quiet_program {
                // Committed memory beyond what the next period allocates goes back to the
                // kernel. Only after a full collection (as G1 shrinks only then): pages
                // released after a young one were faulted back in, zeroed, by the allocation
                // that followed (~640 MiB over a Paper start), and each release costs a TLB
                // shootdown on every core inside the pause.
                safepoint::set_step(safepoint::STEP_RELEASE);
                let keep = if std::mem::take(&mut space.idle_next) { 0 } else { self.young_budget(&space, live) };
                space.release_surplus(keep + keep / 8);
                // So does the malloc heap's free memory: compiler threads free most of what
                // they allocate, in arenas glibc keeps (~1 ms; -15 MiB on a modded server).
                // SAFETY: a plain libc call.
                crate::os::trim_malloc();
            }
            let allocated = space.allocated_since_gc + self.small_allocated.load(std::sync::atomic::Ordering::Relaxed);
            // The allocation rate since the last collection, smoothed over a few.
            let now = std::time::Instant::now();
            if let Some(last) = space.last_gc {
                let rate = allocated as f64 / (now - last).as_secs_f64().max(1e-3);
                space.alloc_rate = if space.alloc_rate <= 0.0 { rate } else { 0.5 * space.alloc_rate + 0.5 * rate };
            }
            space.last_gc = Some(now);
            // A collection an allocation budget asked for arms the next idle one.
            if !space.idle_next && !full {
                space.idle_collected = false;
            }
            space.allocated_since_gc = 0;
            self.small_allocated.store(0, std::sync::atomic::Ordering::Relaxed);
            let touched = (space.fresh - space.base, space.large_area.fresh - space.large_area.base);
            (marked, live, allocated, touched)
        });
        if let (Some((marked, live, allocated, touched)), true) = (done, std::env::var_os("RAVA_GC_LOG").is_some()) {
            eprintln!(
                "[gc {:.3}s] {} after {} MiB allocated: marked {marked} objects, {} MiB live of {}+{} MiB touched, {:.1} ms (stop {:.1}, mark {:.1}, sweep {:.1}), {} page faults",
                self.created.elapsed().as_secs_f64(),
                if full { "full" } else { "young" },
                allocated >> 20,
                live >> 20,
                touched.0 >> 20,
                touched.1 >> 20,
                started.elapsed().as_secs_f64() * 1e3,
                phases[0],
                phases[1],
                phases[2],
                minor_faults() - faults
            );
        }
    }

    /// Live bytes the last collection left (`memlog`).
    pub fn live_after_gc(&self) -> usize {
        self.space.lock().unwrap().live_after_gc
    }

    pub fn used_bytes(&self) -> usize {
        self.space.lock().unwrap().in_use()
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.space.lock().unwrap().find_object(addr).is_some()
    }

    /// Whether `addr` is the start of an object (not a pointer into one).
    pub fn is_object(&self, addr: usize) -> bool {
        self.space.lock().unwrap().find_object(addr) == Some(addr)
    }
}

/// Young collections in a row promoting little after which a grown old generation is collected
/// whole (`Heap::full_due`).
const QUIET: u32 = 8;

fn clear_marks(space: &mut Space) {
    for m in space.pages.iter_mut().flatten() {
        m.marks.iter_mut().for_each(|w| *w = 0);
    }
    space.large.values_mut().for_each(|l| l.marked = false);
}

/// `RAVA_GC_VERIFY`: after a young mark, marks again from scratch and reports every
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

/// The process's minor page faults so far (for `RAVA_GC_LOG`).
fn minor_faults() -> i64 {
    crate::os::minor_faults()
}

/// Gives back the cells of `cache`'s runs no object took: they were marked allocated with the
/// run but hold whatever dead objects left there, which the conservative scan must not take
/// for objects (`space::next_run_in`).
fn release_unused(space: &mut Space, cache: &AllocCache) {
    for c in cache.classes.iter().filter(|c| c.page != 0 && c.bump < c.end) {
        let index = c.page - 1;
        let page = space.page_addr(index);
        let meta = space.pages[index].as_mut().expect("owned page");
        let cell = meta.cell_size as usize;
        meta.set_alloc((c.bump - page) / cell, (c.end - page) / cell, false);
    }
}
