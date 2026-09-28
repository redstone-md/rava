//! The thread-local allocation buffer: init, cursor, and the allocation fast path.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;
use std::cell::RefCell;

/// Bump-allocate an arena array of `len` cells. Zeroed, so a reference array reads as all-null
/// before its first store — the same initial state `newarray`/`anewarray` must produce. Never
/// collects (pure bump), so nothing the caller holds can move before it writes the elements.
pub fn tlab_alloc_array(element_is_ref: bool, len: u32) -> Option<ArenaPtr> {
    tlab_try_alloc(arena_array_class(element_is_ref), len)
}

/// Both caps are ENV-DERIVED CONSTANTS, so they are read once per process rather than once per
/// heap. `tenured_cap` in particular is called from `worker_tlab_init`, i.e. on every worker's
/// entry into every parallel region — and `std::env::var` is a lock, a copy of the whole
/// environment block on Windows, and a `String` allocation for a number that cannot change.
pub(crate) fn tenured_cap() -> usize {
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| {
        let mb = std::env::var("RAVA_HARD_MB")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(4096);
        (mb * 1024 * 1024) / 2
    })
}
pub(crate) fn nursery_cap() -> usize {
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(nursery_cap_uncached)
}
fn nursery_cap_uncached() -> usize {
    // RAVA_NURSERY_BYTES (microscopic test) > RAVA_NURSERY_KB > 32 MiB default.
    // Every MINOR pays an O(tenured) remembered-set scan (the card table is still
    // dry-run), so collection COUNT is the dominant GC cost: growing the nursery
    // 4→32 MiB cut a server boot 166s→154s (measured A/B, log-timestamp walls);
    // 128 MiB measured the same 153s — 32 MiB is the knee, at negligible RSS cost.
    if let Some(b) = std::env::var("RAVA_NURSERY_BYTES").ok().and_then(|s| s.parse::<usize>().ok()) {
        return b.max(64);
    }
    if let Some(kb) = std::env::var("RAVA_NURSERY_KB").ok().and_then(|s| s.parse::<usize>().ok()) {
        return kb * 1024;
    }
    // GILFREE-RINGS BUDGET (the DEFAULT build takes this branch; `RAVA_NO_GILFREE_RINGS` opts out). The lane
    // carves a private nursery slice per task out of MAIN'S young and then holds a
    // shared overflow pool in what is left, for a batch of ~127 tasks that allocate
    // ~480 KB each — ~61 MB of simultaneously-live young, none of it collectable until
    // the region joins. Against the 32 MiB default the arithmetic does not close: an
    // earlier run left 26 of 128 tasks unclaimed for exactly this reason, and shrinking
    // the slice only moves the shortfall into the pool. The nursery knee was measured
    // at 32 MiB with 128 MiB scoring the same wall (153 s), so buying the headroom
    // costs RSS and nothing else.
    if crate::gilfree_cf::rings() {
        return 256 * 1024 * 1024;
    }
    32 * 1024 * 1024
}

/// KIT 2 (parallel pool): give the CURRENT thread a CLEAN, fixed-size nursery for a
/// GIL-free parallel region — each worker calls this on entry so it bump-allocates
/// from its own isolated `[base, end)` with no first-touch races. Replaces any
/// existing TLAB (freeing it); on a freshly spawned worker the slot is empty, so
/// this is the initial reservation. The cursor is republished so the next inline
/// bump (or `tlab_*_alloc`) sees the new extent.
#[allow(dead_code)]
pub fn worker_tlab_init(nursery_bytes: usize) {
    with_tlab(|slot| {
        *slot = Some(GenHeap::new(nursery_bytes.max(64), tenured_cap(), Vec::new()));
        refresh_cursor(slot.as_ref().unwrap());
    });
    // 11.2 (INERT): materialise this worker's per-thread IC table (allocate + snapshot-seed). Guarded
    // by `gilfree()` so the main-thread callers (tests) never allocate one. Unread this step.
    if crate::parallel::gilfree() {
        crate::jit::ic_worker_table_init();
    }
}

// ═══════════════ M3 Variant B scaffold: arena_collection_bridge (INERT) ═══════════════
// 9.2c.2a proved the shape-cache/worldgen heap churn is ~88% GENERIC JDK collections (Object[]/int[]/
// double[]/Map$Entry/ArrayList/Iterator/Stream), not geometry. Variant B redirects those TRANSIENT
// guest collections from the shared `Heap::objects` Vec into the per-thread arena TLAB (already
// isolated + GC-managed per worker). Double ROI: cuts single-thread heap-alloc/GC overhead now, and
// removes the shared-heap contention that blocks the initCache fork. Gated OFF; `try_arena_collection`
// returns None so every caller falls back to the heap path → byte-identical until the real impl lands.

/// G3 shared-nursery-partition: this thread's (main's) young nursery extent `(base, top, cap)`,
/// after syncing the inline cursor so `top` reflects any JIT bump-allocs. The driver partitions
/// `[top, cap)` into per-worker sub-ranges.
/// RSS-reconciliation probe: bytes the arena actually holds COMMITTED (page-backed), by domain:
/// `(young, old, pooled, perm)`. This is the number that counts against the Job-Object cap and
/// the OS RSS — it was NEVER logged before (RAVA_TRACE_ARENA prints only `old.top/cap`), which is
/// why the census↔RSS gap went undiagnosed. Reads this thread's TLAB (call from main) + the two
/// global pools; cheap enough for every census print.
/// P3-S1: process-lifetime dedicated SPILL region for worker TLAB-view refills (low VA, eager,
/// allocated once on first use, reused every fork region — the young nursery is typically ~full
/// at fork time so carving spill from it yields nothing). Sound for regions whose per-element
/// worker allocations never escape the element (chunk-fill: results are plain f64s; the shadow
/// is cleared per element) — nothing in main's heap ever references a spill address after the
/// region. 64MB.
/// DEOPT SCRATCH SLAB. Backing store for the compiled inline-alloc arm's BOUNDED
/// EXIT: when the safepoint reports it can free no space, the compiled body must
/// still be handed a writable, correctly-shaped object pointer or it will either
/// spin (the measured wall) or dereference null. The bytes are throwaway — the exit
/// is only ever taken after the region abort is raised, so every result computed
/// downstream is discarded and re-run serially.
///
/// The slab is per-thread, low-mapped (< 2^40 and 8-aligned, so the ABI
/// discriminator and any conservative GC scan read it exactly like a real arena
/// pointer) and WRAPS: successive exits alias each other, which is fine for bytes
/// nobody will keep, and makes the exit O(1) and unbounded in count. It is outside
/// every nursery extent, so no collector ever traces or moves it.
pub fn deopt_scratch(size: usize) -> usize {
    const SLAB: usize = 1 << 20;
    thread_local! {
        static SCRATCH: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    }
    SCRATCH.with(|c| {
        let (mut base, mut top) = c.get();
        if base == 0 {
            base = map_low_zeroed(SLAB) as usize;
            top = 0;
        }
        let size = (size + 7) & !7;
        if size > SLAB {
            // Larger than the whole slab: hand back the base and let it alias. A single
            // object that big is not something the ring bodies mint.
            c.set((base, 0));
            return base;
        }
        if top + size > SLAB {
            top = 0;
        }
        let p = base + top;
        c.set((base, top + size));
        p
    })
}

