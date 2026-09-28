//! The Heap struct, its JIT context and the census type.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JitCtx {
    pub desc_base: usize,
    pub fault: u32,
    /// M3 COW: 1 strictly while a COW parallel phase-B region is live (workers running),
    /// 0 otherwise. Read by compiled getfield/putfield at ctx offset 12 to branch: armed →
    /// shadow-aware callback (rava_getfield/putfield), else the inline fast path. Reuses the
    /// former `_pad` slot → no ctx-offset shift for the other native field/static readers.
    pub cow_armed: u32,
    /// Base of the per-handle object-field descriptor table (`Heap::obj_descs`);
    /// native getfield reads it at offset 16. Refreshed on each field push.
    pub obj_desc_base: usize,
    /// Base of the global field-slot hint table (`Heap::field_hints`); native
    /// getfield/putfield read it at offset 24. A `fid`-bucketed cache of the last
    /// slot a field id was found at — checked before the linear id scan. Fixed
    /// size, so this base is set once and never moves.
    pub field_hint_base: usize,
    /// Base of the global getstatic value-cell table (`Heap::static_cells`); native
    /// getstatic reads it at offset 32. Each compiled getstatic SITE owns a unique
    /// 2×i64 cell `[gen, packed_value]`. Fixed size → base set once, never moves.
    pub static_cells_base: usize,
    /// Mirror of the current `Vm.static_gen` (read at offset 40). Bumped on every
    /// putstatic; native getstatic compares it to a cell's cached gen — a mismatch
    /// (some static was written) forces the callback, so the cache self-invalidates
    /// without per-cell bookkeeping.
    pub static_gen: u64,
    /// R2 (Flat-Floor): base of the `objects` slot array (`heap.objects.as_ptr()`),
    /// read at ctx offset 48 by compiled ref-dispatch (heap-Ref PIC) + ref-field reads +
    /// BitSet-inline, REPLACING the per-dispatch `rava_heap_objects_base` TLS callout with a
    /// single `[ctx+48]` load. MUST be republished on every `objects` realloc — done in
    /// `sync_obj_desc` (runs after every single alloc) + the batch reserve path; a stale
    /// base would index freed memory. Appended at the struct END so no existing ctx offset
    /// shifts (repr(C)).
    pub objects_base: usize,
    /// Base of this execution context's counting Bloom table for active COW
    /// shadow keys. Compiled getfield reads a counter at offset 56; zero is an
    /// exact negative, non-zero takes the authoritative TLS lookup. The normal
    /// context uses the shared table; detached workers install private tables.
    pub cow_bloom_base: usize,
}

impl Default for JitCtx {
    fn default() -> Self {
        Self {
            desc_base: 0,
            fault: 0,
            cow_armed: 0,
            obj_desc_base: 0,
            field_hint_base: 0,
            static_cells_base: 0,
            static_gen: 0,
            objects_base: 0,
            cow_bloom_base: COW_BLOOM_COUNTS.as_ptr() as usize,
        }
    }
}

