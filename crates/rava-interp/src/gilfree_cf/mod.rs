//! GILFREE-RINGS stage 3 (DEFAULT ON; `RAVA_NO_GILFREE_RINGS` opts out): the ALLOCATING
//! GIL-free lane for CompletableFuture tasks.
//!
//! Sits between the draconian detached tier (no alloc / no heap / no invokes)
//! and the GIL-bound resident tier: the task's COMPILED static-lambda body runs
//! on an executor worker WITHOUT the GIL, inside the same envelope the S2
//! chunk-fill fork proved (gilfree flag + per-worker nursery VIEW over main's
//! young + disjoint handle range + worker JitCtx). Soundness is the S09
//! runtime-barrier contract, not a full static admission: any worker-illegal
//! callback fires its stub → `request_abort` → the task re-runs as a plain
//! GIL-bound guest task (the guard-with-deopt discipline). Publication of the
//! future result happens ONLY under the GIL (`publish`).
//!
//! v1 scope (each rejection falls back to the resident tier, never fails):
//! static (kind-6) lambda impls whose body is ALREADY COMPILED, non-self-reg,
//! no exception handlers, captures packable as flat cells (no Str/sentinel).

use crate::jit;
use crate::value::Value;
use crate::Vm;

/// Tasks refused a claim purely because main's young region had no room left for
/// another `NURSERY_SLICE` (see `pack_and_claim`).
static CLAIM_NURSERY_FULL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Young bytes never handed to the region — main must be able to allocate through
/// PHASE 5 publication (and the serial fallback of a discarded batch) without
/// immediately colliding with a full nursery.
const MAIN_RESERVE: usize = 4 * 1024 * 1024;
/// Disjoint object-handle range per task (worker  transients).
///
/// SCALED BY BATCH SIZE, not a constant. 4096 was sized for the RING batch — 127 near-uniform
/// tasks — and it starves anything that allocates more heap handles per task. The datapack batch
/// is TWO tasks and hit the segment limit 1138 times, aborting a region that had otherwise begun
/// to run (its  went 55 -> 70771 once  stopped being denied outright).
///
/// A batch of two can afford a quarter-million slots each where a batch of 127 cannot, so the
/// budget is a TOTAL divided by the claimants, floored at the old constant so the ring batch is
/// unchanged and capped so a one-task batch cannot reserve the whole handle table.
const OBJ_SLOTS: usize = 4096;
const OBJ_SLOTS_TOTAL: usize = 1 << 19;
const OBJ_SLOTS_MAX: usize = 1 << 18;

/// Claimant count for the batch being packed, published by the claim phase (main, serial).
static CLAIMANTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Deferred-batch cap. The ring phase enqueues ~120 mutually independent tasks
/// before it observes any of them, so one drain covers the whole phase; the cap
/// only bounds how much of main's nursery a single region may reserve.
const MAX_BATCH: usize = 128;
/// Disjoint `Heap::field_box` slice per task. One boxed cell per closure the body
/// mints (the `$fi` functional-interface tag on each `invokedynamic` result).
/// Overflow aborts the task → resident fallback, so this is a budget, not a
/// soundness bound. MEASURED: at 256 the ring body exhausted it on 31 of 127 tasks
/// (`field-box slice exhausted`, heap.rs) — the ring lambda mints closures in a
/// loop, not once. 4096 clears the measured requirement with room; a slot is one
/// `Value`, so the whole batch's reservation is single-digit MB.
const FIELD_BOX_SLOTS: usize = 4096;
/// Same batch-size scaling as the object range, and for the same reason: 4096 was measured
/// against the RING body and the datapack body mints far more per task. Two tasks can have a
/// quarter-million; 127 keep the measured 4096.
const FIELD_BOX_TOTAL: usize = 1 << 19;
const FIELD_BOX_MAX: usize = 1 << 18;

pub(crate) struct GilfreeCfPlan {
    entry: usize,
    cells: Vec<i64>,
    ret: Option<jit::JT>,
    ret_desc: String,
    nbase: usize,
    nlen: usize,
    obase: u32,
    olen: u32,
    fbbase: u32,
    fblen: u32,
    field_txn: bool,
}

