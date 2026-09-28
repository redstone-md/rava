//! Object headers in the arena: hash, size, class, forwarding pointer and cells.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Zero the header's MUTABLE TAIL (forwarding word + identity hash) at allocation.
/// Both layouts zero bytes [8, HDR): the nursery is recycled by resetting `top`
/// WITHOUT wiping bytes, so a re-bumped slot would otherwise carry a stale forwarding
/// word (which `forward_if_moved` mines) or a stale identity stamp. One store under
/// `shrink-header`, two without.
#[inline]
pub unsafe fn obj_init_hdr_tail(p: *mut u8) {
    *(p.add(8) as *mut u64) = 0;
    #[cfg(not(feature = "shrink-header"))]
    {
        *(p.add(16) as *mut u64) = 0;
    }
}

#[inline]
pub unsafe fn obj_hash(p: *mut u8) -> u32 {
    *(p.add(HASH_OFF) as *mut u32)
}
#[inline]
pub unsafe fn obj_set_hash(p: *mut u8, h: u32) {
    *(p.add(HASH_OFF) as *mut u32) = h;
}

#[inline]
pub fn obj_size(nfields: u32) -> usize {
    HDR + nfields as usize * 8
}

/// Current arena high-water mark — the top of every reserved+committed region
/// (`NEXT_LOW` only grows, contiguously from `ARENA_FLOOR`). Anything at/above this
/// is reserved-but-uncommitted VA: a deref there faults, so it is the tight upper
/// bound for "safe to dereference an arena pointer".
#[inline]
pub fn arena_high_water() -> u64 {
    NEXT_LOW.load(Ordering::Relaxed)
}

/// Is `a` a plausibly-valid arena OBJECT pointer — safe to dereference its header?
///
/// The hot `(a >> 40) == 0` discriminator (inline field ops, `unbox_ref`) only proves
/// "not a high-word handle"; it ALSO accepts a small integer that leaked into a
/// reference slot (e.g. a stray `Value::Obj(4)`), whose "header" is an unmapped page →
/// a faulting read. Any boundary that derefs an UNTRUSTED `Value::Obj` must gate on
/// this first: a real arena object sits in `[ARENA_FLOOR, arena_high_water())` and is
/// 8-aligned. The high-water bound (vs a fixed `LOW_CEILING`) rejects the
/// reserved-but-uncommitted `[high_water, 2^40)` band, whose pages fault on read — it
/// matters most for the mark pass, which derefs every candidate cell.
///
/// ⭐**ORDER IS FREE; THE LOAD IS NOT.** Two of the three tests are register-only compares
/// against a compile-time constant; the third is `arena_high_water`, a relaxed load of a
/// global that the optimiser may neither hoist out of a caller's loop nor CSE across a call.
/// A conjunction short-circuits, so evaluating the constant tests FIRST means every candidate
/// that is a tagged handle or box cell (low bits set), a mis-shaped pointer, or a small
/// integer is rejected without touching memory at all — and those are the bulk of what the
/// sweep, the mark and the root scan feed in, one call per cell. Same three operands, same
/// verdict for every input: an evaluation order, not a behaviour, so no kill switch and no
/// new state.
#[inline]
pub fn is_arena_obj_addr(a: u64) -> bool {
    a >= ARENA_FLOOR && a & 7 == 0 && a < arena_high_water()
}

#[inline]
pub unsafe fn obj_class(p: *mut u8) -> u32 {
    *(p as *mut u32)
}
#[inline]
pub unsafe fn obj_nfields(p: *mut u8) -> u32 {
    *(p.add(4) as *mut u32)
}
#[cfg(not(feature = "shrink-header"))]
#[inline]
pub unsafe fn obj_fwd(p: *mut u8) -> *mut u8 {
    *(p.add(8) as *mut *mut u8)
}
#[cfg(not(feature = "shrink-header"))]
#[inline]
pub unsafe fn obj_set_fwd(p: *mut u8, f: *mut u8) {
    *(p.add(8) as *mut *mut u8) = f;
}

