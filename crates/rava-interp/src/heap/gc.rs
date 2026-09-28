//! Collection: the entry points and the table trimming that follows one.

use crate::heap::*;

/// ⭐**ONE SWITCH OVER THE COLLECTOR-PAUSE BLOCK** (`RAVA_NO_FAST_GCPAUSE2=1` disables; it
/// also folds into `RAVA_NO_EMIT_MORE` so the whole block bisects in a single run).
///
/// Everything it gates removes WORK or RESIDENT BYTES from a collection without touching
/// reachability, identity or the ORDER in which slots are retired:
///   * the sweep steps over a 64-slot run whose *liveness* word says there is nothing to
///     retire there, instead of reading 64 `Vec<bool>` bytes it can do nothing with;
///   * the table-capacity trim declines to reallocate a table the mutator is still filling;
///   * the array/map free-list epoch is bumped once per collection rather than once per
///     recycled slot.
/// Each is a pure filter or a pure schedule change — if any is ever suspected, this flag
/// restores the previous code path exactly.
///
/// TABLE SIZE: one `bool` behind a `OnceLock`, i.e. nothing that competes for L1.
pub(crate) fn fast_gcpause2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_GCPAUSE2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The tables `trim_table_capacities` offers to the shrinker, in visit order. The array is
/// positional on purpose: the growth record below has to line up with the calls one-for-one,
/// and a named struct per table would be seven fields that only ever move together.
const TRIM_TABLES: usize = 7;

/// `Vec::len` of each trimmable table as of the previous trim that actually ran.
#[derive(Clone, Copy)]
struct TrimMarks {
    /// Address of the `Heap` the lengths were read from; 0 = no record. The record is
    /// side-table state rather than a `Heap` field because `Heap` is defined in a file this
    /// change does not own, so it has to say WHICH heap it describes: a process can hold
    /// several (tests, a discarded worker heap), and a record from the wrong one would
    /// silently answer the growth question about a different table. A heap that has moved,
    /// or one that reuses a freed heap's address, degrades to "no record" or one stale
    /// verdict — i.e. to one avoidable or one skipped realloc, never to a wrong length,
    /// handle, liveness bit or free-list entry.
    owner: usize,
    len: [usize; TRIM_TABLES],
}

thread_local! {
    /// Thread-local for the same reason `GcScratch` is: only a thread that actually RUNS a
    /// collection ever materialises one, and under the GIL that is the main thread plus,
    /// rarely, a worker.
    static TRIM_MARKS: std::cell::Cell<TrimMarks> =
        const { std::cell::Cell::new(TrimMarks { owner: 0, len: [0; TRIM_TABLES] }) };
}

impl Heap {
    /// Mark-sweep collection. `roots` are every reachable `Value` outside the
    /// heap (class statics + interpreter frame locals/stacks). Marks everything
    /// transitively reachable, frees the rest into the slot free lists (handles
    /// stay stable — no compaction), and returns the bytes reclaimed.
    ///
    /// Maps are reached only through an owning object's `$map` field; that's the
    /// one place a bare `Value::Int` denotes a map id rather than a number.
    pub fn gc(&mut self, roots: &[Value]) -> usize {
        self.gc_with_object_slot_policy(roots, null_object_slots_enabled())
    }

