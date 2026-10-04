//! Marking: conservative roots (thread stacks, value stacks), precise VM roots and statics, and
//! a precise trace of the heap by class layout. The trace runs on every core: mark bits are set
//! atomically, and workers share packets of objects still to scan. Objects in allocation pages
//! that the trace reaches through the heap or a static move into old pages (`evacuate.rs`);
//! what another root references stays in place.
//!
//! A young collection keeps the marks of old objects (those that survived a collection): the
//! trace stops at them, and the old objects on dirty cards are scanned as extra roots.

use super::safepoint;
use super::space::{PageMeta, Space};
use crate::heap::{obj, Oop, ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::klass::{Klass, KlassKind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Objects a worker hands to the shared pool at a time.
const PACKET: usize = 128;
/// Objects prefetched ahead of the scan.
const PREFETCH: usize = 8;

/// State shared by the marking workers.
pub(super) struct Shared<'a> {
    pub(super) space: &'a Space,
    /// Moves young objects reached through the heap (a young collection), taking old runs
    /// through `promote` (the space, mutated only under that lock: pages no marker reads).
    pub(super) evacuate: bool,
    pub(super) promote: Mutex<*mut Space>,
    /// Packets of marked objects not yet scanned.
    pool: Mutex<Vec<Vec<usize>>>,
    /// Packets in the pool (read without the lock).
    pooled: AtomicUsize,
    /// Workers out of work.
    idle: AtomicUsize,
    workers: usize,
    marked: AtomicUsize,
}

// The space is only read while the world is stopped; mark bits change atomically.
unsafe impl Sync for Shared<'_> {}

impl Shared<'_> {
    /// Sets the mark of the object at exactly `start` (a known object start); true if it was
    /// not marked yet (the caller then scans it).
    pub(super) fn mark_start(&self, start: usize) -> bool {
        let space = self.space;
        let first = if let Some(index) = space.page_index(start) {
            let Some(meta) = space.pages[index].as_ref() else { return false };
            let cell = meta.cell_of(start - space.page_addr(index));
            // SAFETY: the mark words are only accessed atomically during marking.
            let word = unsafe { &*(meta.marks.as_ptr().add(cell / 64) as *const AtomicU64) };
            let bit = 1u64 << (cell % 64);
            word.load(Ordering::Relaxed) & bit == 0 && word.fetch_or(bit, Ordering::Relaxed) & bit == 0
        } else {
            let Some(l) = space.large.get(&start) else { return false };
            // SAFETY: as above, for the large object's mark.
            let mark = unsafe { &*(&l.marked as *const bool as *const AtomicBool) };
            !mark.load(Ordering::Relaxed) && !mark.swap(true, Ordering::Relaxed)
        };
        first
    }

    fn give(&self, packet: Vec<usize>) {
        self.pool.lock().unwrap().push(packet);
        self.pooled.fetch_add(1, Ordering::Release);
    }

    fn take(&self) -> Option<Vec<usize>> {
        if self.pooled.load(Ordering::Acquire) == 0 {
            return None;
        }
        let packet = self.pool.lock().unwrap().pop()?;
        self.pooled.fetch_sub(1, Ordering::Release);
        Some(packet)
    }

    /// Scans objects, starting with `stack`, until no worker has any left.
    fn work(&self, mut stack: Vec<usize>, runs: &mut super::evacuate::Runs) {
        let mut scanned = 0;
        // Objects pass through a small FIFO between the stack and the scan, their first line
        // prefetched on the way in, so the scan rarely waits on a miss.
        let mut ring = [0usize; PREFETCH];
        let (mut head, mut len) = (0, 0);
        loop {
            loop {
                while len < PREFETCH {
                    let Some(o) = stack.pop() else { break };
                    // SAFETY: a prefetch never faults.
                    unsafe { std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(o as *const i8) };
                    ring[(head + len) % PREFETCH] = o;
                    len += 1;
                }
                if len == 0 {
                    break;
                }
                let o = ring[head];
                (head, len) = ((head + 1) % PREFETCH, len - 1);
                scanned += 1;
                self.scan(o as Oop, &mut stack, runs);
                // Share surplus while others may be waiting for work.
                if stack.len() >= 2 * PACKET && self.pooled.load(Ordering::Relaxed) == 0 && self.idle.load(Ordering::Relaxed) > 0 {
                    self.give(stack.split_off(stack.len() - PACKET));
                }
            }
            if let Some(p) = self.take() {
                stack = p;
                continue;
            }
            // Out of work: done once every worker is, unless a packet shows up meanwhile.
            self.idle.fetch_add(1, Ordering::AcqRel);
            loop {
                if self.pooled.load(Ordering::Acquire) > 0 {
                    self.idle.fetch_sub(1, Ordering::AcqRel);
                    break;
                }
                if self.idle.load(Ordering::Acquire) == self.workers {
                    self.marked.fetch_add(scanned, Ordering::Relaxed);
                    return;
                }
                std::hint::spin_loop();
            }
        }
    }

    fn scan(&self, o: Oop, stack: &mut Vec<usize>, runs: &mut super::evacuate::Runs) {
        if self.evacuate {
            return ref_slots(o, |slot| self.update(slot, stack, runs));
        }
        refs(o, |r| {
            if self.mark_start(r) {
                stack.push(r);
            }
        });
    }
}