pub fn spill_region_base() -> (usize, usize) {
    static R: OnceLock<(usize, usize)> = OnceLock::new();
    *R.get_or_init(|| {
        let len = 64 * 1024 * 1024;
        (map_low_zeroed(len) as usize, len)
    })
}

/// ⚠**Never take the TLAB borrow here.** Callers are diagnostics, and every one of them runs
/// inside a collection that already holds it — the `with_tlab` version aborted a product boot
/// with `RefCell already borrowed` -> `panic in a function that cannot unwind`. `young`/`old`
/// now come from the lock-free extent table instead, as one figure (it does not distinguish
/// generations); the pooled and perm columns keep their own sources.
pub fn arena_committed_bytes() -> (usize, usize, usize, usize) {
    let young = crate::nextgen::region::reg_committed_total();
    let old = 0usize;
    // `try_lock`, for the same reason: a diagnostic must never be able to block the collection
    // it is reporting on. A contended read prints 0 for that column instead of deadlocking.
    let pooled = RESERVED_POOL
        .try_lock()
        .map(|p| p.iter().map(|&(_, _, c)| c).sum())
        .unwrap_or(0);
    let perm = perm_pool().try_lock().map(|g| g.0.top).unwrap_or(0);
    (young, old, pooled, perm)
}

pub fn young_extent() -> (usize, usize, usize) {
    with_tlab(|slot| {
        tlab_init_if_needed(slot);
        let h = slot.as_mut().unwrap();
        sync_young_from_cursor(h);
        (h.young.base() as usize, h.young.top, h.young.cap())
    })
}

/// G3: advance THIS thread's (main's) young `top` by `delta` — the driver calls this AFTER a
/// parallel region to consume the whole partitioned span (worker allocations already wrote into
/// it), so main's next alloc starts above them. Also republishes the inline cursor.
pub fn young_advance_top(delta: usize) {
    with_tlab(|slot| {
        if let Some(h) = slot.as_mut() {
            h.young.top = (h.young.top + delta).min(h.young.cap());
            refresh_cursor(h);
        }
    });
}

/// G1.4: retreat main's young `top` back to `target` (the pre-reservation value). Called on a fork
/// ABORT to UN-reserve the partition span — otherwise main's young top stays advanced past the
/// aborted region, whose bytes are partial/stale garbage, and the next GC scans them as if they were
/// live objects (mis-parsing garbage headers → heap corruption → the downstream enum/array crash).
/// Sound only on abort: nothing from the aborted fork is kept (the serial retry re-does the work).
pub fn young_set_top(target: usize) {
    with_tlab(|slot| {
        if let Some(h) = slot.as_mut() {
            if target <= h.young.cap() {
                h.young.top = target;
                refresh_cursor(h);
            }
        }
    });
}

/// G3: set THIS worker thread's TLAB to a non-owning view over `[base, base+cap)` of main's
/// young, and publish the inline cursor so compiled bump-allocs land there. The worker never
/// collects (aborts the region on young-full); reset with `worker_tlab_clear` after the region.
pub fn worker_tlab_view(base: usize, cap: usize) {
    with_tlab(|slot| {
        *slot = Some(GenHeap::view_young(base as *mut u8, cap));
        refresh_cursor(slot.as_ref().unwrap());
    });
    // Ownership tracking for the COW owned-bypass: this range is exclusively OURS.
    crate::parallel::owned_note(base, cap);
    // 11.2 (INERT): materialise this worker's per-thread IC table (the guest-fork worker path —
    // column / chunk-fill — enters via a young VIEW). Guarded by `gilfree()`; unread this step.
    if crate::parallel::gilfree() {
        crate::jit::ic_worker_table_init();
    }
}

/// G3: drop this worker thread's view TLAB (so a stale view over main's nursery is never reused).
pub fn worker_tlab_clear() {
    with_tlab(|slot| {
        *slot = None;
    });
    crate::parallel::owned_clear();
    // The handle-side twin dies with the view for the same reason: a claim of ownership must
    // not outlive the region that made it true, or a recycled slot would inherit it.
    crate::parallel::owned_handle_clear();
    JIT_CURSOR.with(|c| unsafe {
        let s = &mut *c.get();
        s[0] = 0;
        s[1] = 0;
    });
    // 11.2: eagerly free this worker's per-thread IC table on region exit (RAII Drop on thread
    // teardown is the backstop for pool workers that leave via `leave_region` without a clear).
    crate::jit::ic_worker_table_free();
}

/// KIT 2: pre-warm the shared lazy state a parallel region depends on, BEFORE the
/// workers fan out, so no two workers race to first-initialise it. Called by the
/// pool coordinator under the GIL. Today: force the arena-string class registration
/// (an idempotent `OnceLock`); future shared registrations hang here.
#[allow(dead_code)]
pub fn prewarm_for_parallel() {
    let _ = string_class_id();
}

