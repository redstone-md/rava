//! Marking: conservative roots (thread stacks, value stacks, statics), precise VM roots, and a
//! precise trace of the heap by class layout. The trace runs on every core: mark bits are set
//! atomically, and workers share packets of objects still to scan.

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

    /// Scans objects until no worker has any left.
    fn work(&self) {
        let mut stack: Vec<usize> = Vec::with_capacity(4 * PACKET);
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

/// The roots: marks each and returns the newly marked ones.
fn roots(shared: &Shared) -> Vec<usize> {
    let mut found = Vec::new();
    let space = shared.space;
    let precise = |r: usize, found: &mut Vec<usize>| {
        if r != 0 && shared.mark_start(r) {
            found.push(r);
        }
    };
    let maybe = |w: usize, found: &mut Vec<usize>| {
        if let Some(start) = space.find_object(w) {
            if shared.mark_start(start) {
                found.push(start);
            }
        }
    };
    for rec in safepoint::all_threads() {
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
                maybe(unsafe { *(p as *const usize) }, &mut found);
                p += 8;
            }
        }
        for r in [t.java_thread, t.scoped_value_cache, t.pending] {
            precise(r as usize, &mut found);
        }
        for &h in t.handles.iter() {
            precise(h as usize, &mut found);
        }
    }
    let (mut refs, mut words) = (Vec::new(), Vec::new());
    crate::vm::vm().for_each_root(&mut |o| refs.push(o as usize), &mut |w| words.push(w));
    refs.into_iter().for_each(|r| precise(r, &mut found));
    words.into_iter().for_each(|w| maybe(w, &mut found));
    found
}

/// Marking threads: the collecting thread and helpers, one per core in all.
fn workers() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        std::env::var("RAVA_CORE_GC_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(cores).clamp(1, 16)
    })
}

/// Marks everything reachable; returns the number of marked objects.
pub(super) fn mark(space: &mut Space) -> usize {
    let workers = workers();
    let shared = Shared {
        space,
        pool: Mutex::new(Vec::new()),
        pooled: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        workers,
        marked: AtomicUsize::new(0),
    };
    for packet in roots(&shared).chunks(PACKET) {
        shared.give(packet.to_vec());
    }
    std::thread::scope(|s| {
        for _ in 1..workers {
            s.spawn(|| shared.work());
        }
        shared.work();
    });
    shared.marked.into_inner()
}
