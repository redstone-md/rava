//! Allocation generations, the marks over them, and the small gates beside.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

pub fn handle_epoch() -> u64 {
    HANDLE_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// The 8 primitive element ids, or `[u32::MAX; 8]` if no heap has been built yet
/// (never matches a real id → every native load falls back to the callback:
/// slow but correct).
pub(crate) fn prim_element_ids() -> [u32; 8] {
    PRIM_ELEMENT_IDS.get().copied().unwrap_or([DEAD_ARRAY_ELEMENT_ID; 8])
}

/// Choose a packed backing for `len` default-initialised elements of `element`.
pub(crate) fn empty_array(element: &str, len: usize) -> ArrayData {
    match element {
        "byte" | "boolean" => ArrayData::Byte(vec![0; len]),
        "char" | "short" | "int" => ArrayData::Int(vec![0; len]),
        "long" => ArrayData::Long(vec![0; len]),
        "float" => ArrayData::Float(vec![0.0; len]),
        "double" => ArrayData::Double(vec![0.0; len]),
        _ => ArrayData::Ref(vec![0i64; len]), // 0 = null cell
    }
}

/// RAVA_ARRAY_TRIM=1: at each mark-sweep GC, shrink_to_fit the backing Vec of every LIVE
/// `ArrayData::Ref` whose slack capacity exceeds 2. Reclaims the persistent over-allocation in
/// collection `$elems` backing ([[rava-array-table-verdict]]: ~56MB of 53%-slack ref arrays).
/// Gated for an A/B measurement — the peak is a between-GC high-water, so whether GC-time trimming
/// lowers PEAK RSS (vs the spine compaction which measured 0) must be verified, not assumed.
pub(crate) fn array_trim_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARRAY_TRIM").is_some())
}

/// Release the per-slot `Box<Object>` allocation when a handle is swept. The
/// pointer-sized slot and handle index remain stable. Default-on after matched
/// RSS acceptance; `RAVA_NULL_OBJECT_SLOTS=0` restores legacy empty boxes.
pub(crate) fn null_object_slots_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NULL_OBJECT_SLOTS")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}

/// Bytes hashbrown actually allocates for a table with `cap` usable entries: it keeps the
/// load factor at 7/8 and rounds the bucket count to a power of two, storing one control
/// byte alongside each `(K, V)`. `HashMap::capacity()` reports the USABLE count, so the raw
/// allocation is meaningfully larger — which is exactly the overhead this census is trying
/// to expose for `MapObj.index`.
pub(crate) fn hashbrown_alloc_bytes<K, V>(cap: usize) -> usize {
    if cap == 0 {
        return 0;
    }
    let buckets = ((cap * 8 + 6) / 7).next_power_of_two();
    buckets * (std::mem::size_of::<(K, V)>() + 1) + ALLOC_TAX
}

/// GC-pass diagnostic gates, memoised. Both sit inside `Heap::gc`, which RIP sampling puts at
/// 5.5% of a serial boot block, and on Windows `std::env::var_os` is `RtlQueryEnvironmentVariable`
/// - a linear walk of the process environment block. Once per collection rather than per object,
/// so this is tidiness rather than a hot fix; memoised so the pattern is uniform and cannot be
/// copied into a per-object loop later.
pub(crate) fn retain_trace_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_RETAIN_TRACE").is_some())
}

pub(crate) fn gc_validate_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GC_VALIDATE").is_some())
}


/// Current generation (phase-start snapshot reads this).
pub(crate) fn current_alloc_gen() -> u64 {
    ALLOC_GEN.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn next_alloc_gen() -> u64 {
    ALLOC_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
/// Stamp object handle `h` with a fresh generation (overwrites any recycled entry).
pub(crate) fn mark_obj_gen(h: u32) {
    let g = next_alloc_gen();
    GEN_OBJ.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(h, g);
}
pub(crate) fn mark_arr_gen(h: u32) {
    let g = next_alloc_gen();
    GEN_ARR.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(h, g);
}
/// Generation of object handle `h` (0 = never stamped ⇒ predates any snapshot ⇒ treated as shared).
pub(crate) fn obj_gen(h: u32) -> u64 {
    GEN_OBJ.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&h).copied()).unwrap_or(0)
}
pub(crate) fn arr_gen(h: u32) -> u64 {
    GEN_ARR.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&h).copied()).unwrap_or(0)
}

