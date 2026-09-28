//! The pinned region: allocation, the handle map and the mirror over it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

// ===== B2: PINNED non-moving region (handle→pointer migration, program charter =
// docs/gilfree-boot-rearch.md is the multicore charter; the representation charter
// is docs/representation-unification-{plan,spec}.md). ================================
//
// A process-global, low-address region for objects that carry a real arena-style
// header (so a reference to one is a raw low pointer, decoded like an arena object)
// but which the moving collector NEVER relocates. This is the foundation for
// representing today's HANDLE objects (`Heap::objects: Vec<ObjectSlot>`, reached via
// the `Value::Ref(u32)` tag) as ONE uniform untagged pointer form alongside arena
// objects — the representation-unification endgame.
//
// DEAD until a later brick allocates into it. The region is mapped LAZILY on the
// first `pinned_alloc`, so a run that allocates no pinned object (the canonical
// ChunkFill today) never advances `NEXT_LOW` and never perturbs arena object
// addresses → bit-exact `8790f04ace726ecf`. Because the region is carved from the
// same contiguous low band as young/old, `is_arena_obj_addr` already accepts a
// pinned address as deref-safe with NO change; and no live cell points into the
// region in canonical, so the collectors never encounter a pinned pointer (the
// non-move guard lands with the first pinned allocator, a later brick).
/// Backing store for the LOCK-FREE pinned bump allocator. The Region's VA is reserved
/// exactly ONCE (`pinned_region`'s OnceLock init); after that a single GIL writer bumps
/// `PINNED_OFF` (Relaxed `fetch_add`) and commits pages via `Region::ensure` before the
/// bytes are used. The `UnsafeCell` hands that single writer the `&mut Region` that
/// `ensure` needs for its `committed` bookkeeping — NO reader ever dereferences the
/// Region: `is_pinned` (the only concurrent reader) touches solely `PINNED_BASE`/`_TOP`.
/// This is the same single-writer / lock-free-reader discipline as the pinned mirror
/// table below (grow-only atomics under the GIL, no CAS).
pub(crate) struct PinnedHeap(std::cell::UnsafeCell<Region>);
// SAFETY: guest allocation is single-threaded under the GIL (workers never allocate
// pinned), so the `&mut Region` taken in `pinned_alloc` is the process's only mutable
// access. Concurrent readers never touch the Region, only the published atomics.
/// The pinned region, mapped on first use and never remapped. `OnceLock` init runs the
/// one-time reservation exactly once (its internal lock is paid ONCE, not per alloc);
/// every subsequent call is a lock-free atomic load of the initialized pointer.
pub(crate) fn pinned_region() -> &'static PinnedHeap {
    static P: OnceLock<PinnedHeap> = OnceLock::new();
    P.get_or_init(|| {
        let r = Region::new_reserved(PINNED_CAP);
        PINNED_BASE.store(r.base() as u64, Ordering::Relaxed);
        PINNED_TOP.store(r.base() as u64, Ordering::Relaxed);
        PinnedHeap(std::cell::UnsafeCell::new(r))
    })
}

/// True iff `a` is inside the LIVE (allocated) pinned region and 8-aligned. Lock-free
/// (two relaxed atomic loads); inert (false) until the first `pinned_alloc`.
#[inline]
pub fn is_pinned(a: u64) -> bool {
    let base = PINNED_BASE.load(Ordering::Relaxed);
    base != 0 && a >= base && a < PINNED_TOP.load(Ordering::Relaxed) && a & 7 == 0
}

/// Bytes handed out by the pinned bump allocator so far. The region NEVER frees, so
/// this is a monotonic high-water mark, and it is what bounds any born-pinned routing
/// policy: `PINNED_CAP` is a hard 256 MiB wall (`pinned_alloc` asserts on overflow).
#[inline]
pub fn pinned_used_bytes() -> usize {
    PINNED_OFF.load(Ordering::Relaxed)
}

/// Remaining pinned headroom in bytes.
#[inline]
pub fn pinned_free_bytes() -> usize {
    PINNED_CAP.saturating_sub(PINNED_OFF.load(Ordering::Relaxed))
}

