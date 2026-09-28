//! `[PC-ARRAYS]` — the array-birth census, and the buffer pool it justifies.
//!
//! ⭐**4.18 MILLION ARRAYS PER `BootstrapBench` RUN, EACH ONE A HOST `Vec`.** The `[PC-BIRTH]`
//! census counts `newarray 3,029,533 + anewarray 1,150,369` from compiled code alone, every one
//! of them "never arena, one host Vec each", against `[PC-MALLOC] allocs=41.5 M frees=39.2 M`
//! for the whole run — most of what is allocated is freed inside the run, and an array is a
//! malloc at birth plus a free at the sweep. `tlab_alloc_array` has ZERO call sites. What was
//! missing before this module is WHICH arrays: by element type, by length, and how many of them
//! die before the run ends — three different fixes depending on the answer (a pool for the small
//! short-lived ones, a nursery for the long ones, nothing for the ones that live).
//!
//! **The census is always on** and uses plain cells: a relaxed `load`+`store` pair, never an
//! atomic RMW. It is lossless because every writer holds `&mut Heap` — array birth
//! (`Heap::put_array`) and array retirement (`retire_array_slot`) are both `&mut self` under the
//! GIL — and the only other reader is the periodic profiler dump, which tolerates a torn view.
//! Buckets: kind × length, where kind is the eight `newarray` primitive types (classified by the
//! element id `Heap::ensure_prim_element_ids` interned) plus `ref` (every `anewarray`), and
//! length is `0 | 1-4 | 5-16 | 17-64 | 65-256 | >256`.
//!
//! **The pool** (`RAVA_NO_ARENA_PRIMARRAY=1` or `RAVA_NO_EMIT_MORE=1` disables it) recycles the
//! backing `Vec` of a retired array instead of returning it to mimalloc, and hands it back to the
//! next birth of the SAME storage kind and the SAME length. Nothing about the array changes:
//! the cell encoding is untouched (`h | ARRAY_TAG`), the descriptor spine is untouched (compiled
//! code still reads `desc_base + h*16` for the data pointer and length), the `Vec` is still a
//! `Vec` with `capacity() == len()` exactly as `vec![0; len]` produces, and its contents are
//! zeroed before it is published. What goes away is one `mi_malloc` + one `mi_free` per pooled
//! array. Bit-exact by construction: the buffer a birth receives is indistinguishable from a
//! fresh one by any read the guest can make.
//!
//! ⭐**Why the pool is keyed by STORAGE KIND and not by element id.** `PRIM_ELEMENT_IDS` is a
//! process-global mirror of one heap's interner; a second heap in the same process (every unit
//! test) may number "int" differently. A pool keyed on that id could hand a `Vec<u8>` to an
//! `int[]`. The slabs are therefore chosen from the `ArrayData` VARIANT — the same authority
//! (`gens::empty_array`) that decides the storage kind on the historic path — and the id is
//! consulted only to LABEL the census row. A mislabelled row is cosmetic; a mis-kinded buffer
//! is not, and the guard cannot produce one.
//!
//! **The guard can only reject.** A buffer enters a slab only if its capacity is in
//! `1..=MAX_POOLED_LEN` and the slab is under its byte budget; a birth takes a buffer only from
//! the bucket whose capacity equals the requested length, in the slab of the variant it is
//! about to build. Every other case is the historic `vec![0; len]`.
//!
//! # Design note: arena-resident arrays (NOT this wave — the cell encoding is out of scope)
//!
//! The pool removes the malloc/free pair; it does not remove the handle. An array from compiled
//! code still costs a 32-byte `ArrayObj` slot, a 16-byte `ArrayDesc` slot, an `arr_alive` byte,
//! a `free_arr` push at the sweep, and a mark bit per collection — ~50 bytes of table per array
//! and O(arrays) sweep work, for 4.18 M arrays a run. HotSpot allocates a `double[4]` as 48
//! contiguous bytes in a TLAB and never touches a table. Doing the same here needs:
//!
//! 1. **A cell shape for an arena array.** JIT cells today: arena object = raw pointer,
//!    handle = `h | 1<<40`, array = `h | 1<<42`, box = `idx | 1<<41`; arena cells: `(h<<2)|1`,
//!    `(h<<2)|2`, `(idx<<2)|3`. Both encodings already agree that a bare 8-aligned pointer below
//!    2^40 is an arena OBJECT, so an arena ARRAY must be distinguishable from an arena object by
//!    its HEADER, not its cell: the header's class word would carry an array-kind marker and a
//!    length word would follow it. `tlab_alloc_array(element_is_ref, len)` already exists in
//!    `nextgen/tlab.rs` and has no caller.
//! 2. **Emitted code that shape-tests before the descriptor read.** `native_arraylength`,
//!    `native_primitive_load/store`, `native_aaload/aastore` (`jit/emit/arrays.rs`) all compute
//!    `desc_base + (cell & 0xFFFF_FFFF) * 16` after a single `hi == ARRAY_TAG` test. An arena
//!    array would take the `hi == 0` branch and read length and data from the header instead.
//!    That is a change to emitted code and to every callback that untags an array
//!    (`untag_array`), which is why it is a separate wave.
//! 3. **The collector.** A primitive arena array has no reference cells and needs no scan; a
//!    reference arena array must be walked by the arena mark (`gc_arena.rs`), and a HANDLE array
//!    holding an arena-array pointer is already handled by the `hi < 256` arm of
//!    `decode_ref_cell`. Evacuation would move the data, so no host may hold its base pointer
//!    across a safepoint — `array_base_ptr` (the data-parallel fork) is the one caller that does.
//!
//! The census below is what decides whether 1-3 are worth it: if the dominant population is
//! small AND short-lived, the pool captures most of the malloc cost and the table cost is what
//! remains; if it is long-lived, the pool captures nothing and only 1-3 help.

