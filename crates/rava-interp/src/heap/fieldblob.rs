//! The packed field storage each object carries.

use crate::heap::*;

impl FieldBlob {
    #[inline]
    pub(crate) fn sentinel() -> std::ptr::NonNull<u8> {
        std::ptr::NonNull::from(&EMPTY_HDR).cast()
    }
    #[inline]
    pub fn new() -> Self {
        FieldBlob(Self::sentinel())
    }
    #[inline]
    pub fn len(&self) -> usize {
        unsafe { *(self.0.as_ptr() as *const u32) as usize }
    }
    #[inline]
    pub(crate) fn cap(&self) -> usize {
        unsafe { *(self.0.as_ptr().add(4) as *const u32) as usize }
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The shared sentinel's `(ids, cells)` pointers — for an empty/dead `ObjDesc`. Both point
    /// at sentinel+8, so `[ids-8]` reads the sentinel's count (0). Never dereferenced for a
    /// value (count 0 ⇒ no slot passes the bounds check).
    #[inline]
    pub fn sentinel_ptrs() -> (*const u32, *const i64) {
        let s = Self::sentinel().as_ptr();
        unsafe { (s.add(Self::ids_off(0)) as *const u32, s.add(Self::cells_off()) as *const i64) }
    }
    /// Census: the real backing allocation in bytes (0 for the shared empty sentinel).
    #[inline]
    pub fn blob_alloc_bytes(&self) -> usize {
        let cap = self.cap();
        if cap == 0 {
            0
        } else {
            Self::alloc_size(cap)
        }
    }
    // CELLS-FIRST layout: `[count:u32 @0][cap:u32 @4][cells:i64*cap @8][ids:u32*cap @(8+8·cap)]
    // [kinds:u8*cap @(8+12·cap)]`. The `i64` cells section is at a FIXED offset 8 (always
    // 8-aligned regardless of cap), so `cap` no longer has to be rounded up to an even number —
    // it grows to the EXACT field count, eliminating the power-of-two rounding waste (~40MB).
    #[inline]
    pub(crate) const fn cells_off() -> usize {
        8
    }
    #[inline]
    pub(crate) const fn ids_off(cap: usize) -> usize {
        8 + 8 * cap
    }
    #[inline]
    pub(crate) const fn kinds_off(cap: usize) -> usize {
        8 + 12 * cap
    }
    /// Bytes handed to `std::alloc::alloc` / `dealloc` for a blob of `cap` slots — NOT the
    /// occupied extent, which is `kinds_off(cap) + cap` = `8 + 13*cap` and is what the section
    /// offsets above are derived from. Nothing here moves a section; only the `Layout::size`
    /// the request and the matching release carry.
    ///
    /// ⭐**THE TOP HOST CALLBACK OUT OF COMPILED CODE IS `mi_malloc_aligned`, AND THIS IS THE
    /// ONE PER-OBJECT ALLOCATION IN THE ENGINE WHOSE SIZE IS NOT A MULTIPLE OF ITS OWN
    /// ALIGNMENT.** The `mimalloc` crate's `GlobalAlloc::alloc` is `mi_malloc_aligned(size,
    /// align)` for EVERY Rust allocation, and in a no-padding (release) build that entry point
    /// dispatches on exactly one predicate:
    ///
    /// ```text
    ///   if (is_power_of_two(alignment) && alignment <= MI_MAX_ALIGN_SIZE
    ///       && (size & (alignment - 1)) == 0)   ->  mi_heap_malloc_small(heap, size)   // fast
    ///   else                                    ->  mi_heap_malloc_aligned_at(...)     // slow
    /// ```
    ///
    /// A blob is `8 + 13*cap` bytes at align 8, and **13 is odd**, so `size & 7 == 0` holds only
    /// when `cap` is a multiple of 8 — i.e. **7 field counts out of every 8 take the aligned
    /// SLOW path**: an extra out-of-line call, the power-of-two and size re-checks, the
    /// small-block attempt and the "did it come back aligned?" re-test, before returning the
    /// very block `mi_heap_malloc_small` would have returned directly. Every other per-object
    /// allocation in the engine already satisfies the predicate by accident — `Box<Object>` is
    /// 32 bytes at align 8, `Vec<i64>`/`Vec<Value>`/`Vec<ObjectSlot>`/`Vec<ObjDesc>` are all
    /// multiples of 8, and the `Vec<u8>`/`Box<str>` family is align 1, for which the mask is 0
    /// and the test is vacuously true. The field blob is the only one that misses, and it
    /// misses on **~5.9 M handle births a boot** ([[allocation-traffic]]: 5,867,627 handle
    /// births, two mallocs each) plus every growth realloc on top.
    ///
    /// **AND IT COSTS ZERO BYTES, WHICH IS THE ONLY REASON IT IS ALLOWED TO EXIST.** RSS is
    /// 1.02 GB against the JVM's 550 MB, so a rounding that actually consumed memory would be
    /// paying for speed in the currency this engine is shortest of. It does not: mimalloc's
    /// small bins below `MI_SMALL_SIZE_MAX` are one machine word apart, so a 47-byte request
    /// already returns a 48-byte block. Asking for the 48 we were being given anyway changes
    /// the size class not at all; it only stops lying to the fast-path predicate about it.
    /// Above the small-bin range the bins are coarser still, so the rounded request is never
    /// in a larger class than the exact one — the block is byte-identical at every `cap`.
    ///
    /// **CONSISTENCY IS THE SAFETY ARGUMENT.** `Layout` requires the release to name the same
    /// size as the request. All four sites — `realloc_to`'s alloc, `realloc_to`'s dealloc of
    /// the old block, `FieldBlob::drop` (heap/fieldkinds.rs) and the `blob_alloc_bytes` census
    /// — go through THIS function, and the switch below is a process constant read once, so a
    /// block allocated under one answer can never be freed under the other. The CLI's counting
    /// allocator meters `Layout::size` on both sides, so its running total still balances
    /// exactly; it merely becomes ≤7 bytes/blob more truthful about what mimalloc reserved.
    ///
    /// `realloc_to`'s free-slack step reads this too, and is provably unaffected: it computes
    /// `round16(alloc_size(cap))`, and `round16(round8(x)) == round16(x)` for every `x`, so the
    /// derived `fits` — hence every stored `cap` — is bit-identical to before.
    ///
    /// Kill switch `RAVA_NO_SLAB=1` (or the blanket `RAVA_NO_EMIT_MORE`) restores the exact
    /// `8 + 13*cap` request, and with it the aligned slow path.
    #[inline]
    pub(crate) fn alloc_size(cap: usize) -> usize {
        // The occupied extent: header + cells + ids + kinds. Unchanged, and still the value
        // every section offset is derived from.
        let occupied = 8 + 13 * cap;
        if !crate::heap::objects::slab_enabled() {
            return occupied;
        }
        // Round to the blob's own alignment. `cap == 0` never reaches an allocator (the shared
        // read-only sentinel owns no block and both `drop` and `blob_alloc_bytes` gate on
        // `cap > 0`), so the `occupied == 8` case here is inert either way.
        (occupied + 7) & !7usize
    }
    #[inline]
    pub fn ids_ptr(&self) -> *const u32 {
        unsafe { self.0.as_ptr().add(Self::ids_off(self.cap())) as *const u32 }
    }
    #[inline]
    pub fn cells_ptr(&self) -> *const i64 {
        unsafe { self.0.as_ptr().add(Self::cells_off()) as *const i64 }
    }
    #[inline]
    pub fn kinds_ptr(&self) -> *const u8 {
        unsafe { self.0.as_ptr().add(Self::kinds_off(self.cap())) }
    }
    #[inline]
    pub fn ids(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.ids_ptr(), self.len()) }
    }
    #[inline]
    pub fn cells(&self) -> &[i64] {
        unsafe { std::slice::from_raw_parts(self.cells_ptr(), self.len()) }
    }
    #[inline]
    pub fn kinds(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.kinds_ptr(), self.len()) }
    }
    #[inline]
    pub fn cells_mut(&mut self) -> &mut [i64] {
        let n = self.len();
        unsafe { std::slice::from_raw_parts_mut(self.cells_ptr() as *mut i64, n) }
    }
    #[inline]
    pub fn kinds_mut(&mut self) -> &mut [u8] {
        let n = self.len();
        unsafe { std::slice::from_raw_parts_mut(self.kinds_ptr() as *mut u8, n) }
    }
    /// Raw `*mut i64` to cell `i` — for the GC's in-place forwarding writeback, which
    /// holds these across the relocation pass (valid: no push happens between collect
    /// and writeback, and the block is a stable single allocation).
    #[inline]
    pub fn cell_mut_ptr(&mut self, i: usize) -> *mut i64 {
        debug_assert!(i < self.len());
        unsafe { (self.cells_ptr() as *mut i64).add(i) }
    }
    #[inline]
    pub(crate) fn set_count(&mut self, n: u32) {
        // Guard: never write through the read-only sentinel (cap==0 ⇒ count already 0).
        if self.cap() > 0 {
            unsafe { *(self.0.as_ptr() as *mut u32) = n }
        } else {
            debug_assert_eq!(n, 0);
        }
    }
    /// Ensure `cap >= need`. CELLS-FIRST: the cells section sits at a fixed 8-aligned offset, so
    /// `cap` needs no even/power-of-two rounding — grow to the EXACT `need`, eliminating the
    /// rounding waste. (Callers that append incrementally still get amortized behaviour via the
    /// single up-front `reserve` on the prepopulate/extend paths; a bare per-field `push` reallocs
    /// by one, which is fine for the small field counts that dominate.)
    pub fn reserve(&mut self, need: usize) {
        if need <= self.cap() {
            return;
        }
        self.realloc_to(need);
    }
    pub(crate) fn realloc_to(&mut self, new_cap: usize) {
        debug_assert!(new_cap > 0);
        let count = self.len();
        let old_cap = self.cap();
        // ⭐**TAKE THE SLACK THE ALLOCATOR IS ALREADY GOING TO GIVE US.** A blob is
        // `8 + 13 * cap` bytes, and 13 divides nothing — so an exact request nearly always lands
        // mid-way through a malloc size class and the tail is paid for and wasted. Rounding the
        // request up to the next 16 bytes and recomputing how many slots FIT costs no extra
        // memory (the allocator would have reserved that block either way) and buys 1-2 free
        // slots on almost every blob.
        //
        // That matters here specifically because `prepopulate_layout` reserves EXACTLY the
        // class's declared field count, so the first synthetic write (`$size`, `$init`,
        // `$elems`, `$pos`, `$sb`) always lands on the "full" boundary and reallocates. This
        // absorbs it without over-reserving on objects that never get one — and this engine's
        // RSS is 774 MB against the JVM's 351, so free slack is the only kind worth taking.
        let new_cap = {
            let rounded = (Self::alloc_size(new_cap) + 15) & !15usize;
            let fits = rounded.saturating_sub(8) / 13;
            fits.max(new_cap)
        };
        let layout = std::alloc::Layout::from_size_align(Self::alloc_size(new_cap), 8).unwrap();
        let np = std::ptr::NonNull::new(unsafe { std::alloc::alloc(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        unsafe {
            let nb = np.as_ptr();
            *(nb as *mut u32) = count as u32;
            *(nb.add(4) as *mut u32) = new_cap as u32;
            if count > 0 {
                std::ptr::copy_nonoverlapping(
                    self.cells_ptr(),
                    nb.add(Self::cells_off()) as *mut i64,
                    count,
                );
                std::ptr::copy_nonoverlapping(
                    self.ids_ptr(),
                    nb.add(Self::ids_off(new_cap)) as *mut u32,
                    count,
                );
                std::ptr::copy_nonoverlapping(
                    self.kinds_ptr(),
                    nb.add(Self::kinds_off(new_cap)),
                    count,
                );
            }
        }
        if old_cap > 0 {
            let old = std::alloc::Layout::from_size_align(Self::alloc_size(old_cap), 8).unwrap();
            unsafe { std::alloc::dealloc(self.0.as_ptr(), old) }
        }
        self.0 = np;
    }
    /// Append a field slot (realloc if full). Mirrors three parallel `SmallVec::push`.
    pub fn push(&mut self, id: u32, cell: i64, kind: u8) {
        let count = self.len();
        if count == self.cap() {
            // ⭐**GROW GEOMETRICALLY.** This was `reserve(count + 1)`, and `reserve` allocates
            // EXACTLY what is asked for — so a blob grew one slot at a time: an object that
            // gains N fields did N allocations and N copies, O(N²) bytes moved.
            //
            // It shows up as `Heap::put_field_syn` sitting at **9.2% of ALL allocations**, top of
            // the ALLOC-LEAF table for `Bootstrap.bootStrap`, because the SYNTHETIC fields
            // (`$size`, `$init`, `$elems`, `$pos`, `$sb`) are written after `prepopulate_known`
            // has already sized the blob to the class's declared fields — so every synthetic
            // write lands exactly on the "full" boundary and reallocates.
            //
            // 1.5x, not 2x: these blobs are per-object and this engine's RSS is already 774 MB
            // against the JVM's 351, so the growth factor is chosen to stop the quadratic without
            // doubling the slack on every object in the heap.
            let cap = self.cap();
            let grow = (cap + cap / 2).max(cap + 2).max(count + 1);
            self.realloc_to(grow);
        }
        let cap = self.cap();
        unsafe {
            let base = self.0.as_ptr();
            *(base.add(Self::cells_off()) as *mut i64).add(count) = cell;
            *(base.add(Self::ids_off(cap)) as *mut u32).add(count) = id;
            *base.add(Self::kinds_off(cap)).add(count) = kind;
        }
        self.set_count((count + 1) as u32);
    }
    /// Reset to empty, KEEPING the allocation (capacity reuse — the freed-slot path).
    #[inline]
    pub fn clear(&mut self) {
        self.set_count(0);
    }
    /// Append every field of `other` (used by the slot-reuse path).
    ///
    /// The copy was a loop of SLICE CONSTRUCTIONS, not a copy. Each iteration rebuilt three
    /// `&[_]` from `other` -- and `ids()`/`kinds()` re-derive their section offset from `cap`, so
    /// every element re-read the header, recomputed `8 + 8*cap` / `8 + 12*cap` and paid three
    /// bounds checks -- then `push` re-read `self`'s header twice more.
    /// WAS O(fields-per-object) iterations x (6 header loads + 3 slice builds + 6 bounds checks);
    /// IS three `memcpy`s over contiguous runs. The `reserve` above already made it non-growing,
    /// so no realloc can intervene between the copies.
    pub fn extend_from(&mut self, other: &FieldBlob) {
        let n = other.len();
        if n == 0 {
            return;
        }
        let count = self.len();
        self.reserve(count + n);
        // `n > 0` => `reserve` allocated => `cap > 0`, so `set_count` writes a real header and
        // never the read-only sentinel.
        let cap = self.cap();
        debug_assert!(!std::ptr::eq(self.0.as_ptr(), other.0.as_ptr()));
        unsafe {
            let base = self.0.as_ptr();
            std::ptr::copy_nonoverlapping(
                other.cells_ptr(),
                (base.add(Self::cells_off()) as *mut i64).add(count),
                n,
            );
            std::ptr::copy_nonoverlapping(
                other.ids_ptr(),
                (base.add(Self::ids_off(cap)) as *mut u32).add(count),
                n,
            );
            std::ptr::copy_nonoverlapping(
                other.kinds_ptr(),
                base.add(Self::kinds_off(cap)).add(count),
                n,
            );
        }
        self.set_count((count + n) as u32);
    }

    /// First slot holding `id`, by plain scan. THE reference semantics for every lookup below:
    /// FIRST match wins (a `$`-named field declared by a guest class AND by its guest super
    /// interns BARE in both, so a blob can legitimately carry one id twice and the earlier slot
    /// is the subclass's).
    #[inline]
    pub(crate) fn scan_id(&self, id: u32) -> Option<u32> {
        let ids = self.ids();
        for i in 0..ids.len() {
            if ids[i] == id {
                return Some(i as u32);
            }
        }
        None
    }

    /// First slot holding `id`, consulting the global `fid`-bucketed slot-hint table at
    /// `hint_base` (`Heap::field_hints`, published in `JitCtx`; 0 = not allocated yet).
    ///
    /// **THE COMPILED HALF OF THIS ENGINE HAS HAD AN O(1) FIELD LOOKUP ALL ALONG AND THE HOST
    /// HALF NEVER READ IT.** `field_hints` is `JIT_FIELD_HINT_N` (8192) u32 slots indexed
    /// `fid & 8191`; every emitted getfield/putfield tries `hint < count && ids[hint] == fid`
    /// before its id scan, and `publish_field_hint` refills it from the crossings. Meanwhile
    /// `get_field_by_id`, `get_field`, `put_field_by_id`, `put_field_hint` and `field_slot` --
    /// the host paths, i.e. ~92% of a product boot's samples -- each open-coded a linear walk of
    /// the object's id array right next to it.
    ///
    /// **Complexity: WAS O(fields-per-object) per access.** That is the wrong shape twice over.
    /// The id array is a constant of the CLASS SHAPE -- `prepopulate_layout` lays every declared
    /// field down at its layout index, identically for every instance -- so the walk re-derives
    /// one class-constant answer per object per access. And the synthetic `$key`/`$val`/
    /// `$keystr`/`$elems` fields are APPENDED PAST the declared layout, so on the 32-field key
    /// class the three reads the map-key path does per key each walked all 32 declared ids
    /// first, i.e. the population that grows fastest was also the one placed worst.
    /// **IS O(1): one direct-indexed load plus the `ids[h] == id` verify; scan only on a miss.**
    ///
    /// **AND IT DELIBERATELY ADDS NO TABLE.** A `(class word, field id) -> slot` cache was built
    /// here twice and measured SLOWER both times (30.7 ns plain scan against 34.1 at 196 KB and
    /// 35.9 at 6 KB) -- a hash plus a table load plus a verify loses to a two-to-four entry scan
    /// over an array the caller has already loaded. This is not that: ZERO new bytes (the 32 KB
    /// table already exists, is already resident, and is already touched by compiled code for
    /// these very ids), NO hash (a mask on a register the caller already holds), and the verify
    /// is the `ids[h]` load the scan would have done anyway. The host publishing back also warms
    /// the table FOR the compiled sites -- the bargain `publish_field_hint` documents in the
    /// other direction, which until now only ever ran one way.
    ///
    /// **SHORT BLOBS KEEP THE PLAIN SCAN, BECAUSE THAT IS WHAT THE REFUTATION ACTUALLY SAID.**
    /// Below `FAST_MIN` ids the whole array sits in the half cache line the caller just touched
    /// and nothing beats walking it; the memo engages only where the walk crosses lines, which
    /// also keeps the shared table's working set to the long-shape ids that need it.
    ///
    /// Sound by construction, and by the SAME construction compiled code already ships with: a
    /// stale, colliding or plain wrong entry fails `ids[h] == id` and costs exactly one re-scan.
    /// The one semantic subtlety is the duplicate id above -- a hint can name the LATER slot of
    /// a doubled id where the scan would have named the earlier one. That is the pre-existing,
    /// default-ON contract of `field_hints`, `get_field_hint` and `field_slot`, unchanged here;
    /// callers that must have first-wins regardless use `scan_id`.
    #[inline]
    pub(crate) fn find_id(&self, id: u32, hint_base: usize) -> Option<u32> {
        let ids = self.ids();
        let n = ids.len();
        // Ordered cheapest-first: `n` is already in a register, `hint_base` is a plain field
        // load, the flag is a `OnceLock` acquire -- and a short blob pays none of the three.
        if n < FAST_MIN || hint_base == 0 || !fast_fieldid_enabled() {
            return self.scan_id(id);
        }
        let bucket = id as usize & (JIT_FIELD_HINT_N - 1);
        // SAFETY: `ensure_field_hints` allocates exactly `JIT_FIELD_HINT_N` u32s and never
        // resizes or moves the table (that is why the base is cached in `JitCtx` at all), and
        // `bucket` is masked into range. `AtomicU32` has the same layout as `u32`; relaxed
        // rather than a bare load/store so a gil-free worker sharing it is not a data race --
        // the identical shape `publish_field_hint` already uses on this same table.
        let cell = unsafe { &*(hint_base as *const std::sync::atomic::AtomicU32).add(bucket) };
        let h = cell.load(std::sync::atomic::Ordering::Relaxed) as usize;
        if h < n && ids[h] == id {
            return Some(h as u32);
        }
        let found = self.scan_id(id);
        if let Some(i) = found {
            cell.store(i, std::sync::atomic::Ordering::Relaxed);
        }
        found
    }
}

/// Blobs with fewer ids than this keep the plain scan, byte-for-byte as before. 8 u32 ids = 32 B
/// = half a cache line, inside the line `ids()` has already pulled in. TABLE SIZE ADDED BY THIS
/// WHOLE MECHANISM: **zero** -- it reads and writes `Heap::field_hints`, the 32 KB
/// (8192 x u32) table `ensure_field_hints` has always allocated for compiled code.
pub(crate) const FAST_MIN: usize = 8;

/// Kill switch for the hint-first field lookup: `RAVA_NO_FAST_FIELDID=1` (or the blanket
/// `RAVA_NO_EMIT_MORE`) restores the plain linear scan on every path. Read once.
#[inline]
pub(crate) fn fast_fieldid_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_FIELDID").is_none() && !crate::flags::emit_more_disabled()
    })
}