/// Bump-allocate `size` bytes (rounded up to 8) in the pinned region, mapping it
/// lazily on first use. Returns an 8-aligned low-address pointer the collector never
/// moves. Called under the GIL (single-threaded guest allocation) — LOCK-FREE: the
/// one-time region map is behind a `OnceLock`, then each alloc is a Relaxed `fetch_add`
/// on `PINNED_OFF` (no per-call Mutex acquire). `Relaxed` suffices because the GIL
/// serializes this writer against the same thread's later reads, and `is_pinned`
/// readers synchronize through the `PINNED_TOP` store published AFTER `ensure` commits
/// the pages — the exact ordering the previous Mutex path used.
/// ⚠**INVARIANT: every byte handed out here must become a header-formatted object.**
/// `pinned_walk` enumerates this region by striding on each header's `nfields`, which is exact
/// only while nothing raw is interleaved. `pinned_object_alloc` is the sole caller for that
/// reason; a raw allocation here makes the walk misparse SILENTLY. It cost one test failure that
/// appeared only in a full run, which is the cheapest way this could ever have been learned.
pub fn pinned_alloc(size: usize) -> *mut u8 {
    let sz = (size + 7) & !7;
    let ph = pinned_region();
    // Single GIL writer: uncontended, monotonic bump. Reserve the range first.
    let off = PINNED_OFF.fetch_add(sz, Ordering::Relaxed);
    // SAFETY: the GIL makes this the only live mutable borrow of the Region; readers
    // (`is_pinned`) never touch it, only the atomics.
    let r = unsafe { &mut *ph.0.get() };
    assert!(off + sz <= r.cap(), "pinned region exhausted ({} + {} > {})", off, sz, r.cap());
    r.ensure(off + sz); // commit pages BEFORE the bytes (and PINNED_TOP) go live
    let p = unsafe { r.base().add(off) };
    PINNED_TOP.store(p as u64 + sz as u64, Ordering::Relaxed);
    p
}

/// ENUMERATE every object in the pinned region, oldest first — the OBJECT-level core that
/// `pinned_region_root_walk` (cells) and `pinned_census` (sizes) are both written on top of.
///
/// ⭐**It needs no side table, because of a property this region already has.** `pinned_alloc` is
/// a single monotonic bump over one contiguous `Region`, and its ONLY production caller is
/// `pinned_object_alloc` — every other reference in the tree is a test. So the region holds
/// nothing but header-formatted objects laid end to end, each carrying its own `nfields` at
/// `[p+4]`, and `obj_size` gives the stride. A linear walk is exact by construction rather than by
/// bookkeeping: there is no allocation to keep in step and nothing to invalidate.
///
/// ⚠**That soundness rests on the single-caller property**, so `pinned_alloc` states it as an
/// invariant: a future caller that writes raw bytes here makes the stride wrong and this walk
/// misparses the region silently. Such a caller must format a header or take its own region.
///
/// `visit` returns `false` to STOP the walk — how a caller that can recognise a malformed object
/// refuses to parse past it, without this level needing to know what "malformed" means.
///
/// CALL AT A SAFEPOINT. The bump is GIL-writer-only and `PINNED_TOP` is published after the bytes,
/// so a walk to the top observes only completely-written objects — but a concurrent allocation
/// would extend the region under the walker, so the top is read ONCE at entry.
#[cfg(feature = "next-gen-gc")]
pub fn pinned_walk(visit: impl FnMut(*mut u8, u32, u32) -> bool) -> usize {
    let base = PINNED_BASE.load(Ordering::Relaxed) as usize;
    if base == 0 {
        return 0; // region never mapped ⇒ no pinned object exists
    }
    pinned_walk_from(base, visit)
}

