//! A worker's view of the Vm and the abort protocol around it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::parallel::*;

/// Signal that the current GIL-free region must abort (e.g. TLAB overflow).
#[allow(dead_code)]
#[track_caller]
pub(crate) fn request_abort() {
    ABORT_RAISED.fetch_add(1, Ordering::Relaxed);
    // GILFREE-RINGS abort SITE (RAVA_FORK_TRACE): the backtrace below names the
    // enclosing symbol, which inlining/ICF can attribute to a NEIGHBOURING
    // callback — not good enough to close a "why did every task abort" question.
    // `#[track_caller]` carries the raiser's exact file:line with no symbolization.
    // RAVA_LANE_ABORT_SITE is the CHEAP gate for just this print. RAVA_FORK_TRACE
    // also arms per-call locked tallies (a mutexed HashMap on every locked
    // dispatch) heavy enough to change what the run does — a boot under it died
    // before reaching the ring phase at all — so naming an abort site must not
    // require paying for them.
    #[cfg(feature = "next-gen-gc")]
    if crate::gilfree_cf::in_lane() && lane_abort_site_enabled() {
        static SITES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SITES.fetch_add(1, Ordering::Relaxed);
        if n < 6 || n % 64 == 0 {
            eprintln!("[LANE-ABORT-SITE#{n}] {}", std::panic::Location::caller());
        }
        // The print above is BUDGETED and therefore samples whichever phase raised first;
        // the tally is not, so the end-of-lane report names every raiser by total.
        crate::gilfree_cf::note_abort_site(std::panic::Location::caller());
    }
    // DIAGNOSTIC (RAVA_FORK_TRACE): name the silent abort sites. Many request_abort()
    // callers don't print (getstatic-miss budget exhausted, newarray-region-full, jit_call
    // worker-guard) yet still tear the region down to serial — this captures which ones.
    // SPREAD, not front-loaded: a boot runs several distinct lane phases, and a
    // prefix-only budget is spent entirely on the earliest one — 12 backtraces meant
    // to explain the ring phase all landed in the datapack phase before it. First
    // few, then periodic, so every later phase is sampled too.
    if std::env::var_os("RAVA_FORK_TRACE").is_some() {
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SHOWN.fetch_add(1, Ordering::Relaxed);
        if n < 4 || n % 32 == 0 {
            eprintln!("[FORK-ABORT-CAUSE#{n}] request_abort backtrace:\n{}", std::backtrace::Backtrace::force_capture());
        }
    }
    // GILFREE-RINGS: the rings lane runs N INDEPENDENT tasks in one region, so
    // the process-wide flag alone would read one task's deopt as every sibling's.
    // Latch it against THIS task too (inert off the lane).
    crate::gilfree_cf::note_lane_abort();
    REGION_ABORT.store(true, Ordering::Release);
}
/// Contract guard for a FROZEN `Vm` field (see docs/gil-free-vm-layout.md): such a field is
/// read-only inside a GIL-free region, so a WRITE while `gilfree()` is a bug. Applied at the
/// mutation side of the frozen fields as the contract's executable spec. `debug_assert` only:
/// zero cost in release, and it NEVER fires today (the single-threaded boot is never gilfree),
/// so wiring it in is bit-exact. Its value is guarding every worker path against a
/// stray frozen-field write — the executable spec behind the GAP-S09 frozen-field
/// contract (docs/gil-free-vm-layout.md).
#[inline]
#[allow(dead_code)]
pub(crate) fn assert_not_gilfree(_field: &'static str) {
    // Same exemption as `Vm::debug_assert_not_gilfree`: the locked-exec envelope grants the
    // exclusivity `&mut` denotes (main parked, siblings blocked on the same re-entrant lock),
    // and `jit::vm_ref` already hands out `&mut Vm` on exactly that condition. Outside it,
    // unchanged.
    #[cfg(feature = "next-gen-gc")]
    let _inside_envelope =
        crate::worker_locked_exec_enabled() && crate::jit::resolve_lock_held_by_me();
    #[cfg(not(feature = "next-gen-gc"))]
    let _inside_envelope = false;
    debug_assert!(
        !gilfree() || _inside_envelope,
        "FROZEN Vm field `{_field}` written inside a GIL-free region OUTSIDE the locked \
         envelope — see docs/gil-free-vm-layout.md"
    );
}

