//! The lane rings, their retries and the deferred work between them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

/// GILFREE-RINGS stage 4 (DEFAULT ON; `RAVA_NO_GILFREE_RINGS` opts out). Three deltas over
/// the stage-3 lane, all measured against the ring phase of the MC boot
/// (`ChunkGeneratorStructureState.lambda$generateRingPositions$5`, 120 tasks):
///
/// 1. INSTANCE lambda impls are admitted. javac compiles a private lambda body
///    to `REF_invokeVirtual` (MethodHandle kind 5), captured receiver first —
///    the stage-3 `kind != 6` predicate rejected every ring task at the first
///    line of `plannable`. The receiver becomes local slot 0 of the compiled
///    frame, exactly the JVM's instance-frame layout.
/// 2. The compiled COW arm is ARMED on the worker's private `JitCtx`
///    (`jit_ctx.cow_armed = 1`), which is what turns every compiled inline
///    putfield into a `rava_putfield` callback — the codegen barrier the
///    stage-3 note called "the real wall". Armed, the worker either writes an
///    object it OWNS (its own TLAB view) or deopts.
/// 3. Array stores get the same treatment through a dedicated codegen arm
///    (`rava_ring_store_guard`), because the field arm does not cover them.
/// THE RING LANE — DEFAULT ON (`RAVA_NO_GILFREE_RINGS` turns it off).
///
/// The numbers now say it should be on. It was a loss for as long as the region ran one worker at
/// a time; what changed is that `worker_direct` lets a worker build its own direct-call entries
/// and `warm_compile` compiles the once-per-task callee a call-count threshold can never see.
/// Each was measured neutral or worse ALONE — together they are what makes the region parallel.
///
/// MEASURED. Process CPU-time delta per wall second: a sustained **4.0–4.67 cores** for ~25 s,
/// against a flat 0.98 in every earlier configuration. Ring batch 57.1 s → 39.7 s, 127/127 kept.
/// Interleaved boot A/B, min-of-3, the lane as the only variable:
///
/// ```text
/// wall  off min=100.1  [101/100.6/100.1]        on min=92.4   [93.9/93.9/92.4]        -7.69% ON CLEAN
/// done  off min=34.319 [34.565/34.319/34.344]   on min=27.122 [28.131/27.665/27.122] -20.97% ON CLEAN
/// ab    off min=42     [42/42/42]               on min=40     [41/41/40]              -4.76% ON CLEAN
/// ```
///
/// Every ON run beat every OFF run on all three, and the guest log is identical between arms
/// modulo timestamps.
///
/// SHIPPING IT required resolving a CONTRACT, not a number. Flipping it first failed
/// `s09_worker_illegal_map_put_aborts_without_publication`, because the frozen-field guards
/// forbade ALL gil-free mutation while `jit::vm_ref` already admitted `&mut Vm` inside the locked
/// envelope — a contradiction that stayed invisible only because no checked build had ever run a
/// worker through that envelope. The guards now exempt the envelope and nothing else, and the
/// half of the contract that had no test (publication INSIDE it) is covered by
/// `s09_worker_putstatic_publishes_inside_the_locked_envelope`. The abort assertions themselves
/// were not touched.
///
/// It was opt-in for as long as it was a LOSS: the region completed but ran strictly one worker at
/// a time, so it cost 10-45% on `Done` for nothing. What changed is not this gate — it is that
/// `worker_direct` lets a worker build its own direct-call entries and `warm_compile` compiles the
/// once-per-task callee that a call-count threshold can never see. Each was measured neutral or
/// worse ALONE; together they are what makes the region actually parallel.
///
/// MEASURED (process CPU-time delta per wall second): a sustained **4.0-4.67 cores** for ~25 s,
/// against a flat 0.98 in every earlier configuration. Interleaved boot A/B, min-of-3, the lane as
/// the only variable:
///
/// ```text
/// wall  off min=100.1  [101/100.6/100.1]        on min=92.4   [93.9/93.9/92.4]        -7.69% ON CLEAN
/// done  off min=34.319 [34.565/34.319/34.344]   on min=27.122 [28.131/27.665/27.122] -20.97% ON CLEAN
/// ab    off min=42     [42/42/42]               on min=40     [41/41/40]              -4.76% ON CLEAN
/// ```
///
/// Every ON run beat every OFF run on all three. Guest output is identical between the arms modulo
/// timestamps.
/// REQUIRES THE JIT. A worker never compiles — that is a hard invariant, not a policy — so with
/// `RAVA_JIT=0` the first call every admitted task makes is a direct miss, the region aborts, and
/// the latched `InternalError: direct miss` reaches guest code. Measured: an interpreter-only boot
/// dies at 133 s with
/// `direct_miss caller=ReloadableServerRegistries ... UnsupportedNative("getstatic LootPool.CODEC")`
/// raised on a ring worker, never on the main thread. The lane has nothing to offer an engine with
/// no compiled bodies, so it stays closed.
pub(crate) fn rings() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_GILFREE_RINGS").is_none() && crate::flags::jit_enabled_env()
    })
}

