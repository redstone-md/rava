//! Array objects: allocation, element access, and the bulk operations over them.

use crate::heap::*;
/// `[PC-ARRAYS]`: the array-birth census and the retired-buffer recycle pool
/// (`RAVA_NO_ARENA_PRIMARRAY=1` disables the pool; the census is always on).
pub(crate) mod pool;
// The heap gate word (see `heap::fields`): one relaxed byte carrying every process-constant
// gate that a field or element access used to read from its own `OnceLock`. Element access is
// the second-hottest host function family in the engine, so the loads it does not make are
// counted in the comments at each site. Kill switch `RAVA_NO_HEAP_GATEWORD=1`.
use crate::heap::fields::{heap_gates, hg_m3heap};

impl Heap {
    /// Allocate an array of `len` elements of `element`, default-initialised
    /// (Null for reference types, 0 for primitives).
    /// Place an array into a reused (swept) slot if one is free, else append.
    pub(crate) fn put_array(&self, element_id: u32, data: ArrayData) -> u32 {
        // ⚠**THE HOLE IN MY OWN ACCOUNTING.** The birth counters covered the two OBJECT
        // allocators and said 8.6 M a boot; arrays were never counted, and this engine allocates
        // one for every collection, every iterator snapshot, every varargs call.
        // ⭐**ONE GATE READ FOR THE WHOLE INSTRUMENT BLOCK, AND IT USED TO BE SIX.** Disarmed —
        // which is every production boot — the old block still executed six `OnceLock` acquire
        // loads (five counter handles plus the selector), a switch over the storage kind, and a
        // `MeterGuard` construction whose `Drop` still ran. That is instrument plumbing on the
        // 8.74 M-births-a-boot path, paid to record nothing. `dump` already returns early when
        // nothing is armed and no counter can be armed while the selector is empty, so gating the
        // entire block on `any_armed()` is observationally identical and leaves ONE load.
        //
        // Armed, everything below is exactly what it was:
        // - WHICH arrays. 8.74 M a boot is more than the object births, and nothing said whether
        //   they are collection backings, IO buffers or primitive work arrays — three different
        //   fixes. Split by the storage kind, which is free (the data is already matched on).
        // - ⭐**AND HOW LONG THEY ARE, BECAUSE THE COST IS IN A TAIL.** The in-situ meter prices a
        //   TYPICAL array birth at 28.8 ns and the population at 188 ns trimmed — 6.5x apart,
        //   which is the signature of a minority of expensive events rather than a uniformly
        //   costly one. 6.9 M births at the trimmed price is 1.3 s, **3.94% of the boot and the
        //   largest line on the re-priced map**, so which births are in that tail decides whether
        //   there is anything to do about it. Length is the first suspect and free to record here.
        // - Split the population at 1 KiB and meter each half separately: if the tail is long
        //   arrays, the two prices diverge and the cure is about copying; if it is not, the tail
        //   is allocation-triggered collection and the cure is about the nursery.
        //
        // Bound to a NAMED `_price` (never a bare `_`), which is what keeps the guard alive to the
        // end of the birth — including across the worker early-return below, exactly as the old
        // `_big`/`_small` pair did.
        // ⭐**AND NOW ONE READ FOR THE WHOLE BIRTH, NOT ONE PER BLOCK.** `diag::any_armed()` is a
        // `OnceLock<Vec<String>>` acquire plus an `is_empty` — and the alloc-generation stamp at
        // the bottom of this function opened with a SECOND `OnceLock` pair of its own
        // (`m3biome::enabled()`). Two acquire pairs per array birth, 8.74 M a boot, both asking
        // about constants of the process. `gw` is the one relaxed word both now read.
        let gw = heap_gates();
        let _price = if crate::heap::fields::hg_diag(gw) {
            crate::diag::births_array().inc();
            let len = match &data {
                ArrayData::Ref(v) => { crate::diag::births_array_ref().inc(); v.len() }
                ArrayData::Byte(v) => { crate::diag::births_array_byte().inc(); v.len() }
                ArrayData::Int(v) => { crate::diag::births_array_int().inc(); v.len() }
                ArrayData::Long(v) => { crate::diag::births_array_long().inc(); v.len() }
                ArrayData::Float(v) => { crate::diag::births_array_fp().inc(); v.len() }
                ArrayData::Double(v) => { crate::diag::births_array_fp().inc(); v.len() }
            };
            Some(if len >= 1024 {
                crate::diag::births_array_big_price().start()
            } else {
                crate::diag::births_array_small_price().start()
            })
        } else {
            None
        };
        // 11.7: same as put_object — a GIL-free worker claims a pre-reserved slot via the atomic
        // `parr_next` bump (worker_array_put) instead of pushing to the shared `arrays` Vec. Only when
        // a parallel array region is open; else (no region) a worker object/array alloc is unexpected
        // → abort. Segment exhaustion also aborts (driver recomputes serially). Gated on
        // `owns_reserved_segment` for the reason spelled out in `put_object`: a GIL-BOUND
        // executor task is not a gil-free worker and belongs on the shared path.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::owns_reserved_segment() {
            if self.parallel_array_active() {
                return match self.worker_array_put(element_id, data) {
                    Some(h) => h,
                    None => {
                        // 11.8 abort diagnostic (Point 1, array variant).
                        eprintln!("[RAVA-ABORT-DIAG] Array allocation limit reached on worker! Segment size exhausted.");
                        crate::parallel::request_abort();
                        0
                    }
                };
            }
            eprintln!("[RAVA-ABORT-DIAG] Array allocation on worker with NO active parallel array region!");
            crate::parallel::request_abort();
            return 0;
        }
        // [PC-ARRAYS] always-on census (`pool.rs`): kind × length of every birth that reaches the
        // shared table. Plain cells, ≤3 integer compares and one discriminant switch; the worker
        // arm above published into a pre-reserved region and is not a `mi_malloc` on this thread.
        self.array_pool.lock().unwrap_or_else(|e| e.into_inner()).note_birth(&data, element_id);
        let arr = ArrayObj { data };
        // Canonical order on the alloc path: free_arr (Mutex) before arrays/arr_alive (RwLock).
        // The array-desc sync is left OUTSIDE this scope so sync_array_desc_with_element can take its own write guard.
        let h = if let Some(h) = self.free_arr.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            self.arrays.write().unwrap_or_else(|e| e.into_inner())[h as usize] = arr;
            self.arr_alive.write().unwrap_or_else(|e| e.into_inner())[h as usize] = true;
            h
        } else {
            let h = self.arrays.read().unwrap_or_else(|e| e.into_inner()).len() as u32;
            self.arrays.write().unwrap_or_else(|e| e.into_inner()).push(arr);
            self.arr_alive.write().unwrap_or_else(|e| e.into_inner()).push(true);
            h
        };
        self.sync_array_desc_with_element(h, element_id);
        // GATE FOLD: `m3biome::enabled()` was an acquire-ordered `OnceLock` pair on the
        // 8.74 M-births-a-boot path, read to learn that a probe nobody armed is off. Same bit,
        // out of the word already loaded at the top, same position (still the last thing the
        // birth does).
        #[cfg(feature = "next-gen-gc")]
        if crate::heap::fields::hg_biome(gw) {
            mark_arr_gen(h);
        }
        h
    }

    /// Refresh the flat descriptor for handle `h` from its current `ArrayObj`
    /// (called on every array (re)allocation), and re-cache the table base in
    /// case the descriptor Vec reallocated.
    pub(crate) fn sync_array_desc(&self, h: u32) {
        self.sync_array_desc_gated(h, fast_arrays())
    }

    /// ⭐**THE SAME GATE, READ ONCE PER MUTATION INSTEAD OF TWICE.**
    ///
    /// Every growing mutation (`array_push`, `array_insert`, `array_remove`) branches on
    /// `fast_arrays()` at its head and then ENDS by calling `sync_array_desc`, which read the very
    /// same relaxed atomic a second time — two loads and two branches per `ArrayList.add`, one of
    /// the most-executed natives in this engine, to answer one question about a constant of the
    /// process. `fast_arrays` is documented as deliberately relaxed-and-not-a-`OnceLock` precisely
    /// because a load on this path is worth counting; counting it twice was the oversight.
    ///
    /// Those three callers now pass the answer they already hold. Every other caller
    /// (`gc_sweep`, `snapshot`, `strings`) goes through the wrapper above and is unchanged.
    pub(crate) fn sync_array_desc_gated(&self, h: u32, fast: bool) {
        let hu = h as usize;
        // ⭐**THE PLUMBING THAT RAN ON EVERY `ArrayList.add`.** Every growing mutation
        // (`array_push`, `array_insert`, `array_remove`, the `$sb` char buffer) ends here, and the
        // old body did FOUR table walks to republish two words: `array_descs.get(h)` for an
        // `element_id` that cannot have changed, then `self.arrays[hu]` TWICE (once for the
        // pointer, once for the length — each its own bounds check and its own switch over the six
        // storage kinds), then a whole-descriptor store.
        //
        // The element type of a live array handle is FIXED FOR ITS LIFETIME — a handle's type is
        // decided at birth by `put_array` and only ever reset (to `DEAD_ARRAY_ELEMENT_ID`) by the
        // sweeper, which writes the descriptor itself. So the refresh only owes the two fields
        // that a growth CAN move: the buffer pointer and the length. One lookup, one switch, two
        // word stores.
        //
        // ⭐**AND IT NO LONGER DIRTIES THE CACHELINE COMPILED CODE READS.** `jit_ctx.desc_base` is
        // `array_descs.as_ptr()`, which changes only when that Vec REALLOCATES — impossible on
        // this path, which writes an existing slot. Storing it per append meant every list append
        // invalidated, in every other core's cache, the line that every compiled array access
        if fast && hu < self.array_descs.read().unwrap_or_else(|e| e.into_inner()).len() {
            let dl = self.arrays.read().unwrap_or_else(|e| e.into_inner())
                .get(hu)
                .map(|a| (a.data.data_ptr(), a.data.len() as u32));
            if let Some((data, len)) = dl {
                let mut descs = self.array_descs.write().unwrap_or_else(|e| e.into_inner());
                let desc = &mut descs[hu];
                desc.data = data;
                desc.len = len;
                return;
            }
        }
        let element_id = self
            .array_descs
            .read().unwrap_or_else(|e| e.into_inner())
            .get(h as usize)
            .map(|desc| desc.element_id)
            .unwrap_or(DEAD_ARRAY_ELEMENT_ID);
        self.sync_array_desc_with_element(h, element_id);
    }

    pub(crate) fn sync_array_desc_with_element(&self, h: u32, element_id: u32) {
        let hu = h as usize;
        // ONE borrow of the array for both descriptor fields — the old form indexed
        // `self.arrays[hu]` twice, paying two bounds checks and two storage-kind switches.
        let (data, len) = {
            let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
            let d = &arrays[hu].data;
            (d.data_ptr(), d.len() as u32)
        };
        let desc = ArrayDesc { data, len, element_id };
        let mut descs = self.array_descs.write().unwrap_or_else(|e| e.into_inner());
        if hu < descs.len() {
            descs[hu] = desc;
            // No push ⇒ the spine cannot have moved ⇒ `desc_base` is already right. The compare
            // keeps this bit-identical to the unconditional store while leaving the line clean.
            let base = descs.as_ptr() as usize;
            if self.jit_ctx.desc_base != base {
                // Stage 2c transitional: repr(C) ABI mirror read by machine code at ctx offset 0.
                self.publish_desc_base(base);
            }
            return;
        }
        while descs.len() < hu {
            descs.push(ArrayDesc {
                data: std::ptr::null(),
                len: 0,
                element_id: DEAD_ARRAY_ELEMENT_ID,
            });
        }
        descs.push(desc);
        let base = descs.as_ptr() as usize;
        self.publish_desc_base(base);
    }

    /// Stage 2c transitional: `jit_ctx.desc_base` is a `repr(C)` ABI field read by
    /// compiled array access; publishing a new spine base needs `&mut` only because
    /// `JitCtx` is plain data under fixed machine-code offsets. Same discipline as
    /// `set_static_gen_mirror` / the object `objects_base` sites.
    #[allow(invalid_reference_casting)]
    fn publish_desc_base(&self, base: usize) {
        unsafe { &mut *(self as *const Self as *mut Self) }.jit_ctx.desc_base = base;
    }

    /// Address of the `JitCtx` block, handed to compiled code as its 2nd ABI
    /// parameter. Stable for the heap's lifetime; the values behind it track
    /// array descriptor-spine reallocations and the per-call fault state.
    pub fn array_desc_cell_ptr(&self) -> i64 {
        active_jit_ctx(&self.jit_ctx as *const JitCtx as *mut JitCtx) as i64
    }

    pub fn alloc_array(&self, element: &str, len: usize) -> u32 {
        // GATE FOLD, 2 → 1: `m3heap::probe_on()` is `enabled() || worldgen_probe()`, i.e. TWO
        // independent `OnceLock<bool>`s = two acquire loads, two derefs and two branches, on
        // every array allocation in the engine (8.74 M a boot across this and the two entries
        // below) to learn that neither classifier is armed. One relaxed byte, one bit.
        if hg_m3heap(heap_gates()) {
            crate::m3heap::record_alloc(&format!("[{element}"));
        }
        // ⭐**THE BUFFER OF A DEAD ARRAY, NOT A FRESH `mi_malloc`.** `[PC-BIRTH]` counts 4.18 M
        // arrays a bench run from compiled code alone, one host `Vec` each, and `[PC-MALLOC]`
        // says most of them die inside the run. With the pool on, the element id is interned
        // FIRST (it labels the census row) and the storage kind is derived through
        // `empty_array(element, 0)` — an empty `Vec` that owns no allocation — so the slab is
        // chosen by the same authority that chose it before. A miss builds `vec![0; len]` exactly
        // as the cold arm below does. `RAVA_NO_ARENA_PRIMARRAY=1`: the cold arm, verbatim.
        if pool::enabled() {
            let element_id = self.intern_array_element(element);
            let data = self.array_pool.lock().unwrap_or_else(|e| e.into_inner()).take_or_fresh(empty_array(element, 0), len, element_id);
            self.bytes.fetch_add(32 + len * data.elem_bytes(), std::sync::atomic::Ordering::Relaxed);
            return self.put_array(element_id, data);
        }
        let data = empty_array(element, len);
        self.bytes.fetch_add(32 + len * data.elem_bytes(), std::sync::atomic::Ordering::Relaxed);
        let element_id = self.intern_array_element(element);
        self.put_array(element_id, data)
    }

    /// Interned element id for `name` if it is already interned (read-only
    /// probe, no insertion) — feeds the per-site `anewarray` memo.
    pub(crate) fn array_element_id_of(&self, name: &str) -> Option<u32> {
        self.array_element_ids.read().unwrap_or_else(|e| e.into_inner()).get(name).copied()
    }

    /// Reference-array allocation by PRE-INTERNED element id — the `anewarray`
    /// site-memo fast path (RAVA_ALLOC_ROUTE_MEMO): no element-name String, no
    /// descriptor build, no interner hash. Identical to `alloc_array` for a
    /// reference element (an `anewarray` descriptor is always `L…;`/`[…` →
    /// `ArrayData::Ref`).
    pub(crate) fn alloc_ref_array_by_id(&self, element_id: u32, len: usize) -> u32 {
        // Same 2 → 1 fold as `alloc_array`; this is the `anewarray` site-memo entry, so it is the
        // busiest of the three.
        if hg_m3heap(heap_gates()) {
            if let Some(n) = self.array_element_name(element_id) {
                let tag = format!("[{n}");
                crate::m3heap::record_alloc(&tag);
            }
        }
        // Same pool as `alloc_array`, `Ref` slab only: this entry's contract is a reference
        // element id, so no probe is needed. Off, or on a miss: the historic `vec![0i64; len]`.
        let data = if pool::enabled() {
            self.array_pool.lock().unwrap_or_else(|e| e.into_inner()).take_ref_or_fresh(len)
        } else {
            ArrayData::Ref(vec![0i64; len])
        };
        self.bytes.fetch_add(32 + len * data.elem_bytes(), std::sync::atomic::Ordering::Relaxed);
        self.put_array(element_id, data)
    }

    /// Allocate an array directly from a value vector (stream/list backing).
    /// Primitive element types are packed; everything else keeps `Value`s.
    pub fn alloc_values(&self, element: &str, values: Vec<Value>) -> u32 {
        // Same 2 → 1 fold as `alloc_array`.
        if hg_m3heap(heap_gates()) {
            crate::m3heap::record_alloc(&format!("[{element}"));
        }
        // Pool-served for a PRIMITIVE backing (the `Ref` arm below rebuilds its cells from
        // `values` and would discard a pooled buffer unused): same probe-then-take shape as
        // `alloc_array`, with the element id interned once and reused at the end. Off
        // (`RAVA_NO_ARENA_PRIMARRAY=1`): the historic body below, verbatim.
        if pool::enabled() {
            let probe = empty_array(element, 0);
            let element_id = self.intern_array_element(element);
            let data = if matches!(probe, ArrayData::Ref(_)) {
                let cells: Vec<i64> = values.into_iter().map(|v| self.encode_ref_cell(v)).collect();
                ArrayData::Ref(cells)
            } else {
                let mut data = self.array_pool.lock().unwrap_or_else(|e| e.into_inner()).take_or_fresh(probe, values.len(), element_id);
                for (i, v) in values.into_iter().enumerate() { data.set(i, v); }
                data
            };
            let (len, eb) = (data.len(), data.elem_bytes());
            self.bytes.fetch_add(32 + len * eb, std::sync::atomic::Ordering::Relaxed);
            return self.put_array(element_id, data);
        }
        let mut data = empty_array(element, values.len());
        if !matches!(data, ArrayData::Ref(_)) {
            for (i, v) in values.into_iter().enumerate() { data.set(i, v); }
        } else {
            let cells: Vec<i64> = values.into_iter().map(|v| self.encode_ref_cell(v)).collect();
            data = ArrayData::Ref(cells);
        }
        let (len, eb) = (data.len(), data.elem_bytes());
        self.bytes.fetch_add(32 + len * eb, std::sync::atomic::Ordering::Relaxed);
        let element_id = self.intern_array_element(element);
        self.put_array(element_id, data)
    }

    /// Clone an array's raw storage into a fresh array of the same element type.
    ///
    /// ⭐**The snapshot an iterator needs, without the round trip through `Vec<Value>`.**
    /// Creating an iterator over a list went `array_values` (decode every cell into a `Value`)
    /// → `alloc_values` (re-encode every one into a new array), which is two full passes and two
    /// allocations to produce a copy the raw cells already are. `bridgestat` on
    /// `bench/StreamApplyCost.java` prices the result: **`List#iterator` at 780,000 calls and
    /// 1.61 s = 36.9% of that probe, 2,064 ns each** — for a FOUR-element list.
    ///
    /// The clone is bit-identical to the `array_values`+`alloc_values` pair: `ArrayData` owns its
    /// cells, so copying it copies exactly the encoded values, references included. Reference
    /// cells are copied AS CELLS, which is what `encode_ref_cell(decode_ref_cell(c))` produces.
    pub fn clone_array(&mut self, handle: u32) -> Option<u32> {
        use crate::heap::arraytypes::ArrayData;
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let a = arrays.get(handle as usize)?;
        // `ArrayData` deliberately does not derive `Clone` — copying an array is a heap event, so
        // it is spelled out here rather than made implicit at every call site.
        let data = match &a.data {
            ArrayData::Byte(v) => ArrayData::Byte(v.clone()),
            ArrayData::Int(v) => ArrayData::Int(v.clone()),
            ArrayData::Long(v) => ArrayData::Long(v.clone()),
            ArrayData::Float(v) => ArrayData::Float(v.clone()),
            ArrayData::Double(v) => ArrayData::Double(v.clone()),
            ArrayData::Ref(v) => ArrayData::Ref(v.clone()),
        };
        drop(arrays);
        let element_id = self.array_descs.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize)?.element_id;
        let (len, eb) = (data.len(), data.elem_bytes());
        self.bytes.fetch_add(32 + len * eb, std::sync::atomic::Ordering::Relaxed);
        Some(self.put_array(element_id, data))
    }

    /// Snapshot an array's elements.
    pub fn array_values(&self, handle: u32) -> Option<Vec<Value>> {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let a = arrays.get(handle as usize)?;
        Some(match &a.data {
            ArrayData::Ref(cells) => cells.iter().map(|c| self.decode_ref_cell(*c)).collect(),
            // `to_values` now switches on the storage kind ONCE for the whole array instead of
            // once per element (see `ArrayData::values_into`) and reserves exactly.
            other => other.to_values(),
        })
    }

    /// Allocate an array initialised from raw bytes (resource/IO data).
    pub fn alloc_bytes(&mut self, bytes: &[u8]) -> u32 {
        self.bytes.fetch_add(32 + bytes.len(), std::sync::atomic::Ordering::Relaxed);
        let element_id = self.intern_array_element("byte");
        self.put_array(element_id, ArrayData::Byte(bytes.to_vec()))
    }

    /// Read `len` bytes from a BYTE array at `off`. Returns fewer only at the end of the
    /// array, and `None` for a non-byte array or a bad handle.
    ///
    /// The point is the SLICE: `array_bytes` clones the whole array, which is a 10 MB copy
    /// to serve a 4-byte `readInt`, and reading through `array_get` costs a `Value` round
    /// trip per byte. Both were on the stream read path.
    pub fn array_read_bytes(&self, handle: u32, off: usize, len: usize) -> Option<Vec<u8>> {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let a = arrays.get(handle as usize)?;
        let ArrayData::Byte(v) = &a.data else { return None };
        if off >= v.len() {
            return Some(Vec::new());
        }
        let end = (off + len).min(v.len());
        Some(v[off..end].to_vec())
    }

    /// Write `src` into a BYTE array at `off`, in place. Returns false if the handle
    /// is not a byte array or the range does not fit.
    ///
    /// IN PLACE AND NEVER GROWING, deliberately: `array_base_ptr` documents that a
    /// Java array's backing `Vec` never reallocates, and the data-parallel fork holds
    /// raw pointers into it across a parallel region on that promise. This writes
    /// existing elements only — the length is untouched — so the promise still holds.
    /// Callers that need MORE room allocate a new, larger array (which is what
    /// `ByteArrayOutputStream` does anyway).
    pub fn array_write_bytes(&mut self, handle: u32, off: usize, src: &[u8]) -> bool {
        let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        let Some(a) = arrays.get_mut(handle as usize) else { return false };
        let ArrayData::Byte(v) = &mut a.data else { return false };
        let Some(end) = off.checked_add(src.len()) else { return false };
        if end > v.len() {
            return false;
        }
        v[off..end].copy_from_slice(src);
        true
    }

    pub fn array_len(&self, handle: u32) -> Option<usize> {
        self.arrays.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize).map(|a| a.data.len())
    }

    /// Raw base pointer + length of a PRIMITIVE array's flat element buffer (for the
    /// data-parallel fork: resolved under the GIL before workers start, then read
    /// directly off-thread). `None` for a reference array (no native primitive buffer)
    /// or a bad handle. The pointer is stable for the array's lifetime — Java arrays are
    /// fixed-length, so the backing Vec never reallocates — and the fork keeps it valid
    /// by allocating nothing in the parallel region (no GC ⇒ no move/free).
    #[cfg(feature = "next-gen-gc")]
    pub fn array_base_ptr(&self, handle: u32) -> Option<(*const u8, usize)> {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let a = arrays.get(handle as usize)?;
        match a.data {
            ArrayData::Ref(_) => None,
            _ => Some((a.data.data_ptr(), a.data.len())),
        }
    }

    /// The cell for a value that needs NO `array_box` slot, decided from the `Value` tag alone —
    /// no `&mut self`, no table walk, no allocation.
    ///
    /// ⭐**THIS SPLIT IS WHAT MAKES A REFERENCE-ARRAY STORE ONE TABLE LOOKUP.** `encode_ref_cell`
    /// needs `&mut self` only to append a box, so every caller had to finish borrowing
    /// `self.arrays`, encode, and then look the SAME handle up a second time to write the cell.
    /// The overwhelming majority of stores are `Ref`/`Array`/`Null`/`Obj` and never touch
    /// `array_box` at all; for those the cell can be computed while the element slot is already in
    /// hand. `None` means "this one really does need a box" and is the only case that pays the
    /// second lookup.
    #[inline]
    pub(crate) fn flat_ref_cell(v: &Value) -> Option<i64> {
        Some(match v {
            Value::Ref(h) => *h as i64 | REF_TAG,
            Value::Array(h) => *h as i64 | ARRAY_TAG,
            Value::Null => 0,
            // FLAT arena object (next-gen-gc): store the raw 8-aligned pointer
            // (< 2^40, low 2 bits 0b00) directly — NOT boxed into array_box. Its
            // header word's hi (bits 32..39) is < 256, disjoint from every high-word
            // tag (REF/ARRAY/ABOX, hi >= 256), so `decode_ref_cell` + `native_aaload`
            // recognise it and return it WITHOUT a callback (kills the per-load
            // `rava_aload` on arena-object arrays, e.g. `Shape[]`).
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => p.addr() as i64,
            _ => return None,
        })
    }

    /// Encode a `Value` into a flat reference-array cell (REF_TAG handle /
    /// ARRAY_TAG handle / 0 null / ABOX_TAG index for Str/boxed via `array_box`).
    pub(crate) fn encode_ref_cell(&self, v: Value) -> i64 {
        if let Some(cell) = Self::flat_ref_cell(&v) {
            return cell;
        }
        let idx = self.array_box.read().unwrap_or_else(|e| e.into_inner()).len();
        self.array_box.write().unwrap_or_else(|e| e.into_inner()).push(v);
        self.bytes.fetch_add(SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
        idx as i64 | ABOX_TAG
    }

    /// Decode a flat reference-array cell back to a `Value`.
    pub(crate) fn decode_ref_cell(&self, cell: i64) -> Value {
        if cell == 0 {
            return Value::Null;
        }
        let hi = (cell as u64 >> 32) as i64;
        // FLAT arena object (next-gen-gc): a raw `ArenaPtr` is `< 2^40` so its hi is
        // < 256, disjoint from every high-word tag below (hi >= 256).
        #[cfg(feature = "next-gen-gc")]
        if hi < 256 {
            return Value::Obj(unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) });
        }
        match hi {
            x if x == REF_TAG >> 32 => Value::Ref((cell & 0xFFFF_FFFF) as u32),
            x if x == ARRAY_TAG >> 32 => Value::Array((cell & 0xFFFF_FFFF) as u32),
            _ => self
                .array_box
                .read().unwrap_or_else(|e| e.into_inner())
                .get((cell & 0xFFFF_FFFF) as usize)
                .cloned()
                .unwrap_or(Value::Null),
        }
    }

    /// One table lookup for the bounds check AND the element — `Err(len)` when the index is out
    /// of range, which is exactly what the caller needs for the exception message.
    ///
    /// `array_len` followed by `array_get` indexes `self.arrays` TWICE, and each does its own
    /// bounds check: two dependent loads through the same table where one of each serves. An
    /// invalid handle answers `Err(0)`, matching what `array_len(h).unwrap_or(0)` produced.
    pub fn array_get_bounded(&self, handle: u32, index: usize) -> Result<Value, usize> {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let Some(a) = arrays.get(handle as usize) else { return Err(0) };
        if !fast_arrays() {
            let len = a.data.len();
            if index >= len {
                return Err(len);
            }
            return Ok(match &a.data {
                ArrayData::Ref(cells) => cells
                    .get(index)
                    .map(|c| self.decode_ref_cell(*c))
                    .unwrap_or(Value::Null),
                other => other.get(index).unwrap_or(Value::Null),
            });
        }
        // ⭐**ONE SWITCH, ONE BOUNDS CHECK, ONE TYPED LOAD.** The comment above is about doing ONE
        // table lookup instead of two; this is about what happened AFTER the lookup. The old body
        // switched over the six storage kinds three times — `a.data.len()` for the range test,
        // `match &a.data` for the arm, and `other.get(index)` inside `ArrayData::get` — and
        // bounds-checked `index` twice, once against a length it had just derived and once inside
        // the slice `get`. Four of those six pieces are the same question asked again.
        //
        // Folding the range test into the slice `get` answers it once: the length is now computed
        // ONLY on the failing arm, where the caller needs it for the
        // `ArrayIndexOutOfBoundsException` message. This runs once per `aaload`/`iaload`/… and the
        // PC profile of `bench/ArrayBench.java` puts it at 4.0% of the interpreter's hot window.
        match &a.data {
            // `byte[]` is SIGNED in the JVM; `boolean[]` holds 0/1 so the sign extension is a
            // no-op for it. `char`/`short`/`int` all share `i32` storage already holding the
            // correctly-extended value, so the load needs no per-opcode fixup here either.
            ArrayData::Byte(v) => match v.get(index) {
                Some(&x) => Ok(Value::Int(x as i8 as i32)),
                None => Err(v.len()),
            },
            ArrayData::Int(v) => match v.get(index) {
                Some(&x) => Ok(Value::Int(x)),
                None => Err(v.len()),
            },
            ArrayData::Long(v) => match v.get(index) {
                Some(&x) => Ok(Value::Long(x)),
                None => Err(v.len()),
            },
            ArrayData::Float(v) => match v.get(index) {
                Some(&x) => Ok(Value::Float(x)),
                None => Err(v.len()),
            },
            ArrayData::Double(v) => match v.get(index) {
                Some(&x) => Ok(Value::Double(x)),
                None => Err(v.len()),
            },
            ArrayData::Ref(v) => match v.get(index) {
                Some(&c) => Ok(self.decode_ref_cell(c)),
                None => Err(v.len()),
            },
        }
    }

    pub fn array_get(&self, handle: u32, index: usize) -> Option<Value> {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let Some(a) = arrays.get(handle as usize) else { return None };
        if !fast_arrays() {
            return match &a.data {
                ArrayData::Ref(cells) => cells.get(index).map(|c| self.decode_ref_cell(*c)),
                other => other.get(index),
            };
        }
        // Same single-switch shape as `array_get_bounded`: the old form asked the storage kind
        // twice (once for the arm, once inside `ArrayData::get`) and produced a nested
        // `Option<Option<Value>>` that the arm then flattened.
        match &a.data {
            ArrayData::Byte(v) => v.get(index).map(|&x| Value::Int(x as i8 as i32)),
            ArrayData::Int(v) => v.get(index).map(|&x| Value::Int(x)),
            ArrayData::Long(v) => v.get(index).map(|&x| Value::Long(x)),
            ArrayData::Float(v) => v.get(index).map(|&x| Value::Float(x)),
            ArrayData::Double(v) => v.get(index).map(|&x| Value::Double(x)),
            ArrayData::Ref(v) => v.get(index).map(|&c| self.decode_ref_cell(c)),
        }
    }

    /// A reference array's raw cells, in ONE indirection — the typed slice itself, with no
    /// `Value` built and no per-element decode.
    ///
    /// ⭐**FOR THE CALLER THAT WANTS TO SCAN, NOT TO READ ONE ELEMENT.** `array_values` costs a
    /// `Vec<Value>` allocation plus a 24-byte boxed `Value` per element; a caller that only needs
    /// to test cells (a `contains` over handles, an identity scan, a null count) can compare the
    /// ENCODED cell directly — `flat_ref_cell` says what a `Value` encodes to without touching the
    /// heap, so `cells.contains(&flat)` answers reference-identity membership in one memchr-shaped
    /// pass. `None` for a primitive array or a dead handle.
    pub fn array_ref_cells(&self, handle: u32) -> Option<Vec<i64>> {
        match &self.arrays.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize)?.data {
            ArrayData::Ref(cells) => Some(cells.clone()),
            _ => None,
        }
    }

    /// `array_values` into a caller-owned buffer — same elements, no fresh `Vec` per call.
    ///
    /// The drain path allocates one `Vec<Value>` per `iterator`/`toArray`/`stream`/`toString`;
    /// a caller with a scratch buffer can `clear()` and refill instead. Returns false (leaving
    /// `out` untouched) for a dead handle, so a miss is distinguishable from an empty array.
    pub fn array_values_into(&self, handle: u32, out: &mut Vec<Value>) -> bool {
        let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
        let Some(a) = arrays.get(handle as usize) else { return false };
        match &a.data {
            ArrayData::Ref(cells) => {
                out.reserve(cells.len());
                out.extend(cells.iter().map(|c| self.decode_ref_cell(*c)));
            }
            other => other.values_into(out),
        }
        true
    }

    /// Append `value` to a growable synthetic collection-backing array in place
    /// (our `$elems` arrays are NOT fixed-length Java arrays — they back
    /// ArrayList/Set/Stream and grow). Avoids the O(n) clone+realloc that
    /// rebuilding the whole array per `add` incurred (the boot-time O(n^2) when a
    /// guava list/set was built element-by-element). Re-syncs the flat descriptor
    /// so the JIT observes the (possibly reallocated) buffer.
    pub fn array_push(&mut self, handle: u32, value: Value) {
        // ⭐**ONE TABLE LOOKUP FOR THE WHOLE APPEND; IT USED TO BE THREE, PLUS FOUR MORE IN
        // `sync_array_desc`.** The old body walked `self.arrays` once to ask "is this a reference
        // array?", a second time to get the slot it would write, and (under next-gen-gc) a third
        // time to ask the element size — three dependent loads through the same table, each with
        // its own bounds check and its own switch over the six storage kinds, to answer questions
        // the FIRST arm already knows. `ArrayList.add` is one of the most-executed natives in this
        // engine, and this was the plumbing wrapped around a `Vec::push`.
        //
        // The one thing that genuinely needed `&mut self` mid-flight was encoding a reference
        // cell, because a boxed value appends to `array_box`. `flat_ref_cell` decides that from
        // the `Value` tag alone, so the common cases encode with the slot in hand and only a
        // Str/boxed element pays the re-borrow.
        if fast_arrays() {
            let mut boxed = None;
            // Scope the write guard to the match: `encode_ref_cell` (boxed arm) needs `&mut self`,
            // which cannot coexist with a live `arrays` guard. The element size falls out of the arm.
            let eb = {
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                let Some(a) = arrays.get_mut(handle as usize) else { return };
                match &mut a.data {
                    ArrayData::Byte(v) => { v.push(value.as_int().unwrap_or(0) as u8); 1 }
                    ArrayData::Int(v) => { v.push(value.as_int().unwrap_or(0)); 4 }
                    ArrayData::Long(v) => { v.push(value.as_long().unwrap_or(0)); 8 }
                    ArrayData::Float(v) => { v.push(value.as_f64() as f32); 4 }
                    ArrayData::Double(v) => { v.push(value.as_f64()); 8 }
                    ArrayData::Ref(cells) => {
                        match Self::flat_ref_cell(&value) {
                            Some(cell) => cells.push(cell),
                            None => boxed = Some(value),
                        }
                        8
                    }
                }
            };
            if let Some(v) = boxed {
                let cell = self.encode_ref_cell(v);
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                if let Some(ArrayData::Ref(cells)) = arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                    cells.push(cell);
                }
            }
            // Same accounting as the legacy arm below, from the size the match already produced.
            #[cfg(feature = "next-gen-gc")]
            { self.bytes.fetch_add(eb, std::sync::atomic::Ordering::Relaxed); }
            #[cfg(not(feature = "next-gen-gc"))]
            { let _ = eb; self.bytes.fetch_add(SLOT_BYTES, std::sync::atomic::Ordering::Relaxed); }
            // The gate this arm was entered on, reused: one relaxed load per mutation, not two.
            self.sync_array_desc_gated(handle, true);
            return;
        }
        {
            let is_ref = {
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                matches!(arrays.get(handle as usize).map(|a| &a.data), Some(ArrayData::Ref(_)))
            };
            if is_ref {
                let cell = self.encode_ref_cell(value);
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                if let Some(ArrayData::Ref(cells)) = arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                    cells.push(cell);
                }
            } else if let Some(a) = self.arrays.write().unwrap_or_else(|e| e.into_inner()).get_mut(handle as usize) {
                match &mut a.data {
                    ArrayData::Byte(v) => v.push(value.as_int().unwrap_or(0) as u8),
                    ArrayData::Int(v) => v.push(value.as_int().unwrap_or(0)),
                    ArrayData::Long(v) => v.push(value.as_long().unwrap_or(0)),
                    ArrayData::Float(v) => v.push(value.as_f64() as f32),
                    ArrayData::Double(v) => v.push(value.as_f64()),
                    ArrayData::Ref(_) => unreachable!(),
                }
            }
            // Account the element's REAL size (`elem_bytes`), not `SLOT_BYTES`.
            #[cfg(feature = "next-gen-gc")]
            {
                let eb = self.arrays.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize).map(|a| a.data.elem_bytes()).unwrap_or(8);
                self.bytes.fetch_add(eb, std::sync::atomic::Ordering::Relaxed);
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                self.bytes.fetch_add(SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
            }
            // Only reachable with the gate OFF (the fast arm returns), so it is known here too.
            self.sync_array_desc_gated(handle, false);
        }
    }

    pub fn array_set(&mut self, handle: u32, index: usize, value: Value) {
        // M3 9.2d.1/.2: an arraystore into a heap array — classify vs the pre-built shared RTree
        // long[], tagging the element type (long[]/Object[]/... = generic temp vs a tree structure).
        // GATE FOLD: this is the `aastore`/`iastore` funnel, so this `OnceLock` acquire pair ran
        // once per ELEMENT STORE. It is now the same bit out of the one relaxed byte, tested
        // before anything else exactly as it was, so an armed run classifies identically.
        #[cfg(feature = "next-gen-gc")]
        if crate::heap::fields::hg_biome(heap_gates()) {
            let ty = self
                .array_descs
                .read().unwrap_or_else(|e| e.into_inner())
                .get(handle as usize)
                .and_then(|desc| self.array_element_name(desc.element_id))
                .map(|element| format!("[{element}"))
                .unwrap_or_else(|| "[?".to_string());
            crate::m3biome::note_write(arr_gen(handle), true, &ty);
        }
        // ⭐**ONE TABLE LOOKUP AND ONE SWITCH; IT USED TO BE TWO OF EACH.** `aastore`/`iastore`
        // probed `self.arrays` once purely to classify the storage kind and then walked the SAME
        // table again to reach the slot. As in `array_push`, `flat_ref_cell` removes the reason
        // the two had to be separate: only a value that needs an `array_box` slot requires
        // `&mut self` between the classify and the store, and that is the rare arm.
        //
        // Out-of-range stays a SILENT NO-OP, exactly as before — `array_set` is reached from
        // paths that have already range-checked, and the interpreter's `astore` opcodes raise
        // `ArrayIndexOutOfBoundsException` from their own bounds test, not from here.
        if fast_arrays() {
            let mut boxed = None;
            {
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                let Some(a) = arrays.get_mut(handle as usize) else { return };
                match &mut a.data {
                    ArrayData::Ref(cells) => match Self::flat_ref_cell(&value) {
                        Some(cell) => {
                            if let Some(s) = cells.get_mut(index) {
                                *s = cell;
                            }
                        }
                        None => boxed = Some(value),
                    },
                    other => other.set(index, value),
                }
            }
            if let Some(v) = boxed {
                let cell = self.encode_ref_cell(v);
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                if let Some(ArrayData::Ref(cells)) = arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                    if let Some(s) = cells.get_mut(index) {
                        *s = cell;
                    }
                }
            }
            return;
        }
        {
            let is_ref = {
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                matches!(arrays.get(handle as usize).map(|a| &a.data), Some(ArrayData::Ref(_)))
            };
            if is_ref {
                let cell = self.encode_ref_cell(value);
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                if let Some(ArrayData::Ref(cells)) = arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                    if let Some(s) = cells.get_mut(index) {
                        *s = cell;
                    }
                }
            } else if let Some(a) = self.arrays.write().unwrap_or_else(|e| e.into_inner()).get_mut(handle as usize) {
                a.data.set(index, value);
            }
        }
    }

    /// Remove the element at `index`, shifting the tail down (native `Vec::remove`,
    /// O(n) memmove — vs the general collection path which clones the whole backing
    /// array into a `Vec<Value>` and rebuilds it). Returns the removed value, or
    /// None if out of range. Backs ArrayList.remove(int) / remove(0).
    pub fn array_remove(&mut self, handle: u32, index: usize) -> Option<Value> {
        // One lookup, one switch — see `array_push`. The removed reference cell is carried OUT of
        // the borrow and decoded after, which is what the legacy arm did too (via a second walk).
        if fast_arrays() {
            let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
            let a = arrays.get_mut(handle as usize);
            let Some(a) = a else { return None };
            let mut cell = None;
            let (mut removed, eb) = match &mut a.data {
                ArrayData::Byte(v) if index < v.len() => (Value::Int(v.remove(index) as i8 as i32), 1),
                ArrayData::Int(v) if index < v.len() => (Value::Int(v.remove(index)), 4),
                ArrayData::Long(v) if index < v.len() => (Value::Long(v.remove(index)), 8),
                ArrayData::Float(v) if index < v.len() => (Value::Float(v.remove(index)), 4),
                ArrayData::Double(v) if index < v.len() => (Value::Double(v.remove(index)), 8),
                ArrayData::Ref(v) if index < v.len() => {
                    cell = Some(v.remove(index));
                    (Value::Null, 8)
                }
                _ => return None,
            };
            drop(arrays);
            if let Some(c) = cell {
                removed = self.decode_ref_cell(c);
            }
            #[cfg(feature = "next-gen-gc")]
            { {
            let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
            self.bytes.store(cur.saturating_sub(eb), std::sync::atomic::Ordering::Relaxed);
        } }
            #[cfg(not(feature = "next-gen-gc"))]
            { let _ = eb; {
            let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
            self.bytes.store(cur.saturating_sub(SLOT_BYTES), std::sync::atomic::Ordering::Relaxed);
        } }
            // The gate this arm was entered on, reused: one relaxed load per mutation, not two.
            self.sync_array_desc_gated(handle, true);
            return Some(removed);
        }
        {
            let is_ref = {
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                matches!(arrays.get(handle as usize).map(|a| &a.data), Some(ArrayData::Ref(_)))
            };
            let removed = if is_ref {
                let cell = {
                    let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                    match arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                        Some(ArrayData::Ref(cells)) if index < cells.len() => cells.remove(index),
                        _ => return None,
                    }
                };
                self.decode_ref_cell(cell)
            } else {
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                let a = arrays.get_mut(handle as usize);
                let Some(a) = a else { return None };
                match &mut a.data {
                    ArrayData::Byte(v) if index < v.len() => Value::Int(v.remove(index) as i8 as i32),
                    ArrayData::Int(v) if index < v.len() => Value::Int(v.remove(index)),
                    ArrayData::Long(v) if index < v.len() => Value::Long(v.remove(index)),
                    ArrayData::Float(v) if index < v.len() => Value::Float(v.remove(index)),
                    ArrayData::Double(v) if index < v.len() => Value::Double(v.remove(index)),
                    _ => return None,
                }
            };
            // Symmetric with array_push: free the element's REAL size, not SLOT_BYTES.
            // Gated → default build keeps its exact prior `SLOT_BYTES` estimate.
            #[cfg(feature = "next-gen-gc")]
            {
                let eb = self.arrays.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize).map(|a| a.data.elem_bytes()).unwrap_or(8);
                {
                let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
                self.bytes.store(cur.saturating_sub(eb), std::sync::atomic::Ordering::Relaxed);
            }
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                {
                let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
                self.bytes.store(cur.saturating_sub(SLOT_BYTES), std::sync::atomic::Ordering::Relaxed);
            }
            }
            // Only reachable with the gate OFF (the fast arm returns), so it is known here too.
            self.sync_array_desc_gated(handle, false);
            Some(removed)
        }
    }

    /// Insert `value` at `index`, shifting the tail up (native `Vec::insert`, O(n)
    /// memmove). `index == len` appends. Out-of-range index past `len` is ignored.
    /// Lets a sorted-array set keep its order with one in-place memmove instead of
    /// the drain + full re-sort + realloc the general path would do per element.
    pub fn array_insert(&mut self, handle: u32, index: usize, value: Value) {
        // One lookup, one switch — see `array_push`. The `index > len` no-op and the
        // `index == len` append are both preserved by the per-arm `index <= v.len()` guard.
        if fast_arrays() {
            let mut boxed = None;
            let eb = {
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                let Some(a) = arrays.get_mut(handle as usize) else { return };
                match &mut a.data {
                    ArrayData::Byte(v) if index <= v.len() => { v.insert(index, value.as_int().unwrap_or(0) as u8); 1 }
                    ArrayData::Int(v) if index <= v.len() => { v.insert(index, value.as_int().unwrap_or(0)); 4 }
                    ArrayData::Long(v) if index <= v.len() => { v.insert(index, value.as_long().unwrap_or(0)); 8 }
                    ArrayData::Float(v) if index <= v.len() => { v.insert(index, value.as_f64() as f32); 4 }
                    ArrayData::Double(v) if index <= v.len() => { v.insert(index, value.as_f64()); 8 }
                    ArrayData::Ref(v) if index <= v.len() => {
                        match Self::flat_ref_cell(&value) {
                            Some(cell) => v.insert(index, cell),
                            None => boxed = Some(value),
                        }
                        8
                    }
                    _ => return,
                }
            };
            if let Some(v) = boxed {
                let cell = self.encode_ref_cell(v);
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                if let Some(ArrayData::Ref(cells)) = arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                    if index <= cells.len() {
                        cells.insert(index, cell);
                    }
                }
            }
            #[cfg(feature = "next-gen-gc")]
            { self.bytes.fetch_add(eb, std::sync::atomic::Ordering::Relaxed); }
            #[cfg(not(feature = "next-gen-gc"))]
            { let _ = eb; self.bytes.fetch_add(SLOT_BYTES, std::sync::atomic::Ordering::Relaxed); }
            // The gate this arm was entered on, reused: one relaxed load per mutation, not two.
            self.sync_array_desc_gated(handle, true);
            return;
        }
        {
            let is_ref = {
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                matches!(arrays.get(handle as usize).map(|a| &a.data), Some(ArrayData::Ref(_)))
            };
            if is_ref {
                let cell = self.encode_ref_cell(value);
                let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
                if let Some(ArrayData::Ref(cells)) = arrays.get_mut(handle as usize).map(|a| &mut a.data) {
                    if index <= cells.len() { cells.insert(index, cell); } else { return; }
                }
            } else if let Some(a) = self.arrays.write().unwrap_or_else(|e| e.into_inner()).get_mut(handle as usize) {
                match &mut a.data {
                    ArrayData::Byte(v) if index <= v.len() => v.insert(index, value.as_int().unwrap_or(0) as u8),
                    ArrayData::Int(v) if index <= v.len() => v.insert(index, value.as_int().unwrap_or(0)),
                    ArrayData::Long(v) if index <= v.len() => v.insert(index, value.as_long().unwrap_or(0)),
                    ArrayData::Float(v) if index <= v.len() => v.insert(index, value.as_f64() as f32),
                    ArrayData::Double(v) if index <= v.len() => v.insert(index, value.as_f64()),
                    _ => return,
                }
            }
            // Mirror array_push's accounting (gated; default build keeps SLOT_BYTES).
            #[cfg(feature = "next-gen-gc")]
            {
                let eb = self.arrays.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize).map(|a| a.data.elem_bytes()).unwrap_or(8);
                self.bytes.fetch_add(eb, std::sync::atomic::Ordering::Relaxed);
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                self.bytes.fetch_add(SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
            }
            // Only reachable with the gate OFF (the fast arm returns), so it is known here too.
            self.sync_array_desc_gated(handle, false);
        }
    }

    /// Natural-order sort of a PRIMITIVE array's `[from, to)` slice, in place.
    /// Returns true if handled (integer element types: byte/char/short/int/long);
    /// false for float/double/reference arrays, which the caller sorts on the
    /// generic Value path (float NaN/-0 ordering + comparator support live there).
    ///
    /// Large slices sort in PARALLEL across rayon's worker pool. This is pure-data
    /// work on the detached backing slice — no heap structure, no Value boxing, no
    /// bytecode callback — so it is data-race-free and needs no GIL. char/short/int
    /// all share `i32` storage and sort correctly by `i32` (char values are 0..65535
    /// → already positive); byte is stored `u8` but sorts by its SIGNED value.
    pub fn sort_primitive(&mut self, handle: u32, from: usize, to: usize) -> bool {
        use rayon::slice::ParallelSliceMut;
        const PAR_THRESHOLD: usize = 8192;
        let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        let Some(a) = arrays.get_mut(handle as usize) else { return false };
        match &mut a.data {
            ArrayData::Int(v) => {
                let hi = to.min(v.len());
                if from >= hi { return true; }
                let s = &mut v[from..hi];
                if s.len() > PAR_THRESHOLD { s.par_sort_unstable(); } else { s.sort_unstable(); }
                true
            }
            ArrayData::Long(v) => {
                let hi = to.min(v.len());
                if from >= hi { return true; }
                let s = &mut v[from..hi];
                if s.len() > PAR_THRESHOLD { s.par_sort_unstable(); } else { s.sort_unstable(); }
                true
            }
            ArrayData::Byte(v) => {
                let hi = to.min(v.len());
                if from >= hi { return true; }
                let s = &mut v[from..hi];
                // byte[] is signed in the JVM; sort by the signed interpretation.
                if s.len() > PAR_THRESHOLD {
                    s.par_sort_unstable_by_key(|&b| b as i8);
                } else {
                    s.sort_unstable_by_key(|&b| b as i8);
                }
                true
            }
            _ => false,
        }
    }

    // --- $sb mutable char-buffer backing ------------------------------------
    // StringBuilder/StringBuffer/Writer/CharBuffer back their `$sb` text with a
    // growable char-array (ArrayData::Int code points). `sb_append` is amortized
    // O(1) (in-place push) — the old immutable-`Value::Str` backing cloned the
    // WHOLE accumulated string per append = O(n^2) on every builder. Code points
    // (not UTF-16 units) match the previous `.chars()`-based semantics. The array
    // is a normal object-field ref, so GC + serialization trace it for free.

    /// Data pointer of the object's interned class-name `Arc<str>` — class names
    /// are interned (one Arc per name), so this is a stable, unique-per-class
    /// token usable as a cheap receiver-class guard (JIT direct-call cache). A
    /// dead/missing handle yields 0 (its class is the interned "" — but return 0
    /// explicitly so a guard can never match garbage).
    /// `[PC-ARRAYS]` retirement hook, called by `retire_array_slot` (`gc_sweep.rs`) after it has
    /// priced the slot and BEFORE it overwrites `self.arrays[h]`.
    ///
    /// Always: count the retirement (kind × length) against the births `put_array` counted, so
    /// the dump can say how many arrays die inside the run. Pool on: move the payload out of the
    /// slot into the recycle pool, leaving an empty `Vec` for the sweep's own overwrite to drop —
    /// which frees nothing, because an empty `Vec` owns nothing. Pool off
    /// (`RAVA_NO_ARENA_PRIMARRAY=1`): touch nothing; the sweep's overwrite frees the buffer
    /// exactly as it always did.
    ///
    /// The element id is read from the descriptor, which the sweep resets to
    /// `DEAD_ARRAY_ELEMENT_ID` only after this returns; `arrays_parallel`'s region placeholders
    /// never reach here (they are not `arr_alive`).
    #[inline]
    pub(crate) fn array_pool_retire(&mut self, h: usize) {
        let element_id = self
            .array_descs
            .read().unwrap_or_else(|e| e.into_inner())
            .get(h)
            .map(|desc| desc.element_id)
            .unwrap_or(DEAD_ARRAY_ELEMENT_ID);
        let mut arrays = self.arrays.write().unwrap_or_else(|e| e.into_inner());
        let kind = self.array_pool.lock().unwrap_or_else(|e| e.into_inner()).note_retire(&arrays[h].data, element_id);
        if pool::enabled() {
            let data = std::mem::replace(&mut arrays[h].data, ArrayData::Ref(Vec::new()));
            self.array_pool.lock().unwrap_or_else(|e| e.into_inner()).give(data, kind);
        }
    }

    /// The `[PC-ARRAYS]` block for the periodic profiler dump (`pc_profile/dump.rs`).
    pub fn array_pool_report() -> String {
        pool::report()
    }

    /// Element type descriptor of an array (e.g. "I", "Ljava/lang/Object;").
    pub fn array_element(&self, handle: u32) -> Option<Arc<str>> {
        let id = self.array_descs.read().unwrap_or_else(|e| e.into_inner()).get(handle as usize)?.element_id;
        self.array_element_name(id)
    }
}
