//! parallel.




use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Threads currently executing inside a GIL-free parallel region.
static IN_REGION: AtomicUsize = AtomicUsize::new(0);
/// A stop-the-world is in progress: region threads must park at the next safepoint.
static STOP: AtomicBool = AtomicBool::new(false);
/// Region threads currently parked at a safepoint.
static PARKED: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    /// Nesting depth for this execution thread. Only the outermost exit may
    /// publish thread-local region telemetry.
    pub(crate) static REGION_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(crate) fn enter_region() {
    REGION_DEPTH.with(|depth| depth.set(depth.get().wrapping_add(1)));
    IN_REGION.fetch_add(1, Ordering::SeqCst);
}
pub(crate) fn leave_region() {
    let outermost = REGION_DEPTH.with(|depth| {
        let current = depth.get();
        debug_assert!(current > 0, "unbalanced parallel region exit");
        depth.set(current.saturating_sub(1));
        current == 1
    });
    if outermost {
        flush_hot_cow_stats();
    }
    IN_REGION.fetch_sub(1, Ordering::SeqCst);
}
pub(crate) fn region_count() -> usize {
    IN_REGION.load(Ordering::SeqCst)
}
/// Whether any thread is running GIL-free right now (callers can keep the GIL
/// fast paths when this is false, i.e. always today).
pub(crate) fn any_parallel() -> bool {
    IN_REGION.load(Ordering::Relaxed) > 0
}