/// Calls `f` with each non-null reference field or element of the live object `o`.
#[inline]
fn refs(o: Oop, mut f: impl FnMut(usize)) {
    ref_slots(o, |slot| {
        // SAFETY: a reference field or element of a live object.
        let r = unsafe { obj::load_ref_at(slot) } as usize;
        if r != 0 {
            f(r);
        }
    });
}

/// Calls `f` with the address of each reference field or element of the live object `o`.
#[inline]
fn ref_slots(o: Oop, mut f: impl FnMut(usize)) {
    // SAFETY: a marked, live object.
    let k: &'static Klass = unsafe { obj::klass(o) };
    match k.kind {
        KlassKind::Instance => {
            for &off in k.ref_offsets.iter() {
                f(o as usize + off as usize);
            }
        }
        KlassKind::ObjArray { .. } => {
            let len = unsafe { obj::read::<i32>(o, ARRAY_LENGTH_OFFSET) } as usize;
            for i in 0..len {
                f(obj::elem_addr(o, i));
            }
        }
        KlassKind::TypeArray { .. } => {}
    }
}

/// A unit of root marking, taken by whichever worker is free.
enum Task {
    /// A thread's stacks (conservatively) and its precise references.
    Thread(&'static safepoint::Record),
    /// The roots of a range of classes.
    Classes(usize, usize),
    /// The statics of a range of classes (a young collection: their other roots are old).
    Statics(usize, usize),
    /// Objects stored into write-once VM roots since the last collection (`recent.rs`).
    Noted,
    /// The VM's other roots (with `true`, those of a young collection).
    Other(bool),
    /// Dirty cards of `[from, to)` in the page area (a young collection).
    Cards(usize, usize),
    /// Dirty cards of `[from, to)` in the large-object area.
    LargeCards(usize, usize),
}

impl Shared<'_> {
    /// Marks the object `r` points at exactly, queuing it if new.
    fn precise(&self, r: usize, out: &mut Vec<usize>) {
        if r != 0 && self.mark_start(r) {
            out.push(r);
        }
    }

    /// Marks the object a word may point into, queuing it if new.
    fn maybe(&self, w: usize, out: &mut Vec<usize>) {
        if let Some(start) = self.space.find_object(w) {
            if self.mark_start(start) {
                out.push(start);
            }
        }
    }

