//! Marking: conservative roots (thread stacks, value stacks, statics), precise VM roots, and a
//! precise trace of the heap by class layout.

use super::safepoint;
use super::space::Space;
use crate::heap::{obj, Oop, ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::klass::{Klass, KlassKind};
use std::sync::atomic::Ordering;

struct Marker<'a> {
    space: &'a mut Space,
    stack: Vec<usize>,
    count: usize,
}

impl Marker<'_> {
    /// Marks the object at exactly `start` (a known object start); queues it if new.
    fn mark_start(&mut self, start: usize) {
        if let Some(index) = self.space.page_index(start) {
            let page = self.space.page_addr(index);
            let Some(meta) = self.space.pages[index].as_mut() else { return };
            let cell = meta.cell_of(start - page);
            let (w, b) = (cell / 64, 1u64 << (cell % 64));
            if meta.marks[w] & b != 0 {
                return;
            }
            meta.marks[w] |= b;
        } else {
            let Some(l) = self.space.large.get_mut(&start) else { return };
            if l.marked {
                return;
            }
            l.marked = true;
        }
        self.count += 1;
        self.stack.push(start);
    }

    /// A word that may or may not point into an object (conservative root).
    fn maybe(&mut self, word: usize) {
        if let Some(start) = self.space.find_object(word) {
            self.mark_start(start);
        }
    }

    fn reference(&mut self, r: usize) {
        if r != 0 {
            self.mark_start(r);
        }
    }

    fn scan_range(&mut self, from: usize, to: usize) {
        let mut p = from & !7;
        while p + 8 <= to {
            // SAFETY: an address range of a stopped thread's stack or value stack.
            let w = unsafe { *(p as *const usize) };
            self.maybe(w);
            p += 8;
        }
    }

    fn trace(&mut self) {
        while let Some(o) = self.stack.pop() {
            let o = o as Oop;
            // SAFETY: a marked, live object.
            let k: &'static Klass = unsafe { obj::klass(o) };
            match k.kind {
                KlassKind::Instance => {
                    for &off in k.ref_offsets.iter() {
                        self.reference(unsafe { obj::read::<usize>(o, off) });
                    }
                }
                KlassKind::ObjArray { .. } => {
                    let len = unsafe { obj::read::<i32>(o, ARRAY_LENGTH_OFFSET) } as usize;
                    for i in 0..len {
                        self.reference(unsafe { obj::read::<usize>(o, ARRAY_BASE_OFFSET + 8 * i as u32) });
                    }
                }
                KlassKind::TypeArray { .. } => {}
            }
        }
    }
}

/// Marks everything reachable; returns the number of marked objects.
pub(super) fn mark(space: &mut Space) -> usize {
    let mut m = Marker { space, stack: Vec::with_capacity(1 << 16), count: 0 };
    for rec in safepoint::all_threads() {
        // SAFETY: the thread is stopped; its record and Thread outlive the collection.
        let t = unsafe { &*rec.thread.load(Ordering::SeqCst) };
        let rsp = rec.rsp.load(Ordering::SeqCst);
        if rsp != 0 {
            m.scan_range(rsp, rec.stack_top);
        }
        let (lo, hi) = t.stack_range();
        m.scan_range(lo as usize, hi as usize);
        for r in [t.java_thread, t.scoped_value_cache, t.pending] {
            m.reference(r as usize);
        }
        for &h in t.handles.iter() {
            m.reference(h as usize);
        }
        m.trace();
    }
    let (mut precise, mut words) = (Vec::new(), Vec::new());
    crate::vm::vm().for_each_root(&mut |o| precise.push(o as usize), &mut |w| words.push(w));
    precise.into_iter().for_each(|r| m.reference(r));
    words.into_iter().for_each(|w| m.maybe(w));
    m.trace();
    m.count
}