/// Op-loop safepoint for a region thread. Cheap (one Acquire load) unless a
/// stop-the-world is in progress, in which case it parks until `resume()`.
#[inline]
pub(crate) fn safepoint() {
    if STOP.load(Ordering::Acquire) {
        PARKED.fetch_add(1, Ordering::SeqCst);
        while STOP.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        PARKED.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Request a stop-the-world and block until `others` region threads have parked
/// at their safepoints. The caller (the GC driver) is NOT one of `others`
/// (it does not park). After this returns the caller runs its exclusive work
/// and must call `resume()`.
pub(crate) fn stop_the_world(others: usize) {
    STOP.store(true, Ordering::Release);
    while PARKED.load(Ordering::Acquire) < others {
        std::hint::spin_loop();
    }
}
/// End the stop-the-world: parked region threads resume.
pub(crate) fn resume() {
    STOP.store(false, Ordering::Release);
}

/// A GIL-free parallel region has hit an unrecoverable condition mid-flight —
/// today the only trigger is a worker's TLAB overflowing (a GIL-free thread
/// cannot run the stop-the-world collector itself, since that needs ALL roots
/// parked). The worker sets this; the pool coordinator polls it, tears the
/// region down, and the driver re-runs the loop serially under the GIL. A
/// process-wide flag is sound because regions do not nest (one `parallel_for`
/// at a time; the coordinator clears it before each region).
static REGION_ABORT: AtomicBool = AtomicBool::new(false);
/// Distinguishes an abort caused by a worker's NURSERY-VIEW OVERFLOW (the per-worker
/// segment was too small for this leaf's allocation) from an uncompiled-callee guard
/// abort. DIAGNOSTIC ONLY (GAP-S05): abort kind/history never routes future admission.
static REGION_OVERFLOW: AtomicBool = AtomicBool::new(false);
/// DIAGNOSTIC (always on — these are `fetch_add`s on an already-cold abort path, never
/// a hot-loop cost). Aggregate abort accounting for the fork.
///
/// ⚠ READ THIS BEFORE INTERPRETING A ZERO. Every fork entry point is OPT-IN
/// (`RAVA_FORK_SHAPES` / `RAVA_FORK_PARALLEL` / `RAVA_FORK_NUMERIC` / the
/// `RAVA_M3_CHUNK_FILL_*` chain),
/// so on a DEFAULT boot no region is ever opened and these are 0 BY CONSTRUCTION — that
/// says nothing about whether aborts would happen, only that no fork ran. A zero is
/// evidence only on a run with a fork gate actually set.
///
/// RAISE counts every worker's signal (N workers can raise for one region); TEARDOWN
/// counts REGIONS actually torn down, read at `clear_abort` — that is the number the
/// abort→serial-retry thesis is about.
static ABORT_RAISED: AtomicUsize = AtomicUsize::new(0);
static OVERFLOW_RAISED: AtomicUsize = AtomicUsize::new(0);
static REGION_TEARDOWN: AtomicUsize = AtomicUsize::new(0);
static REGION_TEARDOWN_OVERFLOW: AtomicUsize = AtomicUsize::new(0);

/// P3-S1 SPILL POOL: a shared bump-cursor span of main's young nursery, reserved at region
/// open, from which a worker whose per-thread TLAB view overflows claims a FRESH view instead
/// of aborting. This dissolves the whole garbage-continuation class for overflow: the alloc
/// callback simply retries into the new view and the compiled kernel continues normally — no
/// fault flag, no unwinding through machcode. (base, cursor, end) as absolute addresses;
/// zeroed outside a region.
static SPILL_BASE: AtomicUsize = AtomicUsize::new(0);
static SPILL_CUR: AtomicUsize = AtomicUsize::new(0);
static SPILL_END: AtomicUsize = AtomicUsize::new(0);
/// Refill granularity. The S2 fork's pool is a dedicated 64 MB low region and a
/// 1 MB chunk is free there; the rings lane's pool is carved out of MAIN'S YOUNG
/// (its results ESCAPE the region into futures, so they must be ordinary young
/// objects the collector owns), where 1 MB × 127 tasks would not fit. So the
/// granularity is a property of the pool, published by whoever opened it.
static SPILL_CHUNK: AtomicUsize = AtomicUsize::new(1 << 20);
/// Chunks handed out since the pool opened — the direct measure of how far a
/// region's real allocation footprint overshoots its per-task view.
static SPILL_CLAIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) fn spill_setup(base: usize, len: usize, chunk: usize) {
    SPILL_BASE.store(base, Ordering::Release);
    SPILL_CUR.store(base, Ordering::Release);
    SPILL_END.store(base + len, Ordering::Release);
    SPILL_CHUNK.store(chunk.max(4096), Ordering::Release);
    SPILL_CLAIMS.store(0, Ordering::Release);
}
pub(crate) fn spill_clear() {
    SPILL_BASE.store(0, Ordering::Release);
    SPILL_CUR.store(0, Ordering::Release);
    SPILL_END.store(0, Ordering::Release);
    SPILL_CHUNK.store(1 << 20, Ordering::Release);
}
/// The open pool's refill granularity (1 MB when no pool is open).
#[allow(dead_code)]
pub(crate) fn spill_chunk() -> usize {
    SPILL_CHUNK.load(Ordering::Acquire)
}
/// (chunks claimed, bytes still unclaimed) — the region ledger's budget line.
#[allow(dead_code)]
pub(crate) fn spill_stats() -> (u64, usize) {
    let end = SPILL_END.load(Ordering::Acquire);
    let cur = SPILL_CUR.load(Ordering::Acquire);
    (SPILL_CLAIMS.load(Ordering::Acquire), end.saturating_sub(cur))
}
#[allow(dead_code)]
pub(crate) fn note_spill_starved() {
    crate::parallel::context::SPILL_STARVED.with(|c| c.set(true));
}
#[allow(dead_code)]
pub(crate) fn take_spill_starved() -> bool {
    crate::parallel::context::SPILL_STARVED.with(|c| c.replace(false))
}
#[cfg(feature = "next-gen-gc")]
fn spill_zero_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SPILL_ZERO").is_some())
}

/// Read-once gate for the `[LANE-ABORT-SITE]` print (`RAVA_LANE_ABORT_SITE`, or
/// the broader `RAVA_FORK_TRACE`).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lane_abort_site_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_LANE_ABORT_SITE").is_some()
            || std::env::var_os("RAVA_FORK_TRACE").is_some()
    })
}

