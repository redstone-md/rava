//! Object slots: allocation, retirement, and the descriptor that mirrors each one.


use crate::heap::*;

impl Heap {
    /// Raw base pointer to the `objects` slot array. Valid only while no push/realloc
    /// happens (guaranteed between `reserve_obj_range` and the region's end). A worker
    /// fills its OWN pre-reserved slot through this; disjoint indices ⇒ no data race.
    /// SAFETY (caller): only write indices inside a range you reserved + own, never
    /// grow `objects` during the region, and join all workers before reading.
    #[allow(dead_code)] // dormant until the worldgen-offload increment wires it in
    pub(crate) fn objects_slot_base(&mut self) -> *mut ObjectSlot {
        self.objects.write().unwrap_or_else(|e| e.into_inner()).as_mut_ptr()
    }

    /// Raw base pointer to the `obj_descs` array (the native field-descriptor table
    /// the JIT/native field path reads). `reserve_obj_range` pre-sizes obj_descs to
    /// match `objects`, so — like `objects_slot_base` — this stays valid for the
    /// whole region (no realloc). A worker, after writing its OWN object slot,
    /// writes that slot's ObjDesc here at the SAME (disjoint) index, so the native
    /// field path can read the worker-built object's fields. SAFETY (caller): only
    /// write indices you reserved + own; do NOT grow `obj_descs` during the region;
    /// after the region (under the GIL) refresh `jit_ctx.obj_desc_base` since reserve
    /// may have moved the buffer. The per-object `ids/cells/kinds` pointers stay
    /// valid because each `Object` is boxed (its field vecs don't move with the Vec).
    #[allow(dead_code)] // dormant until the worldgen-offload increment wires it in
    pub(crate) fn obj_descs_slot_base(&mut self) -> *mut ObjDesc {
        self.obj_descs.write().unwrap_or_else(|e| e.into_inner()).as_mut_ptr()
    }

    /// ⭐**THE WHOLE HANDLE→OBJECT LOOKUP IS ONE INDEX, AND IT MUST STAY INLINABLE.**
    /// `release` builds with `lto = "thin"` and the default 16 codegen units, and every
    /// caller of this accessor family lives in a DIFFERENT unit from `heap::objects` — the
    /// interpreted virtual-dispatch path (`opcodes::invoke_virtual`), the direct-call
    /// guards (`vm::jit_direct_call`), the map-key renderer and the native dispatch table.
    /// Without `#[inline]` the callee is not a candidate for cross-unit import at all, so a
    /// two-load body pays a call, an argument spill and a return on every virtual dispatch.
    /// The bodies are two loads or fewer; none of them can grow code.
    ///
    /// Stage 2c: the read guard is released BEFORE returning. Sound because
    /// `ObjectSlot` owns a `Box<Object>` — the pointee address does not move
    /// when the outer `Vec` reallocates, and a slot is only retired at a GC
    /// safepoint. That is exactly the lifetime assumption every caller already
    /// made when this returned a borrow of the `Vec` itself.
    #[inline]
    pub fn get(&self, handle: u32) -> Option<&Object> {
        let p: *const Object = {
            let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
            g.get(handle as usize)?.get()? as *const Object
        };
        // SAFETY: stable `Box` pointee; see the note above.
        Some(unsafe { &*p })
    }