pub(crate) fn tlab_init_if_needed(slot: &mut Option<GenHeap>) {
    if slot.is_none() {
        *slot = Some(GenHeap::new(nursery_cap(), tenured_cap(), Vec::new()));
        // The first heap to exist becomes the shared one (see `with_tlab`).
        if !crate::parallel::gilfree() {
            publish_shared_tlab();
        }
    }
}

// ═══════════════ Inline TLAB bump cursor (JIT, Phase Raw Speed) ═══════════════
// A thread-local `[next, end]` pair of ABSOLUTE addresses kept in lock-step with
// this thread's `young` nursery, so compiled code bump-allocates inline — load
// `next`, add the object size, compare against `end`, store the new `next` — with
// ZERO callback on the hot path. The mapping:
//     next = young.base + young.top      end = young.base + young.cap
// Coherence with the Rust path: `tlab_try_alloc` / `tlab_collect` SYNC `young.top`
// from the cursor on entry (so a Rust alloc never overwrites objects the JIT just
// bump-allocated) and REFRESH the cursor on exit (so the next inline bump sees the
// new top). The JIT calls `jit_cursor_addr()` ONCE per method to fetch the cursor's
// stable thread-local address, then bumps against `[addr]` / `[addr+8]` directly.
//
// ⭐**TLS SHAPE AUDIT — THIS ONE IS ALREADY THE FLOOR, AND DELIBERATELY SO.** A Rust
// `thread_local!` lowers to a bare `#[thread_local]` datum only when it is BOTH const-initialised
// AND destructor-free; anything else (a `RefCell`, a `Vec`, a `String`, any `Drop` payload) adds a
// lazy-init/destructor-state byte to EVERY access, and a `RefCell` adds a borrow-flag load, a
// compare, a store on entry and a release store on the guard's drop. `UnsafeCell<[u64; 2]>` is
// `Copy` data with no drop glue and is `const`-initialised, so:
//   * ACCESS LOWERS TO: the target's fixed TLS address chain (`_tls_index`, `gs:[0x58]`, the
//     image's block) — CSE'd once per function by the backend — then a plain 8-byte load or store
//     at a constant offset. No state byte, no borrow flag, no call, no "already destroyed" edge.
//   * BYTES: 16 per thread, one cache line, adjacent to nothing that is written by another thread.
//   * TOUCHED: twice per Rust object birth (`cursor_bump` reads `[0]`/`[1]` and stores `[0]`),
//     once per compiled-method prologue that inline-allocates (`jit_cursor_addr`), twice per
//     slow-path allocation (`sync_young_from_cursor` + `refresh_cursor`), once per GIL hand-off
//     (`retire_cursor`) — i.e. the hottest thread-local in the allocator.
// It is also why the JIT can hold the ADDRESS in a register across a whole method: a key with a
// destructor has no stable address to hand out before its state byte says "alive". Nothing here
// is to be given a `Cell<Option<..>>`, a `Vec` or a `RefCell` without re-reading this paragraph.
thread_local! {
    static JIT_CURSOR: std::cell::UnsafeCell<[u64; 2]> =
        const { std::cell::UnsafeCell::new([0, 0]) };
}

/// Republish `[base+top, base+cap]` into the cursor (after a Rust-path alloc or a
/// collection moved `young.top`). Cheap; called on the slow Rust path only.
#[inline]
pub(crate) fn refresh_cursor(h: &GenHeap) {
    let base = h.young.base() as u64;
    JIT_CURSOR.with(|c| unsafe {
        let s = &mut *c.get();
        s[0] = base + h.young.top as u64;
        s[1] = base + h.young.cap() as u64;
    });
}

/// Pull the cursor's `next` back into `young.top` so a Rust-path alloc starts ABOVE
/// any objects the JIT bump-allocated inline since the last sync. No-op until the
/// cursor has been published (`next != 0`).
#[inline]
pub(crate) fn sync_young_from_cursor(h: &mut GenHeap) {
    let next = JIT_CURSOR.with(|c| unsafe { (*c.get())[0] });
    if next != 0 {
        let base = h.young.base() as u64;
        // Published invariant: base <= next <= base + cap.
        h.young.top = (next - base) as usize;
    }
}

/// Fold this thread's inline bump cursor back into the heap and retire it.
///
/// ⚠**MANDATORY AT EVERY GIL HAND-OFF NOW THAT THE HEAP IS SHARED.** The cursor is per-thread
/// but `young.top` became common property: a cursor published before another thread allocated
/// would, on the next `sync_young_from_cursor`, REWIND `top` below live objects and hand the
/// same bytes out twice. Folding it in and zeroing makes the heap authoritative again; the next
/// allocation republishes from it. Costs one TLS read on a hand-off, which happens per safepoint
/// batch — never per allocation — and nothing at all while the cursor was never published.
pub(crate) fn retire_cursor() {
    let next = JIT_CURSOR.with(|c| unsafe { (*c.get())[0] });
    if next == 0 {
        return;
    }
    with_tlab(|slot| {
        if let Some(h) = slot.as_mut() {
            sync_young_from_cursor(h);
        }
    });
    JIT_CURSOR.with(|c| unsafe {
        let s = &mut *c.get();
        s[0] = 0;
        s[1] = 0;
    });
}

