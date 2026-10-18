//! Sweeping: frees unmarked objects by bitmap alone. The marks become the pages' allocation
//! bitmaps (dead cells are left untouched until handed out again); pages without survivors are
//! released whole. Pages are swept in chunks on every marking thread: a young collection of a
//! 5 GiB heap otherwise spent 3.8 ms of its pause reading the mark words of 20 000 pages on one
//! core.

use super::space::{PageMeta, Space, PAGE_SHIFT};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Free large-object memory kept resident after a collection, for the next large arrays.
const LARGE_FREE_KEEP: usize = 32 << 20;

/// Pages a sweeping thread takes at a time.
const CHUNK: usize = 256;

/// What sweeping a chunk of pages found: the lists are merged in page order, as one thread
/// sweeping them all would have built them.
#[derive(Default)]
struct Swept {
    live: usize,
    partial_free: usize,
    empty: Vec<usize>,
    partial: Vec<(u8, usize)>,
    old_partial: Vec<(u8, usize)>,
    mixed_partial: Vec<usize>,
}

/// The page table, for sweeping threads that each take disjoint pages.
struct Pages(*mut Option<PageMeta>);

// SAFETY: every chunk of the table is swept by one thread.
unsafe impl Sync for Pages {}

impl Pages {
    /// The slot of page `index`.
    ///
    /// # Safety
    /// No other thread may use the slot meanwhile.
    #[allow(clippy::mut_from_ref)]
    unsafe fn slot(&self, index: usize) -> &mut Option<PageMeta> {
        &mut *self.0.add(index)
    }
}

/// Sweeps every page and large object; returns live bytes.
pub(super) fn sweep(space: &mut Space, workers: usize) -> usize {
    for list in space.partial.iter_mut().chain(space.old_partial.iter_mut()) {
        list.clear();
    }
    space.mixed_partial.clear();
    let used = ((space.fresh - space.base) >> PAGE_SHIFT).min(space.pages.len());
    let chunks = used.div_ceil(CHUNK);
    let results: Vec<Mutex<Swept>> = (0..chunks).map(|_| Mutex::default()).collect();
    let (pages, base, next) = (Pages(space.pages.as_mut_ptr()), space.base, AtomicUsize::new(0));
    let job = || loop {
        let c = next.fetch_add(1, Ordering::Relaxed);
        if c >= chunks {
            break;
        }
        let mut out = Swept::default();
        for index in c * CHUNK..((c + 1) * CHUNK).min(used) {
            // SAFETY: page `index` belongs to this chunk only.
            sweep_page(unsafe { pages.slot(index) }, index, base + (index << PAGE_SHIFT), &mut out);
        }
        *results[c].lock().unwrap() = out;
    };
    super::pool::run(workers.min(chunks).saturating_sub(1), &job);
    let mut live = 0;
    space.partial_free = 0;
    for r in results {
        let r = r.into_inner().unwrap();
        live += r.live;
        space.partial_free += r.partial_free;
        space.empty.extend(r.empty);
        r.partial.into_iter().for_each(|(class, index)| space.partial[class as usize].push(index));
        r.old_partial.into_iter().for_each(|(class, index)| space.old_partial[class as usize].push(index));
        space.mixed_partial.extend(r.mixed_partial);
    }
    let dead: Vec<usize> = space.large.iter().filter(|(_, l)| !l.marked).map(|(&a, _)| a).collect();
    for a in dead {
        let l = space.large.remove(&a).unwrap();
        space.large_bytes -= l.size;
        space.large_area.free(a, l.size);
    }
    space.large_area.trim(LARGE_FREE_KEEP);
    live + space.large_bytes
}

/// Sweeps the page `index` at `page`, noting what became of it in `out`.
fn sweep_page(slot: &mut Option<PageMeta>, index: usize, page: usize, out: &mut Swept) {
    let Some(meta) = slot.as_mut() else { return };
    let live_cells: usize = meta.marks.iter().map(|w| w.count_ones() as usize).sum();
    if live_cells == 0 {
        // Without metadata no reference resolves into the page; it is zeroed when reused.
        *slot = None;
        out.empty.push(page);
        return;
    }
    // The marks stay: survivors are old from now on.
    meta.alloc.copy_from_slice(&meta.marks);
    if meta.is_mixed() {
        meta.sparse = false;
        // Only the tail is free; the rest comes back when the whole page dies.
        out.live += super::mixed::live_bytes(meta, live_cells);
        if meta.cursor < meta.cells() || !meta.holes.is_empty() {
            out.mixed_partial.push(index);
        }
        return;
    }
    meta.cursor = 0;
    out.live += live_cells * meta.cell_size as usize;
    // Allocation reuses pages up to nine tenths full (holes too short for a run are passed
    // over, `MIN_RUN`). Reusing only pages at most half full left the free cells of fuller
    // ones idle while fresh pages were faulted in: 0.1 GiB more peak RSS at the same speed.
    if meta.old {
        if live_cells < meta.cells() {
            out.old_partial.push((meta.class, index));
        }
    } else if live_cells * 10 <= meta.cells() * 9 {
        out.partial.push((meta.class, index));
        out.partial_free += (meta.cells() - live_cells) * meta.cell_size as usize;
    }
}