#[derive(Default)]
pub struct Heap {
    // Box<Object> so each object has a STABLE heap address that survives the outer
    // Vec growing/reallocating. Prerequisite for parallel worldgen: a worker thread
    // reading an object (immutable registry data) must not have it move under it
    // when another thread allocates. Serde-transparent → snapshot format unchanged.
    // (Single-threaded behaviour is identical — this is a pure stable-address step.)
    pub(crate) objects: std::sync::RwLock<Vec<ObjectSlot>>,
    pub(crate) arrays: std::sync::RwLock<Vec<ArrayObj>>,
    pub(crate) array_pool: std::sync::Mutex<crate::heap::arrays::pool::ArrayPool>, // [PC-ARRAYS] retired-buffer recycle pool + census id mirror (heap/arrays/pool.rs); `RAVA_NO_ARENA_PRIMARRAY=1` leaves it empty
    /// Flat `(data_ptr, len, element_id)` per array handle, in lockstep with
    /// `arrays`. The metadata id occupies the former C-layout padding, so the JIT
    /// descriptor stays 16 bytes while ArrayObj drops a whole aligned word.
    pub(crate) array_descs: std::sync::RwLock<Vec<ArrayDesc>>,
    /// Per-handle object-field descriptors (lockstep with `objects`), for native
    /// getfield. `jit_ctx.obj_desc_base` caches the current base.
    pub(crate) obj_descs: std::sync::RwLock<Vec<ObjDesc>>,
    /// Global `fid`-bucketed slot-hint table for native getfield/putfield (lazily
    /// sized to `JIT_FIELD_HINT_N`, fixed thereafter). `field_hints[fid & (N-1)]`
    /// is the slot the field was last found at; compiled code tries it before the
    /// linear id scan. A stale/colliding hint just re-scans (the `ids[h]==fid`
    /// check guards correctness), so no value is ever read from the wrong slot.
    pub(crate) field_hints: Vec<u32>,
    /// Global getstatic value-cell table: `JIT_STATIC_SLOTS` cells, each two i64s
    /// `[gen, packed_value]`. A compiled getstatic site is assigned one UNIQUE cell
    /// (`next_static_slot`) → no inter-site collisions → a hit needs only the gen
    /// guard (no identity tag). Primitive statics only (ref values could dangle past
    /// a GC). Fixed size → `jit_ctx.static_cells_base` never moves.
    pub(crate) static_cells: Vec<i64>,
    /// Next free getstatic cell index (monotonic; assigned at compile time). At
    /// `JIT_STATIC_SLOTS` the table is full → further sites stay on the callback.
    pub(crate) next_static_slot: u32,
    pub(crate) jit_ctx: JitCtx,
    /// Side table for reference-typed field values that aren't a bare handle —
    /// Str / StdOut / StdErr (kind FK_BOX). Append-only: overwriting such a field
    /// leaves a dead entry (these hold no heap refs, so the GC never has to scan
    /// or free them — a bounded leak, not corruption; a compactor can come later).
    pub(crate) field_box: std::sync::RwLock<Vec<Value>>,
    /// Side table for reference-ARRAY elements that aren't a bare handle/null
    /// (Str / boxed primitive), addressed by `idx|ABOX_TAG`. Append-only, same as
    /// `field_box` (no heap refs inside → GC never scans/frees them).
    pub(crate) array_box: std::sync::RwLock<Vec<Value>>,
    /// Map backings, addressed by a `$map` id field on the owning object. Each
    /// map preserves insertion order while keeping a key→entry index for the
    /// lookup complexity callers expect from Java HashMap/LinkedHashMap.
    pub(crate) maps: Vec<MapObj>,
    /// Interned class names (see `Object.class`).
    ///
    /// SHARED-READABLE (docs/vm-shared-exec-plan.md), the same treatment as the field interner
    /// and the class loader: every allocation consults it and only a first-sighting inserts, so
    /// the lock lets `intern_class` — and `new_object`/`new_object_value` above it — take `&self`.
    pub(crate) class_pool: std::sync::RwLock<crate::FastMap<Box<str>, Arc<str>>>,
    /// Array element-type interner. Runtime slots keep only the `u32` id; names
    /// remain heap-owned and are resolved only at semantic/string boundaries.
    /// Stage 2c+arr: same append-only discipline as the field interner.
    pub(crate) array_element_names: std::sync::RwLock<Vec<Arc<str>>>,
    pub(crate) array_element_ids: std::sync::RwLock<crate::FastMap<Box<str>, u32>>,
    /// fixed set ($map, $result, x, y, …) shared by millions of objects, so an
    /// id lets `Object.fields` store a `u32` key (no per-write allocation).
    /// SHARED-READABLE (see `ClassLoader`'s note): the field interner is append-only and is read
    /// on the warm path of every `resolve_field_id`, written only when a name is seen for the
    /// first time. Behind an `RwLock` it lets `getfield`/`getstatic` — and everything above them —
    /// take `&Vm`, which is what the gil-free lane's ceiling turns on. The read is uncontended and
    /// the write is once per distinct field name in the whole process.
    pub(crate) field_names: std::sync::RwLock<Vec<Box<str>>>,
    pub(crate) field_ids: std::sync::RwLock<crate::FastMap<Box<str>, u32>>,
    /// Per-class field layout (id, default-kind) pushed in by `class_field_layout`
    /// so the alloc choke-point can pre-populate EVERY object — not just the
    /// `new` bytecode path — giving the const-slot getfield a class-constant slot
    /// it can read WITHOUT the id verify (universal pre-population).
    /// SHARED-READABLE, same reason as the field interner above it: `set_layout` runs strictly
    /// once per class, before any object of that class exists, while the reads sit on the arena
    /// allocation path.
    pub(crate) layouts: std::sync::RwLock<crate::FastMap<Box<str>, Arc<[(u32, u8)]>>>,
    /// B-series pinned-mirror: per-class field-id → slot index, mirroring the
    /// order of `layouts`. Built ONCE per class in `set_layout`, and ONLY when
    /// the pinned mirror is enabled, so the dual-store maintenance paths
    /// (`sync_obj_desc`, `pinned_mirror_store`, the read-flip/verify readers) do
    /// an O(1) id→slot lookup instead of a linear `layout.iter().position(id)`
    /// scan — turning `sync_obj_desc`'s per-object re-encode from O(nfields ×
    /// layout_len) into O(nfields). Empty (never built, zero allocation) when the
    /// mirror is off ⇒ the default path is byte-identical. Maps id→FIRST slot,
    /// matching `position()` semantics for any shadowed super-chain field id.
    /// Shared-readable alongside `layouts`: written once per class inside `set_layout` and read
    /// by the pinned-mirror maintenance paths, which are default-OFF.
    pub(crate) layout_slot_index: std::sync::RwLock<crate::FastMap<Box<str>, crate::FastMap<u32, u32>>>,
    /// Running estimate of heap bytes (objects/array-slots/map-entries), backing
    /// the `RAVA_XMX_MB` cap. Counting slots (not entries) is essential: one
    /// array holds thousands of `Value`s, so an entry count blew far past the
    /// byte budget (an 8 GB cap reached 11 GB). Mark-sweep GC decrements it.
    ///
    /// Stage 3: behind an `AtomicUsize` so the allocation paths that bump it can
    /// take `&Heap` instead of `&mut Heap` — that increment was the sole remaining
    /// reason `alloc`/`alloc_interned`/`put_field` needed exclusive access, and
    /// therefore the sole reason `as_mut_transitional` existed. `Relaxed` is
    /// sufficient: this is a running SIZE ESTIMATE read by the `RAVA_XMX_MB` cap
    /// check and the census, never a synchronisation edge for any other field.
    pub(crate) bytes: std::sync::atomic::AtomicUsize,
    /// Liveness + free lists for mark-sweep GC. A slot is reused (handle stable,
    /// no compaction) once swept; `*_alive[i]` distinguishes a live slot from a
    /// swept hole so the sweep doesn't double-free or re-scan it.
    ///
    /// Stage 2c+arr: `obj_alive`, `free_obj` behind Mutex (objects_alloc.rs);
    /// `arr_alive` behind RwLock because worker_array_put writes through raw
    /// pointers during a parallel region (the G3.5 contract: pre-reserved slots,
    /// no realloc, no Vec push — so the write races nothing under a shared guard);
    /// `free_arr` behind Mutex (serial path only: put_array pop, commit push).
    pub(crate) obj_alive: std::sync::Mutex<Vec<bool>>,
    pub(crate) arr_alive: std::sync::RwLock<Vec<bool>>,
    pub(crate) map_alive: Vec<bool>,
    pub(crate) free_obj: std::sync::Mutex<Vec<u32>>,
    pub(crate) free_arr: std::sync::Mutex<Vec<u32>>,
    pub(crate) free_map: Vec<u32>,
    /// G3.5 parallel array region: during a fork, workers claim disjoint pre-reserved array
    /// slots `[parr_base, parr_cap)` via `parr_next` (atomic), writing each slot with a raw
    /// pointer (no `Vec` push → no realloc → race-free). Inactive (`parr_cap == 0`) otherwise.
    pub(crate) parr_base: usize,
    pub(crate) parr_cap: usize,
    pub(crate) parr_next: std::sync::atomic::AtomicUsize,
    pub(crate) parr_epoch: u64,
    /// Pre-interned primitive element ids (indexed by JVM `atype - 4`: boolean/char/float/
    /// double/byte/short/int/long) so a fork worker's `worker_newarray` never mutates the interner.
    pub(crate) parr_prim_element_ids: Vec<u32>,
}