/// The resolved lambda-impl identity a plan needs. Shared by `plannable` (the
/// dry routing probe) and `prepare` (the real claim) so the two predicates can
/// never drift — a task admitted by one and refused by the other would route
/// into the resident runtime unproven, the documented boot-deadlock hazard.
pub(crate) struct ImplTarget {
    pub(crate) owner: String,
    pub(crate) name: String,
    pub(crate) desc: String,
    /// `Some(this)` for an INSTANCE impl (MethodHandle kind 5/7): capture 0 is
    /// the receiver and occupies local slot 0 of the compiled frame. `None` for
    /// a static impl (kind 6), whose captures start at slot 0.
    pub(crate) receiver: Option<Value>,
}

/// Per-(impl, predicate) rejection tally. This is the deliverable when the lane
/// refuses a task: WHICH admission predicate said no, and how often. Printed by
/// `report` at heap teardown whenever the lane is armed.
static REJECTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(String, &'static str), u64>>,
> = std::sync::OnceLock::new();

/// Worker-barrier deopt tallies, split by the surface that fired so the abort
/// rate is attributable rather than a single opaque number.
/// WHICH receiver class aborts a ring region (`RAVA_ABORT_CENSUS`, default OFF).
///
/// ⭐**Ledger row 329.** With `RAVA_ARENA_SYNTH` on, the boot's ring runs at **concurrency 1.00**
/// against **3.55 with it off** — the migration does not make each operation slower, it turns the
/// parallel lane OFF — and `ABORT_SHARED_FIELD` goes **2 → 37,974**. A count says a store to an
/// unowned receiver is doing it; only the RECEIVER'S CLASS says which store, and a global
/// `request_abort()` discards a whole 127-task batch, so a handful of classes can account for all
/// of it.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod abort_census {
    use std::sync::Mutex;

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_ABORT_CENSUS").is_some())
    }

    fn tally() -> &'static Mutex<std::collections::BTreeMap<String, u64>> {
        static M: std::sync::OnceLock<Mutex<std::collections::BTreeMap<String, u64>>> =
            std::sync::OnceLock::new();
        M.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
    }

    /// Name the receiver in whichever representation it arrives in, prefixed so the two are
    /// distinguishable — the question is precisely whether the aborting receivers became arena
    /// objects.
    pub(crate) fn note(vm: &crate::Vm, recv: &crate::value::Value) {
        if !enabled() {
            return;
        }
        let key = match recv {
            crate::value::Value::Obj(p) => format!(
                "arena {}",
                crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                    .as_deref()
                    .unwrap_or("?")
            ),
            crate::value::Value::Ref(h) => {
                format!("handle {}", vm.heap.class_of(*h).unwrap_or("?"))
            }
            other => format!("{other:?}"),
        };
        // HOW MANY owned ranges this worker had when it refused. `cow_owned` answers from
        // `OWNED_RANGES`, so "0 ranges" and "the address is outside N ranges" are completely
        // different defects — the first means the carve was never recorded for this thread, the
        // second means the object really was born elsewhere. A count alone cannot tell them
        // apart, and that is the distinction this whole hunt turns on.
        let nr = crate::parallel::OWNED_RANGES.with(|r| r.borrow().len());
        let key = format!("{key}  [owned_ranges={nr}]");
        // ⚠CAPPED, and deliberately: the tally below takes a GLOBAL MUTEX and builds a `String`
        // on every abort, in the ring's hot store path. Armed on the DEFAULT arm it took the
        // abort count **2 → 3,885** and the boot **56 → 78 s** — the instrument caused the very
        // aborts it was counting. That accident is also the cleanest causal evidence in this hunt
        // (more aborts ⇒ slower boot, on one binary and one arm), but it means the tally's
        // ABSOLUTE numbers are its own artefact. These first few lines are what the hunt actually
        // needs — an address against the ranges it was tested for — and they print at most 8
        // times per boot.
        {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
                let a = match recv {
                    crate::value::Value::Obj(p) => p.addr() as usize,
                    _ => 0,
                };
                let ranges = crate::parallel::OWNED_RANGES.with(|r| {
                    r.borrow()
                        .iter()
                        .map(|&(b, l)| (b, b + l))
                        .collect::<Vec<_>>()
                });
                eprintln!(
                    "[ABORT-ADDR] {key} addr={a:#x} pinned={} arena_obj={} ranges={}",
                    crate::nextgen::is_pinned(a as u64),
                    crate::nextgen::is_arena_obj_addr(a as u64),
                    ranges
                        .iter()
                        .map(|(b, e)| format!("{b:#x}..{e:#x}"))
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
        }
        let mut g = tally().lock().unwrap_or_else(|e| e.into_inner());
        *g.entry(key).or_insert(0) += 1;
    }

    pub(crate) fn report() -> String {
        if !enabled() {
            return String::new();
        }
        let g = tally().lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &u64)> = g.iter().collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        rows.iter()
            .take(12)
            .map(|(c, n)| format!("[ABORT-CENSUS] {n:>8}  {c}"))
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    }
}

