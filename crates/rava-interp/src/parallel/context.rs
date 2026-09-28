//! A worker's context, its token, and the rollback discipline around a claim.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::parallel::*;

/// Claim `chunk` bytes from the spill pool (lock-free bump). None = pool exhausted/absent.
/// The spill region is process-lifetime and REUSED across regions/workers (the cursor resets
/// at `spill_clear`), so a freshly-claimed chunk carries STALE bytes from a prior worker's
/// use. Object allocs zero their own cells, but a primitive array's payload / a read past an
/// object's declared fields would see that stale data → run-to-run non-determinism at N
/// workers. RAVA_SPILL_ZERO scrubs the chunk on claim (a fix candidate for the element-0
/// parallel-FILL divergence). Cheap: a bump chunk is claimed rarely (once per view refill).
pub(crate) fn spill_claim(chunk: usize) -> Option<usize> {
    if SPILL_BASE.load(Ordering::Acquire) == 0 {
        return None;
    }
    let cur = SPILL_CUR.fetch_add(chunk, Ordering::AcqRel);
    if cur + chunk <= SPILL_END.load(Ordering::Acquire) {
        #[cfg(feature = "next-gen-gc")]
        if spill_zero_enabled() {
            unsafe { std::ptr::write_bytes(cur as *mut u8, 0, chunk) };
        }
        SPILL_CLAIMS.fetch_add(1, Ordering::Relaxed);
        Some(cur)
    } else {
        None
    }
}