/// Has an abort been requested for the current region? (Polled by workers + the
/// coordinator on the safepoint path.)
#[allow(dead_code)]
pub(crate) fn abort_requested() -> bool {
    REGION_ABORT.load(Ordering::Acquire)
}
/// Was the current region's abort a nursery-view overflow (vs a G1.1 guard abort)?
#[allow(dead_code)]
pub(crate) fn overflow_requested() -> bool {
    REGION_OVERFLOW.load(Ordering::Acquire)
}
/// Is THIS thread executing guest bytecode GIL-free right now?
///
/// ⭐**AUDITED SHAPE (dispatch-side thread-local sweep): THIS IS ALREADY THE CHEAP SHAPE AND
/// NOTHING WAS DONE TO IT.** `GILFREE` (`parallel/stats.rs`) is a `Cell<bool>` declared with a
/// `const { .. }` initialiser, and `Cell<bool>` has no `Drop`. That is the one combination `std`
/// lowers to a bare `#[thread_local]` static: on x86_64-pc-windows-msvc the access is the
/// `_tls_index` load, the `gs:[0x58]` TEB read and a `movzx` of the byte — **no `LocalKey::with`
/// call, no lazy-initialisation state word, no destructor registration, and no `RefCell` borrow
/// flag.** It is also a PLAIN load, not an atomic one, so LLVM may CSE and hoist it: a function
/// that asks twice pays once, and a loop that asks every iteration pays once. There is no cheaper
/// representation of a per-thread bool available, so this reads as one load and stays as it is.
///
/// Consequently `owns_reserved_segment()` (heap object/array birth) and every one of the 110
/// call sites reached from allocation, dispatch and the seam are already paying a single load.
#[inline]
pub(crate) fn gilfree() -> bool {
    GILFREE.with(|g| g.get())
}
unsafe impl Send for WorkerVmRef {}
unsafe impl Sync for WorkerVmRef {}
unsafe impl Send for GilVmToken {}
/// G1.4 BISECTION gate: `RAVA_FORK_DEBUG_NO_ALLOC=1` makes every worker TLAB allocation abort the
/// region BEFORE mutating segment memory. If the netty/enum corruption VANISHES under it, the
/// defect is in the allocating path (aborted-reservation reclamation / worker writes); if it
/// PERSISTS, the alloc path is innocent and the cause is elsewhere (clinit / shared-metadata).
#[allow(dead_code)]
pub(crate) fn fork_debug_no_alloc() -> bool {
    use std::sync::OnceLock;
    pub(crate) static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FORK_DEBUG_NO_ALLOC").is_some())
}
/// G1.4: trace when the worker-side `<clinit>` barrier fires (RAVA_FORK_TRACE).
#[allow(dead_code)]
pub(crate) fn fork_trace() -> bool {
    std::env::var_os("RAVA_FORK_TRACE").is_some()
}
#[allow(dead_code)]
pub(crate) fn set_gilfree(on: bool) {
    GILFREE.with(|g| g.set(on));
}

/// Does THIS thread allocate out of a PRE-RESERVED, disjoint segment rather
/// than the shared heap tables?
///
/// The answer is "while gil-free", NOT "on an executor thread" — a distinction
/// that was a live bug. `gil::set_worker()` is a THREAD-LIFETIME latch set once
/// when an executor thread starts, so it is equally true of a plain GIL-BOUND
/// guest task, which owns no reservation: its allocations took the segment
/// branch, found the range empty `(0, 0)`, and returned handle `0` — the
/// `[EXECUTOR] uncaught guest task error: Throw(Ref(0))` signature. The
/// reservations are installed by `arena_setup` / `worker_tlab_view` and are
/// always paired with `set_gilfree(true)`, so this is exactly the right test
/// and strictly narrower than the old one.
#[inline]
pub(crate) fn owns_reserved_segment() -> bool {
    gilfree()
}

thread_local! {
    /// This gil-free worker's private allocation arena: `(next, end)` object-slot
    /// handles inside a range pre-reserved (under the GIL) by the offload driver.
    /// The worker fills `[base, end)` via raw disjoint writes — no shared `objects`
    /// Vec push (which would realloc + race). Empty `(0,0)` off a worker.
    pub(crate) static ARENA: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}