/// ⭐**ONE SWITCH OVER THE WHOLE COLLECTOR-COST BLOCK** (`RAVA_NO_FAST_GC=1` disables;
/// it also folds into `RAVA_NO_EMIT_MORE` so the block bisects in a single run).
///
/// Everything it gates removes WORK from the collector without moving a single byte of
/// reachability: the grey-stack duplicate filter, the word-at-a-time sweep skip, the
/// box-table holder list that deletes one of the two full-heap walks per collection, the
/// hoisted arena high-water in the root scan, and the reuse of the mark buffers between
/// collections. Every one of them is a pure filter or a pure representation change — if
/// any of them is ever suspected, this flag restores the previous code path exactly.
pub(crate) fn fast_gc_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_GC").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**ONE SWITCH OVER THE OBJECT-TABLE ACCESS BLOCK** (`RAVA_NO_FAST_OBJTABLE=1` disables;
/// it also folds into `RAVA_NO_EMIT_MORE` so the block bisects in a single run).
///
/// It gates the handle→slot probes that were spelled as several independent table queries
/// (`objects.len()`, `obj_alive.len()`, then a panicking `[]` into each) where ONE `get`
/// answers "in range" and "which slot" together, and the mark bitmap's clear strategy.
/// Every one of them returns the same answer for every input — including a handle past the
/// end of either table and a handle whose slot outlives a shorter `obj_alive` — so this
/// flag exists to BISECT, not to choose semantics.
///
/// TABLE SIZE: one `bool` behind a `OnceLock`, i.e. nothing that competes for L1.
pub(crate) fn fast_objtable_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_OBJTABLE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// A dense mark bitmap over handle slots.
///
/// It replaces `vec![false; n]`, which the collector built and threw away THREE times per
/// collection (objects, arrays, maps). On a product boot the object table alone reaches
/// millions of slots, so that was several megabytes of malloc + memset + free on every
/// single collection, and every sweep read one whole BYTE per slot to test one bit.
///
/// TABLE SIZE: one bit per slot — `(slots + 63) / 64 * 8` bytes. For a 4M-slot object
/// table that is 500 KB against the 4 MB the `Vec<bool>` cost, i.e. the sweep's mark
/// traffic drops 8x. It is a mandatory working set, not a cache: it is streamed once by
/// the mark and once by the sweep, never randomly re-probed, so the L1 rule that killed
/// the 14 KB intern table does not apply — but the 8x shrink still buys the sweep eight
/// times as many slots per cache line.
#[derive(Default)]
pub(crate) struct MarkBits {
    words: Vec<u64>,
    n: usize,
}

impl MarkBits {
    /// Size for `n` slots and clear every bit. Keeps the allocation between collections
    /// (no malloc when the heap has not grown since the last collection).
    ///
    /// ⭐**ONE ZEROING PASS, NOT ONE AND A HALF.** `Vec::resize(words, 0)` *writes* the
    /// zeros it appends — it does not merely reserve — and the old body then ran `fill(0)`
    /// over the whole buffer, so every word ADDED this collection was memset twice. The
    /// object table only ever grows during a boot, so the growth branch was the common one:
    /// on a collection that took the bitmap from `old` to `new` words the writes were
    /// `(new - old) + new`; they are now exactly `new`. Grows with slots per table, so at a
    /// 4M-slot object table this is 500 KB written instead of up to 1 MB, three times per
    /// collection (objects, arrays, maps).
    #[inline]
    pub(crate) fn reset(&mut self, n: usize) {
        self.n = n;
        let words = (n + 63) >> 6;
        if !fast_objtable_enabled() {
            if self.words.len() < words {
                self.words.resize(words, 0);
            } else {
                self.words.truncate(words);
            }
            self.words.fill(0);
            return;
        }
        if self.words.len() < words {
            // Clear only what is RETAINED, then let `resize` zero the new tail once. The
            // words above `len` — whether never allocated or left behind by an earlier
            // `truncate` — are written by `resize` itself, so a stale bit cannot survive.
            self.words.fill(0);
            self.words.resize(words, 0);
        } else {
            self.words.truncate(words);
            self.words.fill(0);
        }
    }

