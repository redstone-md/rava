//! Array descriptors, their wire forms and the parallel cursor over them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::heap::*;

/// Backing store for a Java array. Primitive arrays use a packed native vector
/// (one machine word per element, not a 24-byte `Value`) — Minecraft's block
/// shapes build huge `boolean[]`/`int[]`/`double[]` (BitSets, voxel grids,
/// coordinate lists), so packing them cut heap use several-fold. Reference
/// arrays keep `Value`s. The public `array_*` API stays `Value`-based; this is
/// purely a storage detail.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ArrayData {
    /// `byte[]` and `boolean[]` (a byte read sign-extends; boolean is 0/1).
    Byte(Vec<u8>),
    /// `char[]`, `short[]`, `int[]`.
    Int(Vec<i32>),
    Long(Vec<i64>),
    Float(Vec<f32>),
    Double(Vec<f64>),
    /// Object/array element types, as flat tagged reference cells (REF_TAG handle
    /// / ARRAY_TAG handle / 0 null / ABOX_TAG index into `Heap::array_box`). Flat
    /// so compiled aaload reads an element with a native load + tag check. Decoded
    /// to/from `Value` by `Heap::{encode,decode}_ref_cell` (they need `array_box`).
    Ref(Vec<i64>),
}

pub struct ArrayObj {
    pub data: ArrayData,
}

pub(crate) const DEAD_ARRAY_ELEMENT_ID: u32 = u32::MAX;

/// ⭐**RAVA_NO_FAST_ARRAYS=1 restores the pre-round element paths verbatim.**
///
/// Everything this gate switches is *plumbing around* an element access, never the access's
/// meaning: the fast arms and the legacy arms produce the same `Value`, the same `array_box`
/// slot, the same `self.bytes` delta and the same descriptor. What the fast arms remove is the
/// repeated work — a second and third `self.arrays` lookup, a second and third switch on the
/// `ArrayData` discriminant, a bounds check already proven, and a store into the cacheline
/// compiled code reads.
///
/// ⛔**NOT A `OnceLock`, DELIBERATELY, AND THE TREE HAS PAID FOR THIS ONCE.** `array_get_bounded`
/// is 4.0% of the interpreter's hot window — one call per `aaload`/`iaload` — and a
/// `OnceLock::get_or_init` there is an *acquire* load plus a branch that LLVM may not hoist out
/// of the dispatch loop. A relaxed `AtomicU8` on x86 is a plain `mov` from a static that is
/// permanently L1-resident, and the branch predicts perfectly. Table size: **1 byte.**
/// (Ledger: one relaxed atomic re-introduced on the operand-stack push path measured +17%.)
static FAST_ARRAYS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

#[inline(always)]
pub(crate) fn fast_arrays() -> bool {
    match FAST_ARRAYS.load(std::sync::atomic::Ordering::Relaxed) {
        1 => true,
        0 => false,
        _ => fast_arrays_init(),
    }
}

/// Read the environment once, off the hot path. `emit_more_disabled()` is consulted because this
/// belongs to the same "emit more, cross less" block and has to fall with it.
#[cold]
#[inline(never)]
fn fast_arrays_init() -> bool {
    let on = crate::flags::fast_natives()
        && std::env::var_os("RAVA_NO_FAST_ARRAYS").is_none()
        && !crate::flags::emit_more_disabled();
    FAST_ARRAYS.store(on as u8, std::sync::atomic::Ordering::Relaxed);
    on
}

/// The 8 primitive array element ids (atype 4..=11), interned ONCE process-wide.
/// The JIT's native-load element-type gate (see `native_primitive_load`) needs
/// them as compile-time constants but has no `&mut Heap`; this mirror is filled
/// by `Heap::ensure_prim_element_ids` at VM construction, before any compile.
pub(crate) static PRIM_ELEMENT_IDS: std::sync::OnceLock<[u32; 8]> = std::sync::OnceLock::new();

/// Boundary DTO preserving the legacy snapshot field order and string wire type.
#[derive(serde::Serialize)]
pub(crate) struct SnapshotArrayRef<'a> {
    pub(crate) element: &'a str,
    pub(crate) data: &'a ArrayData,
}

/// Streaming view of the array spine. Bincode still sees the legacy sequence of
/// `{ element: string, data }` records, without a temporary DTO `Vec` per snapshot.
pub(crate) struct SnapshotArrays<'a>(pub(crate) &'a Heap);

/// Owned legacy snapshot shape used only while restoring into the runtime id table.
#[derive(serde::Deserialize)]
pub(crate) struct SnapshotArray {
    pub(crate) element: Arc<str>,
    pub(crate) data: ArrayData,
}

/// Flat, JIT-readable descriptor for an array handle. The final four bytes were
/// C-layout padding; they now own the array element-type id, shrinking ArrayObj
/// without changing the descriptor's 16-byte stride or native JIT access path.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ArrayDesc {
    pub data: *const u8,
    pub len: u32,
    pub element_id: u32,
}

pub(crate) const PARALLEL_ARRAY_CLAIM_CHUNK: usize = 8;
pub(crate) static NEXT_PARALLEL_ARRAY_EPOCH: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[derive(Clone, Copy)]
pub(crate) struct WorkerArrayCursor {
    pub(crate) heap: usize,
    pub(crate) epoch: u64,
    pub(crate) next: usize,
    pub(crate) end: usize,
}

impl WorkerArrayCursor {
    pub(crate) const EMPTY: Self = Self { heap: 0, epoch: 0, next: 0, end: 0 };
}