/// ⭐**STRUCTURAL PURITY ADMISSION FOR THE RINGS LANE** (`RAVA_LANE_PURE_ADMIT=1`, default OFF).
///
/// `plannable` lets the rings lane skip the static offloadability gate and rely on the runtime
/// barriers instead (see the `!rings()` term there). Measured on the product boot, that trades a
/// cheap refusal for an expensive one: an admitted `ReloadableServerRegistries` body walked
/// `vm_ref` → `put_object` → `aastore` → `ring_store_guard` → nursery overflow, burned 108,477
/// callbacks and two claimed nursery slices, and still finished `completed=0/2 keep=false` — the
/// result discarded and the body re-run serially in PHASE 5. Every barrier it hit is correct; the
/// defect is admitting a body whose writes they are guaranteed to refuse.
///
/// ON, the lane additionally requires `offload::analyze` to prove the body writes nothing shared:
/// `putstatic` is refused outright, a `putfield`/`aastore` is refused unless its target is FRESH
/// in the analysed frame, and every invoke target is recursed through. No class or method NAME
/// takes part — the predicate reads opcodes, descriptors and the loaded hierarchy only — so this
/// is a structural gate, not a workload filter.
///
/// Default OFF so the shape stays bit-identical until the A/B prices it.
pub(crate) fn pure_admit() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LANE_PURE_ADMIT").is_some())
}

/// Explicit A/B switch for the write-only field transaction prototype.
/// Default OFF: no worker admission changes until parity and latency are measured.
pub(crate) fn field_txn_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        cfg!(feature = "next-gen-gc")
            && std::env::var("RAVA_LANE_FIELD_TXN")
                .map(|value| value == "1")
                .unwrap_or(false)
    })
}

pub(crate) fn warm_targets_drain() -> Vec<(String, String, String)> {
    let Some(set) = WARM_TARGETS.get() else {
        return Vec::new();
    };
    let mut s = set.lock().unwrap_or_else(|e| e.into_inner());
    s.drain().collect()
}

/// How many times a region may be re-opened after a WARMABLE abort (`RAVA_LANE_RETRIES`,
/// default 2). Zero restores the single-attempt behaviour.
pub(crate) fn lane_retries() -> u32 {
    pub(crate) static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_LANE_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(2)
    })
}