thread_local! {
    /// STARVED LATCH. `rava_arena_safepoint` must be able to tell the compiled
    /// inline-alloc retry loop "I could not free you any space" — otherwise that
    /// loop (which polls nothing) re-enters immediately and spins forever against
    /// a full view. Set by the gil-free arm of `arena_collect_jit` when the spill
    /// pool is absent or exhausted; consumed by the callback that reports it.
    pub(crate) static SPILL_STARVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// Signal a region abort caused specifically by a worker nursery-view overflow (G1.2 backoff).
#[allow(dead_code)]
#[track_caller]
pub(crate) fn request_abort_overflow() {
    // Same cheap site print the general raiser carries — an overflow abort that
    // does NOT print under RAVA_LANE_ABORT_SITE would otherwise be indistinguishable
    // from one that came through `request_abort`.
    #[cfg(feature = "next-gen-gc")]
    if crate::gilfree_cf::in_lane() && lane_abort_site_enabled() {
        static SITES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SITES.fetch_add(1, Ordering::Relaxed);
        if n < 6 || n % 4096 == 0 {
            eprintln!("[LANE-OVF-SITE#{n}] {}", std::panic::Location::caller());
        }
        crate::gilfree_cf::note_abort_site(std::panic::Location::caller());
    }
    // DIAGNOSTIC (RAVA_FORK_TRACE): name the FIRST overflow-abort site (this raiser had no
    // backtrace print — the NO_COW silent-abort hunt).
    //
    // ⛔**IT ASKED THE OPERATING SYSTEM, PER ABORT, A QUESTION SETTLED BEFORE `main` RAN.**
    // `std::env::var_os` is a process environment lookup plus the `OsString` it allocates to
    // carry the answer back, and this raiser is not a rare path: it fires once per iteration of
    // the compiled inline-alloc retry loop, measured at 176,246,054 callbacks on ONE worker
    // (`counters.rs`, the nursery-view exhaustion note) — i.e. per lane-worker allocation
    // overflow, not per region.
    // WAS: one env lookup + one allocation per overflow abort. IS: one relaxed `OnceLock` load
    // (`fork_trace_enabled`, already the gate every other FORK_TRACE site reads); the lookup and
    // the allocation happen once per PROCESS. Output is identical on both arms.
    if fork_trace_enabled() {
        static FIRST_OVF: AtomicBool = AtomicBool::new(false);
        if FIRST_OVF.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            eprintln!("[FORK-OVF-CAUSE] first request_abort_overflow backtrace:\n{}", std::backtrace::Backtrace::force_capture());
        }
    }
    OVERFLOW_RAISED.fetch_add(1, Ordering::Relaxed);
    ABORT_RAISED.fetch_add(1, Ordering::Relaxed);
    crate::gilfree_cf::note_lane_abort();
    REGION_OVERFLOW.store(true, Ordering::Release);
    REGION_ABORT.store(true, Ordering::Release);
}
/// GAP-S09 stage 4 (docs/worker-context-plan.md, Part-2 shape to the extent the
/// kind-A apply-leaf worker needs it): the typed, RAII worker context. `enter()`
/// performs — in the exact order the kind-A worker body used to do ad hoc — the
/// CURRENT_VM publication, the `gil::set_worker` marking, the `frames::` TLS
/// registration, the gil-free flag + region entry, the optional private TLAB
/// view over the coordinator's young partition, and the private `JitCtx` guard
/// (stage-2 fault isolation). `Drop` tears all of it down in the exact reverse
/// discipline the worker body used (guard first, then TLAB, region/flag, TLS,
/// CURRENT_VM), ON ANY EXIT — including a panic unwinding out of the worker
/// closure — so a worker can never leak the gil-free flag, a stale TLAB view or
/// a swapped JitCtx into a pooled/reused thread. `gil::set_worker` stays latched
/// (it always did: the TLS flag means "not the compiling main thread", a
/// thread-lifetime property, and fork workers are scope-scoped threads).
///
/// The S2 chunk-fill worker keeps its existing hand-rolled choreography (it
/// additionally interleaves `arena_setup`/`run_stop_clear` and — on the
/// quarantined interp arm only — the loud interp escape). Stage 5 closed
/// GAP-S09 by QUARANTINING the interpreter-on-worker arms behind explicit
/// env gates and retiring the column fork; merging S2's choreography onto
/// this context is residual GAP-A02 hygiene, not a soundness hole.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct WorkerContext {
    vm: WorkerVmRef,
    /// CURRENT_VM value to restore on drop (null on a fresh worker thread).
    prev_vm: *mut crate::Vm,
    /// Did enter() arm the gil-free flag + region (kind-A `gilfree_on`)?
    gilfree: bool,
    /// Was a private TLAB view installed (kind-A `needs_partition`)?
    tlab: bool,
    /// The stage-2 private fault latch / COW bloom. `Option` only so Drop can
    /// release it FIRST (before the TLS teardown), mirroring the explicit
    /// `drop(worker_jit_ctx)` order of the old worker bodies.
    call_context: Option<crate::vm::thread_context::WorkerThreadContextGuard>,
    jit_ctx: Option<crate::heap::WorkerJitCtxGuard>,
}
#[cfg(feature = "next-gen-gc")]
impl WorkerContext {
    /// Build the worker context on the CURRENT worker thread. Must run after the
    /// coordinator completed every region reserve (young partition, array region,
    /// COW arming) — the same precondition `install_worker_jit_ctx` documents.
    /// `tlab_view` = `Some((base, len))` installs this worker's private view of
    /// the coordinator's young nursery (kind-A `needs_partition`). Phase 4: refresh
    /// the thread-local HeapView after worker_tlab_view installs base/cap.
    pub(crate) fn enter(vm: WorkerVmRef, gilfree: bool, tlab_view: Option<(usize, usize)>) -> Self {
        let prev_vm = vm.install_current();
        crate::gil::set_worker();
        crate::frames::register();
        if gilfree {
            set_gilfree(true);
            enter_region();
        }
        let tlab = tlab_view.is_some();
        if let Some((base, len)) = tlab_view {
            crate::nextgen::worker_tlab_view(base, len);
            // Phase 4: refresh the thread-local HeapView after worker_tlab_view.
            crate::vm::thread_context::refresh_heap_view_from_tlab();
        }
        let call_context = Some(crate::vm::thread_context::WorkerThreadContextGuard::install());
        let jit_ctx = Some(vm.install_worker_jit_ctx());
        WorkerContext { vm, prev_vm, gilfree, tlab, call_context, jit_ctx }
    }
    /// The ACTIVE (worker-private) `JitCtx` address for this thread's compiled
    /// calls — pass this, not the coordinator-computed shared address, to
    /// `call_raw` so a fault latches the PRIVATE fault word.
    pub(crate) fn active_jit_ctx_ptr(&self) -> i64 {
        self.vm.active_jit_ctx_ptr()
    }
}
#[cfg(feature = "next-gen-gc")]
impl Drop for WorkerContext {
    fn drop(&mut self) {
        self.jit_ctx.take();
        if self.tlab {
            // Phase 4: sync the HeapView window back before clearing the view,
            // so the coordinator sees bumps done through the TLS window.
            crate::vm::thread_context::sync_heap_view_to_tlab();
            crate::nextgen::worker_tlab_clear();
        }
        // 3. region exit + gil-free flag,
        if self.gilfree {
            leave_region();
            set_gilfree(false);
        }
        // 4. frames/execution-state TLS,
        crate::frames::unregister();
        self.call_context.take();
        // 5. CURRENT_VM restore (null on a fresh worker thread — identical to
        //    the old explicit `set_current_vm(null)`).
        crate::jit::set_current_vm(self.prev_vm);
    }
}