/// Install a freshly-reserved slot range `[base, base+count)` as this thread's arena.
#[allow(dead_code)]
pub(crate) fn arena_setup(base: u32, count: u32) {
    ARENA.with(|a| a.set((base, base + count)));
}
/// Take the next arena slot, or `None` when the reserved range is exhausted (the
/// caller then re-reserves under the GIL or bails the offload).
#[allow(dead_code)]
pub(crate) fn arena_take() -> Option<u32> {
    ARENA.with(|a| {
        let (next, end) = a.get();
        if next < end {
            a.set((next + 1, end));
            Some(next)
        } else {
            None
        }
    })
}
#[allow(dead_code)]
pub(crate) fn arena_clear() {
    ARENA.with(|a| a.set((0, 0)));
}

thread_local! {
    /// GILFREE-RINGS: this worker's private `Heap::field_box` slice — `(next,
    /// end)` INDICES into a range pre-reserved under the GIL. The boxed-field
    /// twin of `ARENA` above: a worker writes `field_box[next]` in place instead
    /// of pushing to the shared Vec (realloc + torn index). Empty `(0, 0)`
    /// everywhere else, which reads as exhausted → the lane aborts rather than
    /// silently falling back to a racy push.
    pub(crate) static FIELD_BOX: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}
#[cfg(feature = "next-gen-gc")]
pub(crate) fn field_box_setup(base: u32, count: u32) {
    FIELD_BOX.with(|f| f.set((base, base + count)));
}
#[cfg(feature = "next-gen-gc")]
pub(crate) fn field_box_take() -> Option<u32> {
    FIELD_BOX.with(|f| {
        let (next, end) = f.get();
        if next < end {
            f.set((next + 1, end));
            Some(next)
        } else {
            None
        }
    })
}
/// COW isolation for the parallel column-fork. 17.3: DEFAULT-ON (RAVA_NO_FORK_COW disables).
/// Read once at first field-access compile (OnceLock) — so boot-compiled methods carry the
/// shadow branch, isolating a phase-B worker's density-graph memo writes (Cache2D/FlatCache)
/// into its thread-local shadow. Cost off-fork is one not-taken branch per compiled field
/// access (the inline arm still runs on the main thread) — verified negligible (SETUP_MS
/// within noise). The cow-branch's not-armed arm is byte-identical to the pre-COW inline store.
pub(crate) fn cow_gate() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FORK_COW").is_none())
}
/// Redirect writes to the shadow right now? (COW gated AND this is a gil-free worker.)
///
/// ⭐**THE ORDER OF THESE TWO TERMS IS THE WHOLE COST, AND THE FIX WAS ALREADY WRITTEN ONE
/// LAYER UP.** `cow_gate()` is a `OnceLock<bool>` — an ACQUIRE-ordered atomic load from a
/// static, which LLVM may neither hoist out of a loop nor merge with a second read across an
/// opaque call — and it is DEFAULT-ON, so it can never decide this conjunction on its own.
/// `gilfree()` is a bare `#[thread_local]` byte load (see its doc comment above) and is `false`
/// for the entire serial boot, so it decides the conjunction every time. Asking the OnceLock
/// first meant every caller paid the atomic to reach an answer the plain load already had.
///
/// WAS: acquire-ordered `OnceLock` load, then a thread-local byte load.
/// IS: the thread-local byte load, which short-circuits and the `OnceLock` is never touched.
///
/// Who this reaches, none of whose files this round edits — the commute at the DEFINITION fixes
/// all three at once: `jit::callbacks::fields` arena `getfield` AND `putfield` (the compiled →
/// host field seam) and `flatbody::arena_field` (the flat DAG runner's field read, which the
/// boot's own profile attributes ~2.4% of wall to).
///
/// ⚠**NOT SWITCHED, DELIBERATELY, AND THE TREE ALREADY DECIDED THIS.** `opcodes::fields::
/// cow_redirecting` is the identical commute applied locally, with the identical argument written
/// out: `A && B` commutes exactly when both terms are pure and one is a process constant
/// (`gilfree() && CONST == CONST && gilfree()`), so there is no behaviour for a kill switch to
/// restore — only an instruction ORDER, and a runtime switch guarding it would have to read a
/// process constant to decide, which is exactly the load being removed.
#[inline]
pub(crate) fn cow_active() -> bool {
    gilfree() && cow_gate()
}
/// Object-identity key for a receiver, or None if it isn't a shadowable object.
pub(crate) fn cow_obj_key(v: &crate::Value) -> Option<u64> {
    match v {
        crate::Value::Ref(h) => Some(*h as u64),
        #[cfg(feature = "next-gen-gc")]
        crate::Value::Obj(p) => Some(p.addr()),
        _ => None,
    }
}
/// RAVA_COW_OWNED_BYPASS (default OFF): a WORKER-OWNED receiver (allocated in this worker's
/// own TLAB view — the ctx + per-element transients) skips the shadow entirely: it is
/// private to this worker and dies with the element, so a direct write can't race or leak.
/// Shadow stays for SHARED-graph nodes (the memo writes COW exists for). Measured: the
/// shadow HashMap round-trip per field access was the 12× region tax (60s vs 5s cow-off).
pub(crate) fn cow_owned_bypass() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COW_OWNED_BYPASS").is_some())
}
thread_local! {
    /// THIS worker's owned address ranges: its initial view carve + every spill chunk it
    /// claimed. `tlab_in_nursery` is the WRONG check for ownership — it matches the global
    /// young window, so main-young warmup graph nodes classified as "owned" and the bypass
    /// degenerated to cow-off (measured: identical divergent hash 3d7814…).
    pub(crate) static OWNED_RANGES: std::cell::RefCell<Vec<(usize, usize)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
pub(crate) fn owned_note(base: usize, len: usize) {
    OWNED_RANGES.with(|r| r.borrow_mut().push((base, len)));
}
/// RESERVED-HANDLE OWNERSHIP (`RAVA_COW_OWNED_HANDLES`, default OFF).
///
/// `cow_owned` matches `Value::Obj` only, so EVERY handle-backed object counts as shared —
/// including one this worker allocated microseconds earlier. The ring's putfield barrier
/// (`jit.rs`, `ABORT_SHARED_FIELD`) turns that into `request_abort()`, which is GLOBAL: six
/// such writes discarded a 127-task batch (`completed=55/127 keep=false`) and forced the
/// whole ring to be redone serially — measured at ~19 s on the migration arm and, by the
/// same mechanism, ~23 s on the `RAVA_JIT_THRESHOLD=128` arm.
///
/// The raiser's own comment pre-registered both the work and its acceptance test:
/// "Over-aborts on worker-local Ref handles for now (`cow_owned` covers arena Obj only) —
/// safe; completeness (reserved-handle ownership) is a later stage, MEASURED BY ABORT RATE."
///
/// Sound by construction rather than by relaxation: a handle this worker allocated INSIDE
/// its region is private to it until publish, exactly like an arena object in its own
/// range. Only allocations made while `gilfree()` are recorded, and the set dies with the
/// region (`worker_tlab_clear`), so a recycled slot cannot outlive the ownership claim.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn cow_owned_handles() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COW_OWNED_HANDLES").is_some())
}
thread_local! {
    /// Handles allocated by THIS worker during THIS region. Cleared with the view.
    pub(crate) static OWNED_HANDLES: std::cell::RefCell<crate::FastMap<u32, ()>> =
        std::cell::RefCell::new(crate::FastMap::default());
}
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn owned_handle_note(h: u32) {
    OWNED_HANDLES.with(|s| {
        s.borrow_mut().insert(h, ());
    });
}
pub(crate) fn owned_handle_clear() {
    OWNED_HANDLES.with(|s| s.borrow_mut().clear());
}