// ═══════════════ FAST BIRTH (kill switch `RAVA_NO_FAST_BIRTH=1`) ═══════════════
//
// ⭐**A BIRTH IS A BUMP, AND THE BUMP IS TWO WORDS.** The whole nursery frontier is the
// thread's published `[next, end]` cursor — the two `u64`s compiled code already loads,
// compares and stores inline with no callback. The Rust birth path reached those same two
// words through four layers the compiled path skips:
//
//   1. `TLAB.with(..)` — a `LocalKey` whose payload (`RefCell<Option<GenHeap>>`) OWNS a
//      `Region`, so it needs a destructor and the TLS access carries the lazy-init/registered-
//      dtor check rather than the bare `const`-init fast path the cursor's `UnsafeCell` gets;
//   2. `borrow_mut()` — a store of the borrow flag, a compare, and a second store on drop;
//   3. the shared-TLAB redirect — `gil::is_worker()`, `parallel::gilfree()`, an atomic load
//      and a TLS-address compare, on every allocation, to answer a question fixed for the
//      thread's whole time under the GIL;
//   4. `sync_young_from_cursor` + `refresh_cursor` — a THIRD and FOURTH thread-local touch
//      that pull `young.top` out of the cursor and immediately push the identical value back.
//
// So the fast path does what the JIT does: bump the cursor. `young.top` is then BEHIND the
// cursor, which is exactly the state a JIT inline bump already leaves and which every reader
// already repairs — `tlab_try_alloc`'s slow arm, `tlab_alloc_string`, `tlab_collect`,
// `major_collect`, `young_extent`, `retire_cursor` and `jit_cursor_addr` all call
// `sync_young_from_cursor` before they read `top`. No new invariant: one more caller of the
// existing one.
//
// ⚠**REFUSAL IS INVISIBLE.** Cursor not published (first birth on this thread, or the cursor
// was retired at a GIL hand-off) or the object does not fit — both return `None` having
// stored NOTHING, and the slow arm then decides between initialising the TLAB, claiming a
// spill chunk, aborting a GIL-free region and signalling a collection. Those are the paths
// that own that decision; the fast path never guesses at it.
//
// The gate also folds in every diagnostic that is armed OFF the birth path (the watch address
// and watch class, the struct-key histogram, the fork's arena-alloc counter), so an armed
// instrument silently takes the fully-instrumented slow arm rather than quietly reading zero.
#[inline]
pub(crate) fn fast_birth() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_BIRTH").is_none()
            && !crate::flags::emit_more_disabled()
            && !crate::structkey_scan::enabled()
            && watch_addr() == 0
            && watch_class().is_none()
            && !crate::parallel::arena_allocs_counted()
    })
}

// ═══════════════ BIRTH 3 — ONE GATE LOAD, NOT FIVE (`RAVA_NO_FAST_BIRTH3=1`) ═══════════════
//
// ⭐**THE BUMP IS SIX INSTRUCTIONS AND IT WAS GUARDED BY FIVE ACQUIRE LOADS.** `tlab_try_alloc`
// is the Rust birth path — every interpreted `new`, every `anewarray`, every native that mints
// an object — and before it reached `fast_birth`'s two-word bump it evaluated, in order:
//
//   1. `parallel::gilfree()` (a TLS `Cell`) and then `parallel::fork_debug_no_alloc()`
//      (`OnceLock<bool>` over `RAVA_FORK_DEBUG_NO_ALLOC`) — a BISECTION switch;
//   2. `structkey_scan::enabled()` — a second `OnceLock`, and one `fast_birth` ALREADY folds in;
//   3. `diag::births_arena()` — a third `OnceLock`, to reach a `Counter` whose `armed` flag is
//      false in every product build, so the `fetch_add` never runs but the handle is fetched;
//   4. `diag::births_arena_price().start()` — a fourth `OnceLock`, plus a four-field `MeterGuard`
//      that is constructed, held across the whole body, and dropped through a `Drop` impl;
//   5. `fast_birth()` — the fifth.
//
// The cost is not the five `mov`s. `OnceLock::get_or_init` opens with an ACQUIRE load, and an
// acquire load is an optimisation barrier: LLVM may not hoist it out of a caller's allocation
// loop, may not keep `young`/cursor state in registers across it, and must materialise the
// `MeterGuard` because its `Drop` observes memory. Five of them in front of a bump is the same
// shape the tenth round priced out of `Vm::jit_call`.
//
// Every one of the five answers a question that is FIXED FOR THE PROCESS — an env var or a
// registry `armed` bit decided before the first class loads — so they collapse into one
// `OnceLock<bool>` read at the top, and everything they guarded moves into `tlab_alloc_outline`.
// `gilfree()` is per-THREAD, not per-process, and is therefore NOT folded: it is only ever
// consulted in `&&` with the process-constant `fork_debug_no_alloc()`, so a true here proves
// that conjunction false and the whole test can go.
//
// ⚠**AN ARMED INSTRUMENT TURNS THE GATE OFF, IT DOES NOT READ ZERO.** `births_arena` and
// `births_arena_price` are folded in the same way `fast_birth` already folds the watch address,
// the watch class, the struct-key histogram and the fork's alloc counter: arming either one
// makes this gate false, so every birth takes the fully-instrumented outline arm and the
// instrument still counts every event. Removing this gate costs four acquire loads and a
// guard construct/drop on every object the interpreter builds.
#[inline]
pub(crate) fn fast_birth3() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        birth3_env()
            && fast_birth()
            && !crate::parallel::fork_debug_no_alloc()
            && !crate::diag::births_arena().armed()
            && !crate::diag::births_arena_price().armed()
    })
}

/// The kill switch itself, shared by the birth gate and the collector's mask walk so one env var
/// reverts the whole round. `emit_more_disabled()` kills it along with every other rewrite block.
#[inline]
fn birth3_env() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_BIRTH3").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// BIRTH 3, collector half: may `dfs_drain` read a class's reference mask by BORROWING the
/// process-lifetime `ClassMirror` instead of taking an `Arc<[u64]>` clone per traced object?
///
/// `fast_layout()` is required because it is the switch that decides whether class layout is
/// answered from the mirrors at all; with it off, `ref_mask_and_uniform` reads the registry under
/// its `Mutex`, and a fast arm that consulted the mirror anyway would make that kill switch a lie.
#[inline]
pub(crate) fn fast_trace3() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| birth3_env() && crate::nextgen::fast_layout())
}