/// `pinned_walk` from an arbitrary object header, returning the offset it stopped at. The
/// collector wants the whole region; a TEST wants only the objects it allocated, because the
/// region is append-only and process-wide — walking all of it makes a test an assertion about
/// every other test's headers.
#[cfg(feature = "next-gen-gc")]
pub fn pinned_walk_from(start: usize, mut visit: impl FnMut(*mut u8, u32, u32) -> bool) -> usize {
    let top = PINNED_TOP.load(Ordering::Acquire) as usize;
    let mut p = start;
    while p + crate::nextgen::HDR <= top {
        let (cid, nf) = unsafe { (*(p as *const u32), *((p + 4) as *const u32)) };
        let size = crate::nextgen::obj_size(nf);
        // A zero-size stride would spin forever; a header whose object would overrun the published
        // top ends the walk rather than reading out of bounds.
        if size == 0 || size > top - p {
            break;
        }
        if !visit(p as *mut u8, cid, nf) {
            break;
        }
        p += size;
    }
    p
}

/// CENSUS (`RAVA_PINNED_CENSUS`, default OFF): how many objects the pinned region holds and how
/// many inline cells they carry — the size of the root set the walk above makes reachable.
#[cfg(feature = "next-gen-gc")]
pub fn pinned_census() -> (u64, u64) {
    let (mut objs, mut cells) = (0u64, 0u64);
    pinned_walk(|_, _, nf| {
        objs += 1;
        cells += nf as u64;
        true
    });
    (objs, cells)
}

/// B3 building block: allocate a header-formatted object in the pinned region — the
/// SAME `[class u32 | nfields u32 | fwd | hash | inline cells]` layout as an arena
/// object (see `HDR`), so a later brick can decode a reference to it with the exact
/// arena field-access path (`[obj + HDR + slot*8]`), collapsing the arena-vs-handle
/// discrimination. The forwarding word stays 0 (a pinned object never moves) and the
/// hash word starts unset (lazily stamped like arena). Inline cells are zeroed
/// (fresh reserved pages / the `Region` hands out zeroed memory). DEAD until a later
/// brick allocates a handle object's mirror here; unreferenced ⇒ canonical bit-exact.
pub fn pinned_object_alloc(class_id: u32, nfields: u32) -> *mut u8 {
    let p = pinned_alloc(obj_size(nfields));
    unsafe {
        *(p as *mut u32) = class_id; // [0] class
        *(p.add(4) as *mut u32) = nfields; // [4] nfields
        obj_init_hdr_tail(p); // forwarding (pinned never moves) + identity hash unset
        // Zero the inline cells explicitly. Fresh reserved pages are already zeroed,
        // but the pinned region's VA can (in principle) be recycled from a parked,
        // still-committed reserved span carrying stale bytes (RESERVED_POOL without
        // decommit). A born object's cells MUST start at the JVM default (0/null) —
        // the GC also traces ref cells by the class mask, so a stale non-zero cell
        // in a ref slot would be a wild forwardable pointer. Cheap: pinned objects
        // are immortal (allocated once), never on the hot mutator loop.
        for i in 0..nfields as usize {
            set_cell(p, i, 0);
        }
    }
    p
}

/// Allocate one zeroed chunk of `PMM_CHUNK` `AtomicU64` cells and leak it (leak-on-
/// grow, matching `arena_box`). Returns the data pointer to publish into the dir.
#[cold]
pub(crate) fn pmm_alloc_chunk() -> *mut AtomicU64 {
    let mut v: Vec<AtomicU64> = Vec::with_capacity(PMM_CHUNK);
    for _ in 0..PMM_CHUNK {
        v.push(AtomicU64::new(0));
    }
    debug_assert_eq!(v.len(), PMM_CHUNK);
    Box::into_raw(v.into_boxed_slice()) as *mut AtomicU64
}

/// B5: pinned mirror address → owning handle. The reverse of `pinned_mirror_map`,
/// required by (a) the LIVENESS bridge — a raw pinned pointer held as a root must
/// MARK its handle in the mark-sweep, else the object is swept out from under a
/// live pointer — and (b) the IDENTITY bridge — `ref_equal`/identity-hash/map-keys
/// must treat `Obj(mirror)` and `Ref(h)` as the SAME object during coexistence.
///
/// LEGACY PATH — retained only as the `RAVA_NO_FAST_GENHEAP=1` fallback for the direct-mapped
/// reverse table below, so the two can be bisected against each other. Exactly one of the two
/// is ever populated in a process (`fast_genheap` is read once).
pub(crate) fn pinned_handle_map() -> &'static Mutex<crate::FastMap<u64, u32>> {
    static M: OnceLock<Mutex<crate::FastMap<u64, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(crate::FastMap::default()))
}