/// RAVA_COW_OWNED_UNDO (default OFF): owned receivers write DIRECT (reads become free —
/// they dominate compute) but the OLD value is logged and rolled back per element — the
/// per-element-discard semantics of the shadow at direct-write speed. Distinguishes the
/// bypass-divergence mystery: if this yields 8790, the discard on owned objects is the
/// load-bearing semantic; if 3d7814, the delta is elsewhere entirely.
pub(crate) fn cow_owned_undo() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COW_OWNED_UNDO").is_some())
}
thread_local! {
    pub(crate) static UNDO_LOG: std::cell::RefCell<Vec<(u64, u32, crate::Value)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(feature = "next-gen-gc")]
pub(crate) fn undo_note(addr: u64, id: u32, old: crate::Value) {
    let n = CNT_UNDO.fetch_add(1, Ordering::Relaxed);
    if n < 12 && std::env::var_os("RAVA_FORK_TRACE").is_some() {
        let cls = crate::arena::ArenaPtr::new(addr as *mut u8)
            .and_then(|p| crate::nextgen::class_name(crate::nextgen::arena_class_id(p)))
            .unwrap_or_default();
        eprintln!("[UNDO#{n}] recv={addr:#x} class={cls} id={id} old={old:?}");
    }
    UNDO_LOG.with(|l| l.borrow_mut().push((addr, id, old)));
}
/// COW-arm diagnostics (always-on relaxed adds — the arms are already callback-slow).
pub static CNT_SHADOW_PUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static CNT_SHADOW_GET_HIT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CNT_SHADOW_GET_MISS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CNT_SHADOW_CLEAR: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CNT_SHADOW_CLEAR_ENTRIES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CNT_SHADOW_MAX_ENTRIES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CNT_UNDO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static CNT_OWNED_TRUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static CNT_OWNED_FALSE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Shadow puts that landed on a NOT-owned (shared) receiver — under a correct bypass these
/// are the ONLY shadow writes; if 0 while bypass diverges, owned-classification is the bug.
pub static CNT_SHARED_PUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Worker aastore stores skipped as shared-memo writes (RAVA_COW_ARRAY_SKIP).
pub static CNT_ARRAY_SKIP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Identity-hash stamps taken inside a gilfree region (RAVA_IDHASH_PROBE).
pub static CNT_IDHASH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// putstatic writes on a gilfree worker (un-isolated shared write — S2-race probe).
pub static CNT_PUTSTATIC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const COW_LOCAL_SHADOW_PUT: usize = 0;
const COW_LOCAL_SHARED_PUT: usize = 1;
const COW_LOCAL_GET_HIT: usize = 2;
thread_local! {
    /// Hot COW telemetry is execution-thread state, not shared runtime state.
    /// A region worker merges these three totals once at `leave_region`.
    pub(crate) static COW_HOT_SHADOW_PUT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static COW_HOT_SHARED_PUT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static COW_HOT_GET_HIT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static HOT_ARENA_ALLOCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[inline]
pub(crate) fn fork_trace_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FORK_TRACE").is_some())
}