/// Bump the published inline cursor by `size` and hand back the old frontier — the exact
/// sequence `emit_inline_arena_new` emits into machine code. `None` = not published, or would
/// not fit; both leave the cursor untouched so the caller can fall through invisibly.
#[inline(always)]
fn cursor_bump(size: usize) -> Option<*mut u8> {
    JIT_CURSOR.with(|c| unsafe {
        let s = &mut *c.get();
        let next = s[0];
        // `next == 0` (never published) is caught by the fit test too — `end` is 0 as well —
        // but it is kept explicit because this is the one place a wrong answer hands out a
        // pointer to address `size`.
        if next == 0 || next + size as u64 > s[1] {
            return None;
        }
        s[0] = next + size as u64;
        Some(next as *mut u8)
    })
}

/// PHASE-4 FIX — the AUTHORITATIVE bump, exposed so `HeapView` is a WINDOW over this
/// cursor instead of a second, competing one.
///
/// ⚠**THERE IS EXACTLY ONE NURSERY FRONTIER AND IT IS `JIT_CURSOR[0]`.** Compiled code
/// (`emit_inline_arena_new`) and the Rust birth path (`tlab_try_alloc` -> `cursor_bump`)
/// both bump these two words with no lock. A second cursor that tracked its own `bump`
/// and then wrote `young.top` from it would REWIND `top` below every object allocated
/// through this one — the corruption `retire_cursor`'s banner describes. So a caller that
/// wants raw bytes asks HERE; `align` is honoured for the caller's own layout, and the
/// same "stores nothing on refusal" contract as `cursor_bump` holds.
#[inline(always)]
pub(crate) fn cursor_bump_raw(size: usize, align: usize) -> Option<*mut u8> {
    debug_assert!(align.is_power_of_two(), "cursor_bump_raw: align must be a power of two");
    JIT_CURSOR.with(|c| unsafe {
        let s = &mut *c.get();
        let next = s[0];
        // Never published (first birth on this thread, or retired at a GIL hand-off).
        if next == 0 {
            return None;
        }
        let aligned = (next as usize).checked_add(align - 1)? & !(align - 1);
        let end = aligned.checked_add(size)?;
        if end as u64 > s[1] {
            return None;
        }
        s[0] = end as u64;
        Some(aligned as *mut u8)
    })
}

/// PHASE-4 FIX — fold `JIT_CURSOR[0]` into `young.top` and NOTHING ELSE.
///
/// This is `retire_cursor` WITHOUT the zeroing: the cursor stays published, so the next
/// bump continues from the same frontier. The point is the direction of the write — `top`
/// is pulled UP from the cursor, never pushed DOWN from an external counter, so it can
/// only ever move forward past live objects.
///
/// ⚠Takes the TLAB borrow (`with_tlab`): never call from inside a `with_tlab` closure or
/// from within a collection that already holds it (see `arena_committed_bytes`).
pub(crate) fn sync_young_from_cursor_only() {
    let next = JIT_CURSOR.with(|c| unsafe { (*c.get())[0] });
    if next == 0 {
        return;
    }
    with_tlab(|slot| {
        if let Some(h) = slot.as_mut() {
            sync_young_from_cursor(h);
        }
    });
}

/// Zero an object's MUTABLE BODY — bytes `[8, HDR + 8*nfields)` — in one contiguous run: the
/// forwarding word, the identity-hash word and every field cell together.
///
/// ⚠**THE BYTES CANNOT BE SKIPPED.** The nursery is recycled by resetting `top` WITHOUT wiping,
/// so a re-bumped slot carries a stale forwarding word (which `forward_if_moved` mines and the
/// evacuator reads as "already moved") and stale field bytes, and JVMS requires every field to
/// read its default before `<init>` writes it.
///
/// What IS removed is the shape of the write. The previous form was `obj_init_hdr_tail` (two
/// stores) followed by `for i in 0..nfields { set_cell(p, i, 0) }` — a counted loop with an
/// unknown trip count, which LLVM's loop-idiom pass rewrites into a `memset` CALL. The median
/// object born here carries a handful of cells, so that is a call plus a size dispatch to write
/// 16-48 bytes. The arms below cover 1-10 words (up to 8 fields under the 24-byte header)
/// as straight-line stores off one jump table, and only hand the rest to `memset`, where the
/// call really is cheaper than the stores it replaces.
#[inline(always)]
pub(crate) unsafe fn obj_init_body(p: *mut u8, nfields: u32) {
    let w = p.add(8) as *mut u64;
    // (HDR - 8)/8 mutable header words (2 normally, 1 under `shrink-header`) + one per field.
    let n = (HDR - 8) / 8 + nfields as usize;
    macro_rules! z {
        ($($i:literal),*) => {{ $( w.add($i).write(0); )* }};
    }
    match n {
        0 => {}
        1 => z!(0),
        2 => z!(0, 1),
        3 => z!(0, 1, 2),
        4 => z!(0, 1, 2, 3),
        5 => z!(0, 1, 2, 3, 4),
        6 => z!(0, 1, 2, 3, 4, 5),
        7 => z!(0, 1, 2, 3, 4, 5, 6),
        8 => z!(0, 1, 2, 3, 4, 5, 6, 7),
        9 => z!(0, 1, 2, 3, 4, 5, 6, 7, 8),
        10 => z!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9),
        _ => std::ptr::write_bytes(w, 0, n),
    }
}

/// Stamp a freshly bumped `size`-byte slot into a live arena object: the class/field-count
/// header word at `+0`, then the mutable body (`obj_init_body`).
///
/// The three bump arms in this file wrote these two lines out longhand, identically, which is
/// how a fourth arm drifts. Nothing about the emitted code changes — `#[inline(always)]` and
/// the same two statements in the same order — this is the single definition of what "born"
/// means on the Rust path, so the fast arm added below cannot disagree with the slow one about
/// the header layout.
///
/// SAFETY: `p` must be an 8-aligned, writable slot of at least `obj_size(nfields)` bytes that no
/// live object occupies.
#[inline(always)]
pub(crate) unsafe fn obj_init_new(p: *mut u8, class_id: u32, nfields: u32) {
    *(p as *mut u64) = (nfields as u64) << 32 | class_id as u64;
    obj_init_body(p, nfields);
}