    /// Release unused outer-table capacity without changing any table length,
    /// handle, liveness bit or free-list entry. Every raw base is republished
    /// after the potential reallocations. Missing exclusivity proof is a no-op.
    ///
    /// ⭐**DO NOT REALLOCATE A TABLE THE MUTATOR IS STILL FILLING.** This pass ran
    /// `shrink_to_fit` on all seven tables on EVERY collection, and `shrink_to_fit` fits
    /// EXACTLY: a table that is growing gets reallocated down to `len`, and the mutator's
    /// next `push` immediately re-doubles it. During a boot — where the object, descriptor
    /// and array tables only ever grow — that is two full-table `memcpy`s per collection
    /// (one down, one back up) plus the amortised growth guarantee thrown away, and it does
    /// not even lower the peak it is meant to lower: a shrinking `realloc` that has to move
    /// holds the old buffer and the new one at the same time, so the interval's high-water
    /// is `cap + len`, ABOVE the `cap` that skipping would have kept. The valley between two
    /// collections gets smaller; the peak, which is what RSS is measured at, gets bigger.
    ///
    /// So each table is offered to the shrinker only when the release has a chance of
    /// sticking: either the slack is wider than the content (headroom `Vec` doubling cannot
    /// produce — an explicit `reserve`, or a table whose length came back down, such as every
    /// free list) or the table did not grow since the previous trim of this heap. A table
    /// that has stopped growing — post-boot steady state — is trimmed exactly as before, on
    /// the very first collection after it settles, so the end-of-growth reclaim that made
    /// this pass worth having is kept in full.
    ///
    /// This is a policy on WHEN to return capacity to the allocator and nothing else;
    /// `heap_table_trim::shrink` still owns the structural byte floor and the mechanics, and
    /// a wrong verdict in either direction costs only some resident slack or one avoidable
    /// realloc — never a length, a handle, a liveness bit or a free-list entry.
    pub(crate) fn trim_table_capacities(&mut self, requested: bool) -> usize {
        if !requested
            || crate::gil::is_worker()
            || crate::parallel::region_count() != 0
            || self.parallel_array_active()
        {
            return 0;
        }

        // Lengths are read BEFORE any shrink: `shrink_to_fit` moves capacity, never length,
        // so this snapshot is equally valid as the "previous" record for the next call.
        let owner = &*self as *const Heap as usize;
        let now: [usize; TRIM_TABLES] = [
            self.objects.read().unwrap_or_else(|e| e.into_inner()).len(),
            self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).len(),
            self.arrays.read().unwrap_or_else(|e| e.into_inner()).len(),
            self.array_descs.read().unwrap_or_else(|e| e.into_inner()).len(),
            self.free_obj.lock().unwrap_or_else(|e| e.into_inner()).len(),
            self.free_arr.lock().unwrap_or_else(|e| e.into_inner()).len(),
            self.free_map.len(),
        ];
        let gate = fast_gcpause2_enabled();
        let prev = TRIM_MARKS.with(|c| c.get());
        let known = prev.owner == owner;
        // Offer table `i` (whose current capacity is `cap`) to the shrinker?
        //
        //   * RESERVED HEADROOM — `cap - len > len` — is always offered. `Vec` doubling
        //     cannot produce it: a growth leaves `cap == 2 * old_cap` with `len` already
        //     past `old_cap`, so the slack starts just under `len` and only tightens as the
        //     table fills (the exception is a table of a few elements, whose whole capacity
        //     is far below the shrinker's 1 MB floor). Slack wider
        //     than the content therefore came from an explicit `reserve` (a released
        //     parallel-worldgen slot range) or from a table whose length came back down (the
        //     free lists, which shrink on every `pop` and keep their high-water capacity).
        //     Nobody is going to fill it, so releasing it sticks.
        //   * DOUBLING HEADROOM is offered only once the mutator has stopped adding to the
        //     table. While it is still growing, the shrink is undone by the very next `push`
        //     and costs two full-table copies plus a `cap + len` allocator high-water for it.
        //   * No record for THIS heap counts as settled, which is the unconditional
        //     pre-existing behaviour, so a cold process still trims on its first collection.
        let admit = |i: usize, cap: usize| -> bool {
            let len = now[i];
            !gate || cap - len > len || !known || len <= prev.len[i]
        };

        let mut released = 0usize;
        if admit(0, self.objects.read().unwrap_or_else(|e| e.into_inner()).capacity()) {
            released += crate::heap_table_trim::shrink(&mut *self.objects.write().unwrap_or_else(|e| e.into_inner()));
        }
        if admit(1, self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).capacity()) {
            released += crate::heap_table_trim::shrink(&mut *self.obj_descs.write().unwrap_or_else(|e| e.into_inner()));
        }
        if admit(2, self.arrays.read().unwrap_or_else(|e| e.into_inner()).capacity()) {
            released += crate::heap_table_trim::shrink(&mut *self.arrays.write().unwrap_or_else(|e| e.into_inner()));
        }
        if admit(3, self.array_descs.read().unwrap_or_else(|e| e.into_inner()).capacity()) {
            released += crate::heap_table_trim::shrink(&mut *self.array_descs.write().unwrap_or_else(|e| e.into_inner()));
        }
        if admit(4, self.free_obj.lock().unwrap_or_else(|e| e.into_inner()).capacity()) {
            released += crate::heap_table_trim::shrink(&mut *self.free_obj.lock().unwrap_or_else(|e| e.into_inner()));
        }
        if admit(5, self.free_arr.lock().unwrap_or_else(|e| e.into_inner()).capacity()) {
            released += crate::heap_table_trim::shrink(&mut *self.free_arr.lock().unwrap_or_else(|e| e.into_inner()));
        }
        if admit(6, self.free_map.capacity()) {
            released += crate::heap_table_trim::shrink(&mut self.free_map);
        }
        TRIM_MARKS.with(|c| c.set(TrimMarks { owner, len: now }));

        // Republished unconditionally: a table that was skipped did not move, so this is the
        // same value it already held, and the three writes are cheaper than proving which of
        // the seven shrinks fired.
        self.jit_ctx.objects_base = self.objects.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize;
        self.jit_ctx.obj_desc_base = self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize;
        self.jit_ctx.desc_base = self.array_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize;
        released
    }
}
