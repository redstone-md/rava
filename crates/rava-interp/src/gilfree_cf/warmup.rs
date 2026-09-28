//! Warm-compile recording and the pilot pass before a lane runs.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

/// PILOT WARM (`RAVA_GILFREE_NOPILOT` opts OUT; on by default with the lane).
///
/// Every cache a lane worker reads is READ-ONLY on that worker and is filled ONLY
/// by main:
///   * the frozen 16-way per-site direct cache (`jit_direct_call`'s insert arm is
///     skipped under `parallel::gilfree()`, and the worker uses the `&self`
///     `jit_direct_call_worker`), and
///   * the baked per-site single-fptr direct-mono caches `emit_direct_mono` bakes
///     into the compiled body, whose fill in `rava_direct_miss` is likewise skipped
///     on a worker.
/// A batch dispatched entirely cold therefore drives every call site through the
/// locked-exec gate on its first touch, serializing precisely the span the lane
/// exists to parallelize. Running the batch's FIRST task serially exhausts that
/// cold metadata — a previously recorded per-task census put task #1 at 5 cold
/// class loads / 57 compiles / 46 direct-cache fills against 0 / 2 / 0 for tasks
/// 2..128 — so the remaining n-1 tasks run against caches main has already warmed.
///
/// The pilot also reports diagnostic callback counts. Those counts must not
/// decide whether other tasks completed: independent tasks can have different
/// costs, and enabling instrumentation must not change retention or replay.
///
/// The pilot is an ORDINARY serial `vm.apply` under the GIL, identical to the
/// abort path's serial retry, and its future is completed before the region opens.
/// It is excluded from the region and from PHASE 4's keep/discard vote.
pub(crate) fn pilot_warm() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GILFREE_NOPILOT").is_none())
}

/// Exactly ACC_STATIC among {static, native, abstract} for a static impl, none
/// of them for an instance impl; ACC_SYNCHRONIZED (0x0020) rejected either way.
pub(crate) fn access_ok(flags: u16, instance: bool) -> bool {
    let want = if instance { 0 } else { 0x0008 };
    flags & (0x0008 | 0x0020 | 0x0100 | 0x0400) == want
}

/// Process-relative millisecond clock for the run trace below.
pub(crate) fn now_ms() -> u128 {
    pub(crate) static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(std::time::Instant::now).elapsed().as_millis()
}

/// Immediate, bounded stderr trace. The summary `report` fires at heap teardown,
/// which a hard-killed server boot never reaches — and "did these tasks actually
/// run on more than one core" is only answerable from per-run THREAD IDENTITY
/// plus overlapping [t0, t1] intervals, not from a completion count.
pub(crate) fn trace_admit(owner: &str, name: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    pub(crate) static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, Relaxed);
    if n < 4 || n % 32 == 0 {
        eprintln!("[GILFREE-RINGS] admit#{n} {owner}::{name} t={}ms", now_ms());
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_run(t0: u128, aborted: bool, threw: bool, latched: bool, poisoned: bool, cbs: u64, bits: i64) {
    use std::sync::atomic::Ordering::Relaxed;
    pub(crate) static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, Relaxed);
    if n < 16 || n % 16 == 0 {
        let (set, taken, class, cb) = lane_pending_report();
        eprintln!(
            "[GILFREE-RINGS] run#{n} thread={:?} t0={t0}ms t1={}ms aborted={aborted} cbs={cbs} bits={bits:#x} (threw={threw} latched={latched} poisoned={poisoned} pending_set={set} pending_taken={taken} first={class} abort_in_cb={cb})",
            std::thread::current().id(),
            now_ms(),
        );
    }
}

pub(crate) fn warm_compile_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_LANE_WARM_COMPILE").is_none())
}

