//! The card table of the generational write barrier: one byte per 512 bytes of the heap
//! reservation, set when a reference is stored into an object there. A young collection
//! scans only the old objects on dirty cards for references into the young generation.
//!
//! Compiled code marks a card with `shr 9; add [thread.cards]; mov byte [..], 1` (the thread
//! holds the table biased by the heap base); the runtime uses `dirty` and `dirty_range`, which
//! ignore addresses outside the heap (Unsafe accesses to off-heap statics).

use std::sync::OnceLock;

pub const CARD_SHIFT: u32 = 9;
pub const CARD_SIZE: usize = 1 << CARD_SHIFT;

struct Table {
    base: usize,
    span: usize,
    bytes: *mut u8,
}

// The table is shared memory written with plain byte stores, like HotSpot's card table.
unsafe impl Sync for Table {}
unsafe impl Send for Table {}

static TABLE: OnceLock<Table> = OnceLock::new();

/// Creates the table for the heap reservation `[base, base + span)`.
pub(super) fn init(base: usize, span: usize) {
    TABLE.get_or_init(|| {
        let len = span >> CARD_SHIFT;
        let p = crate::os::reserve(len, None, false).expect("cannot reserve the card table");
        // Huge pages, as the heap: barriers dirty cards all over the table.
        crate::os::huge_pages(p, len);
        Table { base, span, bytes: p as *mut u8 }
    });
}

fn table() -> &'static Table {
    TABLE.get().expect("heap created")
}

/// What compiled code needs: the table biased so that `(addr >> CARD_SHIFT) + biased` is the
/// card of `addr`, the heap base, its span, and the table itself.
pub fn layout() -> (usize, usize, usize, usize) {
    TABLE.get().map_or((0, 0, 0, 0), |t| ((t.bytes as usize).wrapping_sub(t.base >> CARD_SHIFT), t.base, t.span, t.bytes as usize))
}

/// Marks the card of `addr` when it lies in the heap.
#[inline]
/// Whether `addr` lies in the heap's reservation (else, for Unsafe, a static slot or native
/// memory).
pub fn in_heap(addr: usize) -> bool {
    TABLE.get().is_some_and(|t| addr.wrapping_sub(t.base) < t.span)
}

pub fn dirty(addr: usize) {
    let Some(t) = TABLE.get() else { return };
    let off = addr.wrapping_sub(t.base);
    if off < t.span {
        // SAFETY: inside the table.
        unsafe { *t.bytes.add(off >> CARD_SHIFT) = 1 };
    }
}

/// Marks the cards of `[addr, addr + len)` that lie in the heap.
pub fn dirty_range(addr: usize, len: usize) {
    if len == 0 {
        return;
    }
    let Some(t) = TABLE.get() else { return };
    let (lo, hi) = (addr.max(t.base), (addr + len).min(t.base + t.span));
    if lo >= hi {
        return;
    }
    let (first, last) = ((lo - t.base) >> CARD_SHIFT, (hi - 1 - t.base) >> CARD_SHIFT);
    // SAFETY: inside the table.
    unsafe {
        // A few references (most copies) lie on one card: no memset call for it.
        if first == last {
            *t.bytes.add(first) = 1;
        } else {
            std::ptr::write_bytes(t.bytes.add(first), 1, last - first + 1);
        }
    }
}

/// Calls `f` with the start address of each dirty card in `[from, to)`, then cleans them.
pub(super) fn drain(from: usize, to: usize, f: &mut dyn FnMut(usize)) {
    let t = table();
    let (first, last) = ((from - t.base) >> CARD_SHIFT, (to - t.base + CARD_SIZE - 1) >> CARD_SHIFT);
    // SAFETY: the range is inside the table, and the world is stopped.
    let cards = unsafe { std::slice::from_raw_parts_mut(t.bytes.add(first), last - first) };
    let mut i = 0;
    while i < cards.len() {
        // Skip clean cards eight at a time.
        if i + 8 <= cards.len() && u64::from_ne_bytes(cards[i..i + 8].try_into().unwrap()) == 0 {
            i += 8;
            continue;
        }
        if cards[i] != 0 {
            cards[i] = 0;
            f(t.base + ((first + i) << CARD_SHIFT));
        }
        i += 1;
    }
}

/// Cleans every card of `[from, to)`.
pub(super) fn clean(from: usize, to: usize) {
    drain(from, to, &mut |_| {});
}
