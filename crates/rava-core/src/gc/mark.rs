//! Marking: conservative roots (thread stacks, value stacks, statics), precise VM roots, and a
//! precise trace of the heap by class layout. The trace runs on every core: mark bits are set
//! atomically, and workers share packets of objects still to scan.
//!
//! A young collection keeps the marks of old objects (those that survived a collection): the
//! trace stops at them, and the old objects on dirty cards are scanned as extra roots.

use super::safepoint;
use super::space::Space;
use crate::heap::{obj, Oop, ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::klass::{Klass, KlassKind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Objects a worker hands to the shared pool at a time.
const PACKET: usize = 128;

/// State shared by the marking workers.
struct Shared<'a> {
    space: &'a Space,
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
    fn mark_start(&self, start: usize) -> bool {
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
    fn work(&self, mut stack: Vec<usize>) {
        let mut scanned = 0;
        loop {
            while let Some(o) = stack.pop() {
                scanned += 1;
                self.scan(o as Oop, &mut stack);
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

    fn scan(&self, o: Oop, stack: &mut Vec<usize>) {
        let mut reference = |r: usize| {
            if r != 0 && self.mark_start(r) {
                stack.push(r);
            }
        };
        // SAFETY: a marked, live object.
        let k: &'static Klass = unsafe { obj::klass(o) };
        match k.kind {
            KlassKind::Instance => {
                for &off in k.ref_offsets.iter() {
                    reference(unsafe { obj::read::<usize>(o, off) });
                }
            }
            KlassKind::ObjArray { .. } => {
                let len = unsafe { obj::read::<i32>(o, ARRAY_LENGTH_OFFSET) } as usize;
                for i in 0..len {
                    reference(unsafe { obj::read::<usize>(o, ARRAY_BASE_OFFSET + 8 * i as u32) });
                }
            }
            KlassKind::TypeArray { .. } => {}
        }
    }
}

/// A unit of root marking, taken by whichever worker is free.
enum Task {
    /// A thread's stacks (conservatively) and its precise references.
    Thread(&'static safepoint::Record),
    /// The roots of a range of classes.
    Classes(usize, usize),
    /// The VM's other roots.
    Other,
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

    fn run(&self, task: &Task, classes: &[&'static Klass], out: &mut Vec<usize>) {
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
                let (mut refs, mut words) = (Vec::new(), Vec::new());
                crate::vm::vm().class_roots(&classes[from..to], &mut |o| refs.push(o as usize), &mut |w| words.push(w));
                refs.into_iter().for_each(|r| self.precise(r, out));
                words.into_iter().for_each(|w| self.maybe(w, out));
            }
            Task::Other => {
                let mut refs = Vec::new();
                crate::vm::vm().other_roots(&mut |o| refs.push(o as usize));
                refs.into_iter().for_each(|r| self.precise(r, out));
            }
            Task::Cards(from, to) => self.page_cards(from, to, out),
            Task::LargeCards(from, to) => {
                let space = self.space;
                let mut last = 0;
                super::cards::drain(from, to, &mut |card| {
                    if let Some((&start, l)) = space.large.range(..=card).next_back() {
                        if card < start + l.size && l.marked && start != last {
                            last = start;
                            self.scan(start as Oop, out);
                        }
                    }
                });
            }
        }
    }

    /// Scans the old objects on the dirty cards of `[from, to)` (page area), once each, and
    /// cleans the cards.
    fn page_cards(&self, from: usize, to: usize, out: &mut Vec<usize>) {
        use super::cards::CARD_SIZE;
        let space = self.space;
        let mut last = 0;
        super::cards::drain(from, to, &mut |card| {
            let Some(index) = space.page_index(card) else { return };
            let Some(meta) = space.pages[index].as_ref() else { return };
            let page = space.page_addr(index);
            let first = meta.cell_of(card - page);
            let end = (meta.cell_of(card + CARD_SIZE - 1 - page) + 1).min(meta.cells());
            for cell in first..end {
                let start = page + cell * meta.cell_size as usize;
                // SAFETY: inside a page in use; old objects have their mark set.
                let old = unsafe { (*(meta.marks.as_ptr().add(cell / 64) as *const AtomicU64)).load(Ordering::Relaxed) } & (1 << (cell % 64)) != 0;
                if old && start != last && unsafe { *(start as *const usize) } != 0 {
                    last = start;
                    self.scan(start as Oop, out);
                }
            }
        });
    }
}

/// The root tasks of a collection: threads, classes in chunks, other roots, and (young) the
/// card table in chunks.
fn tasks(space: &Space, classes: usize, young: bool) -> Vec<Task> {
    const CLASS_CHUNK: usize = 512;
    let mut tasks: Vec<Task> = safepoint::all_threads().into_iter().map(Task::Thread).collect();
    tasks.extend((0..classes).step_by(CLASS_CHUNK).map(|c| Task::Classes(c, (c + CLASS_CHUNK).min(classes))));
    tasks.push(Task::Other);
    if young {
        // Chunks of whole pages, so no object spans two tasks.
        let chunk = 64 * super::space::PAGE_SIZE;
        tasks.extend((space.base..space.fresh).step_by(chunk).map(|c| Task::Cards(c, (c + chunk).min(space.fresh))));
        let area = &space.large_area;
        if area.fresh > area.base {
            tasks.push(Task::LargeCards(area.base, area.fresh));
        }
    }
    tasks
}

/// Marking threads: the collecting thread and helpers, one per core in all.
fn workers() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        std::env::var("RAVA_CORE_GC_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(cores).clamp(1, 16)
    })
}

/// Marks everything reachable (with `young`, what is reachable without passing through old
/// objects, plus what old objects on dirty cards reference); returns the number of newly
/// marked objects.
pub(super) fn mark(space: &mut Space, young: bool) -> usize {
    let workers = workers();
    let shared = Shared {
        space,
        pool: Mutex::new(Vec::new()),
        pooled: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        workers,
        marked: AtomicUsize::new(0),
    };
    let classes = crate::vm::vm().all_classes();
    let tasks = tasks(shared.space, classes.len(), young);
    let next = AtomicUsize::new(0);
    // Each worker takes root tasks until none is left, then traces from what it found.
    let worker = || {
        let mut found = Vec::with_capacity(4 * PACKET);
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(task) = tasks.get(i) else { break };
            shared.run(task, &classes, &mut found);
        }
        shared.work(found);
    };
    std::thread::scope(|s| {
        for _ in 1..workers {
            s.spawn(worker);
        }
        worker();
    });
    shared.marked.into_inner()
}