// ═══════════ REVERSE MAP: pinned ADDRESS → owning handle, BY ADDRESS ARITHMETIC ═══════════
//
// ⭐**THE QUESTION IS "WHICH HANDLE OWNS THIS ADDRESS", AND THE ADDRESS ITSELF IS THE INDEX.**
//
// `pinned_handle_of` sits on the field-read path (`opcodes::fields`, `jit::callbacks::fields`,
// `jit::abi`, plus the map-key / `ref_equal` / stream bridges) and answered it by taking a
// PROCESS-GLOBAL MUTEX and hashing a 64-bit address in a `FastMap`. Two things were wrong,
// and both name a quantity that grows:
//
//   * **The lock scaled with ACCESSES; the answer scaled with nothing at all.** Every pinned
//     receiver paid an uncontended-but-unhoistable `lock`/`unlock` pair plus a hash of the
//     address, per field read — and pinned receivers are ordinary traffic, because born-pinned
//     non-capturing lambda singletons are DEFAULT-ON (`arena_lambda_singleton_enabled`). In the
//     default configuration the map being locked is PERMANENTLY EMPTY: its only two writers
//     (`Heap::alloc`'s reuse-hygiene clear and `prepopulate_known`'s mirror) both sit behind
//     `pinned_mirror_enabled()`, default OFF. The lock existed to answer "no".
//   * **The probe was itself a call.** Not inlined, so an ARENA (non-pinned) receiver — the
//     overwhelming majority of field reads — paid a call and return to run `is_pinned`'s two
//     relaxed loads and return `None`.
//
// WAS: a global-mutex acquire + address hash per field access on a pinned object, behind an
//      out-of-line call taken for EVERY object.
// IS:  one bounds-checked load from a direct-mapped table — no lock, no hash — with the
//      `is_pinned` rejection inlined into the caller.
//
// The key is the mirror's OFFSET IN THE PINNED REGION, `(addr - base) >> 3`, which is exact:
// the region is one contiguous bump span and `pinned_alloc` hands out 8-aligned addresses only.
// The table is CHUNKED and grow-only for the same reason `PMM_DIR` is — a chunk is published
// Release and never moves, so a lock-free reader may hold a chunk pointer while the single GIL
// writer maps a different chunk.
//
// **SIZE, against the rule that a new table must justify itself.** Directory: `PRM_DIR_LEN` =
// `PINNED_CAP` / `PRM_SPAN` = 256 MiB / 64 KiB = 4096 chunk pointers = **32 KiB of zero-init
// BSS**, demand-paged and never touched unless a mirror is recorded. A chunk is `PRM_CHUNK` ×
// `AtomicU32` = **32 KiB**, covers 64 KiB of pinned region, and is mapped only when a mirror
// lands in that span — so the default configuration maps ZERO chunks and the structure costs
// nothing resident. This is not a memo bolted onto a cheap path: it is the SAME map with the
// hash and the lock deleted, and it is also SMALLER, because the worst case (mirrors packed
// end to end) is 4 bytes of table per 8 bytes of region, against a `FastMap<u64,u32>` entry's
// 12 bytes of key+value plus its control byte and load-factor slack for a ≥16-byte object.
const PRM_CHUNK_BITS: u32 = 13;
const PRM_CHUNK: usize = 1 << PRM_CHUNK_BITS; // 8192 slots
const PRM_CHUNK_MASK: usize = PRM_CHUNK - 1;
/// Bytes of pinned region one chunk covers — one slot per 8-byte grain.
const PRM_SPAN: usize = PRM_CHUNK * 8;
/// Chunk pointers needed to span the whole reserved pinned region.
const PRM_DIR_LEN: usize = PINNED_CAP / PRM_SPAN;