impl Drop for Heap {
    fn drop(&mut self) {
        // GAP-P01 tracer report (inert unless RAVA_ARRAY_INTERN_STATS is set).
        array_intern_stats::report();
        // GAP-S09 stage-2 callback census (inert unless RAVA_S09_CB_AUDIT is set).
        crate::jit::s09census::report();
        // GAP-A01/A05 task-graph routing probe (inert unless RAVA_TASKGRAPH_PROBE is set).
        crate::taskgraph_probe::report();
        // GILFREE-RINGS admission/abort accounting (active by default (`RAVA_NO_GILFREE_RINGS` opts out)).
        crate::gilfree_cf::report();
        // Vec-growth census (inert unless RAVA_VECGROW_STAT is set).
        crate::vecgrow::report();
        // Which FROZEN shared-VM sites a gil-free worker reached (inert unless
        // RAVA_FREEZE_TALLY is set). This is the measurement the lane's ceiling turns on.
        crate::freeze_tally::report();
    }
}

/// GAP-P01 tracer (`RAVA_ARRAY_INTERN_STATS`): per-allocation array element-type
/// interner traffic. Every `alloc_array`/`alloc_values`/`alloc_bytes` consults the
/// interner hashmap, and interpreted/JIT `anewarray` additionally FORMATS an
/// `L…;` descriptor string per allocation. These counters size that repeated
/// work before any structural fix. Process-wide atomics (workers also allocate);
/// inert one-load check when the env flag is unset; printed on `Heap` drop.
pub(crate) mod array_intern_stats {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub(crate) static PRIM: AtomicU64 = AtomicU64::new(0);
    pub(crate) static REF: AtomicU64 = AtomicU64::new(0);
    pub(crate) static MISS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static REF_DESC_BUILT: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_ARRAY_INTERN_STATS").is_some())
    }

    #[inline]
    pub(crate) fn count_lookup(name: &str) {
        if !enabled() {
            return;
        }
        let prim = matches!(
            name,
            "boolean" | "char" | "float" | "double" | "byte" | "short" | "int" | "long"
        );
        if prim { &PRIM } else { &REF }.fetch_add(1, Relaxed);
    }

    #[inline]
    pub(crate) fn count_miss() {
        if enabled() {
            MISS.fetch_add(1, Relaxed);
        }
    }

    /// An `anewarray` site built an owned `L…;` descriptor string for this allocation.
    #[inline]
    pub(crate) fn count_ref_desc_built() {
        if enabled() {
            REF_DESC_BUILT.fetch_add(1, Relaxed);
        }
    }

    pub(crate) fn report() {
        if !enabled() {
            return;
        }
        eprintln!(
            "[ARRAY-INTERN] lookups: prim={} ref={} (misses={}) anewarray_desc_strings_built={}",
            PRIM.load(Relaxed),
            REF.load(Relaxed),
            MISS.load(Relaxed),
            REF_DESC_BUILT.load(Relaxed),
        );
    }
}