use crate::heap::*;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering::Relaxed};

// ---------------------------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------------------------

/// Tri-state like `FAST_ARRAYS`: 2 = unresolved, 1 = on, 0 = off. A relaxed byte rather than a
/// `OnceLock` because it is read once per array birth and once per array retirement, and an
/// acquire load on the birth path is the exact thing the heap's gate-word work removed. The
/// environment is still read exactly once.
static POOL_GATE: AtomicU8 = AtomicU8::new(2);

/// Is the recycle pool on? `RAVA_NO_ARENA_PRIMARRAY=1` or `RAVA_NO_EMIT_MORE=1` turns it off,
/// resolved once; off means every birth and every retirement takes the historic arm verbatim.
#[inline(always)]
pub(crate) fn enabled() -> bool {
    match POOL_GATE.load(Relaxed) {
        1 => true,
        0 => false,
        _ => init_gate(),
    }
}

#[cold]
#[inline(never)]
fn init_gate() -> bool {
    // ⭐**PRICED ON THE WRONG WORKLOAD FIRST, AND THAT IS THE LESSON.** On the bootstrap bench this
    // read +0.64/+1.08/-0.75% min and went opt-in -- but the bootstrap barely allocates arrays. On
    // the datapack reload, where `[PC-ARRAYS]` counts **8,482,575 array births a run** (ref
    // 5,166,358 of which 3,142,205 are length 1-4, int 1,481,967, double 921,539) and 94.2% die
    // inside the run, the same switch reads FASTER on every metric and both statistics:
    // RELOAD_MS -4.36/-4.54%, WINDOW_MS -2.53/-2.39%, RELOAD2_MS -2.66/-3.57%, cpu -2.11/-2.89%,
    // cycles -1.29/-2.73% -- ten signs out of ten. Default ON; `RAVA_NO_ARENA_PRIMARRAY=1` restores
    // the fresh-`Vec` arm verbatim.
    let on = std::env::var_os("RAVA_NO_ARENA_PRIMARRAY").is_none()
        && !crate::flags::emit_more_disabled();
    POOL_GATE.store(on as u8, Relaxed);
    on
}