    /// Marked? Out-of-range slots read as false, matching the `h >= n → continue` arms the
    /// mark loop used to spell out.
    ///
    /// ⭐**ONE BOUNDS TEST, NOT TWO — AND NO PANIC PATH IN THE MARK'S INNER LOOP.** It
    /// compared `i` against `n` and then indexed `words` with a panicking `[]`: a second,
    /// independent bounds check the optimiser cannot fold into the first (`n` and
    /// `words.len()` are separate fields it must assume unrelated) plus an unwind landing
    /// pad in the hottest loop of the collector. Testing the WORD index alone is exactly as
    /// strict: `reset` zeroes every word and `set` only ever writes `i < n`, so every slot
    /// in `[n, words.len() * 64)` reads zero under either spelling. Deliberately NOT gated
    /// by `fast_objtable_enabled` — the two forms are provably the same value, and a flag
    /// load inside a loop that runs once per reachable reference would cost more than the
    /// branch it removes.
    #[inline]
    pub(crate) fn get(&self, i: usize) -> bool {
        self.words.get(i >> 6).copied().is_some_and(|w| (w >> (i & 63)) & 1 != 0)
    }

    #[inline]
    pub(crate) fn set(&mut self, i: usize) {
        debug_assert!(i < self.n);
        self.words[i >> 6] |= 1u64 << (i & 63);
    }

    /// The 64 marks starting at slot `wi * 64`. `u64::MAX` means "all 64 live" — the sweep
    /// skips the whole run without touching the liveness vector at all.
    #[inline]
    pub(crate) fn word(&self, wi: usize) -> u64 {
        self.words[wi]
    }

    #[inline]
    pub(crate) fn word_count(&self) -> usize {
        self.words.len()
    }
}

/// Everything the collector needs that is proportional to heap size and is identical from
/// one collection to the next. Retained per thread so a collection allocates nothing that
/// scales with the live set; the buffers are cleared, never inherited, so a stale entry is
/// impossible by construction.
///
/// Per-thread rather than per-heap because `Heap` is edited by other agents' files; only a
/// thread that actually RUNS a collection ever materialises one, which under the GIL is the
/// main thread plus, rarely, a worker.
#[derive(Default)]
pub(crate) struct GcScratch {
    pub(crate) mo: MarkBits,
    pub(crate) ma: MarkBits,
    pub(crate) mm: MarkBits,
    /// The grey stack. Its high-water is the live set, and regrowing it from empty cost a
    /// doubling chain of memcpys on every collection.
    pub(crate) work: Vec<Mark>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) seen_arena: crate::FastSet<usize>,
}

thread_local! {
    static GC_SCRATCH: std::cell::RefCell<GcScratch> = std::cell::RefCell::new(GcScratch::default());
}

