//! Persistent, per-VM background Cranelift compilation.
//!
//! A `JitFn` points into the compiler thread's thread-local `JITModule`; the
//! worker therefore lives for exactly as long as the owning `Vm`. Planning stays
//! on the VM thread, execution stays on the VM thread, and only pure codegen is
//! moved off-thread.

mod queue;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rava_classfile::Code;

use crate::jit::{self, InlineCallee, JitFnParts, JT, StaticIntrinsic};
use crate::LoadedClass;
use queue::{Envelope, Permit, PermitPool};

#[cfg(feature = "next-gen-gc")]
use crate::jit::{ArenaPrimArrayPlan, ConcatShape, CtorInline, DeepShape};

pub(crate) struct CompileJob {
    pub key: usize,
    pub class: Arc<LoadedClass>,
    pub class_name: String,
    pub name: String,
    pub descriptor: String,
    pub field_ids: HashMap<(String, String), (u32, Option<u32>)>,
    pub static_intrinsics: HashMap<u16, StaticIntrinsic>,
    pub inline_targets: HashMap<u16, InlineCallee>,
    pub native_calls: HashSet<u16>,
    pub statics: HashMap<u16, (u32, JT)>,
    #[cfg(feature = "next-gen-gc")]
    pub new_arena: HashMap<u16, (u32, u32)>,
    #[cfg(feature = "next-gen-gc")]
    pub ctor_inlines: HashMap<u16, CtorInline>,
    #[cfg(feature = "next-gen-gc")]
    pub concat_shapes: HashMap<u16, ConcatShape>,
    #[cfg(feature = "next-gen-gc")]
    pub hof_fusions: HashMap<usize, crate::hof_fusion::HofFusionPlan>,
    #[cfg(feature = "next-gen-gc")]
    pub prim_arrays: ArenaPrimArrayPlan,
    #[cfg(feature = "next-gen-gc")]
    pub getter_inlines: HashMap<u16, (u32, JT)>,
    #[cfg(feature = "next-gen-gc")]
    pub deep: Option<DeepShape>,
    /// cp-indices whose field must keep its runtime tag under the bare-deref flip. Computed
    /// on the VM thread because deciding it needs the loader (a field declared as a type with
    /// a native-backed ANCESTOR always holds a handle), and this thread has no `Vm`.
    #[cfg(feature = "next-gen-gc")]
    pub tagged_fields: HashSet<u16>,
    pub reserved_static_slots: u32,
    /// High-water mark before this job reserved, so a rollback can prove the block is still on
    /// top instead of assuming it (see `Heap::free_static_slots_from`).
    pub reserved_static_base: u32,
}

pub(crate) struct CompileResult {
    pub key: usize,
    pub class_name: String,
    pub name: String,
    pub descriptor: String,
    pub compiled: Option<JitFnParts>,
    pub reserved_static_slots: u32,
    pub reserved_static_base: u32,
    pub elapsed: Duration,
}

pub(crate) enum EarlyReserveError {
    Full,
}

pub(crate) enum SubmitError {
    Full(CompileJob),
    Disconnected(CompileJob),
}

pub(crate) struct BackgroundJit {
    jobs: SyncSender<Envelope<CompileJob>>,
    permits: Option<Arc<PermitPool>>,
    results: Receiver<CompileResult>,
    pending_results: Arc<std::sync::atomic::AtomicUsize>,
    join: Vec<JoinHandle<()>>,
}