// ---------------------------------------------------------------------------------------------
// Census cells
// ---------------------------------------------------------------------------------------------

/// Census kinds. 0..8 follow `PRIM_ELEMENT_IDS` order (atype 4..=11); 8 is every reference
/// element type; 9 is a primitive array born before the id mirror was filled (or in a heap the
/// mirror does not describe), which can only happen outside a product VM.
pub(crate) const KIND_NAMES: [&str; 10] =
    ["boolean", "char", "float", "double", "byte", "short", "int", "long", "ref", "prim?"];
const K_REF: usize = 8;
const K_UNKNOWN: usize = 9;
const KINDS: usize = KIND_NAMES.len();

pub(crate) const LEN_BUCKET_NAMES: [&str; 6] = ["0", "1-4", "5-16", "17-64", "65-256", ">256"];
const BUCKETS: usize = LEN_BUCKET_NAMES.len();

#[inline]
fn len_bucket(len: usize) -> usize {
    match len {
        0 => 0,
        1..=4 => 1,
        5..=16 => 2,
        17..=64 => 3,
        65..=256 => 4,
        _ => 5,
    }
}

type Row = [AtomicU64; BUCKETS];
type PerKind = [AtomicU64; KINDS];

static BIRTHS: [Row; KINDS] = [const { [const { AtomicU64::new(0) }; BUCKETS] }; KINDS];
static RETIRED: [Row; KINDS] = [const { [const { AtomicU64::new(0) }; BUCKETS] }; KINDS];
/// Births served from the pool (no host allocation), per kind.
static POOL_HIT: PerKind = [const { AtomicU64::new(0) }; KINDS];
/// Retired buffers the pool kept, per kind.
static POOL_KEPT: PerKind = [const { AtomicU64::new(0) }; KINDS];
/// Retired buffers refused: capacity outside `1..=MAX_POOLED_LEN`.
static POOL_DROP_LEN: PerKind = [const { AtomicU64::new(0) }; KINDS];
/// Retired buffers refused: the slab was at its byte budget.
static POOL_DROP_BUDGET: PerKind = [const { AtomicU64::new(0) }; KINDS];
/// Bytes currently held across all slabs, and the high-water mark.
static HELD_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

/// A plain-cell increment: `mov; add; mov`, no lock prefix. Sound because every caller holds
/// `&mut Heap` (see the module doc); the dump thread only reads.
#[inline(always)]
fn bump(c: &AtomicU64) {
    c.store(c.load(Relaxed).wrapping_add(1), Relaxed);
}

#[inline(always)]
fn add(c: &AtomicU64, n: u64) {
    c.store(c.load(Relaxed).wrapping_add(n), Relaxed);
}

#[inline(always)]
fn sub(c: &AtomicU64, n: u64) {
    c.store(c.load(Relaxed).saturating_sub(n), Relaxed);
}