/// STAGE 1 of docs/worker-context-plan.md (GAP-A02 hygiene): typed handle for the
/// GIL-HELD `*mut Vm` that OS-thread daemons and executor workers carry
/// (`task_runtime` workers, `spawn_os_thread`). NOT a gil-free worker handle: the
/// holder only dereferences the Vm while it owns the global GIL. Pure rename of the
/// raw `usize` / `SendPtr` carriers — zero behavior or locking change.
#[derive(Clone, Copy)]
pub(crate) struct GilVmToken(*mut crate::Vm);
// SAFETY: the token crosses into a spawned OS thread, but the Vm is only dereferenced
// while that thread holds the global GIL — the sole-mutator invariant the old
// `SendPtr` / `vm_addr: usize` carriers relied on (see `spawn_os_thread`).
impl GilVmToken {
    pub(crate) fn new(vm: *mut crate::Vm) -> Self {
        GilVmToken(vm)
    }
    /// The raw pointer, for `jit::set_current_vm` publication (unchanged call shape).
    #[inline]
    pub(crate) fn as_mut_ptr(&self) -> *mut crate::Vm {
        self.0
    }
    /// GIL-held `&mut Vm`.
    ///
    /// SAFETY: caller must hold the global GIL for the whole borrow — the same
    /// comment every raw `&mut *(vm_addr as *mut Vm)` deref site carried.
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn vm_mut(&self) -> &mut crate::Vm {
        &mut *self.0
    }
    // NOTE: there is deliberately no `worker_ref()` here. A `WorkerVmRef`'s
    // contract is "the coordinator is PARKED in the scope join for the whole
    // region"; a resident executor worker has no parked coordinator, and minting
    // one from this token is precisely how the rings lane came to run off-GIL
    // beside a live main thread. The lane builds its ref from `&mut Vm` at the
    // park instead (`gilfree_cf::drain`).
}
/// Roll back this element's direct owned-writes (reverse order) — called with shadow_clear.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn undo_rollback() {
    pub(crate) static VERIFY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    crate::parallel::UNDO_LOG.with(|l| {
        let mut log = l.borrow_mut();
        while let Some((addr, id, old)) = log.pop() {
            if let Some(p) = crate::arena::ArenaPtr::new(addr as *mut u8) {
                crate::opcodes::arena_putfield(p, id, &old);
                // Rollback VERIFY (first 3, FORK_TRACE): read the field back — a mismatch
                // names the silent-rollback failure the undo-divergence hunt suspects.
                //
                // ⛔**THE ORDER OF THESE TWO TERMS WAS THE WHOLE COST.** `VERIFY` is incremented
                // only INSIDE this arm, so with the trace OFF it stays 0 and `VERIFY < 3` is true
                // FOREVER — the counter never budgets anything and the env lookup (plus its
                // `OsString`) therefore ran for EVERY entry popped off the undo log. That log
                // grows with the number of owned field writes the element made, so the budget
                // that was supposed to cap this at three lookups capped it at none.
                // WAS: O(undo-log entries) environment lookups + allocations per element discard.
                // IS: one relaxed `OnceLock` load per entry, and the same three lines when the
                // trace IS on — `&&` short-circuits identically in both orders.
                if fork_trace_enabled() && VERIFY.load(Ordering::Relaxed) < 3 {
                    VERIFY.fetch_add(1, Ordering::Relaxed);
                    let back = crate::opcodes::arena_getfield(p, id);
                    eprintln!(
                        "[UNDO-VERIFY] recv={addr:#x} id={id} wrote={old:?} readback={back:?} match={}",
                        format!("{old:?}") == format!("{back:?}")
                    );
                }
            }
        }
    });
}