impl BackgroundJit {
    pub(crate) fn from_env() -> Option<Self> {
        // Default ON since 2026-08-03 A/B (bench/bgjit_ab.ps1, 3 pairs, real boot):
        // wallToDone 96.3s (off) vs 93.0s (on) — ~3.5% wall, 2/3 pairs in favor,
        // doneNs flat. Small but free; opt out with RAVA_BACKGROUND_JIT=0.
        let enabled = std::env::var("RAVA_BACKGROUND_JIT")
            .map(|value| !matches!(value.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        enabled.then(Self::new)
    }

    fn new() -> Self {
        // One queued job plus one executing job bounds planning memory and avoids
        // compiling a long cold tail that would never be called again.
        //
        // ⭐**DEPTH 8, AND IT NOW HAS THE NUMBER THE COMMENT ASKED FOR.** At depth 1 the VM
        // thread's `try_send` fails whenever the compiler is busy and that job is simply
        // DROPPED — the method keeps running interpreted until it happens to be offered again,
        // which during a boot's compile storm is most of them. The old note weighed that against
        // "the cold-tail risk" and said neither side had a number.
        //
        // Measured, product configuration, ABBA over five pairs (`bench/jvm_cpu.ps1`):
        //
        // ```text
        // depth 1   Done  0.490 0.495 0.495 0.519 0.585
        // depth 8   Done  0.415 0.417 0.419 0.423 0.433
        // ```
        //
        // **Completely disjoint — max(8) = 0.433 below min(1) = 0.490, −18.6%.** Wall is
        // 47.6 vs 48.0 s, overlapping, and RSS is indistinguishable (1153-1433 MB in both arms),
        // so the cold-tail memory the old comment feared does not show up either. `Done` is the
        // window where the compile storm and the work overlap most, which is exactly where a
        // dropped job costs the most.
        //
        // ⛔**AND 8 IS THE PLATEAU, SO DO NOT SWEEP IT AGAIN**: depth 32 reads Done
        // 0.415/0.425 and depth 64 reads 0.431, both overlapping depth 8's 0.417/0.433. The win
        // is in going from "drop the job" to "queue the job", not in queueing more of them.
        //
        // `RAVA_BACKGROUND_JIT_QUEUE=1` restores the old depth.
        let depth = std::env::var("RAVA_BACKGROUND_JIT_QUEUE")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|d| *d >= 1)
            .unwrap_or(8);
        // ⭐**HOW MANY COMPILER THREADS. `RAVA_BG_JIT_THREADS`, default 1 until measured.**
        // A product boot reads `passed_warmup=18,066` against `compiled=1,786`: a method that
        // crosses the warm-up gate while the queue is full is DROPPED back to `Cold(n2)` and has
        // to climb the counter again (see the `TrySendError` arm in `jit_run`). With one worker
        // and a boot-long compile storm that is the common outcome — and the host profile agrees,
        // putting **89.9% of samples in the Rust host and ~10% in compiled guest code** against a
        // JVM that runs this program essentially all-compiled and uses 3.09 cores doing it while
        // we use 1.06.
        //
        // Each worker owns its own thread-local `JITModule`, so N of them is N modules, all
        // living as long as the `Vm` — the same lifetime contract one worker already has. The job
        // receiver is shared behind a mutex; a worker holds it only for the `recv`.
        let threads = std::env::var("RAVA_BG_JIT_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n >= 1 && *n <= 8)
            // ✅**FOUR, AND THE OLD "NEUTRAL" WAS AN ARTEFACT OF THE INSTRUMENT.** The verdict
            // below was taken on the BOOT harness, which the null test later measured at **16.7%
            // intrinsic spread on an idle box** — eight identical boots spanning 27.0-31.5 s,
            // because the background compiler gets through a different set of methods every run.
            // A harness whose noise IS compilation scheduling cannot price compilation scheduling.
            //
            // Re-priced on the warm-reload harness (A-arm spread 0.9%), 4 pairs interleaved:
            //   RELOAD_MS  -2.73% min  -2.89% median   every B below every A (DISJOINT)
            //   WINDOW_MS  -1.80% min  -1.89% median   every B below every A (DISJOINT)
            //   cycles     +2.59% min  +2.77% median   every B above every A (DISJOINT)
            // Exactly the shape parallel compilation should have: the same work finishes sooner in
            // wall terms and costs more total cycles. `RELOAD2_MS` (warm, no compilation) and
            // `cpu_ms` both overlap, so nothing is paid on the steady state.
            //
            // ⛔**EIGHT IS TOO MANY, measured the same way**: `RELOAD_MS` -3.63%/-2.36% but the
            // arms overlap, while `RELOAD2_MS` +1.73%/+3.69%, `cpu_ms` +3.44% and `cycles` +3.33%
            // are all DISJOINT the WRONG way — eight workers start charging the warm pass for
            // eight `JITModule`s and their scheduling. Four is the optimum, not a direction.
            //
            // ⛔**The superseded verdict, kept because its reasoning is still sound and only its
            // instrument was wrong.** The drop counter says **17,652 of 19,476
            // warm-up passages are refused because this queue is full = 90.6%**, which reads like
            // a starved compiler. It is not. Alternating boots, P-cores:
            //   threads=1  wall 39.2 39.6 39.9  cores 1.04-1.05
            //   threads=4  wall 39.9 39.5 41.7  cores 1.07-1.09
            // Four workers did 0.04 cores of extra work and moved no wall. ⭐The queue fills in
            // BURSTS, not because compiling is slow, and a dropped method is re-offered and
            // eventually compiled anyway — the boot ends with 1,793 compiles against 1,126
            // methods that ever cross the 512 cold level. The drop is not the loss it looks like.
            .unwrap_or(4);
        let queue_capacity = depth.max(threads * 4);
        let early_reserve = std::env::var("RAVA_BG_JIT_EARLY_RESERVE")
            .map(|value| value.trim() == "1")
            // First-entry mode submits a larger cold population. Reserve before planning
            // so a full queue does not repeatedly build jobs it cannot accept.
            // Explicit 0 retains the control; ordinary threshold mode keeps its default.
            .unwrap_or_else(|_| std::env::var_os("RAVA_FIRST_ENTRY_JIT").is_some());
        let permits = early_reserve.then(|| PermitPool::new(queue_capacity));
        let (jobs, job_rx) = mpsc::sync_channel(queue_capacity);
        let (result_tx, results) = mpsc::channel();
        let shared = Arc::new(std::sync::Mutex::new(job_rx));
        let pending_results = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut joins = Vec::with_capacity(threads);
        for i in 0..threads {
            let rx = Arc::clone(&shared);
            let tx = result_tx.clone();
            let pending_results = Arc::clone(&pending_results);
            joins.push(
                thread::Builder::new()
                    .name(format!("rava-jit-compiler-{i}"))
                    .spawn(move || compiler_loop_shared(rx, tx, pending_results))
                    .expect("spawn persistent RAVA JIT compiler"),
            );
        }
        drop(result_tx);
        Self {
            jobs,
            permits,
            results,
            pending_results,
            join: joins,
        }
    }

    /// `Ok(None)` is the legacy path. The opt-in path either returns a permit immediately or
    /// reports a full queue without blocking or doing any planning work.
    pub(crate) fn try_early_reserve(&self) -> Result<Option<Permit>, EarlyReserveError> {
        match &self.permits {
            None => Ok(None),
            Some(permits) => permits
                .try_acquire()
                .map(Some)
                .ok_or(EarlyReserveError::Full),
        }
    }

    pub(crate) fn try_submit(
        &self,
        job: CompileJob,
        permit: Option<Permit>,
    ) -> Result<(), SubmitError> {
        self.jobs
            .try_send(Envelope::new(job, permit))
            .map_err(|error| match error {
                TrySendError::Full(job) => SubmitError::Full(job.into_inner()),
                TrySendError::Disconnected(job) => SubmitError::Disconnected(job.into_inner()),
            })
    }

    pub(crate) fn results_ready(&self) -> bool {
        self.pending_results.load(std::sync::atomic::Ordering::Acquire) > 0
    }

    pub(crate) fn try_take(&self) -> Option<CompileResult> {
        let result = self.results.try_recv().ok()?;
        self.pending_results
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        Some(result)
    }

    /// The ONE park on the compile path: `Receiver::recv` on the result channel (std's mpmc list
    /// channel — `Channel::start_recv`, then `Context::wait_until` → `thread::park` →
    /// `WaitOnAddress` → `ZwWaitForAlertByThreadId` when the queue is empty). Reached only from
    /// `Vm::wait_for_background_jit`, itself reached only from `Vm::force_compile`'s `Compiling`
    /// arm; see the `hr_wait` module at the foot of this file for who asks and who no longer
    /// waits.
    ///
    /// Semantics are exactly `self.results.recv().ok()`: `try_recv` first so a result that is
    /// ALREADY in the channel is not counted as a park (recv would return it without sleeping),
    /// then the same blocking `recv` for the empty case. `Disconnected` is only ever returned by
    /// `try_recv` when the channel is empty AND closed, which is precisely when `recv` returns
    /// `Err` too. One relaxed add per park — a park is a scheduler round trip, never a per-call
    /// path.
    pub(crate) fn take_blocking(&self) -> Option<CompileResult> {
        match self.results.try_recv() {
            Ok(result) => {
                self.pending_results
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                Some(result)
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let started = std::time::Instant::now();
                let result = self.results.recv().ok();
                if result.is_some() {
                    self.pending_results
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }
                hr_wait::note_park(started.elapsed().as_nanos() as u64);
                result
            }
        }
    }
}

// ═══════════════ HR-WAIT: THE PARK ON THE COMPILE QUEUE (kill switch `RAVA_NO_HR_WAIT=1`) ═══════════════
//
// ⭐**`ZwWaitForAlertByThreadId` UNDER COMPILED BODIES ON A SINGLE-THREADED BENCH IS THE MAIN
// THREAD SLEEPING ON ITS OWN COMPILER.** Every park reachable from the JIT entry/compile path
// was walked for this:
//   * `BackgroundJit::take_blocking` (above) — `results.recv()`. Reached ONLY through
//     `Vm::wait_for_background_jit`, whose ONLY caller is the `Compiling` arm of
//     `Vm::force_compile`. This is the one that is removed here.
//   * `try_submit` is `try_send` and never blocks (a full queue is a soft miss, `Cold(n2)`);
//     `try_take` is `try_recv` and never blocks; the job-queue mutex is taken only by the
//     compiler threads, never by the VM thread.
//   * `gil::acquire` — parks only when contended, and a compiler thread never takes the GIL;
//     the checkpoint hand-off runs only when a real guest OS thread exists. It has its own spin
//     and its own `[PARK]` census (`gil.rs`, "THE PARK BEFORE THE GIL") and is not touched.
//   * `monitor.rs` — `Condvar` for contended `monitorenter` / `Object.wait`: guest semantics,
//     uncontended on one thread, not touched.
//
// **What the removed park was buying.** `force_compile` is asked for a body whose `jit_cache`
// entry is `Compiling`, i.e. the method ALREADY crossed its own threshold and its job is already
// in a depth-8 FIFO in front of ONE compiler thread. The historic arm slept until that thread had
// emitted everything queued ahead plus the body itself, publishing the strangers as they arrived.
// Who asks, and whether they need the body NOW:
//   * `vm/dispatch.rs` dispatch-tier arm (64th miss to a callee body from compiled code): the
//     result is used only to return `Compiled(fptr)` instead of `NotYet`, and `NotYet` is that
//     site's own documented fallback — "retry (and cache) on a later call". Does NOT need it now.
//   * `vm/invoke.rs::flat_compile_first_use` (`let _ = force_compile`): a one-shot fixed-policy
//     request; a `Compiling` body is already on its way. Does NOT need it now.
//   * `vm/jit_background.rs::drain_worker_hot` (`let _ = force_compile`): a worker's note served
//     on the main thread's passage; the worker is not waiting. Does NOT need it now.
//   * the gilfree lane (`gilfree_cf/{batch,plan}.rs`): checks the bool and opens a region a
//     worker must not `direct miss` in. DOES need it — stays blocking.
//
// **Mechanism.** A caller that does not need the body opens a `DeferScope` (a const-init,
// destructor-free thread-local `Cell<u8>` carrying the caller kind, RAII-restored so a nested
// blocking caller is impossible to leave deferred). `wait_for_background_jit` asks `defer()`
// first: inside a scope, with the switch off, it returns without touching the channel and the
// entry stays `Compiling`; the result is published by the next `drain_background_jit` (every
// main-thread `jit_run` passage and the first line of every `force_compile`), and the asking
// site picks the fptr up on a later call exactly as it does for any callee that is `NotYet`.
// Outside a scope — the lane — the historic blocking loop runs verbatim. The publication path
// (`publish_background_jit`) is untouched: same channel, same thread, same ordering; only WHEN the
// receive happens moves, from "now, sleeping" to "the next passage, non-blocking".
//
// **Why the guard cannot admit an unsafe shape.** Deferring never changes a `jit_cache` state; it
// only declines to advance `Compiling` → `Compiled` at this instant. Every consumer of the cache
// already handles `Compiling` (it is the state every background compile sits in between submit
// and drain), so a deferred caller sees the same world a caller one passage earlier saw.
//
// Kill switch: `RAVA_NO_HR_WAIT=1` (or the block-wide `RAVA_NO_EMIT_MORE=1`) → `defer()` is
// always false → the historic park, byte for byte. Resolved once (`OnceLock`), and only on the
// wait path, never on the miss path. Reported as `[HR-WAIT]` from the periodic profiler dump
// (`pc_profile/dump.rs`, beside `compile_bail::report`).
pub(crate) mod hr_wait {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::OnceLock;

    /// Who is asking `force_compile` for a body that is already queued. The discriminant indexes
    /// the counters below; `UNSCOPED` (no scope open) is the historic blocking caller.
    #[derive(Clone, Copy)]
    #[repr(u8)]
    #[allow(dead_code)] // Opened only by the fixed lambda first-use request.
    pub(crate) enum WaitCaller {
        DispatchTier = 0,
        FlatFirstUse = 1,
        WorkerHot = 2,
    }
    const UNSCOPED: u8 = 3;
    const CALLERS: [&str; 4] = [
        "dispatch-tier",
        "flat-first-use",
        "worker-hot",
        "unscoped(lane)",
    ];

    thread_local! {
        static SCOPE: Cell<u8> = const { Cell::new(UNSCOPED) };
    }

    static PARKS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
    static PARK_NS: AtomicU64 = AtomicU64::new(0);
    static PARK_MAX_NS: AtomicU64 = AtomicU64::new(0);
    static DEFERRED: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

    /// RAII: the caller kind for every `force_compile` made while this is live. Nested scopes
    /// restore the previous kind on drop, so a blocking caller can never inherit a deferral.
    pub(crate) struct DeferScope(u8);

    impl DeferScope {
        #[inline]
        pub(crate) fn enter(caller: WaitCaller) -> Self {
            Self(SCOPE.with(|s| s.replace(caller as u8)))
        }
    }

    impl Drop for DeferScope {
        #[inline]
        fn drop(&mut self) {
            SCOPE.with(|s| s.set(self.0));
        }
    }

    fn disabled() -> bool {
        static OFF: OnceLock<bool> = OnceLock::new();
        *OFF.get_or_init(|| {
            // MEASURED OPT-IN on the bootstrap bench (3 pairs, B = disabled): -0.65/-1.40/-1.52% min,
            // -1.26/-1.77/-1.43% median -- NOT parking is slower on a single-threaded run, because the
            // caller then interprets until a later passage picks the fptr up, and that costs more than
            // the queue makespan it avoided. The park is the historic default; `RAVA_FAST_HR_WAIT=1`
            // defers instead.
            std::env::var_os("RAVA_FAST_HR_WAIT").is_none() || crate::flags::emit_more_disabled()
        })
    }

    /// True iff the wait about to happen may be skipped: a `DeferScope` is open and the switch
    /// is off. Unscoped callers get `false` before the `OnceLock` is even read.
    #[inline]
    pub(crate) fn defer() -> bool {
        let caller = SCOPE.with(|s| s.get());
        if caller == UNSCOPED || disabled() {
            return false;
        }
        DEFERRED[caller as usize].fetch_add(1, Relaxed);
        true
    }

    pub(super) fn note_park(ns: u64) {
        let caller = SCOPE.with(|s| s.get()) as usize;
        PARKS[caller.min(UNSCOPED as usize)].fetch_add(1, Relaxed);
        PARK_NS.fetch_add(ns, Relaxed);
        PARK_MAX_NS.fetch_max(ns, Relaxed);
    }

    pub(crate) fn report() -> String {
        let parks: u64 = PARKS.iter().map(|c| c.load(Relaxed)).sum();
        let deferred: u64 = DEFERRED.iter().map(|c| c.load(Relaxed)).sum();
        if parks + deferred == 0 {
            return String::new();
        }
        let by = |counters: &[AtomicU64; 4]| {
            counters
                .iter()
                .zip(CALLERS)
                .filter_map(|(c, name)| {
                    let n = c.load(Relaxed);
                    (n > 0).then(|| format!("{name}={n}"))
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "[HR-WAIT] compile-queue parks={parks} ({:.1} ms, max {:.2} ms) by caller: {} | deferred={deferred} (kept interpreting, picked up on a later passage) by caller: {}",
            PARK_NS.load(Relaxed) as f64 / 1e6,
            PARK_MAX_NS.load(Relaxed) as f64 / 1e6,
            by(&PARKS),
            by(&DEFERRED),
        )
    }
}

impl Drop for BackgroundJit {
    fn drop(&mut self) {
        // Dropping the last sender asks the worker to exit after any in-flight
        // compile. Joining keeps its `JITModule` mapped until `Vm::jit_cache`
        // (declared before this field) has dropped all `JitFn`s.
        let (dummy_tx, dummy_rx) = mpsc::sync_channel(0);
        let old = std::mem::replace(&mut self.jobs, dummy_tx);
        drop(old);
        drop(dummy_rx);
        for join in self.join.drain(..) {
            let _ = join.join();
        }
    }
}

/// Shared-receiver worker. The mutex is held ONLY for the `recv`, never across a compile, so N
/// workers overlap their Cranelift time; the lock is a queue head, not a compile lock.
fn compiler_loop_shared(
    jobs: Arc<std::sync::Mutex<Receiver<Envelope<CompileJob>>>>,
    results: mpsc::Sender<CompileResult>,
    pending_results: Arc<std::sync::atomic::AtomicUsize>,
) {
    loop {
        let job = {
            let guard = match jobs.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            match guard.recv() {
                // Refund an opt-in permit immediately after recv and before codegen.
                Ok(j) => j.into_inner(),
                Err(_) => return,
            }
        };
        let compiled = compile(job);
        if !publish_result(&results, &pending_results, compiled) {
            return;
        }
}
}
/// Publish the readiness signal before sending so the VM never decrements an unpublished result.
fn publish_result(
    results: &mpsc::Sender<CompileResult>,
    pending_results: &std::sync::atomic::AtomicUsize,
    result: CompileResult,
) -> bool {
    pending_results.fetch_add(1, std::sync::atomic::Ordering::Release);
    if results.send(result).is_err() {
        pending_results.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return false;
    }
    true
}

#[allow(dead_code)]
fn compiler_loop(jobs: Receiver<CompileJob>, results: mpsc::Sender<CompileResult>) {
    while let Ok(job) = jobs.recv() {
        let compiled = compile(job);
        if results.send(compiled).is_err() {
            return;
        }
    }
}

fn compile(job: CompileJob) -> CompileResult {
    let CompileJob {
        key,
        class,
        class_name,
        name,
        descriptor,
        field_ids,
        static_intrinsics,
        inline_targets,
        native_calls,
        statics,
        #[cfg(feature = "next-gen-gc")]
        new_arena,
        #[cfg(feature = "next-gen-gc")]
        ctor_inlines,
        #[cfg(feature = "next-gen-gc")]
        concat_shapes,
        #[cfg(feature = "next-gen-gc")]
        hof_fusions,
        #[cfg(feature = "next-gen-gc")]
        prim_arrays,
        #[cfg(feature = "next-gen-gc")]
        getter_inlines,
        #[cfg(feature = "next-gen-gc")]
        deep,
        #[cfg(feature = "next-gen-gc")]
        tagged_fields,
        reserved_static_slots,
        reserved_static_base,
    } = job;
    let started = std::time::Instant::now();
    let _phase = crate::jit_phase_trace::enter("background-codegen", key);
    let compiled = method_for_key(&class, key).and_then(|code| {
        let mut missing_field = false;
        let mut field_id = |owner: &str, field: &str| {
            field_ids
                .get(&(owner.to_string(), field.to_string()))
                .copied()
                .unwrap_or_else(|| {
                    missing_field = true;
                    (0, None)
                })
        };
        let _scope = jit::compile_scope();
        let output = jit::compile_with_static_intrinsics(
            code,
            &descriptor,
            &class.file.pool,
            Arc::as_ptr(&class) as i64,
            &mut field_id,
            &static_intrinsics,
            &inline_targets,
            &native_calls,
            &statics,
            #[cfg(feature = "next-gen-gc")]
            &new_arena,
            #[cfg(feature = "next-gen-gc")]
            &ctor_inlines,
            #[cfg(feature = "next-gen-gc")]
            &concat_shapes,
            #[cfg(feature = "next-gen-gc")]
            &hof_fusions,
            #[cfg(feature = "next-gen-gc")]
            &getter_inlines,
            #[cfg(feature = "next-gen-gc")]
            deep,
            // Precomputed on the VM thread: this thread has no `Vm`, which is exactly why
            // the set rides the job rather than being derived here.
            #[cfg(feature = "next-gen-gc")]
            Some(&tagged_fields),
            #[cfg(feature = "next-gen-gc")]
            &prim_arrays,
        );
        (!missing_field).then_some(output).flatten()
    });
    CompileResult {
        key,
        class_name,
        name,
        descriptor,
        compiled: compiled.map(jit::JitFn::into_parts),
        reserved_static_slots,
        reserved_static_base,
        elapsed: started.elapsed(),
    }
}

fn method_for_key(class: &LoadedClass, key: usize) -> Option<&Code> {
    class
        .file
        .methods
        .iter()
        .filter_map(|method| method.code.as_ref())
        .find(|code| std::ptr::eq(*code, key as *const Code))
}
#[cfg(test)]
mod pending_result_tests {
    use super::{publish_result, BackgroundJit, CompileResult};
    use std::sync::atomic::AtomicUsize;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    #[test]
    fn result_readiness_clears_when_vm_takes_completion() {
        let (jobs, _job_rx) = mpsc::sync_channel(1);
        let (result_tx, results) = mpsc::channel();
        let pending_results = Arc::new(AtomicUsize::new(0));
        let background = BackgroundJit {
            jobs,
            permits: None,
            results,
            pending_results: Arc::clone(&pending_results),
            join: Vec::new(),
        };
        let result = CompileResult {
            key: 0,
            class_name: String::new(),
            name: String::new(),
            descriptor: String::new(),
            compiled: None,
            reserved_static_slots: 0,
            reserved_static_base: 0,
            elapsed: Duration::ZERO,
        };

        assert!(publish_result(&result_tx, &pending_results, result));
        assert!(background.results_ready());
        assert!(background.try_take().is_some());
        assert!(!background.results_ready());
    }
}