/// Label a storage kind + element id as a census row. ≤3 integer compares; never a string.
#[inline]
fn kind_of(data: &ArrayData, element_id: u32, ids: Option<&[u32; 8]>) -> usize {
    match data {
        ArrayData::Ref(_) => K_REF,
        ArrayData::Float(_) => 2,
        ArrayData::Double(_) => 3,
        ArrayData::Long(_) => 7,
        ArrayData::Byte(_) => match ids {
            Some(ids) if ids[0] == element_id => 0,
            Some(ids) if ids[4] == element_id => 4,
            _ => K_UNKNOWN,
        },
        ArrayData::Int(_) => match ids {
            Some(ids) if ids[1] == element_id => 1,
            Some(ids) if ids[5] == element_id => 5,
            Some(ids) if ids[6] == element_id => 6,
            _ => K_UNKNOWN,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Slabs
// ---------------------------------------------------------------------------------------------

/// Longest array (in elements) the pool keeps. The census histogram's top pooled bucket is
/// `65-256`, so the pool covers exactly the lengths the histogram can vouch for.
pub(crate) const MAX_POOLED_LEN: usize = 256;

/// Byte budget PER SLAB (per storage kind). A retired buffer was live host memory a moment
/// before the sweep, so holding it does not raise peak RSS; the budget bounds what stays
/// resident after the population that needed it is gone. `pool_drop_budget` in the census says
/// whether this bound was ever the reason a buffer was refused.
pub(crate) const SLAB_BUDGET_BYTES_DEFAULT: usize = 8 << 20;

/// ⭐**AND THE CENSUS SAYS IT WAS THE REASON, CONSTANTLY.** One warm `ReloadBench` retires
/// 7,230,093 buffers and this bound refuses **3,659,420** of them — nearly twice the 1,944,008 the
/// pool actually serves. Every refusal is a host allocation on the next birth of that shape, and
/// arrays are the one population that can never be arena-allocated (see the comment on `0xbc` in
/// `jit/emit/ops/objects.rs`: an array is a tagged handle into the `ArrayDesc` spine, so bump
/// allocation would hand every consumer a raw pointer where it expects a handle index).
///
/// `RAVA_POOL_BUDGET_MB` makes the trade measurable inside one binary instead of assumed.
pub(crate) fn slab_budget_bytes() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RAVA_POOL_BUDGET_MB")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&m| m >= 1 && m <= 1024)
            .map(|m| m << 20)
            .unwrap_or(SLAB_BUDGET_BYTES_DEFAULT)
    })
}

/// Exact-capacity buckets for one element type: `buckets[c]` holds `Vec<T>`s with
/// `capacity() == c`. Allocated lazily on the first `give`, so a slab that never receives a
/// buffer costs three words.
struct Slab<T> {
    buckets: Vec<Vec<Vec<T>>>,
    held_bytes: usize,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self { buckets: Vec::new(), held_bytes: 0 }
    }
}

/// What `give` did with a retired buffer.
enum Gave {
    Kept(usize),
    DropLen,
    DropBudget,
}

impl<T: Copy + Default> Slab<T> {
    /// A zeroed `Vec<T>` with `len() == capacity() == len` plus its byte size, or `None`. The
    /// buffer is bit-identical to `vec![T::default(); len]`: `T::default()` is the all-zero bit
    /// pattern for every element type the heap stores (`u8`/`i32`/`i64`/`f32`/`f64`), and the
    /// exact-fit bucket guarantees `resize` cannot reallocate — so the data pointer the
    /// descriptor publishes is the pooled buffer's, stable for the array's lifetime like any
    /// Java array's.
    #[inline]
    fn take(&mut self, len: usize) -> Option<(Vec<T>, usize)> {
        let mut v = self.buckets.get_mut(len)?.pop()?;
        debug_assert_eq!(v.capacity(), len);
        let bytes = len * std::mem::size_of::<T>();
        self.held_bytes -= bytes;
        v.clear();
        v.resize(len, T::default());
        Some((v, bytes))
    }

    #[inline]
    fn give(&mut self, v: Vec<T>) -> Gave {
        let cap = v.capacity();
        if cap == 0 || cap > MAX_POOLED_LEN {
            return Gave::DropLen; // `cap == 0` owns no allocation; nothing to save
        }
        let bytes = cap * std::mem::size_of::<T>();
        if self.held_bytes + bytes > slab_budget_bytes() {
            return Gave::DropBudget;
        }
        if self.buckets.is_empty() {
            self.buckets.resize_with(MAX_POOLED_LEN + 1, Vec::new);
        }
        self.buckets[cap].push(v);
        self.held_bytes += bytes;
        Gave::Kept(bytes)
    }
}

/// The heap's recycle pool: one slab per storage kind, plus the cached primitive-id mirror the
/// census labels rows with. Lives in `Heap` (`Heap::array_pool`), so it is reached only through
/// `&mut Heap` — no lock, no TLS, no cross-thread ownership question.
#[derive(Default)]
pub(crate) struct ArrayPool {
    byte: Slab<u8>,
    int: Slab<i32>,
    long: Slab<i64>,
    float: Slab<f32>,
    double: Slab<f64>,
    refs: Slab<i64>,
    /// `PRIM_ELEMENT_IDS`, copied once it is set: one acquire load per heap lifetime instead of
    /// one per birth. `None` until `Heap::ensure_prim_element_ids` has run (VM construction).
    prim_ids: Option<[u32; 8]>,
}