pub(crate) fn shadow_put_field(key: u64, id: u32, v: crate::Value) {
    let n = count_hot_cow(&CNT_SHADOW_PUT, COW_LOCAL_SHADOW_PUT);
    // Classify this shadowed receiver as owned (this worker's arena) or SHARED (graph node).
    //
    // ⚠**THE SCAN STAYS, DELIBERATELY.** It is a linear walk of `OWNED_RANGES`, whose length
    // grows with every spill chunk this worker has claimed, and it runs on every shadowed write —
    // exactly the shape this round is hunting. It is NOT diagnostic-only: its `!owned` arm is the
    // sole producer of `CNT_SHARED_PUT`, and `shared_put=` is printed UNGATED on the S2
    // chunk-fill completion line (`vm/fork_chunk.rs`) where "92,160 shadow puts / ZERO shared
    // puts" is the fork's acceptance witness. Gating the scan would make that witness read 0 when
    // it is not 0 — a silent false pass, which is strictly worse than the walk. The `any()` does
    // short-circuit, and the owning view is pushed first, so the common (owned) receiver stops at
    // range 0. Making it O(1) needs an address-ordered structure, which is a different change in
    // a different file.
    #[cfg(feature = "next-gen-gc")]
    {
        let owned = crate::parallel::OWNED_RANGES.with(|r| {
            r.borrow().iter().any(|&(b, l)| (key as usize) >= b && (key as usize) < b + l)
        });
        if !owned {
            let sn = count_hot_cow(&CNT_SHARED_PUT, COW_LOCAL_SHARED_PUT);
            // Trace gate FIRST: `fork_trace_enabled` is a `OnceLock` bool, `env::var_os` is a
            // process lookup that allocates. Same twelve lines, none of the lookups.
            if fork_trace_enabled() && sn < 12 {
                let cls = crate::arena::ArenaPtr::new(key as *mut u8)
                    .and_then(|p| crate::nextgen::class_name(crate::nextgen::arena_class_id(p)))
                    .unwrap_or_default();
                eprintln!("[SHARED-PUT#{sn}] recv={key:#x} class={cls} id={id} val={v:?}");
            }
        } else if fork_trace_enabled() && n < 12 {
            let cls = crate::arena::ArenaPtr::new(key as *mut u8)
                .and_then(|p| crate::nextgen::class_name(crate::nextgen::arena_class_id(p)))
                .unwrap_or_default();
            eprintln!("[COW-PUT#{n}] recv={key:#x} class={cls} id={id} owned=true val={v:?}");
        }
    }
    crate::parallel::SHADOW_FIELDS.with(|s| {
        s.borrow_mut().insert(key, id, v);
    });
}
