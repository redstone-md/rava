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
    /// Workers that have started: the trace is done when they are all idle.
    joined: AtomicUsize,
    marked: AtomicUsize,
    /// The cells copies were made into (`finish_run`), for the next young collection's second
    /// chance.
    pub(super) promoted: Mutex<Vec<(usize, usize)>>,
    /// The cells young again this collection (`Space::aging`): a reference to one of them keeps
    /// its card dirty, as one to a copy does.
    pub(super) young_again: &'a [(usize, usize)],
    /// Aging objects marked from once-only roots (`pin`).
    pinned: Mutex<Vec<usize>>,
    /// Objects marked from thread stacks (conservatively) and from the other roots (RAVA_GC_LOG).
    by_stacks: AtomicUsize,
    by_roots: AtomicUsize,
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
    fn work(&self, mut stack: Vec<usize>, runs: &mut super::evacuate::Runs) -> usize {
        let mut scanned = 0;
        // Objects pass through a small FIFO between the stack and the scan, their first line
        // prefetched on the way in, so the scan rarely waits on a miss.
        let mut ring = [0usize; PREFETCH];
        let (mut head, mut len) = (0, 0);
        loop {
            loop {
                while len < PREFETCH {
                    let Some(o) = stack.pop() else { break };
                    // A reference slot to follow (`scan`): its object is copied now, after
                    // the objects it reaches were, so a graph is copied depth first.
                    if o & SLOT != 0 {
                        // What it queued (a fresh copy, in cache) is scanned at once, before the
                        // ring's objects: those would put their own copies between it and its
                        // children.
                        let queued = stack.len();
                        self.update(o & !SLOT, &mut stack, runs);
                        if stack.len() > queued {
                            let n = stack.pop().unwrap();
                            scanned += 1;
                            self.scan(n as Oop, &mut stack, runs);
                        }
                        self.share(&mut stack);
                        continue;
                    }
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
                self.share(&mut stack);
            }
            if let Some(p) = self.take() {
                stack = p;
                continue;
            }
            // Out of work: done once every worker is, unless a packet shows up meanwhile.
            // Spins briefly, then yields the core (HotSpot's task terminator): a pure spin took
            // CPU time from the workers that still had work whenever the markers outnumbered
            // the free cores (or shared a core with them, as SMT siblings do).
            self.idle.fetch_add(1, Ordering::AcqRel);
            let mut spins = 0u32;
            loop {
                if self.pooled.load(Ordering::Acquire) > 0 {
                    self.idle.fetch_sub(1, Ordering::AcqRel);
                    break;
                }
                // Done once every worker that joined is idle; one joining later finds no work.
                let joined = self.joined.load(Ordering::Acquire);
                if self.idle.load(Ordering::Acquire) == joined {
                    self.marked.fetch_add(scanned, Ordering::Relaxed);
                    return scanned;
                }
                spins += 1;
                if spins < IDLE_SPINS {
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
        }
    }

    /// Shares surplus while others may be waiting for work.
    #[inline]
    fn share(&self, stack: &mut Vec<usize>) {
        if stack.len() >= 2 * PACKET && self.pooled.load(Ordering::Relaxed) == 0 && self.idle.load(Ordering::Relaxed) > 0 {
            self.give(stack.split_off(stack.len() - PACKET));
        }
    }

    fn scan(&self, o: Oop, stack: &mut Vec<usize>, runs: &mut super::evacuate::Runs) {
        if self.evacuate {
            // Evacuating, the slots of objects still to be copied go on the stack and are
            // followed as they come off it (G1's order): an object's children are then copied
            // next to it, depth first. Following them here copied all of an array's elements,
            // then all of their children, and so on, breadth first, which put an object and
            // what it references far apart. Other slots are followed at once.
            return ref_slots(o, |slot| {
                // SAFETY: a reference field or element of a live object.
                let t = unsafe { obj::load_ref_at(slot) } as usize;
                if t == 0 {
                } else if self.movable(t) {
                    // SAFETY: a prefetch never faults.
                    unsafe { std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(t as *const i8) };
                    stack.push(slot | SLOT);
                } else {
                    self.update(slot, stack, runs);
                }
            });
        }
        refs(o, |r| {
            if self.mark_start(r) {
                stack.push(r);
            }
        });
    }
}

/// Tags a reference slot on a marker's stack, among objects (slots are 4-byte aligned at least).
const SLOT: usize = 1;

/// Calls `f` with each non-null reference field or element of the live object `o`.
#[inline]
pub(super) fn refs(o: Oop, mut f: impl FnMut(usize)) {
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
/// Idle checks a marker spins through before it starts yielding its core.
const IDLE_SPINS: u32 = 1 << 10;

enum Task {
    /// A thread's stacks (conservatively) and its precise references.
    Thread(&'static safepoint::Record),
    /// The roots of a range of classes.
    Classes(usize, usize),
    /// The listed static slots `[from, to)` whose value changed since the last collection (a
    /// young collection: the other roots of those classes are old; `statics.rs`).
    Statics(usize, usize),
    /// Objects `[from, to)` of those stored into write-once VM roots since the last collection
    /// (`recent.rs`).
    Noted(usize, usize),
    /// The VM's other roots (with `true`, those of a young collection).
    Other(bool),
    /// Dirty cards of `[from, to)` in the page area (a young collection).
    Cards(usize, usize),
    /// Dirty cards of `[from, to)` in the large-object area.
    LargeCards(usize, usize),
}

impl Shared<'_> {
    /// Notes the aging objects among `refs`, roots a collection hears of once (objects a new
    /// class or compiled code references, `recent.rs`): no later young collection marks them
    /// again, so they must not be young again (`age` leaves them old).
    fn pin(&self, refs: &[usize]) {
        let aging: Vec<usize> = refs.iter().copied().filter(|&r| in_ranges(self.young_again, r)).collect();
        if !aging.is_empty() {
            self.pinned.lock().unwrap().extend(aging);
        }
    }

    /// Marks the object `r` points at exactly, queuing it if new.
    fn precise(&self, r: usize, out: &mut Vec<usize>) {
        if r != 0 && self.mark_start(r) {
            self.by_roots.fetch_add(1, Ordering::Relaxed);
            out.push(r);
        }
    }

    /// Marks the object a word may point into, queuing it if new.
    fn maybe(&self, w: usize, out: &mut Vec<usize>) {
        if let Some(start) = self.space.find_object(w) {
            if self.mark_start(start) {
                self.by_stacks.fetch_add(1, Ordering::Relaxed);
                out.push(start);
            }
        }
    }

    /// Runs a root task: what it finds is marked and queued on `out` (a card task queues the
    /// reference slots of old objects instead); reference statics are marked too, or, when
    /// evacuating, their slots queued on `statics` to follow moves.
    fn run(&self, task: &Task, roots: &Roots, out: &mut Vec<usize>, statics: &mut Vec<usize>, changed: &mut Vec<usize>) {
        let classes = roots.classes;
        let mut static_slot = |slot: usize, out: &mut Vec<usize>| {
            if self.evacuate {
                statics.push(slot);
            } else {
                self.precise(super::statics::read(slot), out);
            }
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
                for r in [t.java_thread, t.scoped_value_cache, t.pending, t.jni.exception] {
                    self.precise(r as usize, out);
                }
                for &h in t.handles.iter() {
                    self.precise(h as usize, out);
                }
            }
            Task::Classes(from, to) => {
                let mut refs = Vec::new();
                crate::vm::vm().class_roots(&classes[from..to], &mut |o| refs.push(o as usize));
                self.pin(&refs);
                refs.into_iter().for_each(|r| self.precise(r, out));
                crate::vm::vm().class_statics(&classes[from..to], &mut |slot| static_slot(slot, out));
            }
            Task::Statics(from, to) => {
                let (addr, seen) = &roots.statics;
                for i in from..to {
                    // A slot still holding what the last collection left references an old
                    // object, unless that object was promoted then and is young again now.
                    let value = super::statics::read(addr[i]);
                    if value != seen[i] || in_ranges(roots.second_chance, value) {
                        changed.push(i);
                        static_slot(addr[i], out);
                    }
                }
            }
            Task::Noted(from, to) => {
                let noted = &roots.noted[from..to];
                self.pin(noted);
                noted.iter().for_each(|&r| self.precise(r, out));
            }
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
                // In a mixed page an object starting before the card may reach into it.
                if meta.is_mixed() {
                    if let Some(start) = super::mixed::spanning(meta, page, card) {
                        self.scan_old(start, card, &mut last, out);
                    }
                }
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
/// the noted and embedded roots in chunks, threads, classes in chunks and other roots. A young
/// collection scans only the statics of the first `old` classes.
fn tasks(space: &Space, classes: usize, old: usize, young: bool, statics: usize, noted: usize) -> Vec<Task> {
    const CLASS_CHUNK: usize = 512;
    const SLOT_CHUNK: usize = 1 << 14;
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
    tasks.extend((0..noted).step_by(SLOT_CHUNK).map(|c| Task::Noted(c, (c + SLOT_CHUNK).min(noted))));
    tasks.extend(safepoint::all_threads().into_iter().map(Task::Thread));
    if young {
        tasks.extend((0..statics).step_by(SLOT_CHUNK).map(|c| Task::Statics(c, (c + SLOT_CHUNK).min(statics))));
    }
    tasks.extend(chunks(old, classes).map(|(a, b)| Task::Classes(a, b)));
    tasks.push(Task::Other(young));
    tasks
}

/// Marking threads: the collecting thread and helpers, one per core in all.
pub(super) fn workers() -> usize {
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
    // Recently promoted objects are young again: their marks go, so the trace reaches (and
    // keeps) only those still in use; the others are freed by this sweep.
    let aging = std::mem::take(&mut space.aging);
    let aging = if young && super::evacuate::enabled() { aging } else { Vec::new() };
    let second_chance: Vec<(usize, usize)> = aging.iter().map(|&(s, e, _)| (s, e)).collect();
    clear_marks(space, &second_chance);
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
        joined: AtomicUsize::new(0),
        marked: AtomicUsize::new(0),
        promoted: Mutex::new(Vec::new()),
        young_again: &second_chance,
        pinned: Mutex::new(Vec::new()),
        by_stacks: AtomicUsize::new(0),
        by_roots: AtomicUsize::new(0),
    };
    let classes = crate::vm::vm().all_classes();
    let (noted, old) = super::recent::take(classes.len(), young);
    let old = if young { old.min(classes.len()) } else { 0 };
    let statics = if young { super::statics::listed(&classes, old) } else { Default::default() };
    let roots = Roots { classes: &classes, noted: &noted, statics, second_chance: &second_chance };
    let tasks = tasks(shared.space, classes.len(), old, young, roots.statics.0.len(), noted.len());
    let changed = Mutex::new(Vec::new());
    // The card tasks run first, and nothing is marked until all are done: a mark set meanwhile
    // would make a young object look old, to be scanned again as a card root.
    let cards = tasks.iter().take_while(|t| matches!(t, Task::Cards(..) | Task::LargeCards(..))).count();
    let (next_card, next) = (AtomicUsize::new(0), AtomicUsize::new(cards));
    // Completed card and root tasks: the phases wait for the tasks, not for every helper (one
    // woken late would hold up the others; it finds no task left and joins the trace).
    let (cards_done, roots_done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let wait_for = |done: &AtomicUsize, n: usize| {
        let mut spins = 0u32;
        while done.load(Ordering::Acquire) < n {
            spins += 1;
            if spins < IDLE_SPINS { std::hint::spin_loop() } else { std::thread::yield_now() }
        }
    };
    // Per worker: root tasks ms, trace ms, objects found by roots, objects scanned (RAVA_GC_LOG).
    let started = std::time::Instant::now();
    let stats = Mutex::new(Vec::new());
    // Each worker takes root tasks until none is left, then traces from what it found.
    // Evacuating, every root is marked (kept in place) before anything moves.
    let worker = || {
        shared.joined.fetch_add(1, Ordering::AcqRel);
        let mut found = Vec::with_capacity(4 * PACKET);
        let mut runs = super::evacuate::Runs::default();
        let (mut slots, mut statics, mut changes) = (Vec::new(), Vec::new(), Vec::new());
        if cards > 0 {
            while let Some(task) = tasks[..cards].get(next_card.fetch_add(1, Ordering::Relaxed)) {
                shared.run(task, &roots, &mut slots, &mut statics, &mut changes);
                cards_done.fetch_add(1, Ordering::AcqRel);
            }
            wait_for(&cards_done, cards);
            if !shared.evacuate {
                // SAFETY: reference slots of live old objects.
                slots.drain(..).for_each(|s| shared.precise(unsafe { obj::load_ref_at(s) } as usize, &mut found));
            }
        }
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(task) = tasks.get(i) else { break };
            shared.run(task, &roots, &mut found, &mut statics, &mut changes);
            roots_done.fetch_add(1, Ordering::AcqRel);
        }
        changed.lock().unwrap().extend(changes);
        let (roots_ms, found_n) = (started.elapsed().as_secs_f64() * 1e3, found.len());
        if shared.evacuate {
            wait_for(&roots_done, tasks.len() - cards);
            slots.into_iter().for_each(|s| shared.update(s, &mut found, &mut runs));
            statics.into_iter().for_each(|s| shared.update_static(s, &mut found, &mut runs));
        }
        let scanned = shared.work(found, &mut runs);
        shared.finish_runs(&runs);
        stats.lock().unwrap().push((roots_ms, started.elapsed().as_secs_f64() * 1e3, found_n, scanned));
    };
    super::pool::run(workers - 1, &worker);
    let chances = (young && !second_chance.is_empty()).then(|| chance_bytes(shared.space, &second_chance));
    if let (Some((given, kept)), true) = (chances, std::env::var_os("RAVA_GC_LOG").is_some()) {
        eprintln!(
            "[gc-age] {} KiB of recent promotions young again, {} KiB still in use (tenure {}); marked from stacks {}, from other roots {}",
            given >> 10,
            kept >> 10,
            shared.space.tenure,
            shared.by_stacks.load(Ordering::Relaxed),
            shared.by_roots.load(Ordering::Relaxed)
        );
    }
    if !young && std::env::var_os("RAVA_GC_LOG").is_some() {
        let s = stats.into_inner().unwrap();
        let rows: Vec<String> = s.iter().map(|(r, t, f, n)| format!("{r:.0}/{t:.0}ms {f}->{n}")).collect();
        eprintln!("[gc-mark] {} tasks, workers (roots/end, found->scanned): {}", tasks.len(), rows.join(", "));
    }
    // The statics hold their final values now (moved objects followed).
    super::statics::settle(&classes, young.then(|| changed.into_inner().unwrap()).as_deref());
    let marked = shared.marked.into_inner();
    let mut promoted = shared.promoted.into_inner().unwrap();
    // Only young collections age objects; a full one leaves every survivor old.
    if young && super::evacuate::enabled() {
        promoted.sort_unstable();
        let mut pinned = shared.pinned.into_inner().unwrap();
        pinned.sort_unstable();
        if let Some((given, kept)) = chances {
            space.tenure = super::aging::tenure_after(space.tenure, given, kept);
        }
        space.aging = super::aging::age(space, &aging, &promoted, &pinned);
    }
    marked
}

/// Whether `value` lies in one of the sorted, disjoint `ranges`.
pub(super) fn in_ranges(ranges: &[(usize, usize)], value: usize) -> bool {
    let i = ranges.partition_point(|r| r.0 <= value);
    i > 0 && value < ranges[i - 1].1
}

/// Bytes of the cells in `ranges`, and of those marked now (a mark word at a time).
fn chance_bytes(space: &Space, ranges: &[(usize, usize)]) -> (usize, usize) {
    let (mut given, mut kept) = (0, 0);
    for &(start, end) in ranges {
        let Some(index) = space.page_index(start) else { continue };
        let page = space.page_addr(index);
        let Some(meta) = space.pages[index].as_ref() else { continue };
        if meta.is_mixed() {
            given += end - start;
            super::mixed::each(&meta.marks, page, start, end, |_, size| kept += size);
            continue;
        }
        let cell = meta.cell_size as usize;
        let (first, last) = (meta.cell_of(start - page), meta.cell_of(end - page));
        given += (last - first) * cell;
        let mut c = first;
        while c < last {
            let n = (last - c).min(64 - c % 64);
            let mask = (u64::MAX >> (64 - n)) << (c % 64);
            // SAFETY: mark words, read after the markers are done.
            kept += (unsafe { *meta.marks.as_ptr().add(c / 64) } & mask).count_ones() as usize * cell;
            c += n;
        }
    }
    (given, kept)
}

/// Clears the marks of the cells in `ranges` (cells of old pages, still allocated).
fn clear_marks(space: &mut Space, ranges: &[(usize, usize)]) {
    for &(start, end) in ranges {
        let Some(index) = space.page_index(start) else { continue };
        let page = space.page_addr(index);
        let Some(meta) = space.pages[index].as_mut() else { continue };
        let (first, last) = (meta.cell_of(start - page), meta.cell_of(end - page));
        for cell in first..last {
            meta.marks[cell / 64] &= !(1u64 << (cell % 64));
        }
    }
}

/// What root tasks read: every class, the noted objects, and for a young collection the listed
/// static slots with their values when the last collection ended (`statics.rs`).
struct Roots<'a> {
    classes: &'a [&'static Klass],
    noted: &'a [usize],
    statics: (Vec<usize>, Vec<usize>),
    /// The cells young again this collection (`Space::aging`).
    second_chance: &'a [(usize, usize)],
}

#[cfg(test)]
#[path = "evacuate_tests.rs"]
mod evacuate_tests;
