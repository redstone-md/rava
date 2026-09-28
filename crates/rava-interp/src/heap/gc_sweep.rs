//! The collector proper, with its object-slot policy.

use super::gc::fast_gcpause2_enabled;
use crate::heap::*;

/// Packs the low bit of eight consecutive bytes into eight consecutive bits.
///
/// For a little-endian `u64` whose byte `j` is 0 or 1 — which is exactly what a `Vec<bool>`
/// holds, `bool` having no other valid bit patterns — `(x * THIS) >> 56` is byte `j`'s value
/// at bit `j`. The magic is `sum(1 << (56 - 7*j))`: byte `j` sits at bit `8*j`, so the term
/// `2^(56-7j)` lands it at bit `56+j`. Every cross term `8j + 56 - 7k` with `j != k` is
/// provably below bit 56 (`8j - 7k = m` in `0..8` forces `j == k`), and those low positions
/// are pairwise distinct, so nothing carries into the byte we read out.
const BOOL_BYTES_TO_BITS: u64 = 0x0102_0408_1020_4080;

/// The liveness of slots `base..end` as a 64-bit mask, bit `i` = slot `base + i`.
///
/// ⭐**THE SWEEP'S COST WAS O(TABLE), NOT O(GARBAGE).** The sweep already skipped a run of
/// 64 slots whose MARK word was all-ones, which covers the late-boot case where almost
/// everything is live. It had no answer for the opposite end — the free-list runs and the
/// never-yet-issued tail, whose mark word is all-ZEROES — and there it still read one
/// `Vec<bool>` byte per slot to learn that there was nothing to retire. Reading liveness as
/// a WORD lets the sweep test `alive & !mark == 0` and step over the run in both directions,
/// and lets a mixed run visit only the bits that are actually being retired instead of all
/// 64. Eight `u64` loads plus eight multiplies replace 64 byte loads.
///
/// Slots at or past `alive.len()` read as dead. That range cannot exist — every table is
/// grown in lockstep with its liveness vector, which is why the byte-at-a-time form could
/// index it with a panicking `[]` — but a clamp here is free and turns a would-be panic into
/// the same verdict the rest of the heap already uses for a short liveness vector.
#[inline]
fn alive_word(alive: &[bool], base: usize, end: usize) -> u64 {
    let hi = end.min(alive.len());
    if hi <= base {
        return 0;
    }
    // SAFETY: `bool` is one byte with alignment 1 and only ever holds 0 or 1, so the region
    // is a valid `[u8]` of the same length; `base < hi <= alive.len()` bounds the offset.
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(alive.as_ptr().add(base).cast::<u8>(), hi - base) };
    let mut mask = 0u64;
    let mut i = 0usize;
    while i + 8 <= bytes.len() {
        let eight = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        mask |= ((eight.wrapping_mul(BOOL_BYTES_TO_BITS)) >> 56) << i;
        i += 8;
    }
    while i < bytes.len() {
        mask |= (bytes[i] as u64) << i;
        i += 1;
    }
    mask
}

impl Heap {
    /// Retire one dead array slot: free its payload, invalidate the flat descriptor so a
    /// stale access faults on bounds rather than reading freed memory, and return the handle
    /// to the free list. Extracted verbatim from the sweep so the word-stepping arm and the
    /// bisection arm cannot drift apart. Does NOT bump the handle epoch — see the single
    /// coalesced bump at the end of the sweep.
    #[inline]
    fn retire_array_slot(&mut self, h: usize) -> usize {
        let freed = {
            let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
            let a = &arrays[h];
            32 + a.data.len() * a.data.elem_bytes()
        };
        // array_pool_retire takes its own arrays.write() guard — must not be held here.
        self.array_pool_retire(h); // [PC-ARRAYS] census + (RAVA_NO_ARENA_PRIMARRAY off) moves the payload into the recycle pool, so the overwrite below drops an empty Vec
        {
            let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
            arrays[h] = ArrayObj { data: ArrayData::Ref(Vec::new()) };
        }
        self.arr_alive.write().unwrap_or_else(|e| e.into_inner())[h] = false;
        // Invalidate the flat descriptor: the old buffer is now freed, so any (buggy)
        // access to this dead handle hits null/len-0 → a clean bounds fault instead of a
        // use-after-free.
        {
            let mut descs = self.array_descs.write().unwrap_or_else(|e| e.into_inner());
            if h < descs.len() {
                descs[h] = ArrayDesc {
                    data: std::ptr::null(),
                    len: 0,
                    element_id: DEAD_ARRAY_ELEMENT_ID,
                };
            }
        }
        self.free_arr.lock().unwrap_or_else(|e| e.into_inner()).push(h as u32);
        freed
    }

