//! Counting what the heap holds: histograms, composition, and the census dump.

use crate::heap::*;

impl Heap {
    /// DIAGNOSTIC: the RUST-side footprint of the object/array tables, which the arena GC
    /// cannot reach. Measured 2026-07-17: the boot peaks at 2200MB RSS vs HotSpot's 544MB
    /// (4.04×) and the OOM is a RUST allocation failure — while forcing arena MAJORs at
    /// 400MB changed peak RSS by 0MB, proving the bytes are NOT in the guest arena. This
    /// reports where they actually are. Per heap object we pay: one Box, an Arc<str> class,
    /// and three SmallVec<[_; 4]> (ids/cells/kinds — inline up to 4 fields, heap-spilled
    /// beyond), plus a lockstep ObjDesc — versus HotSpot's single packed object.
    /// Returns (objects, arrays, estimated_bytes).
    /// (live objects, dead slots) in the handle table. A freed slot still costs a full
    /// `Box::new(Object{..})` (heap.rs sweep) and the Vec never shrinks — so "dead slots"
    /// is a real memory number, not bookkeeping. Verifies the premise before any refactor:
    /// the top-12 class histogram alone cannot tell live from dead.
    pub fn live_dead_counts(&self) -> (usize, usize) {
        let mut live = 0usize;
        let alive = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
        let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
        for (h, slot) in g.iter().enumerate() {
            let dead = (h < alive.len() && !alive[h])
                || slot.get().is_none_or(|o| o.class.is_empty());
            if !dead {
                live += 1;
            }
        }
        let total = g.len();
        drop(g);
        drop(alive);
        (live, total - live)
    }

    pub fn rust_side_footprint(&self) -> (usize, usize, usize) {
        let mut est = self.objects.read().unwrap_or_else(|e| e.into_inner()).len() * std::mem::size_of::<ObjectSlot>();
        est += self.objects.read().unwrap_or_else(|e| e.into_inner()).len() * std::mem::size_of::<ObjDesc>();
        // A nullable dead slot owns no `Object`; live/fallback slots own the struct
        // allocation plus any non-empty FieldBlob allocation.
        for o in self.objects.read().unwrap_or_else(|e| e.into_inner()).iter().filter_map(ObjectSlot::get) {
            est += std::mem::size_of::<Object>() + o.fields.blob_alloc_bytes();
        }
        let arrays_g = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let descs_g = self.array_descs.read().unwrap_or_else(|e| e.into_inner());
        est += arrays_g.len() * std::mem::size_of::<ArrayObj>();
        est += descs_g.len() * std::mem::size_of::<ArrayDesc>();
        (self.objects.read().unwrap_or_else(|e| e.into_inner()).len(), arrays_g.len(), est)
    }

    /// DIAGNOSTIC: which CLASSES make up the live Rust-side heap objects, by estimated bytes.
    /// GC scheduling is refuted as an RSS lever (3× measured: arena-major threshold, sweep
    /// floor, sweep head-room all moved peak RSS by 0MB) — the peak is LIVE data, so the only
    /// way forward is to shrink or relocate whatever is actually live. That requires knowing
    /// WHAT is live: bytecode classes already allocate flat into the arena, so these 3.64M
    /// survivors are the handle-backed residue (collections/maps, enums, native-super
    /// subclasses, bytecode-less JDK classes). Returns the top `n` by bytes.
    pub fn live_class_histogram(&self, n: usize) -> Vec<(String, usize, usize)> {
        use std::collections::HashMap;
        let mut by: HashMap<&str, (usize, usize)> = HashMap::new();
        let per_obj = std::mem::size_of::<Object>()
            + std::mem::size_of::<Box<Object>>()
            + std::mem::size_of::<ObjDesc>();
        let alive = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
        let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
        for (h, slot) in g.iter().enumerate() {
            if h < alive.len() && !alive[h] {
                continue;
            }
            let Some(o) = slot.get() else { continue };
            if o.class.is_empty() {
                continue;
            }
            let spill = o.fields.blob_alloc_bytes();
            let e = by.entry(&o.class).or_insert((0, 0));
            e.0 += 1;
            e.1 += per_obj + spill;
        }
        let mut v: Vec<(String, usize, usize)> =
            by.into_iter().map(|(c, (n, b))| (c.to_string(), n, b)).collect();
        v.sort_by(|a, b| b.2.cmp(&a.2));
        v.truncate(n);
        v
    }

