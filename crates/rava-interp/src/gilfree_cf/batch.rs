//! Running a batch of compiled work off the GIL.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

/// THE COORDINATOR'S OWN OVERHEAD — kill switch, DEFAULT ON (`RAVA_NO_FAST_LANE=1`, or the
/// blanket `RAVA_NO_EMIT_MORE=1`, restores the previous shape).
///
/// The lane is worth ~13% of the boot, but a product boot forms only FOUR batches of ONE TO THREE
/// tasks, so everything the coordinator spends between `drain` and the join is paid against a
/// region of one or two bodies. This gate covers the one change that alters an executed path
/// rather than merely removing redundancy: WHEN the frame's arguments are read out of the batch
/// (PHASE 1 vs PHASE 2). The lock elisions below need no arm — they are the same reads of the
/// same bytes, with the borrow checker doing what an uncontended mutex was being asked to do.
fn fast_lane() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::round7() && std::env::var_os("RAVA_NO_FAST_LANE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**A PARK ONLY PAYS IF SOMETHING RUNS BESIDE IT** — kill switch, DEFAULT ON
/// (`RAVA_NO_FAST_PARK=1`, or the blanket `RAVA_NO_EMIT_MORE=1`, opens the region for every
/// shape again, exactly as before).
///
/// PHASE 3's own comment states the contract this reads: "Main parks in the scope join; it holds
/// the GIL throughout, so no OTHER resident thread can run guest code either." So during a region
/// the ONLY guest work in the process is the workers' — and the coordinator's park is pure loss
/// whenever the region cannot produce more than one of them, or cannot produce a result that
/// PHASE 4 is allowed to keep. Both of those are decidable BEFORE the park, from counts the
/// coordinator already holds, and this gate is the two tests (see GATE 1 and GATE 2 in
/// `run_batch`). Neither touches the isolation contract or the keep/discard vote: GATE 1 moves a
/// task onto the path PHASE 0 and PHASE 5 already run it on, and GATE 2 skips a region whose vote
/// `keep = bits.iter().all(is_some)` is ALREADY `false` by construction.
fn fast_park() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_PARK").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_PILOT_N`, read ONCE.
///
/// WAS: a `std::env::var` — a process environment lookup plus the `String` it allocates to hold
/// the answer — on EVERY drain, to re-derive a number that was fixed before `main` ran. IS: one
/// relaxed `OnceLock` load; the lookup and the allocation happen once per process. The parse is
/// deliberately byte-identical to the old inline one (no `trim`), so a malformed value still
/// falls back to 1 exactly as before.
fn pilot_n() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_PILOT_N").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1)
    })
}