#[inline]
/// Is anything going to READ the arena-allocation count? Env-static, so one predictable load.
///
/// `ARENA_ALLOCS` has exactly three readers and all three are diagnostics: the `[ARENA-PROBE]`
/// line inside `dump_jit_stats` (armed by `RAVA_JIT_STATS`) and two fatal "tenured space
/// exhausted" messages that print it on the way out. Nothing branches on it. Counting it cost a
/// GLOBAL atomic read-modify-write on every object birth outside a fork region — a shared cache
/// line bouncing between every thread that allocates — to make three strings more informative.
///
/// This is the `LOCK_COUNT` shape the ledger records as its one clean deletion of the kind
/// (written in four places, read in none; removing it measured wall -2.3% / Done -5.2%). Here the
/// count IS read, so it is gated rather than deleted, and the fatal messages say when it was not
/// being collected instead of printing a confident zero.
fn arena_allocs_armed() -> bool {
    // The safepoint tests assert on these counters directly (`parallel_safepoint_tests`), so a
    // test build always counts — the gate exists to keep a shared cache line off the production
    // allocation path, not to make the counter untestable.
    if cfg!(test) {
        return true;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_JIT_STATS").is_some() || std::env::var_os("RAVA_FORK_STATS").is_some()
    })
}

/// ⭐**THE GATE INLINES; THE COUNTING DOES NOT.**
///
/// This sits on the two arena BIRTH paths — `nextgen::tlab::tlab_try_alloc` (every arena object)
/// and `nextgen::strings` (every arena string) — and on a production boot its entire body is
/// "read a default-off process constant and return". It was one un-inlined `call` with a full
/// prologue on both of them, so the birth site spilled live registers across a function whose
/// answer is almost always "do nothing".
///
/// WAS: `call` + prologue + `OnceLock` load + a thread-local read + a global atomic RMW, all
/// out of line, on every arena birth.
/// IS: the `OnceLock` load and one predicted, not-taken branch INLINE at the birth site; both
/// counters and the thread-local read live in a `#[cold]`, `#[inline(never)]` tail that a
/// production boot never calls. Bit-exact — the same gate, the same two counters, the same
/// region split — and the `cfg!(test)` arm of the gate keeps `parallel_safepoint_tests`
/// counting exactly as before.
#[inline]
pub(crate) fn count_arena_alloc() {
    if !arena_allocs_armed() {
        return;
    }
    count_arena_alloc_armed();
}

