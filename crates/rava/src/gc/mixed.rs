//! Mixed old pages: collections copy the objects they move (up to `MAX` bytes) into old pages
//! of every size at once, one after another in the order the trace reaches them, as G1 copies
//! into its regions. An object and the ones it references then usually share a cache line or
//! two; in pages of one size class each they sat on as many pages as they had sizes (an entity,
//! its list, the list's array and the items: four pages, four misses and as many TLB entries).
//!
//! A mixed page's bitmaps have a bit per 8-byte granule, set at an object's start only (its
//! size comes from its class). Its free space is the tail past `cursor`, which collections fill
//! by bumping; space freed below it comes back only when the whole page dies. Each page counts
//! its live bytes as objects are copied in or marked (`PageMeta::live`), so a sweep need not
//! visit them.
//!
//! On by default (`RAVA_GC_MIXED=0` copies into size-class pages instead).

use super::space::{PageMeta, Space, CLASSES, PAGE_SIZE};
use crate::heap::{obj, Oop};
use crate::klass::KlassKind;
use std::sync::atomic::Ordering;

/// The size class index of mixed pages (past the real ones).
pub const MIXED: usize = CLASSES;
pub const GRANULE: usize = 8;
/// Objects up to this size are copied into mixed pages; larger ones keep their size class, whose
/// pages lose nothing to a tail too short for them.
pub const MAX: usize = 2048;
/// Granules an object can span.
const MAX_GRANULES: usize = MAX / GRANULE;

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_GC_MIXED").map_or(true, |v| v != "0"))
}

impl PageMeta {
    pub fn is_mixed(&self) -> bool {
        self.class as usize == MIXED
    }
}

/// Bytes of the live object at `o`, unrounded.
pub fn object_bytes(o: usize) -> usize {
    // SAFETY: a live object's class and, for an array, its length.
    unsafe {
        let k = obj::klass(o as Oop);
        match k.kind {
            KlassKind::Instance => k.instance_size() as usize,
            _ => crate::heap::array_bytes(k.elem_size(), obj::array_length(o as Oop)),
        }
    }
}

/// Bytes the live object at `o` takes in a mixed page.
pub fn object_size(o: usize) -> usize {
    object_bytes(o).next_multiple_of(GRANULE)
}

/// The granule of the last bit set in `bits` at or below `granule`, within the span of one
/// object (bits read with `load`, which may be atomic).
fn last_at_or_before(bits: &[u64], granule: usize, load: impl Fn(&u64) -> u64) -> Option<usize> {
    let lowest = granule.saturating_sub(MAX_GRANULES - 1);
    let mut w = granule / 64;
    let mut word = load(&bits[w]) & (u64::MAX >> (63 - granule % 64));
    loop {
        if word != 0 {
            let g = w * 64 + 63 - word.leading_zeros() as usize;
            return (g >= lowest).then_some(g);
        }
        if w == 0 || w * 64 <= lowest {
            return None;
        }
        w -= 1;
        word = load(&bits[w]);
    }
}

/// The object of a mixed page at `page` holding `addr` (by allocation bits), if any.
pub fn find_object(meta: &PageMeta, page: usize, addr: usize) -> Option<usize> {
    let g = last_at_or_before(&meta.alloc, (addr - page) / GRANULE, |w| *w)?;
    let start = page + g * GRANULE;
    // SAFETY: an allocated granule starts an object, or a dead one whose class word is zero.
    if unsafe { *(start as *const u32) } == 0 {
        return None;
    }
    (addr < start + object_size(start)).then_some(start)
}

/// The marked object of a mixed page that starts before `at` and extends past it (read while
/// nothing marks).
pub fn spanning(meta: &PageMeta, page: usize, at: usize) -> Option<usize> {
    let g = (at - page) / GRANULE;
    let prev = last_at_or_before(&meta.marks, g.checked_sub(1)?, |w| *w)?;
    let start = page + prev * GRANULE;
    (start + object_size(start) > at).then_some(start)
}

/// `PageMeta::live` of a mixed page: objects copied in above this bit, their bytes below.
const OBJECTS: u32 = 40;

/// Sets the mark of the copy at `n` in mixed page `meta` (at `page`), `size` bytes.
pub fn mark_copy(meta: &PageMeta, page: usize, n: usize, size: usize) {
    let g = (n - page) / GRANULE;
    // SAFETY: mark words are only accessed atomically during marking.
    let word = unsafe { &*(meta.marks.as_ptr().add(g / 64) as *const std::sync::atomic::AtomicU64) };
    word.fetch_or(1 << (g % 64), Ordering::Relaxed);
    meta.live.fetch_add((1 << OBJECTS) + size, Ordering::Relaxed);
}

/// Estimated live bytes of a mixed page with `marked` objects: at the mean size of the objects
/// copied into it.
pub fn live_bytes(meta: &PageMeta, marked: usize) -> usize {
    let copied = meta.live.load(Ordering::Relaxed);
    let (objects, bytes) = (copied >> OBJECTS, copied & ((1 << OBJECTS) - 1));
    if objects == 0 { 0 } else { marked * bytes / objects }
}