/// Everything a plan needs that RESOLUTION can establish: the compiled entry, the
/// return shape, and the frame geometry. Deliberately holds NO packed capture and
/// NO reserved window — building one may load a class, run a `<clinit>` or compile
/// a body, i.e. run guest code and collect, and neither a packed arena pointer nor
/// a carved nursery slice survives a collection.
pub(crate) struct PlanMeta {
    pub(crate) entry: usize,
    pub(crate) ret: Option<jit::JT>,
    pub(crate) ret_desc: String,
    /// The lambda handle whose captures fill the frame — re-read at pack time so
    /// the packed cells are taken from the CURRENT (post-collection) object.
    pub(crate) handle: u32,
    /// ARENA TWIN: set when the task is a `RAVA_ARENA_LAMBDA` closure, which has no
    /// heap handle at all. `pack_and_claim` reads its captures from the arena cells
    /// instead of `heap.lambda_of(handle)`. Additive on purpose — `handle` stays the
    /// handle path's field so nothing downstream of it changes shape.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena: Option<crate::arena::ArenaPtr>,
    /// Wide-slot flag per capture, receiver slot already prepended for an
    /// instance impl.
    pub(crate) params: Vec<bool>,
    pub(crate) frame_len: usize,
    pub(crate) max_locals: usize,
    /// The plan passed the stricter write-only shared-field transaction proof.
    pub(crate) field_txn: bool,
}

/// The lane's deferred batch. Owned by the `Vm` so both collectors can root it —
/// an arbitrary number of main-thread collections can fall between `defer` and
/// `drain`, and a swept supplier or future would be a silent wrong answer.
#[derive(Default)]
pub(crate) struct LaneBatch {
    pub(crate) pending: Vec<LaneTask>,
    /// Re-entrancy latch: a serial fallback inside `drain` runs guest code that may
    /// itself call `supplyAsync`. Such a task must NOT join the batch being drained
    /// (it would never run); it takes the unchanged inline path.
    pub(crate) draining: bool,
    /// TRANSITIVE OBSERVATION SET. A chaining method on a deferred future does not need
    /// it complete — `cf_chain` registers a callback and `allOf` counts pending inputs
    /// down — so those calls no longer drain. But the future they HAND BACK is the one
    /// the guest will eventually join, and a join on it must still force the batch, or
    /// the callback never fires and `Util.blockUntilDone` spins on a future nothing can
    /// complete. So every downstream built over a lane future is recorded here and
    /// counts as observing it.
    ///
    /// Plain `u32` handles: `Ref` is an index into the handle table, which the moving
    /// collector updates in place, so these stay valid across a collection without being
    /// roots (they are not ownership — a swept downstream simply never gets joined).
    pub(crate) dependents: std::collections::HashSet<u32>,
}

impl LaneBatch {
    /// Forward every batched root in place — the MOVING arena collector.
    pub(crate) fn visit_roots(&mut self, mut visit: impl FnMut(&mut Value)) {
        for entry in &mut self.pending {
            visit(&mut entry.task);
            visit(&mut entry.future);
            for a in entry.args.iter_mut() {
                visit(a);
            }
        }
    }
    /// Seed the mark-sweep root set (by value).
    pub(crate) fn gather_roots(&self, roots: &mut Vec<Value>) {
        for entry in &self.pending {
            roots.push(entry.task.clone());
            roots.push(entry.future.clone());
            roots.extend(entry.args.iter().cloned());
        }
    }
}

/// Is `h` a future whose completion the lane owes — either one it deferred, or a
/// downstream chained over one?
pub(crate) fn lane_linked(vm: &Vm, h: u32) -> bool {
    if vm.lane.pending.is_empty() {
        return false;
    }
    vm.lane.dependents.contains(&h)
        || vm
            .lane
            .pending
            .iter()
            .any(|p| matches!(p.future, Value::Ref(x) if x == h))
}

/// Batch size and the class of its first task — for the drain trace only.
pub(crate) fn pending_shape(vm: &Vm) -> (usize, String) {
    let first = vm
        .lane
        .pending
        .first()
        .map(|p| crate::taskgraph_probe::owner_of(vm, &p.task))
        .unwrap_or_default();
    (vm.lane.pending.len(), first)
}

/// Record a downstream future built over a lane future (see `LaneBatch::dependents`).
pub(crate) fn note_dependent(vm: &mut Vm, down: u32) {
    vm.lane.dependents.insert(down);
}

