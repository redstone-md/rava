//! Allocating and retiring object slots, and the descriptor sync behind each.

use crate::heap::*;

impl Heap {
    /// Allocate a zeroed instance of `class`; returns its handle.
    /// Place an object into a reused (swept) slot if one is free, else append.
    pub(crate) fn put_object(&self, obj: Object) -> u32 {
        // 11.7: a GIL-free pool worker must NOT push to the shared `objects` Vec (realloc + torn
        // handle-table race — the 11.6 ULE). Route its transient `Value::Ref` into THIS worker's
        // pre-reserved, disjoint arena segment (arena_setup at region entry). A `None` = segment not
        // set up / exhausted ⇒ a worker can't safely fall back to the shared push, so tear the region
        // down (the driver recomputes serially); return a dummy handle for the aborting iteration.
        //
        // The gate is `owns_reserved_segment`, not `gil::is_worker()`: the latter is a
        // thread-lifetime latch, so a plain GIL-BOUND executor guest task — which holds
        // the GIL and owns no reservation — used to come here too, find an empty range,
        // abort a region that does not exist and hand the guest handle `0`.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::owns_reserved_segment() {
            return match unsafe { self.alloc_in_arena(obj) } {
                Some(h) => h,
                None => {
                    // 11.8 abort diagnostic (Point 1).
                    eprintln!("[RAVA-ABORT-DIAG] Object allocation limit reached on worker! Segment size exhausted.");
                    crate::parallel::request_abort();
                    0
                }
            };
        }
        let h = if let Some(h) = self.free_obj.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            // Reuse the freed slot's Box + field-vec CAPACITY in place rather than
            // `Box::new` + 3 fresh field Vecs. A churning alloc workload (MC entities/
            // BlockPositions, the Allocs bench) hits the free list constantly, so this
            // turns ~4 mallocs/alloc into ~0 (clear keeps capacity; the field pushes
            // that follow refill it). The raw field-vec pointers move, so sync_obj_desc
            // below refreshes the descriptor exactly as on a fresh Box.
            let mut objs = self.objects.write().unwrap_or_else(|e| e.into_inner());
            // Reuse the freed slot's blob CAPACITY in place: clear keeps the allocation,
            // extend_from refills it (the incoming `obj` is usually empty — its fields are
            // pushed on the handle afterward, so this keeps the capacity for those pushes).
            if let Some(slot) = objs[h as usize].get_mut() {
                slot.fields.clear();
                slot.fields.extend_from(&obj.fields);
                slot.class = obj.class;
                slot.lambda = obj.lambda;
            } else {
                objs[h as usize] = ObjectSlot::live(obj);
            }
            drop(objs);
            self.obj_alive.lock().unwrap_or_else(|e| e.into_inner())[h as usize] = true;
            h
        } else {
            let h = self.objects.read().unwrap_or_else(|e| e.into_inner()).len() as u32;
            self.objects.write().unwrap_or_else(|e| e.into_inner()).push(ObjectSlot::live(obj));
            self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).push(true);
            h
        };
        // Pinned-mirror hygiene (flag-gated): a REUSED handle slot may still map to
        // the PREVIOUS object's pinned mirror — serving another object's cells (the
        // diverge-hunt's 12k VoxelShape hits). Clear the mapping at the alloc choke;
        // prepopulate_known re-establishes it when this object is mirror-eligible.
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::pinned_mirror_enabled() {
            crate::nextgen::pinned_mirror_set(h, std::ptr::null_mut());
        }
        self.sync_obj_desc(h);
        // GC-cycle-1 watchpoint hook: when this handle == RAVA_WATCH_HANDLE, call a
        // no-op with the Object's class-Arc LEN field address (offset 8) — an lldb
        // breakpoint on `gc_watch_hook` reads the addr and arms a HARDWARE WATCHPOINT to
        // catch whatever writes garbage there. Cheap compare per alloc; only when set.
        if watch_handle() == h {
            let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
            let object = g[h as usize]
                .get()
                .expect("newly allocated object slot is live");
            let addr = &object.class as *const Arc<str> as u64 + 8;
            gc_watch_hook(addr);
        }
        // M3 9.2d.3: stamp a recycling-robust alloc generation (biome probe only).
        #[cfg(feature = "next-gen-gc")]
        if crate::m3biome::enabled() {
            mark_obj_gen(h);
        }
        h
    }

    /// Retire one live object handle exactly once. The GC owns the liveness and
    /// free-list transition; `release_storage` only selects whether the slot keeps
    /// the legacy empty allocation or becomes a pointer-sized null tombstone.
    pub(crate) fn retire_object_slot(&mut self, h: usize, release_storage: bool) -> usize {
        if !self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).get(h).copied().unwrap_or(false) {
            return 0;
        }
        let freed = self.get(h as u32).map_or(0, |o| {
            48 + o.fields.len() * 13
                + o.lambda.as_deref().map_or(0, Lambda::captured_bytes)
        });
        let replacement = if release_storage {
            ObjectSlot(None)
        } else {
            ObjectSlot::live(Object {
                class: self.intern(""),
                fields: FieldBlob::new(),
                lambda: None,
            })
        };
        self.objects.write().unwrap_or_else(|e| e.into_inner())[h] = replacement;
        self.obj_alive.lock().unwrap_or_else(|e| e.into_inner())[h] = false;
        if h < self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).len() {
            self.obj_descs.write().unwrap_or_else(|e| e.into_inner())[h] = ObjDesc::empty();
        }
        self.free_obj.lock().unwrap_or_else(|e| e.into_inner()).push(h as u32);
        // A retired slot is re-issued to an UNRELATED object by the next `put_object`,
        // so any cache keyed by `Value::Ref` handle can alias from this moment on. One
        // relaxed bump is the invalidation signal (see `handle_epoch`).
        handle_epoch_bump();
        freed
    }

    /// PARALLEL-WORLDGEN (increment 2b core). Reserve `k` contiguous object slots
    /// (empty placeholders) and return the first handle. Call on the MAIN thread
    /// under the GIL before a parallel region. Because all slots are pre-pushed
    /// here, the `objects` Vec does NOT grow (reallocate) while workers fill them,
    /// so `objects_slot_base()` stays valid and workers writing DISJOINT slots
    /// cannot race (no overlapping access, no realloc). obj_desc sync is done
    /// per-slot by the filler (see fill_reserved_object) — also disjoint.
    #[allow(dead_code)] // dormant until the worldgen-offload increment wires it in
    pub(crate) fn reserve_obj_range(&mut self, k: usize) -> u32 {
        let start = self.objects.read().unwrap_or_else(|e| e.into_inner()).len() as u32;
        let empty = self.intern("");
        self.objects.write().unwrap_or_else(|e| e.into_inner()).reserve(k);
        self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).reserve(k);
        self.obj_descs.write().unwrap_or_else(|e| e.into_inner()).reserve(start as usize + k);
        for _ in 0..k {
            self.objects.write().unwrap_or_else(|e| e.into_inner()).push(ObjectSlot::live(Object {
                class: empty.clone(),
                fields: FieldBlob::new(),
                lambda: None,
            }));
            self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).push(true);
        }
        // Keep obj_descs sized to objects so a later sync_obj_desc indexes in range.
        while self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).len() < self.objects.read().unwrap_or_else(|e| e.into_inner()).len() {
            self.obj_descs.write().unwrap_or_else(|e| e.into_inner()).push(ObjDesc::empty());
        }
        // The push loop reallocated obj_descs → republish its base so the JIT field path (and the
        // workers' fill_reserved via obj_descs_slot_base) reads the current buffer, not a stale one.
        self.jit_ctx.obj_desc_base = self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize;
        // R2: the reserve(k) above reallocated `objects` too → republish its base for the JIT
        // `[ctx+48]` heap-Ref reads (this batch path never goes through sync_obj_desc).
        self.jit_ctx.objects_base = self.objects.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize;
        start
    }

    /// GILFREE-RINGS park: give a `reserve_obj_range` claim back after its region was
    /// DISCARDED. Every slot is either an untouched placeholder or an object a worker
    /// filled and the coordinator then threw away — and a filled slot left alive would
    /// point into the nursery span the coordinator just retreated past, i.e. a dangling
    /// field the next collection would follow. Retiring the range also stops the claim
    /// leaking: the slots return to the free list.
    pub(crate) fn release_obj_range(&mut self, base: u32, count: u32) {
        let end = (base as usize + count as usize).min(self.objects.read().unwrap_or_else(|e| e.into_inner()).len());
        for h in base as usize..end {
            self.retire_object_slot(h, false);
        }
    }

    /// GILFREE-RINGS: the `field_box` twin of `reserve_obj_range`. Call on the MAIN
    /// thread under the GIL before a parallel region; the worker then WRITES its own
    /// slots by index (`encode_field`) instead of pushing.
    ///
    /// The extra `reserve` headroom is deliberate: a sibling worker publishing a
    /// finished task DOES hold the GIL and can push a box of its own, and a realloc
    /// there would move the buffer under an in-flight lane worker. Headroom makes
    /// that push non-reallocating for the life of the region. Reserved-but-unwritten
    /// slots are plain `Null` that no object addresses, so the next `compact_boxes`
    /// drops them — the reservation cannot leak.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn reserve_field_box_range(&mut self, k: usize) -> Option<u32> {
        let mut fb = self.field_box.write().unwrap_or_else(|e| e.into_inner());
        let base = u32::try_from(fb.len()).ok()?;
        u32::try_from(fb.len() + k).ok()?;
        fb.reserve(k + FIELD_BOX_REGION_HEADROOM);
        let new_len = fb.len() + k;
        fb.resize(new_len, Value::Null);
        drop(fb);
        self.bytes.fetch_add(k * SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
        Some(base)
    }

    /// Build the ObjDesc for an already-stored boxed object (its field vec pointers).
    /// Pure helper (no `&self` state mutated) so a worker can compute its slot's
    /// descriptor off the boxed object it just wrote, then store it disjointly via
    /// `obj_descs_slot_base`.
    #[allow(dead_code)] // dormant until the worldgen-offload increment wires it in
    pub(crate) fn desc_of(obj: &Object) -> ObjDesc {
        ObjDesc {
            ids: obj.fields.ids_ptr(),
            cells: obj.fields.cells_ptr(),
        }
    }

    /// The single audited unsafe primitive a worker uses to materialize ONE object
    /// into a reserved slot: store the boxed object, then store its native
    /// descriptor at the SAME disjoint index. Keeping both writes here (rather than
    /// open-coded at each call site) bounds the unsafe surface the live offload must
    /// trust to one reviewed place.
    ///
    /// SAFETY (caller must guarantee):
    /// - `idx` is a slot reserved via `reserve_obj_range` that THIS worker owns
    ///   exclusively (no other thread writes the same index) — disjoint indices ⇒
    ///   no data race on `objects`/`obj_descs`.
    /// - `obj_base`/`desc_base` came from `objects_slot_base`/`obj_descs_slot_base`
    ///   for the SAME heap and nothing has grown either Vec since (no realloc).
    /// - all workers are joined before the main thread reads these slots.
    #[allow(dead_code)] // dormant until the worldgen-offload increment wires it in
    pub(crate) unsafe fn fill_reserved(
        obj_base: *mut ObjectSlot,
        desc_base: *mut ObjDesc,
        idx: usize,
        obj: Object,
    ) {
        *obj_base.add(idx) = ObjectSlot::live(obj);
        let stored = (*obj_base.add(idx))
            .get()
            .expect("reserved object slot was just initialized");
        *desc_base.add(idx) = Heap::desc_of(stored);
    }

    /// Allocate `obj` into THIS gil-free worker's private arena (a pre-reserved,
    /// disjoint slot range) and return its handle — the worker-thread equivalent of
    /// `put_object`, with NO shared `objects` Vec push (which would realloc + race).
    /// `None` when the arena is exhausted (driver re-reserves under the GIL). Takes
    /// `&self`: the worker writes only slots IT reserved (disjoint from main + other
    /// workers), and the region guarantees no realloc, so the const→mut cast is sound.
    /// SAFETY: caller must be a gil-free worker inside an active offload region whose
    /// arena was set up from `reserve_obj_range`.
    #[allow(dead_code)] // dormant until the offload driver wires it in
    pub(crate) unsafe fn alloc_in_arena(&self, obj: Object) -> Option<u32> {
        let slot = crate::parallel::arena_take()?;
        let obase = self.objects.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as *mut ObjectSlot;
        let dbase = self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as *mut ObjDesc;
        Heap::fill_reserved(obase, dbase, slot as usize, obj);
        Some(slot)
    }

    /// Refresh the native field descriptor for object `h` from its current field
    /// vecs (called on object (re)allocation and after every field push, which
    /// may have reallocated the vecs), and re-cache the table base.
    ///
    /// `mirror_all` controls the pinned-mirror dual-store (flag-gated):
    /// * `true` — BULK callers (initial object build, `prepopulate_layout`, restore)
    ///   re-encode EVERY field cell into the pinned mirror, because all of them
    ///   genuinely need to land at once.
    /// * `false` — the INCREMENTAL field-PUSH callers (put_field / put_field_hint
    ///   push arms), which add exactly ONE field. Those arms call
    ///   `pinned_mirror_store` for the just-pushed field right after this returns,
    ///   so a whole-object re-encode here would be redundant O(nfields²) waste that
    ///   only rewrites already-correct cells. Skip the mirror block entirely.
    pub(crate) fn sync_obj_desc(&self, h: u32) {
        self.sync_obj_desc_impl(h, true);
    }

    /// Descriptor-only refresh for the incremental field-PUSH path — see
    /// `sync_obj_desc`'s `mirror_all=false` case. The single pushed field is
    /// mirrored by the caller's `pinned_mirror_store`.
    pub(crate) fn sync_obj_desc_incremental(&self, h: u32) {
        self.sync_obj_desc_impl(h, false);
    }

    /// Stage 2c transitional: `jit_ctx.objects_base`/`obj_desc_base` are `repr(C)` ABI
    /// fields read by compiled getfield/putfield at fixed offsets; publishing a new
    /// spine base needs `&mut` only because `JitCtx` is plain data. Same discipline as
    /// `set_static_gen_mirror` and `arrays::publish_desc_base`.
    #[allow(invalid_reference_casting)]
    fn publish_obj_bases(&self, objects_base: usize, obj_desc_base: Option<usize>) {
        let ctx = unsafe { &mut *(self as *const Self as *mut Self) };
        ctx.jit_ctx.objects_base = objects_base;
        if let Some(b) = obj_desc_base {
            ctx.jit_ctx.obj_desc_base = b;
        }
    }

    pub(crate) fn sync_obj_desc_impl(&self, h: u32, mirror_all: bool) {
        let hu = h as usize;
        // R2: republish objects_base after every alloc — this runs right after `alloc`'s
        // `objects.push` (which may have reallocated the Vec), so the JIT `[ctx+48]` read
        // never sees a stale base. Cheap unconditional store; the ptr is usually unchanged.
        let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
        let objects_base = g.as_ptr() as usize;
        let o = g[hu]
            .get()
            .expect("descriptor sync requires a live object slot");
        let desc = ObjDesc {
            ids: o.fields.ids_ptr(),
            cells: o.fields.cells_ptr(),
        };
        drop(g);
        if hu < self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).len() {
            // In-range overwrite (the hot case: every field push on a being-built or
            // existing object). No Vec realloc → the base pointer is unchanged, so only
            // objects_base needs republishing.
            self.obj_descs.write().unwrap_or_else(|e| e.into_inner())[hu] = desc;
            self.publish_obj_bases(objects_base, None);
        } else {
            {
                let mut descs = self.obj_descs.write().unwrap_or_else(|e| e.into_inner());
                while descs.len() < hu {
                    descs.push(ObjDesc::empty());
                }
                descs.push(desc);
            }
            // Grew the Vec → it may have reallocated → republish both bases.
            let obj_desc_base = self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).as_ptr() as usize;
            self.publish_obj_bases(objects_base, Some(obj_desc_base));
        }
        // B3-step-3 (RAVA_PINNED_MIRROR, default OFF): dual-store this handle
        // object's field cells into its pinned mirror, at the class-layout slot (the
        // same slot the arena field-access path reads). Every cell is bridged through
        // the object model: `decode_field` → `Value` → `encode_arena_cell` → the
        // exact ARENA cell encoding — primitives raw, `Ref`→`(h<<2)|1`, `Array`→`|2`,
        // arena `Obj`→addr, `Str`/sentinels→boxed `(idx<<2)|3` — so a later read-flip
        // brick decodes a pinned object's field uniformly with the arena path. The
        // mirror is UNREAD here, so a mis-encode is caught by that brick's hash gate,
        // never silently. Default OFF ⇒ the whole block is skipped ⇒ canonical
        // bit-exact. (Box re-encode on every sync leaks arena_box entries under the
        // flag; acceptable for the dormant migration path, reclaimed by a later brick.)
        if mirror_all && crate::nextgen::pinned_mirror_enabled() {
            let mirror = crate::nextgen::pinned_mirror_get(h);
            if mirror != 0 {
                let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
                let o = g[hu]
                    .get()
                    .expect("descriptor sync requires a live object slot");
                if let Some(layout) = self.layouts.read().unwrap_or_else(|e| e.into_inner()).get(o.class.as_ref()).cloned() {
                    // O(1) id→slot index instead of a per-field O(layout_len)
                    // `position` scan — turns this per-object sync from O(n²)
                    // into O(n). Borrow it once outside the loop.
                    if let Some(idx) = self.layout_slot_index.read().unwrap_or_else(|e| e.into_inner()).get(o.class.as_ref()) {
                    for i in 0..o.fields.len() {
                        let (id, cell, kind) =
                            (o.fields.ids()[i], o.fields.cells()[i], o.fields.kinds()[i]);
                        if let Some(&slot) = idx.get(&id) {
                            let slot = slot as usize;
                            let v = self.decode_field(cell, kind);
                            // Kind-aware (LAYOUT kind): identity-boxed primitives in
                            // a ref slot must BOX, not flatten (encode_mirror_cell).
                            let arena_cell = Self::encode_mirror_cell(&v, layout[slot].1);
                            unsafe {
                                *((mirror as *mut u8).add(crate::nextgen::HDR + slot * 8)
                                    as *mut i64) = arena_cell;
                            }
                        }
                    }
                    }
                }
            }
        }
    }

    pub fn alloc(&self, class: &str) -> u32 {
        // POPULATION FOR THE PRICE. `bench/NativeCost.java` puts one object birth at ~105 ns
        // isolated; at 100 M births that would be a third of the boot, and nothing had ever
        // counted them. `RAVA_DIAG=mem`.
        crate::diag::births_handle().inc();
        let _t = crate::diag::births_handle_price().start();
        // ⭐**SIZE THE HANDLE POPULATION.** Rows 434-435 make retiring `Value::Ref` the next real
        // gain, and the whole question is HOW MANY objects still take a handle. The interpreted
        // `new` census already answers its half — 196,608 arena births against 165 handles, so
        // 99.92% of guest `new` is migrated. Everything still on a handle is born HERE instead,
        // through a native, and nothing counted it. Sampled per class (a lock on every object
        // birth is what once stopped a census boot from reaching `Done`), aggregate exact.
        HANDLE_BIRTHS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if handle_census() {
            let n = HANDLE_BIRTHS.load(std::sync::atomic::Ordering::Relaxed);
            if n % 256 == 0 {
                let mut g = handle_names().lock().unwrap_or_else(|e| e.into_inner());
                match g.get_mut(class) {
                    Some(c) => *c += 1,
                    None => {
                        g.insert(class.to_string(), 1);
                    }
                }
            }
        }
        if crate::m3heap::probe_on() {
            crate::m3heap::record_alloc(class);
        }
        if crate::allochisto::on() {
            crate::allochisto::record(class);
        }
        if class.is_empty() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            // First, then every 500k — so a post-Done leaking site is sampled, not
            // just the boot baseline. RAVA_TRACE_GT-gated (env read only on the
            // empty-class path, which is exactly what we're hunting).
            if (n == 0 || n % 500_000 == 0) && std::env::var("RAVA_TRACE_GT").is_ok() {
                eprintln!("[EMPTY-ALLOC] #{n} empty-class object alloc:\n{}",
                    std::backtrace::Backtrace::force_capture());
            }
        }
        let class = self.intern(class);
        self.bytes.fetch_add(48, std::sync::atomic::Ordering::Relaxed);
        let h = self.put_object(Object { class: class.clone(), fields: FieldBlob::new(), lambda: None });
        self.prepopulate_known(h, &class);
        // RESERVED-HANDLE OWNERSHIP: a handle allocated by a GIL-free worker inside its own
        // region is private to that worker until publish, which is exactly the claim
        // `cow_owned` needs in order to stop treating it as shared. Recorded only while
        // `gilfree()` and only under the flag, so the default path is untouched.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::cow_owned_handles() && crate::parallel::gilfree() {
            crate::parallel::owned_handle_note(h);
        }
        h
    }

    /// Pre-populate a freshly-allocated object's fixed layout slots if the class
    /// layout is known. No-op when layout-alloc is off or the layout hasn't been
    /// computed yet (then the legacy putfield-append path fills the object, and
    /// the const-slot getfield's id verify still keeps reads correct).
    pub(crate) fn prepopulate_known(&self, handle: u32, class: &str) {
        if !crate::layout_alloc_enabled() {
            return;
        }
        let known = self.layouts.read().unwrap_or_else(|e| e.into_inner()).get(class).cloned();
        if let Some(layout) = known {
            self.prepopulate_layout(handle, &layout);
            // B3-step-2 (RAVA_PINNED_MIRROR, default OFF): allocate a pinned header
            // mirror for this layout-known handle object, recording handle→pinned.
            // OFF by default so the pinned region stays unmapped and canonical is
            // bit-exact. B5-prep: the header carries the REAL arena-registered class
            // id (register_class dedups by name), so dispatch/class-of can resolve
            // from the pinned POINTER alone — the prerequisite for references to
            // handle objects becoming raw pinned pointers.
            if crate::nextgen::pinned_mirror_enabled() {
                let arc = self.intern(class);
                let cid = crate::nextgen::register_class(&arc, &layout);
                let p = crate::nextgen::pinned_object_alloc(cid, layout.len() as u32);
                crate::nextgen::pinned_mirror_set(handle, p);
            }
        }
    }

    /// Allocate an empty object whose class name is ALREADY interned — skips the
    /// per-alloc intern hashmap lookup (the per-`new`-site cache supplies the Arc).
    pub(crate) fn alloc_interned(&self, class: Arc<str>) -> u32 {
        // THE SECOND HANDLE MINTER. `Heap::alloc` records into `allochisto`; this did not, so an
        // object created only through here read as "NEVER heap-allocated" while the cell census
        // simultaneously attributed tens of thousands of blocking cells to its class. Two probes
        // and one blind spot between them. Record here too, so the histogram means what its name
        // says.
        if crate::allochisto::on() {
            crate::allochisto::record(&class);
        }
        self.bytes.fetch_add(48, std::sync::atomic::Ordering::Relaxed);
        let h = self.put_object(Object {
            class: class.clone(),
            fields: FieldBlob::new(),
            lambda: None,
        });
        self.prepopulate_known(h, &class);
        h
    }

    // ────────── PHASE 5 — HandleTable facade ──────────
    //
    // `alloc/alloc_interned/put_field` keep their `&mut self` forms — Rust has
    // no overloading and those have 117 call sites — so the shared entries
    // take the `_shared` suffix the tree already uses (`make_exception_shared`).
    // Only `bytes: AtomicUsize` already takes `&self`; `objects/obj_descs/
    // obj_alive/free_obj` remain plain `Vec`/`Mutex` and still need exclusivity
    // via `&mut Heap`. Until they move behind `RwLock<Vec>`/`Mutex<Vec>` the
    // ONE allowed unsafe is `as_mut_transitional` directly below.

    /// THE ONE PLACE the `&Heap -> &mut Heap` cast lives.
    ///
    /// Stage 3 lifted `bytes` to `AtomicUsize` (so a birth's accounting can
    /// `fetch_add` through `&self`), but `objects`, `obj_descs`, `obj_alive`
    /// and `free_obj` are still `Vec` and the helpers they feed into
    /// (`sync_obj_desc`, `prepopulate_known`, `put_object`'s `objects.push`)
    /// take `&mut Heap`. Until that table moves behind `RwLock`/`Mutex`
    /// this is the single chokepoint the two exception-body paths paper over.
    ///
    /// Stage 2c+alloc achieved:
    ///   · objects, obj_descs → RwLock<Vec<…>>
    ///   · obj_alive, free_obj → Mutex<Vec<…>>
    ///   · put_object, alloc, alloc_interned, put_field, sync_obj_desc all &self
    ///     (interior mutability + publish_obj_bases transitional cast for JitCtx)
    ///   · the `_shared`/`as_mut_transitional` forwarding wrappers are removed;
    ///     diag.rs calls the canonical `alloc`/`put_field` directly.

    /// Handle-table length on `&self`. Stage 3: `objects.len()` is still direct
    /// (`Vec`), but the call goes through the facade so Stage 4 only moves the field.
    #[allow(dead_code)] // dormant until the handle-migration wave wires in
    #[inline]
    pub(crate) fn objects_len(&self) -> usize {
        self.objects.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}


/// Every object born as a HEAP HANDLE (the native path). Always counted — one relaxed add on a
/// path that already allocates — and read by the periodic PC dump as `[PC-HANDLE]`.
pub(crate) static HANDLE_BIRTHS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn handle_census() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_HANDLE_CENSUS").is_some())
}

#[allow(clippy::type_complexity)]
fn handle_names() -> &'static std::sync::Mutex<crate::FastMap<String, u64>> {
    static M: std::sync::OnceLock<std::sync::Mutex<crate::FastMap<String, u64>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(crate::FastMap::default()))
}

/// Top handle-born classes, sampled 1-in-256 — the population that must move before
/// `Value::Ref` can be deleted.
pub(crate) fn handle_census_report(top: usize) -> String {
    let g = handle_names().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&String, &u64)> = g.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    let total: u64 = rows.iter().map(|(_, n)| **n).sum();
    let mut out = String::new();
    for (c, n) in rows.iter().take(top) {
        out.push_str(&format!(
            "
[PC-HANDLE]   {:>6.2}%  {:>8}  {c}",
            **n as f64 * 100.0 / total.max(1) as f64,
            **n
        ));
    }
    out
}