impl ArrayPool {
    #[inline]
    fn prim_ids(&mut self) -> Option<&[u32; 8]> {
        if self.prim_ids.is_none() {
            self.prim_ids = PRIM_ELEMENT_IDS.get().copied();
        }
        self.prim_ids.as_ref()
    }

    /// Census: an array of `data`'s kind and length was born under `element_id`.
    #[inline]
    pub(crate) fn note_birth(&mut self, data: &ArrayData, element_id: u32) {
        let k = kind_of(data, element_id, self.prim_ids());
        bump(&BIRTHS[k][len_bucket(data.len())]);
    }

    /// Census: an array of `data`'s kind and length, born under `element_id`, is being retired.
    /// Returns the census row, which `give` reuses.
    #[inline]
    pub(crate) fn note_retire(&mut self, data: &ArrayData, element_id: u32) -> usize {
        let k = kind_of(data, element_id, self.prim_ids());
        bump(&RETIRED[k][len_bucket(data.len())]);
        k
    }

    /// A zeroed array of `probe`'s STORAGE KIND and `len` elements, served from the pool when a
    /// buffer of exactly that capacity is waiting, else built exactly as `gens::empty_array`
    /// builds it. `probe` is `empty_array(element, 0)` — an empty `Vec` of the right variant,
    /// which owns no allocation, so deriving the kind through it costs the string match the
    /// historic path paid anyway and nothing else.
    ///
    /// The probe, not the element id, chooses the slab: see the module doc for why.
    #[inline]
    pub(crate) fn take_or_fresh(&mut self, probe: ArrayData, len: usize, element_id: u32) -> ArrayData {
        let k = kind_of(&probe, element_id, self.prim_ids());
        macro_rules! arm {
            ($slab:expr, $variant:ident, $zero:expr) => {
                match $slab.take(len) {
                    Some((v, bytes)) => {
                        bump(&POOL_HIT[k]);
                        sub(&HELD_BYTES, bytes as u64);
                        ArrayData::$variant(v)
                    }
                    None => ArrayData::$variant(vec![$zero; len]),
                }
            };
        }
        match probe {
            ArrayData::Byte(_) => arm!(self.byte, Byte, 0u8),
            ArrayData::Int(_) => arm!(self.int, Int, 0i32),
            ArrayData::Long(_) => arm!(self.long, Long, 0i64),
            ArrayData::Float(_) => arm!(self.float, Float, 0f32),
            ArrayData::Double(_) => arm!(self.double, Double, 0f64),
            ArrayData::Ref(_) => arm!(self.refs, Ref, 0i64),
        }
    }

    /// A zeroed reference array (`0` = null cell) of `len`, from the pool or fresh. The
    /// `anewarray` site-memo entry (`Heap::alloc_ref_array_by_id`) has no element NAME, only an
    /// id whose contract is "a reference element type", so it is served from the `Ref` slab
    /// directly — the one kind that needs no probe.
    #[inline]
    pub(crate) fn take_ref_or_fresh(&mut self, len: usize) -> ArrayData {
        self.take_or_fresh(ArrayData::Ref(Vec::new()), len, u32::MAX)
    }