    /// DIAGNOSTIC (RAVA_CENSUS): a COMPONENT-level breakdown of the live Rust-side heap.
    ///
    /// Why this exists when `rust_side_footprint` + `live_class_histogram` already do:
    /// neither can settle the question the memory arc actually turns on. `footprint` counts
    /// every SLOT (dead included) and ESTIMATES spill from `len`; `live_class_histogram`
    /// attributes bytes per CLASS but not per COMPONENT. So both would report the same total
    /// whether the bytes sit in the per-object header or in `MapObj.index` — and those two
    /// answers point at opposite refactors. Moving the 3.64M survivors to the arena can only
    /// ever reclaim the HEADER; if the mass is really in map indices or `ArrayData` payload,
    /// that refactor is refuted before a line of it is written. This arc has already burned
    /// three hypotheses (GC scheduling ×3, handle Option-compaction, inline-threshold) on
    /// unmeasured premises; this is the measurement that gates the fourth.
    ///
    /// EXACT, not estimated: `SmallVec::spilled()/capacity()` and `Vec::capacity()` report
    /// real allocations, so these are allocator-true. The one modelled number is `ALLOC_TAX`
    /// (per-allocation malloc header+rounding), reported as its own line so it can be
    /// subtracted. O(objects + arrays + maps); only ever called off the hot path.
    pub fn census(&self) -> HeapCensus {
        let mut c = HeapCensus::default();
        let live_of = |alive: &[bool], h: usize| h >= alive.len() || alive[h];
        // Unique interned LambdaSite headers, deduped by Arc pointer identity (a site is shared
        // by every instantiation of its call-site, so counting it per lambda would overstate).
        let mut lambda_sites: std::collections::HashSet<usize> = std::collections::HashSet::new();
        // Duplicate-capture estimate: (site ptr, capture signature) → (count, captured bytes of one).
        let mut lambda_dup: std::collections::HashMap<(usize, u64), (usize, usize)> =
            std::collections::HashMap::new();

        let alive_g = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
        let objs_g = self.objects.read().unwrap_or_else(|e| e.into_inner());
        c.obj_slots = objs_g.len();
        for (h, slot) in objs_g.iter().enumerate() {
            let object = slot.get();
            let dead = !live_of(&alive_g, h)
                || object.is_none_or(|o| o.class.is_empty());
            if dead {
                c.dead_slots += 1;
                if object.is_some() {
                    c.dead_struct += std::mem::size_of::<Object>() + ALLOC_TAX;
                }
                continue;
            }
            let o = object.expect("live object slot has storage");
            c.live_objects += 1;
            // FieldBlob: ONE allocation per non-empty object (the three SmallVec inline
            // headers are gone — Object is now 32B). A zero-field object (lambda, freshly
            // alloc'd) points at the shared sentinel and costs NOTHING here. `alloc_size(cap)`
            // is the real block; ALLOC_TAX its malloc overhead.
            let blob_bytes = o.fields.blob_alloc_bytes();
            if blob_bytes > 0 {
                c.field_blob_bytes += blob_bytes + ALLOC_TAX;
                c.spilled_objects += 1;
            }
            c.field_slots += o.fields.len();
            if let Some(l) = &o.lambda {
                c.lambda_count += 1;
                // Per-instance cost only: the Lambda struct (its `site` is an 8B Arc ptr
                // INSIDE size_of) + its Box tax + the captured Vec. The owner/name/descriptor
                // bytes are SHARED through the intern pool now, so charging them per lambda
                // would double-count — the unique `LambdaSite` headers are tallied once below
                // (via Arc-pointer dedup) and the interned strings live under `intern tables`.
                let cap_bytes = l.captured.capacity() * std::mem::size_of::<Value>();
                c.lambda_bytes += std::mem::size_of::<Lambda>() + ALLOC_TAX + cap_bytes;
                if lambda_sites.insert(Arc::as_ptr(&l.site) as usize) {
                    c.lambda_site_bytes += std::mem::size_of::<LambdaSite>() + ALLOC_TAX;
                }
                // Captured-context decomposition.
                let ncap = l.captured.len();
                let cap_alloc = if l.captured.capacity() > 0 { cap_bytes + ALLOC_TAX } else { 0 };
                c.lambda_captured_bytes += cap_alloc;
                let hb = match ncap {
                    0 => 0,
                    1 => 1,
                    2..=4 => 2,
                    _ => 3,
                };
                c.lambda_cap_hist[hb].0 += 1;
                c.lambda_cap_hist[hb].1 += cap_alloc;
                if ncap > 0 {
                    c.lambda_capturing += 1;
                    let key = (Arc::as_ptr(&l.site) as usize, capture_sig(&l.captured));
                    let e = lambda_dup.entry(key).or_insert((0, cap_alloc));
                    e.0 += 1;
                }
            }
        }
        // Duplicate-capture rollup: distinct (site, capture) signatures, and the bytes that DEDUP
        // would reclaim (each group past its first instance is a redundant identical closure).
        c.lambda_capture_distinct = lambda_dup.len();
        for (_, (count, bytes)) in &lambda_dup {
            if *count > 1 {
                c.lambda_dup_bytes += (*count - 1) * *bytes;
            }
        }
        // Per-live-object fixed cost: the struct itself + its own Box allocation.
        c.obj_struct = c.live_objects * std::mem::size_of::<Object>();
        c.obj_box_tax = c.live_objects * ALLOC_TAX;
        // Tables sized by SLOT count (dead slots still cost their Vec entry).
        c.obj_table = self.objects.read().unwrap_or_else(|e| e.into_inner()).capacity() * std::mem::size_of::<ObjectSlot>();
        c.obj_desc_table = self.obj_descs.read().unwrap_or_else(|e| e.into_inner()).capacity() * std::mem::size_of::<ObjDesc>();

        // ── Maps: entries Vec + index HashMap, the FieldBlob-vs-arena tiebreaker ──
        c.map_slots = self.maps.len();
        for (h, m) in self.maps.iter().enumerate() {
            if !live_of(&self.map_alive, h) {
                continue;
            }
            c.live_maps += 1;
            let (len, ecap) = (m.entries.len(), m.entries.capacity());
            c.map_entry_count += len;
            if ecap > 0 {
                c.map_entries_bytes += ecap * std::mem::size_of::<(Value, Value)>() + ALLOC_TAX;
            }
            let idx_bytes = hashbrown_alloc_bytes::<MapKey, usize>(m.index.capacity());
            c.map_index_bytes += idx_bytes;
            for k in m.index.keys() {
                if let MapKey::Str(s) = k {
                    c.map_key_str_bytes += s.len() + ALLOC_TAX;
                }
            }
            // Decomposition: entries Vec slack + index usable-slot capacity + fill histogram.
            c.map_entry_cap += ecap;
            c.map_entry_slack_bytes += ecap.saturating_sub(len) * std::mem::size_of::<(Value, Value)>();
            c.map_index_slots += m.index.capacity();
            let fb = match len {
                0 => 0,
                1 => 1,
                2..=4 => 2,
                5..=8 => 3,
                _ => 4,
            };
            c.map_fill_hist[fb].0 += 1;
            c.map_fill_hist[fb].1 += idx_bytes + if ecap > 0 { ecap * std::mem::size_of::<(Value, Value)>() + ALLOC_TAX } else { 0 };
        }

        // ── Arrays: the REAL payload, per element kind ──
        let arrays_g = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let arr_alive_g = self.arr_alive.read().unwrap_or_else(|e| e.into_inner());
        c.arr_slots = arrays_g.len();
        for (h, a) in arrays_g.iter().enumerate() {
            if !live_of(&arr_alive_g, h) {
                continue;
            }
            c.live_arrays += 1;
            let b = a.data.alloc_bytes();
            let k = a.data.kind_idx();
            c.arr_payload[k].0 += 1;
            c.arr_payload[k].1 += b;
            c.arr_payload_bytes += b;
            let n = a.data.len();
            if n == 0 {
                c.arr_le0_count += 1;
            }
            if n <= 8 {
                c.arr_le8_count += 1;
                c.arr_le8_payload += b;
            }
            // REF-vs-PRIM decomposition: (count, alloc, used, unused-capacity).
            let esz = a.data.elem_bytes();
            let cap = a.data.capacity();
            let used = n * esz;
            let unused = cap.saturating_sub(n) * esz;
            let bucket = if a.data.is_ref() { &mut c.arr_ref } else { &mut c.arr_prim };
            bucket.0 += 1;
            bucket.1 += b;
            bucket.2 += used;
            bucket.3 += unused;
            // Length histogram: 0 / 1..=4 / 5..=8 / 9..=64 / >64.
            let hb = match n {
                0 => 0,
                1..=4 => 1,
                5..=8 => 2,
                9..=64 => 3,
                _ => 4,
            };
            c.arr_len_hist[hb].0 += 1;
            c.arr_len_hist[hb].1 += b;
        }
        c.arr_obj_table = arrays_g.capacity() * std::mem::size_of::<ArrayObj>();
        c.arr_desc_table = self.array_descs.read().unwrap_or_else(|e| e.into_inner()).capacity() * std::mem::size_of::<ArrayDesc>();

        // ── Append-only side tables (documented bounded leaks) ──
        c.field_box_bytes = self.field_box.read().unwrap_or_else(|e| e.into_inner()).capacity() * std::mem::size_of::<Value>();
        c.array_box_bytes = self.array_box.read().unwrap_or_else(|e| e.into_inner()).capacity() * std::mem::size_of::<Value>();
        let free_arr_g = self.free_arr.lock().unwrap_or_else(|e| e.into_inner());
        c.free_handles = [
            (self.free_obj.lock().unwrap_or_else(|e| e.into_inner()).len(), self.free_obj.lock().unwrap_or_else(|e| e.into_inner()).capacity()),
            (free_arr_g.len(), free_arr_g.capacity()),
            (self.free_map.len(), self.free_map.capacity()),
        ];
        c.free_handle_bytes = c
            .free_handles
            .iter()
            .map(|(_, cap)| cap * std::mem::size_of::<u32>())
            .sum();

        // ── Interning tables ──
        c.intern_bytes = self.field_names_r().iter().map(|n| n.len() + ALLOC_TAX).sum::<usize>()
            + self.class_pool.read().unwrap_or_else(|e| e.into_inner()).len() * (std::mem::size_of::<Arc<str>>() + 32)
            + self.field_hints.capacity() * 4
            + self.static_cells.capacity() * 8;

        c.total = c.obj_struct
            + c.obj_box_tax
            + c.obj_table
            + c.obj_desc_table
            + c.dead_struct
            + c.field_blob_bytes
            + c.lambda_bytes
            + c.lambda_site_bytes
            + c.map_entries_bytes
            + c.map_index_bytes
            + c.map_key_str_bytes
            + c.arr_payload_bytes
            + c.arr_obj_table
            + c.arr_desc_table
            + c.field_box_bytes
            + c.array_box_bytes
            + c.free_handle_bytes
            + c.intern_bytes;
        c
    }