pub(crate) static ABORT_SHARED_FIELD: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static ABORT_ARRAY_STORE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static ABORT_ARRAY_ALLOC: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// WARM-COMPILE (DEFAULT ON; `RAVA_NO_LANE_WARM_COMPILE` opts out): compile, on MAIN, the callees the
/// SERIAL PILOT reached through the general `jit_call` path, before the region opens.
///
/// WHY. The held-time attribution of a completing ring region is not spread across the twelve
/// locked callees at all — one symbol owns it:
///
/// ```text
/// BiomeSource.findBiomeHorizontal(...)  n=112  hold_ms=55028  max_ms=798
/// ```
///
/// 112 outer acquisitions holding 55 s of a 77 s batch, ~491 ms each; every other anchor is 0-1 ms.
/// A ring task IS essentially one call of it, and that call runs INTERPRETED inside the lock,
/// because a worker may not compile and the method is invoked about ONCE PER TASK — so it can
/// never reach a call-count compile threshold no matter how long the boot runs. Hotness counting
/// cannot see a method whose cost is one enormous invocation.
///
/// The pilot already executes one whole task serially on main, under the GIL, where compiling is
/// legal. Recording what it reaches on the general path and compiling exactly those is therefore
/// free of guesswork: it is the measured list, not a heuristic.
///
/// The recorded key is the CONCRETE RECEIVER class, not the declared owner — `force_compile`
/// needs the class whose bytecode will actually run, and a virtual site's declared owner is
/// usually abstract.
static WARM_TARGETS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<(String, String, String)>>,
> = std::sync::OnceLock::new();

/// Is the pilot running and recording? Only true between `warm_record_begin`/`_end`, so the cost
/// is confined to the one serial task and the flag is a relaxed load everywhere else.
static WARM_RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// EXACT abort attribution: `request_abort`'s `#[track_caller]` file:line → how many times it
/// tore this lane's region down.
///
/// WHY, given the site print already exists. That print has a prefix-then-periodic budget
/// (`n < 6 || n % 64 == 0`), so a boot whose first six aborts land in an early phase reports
/// NOTHING about the phase under investigation — measured exactly that on the first no-lock run:
/// six lines, all from the datapack phase, and the 127-task ring batch attributed to nobody.
/// A total per site has no budget and no ordering, so "which leaf tears the region down" is
/// answered by reading one line.
static ABORT_SITES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(&'static str, u32), u64>>,
> = std::sync::OnceLock::new();

/// RAII envelope for one lane task, in the S2 chunk-fill worker's exact order.
/// It is RAII and not a straight-line sequence for one reason: the lane's workers
/// are `thread::scope` threads that DIE at the join, and `frames::register` puts a
/// pointer to this thread's frame stack into a process-wide list the coordinator's
/// GC walks. A worker that left that registration behind — on a normal exit or a
/// panic — would hand main's next collection a pointer into a dead thread's TLS.
struct LaneWorker<'a> {
    vm: &'a crate::parallel::WorkerVmRef,
    prev_vm: *mut Vm,
    jit_ctx: Option<crate::heap::WorkerJitCtxGuard>,
}