/// `PRM_DIR[ci]` is null until a mirror is recorded in pinned span `ci`. Release-published /
/// Acquire-read, so a reader that observes a non-null chunk also observes its zeroed cells.
static PRM_DIR: [std::sync::atomic::AtomicPtr<std::sync::atomic::AtomicU32>; PRM_DIR_LEN] =
    [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; PRM_DIR_LEN];

/// KILL SWITCH for the direct-mapped reverse table — `RAVA_NO_FAST_GENHEAP=1` restores the
/// mutex+hash map verbatim. Also consults `emit_more_disabled` so one block A/B prices this
/// with the rest of the round. Read once into a `OnceLock`.
#[inline]
pub(crate) fn fast_genheap() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_GENHEAP").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Chunk index + slot for a KNOWN-pinned, 8-aligned address. `is_pinned` has already proved
/// `base <= addr < top <= base + PINNED_CAP`, so the index is in range by construction;
/// `PRM_DIR.get` re-proves it below without introducing a panic path on the read.
#[inline]
fn prm_index(addr: u64) -> Option<(usize, usize)> {
    let base = PINNED_BASE.load(Ordering::Relaxed);
    if base == 0 || addr < base {
        return None;
    }
    let idx = ((addr - base) >> 3) as usize;
    Some((idx >> PRM_CHUNK_BITS, idx & PRM_CHUNK_MASK))
}

/// Lock-free read. Cells hold `handle + 1` so that 0 — the state a fresh chunk and an unmapped
/// directory entry are already in — means ABSENT, exactly as the `FastMap` miss did. Same
/// bias-by-1 discipline as the `shrink-header` forwarding offset, and for the same reason:
/// zero must keep its existing meaning so no discriminator has to change.
#[inline]
fn prm_get(addr: u64) -> Option<u32> {
    let (ci, off) = prm_index(addr)?;
    let chunk = PRM_DIR.get(ci)?.load(Ordering::Acquire);
    if chunk.is_null() {
        return None; // no mirror was ever recorded in this 64 KiB span
    }
    match unsafe { (*chunk.add(off)).load(Ordering::Relaxed) } {
        0 => None,
        v => Some(v - 1),
    }
}

/// Publish `biased` (`handle + 1`, or 0 to clear) for pinned address `addr`, mapping the
/// covering chunk on first use. Single GIL writer ⇒ no CAS. `#[cold]`: this runs once per
/// mirror established or retired, never on a read.
#[cold]
fn prm_set(addr: u64, biased: u32) {
    let Some((ci, off)) = prm_index(addr) else { return };
    let Some(slot) = PRM_DIR.get(ci) else { return };
    let mut chunk = slot.load(Ordering::Acquire);
    if chunk.is_null() {
        if biased == 0 {
            return; // clearing a span that never held a mirror — nothing to map
        }
        let mut v: Vec<std::sync::atomic::AtomicU32> = Vec::new();
        v.resize_with(PRM_CHUNK, || std::sync::atomic::AtomicU32::new(0));
        chunk = Box::into_raw(v.into_boxed_slice()) as *mut std::sync::atomic::AtomicU32;
        // Release: a reader observing this pointer must observe the zeroed cells behind it.
        slot.store(chunk, Ordering::Release);
    }
    unsafe { (*chunk.add(off)).store(biased, Ordering::Relaxed) };
}

/// The handle owning pinned mirror `addr`, or None (not a live mirror address).
///
/// `#[inline]`: the whole body is `is_pinned`'s two relaxed loads plus, for a pinned address,
/// one directory load. The caller is a field read and the answer for an arena receiver is
/// "no" — which used to cost a call.
#[inline]
pub fn pinned_handle_of(addr: u64) -> Option<u32> {
    if !is_pinned(addr) {
        return None;
    }
    if fast_genheap() {
        return prm_get(addr);
    }
    pinned_handle_of_locked(addr)
}

/// `RAVA_NO_FAST_GENHEAP=1` path: the original global-mutex hash probe, kept out of line so an
/// ordinary build carries none of its code inside the inlined fast path.
#[cold]
#[inline(never)]
fn pinned_handle_of_locked(addr: u64) -> Option<u32> {
    pinned_handle_map().lock().unwrap().get(&addr).copied()
}