/// Ensure the TLAB exists, publish its current extent into the cursor, and return
/// the cursor's STABLE thread-local address for compiled inline-bump code. Called
/// once at the entry of any compiled method that inline-allocates. The address is
/// stable for the thread's lifetime (an `UnsafeCell` that never moves); only its
/// `[next,end]` contents change, re-read fresh on every bump.
///
/// ⭐**PUBLISHED ONCE PER THREAD, NOT ONCE PER COMPILED ENTRY.** This runs in the prologue of
/// every compiled method that inline-allocates AND at every late-bound `new` site, and when the
/// cursor is ALREADY published its whole body is provably a no-op on the cursor: `sync` sets
/// `top = next - base`, `refresh` then writes back `base + top`, i.e. `next`, and `base + cap`,
/// which is what `end` already held (every replacement of the young region — `worker_tlab_view`,
/// `worker_tlab_init`, the spill refill, a snapshot restore — republishes or zeroes the cursor
/// on the spot). The only residue is `young.top`, and every reader of `young.top` syncs from the
/// cursor first. So a published cursor short-circuits to its address, and the `RefCell` borrow,
/// the shared-TLAB redirect and the two extra thread-local touches leave the compiled call path
/// entirely. Removing this makes every inline-allocating call pay a Rust heap round trip to
/// re-derive two words it already had.
pub fn jit_cursor_addr() -> i64 {
    let addr = JIT_CURSOR.with(|c| c.get());
    if fast_birth() {
        // SAFETY: `addr` is this thread's own `UnsafeCell<[u64;2]>`, live for the thread.
        if unsafe { (*addr)[0] } != 0 {
            return addr as i64;
        }
    }
    with_tlab(|slot| {
        tlab_init_if_needed(slot);
        // CRITICAL: pull the cursor's frontier into `young.top` BEFORE republishing.
        // A compiled method bumps `cursor.next` inline WITHOUT touching `young.top`,
        // so when such a method calls ANOTHER compiled method, this entry refresh
        // would otherwise set `cursor.next = base + young.top` — REGRESSING the
        // cursor below the objects the caller just inline-allocated. The nested
        // method then bump-allocates OVER a live object (e.g. a record's freshly
        // built instance lands on top of an existing Identifier), so its `putfield`
        // of that object's own pointer self-aliases → infinite `toString` recursion.
        // Syncing first makes the refresh idempotent: the cursor never moves
        // backwards across a nested compiled call.
        let h = slot.as_mut().unwrap();
        sync_young_from_cursor(h);
        refresh_cursor(h);
    });
    addr as i64
}

/// Try a thread-local pointer bump in the nursery. `None` when the nursery is full
/// (the caller then runs `arena_minor_gc` to evacuate + retry). NO sync.
///
/// ⭐**ONE GATE, THEN THE BUMP.** See the `fast_birth3` block: the five process-constant gates
/// this function used to read before it could reach a two-word cursor bump are now one
/// `OnceLock<bool>`, and everything they guarded lives in `tlab_alloc_outline`. Small enough to
/// inline into the interpreter's `new` and into every native that mints an object, which is the
/// point — the whole body on the common path is a gate load, a `HDR + 8*nfields`, a cursor load,
/// a compare, a store-back, the header word and `obj_init_body`'s straight-line zeroes.
///
/// ⚠A refusal from `cursor_bump` (cursor never published on this thread, or the object does not
/// fit) stores NOTHING and falls into the outline arm, which owns every decision that follows —
/// initialise the TLAB, claim a spill chunk, abort a GIL-free region, or signal a collection.
#[inline]
pub fn tlab_try_alloc(class_id: u32, nfields: u32) -> Option<ArenaPtr> {
    if fast_birth3() {
        if let Some(p) = cursor_bump(obj_size(nfields)) {
            unsafe { obj_init_new(p, class_id, nfields) };
            return ArenaPtr::new(p);
        }
    }
    tlab_alloc_outline(class_id, nfields)
}

/// Everything `tlab_try_alloc` used to do inline, verbatim and in the original order: the
/// bisection gate, the struct-key histogram, the birth counter and its price meter, the
/// pre-`fast_birth3` fast arm, and the full `with_tlab` slow path (TLAB init, cursor sync,
/// nursery-full → spill refill / region abort / collection signal).
///
/// `#[inline(never)]`: it is entered on a nursery-full, on the first birth of a thread whose
/// cursor is not yet published, and on every birth when a diagnostic is armed or the kill switch
/// is set. Keeping it out of line is what lets the caller above stay small enough to inline.
#[inline(never)]
fn tlab_alloc_outline(class_id: u32, nfields: u32) -> Option<ArenaPtr> {
    // G1.4 NO-ALLOC BISECTION GATE: a worker allocation aborts the region BEFORE any segment write.
    if crate::parallel::gilfree() && crate::parallel::fork_debug_no_alloc() {
        crate::parallel::request_abort();
        return None;
    }
    // M5.1 STRUCTKEY SCAN: inert per-class allocation histogram (this Rust-path alloc; the JIT inline
    // bump path is separate — the map-key histogram, which catches ALL keys, is the complete measure).
    if crate::structkey_scan::enabled() {
        crate::structkey_scan::note_alloc(class_id);
    }
    crate::diag::births_arena().inc();
    let _t = crate::diag::births_arena_price().start();
    // ⭐FAST BIRTH — the common case is a two-word bump on this thread's published cursor and
    // nothing else. See the `fast_birth` block above for why `young.top` may lag behind it and
    // why a refusal here is invisible. Kill switch: `RAVA_NO_FAST_BIRTH=1`.
    if fast_birth() {
        if let Some(p) = cursor_bump(obj_size(nfields)) {
            unsafe { obj_init_new(p, class_id, nfields) };
            return ArenaPtr::new(p);
        }
    }

    with_tlab(|slot| {
        tlab_init_if_needed(slot);
        let h = slot.as_mut().unwrap();
        // Pick up any objects the JIT bump-allocated inline since the last sync, so
        // this Rust alloc starts ABOVE them (never overwrites them).
        sync_young_from_cursor(h);
        let size = obj_size(nfields);
        if h.young.top + size > h.young.cap() {
            // Workers cannot collect while siblings are running. Continue from a
            // reserved spill chunk when available; a missing/undersized chunk is
            // the only overflow condition that aborts the region.
            if crate::parallel::gilfree() {
                if refill_worker_tlab(h, size) {
                    let p = unsafe { h.young.base().add(h.young.top) };
                    h.young.top += size;
                    unsafe { obj_init_new(p, class_id, nfields) };
                    refresh_cursor(h);
                    return ArenaPtr::new(p);
                }
                crate::parallel::request_abort_overflow();
            }
            return None; // nursery full → signal collection (or region abort)
        }
        let p = unsafe { h.young.base().add(h.young.top) };
        h.young.top += size;
        // The nursery is recycled by resetting top WITHOUT wiping bytes, so a reused slot
        // keeps stale header/hash bytes — `obj_init_new` zeroes the body explicitly.
        unsafe { obj_init_new(p, class_id, nfields) };
        let wa = watch_addr();
        if wa != 0 && p as u64 == wa {
            eprintln!("[WATCH] tlab_try_alloc addr={:#x} class_id={class_id} ({:?}) nfields={nfields}",
                p as u64, class_name(class_id));
        }
        // Republish the cursor so the next inline bump starts above this object.
        refresh_cursor(h);
        crate::parallel::count_arena_alloc();
        Some(ArenaPtr::new(p).expect("tlab pointer non-null"))
    })
}
/// Refill a worker's exhausted TLAB view from the already-reserved region spill pool.
/// Callers own the failure policy: a missing or undersized chunk aborts the region.
pub(crate) fn refill_worker_tlab(h: &mut GenHeap, needed: usize) -> bool {
    let chunk = crate::parallel::spill_chunk();
    if needed > chunk {
        return false;
    }
    let Some(base) = crate::parallel::spill_claim(chunk) else {
        return false;
    };

    *h = GenHeap::view_young(base as *mut u8, chunk);
    refresh_cursor(h);
    crate::parallel::owned_note(base, chunk);
    true
}