    /// Is this object handle currently LIVE (not swept)? For the lambda-dedup cache prune — a swept
    /// handle's slot is reused, so a stale cache entry must be dropped. A handle past the table or a
    /// swept slot (obj_alive false / empty class) is dead.
    ///
    /// ⭐**TWO TABLE PROBES, NOT FOUR.** One liveness question used to ask the object table
    /// for its LENGTH, ask `obj_alive` for its LENGTH, then index BOTH with a panicking
    /// `[]` — four probes and two unwind landing pads, where `get` answers "in range" and
    /// "which slot" in a single bounds test. Grows with nothing; it is the constant factor
    /// on a predicate the GC runs once per live lambda-dedup entry per collection and the
    /// natives run per reachability query. The answer is identical for every input,
    /// including a handle past either table and a handle whose slot index outlives a
    /// shorter `obj_alive` (still treated as alive — the `h >= obj_alive.len()` arm).
    #[inline]
    pub(crate) fn is_ref_alive(&self, handle: u32) -> bool {
        let h = handle as usize;
        if !fast_objtable_enabled() {
            // Stage 2c: ONE guard acquisition per table. `std::sync::Mutex` is NOT
            // reentrant and a temporary guard lives to the end of the statement, so
            // naming the guard is a correctness requirement here, not a style choice.
            let alive = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
            let n_objects = self.objects.read().unwrap_or_else(|e| e.into_inner()).len();
            return h < n_objects
                && (h >= alive.len() || alive[h])
                && self.get(handle).is_some_and(|o| !o.class.is_empty());
        }
        // `obj_alive` is consulted only where it exists; a slot past its end counts as alive,
        // exactly as the `h >= self.obj_alive.len()` disjunct did. A handle past the OBJECT
        // table falls out of the `get` below, so no separate length test is needed.
        if self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).get(h).is_some_and(|alive| !*alive) {
            return false;
        }
        self.get(handle).is_some_and(|o| !o.class.is_empty())
    }

    /// Object.clone(): a new object of the same class with every field copied
    /// (shallow — field values, incl. array/object refs, are shared). Backs the
    /// `super.clone()` JDK contract (e.g. fastutil maps clone then deep-copy
    /// their own key/value arrays).
    ///
    /// ⭐**ONE FIELD-BLOCK PER CLONE INSTEAD OF TWO PLUS A FREE.** A `FieldBlob` is its own
    /// `std::alloc::alloc` block (`8 + 13*cap` bytes) — the SECOND allocation every handle
    /// object carries on top of its `Box<Object>` — and this used to mint a throwaway one.
    /// `o.fields.clone()` allocated a full copy of the source's block, handed it to
    /// `put_object`, and `put_object`'s free-slot arm then did `clear()` + `extend_from()`
    /// into the RECYCLED slot's already-owned block and dropped the copy on the floor:
    /// **one malloc + one memcpy + one free that nothing ever read.** Worse, the recycled
    /// block's retained capacity was the thing that made the free-slot arm cheap in the
    /// first place, and the temporary bypassed it.
    ///
    /// Now the object is stored with an EMPTY blob (the shared read-only sentinel — zero
    /// bytes), and the copy is made straight into whatever block the stored slot ended up
    /// owning. Free-slot arm with enough retained capacity: **zero allocations.** Fresh-push
    /// arm: exactly one, the same one `clone()` would have made.
    ///
    /// BIT-IDENTICAL: `extend_from` performs the identical three `memcpy`s over
    /// cells/ids/kinds that `Clone` does, in the same order, onto a blob whose `len()` is 0
    /// in both arms (`put_object` clears a recycled slot's blob, and a pushed slot starts at
    /// the sentinel). Field VALUES, ids and kinds are copied verbatim, so FK_BOX cells still
    /// share the source's `field_box` index exactly as before. The only difference is the
    /// block's `cap`, which is not guest-observable (it is re-derived by `sync_obj_desc`
    /// below, which `put_object` must be followed by anyway because the copy may have
    /// reallocated the block and moved `ids`/`cells`).
    ///
    /// Kill switch `RAVA_NO_FAST_BIRTH2=1` restores the throwaway-clone shape verbatim.
    pub fn shallow_clone(&mut self, handle: u32) -> u32 {
        let (class, lambda, nfields) = match self.get(handle) {
            Some(o) => (o.class.clone(), o.lambda.clone(), o.fields.len()),
            None => return self.alloc("java/lang/Object"),
        };
        // FK_BOX cells share the source's field_box index — fine, those entries
        // are immutable (Str) and never freed.
        let captured_bytes = lambda.as_deref().map_or(0, Lambda::captured_bytes);
        self.bytes.fetch_add(48 + nfields * 13 + captured_bytes, std::sync::atomic::Ordering::Relaxed);
        // REFUSE ON A GIL-FREE WORKER, and the reason is precise: there `put_object` takes
        // the `alloc_in_arena` branch, which returns handle `0` on an exhausted segment
        // WITHOUT storing anything, and which deliberately skips `sync_obj_desc` because that
        // writes shared `jit_ctx`/`obj_descs` state a worker may not touch. Both properties
        // break the fill-after-store shape below (it would write the source's fields into
        // object 0, and it would publish a descriptor off-GIL), so the whole clone falls back
        // to the shape `put_object` was built for.
        #[cfg(feature = "next-gen-gc")]
        let worker = crate::parallel::owns_reserved_segment();
        #[cfg(not(feature = "next-gen-gc"))]
        let worker = false;
        if worker || !fast_birth2_enabled() {
            let fields = self.objects.read().unwrap_or_else(|e| e.into_inner())[handle as usize]
                .get()
                .expect("source slot was live one statement ago")
                .fields
                .clone();
            return self.put_object(Object { class, fields, lambda });
        }
        let h = self.put_object(Object { class, fields: FieldBlob::new(), lambda });
        if nfields == 0 {
            // Nothing to copy; `put_object` already published the (empty) descriptor.
            return h;
        }
        // `h != handle` is a THEOREM, not a hope: `put_object` returns either `objects.len()`
        // (past every existing slot) or a `free_obj` pop, and a handle only reaches `free_obj`
        // through `retire_object_slot`, which first replaces its slot with either a null
        // tombstone (→ the `None` arm above already returned) or a live object holding
        // `FieldBlob::new()` (→ `nfields == 0`, returned just above). So a source with fields
        // cannot be on the free list, and the two blobs below are distinct allocations.
        debug_assert_ne!(h, handle, "put_object re-issued a live source handle");
        // Take the destination's blob OUT of the table so the copy borrows the source
        // immutably and the destination not at all — no aliasing, no raw pointers. The
        // sentinel left behind owns nothing, so dropping it when the filled blob goes back
        // is a no-op.
        let mut blob = std::mem::replace(
            &mut self.objects.write().unwrap_or_else(|e| e.into_inner())[h as usize]
                .get_mut()
                .expect("put_object just stored a live slot")
                .fields,
            FieldBlob::new(),
        );
        {
            let g_r = self.objects.read().unwrap_or_else(|e| e.into_inner());
            blob.extend_from(
                &g_r[handle as usize]
                    .get()
                    .expect("source slot was live one statement ago")
                    .fields,
            );
        }
        // Stage 2c: the destination write needs the WRITE guard, and the source read
        // guard above is dropped first — `std::sync::RwLock` is not reentrant, so
        // holding both at once would deadlock.
        self.objects
            .write()
            .unwrap_or_else(|e| e.into_inner())[h as usize]
            .get_mut()
            .expect("put_object just stored a live slot")
            .fields = blob;
        // The copy may have grown the block (fresh-push arm always does), which moves
        // `ids`/`cells` — refresh the native descriptor the JIT/native field path reads.
        // `put_object`'s own sync ran against the empty sentinel and is now stale.
        self.sync_obj_desc(h);
        h
    }

    #[inline]
    pub fn class_of(&self, handle: u32) -> Option<&str> {
        self.get(handle).map(|o| o.class.as_ref())
    }

    /// DEBUGGER EXTENSION (RAVA_JIT_SENTINEL): scan every object's class Arc<str> len; return
    /// (count of CORRUPT ones — len > 8192 impossible for a real class name — , first such
    /// handle). Bracketing a compiled call with this NAMES the codegen op that corrupts a
    /// heap Object (the class Arc is at Box offset 8; a stray compiled write lands there).
    /// O(n) but only when the env is set. `objects.len()` is the scan bound.
    pub(crate) fn corrupt_class_probe(&self) -> (usize, u32) {
        let mut count = 0usize;
        let mut first = u32::MAX;
        for (i, o) in self.objects.read().unwrap_or_else(|e| e.into_inner()).iter().enumerate() {
            if !self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(o) = o.get() else { continue };
            if o.class.len() > 8192 {
                count += 1;
                if first == u32::MAX {
                    first = i as u32;
                }
            }
        }
        (count, first)
    }

    /// FORENSIC (use-after-free hunt): the FIRST live object that holds `handle` in a field
    /// cell — returns (holder class, that field's kind, field id). Reveals whether the swept
    /// handle's holder stored it under FK_REF (kind 4, would be marked) or a WRONG kind
    /// (skipped by heap.gc Mark::Obj → the cross-heap gap).
    pub(crate) fn holder_of(&self, handle: u32) -> Option<(String, u8, u32)> {
        let hv = handle as i64;
        for (idx, o) in self.objects.read().unwrap_or_else(|e| e.into_inner()).iter().enumerate() {
            if !self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).get(idx).copied().unwrap_or(false) {
                continue;
            }
            let Some(o) = o.get() else { continue };
            let (ids, cells, kinds) = (o.fields.ids(), o.fields.cells(), o.fields.kinds());
            for i in 0..cells.len() {
                if cells[i] == hv {
                    return Some((o.class.to_string(), kinds[i], ids[i]));
                }
            }
        }
        None
    }

    /// FORENSIC (GC-cycle-1 hunt): the ADDRESS of a handle's boxed Object. If it lands
    /// in the arena VA range (>= ARENA_FLOOR, 8 GiB), is_arena_obj_addr false-positives on
    /// it → the moving collector treats this heap Box<Object> as an arena object and
    /// evacuates/overwrites it → the class-field corruption. Also count how many boxes are
    /// in that range (the exposure). Non-perturbing (called only at the corruption point).
    pub(crate) fn box_probe(&self, handle: u32) -> (u64, bool, usize) {
        let addr = self.get(handle)
            .map_or(0, |o| o as *const Object as u64);
        let in_arena = crate::nextgen::is_arena_obj_addr(addr);
        let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
        let cnt = g
            .iter()
            .filter_map(ObjectSlot::get)
            .filter(|o| crate::nextgen::is_arena_obj_addr(*o as *const Object as u64))
            .count();
        (addr, in_arena, cnt)
    }

    #[inline]
    pub fn class_name_ptr(&self, handle: u32) -> usize {
        match self.get(handle) {
            Some(o) if !o.class.is_empty() => o.class.as_ptr() as usize,
            _ => 0,
        }
    }

    /// 23.4 heap-Ref PIC: current base pointer of the pointer-sized `objects` slot table. Re-read
    /// per dispatch so a realloc / VM move is always reflected — this address is NEVER baked into
    /// machine code (the codegen calls this each dispatch via a leaf accessor). Memory-safe by
    /// construction: the returned base is always the live Vec's current data pointer.
    #[inline]
    pub fn objects_base_ptr(&self) -> i64 {
        self.objects.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as i64
    }

    /// Byte offset of the `class` field within `Object`. `Object` is `#[repr(Rust)]` (derives
    /// Serialize) so fields may be reordered — query the real offset, never assume source order.
    /// The machine-code heap-Ref PIC reads `*(box_ptr + this)` = the first word of the `Arc<str>`
    /// (the pointer-to-ArcInner), the interned per-class key that matches `class_name_ptr`'s object.
    #[inline]
    pub fn object_class_offset() -> i64 {
        std::mem::offset_of!(Object, class) as i64
    }

    /// The authoritative class key for a handle: the first word of `Object.class` (`Arc<str>` →
    /// ptr-to-ArcInner), read exactly as the machine-code PIC reads it. Used by the 23.4a shadow to
    /// verify the machine-code extraction bit-for-bit. `None` if the handle is out of range.
    #[inline]
    pub fn object_class_word(&self, handle: u32) -> Option<i64> {
        let obj = self.get(handle)?;
        Some(unsafe { *(&obj.class as *const Arc<str> as *const i64) })
    }
}

