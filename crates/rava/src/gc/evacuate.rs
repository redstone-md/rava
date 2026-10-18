//! Evacuation: an object of an allocation page that the trace reaches through the heap (an old
//! object's dirty card, another object already moved or kept) or a static is copied into an old
//! page and the reference rewritten; its cell is freed by the sweep. Objects another root
//! references are marked in place first (`mark.rs`), so stacks, the VM's own references and
//! compiled code's constants (`recent::embed`) never see a move; so are locked objects
//! (inflated monitors are found by address) and large ones. A full collection also moves what
//! earlier collections kept in place, once no root holds it.
//!
//! Allocation pages thus keep few survivors, and are reused whole by the next allocations
//! instead of turning into pages of scattered short holes, while survivors are packed densely.
//!
//! A moved object's mark word holds its new address tagged `FORWARDED` (lock state 3, which
//! locking never uses); a marker copying it holds it at `BUSY` meanwhile, and others wait.

use super::mark::Shared;
use super::mixed;
use super::space::{class_of, CLASSES, CLASS_SIZES};
use crate::heap::{obj, Oop};
use std::sync::atomic::{AtomicU64, Ordering};

const FORWARDED: u64 = 3;
const BUSY: u64 = FORWARDED;

/// Whether young collections move objects (`RAVA_GC_MOVE=0` keeps them in place; so does
/// `RAVA_GC_VERIFY`, whose check compares marks of objects in place).
pub(super) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_GC_MOVE").map_or(true, |v| v != "0") && std::env::var_os("RAVA_GC_VERIFY").is_none())
}

/// A marker's runs of old cells, per size class, and its run of a mixed page (`mixed.rs`, last):
/// `(start, next, end)`. The copies in a class's `start..next` are marked when the run is done
/// (`Shared::finish_run`): one atomic update per mark word rather than per copy, which markers
/// sharing a page fought over. A mixed run is the marker's own page tail; its copies are marked
/// one by one.
pub(super) struct Runs([(usize, usize, usize); CLASSES + 1]);

impl Default for Runs {
    fn default() -> Self {
        Runs([(0, 0, 0); CLASSES + 1])
    }
}