    /// Retire one dead map slot. Same extraction, same epoch discipline, as above.
    #[inline]
    fn retire_map_slot(&mut self, id: usize) -> usize {
        let freed = 32
            + self.maps[id]
                .index
                .keys()
                .map(MapKey::entry_bytes)
                .sum::<usize>();
        self.maps[id] = MapObj::default();
        self.map_alive[id] = false;
        self.free_map.push(id as u32);
        freed
    }

    pub(crate) fn gc_with_object_slot_policy(
        &mut self,
        roots: &[Value],
        release_dead_objects: bool,
    ) -> usize {
        // RAVA_NO_FAST_GC=1 restores the previous shape of this pass exactly: unfiltered
        // grey pushes, a slot-at-a-time sweep, and freshly allocated mark buffers.
        let fast = fast_gc_enabled();
        let (no, na, nm) = (self.objects.read().unwrap_or_else(|e| e.into_inner()).len(), self.arrays.read().unwrap_or_else(|e| e.into_inner()).len(), self.maps.len());
        // MARK BUFFERS, RETAINED (see `MarkBits`): three `vec![false; n]` per collection —
        // several MB of malloc + memset + free on a product heap, then one BYTE read per
        // slot in the sweep to test one bit — became three bitmaps kept between
        // collections. Peak RSS is unchanged (the old buffers were live across the whole
        // pass, which is where peak sits anyway); what goes away is the allocation and 7/8
        // of the mark traffic.
        let mut scratch = if fast { gc_scratch_take() } else { GcScratch::default() };
        scratch.mo.reset(no);
        scratch.ma.reset(na);
        scratch.mm.reset(nm);
        scratch.work.clear();
        // The `$map` field carries a map id as a bare Value::Int; resolve its
        // interned field id once so the mark loop can recognise it by id.
        let map_field_id = self.field_id_existing("$map");
        // UNIFIED MARK (next-gen-gc): a Value::Obj root / FK_OBJ field seeds an arena
        // traversal that walks LIVE arena objects' ref cells and marks the handles they
        // hold — replacing the blind `arena_handle_roots` full-tenured scan, which
        // rooted handles from DEAD tenured arena objects too (pinning dead H↔A cycles).
        //
        // ⭐**FxHash, not the std default.** This set is inserted into for EVERY arena object the
        // mark loop visits, and it was `std::collections::HashSet` — SipHash, over pointers. The
        // allocation-site profile of `Bootstrap.bootStrap` puts
        // `hashbrown::HashMap::insert<u64,(),RandomState>` at **1.2% of all samples**, which is
        // this line: hashing a pointer with a cryptographic hash to answer "have I seen this
        // address". The rest of the engine already keys its hot maps with `FastMap`/`FastSet`
        // for exactly this reason; the GC's own membership set was missed.
        #[cfg(feature = "next-gen-gc")]
        scratch.seen_arena.clear();
        #[cfg(feature = "next-gen-gc")]
        let arena_nclasses = crate::nextgen::registry_len();
        // ⭐**DUPLICATE FILTER ON THE PUSH, NOT ON THE POP.** The grey stack used to receive
        // one 16-byte entry per REFERENCE EDGE and discard the repeats when they came back
        // off — so its traffic was O(edges), and a heap whose objects average three
        // incoming references pushed and popped three times more than it had nodes. Testing
        // the mark bit before the push turns that into ~O(nodes). It is a pure FILTER, not a
        // change of marking discipline: the pop arms still own the mark, so a slot that the
        // pop would have refused (empty `ObjectSlot`, out-of-range handle) is refused
        // identically, and reachability is bit-for-bit the set it was. The residue — an edge
        // seen twice before the target is popped — is bounded by the LIFO order, which pops
        // a child almost immediately after pushing it.
        macro_rules! grey {
            (obj $h:expr) => {{
                let h = $h as usize;
                if !fast || (h < no && !scratch.mo.get(h)) {
                    scratch.work.push(Mark::Obj(h as u32));
                }
            }};
            (arr $h:expr) => {{
                let h = $h as usize;
                if !fast || (h < na && !scratch.ma.get(h)) {
                    scratch.work.push(Mark::Arr(h as u32));
                }
            }};
            (map $h:expr) => {{
                let h = $h as usize;
                if !fast || (h < nm && !scratch.mm.get(h)) {
                    scratch.work.push(Mark::Map(h as u32));
                }
            }};
            (val $v:expr) => {
                match $v {
                    Value::Ref(h) => grey!(obj *h),
                    Value::Array(h) => grey!(arr *h),
                    #[cfg(feature = "next-gen-gc")]
                    Value::Obj(p) => scratch.work.push(Mark::Arena(p.as_ptr() as usize)),
                    _ => {}
                }
            };
        }
        // RETENTION ATTRIBUTION (RAVA_RETAIN_TRACE): count, per referrer class, how many
        // live objects of a dominant transient class (Iterator/Stream/$Lambda/Optional) it
        // holds via a direct ref field or a lambda capture — so the OOM's 3.3M retained
        // iterators can be traced to the structure keeping them alive.
        let retain_trace = retain_trace_enabled();
        let mut referrers: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let is_target = |cn: &str| {
            cn.contains("Iterator")
                || cn.contains("stream/Stream")
                || cn.contains("$Lambda")
                || cn.ends_with("Optional")
                || cn.contains("ArrayItr")
        };
        // The arena arm's two per-object scratch vectors, hoisted out of the loop. They were
        // allocated and freed ONCE PER VISITED ARENA OBJECT — a pair of mallocs per node of
        // the arena graph, on every collection. Cleared before each use, so a stale element
        // cannot exist; `fast` restores the per-visit allocation for bisection.
        #[cfg(feature = "next-gen-gc")]
        let mut arena_handles: Vec<Value> = Vec::new();
        #[cfg(feature = "next-gen-gc")]
        let mut arena_children: Vec<usize> = Vec::new();
        for v in roots {
            grey!(val v);
        }
        while let Some(m) = scratch.work.pop() {
            match m {
                Mark::Obj(h) => {
                    let h = h as usize;
                    if h >= no || scratch.mo.get(h) {
                        continue;
                    }
                    let Some(o) = self.get(h as u32) else {
                        continue;
                    };
                    scratch.mo.set(h);
                    let (ids, cells, kinds) = (o.fields.ids(), o.fields.cells(), o.fields.kinds());
                    for i in 0..ids.len() {
                        match kinds[i] {
                            FK_REF => {
                                let child = cells[i] as u32;
                                if retain_trace {
                                    if let Some(cc) = self.get(child) {
                                        if is_target(cc.class.as_ref()) {
                                            *referrers.entry(o.class.to_string()).or_insert(0) += 1;
                                        }
                                    }
                                }
                                grey!(obj child)
                            }
                            FK_ARRAY => {
                                let ah = cells[i] as u32;
                                if retain_trace {
                                    let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                                    if let Some(a) = arrays.get(ah as usize) {
                                        if let ArrayData::Ref(cells) = &a.data {
                                            let mut hit = 0usize;
                                            for &cell in cells {
                                                if cell != 0 && (cell as u64 >> 32) as i64 == REF_TAG >> 32 {
                                                    if let Some(cc) = self.get((cell & 0xFFFF_FFFF) as u32)
                                                    {
                                                        if is_target(cc.class.as_ref()) {
                                                            hit += 1;
                                                        }
                                                    }
                                                }
                                            }
                                            if hit > 0 {
                                                *referrers.entry(format!("{}[] (array field)", o.class)).or_insert(0) += hit;
                                            }
                                        }
                                    }
                                }
                                grey!(arr ah)
                            }
                            // A `$map` field carries its map id as a bare int cell.
                            FK_INT if Some(ids[i]) == map_field_id => {
                                grey!(map cells[i] as u32)
                            }
                            // An FK_OBJ field holds a raw arena pointer — traverse it
                            // so handles the arena object references stay marked.
                            #[cfg(feature = "next-gen-gc")]
                            FK_OBJ => scratch.work.push(Mark::Arena(cells[i] as usize)),
                            // Primitives + FK_BOX (Str/sentinels) hold no heap refs.
                            _ => {}
                        }
                    }
                    if let Some(lam) = &o.lambda {
                        for v in &lam.captured {
                            if retain_trace {
                                if let Value::Ref(ch) = v {
                                    if let Some(cc) = self.get(*ch) {
                                        if is_target(cc.class.as_ref()) {
                                            *referrers.entry(format!("{} (lambda-capture)", o.class)).or_insert(0) += 1;
                                        }
                                    }
                                }
                            }
                            grey!(val v);
                        }
                    }
                }
                Mark::Arr(h) => {
                    let h = h as usize;
                    if h >= na || scratch.ma.get(h) {
                        continue;
                    }
                    scratch.ma.set(h);
                    let ref_cells: Option<Vec<i64>> = {
                        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                        match &arrays[h].data {
                            ArrayData::Ref(cells) => Some(cells.clone()),
                            _ => None,
                        }
                    };
                    if let Some(cells) = ref_cells {
                        for &cell in &cells {
                            if cell != 0 {
                                match (cell as u64 >> 32) as i64 {
                                    x if x == REF_TAG >> 32 => {
                                        grey!(obj (cell & 0xFFFF_FFFF) as u32)
                                    }
                                    x if x == ARRAY_TAG >> 32 => {
                                        grey!(arr (cell & 0xFFFF_FFFF) as u32)
                                    }
                                    // A RAW ARENA POINTER (Value::Obj) stored in an
                                    // Object[] cell is a LOW address — no high-word tag —
                                    // so it lands here, NOT under REF/ARRAY_TAG. Without
                                    // handling it the unified mark NEVER reaches such an
                                    // object (`reached=false`) → the handles it holds (e.g.
                                    // a VoxelShape's `shape`) are swept + reused → a stale
                                    // receiver. FLAT IMMEDIATE-HANDLE PASS: surface ONLY
                                    // the direct Java handles this array-cell arena object
                                    // holds — NOT a recursive `Mark::Arena` over its arena
                                    // subtree, which would drag a (possibly dead) arena
                                    // graph into tenured via the cross-heap pin (the
                                    // dangling shield then keeps it alive → `tenured
                                    // exhausted`). The handle's own transitive graph is
                                    // marked normally via Mark::Obj/Arr; the arena children
                                    // stay reachable only through the real root graph.
                                    // ABOX_TAG (boxed Str) keeps a high-word tag, so
                                    // `is_arena_obj_addr` excludes it.
                                    // A raw arena pointer stored in a heap reference
                                    // array (e.g. DFU `List<Type>` / a VoxelShape merger's
                                    // `Object[]` of arena Type templates). A LIVE heap array
                                    // keeps its arena elements live, so traverse them FULLY
                                    // via `Mark::Arena` — surface their handles AND recurse
                                    // their arena children (whose deeper handles, e.g. the
                                    // captured `TypeFamily`, were otherwise swept + reused as
                                    // a `CubePointRange` → `UnsatisfiedLinkError`). Precise,
                                    // not the old blind full-tenured scan: only arena objects
                                    // reachable from a MARKED (live) array are pinned; a dead
                                    // array is never marked, so its arena graph stays dead.
                                    #[cfg(feature = "next-gen-gc")]
                                    _ if crate::nextgen::is_arena_obj_addr(cell as u64) => {
                                        scratch.work.push(Mark::Arena(cell as usize));
                                    }
                                    // ABOX_TAG → array_box (Str/boxed; no heap refs).
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Mark::Map(id) => {
                    let id = id as usize;
                    if id >= nm || scratch.mm.get(id) {
                        continue;
                    }
                    scratch.mm.set(id);
                    for (k, v) in &self.maps[id].entries {
                        grey!(val k);
                        grey!(val v);
                    }
                }
                // A live arena object reached from a real root: surface the handles it
                // holds (so they stay marked) and recurse into its arena children. A
                // dead arena object is NEVER enqueued here (nothing roots it), so a dead
                // H↔A cycle is no longer pinned → it gets swept.
                #[cfg(feature = "next-gen-gc")]
                Mark::Arena(addr) => {
                    if (addr as u64) == 0 || !scratch.seen_arena.insert(addr) {
                        continue;
                    }
                    // B5 LIVENESS BRIDGE: a raw pointer that is a PINNED MIRROR keeps
                    // its owning handle object alive — without this, an object whose
                    // only reference is the pinned form is swept out from under a live
                    // pointer (FieldBlob/native state freed, slot reused). Inert until
                    // pinned pointers flow (pinned_handle_of is None otherwise).
                    #[cfg(feature = "next-gen-gc")]
                    if let Some(h) = crate::nextgen::pinned_handle_of(addr as u64) {
                        grey!(obj h);
                    }
                    if fast {
                        arena_handles.clear();
                        arena_children.clear();
                    } else {
                        arena_handles = Vec::new();
                        arena_children = Vec::new();
                    }
                    crate::nextgen::arena_trace_refs(
                        addr as *mut u8,
                        arena_nclasses,
                        &mut |v| arena_handles.push(v),
                        &mut |c| arena_children.push(c as usize),
                    );
                    for v in arena_handles.iter() {
                        grey!(val v);
                    }
                    for c in arena_children.iter() {
                        scratch.work.push(Mark::Arena(*c));
                    }
                }
            }
        }
        // RETENTION ATTRIBUTION REPORT (RAVA_RETAIN_TRACE): top referrer classes holding
        // the dominant transient class alive. A dominant single referrer names the leak;
        // a flat spread suggests genuine per-structure retention (pressure, not a leak).
        if retain_trace && !referrers.is_empty() {
            let mut v: Vec<(String, usize)> = referrers.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            let total: usize = v.iter().map(|(_, n)| n).sum();
            eprintln!("[RETAIN] {total} target refs held; top referrer classes:");
            for (cls, n) in v.into_iter().take(15) {
                eprintln!("[RETAIN]   {n:>9}  {cls}");
            }
        }
        // DANGLING-EDGE VALIDATOR (RAVA_GC_VALIDATE): audit the unified mark's
        // COMPLETENESS. Walk every LIVE arena object; if it holds a handle the mark is
        // about to sweep (`!mo`/`!ma`), that's a use-after-free in waiting (the swept
        // handle gets reused under a new type — the getSize/isEmpty stale receiver).
        // `reached` tells root-gap (false) from ref-mask-gap (true: visited but cell
        // skipped). Cheap when off; bounded log when on.
        #[cfg(feature = "next-gen-gc")]
        if gc_validate_enabled() {
            let alive_g = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner());
            let (alive, mo_r, ma_r, seen_r) = (&*alive_g, &scratch.mo, &scratch.ma, &scratch.seen_arena);
            let mut reported = 0usize;
            crate::nextgen::arena_for_each_obj(&mut |p| {
                if reported >= 50 {
                    return;
                }
                let mut bad: Vec<String> = Vec::new();
                crate::nextgen::arena_trace_refs(
                    p,
                    arena_nclasses,
                    &mut |v| match v {
                        Value::Ref(h) => {
                            let hi = h as usize;
                            if hi < no && alive.get(hi).copied().unwrap_or(false) && !mo_r.get(hi) {
                                bad.push(format!("Ref({h})"));
                            }
                        }
                        Value::Array(h) => {
                            let hi = h as usize;
                            if hi < na && !ma_r.get(hi) {
                                bad.push(format!("Array({h})"));
                            }
                        }
                        _ => {}
                    },
                    &mut |_| {},
                );
                if !bad.is_empty() {
                    let cls = crate::nextgen::class_name(unsafe { crate::nextgen::obj_class(p) })
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    // `reached=false` alone cannot accuse anyone: this walk strides the bump
                    // regions linearly, so it also visits arena objects that are GARBAGE
                    // awaiting collection — and a dead holder naming a swept handle is
                    // correct, not a defect. TENURED is the available discriminator: an
                    // object in the old region has already survived at least one arena
                    // collection, so it was live. `tenured=true` + `reached=false` is the
                    // report that actually accuses the rooting; `tenured=false` may be
                    // ordinary garbage.
                    let tenured = crate::arena::ArenaPtr::new(p)
                        .map_or(false, |ap| !crate::nextgen::tlab_in_nursery(ap));
                    eprintln!(
                        "[DANGLING] holder={cls} ptr={:#x} reached={} tenured={tenured} bad={bad:?}",
                        p as usize,
                        seen_r.contains(&(p as usize))
                    );
                    reported += 1;
                }
            });
        }
        let mut freed = 0usize;
        // `fast2` gates the pause block of THIS round (RAVA_NO_FAST_GCPAUSE2): the sweep
        // stepping by liveness word, and the coalesced free-list epoch bump below.
        let fast2 = fast_gcpause2_enabled();
        // OBJECT SWEEP, WORD AT A TIME. A full word of marks is 64 consecutive LIVE
        // handles: the bitmap lets the sweep step over them with one compare instead of
        // reading 64 `obj_alive` bytes it can do nothing with. Late in a boot most of the
        // object table is live, which is exactly when the old byte loop was most wasteful.
        //
        // ⭐**AND THE OTHER HALF OF THE TABLE.** The all-marked skip says nothing about a
        // run that is all DEAD — the free list and the tail the mutator has not reached —
        // where the loop still read 64 liveness bytes per word to retire nothing. `dead =
        // alive & !mark` answers both ends with one word: zero means step over the run
        // (whether because every slot is live or because none is allocated), non-zero means
        // visit exactly the slots being retired. The retirement ORDER is unchanged —
        // `trailing_zeros` walks the set bits ascending, the same order the byte loop used —
        // so `free_obj` receives its handles in exactly the sequence it did before, and
        // handle reuse is bit-for-bit identical.
        for wi in 0..scratch.mo.word_count() {
            let w = scratch.mo.word(wi);
            if fast && w == u64::MAX {
                continue;
            }
            let base = wi << 6;
            let end = std::cmp::min(base + 64, no);
            if fast2 {
                let mut dead = { let g = self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()); alive_word(&*g, base, end) } & !w;
                while dead != 0 {
                    let h = base + dead.trailing_zeros() as usize;
                    dead &= dead - 1;
                    freed += self.retire_object_slot(h, release_dead_objects);
                }
                continue;
            }
            for h in base..end {
                if (w >> (h - base)) & 1 == 0 && self.obj_alive.lock().unwrap_or_else(|e| e.into_inner())[h] {
                    freed += self.retire_object_slot(h, release_dead_objects);
                }
            }
        }
        // ⭐**ONE EPOCH BUMP PER COLLECTION, NOT ONE PER RECYCLED SLOT.** `handle_epoch` is a
        // relaxed global counter whose ONLY contract is "if the value changed, a handle-keyed
        // cache may now alias a re-issued slot". Bumping it inside the sweep made a
        // collection that frees half a million arrays perform half a million `lock xadd` on
        // one shared cache line, for a value nothing can observe mid-sweep: the sweep is
        // stop-the-world and every reader is a mutator that is not running. One bump after
        // both free-list sweeps carries exactly the same signal to every reader.
        let mut recycled = 0usize;
        let trim = array_trim_enabled();
        for wi in 0..scratch.ma.word_count() {
            let w = scratch.ma.word(wi);
            // The all-live skip is only sound while RAVA_ARRAY_TRIM is off — the trim
            // branch below has work to do on LIVE arrays, so a marked word is not empty
            // of work when it is on.
            if fast && !trim && w == u64::MAX {
                continue;
            }
            let base = wi << 6;
            let end = std::cmp::min(base + 64, na);
            // Same word step as the object sweep, and for the same reason. Gated on `!trim`
            // for the same reason the all-live skip is: with RAVA_ARRAY_TRIM on, a LIVE slot
            // is not "nothing to do".
            if fast2 && !trim {
                let mut dead = {
                    let arr_alive = self.arr_alive.read().unwrap_or_else(|e| e.into_inner());
                    alive_word(&arr_alive, base, end) & !w
                };
                while dead != 0 {
                    let h = base + dead.trailing_zeros() as usize;
                    dead &= dead - 1;
                    freed += self.retire_array_slot(h);
                    recycled += 1;
                }
                continue;
            }
            for h in base..end {
                let marked = (w >> (h - base)) & 1 != 0;
                if !marked && self.arr_alive.read().unwrap_or_else(|e| e.into_inner())[h] {
                    freed += self.retire_array_slot(h);
                    recycled += 1;
                    if !fast2 {
                        handle_epoch_bump(); // slot re-issuable ⇒ handle-keyed caches alias
                    }
                } else if trim && self.arr_alive.read().unwrap_or_else(|e| e.into_inner())[h] {
                    // LIVE ArrayData::Ref (collection `$elems` backing): trim its persistent unused
                    // capacity ([[rava-array-table-verdict]]: ref arrays are ~53% slack — growable Vec
                    // 2× doubling that never shrinks; PRIM arrays are fixed-length + tight). Only when the
                    // slack exceeds 2 (skip micro-arrays). shrink_to_fit REALLOCS → the data buffer moves →
                    // the flat array pointer dangles, so sync_array_desc republishes it. Sound in the
                    // mark-sweep: it is stop-the-world (JIT frames suspended, re-read the desc after) and
                    // holds NO raw pointer into an array's own Vec buffer (arena forwarding — a distinct
                    // pass — collects into object-field / ref-array CELLS, not the backing Vec itself).
                    let trimmed = if let ArrayData::Ref(v) = &mut self.arrays.write().unwrap_or_else(|e| e.into_inner())[h].data {
                        if v.capacity() > v.len() + 2 {
                            let saved = (v.capacity() - v.len()) * 8;
                            v.shrink_to_fit();
                            freed += saved;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if trimmed {
                        self.sync_array_desc(h as u32);
                    }
                }
            }
        }
        for wi in 0..scratch.mm.word_count() {
            let w = scratch.mm.word(wi);
            if fast && w == u64::MAX {
                continue;
            }
            let base = wi << 6;
            let end = std::cmp::min(base + 64, nm);
            if fast2 {
                let mut dead = alive_word(&self.map_alive, base, end) & !w;
                while dead != 0 {
                    let id = base + dead.trailing_zeros() as usize;
                    dead &= dead - 1;
                    freed += self.retire_map_slot(id);
                    recycled += 1;
                }
                continue;
            }
            for id in base..end {
                if (w >> (id - base)) & 1 == 0 && self.map_alive[id] {
                    freed += self.retire_map_slot(id);
                    recycled += 1;
                    handle_epoch_bump(); // slot re-issuable ⇒ handle-keyed caches alias
                }
            }
        }
        // The coalesced signal for every array and map slot this collection returned to a
        // free list. Object slots keep their per-slot bump inside `retire_object_slot`
        // (a call site outside this file), so a collection that frees objects already
        // publishes a change; this covers the case where only arrays or maps died.
        if fast2 && recycled != 0 {
            handle_epoch_bump();
        }
        // Reclaim the append-only box side tables. They hold only Str/boxed
        // primitives (no heap refs), so the mark/sweep above never touches them —
        // without this they grow unbounded: every boxed store into a ref-array or
        // Str field appends forever, reaching hundreds of millions of entries /
        // many GB on autobox-heavy code (e.g. HashMap<Integer,Integer>).
        freed += self.compact_box_tables(&scratch.mo, &scratch.ma);
        {
            let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
            self.bytes.store(cur.saturating_sub(freed), std::sync::atomic::Ordering::Relaxed);
        }
        // Outer Vec capacity is not included authoritatively in `self.bytes`, so
        // table savings are deliberately excluded from GC's `freed` return value.
        let _ = self.trim_table_capacities(crate::heap_table_trim::enabled());
        if fast {
            gc_scratch_put(scratch);
        }
        freed
    }
}