pub(crate) fn warm_recording() -> bool {
    WARM_RECORDING.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn warm_record_begin() {
    WARM_RECORDING.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn warm_record_end() {
    WARM_RECORDING.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Is there deferred work the guest must not observe past? Cheap enough for the
/// top of every CompletableFuture native (a `Vec::is_empty` on a normally empty
/// vector).
#[inline]
pub(crate) fn has_deferred(vm: &Vm) -> bool {
    !vm.lane.pending.is_empty() && !vm.lane.draining
}

/// Does this seam call OBSERVE a future the lane deferred? — THE DRAIN'S SCOPE.
///
/// ⭐**MEASURED, and it is the product boot's single largest number.** `has_deferred` alone
/// drained the WHOLE batch at the first `CompletableFuture` method on ANY future, related or
/// not. On MC's boot the ring's 127 `generateRingPositions$5` bodies are admitted at t=10180ms
/// and executed 60 ms later by a touch on a future that is not one of them — so the lane
/// FORCES work the guest never asked for:
///
/// | | wall | CPU | cores | `Done` |
/// |---|---|---|---|---|
/// | lane on, blanket drain | 112.7 s | 290.5 s | 2.58 | **64.303** |
/// | lane off entirely | 53.4 s | 55.9 s | 1.05 | **0.494** |
/// | `java` (HotSpot) | 7.2 s | 23.5 s | 3.25 | 0.202 |
///
/// The ring's own window is 10.2→73.2 s = **62.9 s**, and 112.7 − 62.9 = 49.8 s ≈ the whole
/// lane-off boot. So the lane is not slow — **it is eager**, and HotSpot's 0.202 s `Done` shows
/// the JVM does not run that work in this window either.
///
/// The discriminator four earlier attempts looked for is therefore not a property of the TASK at
/// all (rows 432/433/451/452 refuted batch shape, arrival shape and caller bytecode — correctly,
/// because at the deferral point the two workloads ARE identical). It is a property of the
/// OBSERVATION: `RingLane` joins the futures it deferred, so it still drains and keeps its win;
/// MC's boot touches a different future, so the ring stays pending. Identity of the observed
/// object is permitted evidence — it is neither a name, nor an input, nor a timing.
///
/// `RAVA_LANE_DRAIN_ALL=1` restores the blanket drain, so the two arms differ only in the
/// executed path of one binary (row 269).
pub(crate) fn observes_deferred(vm: &Vm, name: &str, args: &[Value]) -> bool {
    if !has_deferred(vm) {
        return false;
    }
    static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ALL.get_or_init(|| std::env::var_os("RAVA_LANE_DRAIN_ALL").is_some()) {
        return true;
    }
    // A CHAINING call does not need its source complete — `cf_chain` registers the
    // continuation and `allOf` counts pending inputs down; `allOf`'s own comment already
    // says "Inputs may still be pending (deferred chunk-gen futures)". Serving these while
    // the batch sleeps is the whole point: MEASURED, the ring batch is forced by exactly
    // `thenCompose` x3 and `allOf` x2 and by nothing else in the entire boot.
    //
    // Everything NOT listed drains — `join`, `get`, `isDone`, `complete`, `cancel`. That
    // direction matters: an unknown method must fall on the safe side, and `isDone` in
    // particular is what `Util.blockUntilDone` spins on, so answering it "false" forever
    // would hang instead of merely being slow.
    // ⛔**AND THE LIST IS TWO NAMES, NOT TWENTY.** The first version exempted the whole chaining
    // family on the argument that `cf_chain` registers a continuation. It does — and the boot
    // still died: `Failed to load datapacks`, with `UnsatisfiedLinkError: invokevirtual
    // Codec.decode (from Codec$RecursiveCodec)`, the same wrong-typed-value signature
    // `monitor.rs` records for a racing memoize. Registering the continuation is not the whole
    // contract: MC's reload builds its graph INSIDE the `thenCompose` function, and running that
    // later is not equivalent to running it then.
    //
    // ⭐**THE LINE FALLS BETWEEN `thenCompose` AND EVERYTHING ELSE, AND IT WAS MEASURED, NOT
    // REASONED.** Traced with `RAVA_LANE_DRAIN_TRACE`, the boot has exactly five drains: the
    // reload's three go through `thenCompose`, and the ring's 128-task batch goes through
    // `allOf`. Exempting `thenCompose` too is what killed the datapack load; exempting only
    // `allOf` moved the force one link down the chain to the `thenApply` MC calls on the future
    // `allOf` returns, and bought nothing. So the family is exempt and `thenCompose` is not.
    //
    // The difference is what the guest does with the value, not what our handler does with it:
    // `thenApply` maps a result, and deferring a map is invisible; MC's reload BUILDS ITS STAGE
    // GRAPH inside the `thenCompose` function, and running that later is not equivalent to
    // running it then. Anything not listed drains — the safe direction, because being merely
    // slow is recoverable and publishing a half-built graph is not.
    if matches!(
        name,
        "allOf" | "anyOf"
            | "thenApply" | "thenApplyAsync" | "thenAccept" | "thenAcceptAsync"
            | "thenRun" | "thenRunAsync" | "thenCombine" | "thenCombineAsync"
            | "thenAcceptBoth" | "thenAcceptBothAsync" | "whenComplete" | "whenCompleteAsync"
            | "handle" | "handleAsync" | "exceptionally"
            | "applyToEither" | "acceptEither" | "runAfterBoth" | "runAfterEither"
            | "toCompletableFuture" | "minimalCompletionStage" | "copy"
    ) {
        return false;
    }
    args.iter().any(|a| observes_one(vm, a))
}

/// A varargs array is scanned one level deep, so a `get`/`join` reached through one still
/// counts. Anything else is compared by reference identity.
fn observes_one(vm: &Vm, v: &Value) -> bool {
    match v {
        Value::Array(h) => match vm.heap.array_values(*h) {
            Some(vals) => vals.iter().any(|e| observes_one(vm, e)),
            None => false,
        },
        Value::Ref(h) => crate::gilfree_cf::lane_linked(vm, *h),
        _ => false,
    }
}

/// THE PARK. Runs every deferred task and completes its future, with main held
/// here — inside a native, holding the GIL, executing no guest bytecode — for the
/// whole region. On return the batch is empty and every one of its futures is
/// complete, which is exactly the state the unchanged inline seam would have left.
pub(crate) fn drain(vm: &mut Vm) {
    if !has_deferred(vm) {
        return;
    }
    // BATCH-SHAPE CENSUS (always on, three relaxed adds per DRAIN — not per task).
    // ⭐Row 432 lost two discriminators to guesses about this distribution: "a batch of one has
    // nothing to fan out" moved the boot not at all, and "the first body of a batch has no peers"
    // collapsed the ring. Both assumed a batch shape nobody had measured. This measures it.
    {
        use std::sync::atomic::Ordering::Relaxed;
        let n = vm.lane.pending.len() as u64;
        DRAINS.fetch_add(1, Relaxed);
        DRAINED_TASKS.fetch_add(n, Relaxed);
        MAX_BATCH_SEEN.fetch_max(n, Relaxed);
        if n == 1 {
            SOLO_DRAINS.fetch_add(1, Relaxed);
        }
    }
    vm.lane.draining = true;
    run_batch(vm);
    vm.lane.pending.clear();
    // The batch is complete, so every downstream over it is now an ordinary future.
    vm.lane.dependents.clear();
    vm.lane.draining = false;
}

/// Lock a slot, ignoring poisoning: a panicking worker's task is already an abort,
/// and the coordinator must still reach its serial retry.
pub(crate) fn lock(slot: &std::sync::Mutex<LaneSlot>) -> std::sync::MutexGuard<'_, LaneSlot> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lane batch shape: how many drains, how many tasks they carried, the largest batch, and how
/// many drains carried exactly one task. Read by the periodic PC dump (`[PC-LANE]`).
pub(crate) static DRAINS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static DRAINED_TASKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static MAX_BATCH_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static SOLO_DRAINS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One line of lane-batch census, for the periodic dump and the gate harness.
pub(crate) fn batch_census() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let (d, t, m, s) = (
        DRAINS.load(Relaxed),
        DRAINED_TASKS.load(Relaxed),
        MAX_BATCH_SEEN.load(Relaxed),
        SOLO_DRAINS.load(Relaxed),
    );
    format!(
        "drains={d} tasks={t} mean={:.2} max={m} solo={s} ({:.1}% of drains)",
        if d == 0 { 0.0 } else { t as f64 / d as f64 },
        if d == 0 { 0.0 } else { s as f64 * 100.0 / d as f64 }
    )
}
