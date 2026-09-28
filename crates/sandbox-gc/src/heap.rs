//! The sandbox's own generational heap.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::*;

pub(crate) struct GenHeap {
    pub(crate) young: Region,
    pub(crate) old: Region,
    pub(crate) classes: Vec<ClassDesc>,
    pub(crate) minor_gcs: u64,
    pub(crate) copied_objs: u64,
}

impl GenHeap {
    pub(crate) fn new(young_cap: usize, old_cap: usize, classes: Vec<ClassDesc>) -> Self {
        GenHeap {
            young: Region::new(young_cap),
            old: Region::new(old_cap),
            classes,
            minor_gcs: 0,
            copied_objs: 0,
        }
    }

    /// Bump-allocate in the nursery. On a full nursery, run a minor GC (copy the
    /// live set reachable from `roots` to tenured) then retry. `roots` is updated
    /// in place — a moved root's pointer is rewritten to its tenured copy.
    #[inline]
    pub(crate) fn alloc(&mut self, class_id: u32, roots: &mut [*mut u8]) -> *mut u8 {
        let nf = self.classes[class_id as usize].nfields;
        let size = obj_size(nf);
        if self.young.top + size > self.young.cap() {
            self.minor_gc(roots);
            if self.young.top + size > self.young.cap() {
                panic!("nursery cannot fit a single {size}-byte object");
            }
        }
        let p = unsafe { self.young.base().add(self.young.top) };
        self.young.top += size;
        unsafe {
            // Pack the header in one 64-bit store (class<<32 | nfields) + a null
            // forwarding word. Cells are NOT zeroed — the caller initialises every
            // cell before any GC can observe it (matches a JVM TLAB bump with the
            // object's <init> writing all fields).
            *(p as *mut u64) = (nf as u64) << 32 | class_id as u64; // [0]=class, [4]=nf
            *(p.add(8) as *mut *mut u8) = std::ptr::null_mut();
        }
        p
    }

    /// Cheney semispace copy: nursery (from-space) → tenured (to-space). Only
    /// young objects move; tenured pointers are stable (newer→older pointer
    /// direction in these workloads ⇒ no remembered set needed).
    pub(crate) fn minor_gc(&mut self, roots: &mut [*mut u8]) {
        self.minor_gcs += 1;
        let scan_start = self.old.top;
        for r in roots.iter_mut() {
            if !r.is_null() {
                *r = self.copy_obj(*r);
            }
        }
        // Scan tenured to-space; forward each copied object's ref cells.
        let mut scan = scan_start;
        while scan < self.old.top {
            let obj = unsafe { self.old.base().add(scan) };
            let nf = unsafe { o_nf(obj) };
            let cid = unsafe { o_class(obj) };
            let mask = self.classes[cid as usize].ref_mask;
            if mask != 0 {
                for i in 0..nf as usize {
                    if mask & (1 << i) != 0 {
                        let child = unsafe { cell(obj, i) } as usize as *mut u8;
                        if !child.is_null() {
                            let nc = self.copy_obj(child);
                            unsafe { set_cell(obj, i, nc as i64) };
                        }
                    }
                }
            }
            scan += obj_size(nf);
        }
        // Reclaim the whole nursery in O(1).
        self.young.top = 0;
    }

    #[inline]
    pub(crate) fn copy_obj(&mut self, p: *mut u8) -> *mut u8 {
        if !self.young.contains(p) {
            return p; // already tenured (or interior) — never moves in a minor GC
        }
        let fwd = unsafe { o_fwd(p) };
        if !fwd.is_null() {
            return fwd;
        }
        let nf = unsafe { o_nf(p) };
        let size = obj_size(nf);
        if self.old.top + size > self.old.cap() {
            panic!("tenured space exhausted (RAVA_HARD_MB) — sandbox abort, host safe");
        }
        let np = unsafe { self.old.base().add(self.old.top) };
        self.old.top += size;
        unsafe {
            std::ptr::copy_nonoverlapping(p, np, size);
            o_set_fwd(p, np);
        }
        self.copied_objs += 1;
        np
    }
}