/// Take the retained collector buffers (leaving an empty set behind, so a re-entrant
/// collection would simply build its own rather than alias ours).
pub(crate) fn gc_scratch_take() -> GcScratch {
    GC_SCRATCH.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Ceiling on a RETAINED buffer, in entries. Reuse must not turn a transient GC-time
/// allocation into permanent resident memory: the mark bitmaps are inherently ~1/8 of the
/// slot count and stay, but `work` (16 B/entry) and `seen_arena` (~24 B/entry after
/// hashbrown's 7/8 load factor and power-of-two bucket rounding) scale with the arena live
/// set and would otherwise pin tens of megabytes between collections on a heap that is
/// already 1.2 GB. 1 Mi entries caps them at ~16 MB and ~24 MB respectively; past that the
/// buffer is dropped and the next collection re-grows it, which is what used to happen
/// every time anyway. It is now the OUTER bound only — `scratch_shrink_target` sizes the
/// buffers that can observe their own use against the last collection instead.
const SCRATCH_RETAIN_CAP: usize = 1 << 20;

/// Floor under the shrink policy below: never trim a retained buffer under this many
/// entries. 4096 entries is 64 KB for `Vec<Mark>`, 16 KB for `Vec<u32>` and ~72 KB for a
/// `FastSet<usize>` — invisible against a 1.2 GB RSS, and large enough that an ordinary
/// small collection never reallocates at all.
const SCRATCH_RETAIN_FLOOR: usize = 4096;

/// ⭐**RETAINED AT THE RECENT LIVE SET, NOT AT THE ALL-TIME PEAK.**
///
/// New capacity for a retained buffer that actually held `used` entries this collection, or
/// `None` to leave it exactly as it is.
///
/// WAS: capacity retained at the high-water of the WORST collection the process ever ran,
/// bounded only by `SCRATCH_RETAIN_CAP`. One datapack-reload collection that touches a
/// million arena objects or a million box cells therefore pinned tens of megabytes of
/// resident memory for the rest of the run — on a process whose 1.2 GB RSS against the JVM's
/// 550 MB is itself one of the two things being fixed. Growth was in "the biggest collection
/// ever", which is monotone and never comes back down.
///
/// IS: capacity proportional to what the LAST collection needed. The hysteresis is
/// deliberate and asymmetric — shrink only when the buffer is oversized by 4x, and then only
/// down to 2x — so a buffer genuinely used at its capacity every collection is never
/// touched, and a collection whose demand is steady can never be made to re-grow.
#[inline]
fn scratch_shrink_target(cap: usize, used: usize) -> Option<usize> {
    let slack = used.saturating_mul(4).max(SCRATCH_RETAIN_FLOOR);
    (cap > SCRATCH_RETAIN_CAP || cap > slack)
        .then(|| used.saturating_mul(2).max(SCRATCH_RETAIN_FLOOR))
}

/// Hand the buffers back for the next collection. Dropped on the floor if the collection
/// unwound — the next one just pays one allocation.
///
/// Emptying is normally the NEXT collection's job (`work.clear()` / `seen_arena.clear()` at
/// its entry) — clearing here too would memset the same buckets twice per collection. The one
/// exception is the shrink below: a buffer about to be REALLOCATED is cheaper to empty first,
/// because the reallocation then copies nothing and rehashes nothing.
pub(crate) fn gc_scratch_put(mut s: GcScratch) {
    if !fast_objtable_enabled() {
        if s.work.capacity() > SCRATCH_RETAIN_CAP {
            s.work = Vec::new();
        }
        #[cfg(feature = "next-gen-gc")]
        if s.seen_arena.capacity() > SCRATCH_RETAIN_CAP {
            s.seen_arena = crate::FastSet::default();
        }
        GC_SCRATCH.with(|c| *c.borrow_mut() = s);
        return;
    }
    // `work` is DRAINED to empty by the mark loop, so its length here is 0 and says nothing
    // about this collection's high-water. The hard cap is the only bound this function can
    // observe, so it stays exactly as it was — sizing it to the live set needs a high-water
    // counter on the push path, which lives in the collector, not here.
    if s.work.capacity() > SCRATCH_RETAIN_CAP {
        s.work = Vec::new();
    }
    // `seen_arena` IS readable: the next collection clears it at entry, so its length right
    // now is the arena live set this collection walked. Clear before shrinking — an empty
    // hashbrown table reallocates without rehashing a single key, and the clear the next
    // collection would have done is then a memset of a small table instead of a huge one.
    #[cfg(feature = "next-gen-gc")]
    if let Some(target) = scratch_shrink_target(s.seen_arena.capacity(), s.seen_arena.len()) {
        s.seen_arena.clear();
        s.seen_arena.shrink_to(target);
    }
    GC_SCRATCH.with(|c| *c.borrow_mut() = s);
}

/// Buffers for `compact_box_tables`, held apart from `GcScratch` because that pass runs
/// INSIDE a collection that already owns its scratch.
#[derive(Default)]
pub(crate) struct BoxCompactScratch {
    /// Old box index -> new box index, rebuilt every collection.
    pub(crate) remap: crate::FastMap<u32, u32>,
    /// Old box indices in first-visit order — the authority for the new numbering, so the
    /// resulting table is byte-identical to the one the two-pass version produced.
    pub(crate) order: Vec<u32>,
    /// The handles that actually HOLD a box cell. Recording them in the discovery pass is
    /// what lets the rewrite pass stop being a second full walk of the heap.
    pub(crate) holders: Vec<u32>,
    pub(crate) fresh: Vec<Value>,
}

thread_local! {
    static BOX_SCRATCH: std::cell::RefCell<BoxCompactScratch> =
        std::cell::RefCell::new(BoxCompactScratch::default());
}

pub(crate) fn box_scratch_take() -> BoxCompactScratch {
    BOX_SCRATCH.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

pub(crate) fn box_scratch_put(mut s: BoxCompactScratch) {
    if !fast_objtable_enabled() {
        // The value buffer holds the PREVIOUS box table after the swap; drop its contents now
        // (exactly where the old `self.field_box = fresh` dropped them). Capacity is kept only
        // up to `SCRATCH_RETAIN_CAP` — see there for why reuse must not become residency.
        if s.fresh.capacity() > SCRATCH_RETAIN_CAP {
            s.fresh = Vec::new();
        } else {
            s.fresh.clear();
        }
        if s.remap.capacity() > SCRATCH_RETAIN_CAP {
            s.remap = crate::FastMap::default();
            s.order = Vec::new();
        } else {
            s.remap.clear();
            s.order.clear();
        }
        if s.holders.capacity() > SCRATCH_RETAIN_CAP {
            s.holders = Vec::new();
        } else {
            s.holders.clear();
        }
        BOX_SCRATCH.with(|c| *c.borrow_mut() = s);
        return;
    }
    // Every one of these four is drained HERE, not at the next collection's entry, so this is
    // the last moment at which what the collection needed is observable — read the lengths
    // before the clears erase the evidence, then size against them (`scratch_shrink_target`).
    // All four scale with the number of LIVE box cells, which falls after the reload's peak
    // and never brought the capacity down with it.
    let (fresh_used, remap_used) = (s.fresh.len(), s.remap.len());
    let (order_used, holders_used) = (s.order.len(), s.holders.len());
    // The value buffer holds the PREVIOUS box table after the swap; dropping its contents
    // here is exactly where the old `self.field_box = fresh` dropped them, and must stay
    // unconditional — a `Value` can own an `Arc`.
    s.fresh.clear();
    if let Some(target) = scratch_shrink_target(s.fresh.capacity(), fresh_used) {
        s.fresh.shrink_to(target);
    }
    // Clear first, shrink second: shrinking an EMPTY hashbrown table allocates the smaller
    // one without rehashing a key.
    s.remap.clear();
    if let Some(target) = scratch_shrink_target(s.remap.capacity(), remap_used) {
        s.remap.shrink_to(target);
    }
    s.order.clear();
    if let Some(target) = scratch_shrink_target(s.order.capacity(), order_used) {
        s.order.shrink_to(target);
    }
    s.holders.clear();
    if let Some(target) = scratch_shrink_target(s.holders.capacity(), holders_used) {
        s.holders.shrink_to(target);
    }
    BOX_SCRATCH.with(|c| *c.borrow_mut() = s);
}