/// Record `handle`'s pinned mirror address (resizing the parallel table as handles
/// grow). Maintains the reverse map: a replaced/cleared mapping drops its old
/// reverse entry (slot-reuse hygiene — a stale reverse entry would resurrect the
/// PREVIOUS object's identity).
pub fn pinned_mirror_set(handle: u32, ptr: *mut u8) {
    // Single guest writer under the GIL: no CAS needed on the directory or cell.
    let ci = (handle >> PMM_CHUNK_BITS) as usize;
    let off = (handle & PMM_CHUNK_MASK) as usize;
    let mut chunk = PMM_DIR[ci].load(Ordering::Acquire);
    if chunk.is_null() {
        // Grow: allocate a fresh zeroed chunk and PUBLISH it Release so the lock-free
        // reader's Acquire load of the directory pointer sees the zeroed cells.
        chunk = pmm_alloc_chunk();
        PMM_DIR[ci].store(chunk, Ordering::Release);
        let used = PMM_DIR_USED.load(Ordering::Relaxed);
        if ci + 1 > used {
            PMM_DIR_USED.store(ci + 1, Ordering::Relaxed);
        }
    }
    let cell = unsafe { &*chunk.add(off) };
    // The address itself is a self-consistent u64 (0 or a valid pinned addr); the GIL
    // serializes this write against the same guest thread's later reads, so Relaxed
    // matches `is_pinned`'s discipline (the chunk-pointer Release/Acquire above is
    // what carries the memory-initialization happens-before).
    let old = cell.load(Ordering::Relaxed);
    cell.store(ptr as u64, Ordering::Relaxed);
    // REVERSE MAP. Nothing to record at all when the slot was empty and stays empty — which is
    // every call from `Heap::alloc`'s reuse-hygiene clear, i.e. the majority of them. The old
    // code took the global mutex BEFORE it could discover that.
    if old == 0 && ptr.is_null() {
        return;
    }
    if fast_genheap() {
        if old != 0 {
            prm_set(old, 0);
        }
        if !ptr.is_null() {
            // Bias by 1 so 0 keeps its "absent" meaning. `u32::MAX` is therefore not encodable;
            // assert LOUDLY rather than wrap, because a truncated reverse entry would give this
            // address the WRONG object's identity — corruption with no crash. Reaching it needs
            // 4 G live handles, which the heap cannot hold.
            assert!(
                handle != u32::MAX,
                "handle {handle} is not encodable in the pinned reverse table"
            );
            prm_set(ptr as u64, handle + 1);
        }
        return;
    }
    let mut r = pinned_handle_map().lock().unwrap();
    if old != 0 {
        r.remove(&old);
    }
    if !ptr.is_null() {
        r.insert(ptr as u64, handle);
    }
}

/// The pinned mirror address for `handle`, or 0 if none. Lock-free: one Acquire load
/// of the chunk pointer (0 if the handle's range was never written) + one Relaxed
/// load of the cell.
pub fn pinned_mirror_get(handle: u32) -> u64 {
    let ci = (handle >> PMM_CHUNK_BITS) as usize;
    let chunk = PMM_DIR[ci].load(Ordering::Acquire);
    if chunk.is_null() {
        return 0;
    }
    let off = (handle & PMM_CHUNK_MASK) as usize;
    unsafe { (*chunk.add(off)).load(Ordering::Relaxed) }
}