/// STAGE 1 of docs/worker-context-plan.md: typed replacement for the raw `vmptr: usize`
/// the fork worker payloads carried (apply-leaf `Sh`, S2 chunk fill, counted loop, and
/// the since-retired column fork). MECHANICAL newtype — zero behavior change — that makes every worker-side
/// use of the coordinator's Vm grep-enumerable: `install_current()` (the CURRENT_VM
/// publication every worker performs) plus ONE loud unsafe escape
/// (`vm_mut_unsound_interp`) at the interpreter-arm forks. The pointee is the
/// COORDINATOR's `Vm`, parked in the `std::thread::scope` join for the whole region
/// lifetime, so the pointer never dangles while a worker holds this ref.
#[derive(Clone, Copy)]
pub(crate) struct WorkerVmRef(*const crate::Vm);
// SAFETY: same contract the raw `vmptr: usize` payload fields carried (and the
// apply-leaf `unsafe impl Sync for Sh` documents): the coordinator's Vm outlives the
// region (it is parked in the scope join), workers only READ shared Vm state through it
// or funnel mutation through the loud interp escape below, and all per-thread MUTABLE
// state lives in `frames::` TLS + arena/nursery views reached WITHOUT this ref.
impl WorkerVmRef {
    /// Capture the coordinator's Vm for a fork region. Takes `&mut Vm` to document that
    /// the caller IS the coordinator handing its exclusive borrow to the region: it
    /// parks in the `std::thread::scope` join until every worker is done, exactly as
    /// the raw `self as *mut Vm as usize` capture did.
    pub(crate) fn new(vm: &mut crate::Vm) -> Self {
        WorkerVmRef(vm as *mut crate::Vm as *const crate::Vm)
    }
    /// Publish this Vm as the worker thread's CURRENT_VM (the pointer the compiled-code
    /// callbacks resolve through). Returns the previous value, mirroring
    /// `jit::set_current_vm` (all current workers discard it and clear with a null
    /// `set_current_vm` on exit, unchanged).
    #[inline]
    pub(crate) fn install_current(&self) -> *mut crate::Vm {
        crate::jit::set_current_vm(self.0 as *mut crate::Vm)
    }
    /// GAP-S09 stage 2 (LIVE HOLE 2): install THIS worker's private `JitCtx`
    /// guard through a SHARED borrow — `Heap::install_worker_jit_ctx` is
    /// `&self` (it copies the already-published `jit_ctx` snapshot and swaps
    /// a TLS pointer), so no `&mut Vm` escape is needed to privatize the
    /// worker's fault latch + COW bloom. The caller must invoke this only
    /// AFTER the coordinator completed every region reserve (the snapshot
    /// freezes the descriptor bases) and must drop the guard before the
    /// region tears down — RAII restores the shared ctx on unwind.
    pub(crate) fn install_worker_jit_ctx(&self) -> crate::heap::WorkerJitCtxGuard {
        unsafe { (*self.0).heap.install_worker_jit_ctx() }
    }
    /// GILFREE-RINGS: publish the coordinator Vm as this worker's CURRENT_VM
    /// without stamping the shared `jit_ctx_slot` (see
    /// `jit::set_current_vm_worker`). Returns the previous value for
    /// `restore_current_worker`.
    pub(crate) fn install_current_worker(&self) -> *mut crate::Vm {
        crate::jit::set_current_vm_worker(self.0 as *mut crate::Vm)
    }
    pub(crate) fn restore_current_worker(&self, prev: *mut crate::Vm) {
        crate::jit::set_current_vm_worker(prev);
    }
    /// The ACTIVE `JitCtx` address for compiled calls on THIS thread — the
    /// worker guard's private block once `install_worker_jit_ctx` ran, the
    /// shared block otherwise. Shared borrow (`Heap::array_desc_cell_ptr` is
    /// `&self`); a kind-A worker passes this — not the coordinator-computed
    /// shared address — to `call_raw` so its compiled code reads/latches the
    /// PRIVATE fault word.
    pub(crate) fn active_jit_ctx_ptr(&self) -> i64 {
        unsafe { (*self.0).heap.array_desc_cell_ptr() }
    }
    /// GAP-S09 stage 4: the direct compiled→compiled call through a SHARED
    /// borrow — routes to `Vm::jit_direct_call_worker` (`&self`), the
    /// gilfree read-only arm (cached hit → invoke; any miss → None, no
    /// resolve, no insert). This is how the S2 compiled-body worker makes
    /// its per-element compute call WITHOUT the `vm_mut_unsound_interp`
    /// `&mut` escape. SAFETY of the deref: same region contract as
    /// `install_worker_jit_ctx` (coordinator parked in the scope join).
    pub(crate) fn jit_direct_call_worker(
        &self,
        caller_ptr: *const crate::class_loader::LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<Result<i64, crate::VmError>> {
        // 0 = no compiled site is baked for this surface, so there is nothing to teach.
        unsafe { (*self.0).jit_direct_call_worker(caller_ptr, cp, kind, raw, 0) }
    }
    /// The ONE loud escape: a worker-side `&mut Vm` for the QUARANTINED
    /// interpreter-arm forks (GAP-S09 stage 5, docs/runtime-gap-register.md).
    /// This aliased `&mut` is OUTSIDE the closed GAP-S09 soundness claim — it is
    /// exactly the old `&mut *(vmptr as *mut Vm)` made grep-enumerable. Exactly
    /// TWO fork surfaces may construct it, each behind its own default-off env
    /// gate with a QUARANTINE doc block at the arm: the S2 chunk-fill interp arm
    /// (`RAVA_M3_CHUNK_FILL_INTERP`; the compiled S2 arm keeps it only for the
    /// sanctioned locked-exec `jit_call` fallback + fault take) and the counted
    /// loop (`RAVA_FORK_NUMERIC`). The column fork was retired outright.
    /// Residual typed-interpreter work is filed under GAP-A02.
    ///
    /// SAFETY: caller must be a fork worker inside a region whose coordinator is
    /// parked in the scope join, and must uphold that fork's documented discipline
    /// (frozen-field reads + disjoint index-partitioned writes only).
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn vm_mut_unsound_interp(&self) -> &mut crate::Vm {
        &mut *(self.0 as *mut crate::Vm)
    }
}

/// Is this receiver worker-owned (allocated in one of THIS worker's own view/spill ranges)?
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn cow_owned(v: &crate::Value) -> bool {
    if let crate::Value::Ref(h) = v {
        // Flag-gated so the default path is byte-identical: with it off this arm is a
        // single already-resolved bool test and the answer stays `false`, as before.
        return cow_owned_handles()
            && crate::parallel::OWNED_HANDLES.with(|s| s.borrow().contains_key(h));
    }
    let crate::Value::Obj(p) = v else { return false };
    let a = p.addr() as usize;
    let owned = crate::parallel::OWNED_RANGES.with(|r| r.borrow().iter().any(|&(b, l)| a >= b && a < b + l));
    if owned {
        let n = CNT_OWNED_TRUE.fetch_add(1, Ordering::Relaxed);
        // RAVA_OWNED_PROBE: name the FIRST few owned-classified receivers (class + addr) — the
        // owned/shared paradox hunt's step (1). A transient (SinglePointContext) ⇒ divergence is
        // the per-element DISCARD semantic (owned direct-write persists across elements where the
        // shadow discards); a shared graph node (Cache2D/FlatCache/DensityFunction) ⇒ genuine
        // range MISCLASSIFICATION (shared addr leaked into an OWNED_RANGE).
        if n < 24 && std::env::var_os("RAVA_OWNED_PROBE").is_some() {
            let cls = crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).unwrap_or_default();
            eprintln!("[OWNED#{n}] addr={a:#x} class={cls}");
        }
    } else {
        CNT_OWNED_FALSE.fetch_add(1, Ordering::Relaxed);
    }
    owned
}