/// Narrow forwarding word (`shrink-header`): a u32 CELL OFFSET from `ARENA_FLOOR`,
/// biased by 1 so that 0 keeps its exact original meaning of "not forwarded" — the
/// allocation paths already zero this word, so every existing null-vs-non-null
/// discriminator (`copy_obj`, `major_copy`, `forward_if_moved`, `relocate`) is
/// preserved verbatim. A forwarding target is ALWAYS an 8-aligned address inside an
/// arena region, so `(addr - ARENA_FLOOR) >> 3` is lossless and spans 32 GiB of
/// arena VA — far beyond the RAVA_HARD_MB-bounded regions, and the encoder asserts
/// it rather than truncating (a truncated forward is the silent-corruption case).
#[cfg(feature = "shrink-header")]
#[inline]
pub unsafe fn obj_fwd(p: *mut u8) -> *mut u8 {
    let off = *(p.add(8) as *mut u32);
    if off == 0 {
        std::ptr::null_mut()
    } else {
        (ARENA_FLOOR + ((off - 1) as u64) * 8) as *mut u8
    }
}
#[cfg(feature = "shrink-header")]
#[inline]
pub unsafe fn obj_set_fwd(p: *mut u8, f: *mut u8) {
    let a = f as u64;
    // HARD (not debug_assert): a target below the floor would wrap the subtraction in
    // release and encode a garbage offset — a forward that silently resolves to the
    // wrong live object. Every real target is a `map_low` region address, so this can
    // only fire on a future path that breaks that invariant, and it must fire LOUDLY.
    assert!(
        a >= ARENA_FLOOR && a & 7 == 0,
        "forwarding target is not an 8-aligned arena address: {a:#x} (floor {ARENA_FLOOR:#x})"
    );
    let off = ((a - ARENA_FLOOR) >> 3) + 1;
    // LOUD, not silent: a truncated forwarding offset would leave referring cells
    // pointing at a wrong live object — corruption with no crash. Abort instead.
    assert!(
        off <= u32::MAX as u64,
        "arena forwarding offset {off} overflows u32 (target {a:#x}); arena VA exceeded 32 GiB \
         above ARENA_FLOOR — the shrink-header layout cannot encode it"
    );
    *(p.add(8) as *mut u32) = off as u32;
}
#[inline]
pub unsafe fn cell(p: *mut u8, i: usize) -> i64 {
    *(p.add(HDR + i * 8) as *mut i64)
}
#[inline]
pub unsafe fn set_cell(p: *mut u8, i: usize, v: i64) {
    *(p.add(HDR + i * 8) as *mut i64) = v;
}

/// Is `p` (already known to be inside an arena region) a PLAUSIBLE live object header
/// — class id within `nclasses` and a sane field count? The mark / conservative scan
/// uses this to reject a type-erased primitive that aliases an in-window 8-aligned
/// address (its bytes are object DATA, not a header → an absurd class id / field
/// count), so it never reaches the moving collector or a ref-cell walk as a bogus
/// root that would be dereferenced as garbage.
#[inline]
pub fn arena_obj_header_plausible(p: *mut u8, nclasses: u32) -> bool {
    // A real object header is 8-aligned. The callers gate on `young.contains(p)` (a
    // RANGE check that ignores alignment), so a type-erased primitive whose bytes land
    // on a MIS-aligned in-window address reaches here; reading `obj_class` on it aborts
    // ("misaligned pointer dereference … 0x200000235" at obj_class). A misaligned
    // address is never a live object → reject it before the deref.
    if (p as u64) & 7 != 0 {
        return false;
    }
    let (cls, nf) = unsafe { (obj_class(p), obj_nfields(p)) };
    cls < nclasses && nf <= (1 << 22)
}

/// Side table for arena cells that can't be packed into an i64 inline cell —
/// `Value::Str` / stdout-stderr sentinels (Phase 5 will move these into a proper
/// box-table migration). Append-only, addressed by `(idx << 2) | 0b11` in the
/// cell. The moving GC skips tagged cells (low bits non-zero), so boxed values
/// survive untouched; never compacted yet (acceptable boot-scale leak).
pub(crate) fn arena_box() -> &'static Mutex<Vec<crate::value::Value>> {
    static B: OnceLock<Mutex<Vec<crate::value::Value>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(Vec::new()))
}

/// Box a non-packable Value into the arena side table; returns its index.
pub fn arena_box_push(v: crate::value::Value) -> usize {
    let mut b = arena_box().lock().unwrap();
    b.push(v);
    b.len() - 1
}

/// HIGH-WATER of the box side table. This is the only cell arm with NO reclamation —
/// the table is append-only and never compacted, so the index a cell carries grows
/// monotonically with the PROCESS, not with the live set. Any proposal to narrow a cell
/// (and with it the box index's bit budget) has to be argued against this number, not
/// against the count of boxes that happen to be live at a collection.
pub fn arena_box_len() -> usize {
    arena_box().lock().unwrap().len()
}

/// Read a boxed Value back from the arena side table.
pub fn arena_box_get(idx: usize) -> crate::value::Value {
    arena_box()
        .lock()
        .unwrap()
        .get(idx)
        .cloned()
        .unwrap_or(crate::value::Value::Null)
}

/// Total arena objects bump-allocated (diagnostic — how many `new`s hit the TLAB).
pub static ARENA_ALLOCS: AtomicU64 = AtomicU64::new(0);