impl<'a> LaneWorker<'a> {
    fn enter(plan: &GilfreeCfPlan, vm: &'a crate::parallel::WorkerVmRef) -> Self {
        crate::frames::register();
        // "Not the compiling main thread": the TLS latch that blocks a worker from
        // resolving/compiling/inserting into shared engine state. Thread-lifetime,
        // never cleared — the same contract `WorkerContext::enter` documents.
        crate::gil::set_worker();
        crate::gilfree_cf::stats::LANE.with(|c| {
            let mut s = c.get();
            s.in_lane = true;
            c.set(s)
        });
        crate::gilfree_cf::stats::LANE_ABORT.with(|c| c.set(false));
        lane_pending_reset();
        crate::parallel::set_gilfree(true);
        crate::parallel::enter_region();
        crate::nextgen::worker_tlab_view(plan.nbase, plan.nlen);
        crate::parallel::arena_setup(plan.obase, plan.olen);
        crate::parallel::field_box_setup(plan.fbbase, plan.fblen);
        // Publish the coordinator Vm as this thread's CURRENT_VM. Without it every
        // callback's `vm_ref_worker_sanctioned()` reads a NULL slot and returns a
        // silent 0 — the body would compute GARBAGE and report success instead of
        // deopting. The worker variant deliberately does NOT stamp the shared
        // `Vm::jit_ctx_slot` (that word is the MAIN thread's deep-IC prologue base,
        // and N workers would be racing one shared word).
        let prev_vm = vm.install_current_worker();
        if plan.field_txn && !crate::gilfree_cf::field_txn::begin() {
            crate::parallel::request_abort();
        }
        let jit_ctx = Some(vm.install_worker_jit_ctx());
        // ARM the compiled write barrier on THIS worker's private JitCtx: armed,
        // every compiled putfield takes its `brif(is_armed, cow_blk, inl_blk)` cow
        // arm into `rava_putfield`, where the ownership barrier lives — no inline
        // store can escape ungated.
        if rings() {
            crate::heap::set_active_cow_armed(true);
        }
        crate::lane_probe::enter_worker();
        LaneWorker {
            vm,
            prev_vm,
            jit_ctx,
        }
    }
}

impl Drop for LaneWorker<'_> {
    fn drop(&mut self) {
        if rings() {
            crate::heap::set_active_cow_armed(false);
        }
        crate::lane_probe::leave_worker();
        let _ = crate::gilfree_cf::field_txn::take();
        crate::gilfree_cf::stats::LANE.with(|c| {
            let mut s = c.get();
            s.in_lane = false;
            c.set(s)
        });
        self.jit_ctx.take();
        self.vm.restore_current_worker(self.prev_vm);
        crate::parallel::field_box_clear();
        crate::nextgen::worker_tlab_clear();
        crate::parallel::leave_region();
        crate::parallel::set_gilfree(false);
        crate::frames::unregister();
    }
}

/// One deferred lane task: the guest supplier and the pending future its result
/// completes. Both are GC roots for as long as the batch holds them.
struct LaneTask {
    task: Value,
    future: Value,
    /// SAM ARGUMENTS the deferred body is applied to, empty for a `Supplier`.
    ///
    /// The lane could only ever carry a Supplier, and the reason was arithmetic rather than
    /// policy: `pack_and_claim` requires `captured.len() == meta.params.len()`, and an impl
    /// method's descriptor already counts the SAM parameters after the captures — so a
    /// one-argument body (`Function.apply`, `Consumer.accept`) failed the arity check and
    /// was refused before any admission rule ran. That is why `[PC-DEFER]` reads
    /// `offered=7 | DEFERRED=7`: the reload's `*Async` chaining bodies are all
    /// one-argument, and the only seam that offers anything is `supplyAsync`.
    ///
    /// Rooted by BOTH collectors alongside `task`/`future` (`visit_roots`,
    /// `gather_roots`) — an argument swept between `defer` and `drain` would be a silent
    /// wrong answer of exactly the kind those two walks exist to prevent.
    args: Vec<Value>,
}

/// One batch entry's claimed plan and its region outcome. `Mutex` (not `unsafe`)
/// because the region's cost unit is a whole compiled task body — an uncontended
/// lock per task is free at that scale.
struct LaneSlot {
    plan: Option<GilfreeCfPlan>,
    /// `(result bits, aborted, diagnostic compiled-callback count)`.
    out: Option<(i64, bool, u64)>,
    txn: field_txn::FieldTxn,
}

mod batch;
mod completion;
pub(crate) mod field_txn;
pub(crate) use batch::*;
pub(crate) mod plan;
pub(crate) use plan::*;
mod stats;
pub(crate) use stats::*;
mod rings;
pub(crate) use rings::*;
mod aborts;
pub(crate) use aborts::*;
mod warm;
pub(crate) use warm::*;
mod counters;
pub(crate) use counters::*;
pub(crate) mod warmup;
pub(crate) use warmup::*;