/// Execute the compiled body on THIS worker thread WITHOUT the GIL. Returns
/// result bits, abort status, callback count, and its ordered field journal.
pub(crate) fn run(
    plan: &mut GilfreeCfPlan,
    vmref: &crate::parallel::WorkerVmRef,
) -> (i64, bool, u64, crate::gilfree_cf::field_txn::FieldTxn) {
    let t0 = now_ms();
    let worker = LaneWorker::enter(plan, vmref);
    // The compiled ABI's 2nd parameter IS the JitCtx pointer: array descriptor
    // spine, static-cell base, the fault latch, and `cow_armed` all hang off it.
    // Passing 0 (the stage-3 code) makes the very first field/array access
    // dereference near-NULL. Hand over the ACTIVE — worker-private — block.
    let ctx_ptr = worker.vm.active_jit_ctx_ptr();
    crate::gilfree_cf::stats::LANE.with(|c| { let mut s = c.get(); s.cb_n = 0; c.set(s) });
    let bits = unsafe { jit::call_raw(plan.entry as *const u8, &mut plan.cells, ctx_ptr) };
    let cbs = crate::gilfree_cf::stats::LANE.with(|c| c.get().cb_n);
    // A guest throw latches the TLS pending slot and the compiled root runs to
    // its return with garbage bits. There is no worker-side unwind here, so
    // treat it as a deopt: the serial re-run raises the real exception and the
    // future completes exceptionally, per the CompletableFuture contract.
    let threw = crate::execution_state::pending_is_set();
    if threw {
        crate::execution_state::pending_clear();
    }
    let latched = crate::gilfree_cf::stats::LANE_ABORT.with(|c| c.get());
    // SIBLING POISONING — this task's result is only trustworthy if NO task in the
    // region aborted. `rava_dispatch_miss` short-circuits on the PROCESS-WIDE abort
    // flag (jit.rs, "Abort visibility") and hands the caller a discarded 0, so the
    // instant any sibling aborts every other live body unwinds through garbage
    // continuations and returns early. Its own per-task `LANE_ABORT` never fires, so
    // without this term such a task reports `aborted=false` having computed NOTHING —
    // MEASURED: 100 of 127 ring tasks "completed" in 1 ms firing 3 callbacks each,
    // against 804 ms / 5.6M callbacks for the identical body run serially. The
    // keep-all-or-discard-all vote happened to save us (the aborting task's own slot
    // votes discard), but the lane must not depend on that coincidence to avoid
    // publishing garbage — nor report a completion count it did not earn.
    // A BOX-TABLE INDEX IS THREAD-LOCAL AND MUST NOT CROSS THE LANE BOUNDARY.
    //
    // `is_box_cell`'s own contract says such a cell "must be re-homed before the table
    // unwinds", and the lane never re-homed one: the worker boxed its result into ITS
    // `jit_box_table` and returned the INDEX, then main resolved that index in its OWN
    // table and read whatever was there. Measured with `bench/SupplyMini2.java`:
    //
    //     CompletableFuture.supplyAsync(() -> 5).join()   -> 0     (bits = 0x20000000000)
    //     ... (() -> 7L)  (() -> 8.5)  (Klass::boxedMethod)   all -> 0
    //     ... (() -> "s")                                          -> "s"   (interned)
    //     ... (() -> true)                                         -> true  (singleton)
    //
    // Every boxed NUMBER came back zero and the two that survived did so only because
    // they were never worker-allocated. `bits = BOX_TAG | 0` is exactly index 0 of a
    // table main had never filled.
    //
    // Treated as an ABORT rather than published: the coordinator re-runs an aborted task
    // serially, which is already the lane's answer for a result it cannot trust. That
    // costs those tasks their parallelism and returns the right value — re-homing on the
    // worker would keep both, and is the proper fix once it can be measured.
    // FLAG-GATED so its cost can be priced by the one design that cannot be confounded
    // (ledger row 269): same binary, both arms, only the executed path differs.
    // `RAVA_NO_LANE_BOX_GUARD=1` restores the old behaviour — which returns WRONG VALUES,
    // so it is a measurement handle and never a tuning knob.
    static GUARD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let guard_on = *GUARD.get_or_init(|| std::env::var_os("RAVA_NO_LANE_BOX_GUARD").is_none());
    let boxed_result = guard_on && jit::is_box_cell(bits);
    if boxed_result {
        crate::gilfree_cf::counters::note_boxed_result();
    }
    let txn = match (plan.field_txn, crate::gilfree_cf::field_txn::take()) {
        (true, Some(txn)) => txn,
        (true, None) | (false, Some(_)) => {
            crate::parallel::request_abort();
            crate::gilfree_cf::field_txn::FieldTxn::default()
        }
        (false, None) => crate::gilfree_cf::field_txn::FieldTxn::default(),
    };
    let poisoned = crate::parallel::abort_requested();
    let aborted = latched || threw || poisoned || boxed_result;
    if rings() {
        // ATTRIBUTION. `aborted` alone is not actionable: a GUEST THROW, a latched
        // region abort and a sibling's poisoning are different frontiers with
        // different fixes, and the S09 stubs / lane guards only account for the
        // second. Splitting them is what turns "every task aborted" into a named
        // next cause.
        trace_run(t0, aborted, threw, latched, poisoned, cbs, bits);
    }
    drop(worker);
    (bits, aborted, cbs, txn)
}