/// Offer a `supplyAsync` task to the lane. `true` = the lane took it and the caller
/// must return its PENDING future; `false` = unchanged behaviour at the seam.
/// WHY a `supplyAsync` body did not reach the lane, by arm. The admission census in
/// `plan.rs` records only refusals it MAKES, so a boot that showed four admits and an EMPTY
/// rejection table meant the body never reached `plannable` at all — and nothing said which
/// of the three arms above it turned the body away. Four relaxed adds on a path that runs a
/// few hundred times a boot.
pub(crate) static DEFER_WHY: [std::sync::atomic::AtomicU64; 5] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 5];
pub(crate) const DEFER_ARMS: [&str; 5] = [
    "disabled/trampoline/worker",
    "draining",
    "batch-full",
    "unplannable",
    "DEFERRED",
];

/// One line, or empty when nothing has been offered yet.
pub(crate) fn defer_why_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let v: Vec<u64> = DEFER_WHY.iter().map(|a| a.load(Relaxed)).collect();
    let total: u64 = v.iter().sum();
    if total == 0 {
        return String::new();
    }
    format!(
        "offered={total} | {}",
        DEFER_ARMS
            .iter()
            .zip(v.iter())
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

pub(crate) fn defer(vm: &mut Vm, task: &Value, future: &Value) -> bool {
    defer_with_args(vm, task, future, Vec::new())
}

/// `defer`, carrying the SAM arguments the body is applied to (see `LaneTask::args`).
pub(crate) fn defer_with_args(vm: &mut Vm, task: &Value, future: &Value, args: Vec<Value>) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if !enabled() || vm.trampoline || crate::gil::is_worker() {
        DEFER_WHY[0].fetch_add(1, Relaxed);
        return false;
    }
    // A task produced by a serial fallback inside the drain, or a batch already at
    // its cap, stays on the unchanged inline path.
    if vm.lane.draining {
        DEFER_WHY[1].fetch_add(1, Relaxed);
        return false;
    }
    if vm.lane.pending.len() >= MAX_BATCH {
        DEFER_WHY[2].fetch_add(1, Relaxed);
        return false;
    }
    let Some(target) = plannable_with_target(vm, task) else {
        DEFER_WHY[3].fetch_add(1, Relaxed);
        return false;
    };
    DEFER_WHY[4].fetch_add(1, Relaxed);
    pub(crate) static PROOF_CENSUS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *PROOF_CENSUS.get_or_init(|| std::env::var_os("RAVA_LANE_PROOF_CENSUS").is_some()) {
        let report = vm.offload_census_devirt(&target.owner, &target.name, &target.desc);
        // ADDITIVE store-form join (`RAVA_LANE_STORE_CENSUS`): appended to the existing
        // line only when the census is on; evidence of emitted store FORM, never a
        // concurrency proof.
        let store_join = match &report.store_join {
            Some(join) => format!(" store_join={join}"),
            None => String::new(),
        };
        eprintln!(
            "[LANE-PROOF] {}.{}{} dispatch_closed={} class_load_closed={} write_set_closed={} dry_loaded={} dry_names={:?} reason={}{}",
            target.owner,
            target.name,
            target.desc,
            report.dispatch_closed,
            report.class_load_closed,
            report.write_set_closed,
            report.dry_loaded,
            report.dry_names,
            report.reason.as_deref().unwrap_or("admitted"),
            store_join
        );
    }
    // ISOLATION PROBE (RAVA_GILFREE_NOLANE): `plannable` still force-compiles the
    // impl, but nothing routes off-GIL — it stays an inline guest task.
    // Distinguishes a force_compile bug from an off-GIL-run bug.
    pub(crate) static NOLANE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if crate::jit::env_gate("RAVA_GILFREE_NOLANE", &NOLANE) {
        return false;
    }
    vm.lane.pending.push(LaneTask {
        task: task.clone(),
        future: future.clone(),
        args,
    });
    true
}