    /// Offer a retired array's backing buffer to its slab. `kind` is the census row the caller
    /// already resolved through `note_retire`. Whatever the slab refuses is dropped here —
    /// exactly the free the historic path did.
    #[inline]
    pub(crate) fn give(&mut self, data: ArrayData, kind: usize) {
        let gave = match data {
            ArrayData::Byte(v) => self.byte.give(v),
            ArrayData::Int(v) => self.int.give(v),
            ArrayData::Long(v) => self.long.give(v),
            ArrayData::Float(v) => self.float.give(v),
            ArrayData::Double(v) => self.double.give(v),
            ArrayData::Ref(v) => self.refs.give(v),
        };
        match gave {
            Gave::Kept(bytes) => {
                bump(&POOL_KEPT[kind]);
                add(&HELD_BYTES, bytes as u64);
                let held = HELD_BYTES.load(Relaxed);
                if held > PEAK_BYTES.load(Relaxed) {
                    PEAK_BYTES.store(held, Relaxed);
                }
            }
            Gave::DropLen => bump(&POOL_DROP_LEN[kind]),
            Gave::DropBudget => bump(&POOL_DROP_BUDGET[kind]),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------------------------

/// The `[PC-ARRAYS]` block for the periodic profiler dump; empty when nothing was born.
pub(crate) fn report() -> String {
    let load_rows = |rows: &[Row; KINDS]| -> Vec<[u64; BUCKETS]> {
        rows.iter().map(|r| std::array::from_fn(|b| r[b].load(Relaxed))).collect()
    };
    let sum = |cells: &PerKind| -> u64 { cells.iter().map(|c| c.load(Relaxed)).sum() };
    let births = load_rows(&BIRTHS);
    let retired = load_rows(&RETIRED);
    let tot_b: u64 = births.iter().flatten().sum();
    if tot_b == 0 {
        return String::new();
    }
    let tot_r: u64 = retired.iter().flatten().sum();
    let (hit, kept) = (sum(&POOL_HIT), sum(&POOL_KEPT));
    let (drop_len, drop_budget) = (sum(&POOL_DROP_LEN), sum(&POOL_DROP_BUDGET));
    let pct = |x: u64, of: u64| if of > 0 { x as f64 * 100.0 / of as f64 } else { 0.0 };
    let mut s = format!(
        "[PC-ARRAYS] array births {tot_b} | retired {tot_r} ({:.1}%) | live now {} | pool {}: served {hit} births ({:.1}%, no host alloc), kept {kept} of {} retired buffers, refused len {drop_len} budget {drop_budget}, holding {} KiB (peak {} KiB; budget {} KiB/kind, len<={})\n",
        pct(tot_r, tot_b),
        tot_b.saturating_sub(tot_r),
        if enabled() { "ON" } else { "OFF (RAVA_NO_ARENA_PRIMARRAY)" },
        pct(hit, tot_b),
        kept + drop_len + drop_budget,
        HELD_BYTES.load(Relaxed) >> 10,
        PEAK_BYTES.load(Relaxed) >> 10,
        slab_budget_bytes() >> 10,
        MAX_POOLED_LEN,
    );
    let line = |name: &str, b: &str, r: &str, l: &str, h: [String; BUCKETS], ph: &str, pk: &str| {
        format!(
            "[PC-ARRAYS]   {name:<8} {b:>10} {r:>10} {l:>10} |{:>9}{:>9}{:>9}{:>9}{:>9}{:>9} | {ph:>10} {pk:>10}\n",
            h[0], h[1], h[2], h[3], h[4], h[5]
        )
    };
    s.push_str(&line(
        "kind", "births", "retired", "live",
        std::array::from_fn(|b| LEN_BUCKET_NAMES[b].to_string()),
        "pool-hit", "pool-kept",
    ));
    let mut rows: Vec<usize> = (0..KINDS).filter(|&k| births[k].iter().sum::<u64>() > 0).collect();
    rows.sort_by_key(|&k| std::cmp::Reverse(births[k].iter().sum::<u64>()));
    for k in rows {
        let b: u64 = births[k].iter().sum();
        let r: u64 = retired[k].iter().sum();
        s.push_str(&line(
            KIND_NAMES[k], &b.to_string(), &r.to_string(), &b.saturating_sub(r).to_string(),
            std::array::from_fn(|i| births[k][i].to_string()),
            &POOL_HIT[k].load(Relaxed).to_string(), &POOL_KEPT[k].load(Relaxed).to_string(),
        ));
    }
    s
}