/// Decode the result bits into the future's completion Value — UNDER THE GIL.
pub(crate) fn publish(plan: &GilfreeCfPlan, vm: &Vm, bits: i64) -> Value {
    match plan.ret {
        None => Value::Null,
        Some(jit::JT::Ref) => jit::unbox_ref(vm, bits, &plan.ret_desc),
        Some(_) => jit::unpack_ret(plan.ret, bits).unwrap_or(Value::Null),
    }
}

// The coordinator owns the VM throughout reservation, worker join and retirement.
// Guest execution resumes only once every worker has joined and the nursery and
// handle ranges have been retained or discarded by JoinedRegion.

pub(crate) fn run_batch(vm: &mut Vm) {
    let n = vm.lane.pending.len();
    // PHASE 0 — PILOT WARM (see `pilot_warm`). The batch's FIRST task runs serially,
    // under the GIL, exactly as the abort path's serial retry does — that is what
    // fills the frozen direct cache and the baked direct-mono site caches that every
    // worker arm can only READ. Its future is completed right here rather than in
    // PHASE 5: holding the result `Value` across PHASE 1 would expose it to that
    // phase's class loading and collections. The pilot takes NO slot, so PHASE 4's
    // keep-all-or-discard-all vote covers only work that actually ran off-GIL.
    // PILOT COUNT is settable (`RAVA_PILOT_N`, default 1) so "does warming work at all" is a
    // measurable question rather than an argument. If the pilot really fills the frozen direct
    // cache that every worker arm can only read, then running MORE tasks serially must reduce
    // the locked-callee tally; if three pilots change nothing, warming is not the mechanism and
    // the eight `unknown` sites are unknown for some other reason. Capped at n-1 so at least one
    // task still runs off-GIL.
    let mut pilot = if pilot_warm() && n > 1 { pilot_n().clamp(1, n - 1) } else { 0 };
    // ⭐**GATE 1 — A REGION OF ONE OVERLAPS NOTHING, SO ITS PARK IS THE WHOLE TASK.**
    //
    // `workers = worker_cap().min(claimed).max(1)`, and `claimed <= par = n - pilot`. So when
    // `par` is 1 — or when the pool is one thread wide — the region is exactly ONE worker running
    // exactly ONE body while the coordinator sleeps in the scope join holding the GIL. Wall is
    // `t(task)` either way; what the region adds on top of that identical wall is a 256 MiB-stack
    // `CreateThread`, the park/unpark round trip, the spill pool, the parallel-array reserve, the
    // worker's armed isolation barriers (COW shadow branch, private view, abort latches) — and the
    // abort risk, which on this shape is not a risk of losing parallelism (there is none) but of
    // paying for the body TWICE, once off-GIL into the discard and once again in PHASE 5.
    //
    // ⭐AND THE BOOT'S BATCH SHAPES ARE MOSTLY THIS ONE. `warmup::drain`'s census exists because
    // row 432 guessed at the distribution and lost two discriminators to the guess; the shapes it
    // and `fast_lane` record are batches of ONE TO THREE tasks, and with the default pilot of 1
    // that is `par` of 1, 1 and 2 — two of the three open a region that cannot overlap anything.
    //
    // The cure is NOT a new path: the pilot loop below IS the serial run, "an ORDINARY serial
    // `vm.apply` under the GIL, identical to the abort path's serial retry", and it completes each
    // future in place. So the fix is one clamp — let the pilot take the whole batch — after which
    // `par == 0` makes PHASES 1-5 empty loops and the coordinator never parks at all.
    //
    // ⚠**THIS DOES NOT DISABLE THE LANE, AND THE DISTINCTION IS THE WHOLE POINT.** The lane's
    // measured ~13% is bought by DEFERRAL — the future stays pending and main runs on from the
    // `supplyAsync` seam to the observation point — and every bit of that has already happened by
    // the time `drain` calls us. What is decided here is only WHERE the deferred body runs once
    // main has arrived at the point it must block, and on this shape both answers block main for
    // the same span.
    //
    // ⛔The `n - 1` cap this overrides was written so "at least one task still runs off-GIL". That
    // is the right floor for `RAVA_PILOT_N`, which is a warming knob and must not be able to
    // starve the region by hand — it is not a floor for a batch that has nothing to run off-GIL
    // beside anything, so the clamp above is left exactly as it was and this is a separate test.
    if fast_park() && (n - pilot <= 1 || worker_cap() < 2) {
        pilot = n;
    }
    let pilot = pilot;
    // Callback counts are diagnostics, never a condition for retaining results.
    let mut pilot_cbs = 0u64;
    for i in 0..pilot {
        let task = vm.lane.pending[i].task.clone();
        let future = vm.lane.pending[i].future.clone();
        // Same argument contract as the serial retry in PHASE 5: the pilot IS a serial run
        // of a real batch task, so it must be applied to the arguments that task carries.
        let pilot_args = vm.lane.pending[i].args.clone();
        // Record the serial task's callback count for the diagnostic ledger.
        let t0 = now_ms();
        let cb0 = jit_cb_total();
        let arr0 = jit_cb_kind(5);
        let outcome = {
            let _census = crate::pilot_census::enabled()
                .then(|| crate::taskgraph_probe::owner_of(vm, &task))
                .and_then(crate::pilot_census::task_scope);
            if warm_compile_enabled() {
                warm_record_begin();
            }
            let r = Ok(vm.apply(&task, pilot_args));
            warm_record_end();
            r
        };
        let _ = &outcome;
        pilot_cbs = jit_cb_total().saturating_sub(cb0);
        if rings() {
            eprintln!(
                "[GILFREE-RINGS] pilot#{i} SERIAL ms={} cbs={pilot_cbs} newarray={}",
                now_ms() - t0,
                jit_cb_kind(5).saturating_sub(arr0),
            );
        }
        crate::reflect::complete_queued_task(vm, Some(future), None, outcome);
    }
    // Compile what the pilot could only interpret. Still on main, still under the GIL, still
    // before anything is claimed — so a compile here costs one serial pause and buys every
    // worker a direct call where it would otherwise take the lock and interpret a subtree.
    if warm_compile_enabled() && pilot > 0 {
        let targets = warm_targets_drain();
        let (mut ok, mut n) = (0usize, 0usize);
        let t0 = now_ms();
        for (cls, name, desc) in targets {
            n += 1;
            if vm.force_compile(&cls, &name, &desc) {
                ok += 1;
            }
        }
        if rings() {
            eprintln!(
                "[GILFREE-RINGS] warm-compile {ok}/{n} general-path callees in {}ms | {}",
                now_ms() - t0,
                // WHICH ARM refused the ones that did not compile. Each uncompiled callee is a
                // live `InternalError: direct miss` the moment a worker reaches it, and that is
                // three of four aborting runs — so "9 of 25" without a reason is the number that
                // matters least.
                crate::Vm::force_why_report()
            );
        }
    }
    // PHASE 1 — RESOLUTION. May load classes, run `<clinit>`, compile: guest code,
    // therefore collections. It claims nothing, so a collection here costs only
    // work, never correctness. The task is re-read from the (forwarded) batch each
    // iteration. Slot `k` here is batch task `pilot + k`.
    let par = n - pilot;
    let mut metas: Vec<Option<PlanMeta>> = Vec::with_capacity(par);
    // ⭐**THE ARGUMENTS ARE NOT READ HERE, AND THE REASON IS THE ONE `PlanMeta::handle` ALREADY
    // GIVES FOR THE LAMBDA:** "re-read at pack time so the packed cells are taken from the
    // CURRENT (post-collection) object". This loop loads classes and runs `<clinit>` — that is
    // its whole point — so it COLLECTS, and a `Vec<Vec<Value>>` of clones assembled here is
    // rooted by NEITHER collector: `LaneBatch::visit_roots`/`gather_roots` walk the batch, not a
    // coordinator local. So iteration k's `<clinit>` could move (moving arena) what iteration 0's
    // copy names, and `pack_and_claim` would then pack a forwarded-away address into the
    // worker's frame. The handle was given this discipline; the arguments were left out of it.
    // Leaving them in the batch — which IS rooted, across arbitrarily many collections — and
    // cloning them in PHASE 2, where the phase contract is "nothing runs guest code and nothing
    // collects", is both provably fresh and one whole `Vec<Vec<Value>>` (plus its per-task
    // element clones and their `Value` refcount pairs) cheaper per batch.
    // Only the ARITY is a PHASE-1 input, and a length needs no clone.
    let mut argsv: Vec<Vec<Value>> = Vec::new();
    for i in pilot..n {
        let task = vm.lane.pending[i].task.clone();
        if fast_lane() {
            let arity = vm.lane.pending[i].args.len();
            metas.push(plan_meta(vm, &task, arity));
        } else {
            let args = vm.lane.pending[i].args.clone();
            metas.push(plan_meta(vm, &task, args.len()));
            argsv.push(args);
        }
    }
    // PHASE 2 — CLAIM. From here to the join nothing runs guest code and nothing
    // collects, so the packed captures and the carved windows are all still valid
    // when the workers read them. `young_top` is the pre-batch nursery top, kept for
    // the abort retreat (G1.4).
    let (ybase, young_top, ycap) = crate::nextgen::young_extent();
    // THE ALLOCATION BUDGET, decided once for the whole region. Young headroom minus
    // main's reserve is what the region may consume; `pool_pct` of that is held back
    // as the SHARED OVERFLOW POOL and the rest is pre-carved into per-task slices.
    let usable = ycap.saturating_sub(young_top).saturating_sub(MAIN_RESERVE);
    let mut budget = usable - usable * pool_pct() / 100;
    // Tell `obj_slots` how many tasks are about to share the handle budget.
    CLAIMANTS.store(metas.iter().filter(|m| m.is_some()).count(), std::sync::atomic::Ordering::Relaxed);
    // WAS: `slots.iter().filter(|s| lock(s).plan.is_some()).count()` — a mutex acquire and
    // release per slot, to re-read a bool main had written microseconds earlier, at a point where
    // not one worker thread exists yet to contend it. IS: counted as the slots are built, which
    // is where the answer already was.
    let mut claimed = 0usize;
    let mut slots: Vec<std::sync::Mutex<LaneSlot>> = Vec::with_capacity(par);
    for (k, meta) in metas.into_iter().enumerate() {
        // The arguments the compiled frame is packed from, taken HERE (see PHASE 1's note): from
        // this line to the join nothing runs guest code and nothing collects, so this clone
        // cannot be stale by the time `pack_and_claim` reads it.
        let args = if fast_lane() {
            vm.lane.pending[pilot + k].args.clone()
        } else {
            std::mem::take(&mut argsv[k])
        };
        let plan = meta.and_then(|meta| pack_and_claim(vm, meta, &mut budget, &args));
        claimed += usize::from(plan.is_some());
        slots.push(std::sync::Mutex::new(LaneSlot {
            plan,
            out: None,
            txn: crate::gilfree_cf::field_txn::FieldTxn::default(),
        }));
    }
    // ⭐**GATE 2 — A REGION WHOSE VOTE IS ALREADY `false` MUST NOT BE PARKED ON.**
    //
    // PHASE 4 keeps a region only if EVERY slot voted: `keep = bits.iter().all(Option::is_some)`,
    // and `bits[k]` is `Some` only for a slot that produced an `out`. A slot that failed to claim
    // carries `plan: None`, the worker loop's `let Some(mut plan) = ... else { continue }` skips
    // it, its `out` stays `None`, and `bits[k]` is `None`. So `claimed < par` DECIDES the vote
    // before a single worker exists: the region is going to be discarded whatever the workers
    // compute, PHASE 5 is going to re-run all `par` tasks serially, and the only thing the region
    // can still do is spend a thread spawn plus the coordinator's park plus one full off-GIL
    // execution of a body whose result is guaranteed to be thrown away. On a boot that runs at
    // ~1.07 cores that discarded execution is not idle time — it is real CPU spent to produce
    // nothing, and main is asleep for the whole of it.
    //
    // Nothing about the vote changes: `keep` still reads `false`, by exactly the same term, from
    // exactly the same absent `out`. The keep/discard machinery downstream — the young-top retreat
    // to `young_top`, `release_obj_range` per plan, the serial re-run with the task's real
    // arguments — is the code that was going to run anyway; it now runs without the detour.
    //
    // Combined with GATE 1 the invariant is: with the gate on, a region opens only when
    // `claimed == par >= 2`, i.e. only when at least two bodies will run concurrently AND the
    // result is still eligible to be kept. Off, the shape is bit-for-bit the previous one.
    let open_region = claimed > 0 && (!fast_park() || claimed == par);
    // THE SPILL POOL (part 1). `parallel::spill_setup` had exactly ONE caller — the S2
    // chunk-fill fork — so the rings lane's overflow arm found `SPILL_BASE == 0`, could
    // not refill, and returned to a compiled retry loop with the view still full. That
    // is the wall. Mirror the S2 discipline (open at region open, `spill_clear` at
    // teardown) with ONE deliberate difference: S2 points the pool at the dedicated
    // 64 MB low region (`nextgen::spill_region_base`) because its results are f64s that
    // never leave the region, whereas a RING task's result ESCAPES into a
    // CompletableFuture. A published object sitting in process-lifetime memory that the
    // NEXT region's cursor reset hands out again is a use-after-free with extra steps.
    // So this pool is carved from MAIN'S OWN YOUNG: spill objects are ordinary young
    // objects the collector already owns, the discard path's `young_set_top` retreat
    // covers them, and publication is safe.
    let after_slices = crate::nextgen::young_extent().1;
    let pool_base = ybase + after_slices;
    // Keyed on `open_region`, not on `claimed`: a pool advanced for a region that never opens is
    // young headroom the batch takes off main and hands straight back at the discard retreat.
    let pool_len = if open_region {
        let want = ycap.saturating_sub(after_slices).saturating_sub(MAIN_RESERVE);
        crate::nextgen::young_advance_top(want);
        want
    } else {
        0
    };
    let mut pool_claims = 0u64;
    let mut pool_used = 0usize;
    let mut region_aborted = false;

    // PHASE 3 — THE REGION. Main parks in the scope join; it holds the GIL
    // throughout, so no OTHER resident thread can run guest code either — which is
    // precisely why GATE 1 and GATE 2 above refuse to open one that cannot overlap
    // two bodies or cannot be kept: on those shapes the park buys nothing and the
    // whole span is a thread waiting for a thread.
    if open_region {
        // LANE PROBE (RAVA_LANE_PROBE=<ms>): start the periodic worker-side dump
        // BEFORE the region opens. `report()` fires at heap teardown, which a
        // region that never finishes never reaches — the watchdog is the only way
        // to read a grinding task's classification while it grinds.
        crate::lane_probe::start_watchdog();
        crate::parallel::clear_abort();
        // Refill granularity = one more starting view. A worker that outgrows its slice
        // claims another slice-sized chunk and carries on; only a fully drained pool
        // starves it, and THAT now takes the compiled bounded exit instead of spinning.
        crate::parallel::spill_setup(pool_base, pool_len, nursery_slice());
        // PARALLEL ARRAY REGION. The heap's array Vecs are not concurrent-safe to
        // push, so a worker `newarray`/`anewarray` must claim a PRE-RESERVED slot.
        // Without an open region `rava_newarray` aborts on the task's first array
        // allocation and hands the compiled body a 0 — which it promptly dereferences
        // into `NullPointerException: getfield`. That was the measured cause of the
        // ring lane's first-task abort, and (through the process-wide abort flag that
        // blinds `rava_dispatch_miss`) of every sibling truncating to a no-op.
        // Exhaustion is SAFE: `worker_newarray` returns None and the task aborts to
        // the serial retry, exactly like the S2 fork's over-reserve discipline.
        vm.heap.parallel_array_reserve(claimed.saturating_mul(arr_slots_per_task()).max(64));
        let workers = worker_cap().min(claimed).max(1);
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let slots_ref = &slots;
        let cursor_ref = &cursor;
        // Handing the coordinator's exclusive borrow to the region: main does not
        // touch `vm` again until the join returns.
        let vmref = crate::parallel::WorkerVmRef::new(vm);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                // A ring worker EXECUTES GUEST CODE, so it needs a guest-sized stack for the same
                // reason the engine thread is spawned with 1 GiB: deep Java call trees map onto
                // deep native recursion. A scoped `spawn` inherits the platform default (1 MiB on
                // Windows), which is nowhere near enough — measured, a worker running the general
                // path overflowed its stack in block C before the ring was even reached
                // ("thread has overflowed its stack", no panic message, process gone). Under the
                // resolve lock that was invisible, because the lock kept the deep subtree on the
                // main thread's stack; the moment a worker runs one itself it needs the room.
                // 256 MiB is the same order as the task runtime's 64 MiB workers with headroom
                // for worldgen's recursion depth, and it is reserved address space, not committed
                // memory, so the cost of the headroom is nil.
                std::thread::Builder::new()
                    .name("rava-ring-worker".into())
                    .stack_size(256 * 1024 * 1024)
                    .spawn_scoped(scope, move || loop {
                    let k = cursor_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if k >= slots_ref.len() {
                        break;
                    }
                    let Some(mut plan) = lock(&slots_ref[k]).plan.take() else { continue };
                    // A panicking task is an abort like any other: the whole region
                    // is discarded and every task re-runs serially. Catching it here
                    // keeps the panic from unwinding out of the scope and taking the
                    // coordinator's park with it.
                    let (bits, aborted, cbs, txn) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run(&mut plan, &vmref)
                        }))
                        .unwrap_or((
                            0,
                            true,
                            0,
                            crate::gilfree_cf::field_txn::FieldTxn::default(),
                        ));
                    let mut slot = lock(&slots_ref[k]);
                    slot.plan = Some(plan);
                    slot.out = Some((bits, aborted, cbs));
                    slot.txn = txn;
                    // This thread's ThreadLocal values belong to the TASK, not the thread: the
                    // worker loop takes another task next and Java's contract is per-thread, so
                    // carrying entries across would leak one task's scratch state into the next.
                    // It also bounds the table's lifetime to a window in which no collection can
                    // run, which is what lets the entries be unrooted (see crate::tl_worker).
                    crate::tl_worker::clear();
                    crate::worker_direct::clear();
                })
                    // A worker that cannot be spawned is a region that cannot run: request the
                    // abort so the coordinator recomputes serially, exactly as any other
                    // worker-side failure does, rather than silently running short-handed.
                    .unwrap_or_else(|e| {
                        eprintln!("[GILFREE-RINGS] worker spawn failed ({e}) — aborting the region");
                        crate::parallel::request_abort();
                        panic!("ring worker spawn failed: {e}");
                    });
            }
        });
        // Close the array region (trims unclaimed slots) whether or not we aborted —
        // the fork's discipline: an aborted batch's arrays are unreferenced garbage
        // the collector reclaims, but the region itself must not stay open.
        vm.heap.parallel_array_commit();
        // Capture the authoritative region outcome after every worker joins.
        // A sibling may abort after another task has already returned successfully.
        region_aborted = crate::parallel::abort_requested();
        crate::parallel::clear_abort();
        pool_claims = crate::parallel::spill_stats().0;
        pool_used = pool_len.saturating_sub(crate::parallel::spill_stats().1);
        crate::parallel::spill_clear(); // region-scoped, exactly as the S2 fork

    }

    let slots = slots.into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect();
    let region = super::completion::JoinedRegion::new(slots, region_aborted);
    region.report(n, pilot, open_region, claimed, pool_len, pool_used, pool_claims, pilot_cbs);
    region.finish(vm, pilot, young_top, after_slices + pool_used);
}