/// B3-collector: enumerate every live pinned mirror's REFERENCE cells (precise —
/// only slots the class ref_mask marks, so a primitive whose bits look like an
/// arena address can never be falsely forwarded). The moving collector calls this
/// to add mirror ref cells to its forwardable root set: without it, a GC that
/// moves an arena object leaves the mirror cell at the STALE pre-move address
/// (the diverge-hunt's `ListType#element` signature: mirror=old ptr, blob=new).
/// Called under the stop-the-world GIL; `f` receives a raw pointer to each cell.
pub fn pinned_mirror_ref_cells(mut f: impl FnMut(*mut i64)) {
    // Under the STW GIL — no concurrency, just a structural walk of the chunked table.
    // Bounded by the highest allocated chunk (0 in canonical ⇒ zero iterations).
    let used = PMM_DIR_USED.load(Ordering::Relaxed);
    for ci in 0..used {
        let chunk = PMM_DIR[ci].load(Ordering::Acquire);
        if chunk.is_null() {
            continue;
        }
        for off in 0..PMM_CHUNK {
            let mirror = unsafe { (*chunk.add(off)).load(Ordering::Relaxed) };
            if mirror == 0 {
                continue;
            }
            let p = mirror as *mut u8;
            let (cid, nfields) = unsafe { (*(p as *const u32), *(p.add(4) as *const u32)) };
            let (mask, uniform) = ref_mask_and_uniform(cid);
            for slot in 0..nfields as usize {
                if uniform || mask.get(slot / 64).map_or(false, |w| w & (1u64 << (slot % 64)) != 0) {
                    f(unsafe { p.add(HDR + slot * 8) } as *mut i64);
                }
            }
        }
    }
}

/// BORN-PINNED ROOT WALK: linearly sweep the CONTIGUOUS pinned region `[base, top)`,
/// parsing each self-describing object header (`[class|nfields|fwd|hash|inline cells]`,
/// the same format `pinned_object_alloc` writes) and invoking `f` on every REFERENCE
/// cell (precise — only the slots the class `ref_mask` marks, so a primitive whose bits
/// look like an arena address is never falsely forwarded).
///
/// A born-pinned object is IMMORTAL (the pinned bump allocator never frees) and never
/// moves, but its ref cells can hold a raw pointer to a MOVING-arena object that the
/// Cheney collector relocates. So the collector must treat EVERY born-pinned object as
/// a live root and FORWARD those cells in place — else a moved arena object reachable
/// ONLY through a pinned object is reclaimed out from under a live pointer (the same
/// hazard the mirror directory's `pinned_mirror_ref_cells` closes for dual-store).
///
/// The region is a PACKED sequence of `pinned_object_alloc` objects (in production the
/// only pinned allocators are born-pinned objects + the default-OFF mirror; both use
/// that header + `obj_size` stride, which is 8-multiple so the bump never inserts
/// padding). Bounded by `PINNED_TOP` (0 until the first pinned alloc ⇒ zero iterations,
/// canonical bit-exact). A malformed header (class id past the registry, or an
/// `obj_size` that would overrun `top`) STOPS the walk rather than risk an OOB read.
/// Called under the STW GIL (single writer, no concurrency).
pub fn pinned_region_root_walk(mut f: impl FnMut(*mut i64)) {
    let base = PINNED_BASE.load(Ordering::Relaxed) as usize;
    if base == 0 {
        return; // region never mapped ⇒ no pinned object exists
    }
    let nclasses = registry_len();
    let mut cells = 0u64;
    // The OBJECT enumeration is `pinned_walk`; this adds only the CELL selection, so the two
    // walks cannot drift apart in their idea of where an object ends.
    let end = pinned_walk_from(base, |p, cid, nf| {
        // Malformed-header guard: a class id past the registry has no ref mask to consult, so
        // stop rather than parse on.
        if cid >= nclasses {
            return false;
        }
        let (mask, uniform) = ref_mask_and_uniform(cid);
        for slot in 0..nf as usize {
            if uniform || mask.get(slot / 64).map_or(false, |w| w & (1u64 << (slot % 64)) != 0) {
                f(unsafe { p.add(HDR + slot * 8) } as *mut i64);
                cells += 1;
            }
        }
        true
    });
    // COLLECTOR ACCOUNTING: this walk is the pinned region's analogue of the tenured
    // remembered-set sweep, so routing a population from moving→pinned trades one
    // linear per-collection scan for the other. Counting both is what makes that
    // trade measurable instead of assumed.
    if gc_stats_enabled() {
        crate::nextgen::header::gcs::add(
            &crate::nextgen::header::gcs::PINNED_BYTES,
            (end - base) as u64,
        );
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::PINNED_CELLS, cells);
    }
}