    /// Runs a root task: what it finds is marked and queued on `out` (a card task queues the
    /// reference slots of old objects instead); reference statics are marked too, or, when
    /// evacuating, their slots queued on `statics` to follow moves.
    fn run(&self, task: &Task, classes: &[&'static Klass], noted: &[usize], out: &mut Vec<usize>, statics: &mut Vec<usize>) {
        let mut static_slots = |from: usize, to: usize, out: &mut Vec<usize>| {
            crate::vm::vm().class_statics(&classes[from..to], &mut |slot| {
                if self.evacuate {
                    statics.push(slot);
                } else {
                    // SAFETY: a static slot holds a full-width reference.
                    self.precise(unsafe { *(slot as *const usize) }, out);
                }
            });
        };
        match *task {
            Task::Thread(rec) => {
                // SAFETY: the thread is stopped; its record and Thread outlive the collection.
                let t = unsafe { &*rec.thread.load(Ordering::SeqCst) };
                let rsp = rec.rsp.load(Ordering::SeqCst);
                let (lo, hi) = t.stack_range();
                let mut ranges = vec![(lo as usize, hi as usize)];
                if rsp != 0 {
                    ranges.push((rsp, rec.stack_top));
                }
                for (lo, hi) in ranges {
                    let mut p = lo & !7;
                    while p + 8 <= hi {
                        // SAFETY: an address range of a stopped thread's stack or value stack.
                        self.maybe(unsafe { *(p as *const usize) }, out);
                        p += 8;
                    }
                }
                for r in [t.java_thread, t.scoped_value_cache, t.pending] {
                    self.precise(r as usize, out);
                }
                for &h in t.handles.iter() {
                    self.precise(h as usize, out);
                }
            }
            Task::Classes(from, to) => {
                let mut refs = Vec::new();
                crate::vm::vm().class_roots(&classes[from..to], &mut |o| refs.push(o as usize));
                refs.into_iter().for_each(|r| self.precise(r, out));
                static_slots(from, to, out);
            }
            Task::Statics(from, to) => static_slots(from, to, out),
            Task::Noted => noted.iter().for_each(|&r| self.precise(r, out)),
            Task::Other(young) => {
                let mut refs = Vec::new();
                crate::vm::vm().other_roots(young, &mut |o| refs.push(o as usize));
                refs.into_iter().for_each(|r| self.precise(r, out));
            }
            Task::Cards(from, to) => self.page_cards(from, to, out),
            Task::LargeCards(from, to) => {
                let space = self.space;
                let mut last = 0;
                super::cards::drain(from, to, &mut |card| {
                    if let Some((&start, l)) = space.large.range(..=card).next_back() {
                        if card < start + l.size && l.marked {
                            self.scan_old(start, card, &mut last, out);
                        }
                    }
                });
            }
        }
    }

    /// Collects the reference slots of the old object at `start` for a dirty card at `card`,
    /// without marking: an object array only in the card's elements, anything else whole and
    /// once (`last` is the last one scanned whole).
    fn scan_old(&self, start: usize, card: usize, last: &mut usize, out: &mut Vec<usize>) {
        use super::cards::CARD_SIZE;
        // SAFETY: a live old object.
        let k: &'static Klass = unsafe { obj::klass(start as Oop) };
        if let KlassKind::ObjArray { .. } = k.kind {
            let len = unsafe { obj::read::<i32>(start as Oop, ARRAY_LENGTH_OFFSET) } as usize;
            let base = start + ARRAY_BASE_OFFSET as usize;
            let size = crate::heap::REF_SIZE as usize;
            let from = card.max(base).saturating_sub(base).div_ceil(size);
            let to = ((card + CARD_SIZE).saturating_sub(base) / size).min(len);
            out.extend((from..to).map(|i| obj::elem_addr(start as Oop, i)));
            return;
        }
        if start != *last {
            *last = start;
            ref_slots(start as Oop, |slot| out.push(slot));
        }
    }

    /// Collects the reference slots of the old objects on the dirty cards of `[from, to)`
    /// (whole pages), once each, and cleans the cards. Most dirty cards lie on pages allocated since
    /// the last collection (initializing stores), with no mark set: those are only cleaned.
    /// Elsewhere, only the marked cells of each card are visited, a mark word at a time.
    fn page_cards(&self, from: usize, to: usize, out: &mut Vec<usize>) {
        use super::cards::CARD_SIZE;
        use super::space::PAGE_SIZE;
        let space = self.space;
        let mut last = 0;
        for page in (from..to).step_by(PAGE_SIZE) {
            let meta = space.page_index(page).and_then(|i| space.pages[i].as_ref());
            // SAFETY: mark words of a page in use; nothing marks during the card phase.
            let word = |m: &PageMeta, w: usize| unsafe { (*(m.marks.as_ptr().add(w) as *const AtomicU64)).load(Ordering::Relaxed) };
            let Some(meta) = meta.filter(|m| (0..m.marks.len()).any(|w| word(m, w) != 0)) else {
                super::cards::clean(page, page + PAGE_SIZE);
                continue;
            };
            let size = meta.cell_size as usize;
            super::cards::drain(page, page + PAGE_SIZE, &mut |card| {
                let first = meta.cell_of(card - page);
                let end = (meta.cell_of(card + CARD_SIZE - 1 - page) + 1).min(meta.cells());
                if first >= end {
                    return;
                }
                for w in first / 64..end.div_ceil(64) {
                    let lo = (first.max(w * 64) - w * 64) as u32;
                    let hi = (end.min(w * 64 + 64) - w * 64) as u32;
                    let range = (u64::MAX >> (64 - (hi - lo))) << lo;
                    let mut bits = word(meta, w) & range;
                    while bits != 0 {
                        let start = page + (w * 64 + bits.trailing_zeros() as usize) * size;
                        bits &= bits - 1;
                        // SAFETY: inside a page in use; old objects have their mark set.
                        if unsafe { *(start as *const u32) } != 0 {
                            self.scan_old(start, card, &mut last, out);
                        }
                    }
                }
            });
        }
    }
}

/// The root tasks of a collection: (young) the card table in chunks, which come first, then
/// the noted and embedded roots, threads, classes in chunks and other roots. A young collection
/// scans only the statics of the first `old` classes.
fn tasks(space: &Space, classes: usize, old: usize, young: bool) -> Vec<Task> {
    const CLASS_CHUNK: usize = 512;
    let old = if young { old.min(classes) } else { 0 };
    let chunks = |from: usize, to: usize| (from..to).step_by(CLASS_CHUNK).map(move |c| (c, (c + CLASS_CHUNK).min(to)));
    let mut tasks = Vec::new();
    if young {
        // Chunks of whole pages, so no object spans two tasks.
        let chunk = 64 * super::space::PAGE_SIZE;
        tasks.extend((space.base..space.fresh).step_by(chunk).map(|c| Task::Cards(c, (c + chunk).min(space.fresh))));
        let area = &space.large_area;
        if area.fresh > area.base {
            tasks.push(Task::LargeCards(area.base, area.fresh));
        }
    }
    tasks.push(Task::Noted);
    tasks.extend(safepoint::all_threads().into_iter().map(Task::Thread));
    tasks.extend(chunks(0, old).map(|(a, b)| Task::Statics(a, b)));
    tasks.extend(chunks(old, classes).map(|(a, b)| Task::Classes(a, b)));
    tasks.push(Task::Other(young));
    tasks
}

/// Marking threads: the collecting thread and helpers, one per core in all.
fn workers() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let cores = crate::processors();
        std::env::var("RAVA_GC_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(cores).clamp(1, 16)
    })
}