/// The current thread's nursery window `[base, base+cap)` as raw addresses, or
/// `(0, 0)` if no TLAB. A MINOR collection moves only objects in this window, so a
/// heap root cell needs forwarding iff its pointer falls here — letting the root
/// scan skip the (vastly more numerous) tenured-pointing cells. Fetched ONCE per
/// collection and inlined, avoiding a per-cell thread-local borrow.
pub fn tlab_young_window() -> (u64, u64) {
    with_tlab(|slot| {
        slot
            .as_ref()
            .map(|h| {
                let lo = h.young.base() as u64;
                (lo, lo + h.young.cap() as u64)
            })
            .unwrap_or((0, 0))
    })
}

/// Last observed nursery extent, refreshed on every UNCONTENDED `tlab_in_nursery`.
static NURSERY_LO: AtomicUsize = AtomicUsize::new(0);
static NURSERY_HI: AtomicUsize = AtomicUsize::new(0);

/// Is `p` in the current thread's nursery (i.e. would the next collection move it)?
///
/// ⚠**Must not panic when the TLAB is already borrowed.** The cell census walks the arena from
/// INSIDE a collection and asks this of every object; the borrowing version aborted a product
/// boot outright (`RefCell already borrowed` in a `nounwind` frame). The contended path answers
/// from the extent last published by an uncontended call — at most one call stale, and a
/// collection does not move the nursery until it finishes, so within a walk it is exact.
///
/// ⛔**TLS SHAPE AUDIT — EXPENSIVE, AND MEASURED NOT WORTH REWRITING.** This is the one reader in
/// this file that takes the full `TLAB` shape (destructor-state check + `try_borrow_mut` flag +
/// `Option` discriminant + two `Region` loads + two global relaxed atomics) for what is a two-word
/// range compare, and `Region::contains` is exactly `[base, base+cap)` — the same window
/// `tlab_young_window` returns — so a `const`-init, drop-free `[lo, hi]` mirror published in
/// `refresh_cursor` would serve it in a load, a load and two compares. **It is deliberately NOT
/// built, because every caller in a DEFAULT product build is off:** the compiled write barrier
/// (`jit::callbacks::alloc::rava_card_mark`) is emitted only when `card_mark_callback_enabled()`
/// is true, i.e. only under `RAVA_SHADOW_GC` / `RAVA_TRACE_STALESTORE` / the m3 probes; the
/// interpreted twin in `opcodes::arena_cells` is behind `shadow_enabled()`; `gc_sweep`'s use is
/// inside a `[DANGLING]` print; and `publish_new_site` asks once per compiled `new` SITE, not per
/// object. A mirror added here would be a side table on a path that does not run — the shape the
/// last two rounds priced at +3.0% and +2.3% and demoted. Build it when the card table stops
/// being a dry run and the barrier becomes default-ON; the argument for its soundness is written
/// down above (`base`/`cap` change only where `refresh_cursor` is already called: `GenHeap::new`,
/// `view_young`, the spill refill, a snapshot restore — `top` alone moves otherwise).
pub fn tlab_in_nursery(p: ArenaPtr) -> bool {
    if let Some(hit) = try_with_tlab(|slot| {
        let h = match slot.as_ref() {
            Some(h) => h,
            None => return false,
        };
        // ⚠A write barrier calls this (`jit::callbacks::alloc`), so the publish must not dirty a
        // cache line per call. The nursery moves only when a region is (re)mapped, so compare
        // first and store on the rare change.
        let lo = h.young.base() as usize;
        if NURSERY_LO.load(Ordering::Relaxed) != lo {
            NURSERY_LO.store(lo, Ordering::Relaxed);
            NURSERY_HI.store(lo + h.young.cap(), Ordering::Relaxed);
        }
        h.young.contains(p.as_ptr())
    }) {
        return hit;
    }
    let a = p.as_ptr() as usize;
    a >= NURSERY_LO.load(Ordering::Relaxed) && a < NURSERY_HI.load(Ordering::Relaxed)
}