/// Kill switch for the OBJECT-BIRTH ALLOCATION SHAPE (`RAVA_NO_SLAB=1`, or the blanket
/// `RAVA_NO_EMIT_MORE`). Lives here, beside `fast_birth2_enabled`, because this is the file
/// that owns how a handle object is born; its one consumer today is
/// `FieldBlob::alloc_size` (heap/fieldblob.rs), which rounds the field blob's allocation
/// request up to the blob's own 8-byte alignment so it clears mimalloc's
/// `mi_malloc_aligned` fast-path predicate `(size & (align-1)) == 0` instead of falling
/// through to `mi_heap_malloc_aligned_at`. See that function for the full argument.
///
/// ⭐**READ ONCE, AND THAT IS A CORRECTNESS PROPERTY HERE, NOT A PERFORMANCE ONE.** The
/// answer decides the `Layout::size` of BOTH the allocation and the matching deallocation of
/// the same block, which `Layout`'s contract requires to agree. A `OnceLock` is a process
/// constant, so no block can ever be requested under one answer and released under the
/// other. Do not make this per-call, per-thread or re-readable.
///
/// The cold arm allocates (`env::var_os` builds a `String`), which is safe ONLY because
/// nothing reachable from the global allocator calls back into `alloc_size` — mimalloc and
/// the CLI's counting wrapper touch no `FieldBlob`. [[allocation-traffic]] records what the
/// other shape costs: a flag whose `get_or_init` closure re-enters the allocator that is
/// waiting on that same closure hangs the boot silently.
#[inline]
pub(crate) fn slab_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_SLAB").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Kill switch for the handle-birth allocation-shape work in this file
/// (`RAVA_NO_FAST_BIRTH2=1`, or the blanket `RAVA_NO_EMIT_MORE`): restores the
/// throwaway-`FieldBlob`-clone shape `shallow_clone` used to have, byte for byte.
/// Read once; the `OnceLock` is only ever consulted on the clone path, which is
/// several orders of magnitude colder than the object-birth path itself.
#[inline]
pub(crate) fn fast_birth2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_BIRTH2").is_none() && !crate::flags::emit_more_disabled()
    })
}