    /// Rough live-byte estimate for the optional `RAVA_XMX_MB` cap. O(1): a
    /// running total bumped at each allocation (monotonic — we have no GC yet).
    pub fn approx_bytes(&self) -> usize {
        self.bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// (field_box, array_box) entry counts — these side tables are append-only
    /// (Str/boxed values), so they grow unbounded and are a memory-leak suspect.
    pub fn box_table_len(&self) -> (usize, usize) {
        (self.field_box.read().unwrap_or_else(|e| e.into_inner()).len(), self.array_box.read().unwrap_or_else(|e| e.into_inner()).len())
    }

    /// A heap-composition report (top object classes by count, array bytes by
    /// element type, totals) — for diagnosing where memory goes. O(heap), only
    /// called at the OOM abort, never on the hot path.
    pub fn composition(&self) -> String {
        let mut by_class: HashMap<&str, (usize, usize)> = HashMap::new();
        let alive = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
        let g = self.objects.read().unwrap_or_else(|e| e.into_inner());
        for (h, slot) in g.iter().enumerate() {
            // Skip swept tombstones (empty class, not alive) — they're free slots
            // whose bytes are already reclaimed; counting them hides the real leak.
            if h < alive.len() && !alive[h] {
                continue;
            }
            let Some(o) = slot.get() else { continue };
            if o.class.is_empty() {
                continue;
            }
            let e = by_class.entry(&o.class).or_insert((0, 0));
            e.0 += 1;
            e.1 += 48 + o.fields.len() * 13;
        }
        let mut classes: Vec<(&str, (usize, usize))> = by_class.into_iter().collect();
        classes.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
        let mut arr_bytes: HashMap<Arc<str>, (usize, usize)> = HashMap::new();
        let arrays_g = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let arr_alive_g = self.arr_alive.read().unwrap_or_else(|e| e.into_inner());
        let descs_g = self.array_descs.read().unwrap_or_else(|e| e.into_inner());
        for (h, a) in arrays_g.iter().enumerate() {
            if h < arr_alive_g.len() && !arr_alive_g[h] {
                continue;
            }
            let element = descs_g
                .get(h)
                .and_then(|desc| self.array_element_name(desc.element_id))
                .unwrap_or_else(|| "<invalid>".into());
            let e = arr_bytes.entry(element).or_insert((0, 0));
            e.0 += 1;
            e.1 += 32 + a.data.len() * a.data.elem_bytes();
        }
        drop(arrays_g);
        drop(arr_alive_g);
        drop(descs_g);
        let mut arrs: Vec<(Arc<str>, (usize, usize))> = arr_bytes.into_iter().collect();
        arrs.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
        let mb = |b: usize| b / (1024 * 1024);
        let map_entries: usize = self.maps.iter().map(MapObj::len).sum();
        let mut s = format!(
            "[HEAP] {} objects, {} arrays, {} maps ({} entries), ~{} MB total\n",
            self.objects.read().unwrap_or_else(|e| e.into_inner()).len(), self.arrays.read().unwrap_or_else(|e| e.into_inner()).len(), self.maps.len(), map_entries, mb(self.bytes.load(std::sync::atomic::Ordering::Relaxed)),
        );
        s.push_str("  top object classes (count, MB):\n");
        for (c, (n, b)) in classes.iter().take(12) {
            s.push_str(&format!("    {:>10} {:>5}MB  {}\n", n, mb(*b), c));
        }
        s.push_str("  array bytes by element (count, MB):\n");
        for (el, (n, b)) in arrs.iter().take(10) {
            s.push_str(&format!("    {:>10} {:>5}MB  {}\n", n, mb(*b), el));
        }
        s
    }

    /// M3 9.2c engine-mutation probe: total heap-handle allocation count (objects + arrays + maps).
    /// A growth here between two initCache calls = a shared `Vec` push (realloc/move) — a data-race
    /// hazard for a `*const Vm` shared read. Read-only accessor.
    pub(crate) fn heap_alloc_count(&self) -> usize {
        self.objects.read().unwrap_or_else(|e| e.into_inner()).len() + self.arrays.read().unwrap_or_else(|e| e.into_inner()).len() + self.maps.len()
    }

    /// Raw bytes of a `byte[]` (for IO natives).
    ///
    /// ⭐**THE GENERAL ARM RE-DISPATCHED PER BYTE.** WAS: one switch over the six `ArrayData`
    /// storage kinds, a bounds check, an `Option`, a `Value` and a second switch (`as_int`)
    /// *per element* — plumbing that grows with the SIZE OF THE ARRAY to answer a question
    /// (which storage kind?) that is a constant of the array. This is the whole-array IO entry
    /// point — the world save, resource reads, digests, channel writes all land here — so the
    /// per-element cost rode on every byte moved. IS: `ArrayData::to_bytes`, one switch for the
    /// whole array and then a `memcpy` / tight truncating loop / `memset`.
    ///
    /// One gate read PER ARRAY, not per element; `RAVA_NO_FAST_ARRAYDATA=1` restores the old
    /// per-element walk verbatim for an A/B. The two produce identical bytes, zero arms
    /// included — see the ⚠note on `to_bytes` for why the zeros are the OLD behaviour.
    pub fn array_bytes(&self, handle: u32) -> Option<Vec<u8>> {
        let g = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let data = &g.get(handle as usize)?.data;
        if !super::arraydata::fast_arraydata() {
            return Some(match data {
                ArrayData::Byte(v) => v.clone(),
                other => (0..other.len()).map(|i| other.get(i).and_then(|v| v.as_int()).unwrap_or(0) as u8).collect(),
            });
        }
        Some(data.to_bytes())
    }

    /// Diagnostic: the object's (field-id, kind, cell) triples with resolved
    /// names where the interner knows them.
    pub fn debug_field_ids(&self, handle: u32) -> Vec<(u32, u8, i64, String)> {
        let Some(o) = self.get(handle) else {
            return Vec::new();
        };
        o.fields
            .ids()
            .iter()
            .zip(o.fields.kinds())
            .zip(o.fields.cells())
            .map(|((&id, &k), &c)| {
                let name = self
                    .field_ids_r()
                    .iter()
                    .find(|(_, &v)| v == id)
                    .map(|(n, _)| n.to_string())
                    .unwrap_or_default();
                (id, k, c, name)
            })
            .collect()
    }
}
