//! Field kinds, the field blob header and the object slot layout.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

/// FieldBlob — the three per-object field arrays (ids/cells/kinds) packed into ONE
/// heap allocation instead of three inline `SmallVec<[_;4]>` headers.
/// [[rava-heap-census-verdict]] measured those headers at 287MB (79% of obj.struct),
/// ~99MB of it PURE WASTE on the 1.13M zero-field lambdas that carry empty headers.
/// `Object` drops from 112B to ~32B (Arc<str> is a 16B fat ptr).
///
/// Layout (single allocation, 8-aligned):
/// ```text
///   [count:u32 @0][cap:u32 @4][ids:u32*cap @8][cells:i64*cap @(8+4*cap)][kinds:u8*cap @(8+12*cap)]
/// ```
/// `cap` is ALWAYS EVEN (or 0), so the cells section at `8+4*cap` stays 8-aligned for
/// the `i64` loads. An EMPTY blob points at a shared `static EMPTY_HDR` (count=cap=0)
/// and allocates NOTHING — the zero-field-object fast path (lambdas, freshly-alloc'd
/// objects before their fields are pushed). Every mutator grows-before-write so the
/// sentinel is never written; `Drop` frees only when `cap > 0`.
///
/// `ObjDesc` mirrors ids/cells/kinds as raw pointers INTO this one block. A `push`
/// that reallocs moves the block, so `sync_obj_desc` (already called after every push)
/// refreshes the descriptor — the IDENTICAL realloc hazard the old SmallVec spill had.
pub struct FieldBlob(pub(crate) std::ptr::NonNull<u8>);

#[repr(C, align(8))]
pub(crate) struct EmptyHdr {
    pub(crate) _count: u32,
    pub(crate) _cap: u32,
}
pub(crate) static EMPTY_HDR: EmptyHdr = EmptyHdr { _count: 0, _cap: 0 };

// SAFETY: the block is uniquely owned (deep `Clone`, no aliasing) except through the
// shared `*mut Vm` that is already governed by the GIL + stop-the-world contract — the
// same contract under which the existing `ArenaPtr` is Send+Sync.
impl Default for FieldBlob {
    fn default() -> Self {
        FieldBlob::new()
    }
}

impl Clone for FieldBlob {
    fn clone(&self) -> Self {
        let count = self.len();
        if count == 0 {
            return FieldBlob::new();
        }
        let mut b = FieldBlob::new();
        b.reserve(count);
        let cap = b.cap();
        unsafe {
            let base = b.0.as_ptr();
            *(base as *mut u32) = count as u32;
            std::ptr::copy_nonoverlapping(self.cells_ptr(), base.add(FieldBlob::cells_off()) as *mut i64, count);
            std::ptr::copy_nonoverlapping(self.ids_ptr(), base.add(FieldBlob::ids_off(cap)) as *mut u32, count);
            std::ptr::copy_nonoverlapping(self.kinds_ptr(), base.add(FieldBlob::kinds_off(cap)), count);
        }
        b
    }
}

impl Drop for FieldBlob {
    fn drop(&mut self) {
        let cap = self.cap();
        if cap > 0 {
            let layout = std::alloc::Layout::from_size_align(FieldBlob::alloc_size(cap), 8).unwrap();
            unsafe { std::alloc::dealloc(self.0.as_ptr(), layout) }
        }
    }
}

/// Pointer-sized object-table slot. `Some(Box<Object>)` has the exact pointer
/// representation consumed by the JIT's raw object-table base; `None` is a
/// swept hole. The transparent wrapper keeps the native table stride unchanged.
#[repr(transparent)]
pub(crate) struct ObjectSlot(pub(crate) Option<Box<Object>>);

impl ObjectSlot {
    #[inline]
    pub(crate) fn live(object: Object) -> Self {
        Self(Some(Box::new(object)))
    }

    #[inline]
    pub(crate) fn get(&self) -> Option<&Object> {
        self.0.as_deref()
    }

    #[inline]
    pub(crate) fn get_mut(&mut self) -> Option<&mut Object> {
        self.0.as_deref_mut()
    }
}

pub(crate) const FK_INT: u8 = 0;
pub(crate) const FK_LONG: u8 = 1;
pub(crate) const FK_FLOAT: u8 = 2;
pub(crate) const FK_DOUBLE: u8 = 3;
pub(crate) const FK_REF: u8 = 4;
pub(crate) const FK_ARRAY: u8 = 5;
pub(crate) const FK_NULL: u8 = 6;
pub(crate) const FK_BOX: u8 = 7; // index into Heap::field_box (Str / StdOut / StdErr)
/// next-gen-gc: a `Value::Obj` (arena pointer) stored RAW in the field cell — NOT a
/// `field_box` index. Arena pointers are 8-aligned and the GC forwards the cell in
/// place (`arena_root_locations`), so this avoids the append-only `field_box` growth
/// that an arena object stored into a handle field would otherwise leak per write.
#[cfg(feature = "next-gen-gc")]
pub(crate) const FK_OBJ: u8 = 8;

