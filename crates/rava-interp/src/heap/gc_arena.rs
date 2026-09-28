//! Finding arena roots and holders, and compacting the box tables after a move.

use crate::heap::*;

impl Heap {
    /// next-gen-gc — DANGLING SHIELD: every mutable location in the native
    /// collection layer that may hold an ARENA pointer. The moving collector roots
    /// + forwards these in place so an arena object referenced ONLY from a Java
    /// collection (HashMap entry, reference-array cell, or a boxed object field) is
    /// not reclaimed out from under it (which would leave a dangling pointer →
    /// garbage coordinates). Returns (Value slots, flat i64 reference-array cells);
    /// the caller keeps only the in-nursery arena pointers. Conservative (it visits
    /// not-yet-swept dead collections too) — correctness over precision.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn arena_root_locations(&mut self, young_only: bool) -> (Vec<*mut Value>, Vec<*mut i64>) {
        let mut vlocs: Vec<*mut Value> = Vec::new();
        let mut clocs: Vec<*mut i64> = Vec::new();
        // Collect ONLY cells that currently hold an arena pointer. The arena
        // collector forwards arena `Value::Obj` / in-arena raw pointers; a cell
        // holding an Int/Ref/Str/Null never moves, so skipping it is lossless. This
        // matters enormously at scale: during spawn-region worldgen the heap holds
        // tens of millions of map/array/field cells, almost all NON-arena — pushing
        // every one then filtering in the caller grew this temp Vec to 256MB+ and
        // the doubling realloc aborted the process (OOM in grow_one).
        //
        // `young_only` (a MINOR collection): a minor moves only nursery objects, so a
        // heap cell pointing into TENURED never needs forwarding — restrict to the
        // young window. Most chunk-storage block-state cells point to tenured (interned
        // block states), so this collapses the root set from O(heap) to O(young),
        // fixing both the OOM and the per-minor scan cost. (Arena-internal old→young
        // edges are handled by tlab_collect's own rem-set, not here.) A MAJOR keeps
        // every arena cell (old objects move too).
        let (ylo, yhi) = if young_only {
            crate::nextgen::tlab_young_window()
        } else {
            (0u64, 0u64)
        };
        // ⭐**ONE UNSIGNED COMPARE PER CELL, AND THE ATOMIC HOISTED OUT OF THE SCAN.**
        // `is_arena_obj_addr` reloads the relaxed atomic `NEXT_LOW` on EVERY call, and this
        // scan calls it once per map entry, once per reference-array CELL and once per
        // object field of the whole live heap — tens of millions of times per collection.
        // The cost of an atomic here is not the `mov`, it is that LLVM may not hoist it out
        // of the loop (the lesson that bought -15% on the dispatch push path). Nothing in
        // this function allocates an arena object and `NEXT_LOW` only ever grows, so one
        // snapshot taken here is bit-for-bit the value every per-cell call would have read.
        //
        // The two range tests then fold into one: `is_arena_obj_addr` is
        // `[ARENA_FLOOR, high_water)`, `young_only` intersects `[ylo, yhi)`, and the
        // intersection of two half-open ranges is a half-open range — testable with a
        // single wrapping subtract + compare, which also rejects 0/small ints and tagged
        // handles (all far below the 8 GiB floor) in the same instruction pair.
        let fast = fast_gc_enabled();
        let hw = crate::nextgen::arena_high_water();
        let lo = if young_only { ylo.max(crate::nextgen::ARENA_FLOOR) } else { crate::nextgen::ARENA_FLOOR };
        let hi = if young_only { yhi.min(hw) } else { hw };
        let span = hi.saturating_sub(lo); // empty intersection ⇒ 0 ⇒ nothing kept
        let keep = |addr: u64| -> bool {
            if fast {
                addr.wrapping_sub(lo) < span && addr & 7 == 0
            } else {
                crate::nextgen::is_arena_obj_addr(addr) && (!young_only || (addr >= ylo && addr < yhi))
            }
        };
        for m in self.maps.iter_mut() {
            for (k, v) in m.entries.iter_mut() {
                if matches!(k, Value::Obj(p) if keep(p.addr())) { vlocs.push(k as *mut Value); }
                if matches!(v, Value::Obj(p) if keep(p.addr())) { vlocs.push(v as *mut Value); }
            }
        }
        let mut arrays_g = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        for a in arrays_g.iter_mut() {
            if let ArrayData::Ref(cells) = &mut a.data {
                for c in cells.iter_mut() {
                    if keep(*c as u64) { clocs.push(c as *mut i64); }
                }
            }
        }
        // Per live handle object: forward both its FK_OBJ raw arena-pointer field
        // cells AND any arena `Value::Obj` captured by a closure it carries.
        // Stage 2c: the read guard in a `for`-header lives across the whole body (tested
        // with probe_forguard.rs). Converting line 83's `.read()[h].get_mut()` into
        // `.write()[h].get_mut()` would deadlock `std::sync::RwLock`. Hoist the length,
        // bind the obj_alive guard once, and take a WRITE guard inside the body so the
        // source read is released before the destination write begins.
        let n_objs = self.objects.read().unwrap_or_else(|e| e.into_inner()).len();
        let alive = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
        for h in 0..n_objs {
            if !alive.get(h).copied().unwrap_or(false) {
                continue;
            }
            // FK_OBJ object-field cells + lambda captures: written through a write guard
            // whose lifetime starts here and ends at the block close — the read guard on
            // `n_objs` is long gone.
            let mut g = self.objects.write().unwrap_or_else(|e| e.into_inner());
            let Some(o) = g[h].get_mut() else { continue };
            let n = o.fields.len();
            if n != 0 && fast {
                let kinds = o.fields.kinds_ptr();
                let cells = o.fields.cell_mut_ptr(0);
                for i in 0..n {
                    unsafe {
                        if *kinds.add(i) == FK_OBJ && keep(*cells.add(i) as u64) {
                            clocs.push(cells.add(i));
                        }
                    }
                }
            } else {
                for i in 0..n {
                    if o.fields.kinds()[i] == FK_OBJ && keep(o.fields.cells()[i] as u64) {
                        clocs.push(o.fields.cell_mut_ptr(i));
                    }
                }
            }
            // Lambda CAPTURED values: a closure that captured an arena `Value::Obj`
            // (e.g. the RegularImmutableMap built by `toImmutableMap` and captured by
            // Block.lambda$getShapeForEachState$13) must have that pointer FORWARDED
            // when the collector relocates it — else the captured map goes stale and
            // resolves to whatever reused its young slot (an IndirectMerger → ULE).
            if let Some(lam) = &mut o.lambda {
                for v in lam.captured.iter_mut() {
                    if matches!(v, Value::Obj(p) if keep(p.addr())) { vlocs.push(v as *mut Value); }
                }
            }
        }
        // Boxed object/array fields + boxed ref-array cells (a `Value::Obj` stored
        // into a handle object's field goes through `field_box`; a Str/boxed cell
        // through `array_box` — scan both, keeping only the arena objects).
        for v in self.field_box.write().unwrap_or_else(|e| e.into_inner()).iter_mut() {
            if matches!(v, Value::Obj(p) if keep(p.addr())) { vlocs.push(v as *mut Value); }
        }
        for v in self.array_box.write().unwrap_or_else(|e| e.into_inner()).iter_mut() {
            if matches!(v, Value::Obj(p) if keep(p.addr())) { vlocs.push(v as *mut Value); }
        }
        (vlocs, clocs)
    }

    /// Wall #19 forensic: who holds the raw arena pointer `addr`? Scan every heap
    /// object's FK_OBJ fields + lambda captures, every native map's entries, and
    /// every ref-array's cells, returning a human description of each holder. Run at
    /// a stale-receiver panic to name the un-forwarded location (the reference the
    /// collector never updated). Diagnostic only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn find_arena_holders(&self, addr: u64) -> Vec<String> {
        let mut out = Vec::new();
        for (h, slot) in self.objects.read().unwrap_or_else(|e| e.into_inner()).iter().enumerate() {
            let live = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).get(h).copied().unwrap_or(false);
            let Some(o) = slot.get() else { continue };
            let (ids, cells, kinds) = (o.fields.ids(), o.fields.cells(), o.fields.kinds());
            for i in 0..cells.len() {
                if kinds.get(i) == Some(&FK_OBJ) && cells[i] as u64 == addr {
                    let fid = ids.get(i).copied().unwrap_or(0);
                    let fname = self.field_names_r().get(fid as usize).map(|s| s.to_string()).unwrap_or_else(|| "?".to_string());
                    out.push(format!("object#{h} class={} field={fname}(id={fid}) FK_OBJ alive={live}", o.class));
                }
            }
            if let Some(lam) = &o.lambda {
                for (ci, v) in lam.captured.iter().enumerate() {
                    if matches!(v, Value::Obj(p) if p.addr() == addr) {
                        out.push(format!("object#{h} class={} lambda.captured[{ci}] alive={live}", o.class));
                    }
                }
            }
        }
        for (id, m) in self.maps.iter().enumerate() {
            for (ei, (k, v)) in m.entries.iter().enumerate() {
                if matches!(k, Value::Obj(p) if p.addr() == addr) {
                    out.push(format!("map#{id} entry[{ei}].KEY"));
                }
                if matches!(v, Value::Obj(p) if p.addr() == addr) {
                    out.push(format!("map#{id} entry[{ei}].val"));
                }
            }
        }
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        for (h, a) in arrays.iter().enumerate() {
            if let ArrayData::Ref(cells) = &a.data {
                for (i, &c) in cells.iter().enumerate() {
                    if c as u64 == addr && c != 0 && c & 7 == 0 {
                        let element = self
                            .array_descs
                            .read().unwrap_or_else(|e| e.into_inner())
                            .get(h)
                            .and_then(|desc| self.array_element_name(desc.element_id))
                            .unwrap_or_else(|| std::sync::Arc::from("<invalid>"));
                        out.push(format!("array#{h} element={element} cell[{i}]"));
                    }
                }
            }
        }
        // Arena OBJECT ref cells (e.g. Holder$Direct.value) — the arena-field holders the
        // heap-object/map/array scans above miss. Names the arena object whose ref cell still
        // retains the stale address = the worldgen Holder → DensityFunction strand.
        #[cfg(feature = "next-gen-gc")]
        {
            let ncls = crate::nextgen::registry_len();
            crate::nextgen::arena_for_each_obj(&mut |p| {
                crate::nextgen::arena_trace_refs(p, ncls, &mut |_h| {}, &mut |child| {
                    if child as u64 == addr {
                        let cid = unsafe { (*(p as *const u64) & 0xFFFF_FFFF) as u32 };
                        out.push(format!(
                            "arena-obj@{:#x} class={:?} ref-cell -> addr",
                            p as usize,
                            crate::nextgen::class_name(cid)
                        ));
                    }
                });
            });
        }
        out
    }

    /// Compact `array_box`/`field_box`: keep only entries still addressed by a
    /// live ref-array `ABOX` cell or live object `FK_BOX` field, renumber them,
    /// and rewrite the holding cells. Safe because nothing outside heap cells
    /// holds a box index (operand stack/locals carry decoded `Value`s), and box
    /// entries never reference heap objects. Returns an estimate of bytes freed.
    ///
    /// ⭐**ONE FULL-HEAP WALK PER TABLE, NOT TWO.** This pass ran discovery over every
    /// marked array (and every field of every marked object), then ran the IDENTICAL walk
    /// again to rewrite the cells — so a collection paid two complete traversals of the
    /// heap's cells for tables that are usually a rounding error of it. The discovery pass
    /// already knows which handles hold a box cell, so it now records them and the rewrite
    /// visits only those. Where boxes are sparse — the normal case, since they exist only
    /// where autoboxing put an Integer/Str into a reference slot — the second walk
    /// collapses from O(all cells in the heap) to O(cells of the few holders).
    ///
    /// The NUMBERING is unchanged: `order` is filled in exactly the first-visit sequence
    /// the old `fresh.push` inside `or_insert_with` used, so the rebuilt table is
    /// byte-identical and every rewritten cell receives the index it received before.
    ///
    /// RETAINED SCRATCH SIZE: `remap` 8 B per live box entry, `order` 4 B per live entry,
    /// `holders` 4 B per holding handle. Proportional to the box tables, not to the heap,
    /// and streamed rather than probed — the L1 rule for caches does not apply.
    pub(crate) fn compact_box_tables(&mut self, mo: &MarkBits, ma: &MarkBits) -> usize {
        let (old_ab, old_fb) = (self.array_box.read().unwrap_or_else(|e| e.into_inner()).len(), self.field_box.read().unwrap_or_else(|e| e.into_inner()).len());
        if old_ab == 0 && old_fb == 0 {
            return 0;
        }
        let fast = fast_gc_enabled();
        let mut scratch = if fast { box_scratch_take() } else { BoxCompactScratch::default() };
        let BoxCompactScratch { remap, order, holders, fresh } = &mut scratch;
        let is_box = is_box_cell;
        if old_ab != 0 {
            // `FastMap` (FxHasher), not the std default. This is rebuilt on EVERY collection and
            // walked over every marked array/object, and its key is a `u32` — SipHash on a
            // 4-byte integer key is pure ceremony. Profiling the codec/recipe phase (ledger row
            // 202) puts `std::hash::random::write` at 3.06% with `Heap::gc` up to 6.25%.
            //
            // Iteration order is not observable here: the map only answers "old index -> new",
            // and the new order is fixed by `order`, which is filled in traversal order.
            remap.clear();
            order.clear();
            holders.clear();
            let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
            for h in 0..arrays.len() {
                if !ma.get(h) {
                    continue;
                }
                let ArrayData::Ref(cells) = &arrays[h].data else { continue };
                let mut holds = false;
                for &cell in cells.iter().filter(|&&c| is_box(c)) {
                    holds = true;
                    let old = (cell & 0xFFFF_FFFF) as u32;
                    remap.entry(old).or_insert_with(|| {
                        let n = order.len() as u32;
                        order.push(old);
                        n
                    });
                }
                if holds {
                    holders.push(h as u32);
                }
            }
            drop(arrays);
            fresh.clear();
            fresh.reserve(order.len());
            for &old in order.iter() {
                fresh.push(self.array_box.read().unwrap_or_else(|e| e.into_inner())[old as usize].clone());
            }
            if fast {
                for k in 0..holders.len() {
                    let h = holders[k] as usize;
                    if let ArrayData::Ref(cells) = &mut self.arrays.write().unwrap_or_else(|e| e.into_inner())[h].data {
                        for cell in cells.iter_mut().filter(|c| is_box(**c)) {
                            *cell = remap[&((*cell & 0xFFFF_FFFF) as u32)] as i64 | ABOX_TAG;
                        }
                    }
                }
            } else {
                for h in 0..self.arrays.read().unwrap_or_else(|e| e.into_inner()).len() {
                    if ma.get(h) {
                        if let ArrayData::Ref(cells) = &mut self.arrays.write().unwrap_or_else(|e| e.into_inner())[h].data {
                            for cell in cells.iter_mut().filter(|c| is_box(**c)) {
                                *cell = remap[&((*cell & 0xFFFF_FFFF) as u32)] as i64 | ABOX_TAG;
                            }
                        }
                    }
                }
            }
            // Swap rather than assign: `fresh` keeps its capacity for the next collection
            // and carries the OLD table out, whose contents `box_scratch_put` drops exactly
            // where `self.array_box = fresh` used to drop them.
            std::mem::swap(&mut *self.array_box.write().unwrap_or_else(|e| e.into_inner()), fresh);
        }
        if old_fb != 0 {
            // Same FxHasher argument as above, same first-visit numbering, same holder list.
            remap.clear();
            order.clear();
            holders.clear();
            for h in 0..self.objects.read().unwrap_or_else(|e| e.into_inner()).len() {
                if !mo.get(h) {
                    continue;
                }
                let Some(o) = self.get(h as u32) else { continue };
                let (cells, kinds) = (o.fields.cells(), o.fields.kinds());
                let mut holds = false;
                for i in 0..kinds.len() {
                    if kinds[i] == FK_BOX {
                        holds = true;
                        let old = (cells[i] & 0xFFFF_FFFF) as u32;
                        remap.entry(old).or_insert_with(|| {
                            let n = order.len() as u32;
                            order.push(old);
                            n
                        });
                    }
                }
                if holds {
                    holders.push(h as u32);
                }
            }
            fresh.clear();
            fresh.reserve(order.len());
            for &old in order.iter() {
                fresh.push(self.field_box.read().unwrap_or_else(|e| e.into_inner())[old as usize].clone());
            }
            if fast {
                let mut g = self.objects.write().unwrap_or_else(|e| e.into_inner());
                for k in 0..holders.len() {
                    let h = holders[k] as usize;
                    let Some(o) = g[h].get_mut() else { continue };
                    let n = o.fields.len();
                    for i in 0..n {
                        if o.fields.kinds()[i] == FK_BOX {
                            let old = (o.fields.cells()[i] & 0xFFFF_FFFF) as u32;
                            o.fields.cells_mut()[i] = remap[&old] as i64 | ABOX_TAG;
                        }
                    }
                }
            } else {
                let n_objs = self.objects.read().unwrap_or_else(|e| e.into_inner()).len();
                for h in 0..n_objs {
                    if mo.get(h) {
                        let mut g = self.objects.write().unwrap_or_else(|e| e.into_inner());
                        let Some(o) = g[h].get_mut() else { continue };
                        let n = o.fields.len();
                        for i in 0..n {
                            if o.fields.kinds()[i] == FK_BOX {
                                let old = (o.fields.cells()[i] & 0xFFFF_FFFF) as u32;
                                o.fields.cells_mut()[i] = remap[&old] as i64 | ABOX_TAG;
                            }
                        }
                    }
                }
            }
            std::mem::swap(&mut *self.field_box.write().unwrap_or_else(|e| e.into_inner()), fresh);
        }
        let saved = (old_ab - self.array_box.read().unwrap_or_else(|e| e.into_inner()).len()) * 24 + (old_fb - self.field_box.read().unwrap_or_else(|e| e.into_inner()).len()) * 24;
        if fast {
            box_scratch_put(scratch);
        }
        saved
    }
}
