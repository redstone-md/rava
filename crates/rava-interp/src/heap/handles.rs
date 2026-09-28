//! Handle bookkeeping: epochs, watches and the census over free slots.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

/// RAVA_WATCH_HANDLE (GC-cycle-1 corruption hunt): the object handle whose class-len field
/// address to hand to `gc_watch_hook` when it is allocated. u32::MAX = unset.
///
/// `put_object` compares against this on EVERY allocation, and `put_object` lives in another
/// codegen unit (`release` is thin-LTO with the default 16), so without `#[inline]` the
/// already-initialised `OnceLock` read — one acquire load and a predicted branch — is a call
/// per object born. Same value, one instruction sequence instead of a call frame.
///
/// ⭐**ONE LOAD PER OBJECT BORN, NOT TWO.** `OnceLock<u32>` answers two questions with two
/// DEPENDENT loads: an acquire load of the `Once` state ("is it initialised?"), then a load
/// of the payload cell ("what is it?") — and the second cannot issue until the first
/// retires. On the object-birth path that is a serialised pair on a line nothing else on
/// that path touches, per object. Packing both answers into ONE `u64` — `0` while
/// uninitialised, `PRESENT | value` afterwards — makes it a single relaxed load, one
/// not-taken branch on the cold `== 0` arm, and the compare the caller was going to do
/// anyway. Relaxed is sufficient because the payload is a plain scalar: there is no
/// separately-published memory for an acquire to order against, unlike `OnceLock`, whose
/// acquire exists to publish the `MaybeUninit` cell it guards.
///
/// Bit-identical: the value is still `RAVA_WATCH_HANDLE` parsed as a `u32`, `u32::MAX` when
/// unset or unparseable, and `PRESENT | u32::MAX` truncates back to `u32::MAX`. Two threads
/// racing the cold arm both read the same env var and store the same word, so the double
/// init is idempotent — the property `OnceLock`'s exclusion was buying, and the reason it is
/// not needed here.
///
/// Cost if reverted: one extra dependent load and one extra branch on every handle birth.
#[inline]
pub(crate) fn watch_handle() -> u32 {
    /// High bit above the `u32` payload: distinguishes "initialised to `u32::MAX`" (the
    /// overwhelmingly common unset case) from "not yet read", which a bare `u32` cannot.
    const PRESENT: u64 = 1 << 32;
    static H: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let v = H.load(std::sync::atomic::Ordering::Relaxed);
    if v != 0 {
        return v as u32;
    }
    watch_handle_init(&H, PRESENT)
}

/// The env read, kept OUT of `watch_handle`'s inlined body: it allocates a `String`, parses,
/// and runs exactly once per process, so leaving it inline would push a cold `env::var` call
/// (and its spills) into every `put_object` that inlines the probe.
#[cold]
#[inline(never)]
fn watch_handle_init(cell: &std::sync::atomic::AtomicU64, present: u64) -> u32 {
    let h = std::env::var("RAVA_WATCH_HANDLE").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(u32::MAX);
    cell.store(present | h as u64, std::sync::atomic::Ordering::Relaxed);
    h
}
/// One relaxed increment per RETIRED slot — the invalidation signal for every handle-keyed
/// cache (see `handle_epoch`).
///
/// REFUSED, and the arithmetic is why. This is a `lock xadd` per swept object, array and map
/// slot, so a collection that retires 500k slots pays ~500k × ~20 cycles ≈ 3 ms, and a boot
/// runs on the order of tens of collections ⇒ **~0.2% of a 32 s boot**, which is far under
/// the measurement floor this box has shown. Coalescing it (bump only on the FIRST retire
/// after a read, a `swap` on a companion `DIRTY` flag) is not free either: the reader
/// `handle_epoch` would have to WRITE to a globally shared line on a path that runs per
/// `pure_memo` window, and the safe interleaving argument needs the retire burst and the read
/// to be mutually exclusive — true today only because collection is stop-the-world under the
/// GIL, which is not a property this function can check. Cost priced, cure priced, not worth
/// the exposure: left exactly as it was.
#[inline]
pub(crate) fn handle_epoch_bump() {
    HANDLE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod free_handle_census_test {
    use super::*;

    #[test]
    fn census_accounts_retained_free_handle_capacity() {
        let mut heap = Heap::default();
        *heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()) = Vec::with_capacity(11);
        *heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()) = Vec::with_capacity(7);
        heap.free_map = Vec::with_capacity(5);
        heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).extend([1, 2, 3]);
        heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()).extend([4, 5]);
        heap.free_map.push(6);

        let expected_handles = [
            (heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).len(), heap.free_obj.lock().unwrap_or_else(|e| e.into_inner()).capacity()),
            (heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()).len(), heap.free_arr.lock().unwrap_or_else(|e| e.into_inner()).capacity()),
            (heap.free_map.len(), heap.free_map.capacity()),
        ];
        let expected_bytes = expected_handles
            .iter()
            .map(|(_, cap)| cap * std::mem::size_of::<u32>())
            .sum();
        let census = heap.census();

        assert_eq!(census.free_handles, expected_handles);
        assert_eq!(census.free_handle_bytes, expected_bytes);
        assert!(census.total >= expected_bytes);
    }
}