/// LIVE Cheney collection (Phase 3): evacuate every nursery object reachable from
/// `roots` (+ the card-table remembered set) into tenured, rewriting pointers, then
/// reset the nursery. `roots` is forwarded IN PLACE (each entry → its tenured copy).
/// Returns (objects_evacuated, tenured_bytes_used). Tenured overflow → hard exit
/// inside `copy_obj` (RAVA_HARD_MB, host-safe).
/// Monotonic GC epoch — bumped at the start of every `tlab_collect`. Consumers that
/// cache address-keyed data (e.g. the map value-key memo) tag entries with the epoch
/// so a collection (which may move/free objects) implicitly invalidates them.
pub(crate) static GC_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn gc_epoch() -> u64 {
    GC_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

// ═══════════════ COLLECTOR ACCOUNTING (RAVA_GC_STATS=1) ═══════════════
// A wall regression attributed to "the collector copies long-lived objects" was, until
// now, un-checkable: nothing counted collections, tenured sweep volume, or GC time. The
// two numbers that actually decide a lifetime-routing question are COLLECTION COUNT and
// the TENURED BYTES swept per collection — every MINOR pays an O(tenured) remembered-set
// sweep (`nursery_cap`'s own note: "collection COUNT is the dominant GC cost"), while an
// object that survives one minor is COPIED exactly once (`copy_obj` early-returns on a
// non-young pointer). Counters are relaxed atomics bumped once per COLLECTION, never per
// object on the mutator path, so the default build pays nothing measurable.
pub(crate) mod gcs {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub static MINORS: AtomicU64 = AtomicU64::new(0);
    pub static MAJORS: AtomicU64 = AtomicU64::new(0);
    /// Σ tenured bytes linearly swept by the remembered-set scan, over all minors.
    pub static REMSET_BYTES: AtomicU64 = AtomicU64::new(0);
    /// Σ bytes promoted young→tenured (the Cheney copy volume).
    pub static PROMOTED_BYTES: AtomicU64 = AtomicU64::new(0);
    pub static COPIED_OBJS: AtomicU64 = AtomicU64::new(0);
    /// Σ pinned-region bytes linearly swept by the born-pinned root walk.
    pub static PINNED_BYTES: AtomicU64 = AtomicU64::new(0);
    pub static PINNED_CELLS: AtomicU64 = AtomicU64::new(0);
    /// Σ nanoseconds inside `arena_collect_jit` (root gather + collect + writeback).
    pub static NANOS: AtomicU64 = AtomicU64::new(0);
    /// Arena-lambda births, split by region.
    pub static LAMBDA_MOVING: AtomicU64 = AtomicU64::new(0);
    pub static LAMBDA_PINNED: AtomicU64 = AtomicU64::new(0);
    /// Σ nanoseconds in the WHOLE closure-minting arm (`Plan::Lambda`), in BOTH
    /// representations, plus the two per-birth global-lock lookups the arena arm adds
    /// (`lambda_site_intern`, `lambda_class_id`). Attribution: a representation's cost
    /// is either its collector or its allocation path, and only measuring both says
    /// which. Timed with the SAME clock on both arms so the A/B is like-for-like.
    pub static LAMBDA_BIRTH_NS: AtomicU64 = AtomicU64::new(0);
    pub static LAMBDA_INTERN_NS: AtomicU64 = AtomicU64::new(0);
    pub static LAMBDA_CID_NS: AtomicU64 = AtomicU64::new(0);
    /// Closure-minting outcomes: a dedup cache HIT allocates nothing; a handle birth
    /// allocates on the Rust heap; an arena birth bumps the nursery.
    pub static LAMBDA_DEDUP_HIT: AtomicU64 = AtomicU64::new(0);
    pub static LAMBDA_HANDLE: AtomicU64 = AtomicU64::new(0);
    /// CONSUMER side: taking a closure SNAPSHOT (`heap::Lambda`) — the accessor every
    /// per-element guest apply goes through. Counted per representation, because that
    /// is where a representation change is paid millions of times over.
    pub static SNAP_ARENA_NS: AtomicU64 = AtomicU64::new(0);
    pub static SNAP_ARENA_N: AtomicU64 = AtomicU64::new(0);
    pub static SNAP_HANDLE_NS: AtomicU64 = AtomicU64::new(0);
    pub static SNAP_HANDLE_N: AtomicU64 = AtomicU64::new(0);
    /// …of which the interned-site table lookup (a process-global Mutex).
    pub static SITE_GET_NS: AtomicU64 = AtomicU64::new(0);
    #[inline]
    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Relaxed);
    }
    #[inline]
    pub fn get(c: &AtomicU64) -> u64 {
        c.load(Relaxed)
    }
}

#[inline]
pub fn arena_class_id(p: ArenaPtr) -> u32 {
    unsafe { obj_class(p.as_ptr()) }
}
#[inline]
pub fn arena_nfields(p: ArenaPtr) -> u32 {
    unsafe { obj_nfields(p.as_ptr()) }
}
#[inline]
pub fn arena_cell(p: ArenaPtr, slot: usize) -> i64 {
    unsafe { cell(p.as_ptr(), slot) }
}
#[inline]
pub fn arena_set_cell(p: ArenaPtr, slot: usize, v: i64) {
    unsafe { set_cell(p.as_ptr(), slot, v) }
}