/// Marks everything reachable (with `young`, what is reachable without passing through old
/// objects, plus what old objects on dirty cards reference); returns the number of newly
/// marked objects.
pub(super) fn mark(space: &mut Space, young: bool) -> usize {
    let workers = workers();
    let promote: *mut Space = space;
    let shared = Shared {
        // SAFETY: markers read the space while `promote` takes old runs (`evacuate.rs`).
        space: unsafe { &*promote },
        evacuate: super::evacuate::enabled(),
        promote: Mutex::new(promote),
        pool: Mutex::new(Vec::new()),
        pooled: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        workers,
        marked: AtomicUsize::new(0),
    };
    let classes = crate::vm::vm().all_classes();
    let (noted, old) = super::recent::take(classes.len());
    let tasks = tasks(shared.space, classes.len(), old, young);
    // The card tasks run first, and nothing is marked until all are done: a mark set meanwhile
    // would make a young object look old, to be scanned again as a card root.
    let cards = tasks.iter().take_while(|t| matches!(t, Task::Cards(..) | Task::LargeCards(..))).count();
    let (next_card, next) = (AtomicUsize::new(0), AtomicUsize::new(cards));
    let barrier = std::sync::Barrier::new(workers);
    // Each worker takes root tasks until none is left, then traces from what it found.
    // Evacuating, every root is marked (kept in place) before anything moves.
    let worker = || {
        let mut found = Vec::with_capacity(4 * PACKET);
        let mut runs = super::evacuate::Runs::default();
        let (mut slots, mut statics) = (Vec::new(), Vec::new());
        if cards > 0 {
            while let Some(task) = tasks[..cards].get(next_card.fetch_add(1, Ordering::Relaxed)) {
                shared.run(task, &classes, &noted, &mut slots, &mut statics);
            }
            barrier.wait();
            if !shared.evacuate {
                // SAFETY: reference slots of live old objects.
                slots.drain(..).for_each(|s| shared.precise(unsafe { obj::load_ref_at(s) } as usize, &mut found));
            }
        }
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(task) = tasks.get(i) else { break };
            shared.run(task, &classes, &noted, &mut found, &mut statics);
        }
        if shared.evacuate {
            barrier.wait();
            slots.into_iter().for_each(|s| shared.update(s, &mut found, &mut runs));
            statics.into_iter().for_each(|s| shared.update_static(s, &mut found, &mut runs));
        }
        shared.work(found, &mut runs);
        shared.finish_runs(&runs);
    };
    std::thread::scope(|s| {
        for _ in 1..workers {
            s.spawn(worker);
        }
        worker();
    });
    shared.marked.into_inner()
}