impl Shared<'_> {
    /// Rewrites the reference at `slot` to where its object is after this collection (moving
    /// it if young), queuing what must be scanned.
    pub(super) fn update(&self, slot: usize, stack: &mut Vec<usize>, runs: &mut Runs) {
        // SAFETY: a reference field or element of a live object.
        let t = unsafe { obj::load_ref_at(slot) } as usize;
        if t == 0 {
            return;
        }
        let n = self.forward(t, stack, runs);
        if n != t {
            // SAFETY: as above; the object moved, the slot follows.
            unsafe { obj::store_ref_raw(slot, n as Oop) };
        }
        // A copy, or an object still aging, is young again in the next young collection: the
        // slot's card stays dirty so that collection finds this reference.
        if n != t || super::mark::in_ranges(self.young_again, n) {
            crate::gc::cards::dirty(slot);
        }
    }

    /// `update` for a static slot (a full-width reference; no card).
    pub(super) fn update_static(&self, slot: usize, stack: &mut Vec<usize>, runs: &mut Runs) {
        // SAFETY: a reference static's slot.
        let cell = unsafe { &*(slot as *const AtomicU64) };
        let t = cell.load(Ordering::Relaxed) as usize;
        if t != 0 {
            let n = self.forward(t, stack, runs);
            if n != t {
                cell.store(n as u64, Ordering::Relaxed);
            }
        }
    }

    /// Whether `update` may copy the object at `t`: unmarked in a page whose objects move.
    #[inline]
    pub(super) fn movable(&self, t: usize) -> bool {
        let space = self.space;
        let Some(index) = space.page_index(t) else { return false };
        let Some(meta) = space.pages[index].as_ref() else { return false };
        if meta.old && !meta.sparse {
            return false;
        }
        let cell = meta.cell_of(t - space.page_addr(index));
        // SAFETY: the mark words are only accessed atomically during marking.
        let word = unsafe { &*(meta.marks.as_ptr().add(cell / 64) as *const AtomicU64) };
        word.load(Ordering::Relaxed) & (1 << (cell % 64)) == 0
    }

    /// Where the object at `t` is after this collection: itself if old, kept in place (large,
    /// locked, or marked from a root) or out of old cells; else a copy in an old page.
    fn forward(&self, t: usize, stack: &mut Vec<usize>, runs: &mut Runs) -> usize {
        let space = self.space;
        let keep = |stack: &mut Vec<usize>| {
            if self.mark_start(t) {
                stack.push(t);
            }
            t
        };
        let Some(index) = space.page_index(t) else { return keep(stack) };
        let Some(meta) = space.pages[index].as_ref() else { return t };
        let cell = meta.cell_of(t - space.page_addr(index));
        // SAFETY: the mark words are only accessed atomically during marking.
        let word = unsafe { &*(meta.marks.as_ptr().add(cell / 64) as *const AtomicU64) };
        if word.load(Ordering::Relaxed) & (1 << (cell % 64)) != 0 {
            return t;
        }
        // Objects in old pages stay (only a full collection reaches unmarked ones there), but
        // for those of the mixed pages a full collection empties (`Space::select_sparse`).
        if meta.old && !meta.sparse {
            return keep(stack);
        }
        // SAFETY: a live young object.
        let mark = unsafe { obj::mark(t as Oop) };
        loop {
            let m = mark.load(Ordering::Acquire);
            match m & 3 {
                FORWARDED if m == BUSY => std::hint::spin_loop(),
                FORWARDED => return (m & !3) as usize,
                0 => {
                    if mark.compare_exchange(m, BUSY, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                        continue;
                    }
                    let Some(n) = self.copy(t, m, runs) else {
                        mark.store(m, Ordering::Release);
                        return keep(stack);
                    };
                    mark.store(n as u64 | FORWARDED, Ordering::Release);
                    stack.push(n);
                    return n;
                }
                _ => return keep(stack),
            }
        }
    }

    /// Copies the object at `t` (mark word `m`) into an old cell, marked; None without one.
    fn copy(&self, t: usize, m: u64, runs: &mut Runs) -> Option<usize> {
        let size = mixed::object_bytes(t);
        if size <= mixed::MAX && mixed::enabled() {
            return self.copy_mixed(t, m, size, runs);
        }
        let class = class_of(size);
        let cell = CLASS_SIZES[class] as usize;
        let run = &mut runs.0[class];
        if run.1 + cell > run.2 {
            // Retire the run even if no replacement can be allocated.
            self.finish_run(std::mem::take(run));
            let promote = self.promote.lock().unwrap();
            let space: *mut super::space::Space = *promote;
            // SAFETY: the space's page lists and fresh pages change only under this lock; the
            // markers read no page `old_run` hands out or mutates the free cells of.
            let (_, start, end) = unsafe { (*space).old_run(class) }?;
            *run = (start, start, end);
        }
        let n = run.1;
        run.1 += cell;
        // SAFETY: a free old cell of at least `size` bytes, owned by this marker; the source is
        // a live object only this marker copies (its mark word holds BUSY).
        unsafe {
            std::ptr::copy_nonoverlapping(t as *const u8, n as *mut u8, size);
            obj::mark(n as Oop).store(m, Ordering::Relaxed);
        }
        Some(n)
    }

    /// `copy` into the marker's mixed run, in trace order.
    fn copy_mixed(&self, t: usize, m: u64, size: usize, runs: &mut Runs) -> Option<usize> {
        let need = size.next_multiple_of(mixed::GRANULE);
        let run = &mut runs.0[mixed::MIXED];
        if run.1 + need > run.2 {
            self.finish_run(std::mem::take(run));
            let promote = self.promote.lock().unwrap();
            let space: *mut super::space::Space = *promote;
            // SAFETY: as in `copy`; the page tail handed out is this marker's alone.
            let (_, start, end) = unsafe { (*space).mixed_run(need) }?;
            drop(promote);
            *run = (start, start, end);
        }
        let n = run.1;
        run.1 += need;
        // SAFETY: as in `copy`, into the marker's own free granules.
        unsafe {
            std::ptr::copy_nonoverlapping(t as *const u8, n as *mut u8, size);
            obj::mark(n as Oop).store(m, Ordering::Relaxed);
        }
        let space = self.space;
        let index = space.page_index(n).expect("mixed run in the page area");
        mixed::mark_copy(space.pages[index].as_ref().expect("mixed page"), space.page_addr(index), n, need);
        Some(n)
    }

    /// Marks the copies of a run, `start..next` (cells of one page); a mixed run's copies are
    /// marked already, and the rest of its page tail goes back.
    pub(super) fn finish_run(&self, (start, next, end): (usize, usize, usize)) {
        if end == 0 {
            return;
        }
        let space = self.space;
        let index = space.page_index(start).expect("old run in the page area");
        let meta = space.pages[index].as_ref().expect("old page");
        if meta.is_mixed() {
            let promote = self.promote.lock().unwrap();
            let space: *mut super::space::Space = *promote;
            // SAFETY: the tail is this marker's until given back, under the lock.
            unsafe { (*space).return_tail(next, end) };
            drop(promote);
            if next > start {
                self.promoted.lock().unwrap().push((start, next));
            }
            return;
        }
        if next == start {
            return;
        }
        let page = space.page_addr(index);
        let (first, end) = (meta.cell_of(start - page), meta.cell_of(next - page));
        let mut c = first;
        while c < end {
            let n = (end - c).min(64 - c % 64);
            // SAFETY: as in `forward`.
            let word = unsafe { &*(meta.marks.as_ptr().add(c / 64) as *const AtomicU64) };
            word.fetch_or((u64::MAX >> (64 - n)) << (c % 64), Ordering::Relaxed);
            c += n;
        }
        self.promoted.lock().unwrap().push((start, next));
    }

    /// Marks the copies of every run of `runs`.
    pub(super) fn finish_runs(&self, runs: &Runs) {
        runs.0.iter().for_each(|&r| self.finish_run(r));
    }
}