thread_local! {
    /// Per-thread TLAB: young nursery (bump) + tenured. Lazily allocated. A full
    /// nursery does NOT panic — `tlab_collect` (Phase 3) evacuates live survivors
    /// to tenured (Cheney) and resets the nursery. Tenured overflow → hard exit.
    ///
    /// ⚠**THE EXPENSIVE SHAPE, AND IT MUST STAY THAT SHAPE.** `GenHeap` owns two `Region`s, and a
    /// `Region` owns a RESERVED/COMMITTED VA span that has to be released when the thread dies —
    /// so this key needs a destructor, and a `thread_local!` with a `Drop` payload cannot lower to
    /// a bare `#[thread_local]` load: every access carries the lazy-init/destructor-state check as
    /// well. The `RefCell` on top adds a borrow-flag load, a compare, a store on entry and a store
    /// on the guard's drop. Both are load-bearing: the flag is what turns a re-entrant collector
    /// callback into a diagnosable `try_borrow` refusal (`try_with_tlab`, `tlab_in_nursery`)
    /// instead of two live `&mut GenHeap`s, and it caught exactly that twice (see the ⚠ notes on
    /// `arena_committed_bytes` and `tlab_in_nursery`).
    ///
    /// ⭐**SO THE FIX WAS TO STOP TOUCHING IT, NOT TO CHEAPEN IT — and that is already done.** The
    /// birth path reaches `[next, end]` through `JIT_CURSOR` (a `const`-init, drop-free
    /// `UnsafeCell<[u64;2]>`, see FAST BIRTH), so a common-case object birth never names this key
    /// at all: `with_tlab` is entered only to initialise a TLAB, to refill from the spill pool, to
    /// abort a GIL-free region, to signal a collection, or from the collector/census/snapshot
    /// paths — none of them per-object. `refresh_cursor` republishes the two words on the way out,
    /// which is what keeps the drop-free mirror authoritative for the next N births.
    pub(crate) static TLAB: RefCell<Option<GenHeap>> = const { RefCell::new(None) };
}

/// The FIRST thread's TLAB cell, as a raw address — the heap every GIL-serialised
/// thread shares (0 until the first `tlab_init_if_needed`).
static SHARED_TLAB: AtomicUsize = AtomicUsize::new(0);

/// ⭐⭐**THE HEAP THIS THREAD MUST USE — and for an ordinary worker that is MAIN'S, not its own.**
///
/// ⛔Row 425: `TLAB` is a `thread_local`, so each thread owned a separate nursery AND a separate
/// tenured space, while `minor_collect` forwards only `h.young.contains(child)` and sweeps only
/// its own `h.old`. **A store from one thread's nursery into another heap's object was therefore
/// invisible to both collectors** — the offloaded reload's dangling
/// `Suppliers$MemoizingSupplier.value`, and the reason it survived only with a nursery too large
/// to ever collect.
///
/// Sharing one heap removes the cross-heap edge by construction rather than by tracking it: the
/// GIL already guarantees a single thread runs bytecode and allocates at a time, so the shared
/// heap has exactly the single-writer discipline the private ones had.
///
/// ⚠**TWO EXEMPTIONS, both deliberate.** A GIL-FREE region worker keeps its private view — that
/// lane is built around zero-cap views and abort-on-overflow, and it does not hold the GIL. And
/// the OWNING thread reaches its cell through the normal `RefCell`, so it keeps the borrow check
/// (and the panic) it has today; only a foreign thread takes the raw path, under the GIL.
pub(crate) fn with_tlab<R>(f: impl FnOnce(&mut Option<GenHeap>) -> R) -> R {
    // ⚠**ONLY A REGISTERED VM WORKER SHARES.** The GIL is what makes one heap safe, and only a
    // thread that takes the GIL is bound by it — `cargo test` runs its cases on plain threads
    // that never do, and routing those onto one heap raced instantly (STATUS_ACCESS_VIOLATION on
    // the first run). `is_worker()` is set exactly on the daemon/task-runtime threads that
    // execute guest code under the GIL; main IS the shared heap, so it takes the normal path.
    if crate::gil::is_worker() && !crate::parallel::gilfree() {
        let shared = SHARED_TLAB.load(Ordering::Relaxed);
        if shared != 0 {
            let mine = TLAB.with(|t| t as *const RefCell<Option<GenHeap>> as usize);
            if shared != mine {
                // SAFETY: the owner's TLAB lives in its TLS block for the thread's lifetime, and
                // the GIL serialises every access — the same basis `frames::all` reads other
                // threads' frame stacks on.
                return f(unsafe { &mut *(shared as *mut RefCell<Option<GenHeap>>) }.get_mut());
            }
        }
    }
    TLAB.with(|t| f(&mut t.borrow_mut()))
}

/// `with_tlab` that reports a re-entrant borrow instead of panicking on one. For diagnostics
/// that run inside a collection; see `tlab_in_nursery`.
pub(crate) fn try_with_tlab<R>(f: impl FnOnce(&mut Option<GenHeap>) -> R) -> Option<R> {
    if crate::gil::is_worker() && !crate::parallel::gilfree() {
        let shared = SHARED_TLAB.load(Ordering::Relaxed);
        if shared != 0 {
            let mine = TLAB.with(|t| t as *const RefCell<Option<GenHeap>> as usize);
            if shared != mine {
                // SAFETY: identical to `with_tlab` — the owner's TLAB outlives the thread and the
                // GIL serialises access.
                return Some(f(unsafe { &mut *(shared as *mut RefCell<Option<GenHeap>>) }.get_mut()));
            }
        }
    }
    TLAB.with(|t| t.try_borrow_mut().ok().map(|mut b| f(&mut b)))
}

/// Publish this thread's TLAB as the shared one, if nobody has yet. Called from
/// `tlab_init_if_needed`, so the first thread to allocate (main) wins.
fn publish_shared_tlab() {
    let mine = TLAB.with(|t| t as *const RefCell<Option<GenHeap>> as usize);
    let _ = SHARED_TLAB.compare_exchange(0, mine, Ordering::Release, Ordering::Relaxed);
}