/// Calls `f(start, size)` for each object of mixed page `meta` (at `page`) whose start bit is
/// set in `bits` within `[from, to)`.
pub fn each(bits: &[u64], page: usize, from: usize, to: usize, mut f: impl FnMut(usize, usize)) {
    let (first, end) = ((from - page) / GRANULE, (to - page).div_ceil(GRANULE));
    let mut g = first;
    while g < end {
        let w = g / 64;
        let word = bits[w] & (u64::MAX << (g % 64));
        if word == 0 {
            g = (w + 1) * 64;
            continue;
        }
        let s = w * 64 + word.trailing_zeros() as usize;
        if s >= end {
            break;
        }
        let start = page + s * GRANULE;
        let size = object_size(start);
        f(start, size);
        g = s + size / GRANULE;
    }
}

/// Holes shorter than this are left alone (a run costs a marker a trip to the lock).
const MIN_HOLE: usize = 256;

/// Live share (percent) up to which a full collection empties a mixed page (G1's
/// G1MixedGCLiveThresholdPercent is 85): half left the dead space of fuller pages unused, 0.5 GiB
/// on All the Mods 10.
const SPARSE_PERCENT: usize = 75;

/// Notes the free ranges `holes` of mixed page `meta` (at `page`), each the space of objects
/// that died since the last collection, for later copies.
pub fn add_holes(meta: &mut PageMeta, page: usize, holes: &[(usize, usize)]) {
    let off = |a: usize| ((a - page) / GRANULE) as u32;
    meta.holes.extend(holes.iter().filter(|&&(s, e)| e - s >= MIN_HOLE).map(|&(s, e)| (off(s), off(e))));
}

impl Space {
    /// A run of at least `need` bytes in a mixed page for a collection's copies: `(page, start,
    /// end)`, a hole or else the page's whole free tail, which the caller owns until it gives the
    /// rest back (`return_tail`).
    pub fn mixed_run(&mut self, need: usize) -> Option<(usize, usize, usize)> {
        while let Some(p) = self.mixed_partial.pop() {
            let page = self.page_addr(p);
            let meta = self.pages[p].as_mut().expect("mixed page");
            if let Some(i) = meta.holes.iter().position(|&(s, e)| (e - s) as usize * GRANULE >= need) {
                let (s, e) = meta.holes.swap_remove(i);
                // The page may serve again, from another hole or its tail.
                self.mixed_partial.push(p);
                return Some((p, page + s as usize * GRANULE, page + e as usize * GRANULE));
            }
            if PAGE_SIZE - meta.cursor * GRANULE >= need {
                let start = page + meta.cursor * GRANULE;
                meta.cursor = meta.cells();
                return Some((p, start, page + PAGE_SIZE));
            }
        }
        let p = self.take_page(MIXED, true)?;
        let page = self.page_addr(p);
        self.pages[p].as_mut().unwrap().cursor = PAGE_SIZE / GRANULE;
        Some((p, page, page + PAGE_SIZE))
    }

    /// Before a full collection: mixed pages at most SPARSE_PERCENT live (by the last count) are emptied
    /// by it, their objects moving into other pages as young ones do (what a root holds stays),
    /// so the space their dead objects left comes back; G1 picks its old regions the same way.
    pub fn select_sparse(&mut self) {
        if !super::evacuate::enabled() {
            return;
        }
        for meta in self.pages.iter_mut().flatten().filter(|m| m.is_mixed()) {
            let marked = meta.marks.iter().map(|w| w.count_ones() as usize).sum();
            meta.sparse = live_bytes(meta, marked) <= PAGE_SIZE * SPARSE_PERCENT / 100;
            if meta.sparse {
                meta.holes = Vec::new();
            }
        }
        let pages = &self.pages;
        self.mixed_partial.retain(|&p| !pages[p].as_ref().is_some_and(|m| m.sparse));
    }

    /// Gives back the rest `[next, end)` of a mixed run: its page's tail, or a hole.
    pub fn return_tail(&mut self, next: usize, end: usize) {
        let p = self.page_index(next.min(end - 1)).expect("mixed run in the page area");
        let page = self.page_addr(p);
        let meta = self.pages[p].as_mut().expect("mixed page");
        if end == page + PAGE_SIZE {
            meta.cursor = (next - page) / GRANULE;
            if next < end {
                self.mixed_partial.push(p);
            }
        } else {
            add_holes(meta, page, &[(next, end)]);
            if !meta.holes.is_empty() {
                self.mixed_partial.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_bit_within_one_object_span() {
        let mut bits = vec![0u64; 8];
        bits[0] = 1 << 5;
        assert_eq!(last_at_or_before(&bits, 5, |w| *w), Some(5));
        assert_eq!(last_at_or_before(&bits, 70, |w| *w), Some(5));
        assert_eq!(last_at_or_before(&bits, 4, |w| *w), None);
        // Beyond one object's span the earlier start cannot hold the address.
        assert_eq!(last_at_or_before(&bits, 5 + MAX_GRANULES, |w| *w), None);
        bits[3] = 1 << 63;
        assert_eq!(last_at_or_before(&bits, 300, |w| *w), Some(255));
    }
}