/// DIAGNOSTIC: component-level breakdown of the live Rust-side heap. See `Heap::census`.
#[derive(Default, Clone)]
pub struct HeapCensus {
    pub live_objects: usize,
    pub dead_slots: usize,
    pub obj_slots: usize,
    pub field_slots: usize,
    pub spilled_objects: usize,
    /// Per-live-object fixed costs.
    pub obj_struct: usize,
    pub obj_box_tax: usize,
    pub obj_table: usize,
    pub obj_desc_table: usize,
    pub dead_struct: usize,
    /// FieldBlob backing allocations (one per non-empty object): the sum of
    /// `alloc_size(cap) + ALLOC_TAX`. Replaces the old three-SmallVec inline headers.
    pub field_blob_bytes: usize,
    pub lambda_count: usize,
    pub lambda_bytes: usize,
    /// Shared interned `LambdaSite` headers (deduped by Arc identity), counted once each.
    pub lambda_site_bytes: usize,
    /// Captured-context decomposition: total captured-Vec allocation bytes, a capture-arity
    /// histogram (0 pure / 1 / 2..=4 / >4) with (count, bytes), and the duplicate-capture estimate
    /// (lambdas of the same site with an IDENTICAL captured signature — the singleton opportunity).
    pub lambda_captured_bytes: usize,
    pub lambda_cap_hist: [(usize, usize); 4],
    pub lambda_capturing: usize,
    pub lambda_capture_distinct: usize,
    pub lambda_dup_bytes: usize,
    pub live_maps: usize,
    pub map_slots: usize,
    pub map_entry_count: usize,
    pub map_entries_bytes: usize,
    pub map_index_bytes: usize,
    pub map_key_str_bytes: usize,
    /// Map decomposition (FlatMap justification): entries Vec real len vs allocated capacity, and
    /// the index HashMap usable-slot capacity, so slack (empty entry slots + bucket overhead) is exact.
    pub map_entry_cap: usize,
    pub map_entry_slack_bytes: usize,
    pub map_index_slots: usize,
    /// Fill histogram by real entry count: 0 / 1 / 2..=4 / 5..=8 / >8 — (count, index+entries bytes).
    pub map_fill_hist: [(usize, usize); 5],
    pub live_arrays: usize,
    pub arr_slots: usize,
    pub arr_obj_table: usize,
    pub arr_desc_table: usize,
    /// (count, bytes) per ArrayData kind: byte/int/long/float/double/ref.
    pub arr_payload: [(usize, usize); 6],
    pub arr_payload_bytes: usize,
    /// GATE PROBE for the "inline small arrays" design: live arrays with len ≤ 8, and their
    /// current heap-payload bytes. Inline-≤8 would MOVE these into ArrayObj (growing the spine
    /// `arr_obj_table` by sizeof-per-slot) and SAVE their separate Vec allocs. Decides whether
    /// the change is a net win before touching the dangling descriptor-pointer-on-realloc hazard.
    pub arr_le8_count: usize,
    pub arr_le8_payload: usize,
    pub arr_le0_count: usize,
    /// REF-vs-PRIM decomposition of live arrays: (count, alloc_bytes, used_bytes, unused_cap_bytes).
    /// used = len·elem; unused = (cap−len)·elem; alloc = cap·elem + tax. Shows how much of the
    /// array payload is object-array cells vs primitives, and how much is slack capacity.
    pub arr_ref: (usize, usize, usize, usize),
    pub arr_prim: (usize, usize, usize, usize),
    /// Length histogram: buckets 0 / 1..=4 / 5..=8 / 9..=64 / >64 — (count, alloc_bytes) each.
    pub arr_len_hist: [(usize, usize); 5],
    pub field_box_bytes: usize,
    pub array_box_bytes: usize,
    /// Exact retained allocation of the three free-handle Vec<u32> containers.
    /// Counts duplicate dead-slot indices; the alive/descriptor tables already encode
    /// the same holes, so this is the upper bound for an intrusive free-chain win.
    pub free_handle_bytes: usize,
    /// (len, capacity) for object / array / map free-handle vectors.
    pub free_handles: [(usize, usize); 3],
    pub intern_bytes: usize,
    pub total: usize,
}