#[cold]
#[inline(never)]
fn count_arena_alloc_armed() {
    let in_region = REGION_DEPTH.with(|depth| depth.get() != 0);
    if !in_region {
        crate::nextgen::ARENA_ALLOCS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    HOT_ARENA_ALLOCS.with(|value| value.set(value.get().wrapping_add(1)));
}

/// For the two crash messages: is the number they are about to print real?
pub(crate) fn arena_allocs_counted() -> bool {
    arena_allocs_armed()
}

#[cfg(test)]
pub(crate) fn hot_cow_pending_for_test() -> [u64; 3] {
    [
        COW_HOT_SHADOW_PUT.with(|value| value.get()),
        COW_HOT_SHARED_PUT.with(|value| value.get()),
        COW_HOT_GET_HIT.with(|value| value.get()),
    ]
}

#[cfg(test)]
pub(crate) fn hot_arena_allocs_pending_for_test() -> u64 {
    HOT_ARENA_ALLOCS.with(|value| value.get())
}

#[cfg(test)]
pub(crate) fn hot_cow_buffering_for_test() -> bool {
    !fork_trace_enabled()
}

/// Diagnostic-only COW timing/cardinality counters. The gate is immutable and
/// workload-agnostic; it never participates in admission or execution policy.
pub(crate) fn cow_profile_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COW_PROFILE").is_some())
}

pub(crate) fn shadow_get_field(key: u64, id: u32) -> Option<crate::Value> {
    let r = SHADOW_FIELDS.with(|s| s.borrow().get_cloned(key, id));
    if r.is_some() {
        count_hot_cow(&CNT_SHADOW_GET_HIT, COW_LOCAL_GET_HIT);
    } else if cow_profile_enabled() {
        CNT_SHADOW_GET_MISS.fetch_add(1, Ordering::Relaxed);
    }
    r
}
#[cfg(test)]
pub(crate) fn arena_remaining() -> u32 {
    ARENA.with(|a| {
        let (n, e) = a.get();
        e.saturating_sub(n)
    })
}

#[cfg(test)]
pub(crate) fn parked_count() -> usize {
    PARKED.load(Ordering::SeqCst)
}

#[cfg(test)]
pub(crate) fn request_stop_for_test() {
    STOP.store(true, Ordering::Release);
}

/// Serializes tests that drive the GLOBAL safepoint statics (IN_REGION/STOP/
/// PARKED). cargo runs tests in parallel by default; two concurrent users of
/// these process-wide atomics would interleave (one test's parked workers
/// satisfy another's `stop_the_world` count). Every safepoint test locks this.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
mod worker;
pub(crate) use worker::*;
mod context;
pub(crate) use context::*;
mod stats;
pub(crate) use stats::*;