/// Flat, JIT-readable descriptor for an object handle's instance fields: the
/// (interned-id, flat-cell) arrays and their count. Lets compiled getfield scan
/// for a field id and read its cell natively. Pointers track the object's own
/// field vecs (stable across the `objects` Vec reallocating — a Vec move keeps
/// its heap buffer); refreshed whenever a field is pushed (which may realloc).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjDesc {
    /// = FieldBlob `ids_ptr` = blob base + 8 + 8*cap (cap-dependent, hence CACHED here so a field
    /// read needn't re-derive the offset for the id-verify scan).
    pub ids: *const u32,
    /// = FieldBlob `cells_ptr` = blob base + 8 (CELLS-FIRST: fixed offset). The blob header lives
    /// at [cells-8] (count u32) and [cells-4] (cap u32), so the JIT reads `count` as
    /// `load(cells-8)` — the SOURCE of truth (never a stale mirror). `kinds_ptr` (blob+8+12*cap)
    /// is dead on all read paths — the one writer (native_putfield_ref) re-derives `cells + 12*cap`.
    pub cells: *const i64,
}

impl ObjDesc {
    /// An empty/dead descriptor: `ids`/`cells` point at the shared FieldBlob sentinel so a
    /// (buggy) dead-handle access reads count 0 from [ids-8] instead of dereferencing null.
    pub(crate) fn empty() -> Self {
        let (ids, cells) = FieldBlob::sentinel_ptrs();
        ObjDesc { ids, cells }
    }
}

/// Native-map index key. `Int` is the alloc-free fast path for `Value::Int` keys
/// (the autobox-heavy HashMap<Integer,_> path — no `format!("i:{n}")` per op);
/// every other key (String/object/structural/Long) keeps its exact prior string
/// form as `Str`, so registry behaviour is byte-identical. (A top-level `Value::Int`
/// was already uniquely encoded `"i:n"`, so moving it to `Int(n)` collides with
/// nothing — including nested ints inside structural `Str` keys, which stay strings.)
#[derive(Hash, Eq, PartialEq, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum MapKey {
    Int(i64),
    Str(Box<str>),
    /// Alloc-free combo-map key: an order-independent u64 fold of a block-state
    /// property map's entries (Reference2ObjectArrayMap<Property,Comparable>),
    /// replacing the `m:{k=v,…}` canonical String + Box<str> that populateNeighbours
    /// churned by the million. A DISTINCT variant (never == Int/Str) so it cannot
    /// alias a real integer or string key; equal maps fold to the same u64 (same
    /// entry multiset → same leaf `structural_key` bytes → same hash), and a false
    /// collision can only occur WITHIN one map's index (≤~1300 states/block → 64-bit
    /// birthday ≈1e-13) and is caught by the bit-exact worldgen arbiter.
    Hash(u64),
    /// ALLOC-FREE IDENTITY KEY. A map probe whose semantics are reference identity — either the
    /// map IS one (`IdentityHashMap`, fastutil `Reference2*`) or the KEY's class inherits both
    /// `equals` and `hashCode` from `java/lang/Object` unchanged, which makes `HashMap` behave
    /// identically for it.
    ///
    /// ⭐**MEASURED, `bench/NativeCost.java`, compiled bodies, product config:** the same
    /// `HashMap`, the same bridge, the same call shape — `get` with bare `Object` keys 892 ns,
    /// `get` with `Integer` keys (which already took the alloc-free `MapKey::Int` path) 59 ns.
    /// 15x, and HotSpot does both in 2 ns. The cost was never the bridge; it was deriving a
    /// `String` per probe. Encoding mirrors the strings this replaces EXACTLY, so the two
    /// distinctions the old text drew are preserved: `@{h}` (a handle, and a pinned arena
    /// mirror bridged to its handle) becomes `Id(h)`, and `@a{hash}` (an arena object with no
    /// handle) becomes `Id(ARENA_TAG | hash)`. A DISTINCT variant, so it can alias neither a real
    /// integer key nor a structural string.
    Id(u64),
}

/// High bit marks an arena identity hash, so it can never collide with a handle id.
pub(crate) const MAPKEY_ARENA_TAG: u64 = 1 << 63;

impl Default for MapKey {
    fn default() -> Self {
        MapKey::Str(Box::from(""))
    }
}

impl MapKey {
    /// Build a `Str` key from any string-ish value (the registry/structural path).
    pub(crate) fn str(s: impl Into<Box<str>>) -> Self {
        MapKey::Str(s.into())
    }
    pub(crate) fn approx_len(&self) -> usize {
        match self {
            MapKey::Int(_) => 8,
            MapKey::Str(s) => s.len(),
            MapKey::Hash(_) => 8,
            MapKey::Id(_) => 8,
        }
    }

    #[inline]
    pub(crate) fn entry_bytes(&self) -> usize {
        32 + self.approx_len() + SLOT_BYTES * 2
    }
}

/// A grey-set entry during the GC mark phase.
pub(crate) enum Mark {
    Obj(u32),
    Arr(u32),
    Map(u32),
    /// next-gen-gc: a live arena object reached from a real root (or from a handle's
    /// FK_OBJ field / another arena object). Traversed for the handles it references,
    /// so the mark reaches arena-held handles WITHOUT the blind full-tenured scan.
    #[cfg(feature = "next-gen-gc")]
    Arena(usize),
}

/// Approximate bytes for one operand/field slot (`Value` plus container slack).
pub(crate) const SLOT_BYTES: usize = 24;

/// Spare `field_box` CAPACITY kept past a gil-free region's reservation, so a
/// GIL-holding sibling's own box push during the region cannot reallocate the Vec
/// out from under a lane worker writing its reserved slice. See
/// `Heap::reserve_field_box_range`.
#[cfg(feature = "next-gen-gc")]
pub(crate) const FIELD_BOX_REGION_HEADROOM: usize = 4096;

/// Modelled allocator overhead per SEPARATE heap allocation (header + size rounding).
/// The one non-exact number in `census`, reported on its own line so it can be subtracted.
pub(crate) const ALLOC_TAX: usize = 16;
