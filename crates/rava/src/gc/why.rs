//! `RAVA_GC_WHY=<class>`: after a full collection's mark, which roots hold the live objects of
//! one class (internal name). Every root is traced in turn on the marks (saved and restored
//! around the walk), and each object counts for the first root that reaches it: the VM's own
//! roots, then class roots and statics, then the threads, so a thread stack is named only for
//! what nothing else holds.

use super::safepoint;
use super::space::Space;
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use std::collections::HashSet;
use std::sync::atomic::Ordering;

struct Walk<'a> {
    space: &'a mut Space,
    targets: HashSet<usize>,
    large_seen: HashSet<usize>,
    /// Objects of the class reached, objects reached, the root's label.
    tally: Vec<(usize, usize, String)>,
}

impl Walk<'_> {
    /// Takes the marked, not yet walked object at exactly `start`.
    fn take(&mut self, start: usize) -> bool {
        let space = &mut *self.space;
        if let Some(index) = space.page_index(start) {
            let page = space.page_addr(index);
            let Some(meta) = space.pages[index].as_mut() else { return false };
            let cell = meta.cell_of(start - page);
            let bit = 1u64 << (cell % 64);
            let marked = meta.marks[cell / 64] & bit != 0;
            meta.marks[cell / 64] &= !bit;
            marked
        } else {
            space.large.get(&start).is_some_and(|l| l.marked) && self.large_seen.insert(start)
        }
    }

    fn root(&mut self, start: usize, label: impl FnOnce() -> String) {
        if start == 0 || !self.take(start) {
            return;
        }
        let (mut stack, mut hits, mut total) = (vec![start], 0, 0);
        while let Some(o) = stack.pop() {
            total += 1;
            // SAFETY: a marked, live object.
            if self.targets.contains(&(unsafe { obj::klass(o as Oop) } as *const Klass as usize)) {
                hits += 1;
            }
            let mut found = Vec::new();
            super::mark::refs(o as Oop, |r| found.push(r));
            stack.extend(found.into_iter().filter(|&r| self.take(r)));
        }
        if hits > 0 {
            // SAFETY: as above.
            let class = unsafe { obj::klass(start as Oop) }.name.to_string();
            self.tally.push((hits, total, format!("{} -> {class} @{start:#x}", label())));
        }
    }
}

pub(super) fn report(space: &mut Space, target: &str) {
    let classes = crate::vm::vm().all_classes();
    let targets = classes.iter().filter(|k| &*k.name.to_string() == target).map(|&k| k as *const Klass as usize).collect();
    let saved: Vec<Option<Box<[u64]>>> = space.pages.iter().map(|m| m.as_ref().map(|m| m.marks.clone())).collect();
    let mut w = Walk { space, targets, large_seen: HashSet::new(), tally: Vec::new() };
    let mut other = Vec::new();
    crate::vm::vm().other_roots(false, &mut |o| other.push(o as usize));
    other.into_iter().for_each(|r| w.root(r, || "other root".into()));
    for r in super::recent::embedded() {
        w.root(r, || "compiled-code constant".into());
    }
    for &k in &classes {
        let mut held = Vec::new();
        crate::vm::vm().class_roots(&[k], &mut |o| held.push(o as usize));
        held.into_iter().for_each(|r| w.root(r, || format!("class roots of {}", k.name)));
        for f in k.fields.iter().filter(|f| f.is_static() && f.ty.is_ref()) {
            let slot = &k.statics[f.offset as usize] as *const _ as usize;
            w.root(super::statics::read(slot), || format!("static {}.{}", k.name, f.name));
        }
    }
    for (i, rec) in safepoint::all_threads().into_iter().enumerate() {
        // SAFETY: the world is stopped; the thread's record and Thread outlive the collection.
        let t = unsafe { &*rec.thread.load(Ordering::SeqCst) };
        for r in [t.java_thread, t.scoped_value_cache, t.pending, t.jni.exception] {
            w.root(r as usize, || format!("thread {i} field"));
        }
        for &h in t.handles.iter() {
            w.root(h as usize, || format!("thread {i} handle"));
        }
        let (lo, hi) = t.stack_range();
        let rsp = rec.rsp.load(Ordering::SeqCst);
        let mut ranges = vec![("value stack", lo as usize, hi as usize)];
        if rsp != 0 {
            ranges.push(("native stack", rsp, rec.stack_top));
        }
        for (what, lo, hi) in ranges {
            let mut p = lo & !7;
            while p + 8 <= hi {
                // SAFETY: an address range of a stopped thread's stack or value stack.
                if let Some(start) = w.space.find_object(unsafe { *(p as *const usize) }) {
                    let before = w.tally.len();
                    w.root(start, || format!("thread {i} {what} word {} of {}", (p - lo) / 8, (hi - lo) / 8));
                    if w.tally.len() > before && w.tally[before].0 > 100_000 {
                        dump_words(w.space, lo, hi, p);
                    }
                }
                p += 8;
            }
        }
    }
    let mut tally = std::mem::take(&mut w.tally);
    tally.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let all: usize = tally.iter().map(|t| t.0).sum();
    eprintln!("[gc-why] {all} live {target} held by {} roots:", tally.len());
    for (hits, total, label) in tally.iter().take(25) {
        eprintln!("[gc-why]   {hits:>9} of {total:>9} reached: {label}");
    }
    for (meta, marks) in w.space.pages.iter_mut().zip(saved) {
        if let (Some(meta), Some(marks)) = (meta.as_mut(), marks) {
            meta.marks = marks;
        }
    }
}

/// The words around `at` in the stack range `[lo, hi)`: code addresses (return addresses mark
/// the frames) and objects.
fn dump_words(space: &Space, lo: usize, hi: usize, at: usize) {
    #[cfg(windows)]
    let exe = crate::os::exe_base();
    #[cfg(not(windows))]
    let exe = 0;
    let from = at.saturating_sub(48 * 8).max(lo);
    let to = (at + 48 * 8).min(hi);
    eprintln!("[gc-why]     exe base {exe:#x}, words {}..{} (hit at {}):", (from - lo) / 8, (to - lo) / 8, (at - lo) / 8);
    for p in (from..to).step_by(8) {
        // SAFETY: inside the stopped thread's stack range.
        let v = unsafe { *(p as *const usize) };
        let what = if v >= exe && v < exe + (256 << 20) {
            format!("exe+{:#x}", v - exe)
        } else if crate::jit::code_cache::contains(v) {
            "compiled code".to_string()
        } else if let Some(o) = space.find_object(v) {
            // SAFETY: a live object start.
            format!("object {}", unsafe { obj::klass(o as Oop) }.name)
        } else {
            String::new()
        };
        if !what.is_empty() {
            eprintln!("[gc-why]       {:>5} {v:#018x} {what}{}", (p - lo) / 8, if p == at { "   <== HIT" } else { "" });
        }
    }
}