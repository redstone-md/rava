//! CompletableFuture and the callbacks it runs.

use crate::reflect::*;

/// Compute the downstream value for one callback against the completed `src` and
/// complete `down` (or, for chaining kinds, wire `down` to a further source).

/// ⭐**THE OTHER INPUT OF A `combine` MUST BE SETTLED, NOT ASSUMED** (row 422).
///
/// `thenCombine` read the other future's `$result` only when it was already done and passed
/// **`Value::Null`** otherwise. That was invisible for as long as every future was born complete
/// ("we run everything inline, so the returned future is already complete"). The moment a body
/// genuinely ran on a worker, MC's `ServerFunctionLibrary.reload` combined a done future with a
/// running one, got `Pair::of(tags, null)`, and `lambda$reload$8` took
/// `NullPointerException: Cannot invoke "java.util.Map.forEach(...)"`.
///
/// ⚠**Waits only for a future the task runtime is TRACKING** — one something is contracted to
/// complete. An untracked pending future keeps the old `Null`, because blocking on a future that
/// nobody will complete is a hang, and this arc has already produced five of those.
fn settled_other(vm: &mut Vm, other: &Value) -> Value {
    let Value::Ref(oh) = other else { return Value::Null };
    let oh = *oh;
    if !matches!(vm.heap.get_field(oh, "$done"), Value::Int(1)) && vm.task_runtime.tracks_future(oh)
    {
        vm.wait_for_tracked_future(oh);
    }
    if matches!(vm.heap.get_field(oh, "$done"), Value::Int(1)) {
        vm.heap.get_field(oh, "$result")
    } else {
        Value::Null
    }
}

pub(crate) fn cf_run_cb(vm: &mut Vm, src: u32, kind: &str, f: &Value, other: &Value, down: u32) -> Result<(), VmError> {
    let result = vm.heap.get_field(src, "$result");
    let error = match vm.heap.get_field(src, "$error") { Value::Null => None, e => Some(e) };
    // allOf countdown: complete the shared `down` only when the last input lands.
    if kind == "allof" {
        if let Some(e) = &error {
            if matches!(vm.heap.get_field(down, "$err0"), Value::Null) { vm.heap.put_field(down, "$err0", e.clone()); }
        }
        let p = vm.heap.get_field(down, "$pending").as_int().unwrap_or(1) - 1;
        vm.heap.put_field(down, "$pending", Value::Int(p));
        if p <= 0 {
            let err0 = match vm.heap.get_field(down, "$err0") { Value::Null => None, e => Some(e) };
            cf_complete(vm, down, Value::Null, err0)?;
        }
        return Ok(());
    }
    if kind == "anyof" {
        cf_complete(vm, down, result, error)?;
        return Ok(());
    }
    // GAP-A01/A05 probe: continuation actions (thenApply/Accept/Run/compose/...)
    // always run INLINE today — there is no resident routing for them. `mirror`
    // moves values without guest code, so it is excluded.
    let _probe = if kind != "mirror" {
        taskgraph_probe::inline_scope(vm, InlineSeam::CfCallback, f)
    } else {
        None
    };
    // ⭐**OFFLOAD-DRILL for continuations** (`RAVA_OFFLOAD_DRILL`, off by default).
    // `futures.rs:336` drills only the `supplyAsync` seam; a `thenApply`/`compose`
    // body never reaches that arm and therefore never printed its rejection chain.
    // `CF.callback=24 / wall=3.8s` proves the hot continuation is a single
    // `ReloadableServerResources::lambda$loadResources$1` continuation — drill it
    // here at its ENTRY, for free when the gate is off and once per distinct
    // root when it is on (deduped inside `offload_drill_print`).
    #[cfg(feature = "next-gen-gc")]
    if crate::Vm::offload_drill_gate() {
        let site: Option<std::sync::Arc<crate::heap::LambdaSite>> = match f {
            crate::Value::Ref(h) => vm.heap.lambda_site_of(*h).clone(),
            #[cfg(feature = "next-gen-gc")]
            crate::Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => {
                crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(*p))
            }
            _ => None,
        };
        if let Some(s) = site {
            vm.offload_drill_print(&s.owner, &s.name, &s.descriptor);
        }
    }
    // Wall #15: a callback ACTION that throws must complete `down` EXCEPTIONALLY (the
    // CompletableFuture contract for thenApply/Accept/Run/Compose/Combine/handle/
    // whenComplete/exceptionally), NOT propagate. The old `vm.apply(..)?` propagated, so
    // a throwing reload callback (e.g. WorldLoader.load's whenComplete on the reload
    // future) orphaned the chain — nothing ever completed `Util.blockUntilDone`'s future
    // and it hot-spun forever (the worldgen "softlock"). Capture the throw → `down`
    // exceptional. The `compose` "deferred" path (wire `down` to a further source) must
    // still skip the completion, so the computation reports it via `Out::Deferred`.
    #[cfg(feature = "next-gen-gc")]
    {
        enum Out { Settle(Value, Option<Value>), Deferred }
        let computed: Result<Out, VmError> = (|| {
            Ok(match kind {
                "apply" => match &error { Some(e) => Out::Settle(Value::Null, Some(e.clone())),
                    None => Out::Settle(vm.apply(f, vec![result.clone()])?.unwrap_or(Value::Null), None) },
                "accept" => match &error { Some(e) => Out::Settle(Value::Null, Some(e.clone())),
                    None => { vm.apply(f, vec![result.clone()])?; Out::Settle(Value::Null, None) } },
                "run" => match &error { Some(e) => Out::Settle(Value::Null, Some(e.clone())),
                    None => { vm.apply(f, vec![])?; Out::Settle(Value::Null, None) } },
                "handle" => { let e = error.clone().unwrap_or(Value::Null);
                    Out::Settle(vm.apply(f, vec![result.clone(), e])?.unwrap_or(Value::Null), None) },
                "whenComplete" => { let e = error.clone().unwrap_or(Value::Null);
                    vm.apply(f, vec![result.clone(), e])?; Out::Settle(result.clone(), error.clone()) },
                "exceptionally" => match &error { Some(e) => Out::Settle(vm.apply(f, vec![e.clone()])?.unwrap_or(Value::Null), None),
                    None => Out::Settle(result.clone(), None) },
                "combine" => match &error { Some(e) => Out::Settle(Value::Null, Some(e.clone())), None => {
                    let or = settled_other(vm, other);
                    Out::Settle(vm.apply(f, vec![result.clone(), or])?.unwrap_or(Value::Null), None) } },
                "compose" => match &error { Some(e) => Out::Settle(Value::Null, Some(e.clone())), None => {
                    let cf = vm.apply(f, vec![result.clone()])?.unwrap_or(Value::Null);
                    if let Value::Ref(cfh) = cf {
                        if matches!(vm.heap.get_field(cfh, "$done"), Value::Int(1)) {
                            let r = vm.heap.get_field(cfh, "$result");
                            let er = match vm.heap.get_field(cfh, "$error") { Value::Null => None, e => Some(e) };
                            Out::Settle(r, er)
                        } else { cf_add_cb(vm, cfh, "mirror", Value::Null, Value::Null, down); Out::Deferred }
                    } else { Out::Settle(Value::Null, None) } } },
                "mirror" => Out::Settle(result.clone(), error.clone()),
                _ => Out::Settle(result.clone(), error.clone()),
            })
        })();
        let (dres, derr) = match computed {
            Ok(Out::Settle(r, e)) => (r, e),
            Ok(Out::Deferred) => return Ok(()),
            Err(VmError::Throw(e)) => (Value::Null, Some(e)),
            Err(other) => return Err(other),
        };
        return cf_complete(vm, down, dres, derr);
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let (dres, derr): (Value, Option<Value>) = match kind {
            "apply" => match &error { Some(e) => (Value::Null, Some(e.clone())),
                None => (vm.apply(f, vec![result])?.unwrap_or(Value::Null), None) },
            "accept" => match &error { Some(e) => (Value::Null, Some(e.clone())),
                None => { vm.apply(f, vec![result])?; (Value::Null, None) } },
            "run" => match &error { Some(e) => (Value::Null, Some(e.clone())),
                None => { vm.apply(f, vec![])?; (Value::Null, None) } },
            "handle" => { let e = error.clone().unwrap_or(Value::Null);
                (vm.apply(f, vec![result, e])?.unwrap_or(Value::Null), None) },
            "whenComplete" => { let e = error.clone().unwrap_or(Value::Null);
                vm.apply(f, vec![result.clone(), e])?; (result, error) },
            "exceptionally" => match &error { Some(e) => (vm.apply(f, vec![e.clone()])?.unwrap_or(Value::Null), None),
                None => (result, None) },
            "combine" => match &error { Some(e) => (Value::Null, Some(e.clone())), None => {
                let or = settled_other(vm, other);
                (vm.apply(f, vec![result, or])?.unwrap_or(Value::Null), None) } },
            "compose" => match &error { Some(e) => (Value::Null, Some(e.clone())), None => {
                let cf = vm.apply(f, vec![result])?.unwrap_or(Value::Null);
                if let Value::Ref(cfh) = cf {
                    if matches!(vm.heap.get_field(cfh, "$done"), Value::Int(1)) {
                        let r = vm.heap.get_field(cfh, "$result");
                        let er = match vm.heap.get_field(cfh, "$error") { Value::Null => None, e => Some(e) };
                        (r, er)
                    } else { cf_add_cb(vm, cfh, "mirror", Value::Null, Value::Null, down); return Ok(()); }
                } else { (Value::Null, None) } } },
            "mirror" => (result, error),
            _ => (result, error),
        };
        cf_complete(vm, down, dres, derr)
    }
}

pub(crate) fn future_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // GILFREE-RINGS — THE PARK BOUNDARY. Every CompletableFuture method except the
    // two *Async factories is a point where the guest can OBSERVE a future the
    // off-GIL lane deferred: it joins it, chains it, sequences it, or asks whether
    // it is done. All of them route through here (the `cf_*` helpers below are only
    // ever reached from this function), so draining at this one line is exhaustive
    // — after it, every deferred future is complete, exactly as the inline seam
    // would have left it. Main is inside a native holding the GIL here, which is
    // what makes the park possible at all.
    // ⚠**AND THE DRAIN IS SCOPED TO THE FUTURE ACTUALLY OBSERVED** (`observes_deferred`).
    // "All of them route through here, so draining at this one line is exhaustive" is true and
    // was never the problem; draining at this line for a future that is NOT in the batch is.
    // See `observes_deferred` for the measurement: it is 62.9 s of MC's 112.7 s boot.
    #[cfg(feature = "next-gen-gc")]
    if !matches!(name, "supplyAsync" | "runAsync") && crate::gilfree_cf::observes_deferred(vm, name, args) {
        if crate::env_flag!("RAVA_LANE_DRAIN_TRACE") {
            { let (n, first) = crate::gilfree_cf::pending_shape(vm);
              eprintln!("[LANE-DRAIN] by `{name}` n={n} first={first}"); }
        }
        crate::gilfree_cf::drain(vm);
    }
    // Static factories. We run everything inline (single-threaded), so the
    // returned future is already complete; *Async just runs the supplier now.
    match name {
        "completedFuture" | "completedStage" => return Ok(Some(Some(make_cf(vm, args.first().cloned().unwrap_or(Value::Null), None)))),
        "failedFuture" | "failedStage" => return Ok(Some(Some(make_cf(vm, Value::Null, Some(args.first().cloned().unwrap_or(Value::Null)))))),
        "supplyAsync" => {
            // Did the call even ARRIVE here? With compilers off, 8 bodies reach the queue; with
            // them on, 1 — and neither `refused` nor `executor-unproven` moved, so the other
            // seven do not reach the decision at all. This separates "not asked" from "asked and
            // declined", which is the distinction the whole arc keeps needing.
            crate::vm::threads::OFFLOAD_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Periodic dump point: `supplyAsync` is exactly where the verdicts accumulate.
            taskgraph_probe::tick();
            // CompletableFuture contract: a supplier that throws COMPLETES the future
            // exceptionally — it does NOT propagate. Wall #15: rava ran the supplier
            // inline with `?`, so a throwing reload listener (e.g. a loot-table decode
            // error) escaped as an uncaught throw instead of being captured; when an
            // outer frame swallowed it the listener's future stayed un-done forever and
            // `Util.blockUntilDone` hot-spun on it (worldgen "softlock"). Capture it.
            #[cfg(feature = "next-gen-gc")]
            {
                let task = args.first().cloned().unwrap_or(Value::Null);
                let future = make_pending_future(vm, "java/util/concurrent/CompletableFuture");
                // GILFREE-RINGS: DEFER the task to the off-GIL lane. It does not run
                // here and no worker starts — the batch executes at the next park (see
                // `gilfree_cf::drain`), which is what keeps main from mutating the live
                // structures the lane carves its isolation out of. Returning a pending
                // future is exactly what CompletableFuture's contract licenses.
                // ⭐⭐**THE TASK RUNTIME GOES FIRST, AND THE LANE IS THE FALLBACK** (opt out with
                // `RAVA_NO_OFFLOAD_FIRST`).
                //
                // ⛔The deferral lane is COMPILER-GATED — it arms only when the JIT is on — and it
                // claimed the body before the runtime ever saw it. That is why the offload's 10×
                // never reached the product configuration: with compilers OFF the lane is absent,
                // 8 bodies reach the runtime and `Done` is 0.6 s; with them ON the lane swallowed
                // **134 of 135** `supplyAsync` calls (counted: `seen=135 queued=1 refused=0
                // executor-unproven=0`) and the boot paid **78 s inside the `Done` window** for
                // the stronghold search the runtime would have taken off the critical path.
                //
                // The two are not equivalent and the difference is exactly the one row 375 named:
                // the lane DEFERS — its batch runs at the next park, and during loading the main
                // thread never parks — while the runtime RUNS the body on a real thread now. So
                // the runtime is tried first and the lane keeps everything it refuses.
                let runtime_first = crate::scratch::offload_first();
                if runtime_first
                    && (args.get(1).is_none()
                        || has_resident_task_capability(vm, args.get(1))
                        || crate::scratch::async_any_executor())
                    && vm.enqueue_guest_task(task.clone(), Some(future.clone()), None)
                {
                    return Ok(Some(Some(future)));
                }
                if crate::gilfree_cf::defer(vm, &task, &future) {
                    return Ok(Some(Some(future)));
                }
                // `RAVA_ASYNC_ANY_EXECUTOR` — an executor we cannot PROVE resident is still not a
                // licence to run on the CALLING THREAD.
                //
                // ⭐Ledger row 368: `supplyAsync(supplier, Util.backgroundExecutor())` is a
                // two-argument call, MC's executor carries no resident-task capability, so this
                // test fails and the body falls through to `vm.apply` inline — which is
                // **91% of the compilers-off boot wall**, running the stronghold ring search
                // inside `ServerLevel.<init>`. The contract forbids exactly one execution site,
                // and it is the caller's thread; our own task runtime is strictly closer to it
                // than inline is. Opt-in while unproven.
                // ORDER MATTERS: submitting to an executor the guest DRAINS ITSELF must come
                // before handing the body to our own task runtime. `Util.blockUntilDone` passes a
                // queue-backed executor and then spins draining that queue; put the body anywhere
                // else and the queue stays empty and the spin never ends — measured four times.
                // SUBMIT TO THE GIVEN EXECUTOR (`RAVA_CF_SUBMIT`). The wrapper is a `FutureTask`
                // whose `run()` this module already implements: it applies `$task` and completes
                // itself, and the `$cf` link added below makes it complete the CompletableFuture
                // this call returns. So whoever drains the executor — MC's own `blockUntilDone`
                // queue, or a background pool that is never drained during boot — decides WHEN the
                // body runs, which is precisely what the contract says and what the JVM does.
                if crate::scratch::cf_submit() {
                    if let Some(exec) = args.get(1) {
                        // ⛔**ROW 372 DISMISSED THIS BRANCH TOO FAST.** It argued that a virtual
                        // `execute` is pointless because `reflect/concurrent.rs` intercepts
                        // `execute` and runs the Runnable inline. That is true only for the
                        // executor classes we implement natively. MC threads its executor through
                        // GUEST wrappers, and a park trace taken with the functional branch already
                        // live proves it: `ReloadableServerRegistries.scheduleRegistryLoad(…,
                        // Executor)` still fell through, so ITS executor is an OBJECT. Invoking
                        // `execute` on it runs MC's own delegation chain — which for
                        // `Util.blockUntilDone` ends at the queue the caller is draining.
                        let obj_exec_class = match exec {
                            Value::Ref(h) if !exec.is_fn_value() => {
                                vm.heap.class_of(*h).map(|c| c.to_string())
                            }
                            #[cfg(feature = "next-gen-gc")]
                            Value::Obj(pp) if !exec.is_fn_value() => {
                                crate::nextgen::class_name(crate::nextgen::arena_class_id(*pp))
                                    .map(|n| n.to_string())
                            }
                            _ => None,
                        };
                        if let Some(cls) = obj_exec_class {
                            if let Value::Ref(fh) = &future {
                                let tt = vm.heap.alloc("java/util/concurrent/FutureTask");
                                vm.heap.put_field(tt, "$task", task.clone());
                                vm.heap.put_field(tt, "$cf", Value::Ref(*fh));
                                #[cfg(feature = "next-gen-gc")]
                                crate::vm::thread_context::push_root(
                                    &mut vm.gc_protect,
                                    Value::Ref(tt),
                                );
                                #[cfg(feature = "next-gen-gc")]
                                crate::vm::thread_context::push_root(
                                    &mut vm.gc_protect,
                                    future.clone(),
                                );
                                let handed = match vm.resolve_by_args(&cls, "execute", "Ljava/lang/Runnable;") {
                                    Ok(Some((decl, desc, _))) => vm
                                        .invoke(decl, "execute", &desc, vec![exec.clone(), Value::Ref(tt)])
                                        .map(|_| true),
                                    // No `execute` we can resolve: leave the task to the paths
                                    // below exactly as before.
                                    Ok(None) => Ok(false),
                                    Err(e) => Err(e),
                                };
                                #[cfg(feature = "next-gen-gc")]
                                {
                                    let _ = crate::vm::thread_context::pop_root(&mut vm.gc_protect);
                                    let _ = crate::vm::thread_context::pop_root(&mut vm.gc_protect);
                                }
                                if matches!(handed, Ok(true)) {
                                    return Ok(Some(Some(future)));
                                }
                            }
                        }
                        if exec.is_fn_value() {
                            if let Value::Ref(fh) = &future {
                                let tt = vm.heap.alloc("java/util/concurrent/FutureTask");
                                vm.heap.put_field(tt, "$task", task.clone());
                                vm.heap.put_field(tt, "$cf", Value::Ref(*fh));
                                // Both the wrapper and the future are reachable only from these
                                // Rust locals across an `apply` that allocates.
                                #[cfg(feature = "next-gen-gc")]
                                crate::vm::thread_context::push_root(
                                    &mut vm.gc_protect,
                                    Value::Ref(tt),
                                );
                                #[cfg(feature = "next-gen-gc")]
                                crate::vm::thread_context::push_root(
                                    &mut vm.gc_protect,
                                    future.clone(),
                                );
                                let submitted = vm.apply(exec, vec![Value::Ref(tt)]);
                                #[cfg(feature = "next-gen-gc")]
                                {
                                    let _ = crate::vm::thread_context::pop_root(&mut vm.gc_protect);
                                    let _ = crate::vm::thread_context::pop_root(&mut vm.gc_protect);
                                }
                                match submitted {
                                    Ok(_) => return Ok(Some(Some(future))),
                                    // An executor we cannot call is not a reason to lose the work:
                                    // fall through to the paths below exactly as before.
                                    Err(VmError::Throw(_)) | Err(_) => {}
                                }
                            }
                        }
                    }
                }
                let executor_proven = args.get(1).is_none()
                    || has_resident_task_capability(vm, args.get(1))
                    || crate::scratch::async_any_executor();
                if !executor_proven {
                    crate::vm::threads::OFFLOAD_UNPROVEN
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if executor_proven
                    && vm.enqueue_guest_task(task.clone(), Some(future.clone()), None)
                {
                    return Ok(Some(Some(future)));
                }
                // GILFREE-RINGS stage 1 (RAVA_TASKGRAPH_PROBE): map WHY each inline
                // supplyAsync body fails offload admission — per-owner verdict tally
                // + the full rejection chain via the deduped drill printer. The
                // analysis may load classes (resolve walk); acceptable on a
                // diagnostic boot, never on a measurement run.
                #[cfg(feature = "next-gen-gc")]
                if taskgraph_probe::enabled() {
                    if let Value::Ref(h) = &task {
                        if let Some(site) = vm.heap.lambda_site_of(*h) {
                            let (o, n, d) = (
                                site.owner.to_string(),
                                site.name.to_string(),
                                site.descriptor.to_string(),
                            );
                            let ok = vm.is_offloadable_devirt(&o, &n, &d, false, false);
                            taskgraph_probe::note_offload(&format!("{o}::{n}"), ok);
                            if !ok {
                                vm.offload_drill_print(&o, &n, &d);
                            }
                        }
                    }
                }
                // LAZY FALLBACK (`RAVA_LAZY_CF`): park the supplier ON the future instead of
                // running it here. Every instance method of a future forces it first, so the body
                // still runs before anyone can observe a result — and a future nobody observes
                // costs nothing, which is what the JVM's own boot does with the stronghold search.
                if crate::scratch::lazy_cf() {
                    if let Value::Ref(fh) = &future {
                        // Trace WHICH bodies get parked, so a park with no matching force can be
                        // named instead of guessed at. Capped and off by default.
                        let cap = crate::scratch::lazy_cf_trace();
                        if cap > 0 {
                            use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
                            static N: AtomicUsize = AtomicUsize::new(0);
                            let n = N.fetch_add(1, Relaxed);
                            if n < cap {
                                // ⚠**PRINT THE CLASS FOR BOTH REPRESENTATIONS.** The first version
                                // asked `lambda_site_of`, which takes a HANDLE — so every arena
                                // task (`Value::Obj`) printed `<not a ref>` and the three parked
                                // bodies stayed anonymous through four failed fix attempts. A task
                                // this path cannot name is a task whose refusal cannot be reasoned
                                // about.
                                let who = match &task {
                                    Value::Ref(th) => vm
                                        .heap
                                        .lambda_site_of(*th)
                                        .map(|s| format!("{}::{}", s.owner, s.name))
                                        .unwrap_or_else(|| {
                                            format!(
                                                "ref class={}",
                                                vm.heap.class_of(*th).unwrap_or("?")
                                            )
                                        }),
                                    #[cfg(feature = "next-gen-gc")]
                                    Value::Obj(pp) => {
                                        let cid = crate::nextgen::arena_class_id(*pp);
                                        format!(
                                            "arena class={}",
                                            crate::nextgen::class_name(cid)
                                                .map(|n| n.to_string())
                                                .unwrap_or_else(|| format!("<cid {cid}>"))
                                        )
                                    }
                                    other => format!("<{other:?}>"),
                                };
                                // The class of an arena lambda is its CAPTURE SHAPE, not its
                                // identity, so three parks reading `[arena-lambda:4_4_4]` name
                                // nothing. The caller chain does.
                                eprintln!(
                                    "[LAZY-PARK #{n}] fut={} {who}
{}",
                                    *fh,
                                    vm.backtrace()
                                );
                            }
                        }
                        vm.heap.put_field(*fh, "$lazytask", task);
                        super::PARKED.lock().unwrap_or_else(|e| e.into_inner()).push(*fh);
                        return Ok(Some(Some(future)));
                    }
                }
                let outcome = {
                    let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::SupplyAsync, &task);
                    // PILOT CENSUS: per-task cold-metadata + would-be-locked deltas.
                    let _pilot = crate::pilot_census::enabled()
                        .then(|| taskgraph_probe::owner_of(vm, &task))
                        .and_then(crate::pilot_census::task_scope);
                    vm.apply(&task, vec![])
                };
                return Ok(Some(Some(match outcome {
                    Ok(r) => make_cf(vm, r.unwrap_or(Value::Null), None),
                    Err(VmError::Throw(e)) => make_cf(vm, Value::Null, Some(e)),
                    Err(other) => return Err(other),
                })));
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                let task = args.first().cloned().unwrap_or(Value::Null);
                let r = {
                    let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::SupplyAsync, &task);
                    vm.apply(&task, vec![])?.unwrap_or(Value::Null)
                };
                return Ok(Some(Some(make_cf(vm, r, None))));
            }
        }
        "runAsync" => {
            #[cfg(feature = "next-gen-gc")]
            {
                let task = args.first().cloned().unwrap_or(Value::Null);
                let future = make_pending_future(vm, "java/util/concurrent/CompletableFuture");
                let executor_proven =
                    args.get(1).is_none() || has_resident_task_capability(vm, args.get(1));
                if executor_proven && vm.enqueue_guest_task(
                    task.clone(),
                    Some(future.clone()),
                    Some(Value::Null),
                ) {
                    return Ok(Some(Some(future)));
                }
                let outcome = {
                    let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::RunAsync, &task);
                    vm.apply(&task, vec![])
                };
                return Ok(Some(Some(match outcome {
                    Ok(_) => make_cf(vm, Value::Null, None),
                    Err(VmError::Throw(e)) => make_cf(vm, Value::Null, Some(e)),
                    Err(other) => return Err(other),
                })));
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                let task = args.first().cloned().unwrap_or(Value::Null);
                let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::RunAsync, &task);
                vm.apply(&task, vec![])?;
                return Ok(Some(Some(make_cf(vm, Value::Null, None))));
            }
        }
        // guava ListenableFutureTask.create(Callable) | create(Runnable, V) and
        // SettableFuture.create(): build the task object; run() executes it later.
        "create" => {
            let f = vm.heap.alloc("java/util/concurrent/FutureTask");
            if let Some(task) = args.first().cloned() {
                vm.heap.put_field(f, "$task", task);
            }
            // create(Runnable, result): preset the result run() keeps (a Runnable
            // produces no value of its own).
            if let Some(r) = args.get(1).cloned() {
                vm.heap.put_field(f, "$result", r);
            }
            return Ok(Some(Some(Value::Ref(f))));
        }
        "anyOf" => {
            let inputs = crate::natives::varargs(vm, args.first());
            // Complete from the first already-done input; else fire on the first
            // to complete.
            // Force before inspecting: a parked body is not "not done", it is "not started".
            for f in inputs.iter() {
                super::force_parked_value(vm, f)?;
            }
            if let Some(donef) = inputs.iter().find(|f| matches!(f, Value::Ref(h) if matches!(vm.heap.get_field(*h,"$done"), Value::Int(1)))) {
                let r = crate::natives::field_or_null(vm, Some(donef), "$result");
                return Ok(Some(Some(make_cf(vm, r, None))));
            }
            let down = vm.heap.alloc("java/util/concurrent/CompletableFuture");
            for f in &inputs { if let Value::Ref(h) = f { cf_add_cb(vm, *h, "anyof", Value::Null, Value::Null, down); } }
            return Ok(Some(Some(Value::Ref(down))));
        }
        // Completes when every input completes. Inputs may still be pending
        // (deferred chunk-gen futures), so count down via callbacks.
        "allOf" => {
            let inputs = crate::natives::varargs(vm, args.first());
            let down = vm.heap.alloc("java/util/concurrent/CompletableFuture");
            let pending: Vec<u32> = inputs.iter().filter_map(|f| match f {
                Value::Ref(h) if !matches!(vm.heap.get_field(*h, "$done"), Value::Int(1)) => Some(*h), _ => None,
            }).collect();
            let err0 = inputs.iter().find_map(|f| match crate::natives::field_or_null(vm, Some(f), "$error") { Value::Null => None, e => Some(e) });
            if pending.is_empty() {
                cf_complete(vm, down, Value::Null, err0)?;
            } else {
                if let Some(e) = err0 { vm.heap.put_field(down, "$err0", e); }
                vm.heap.put_field(down, "$pending", Value::Int(pending.len() as i32));
                for h in pending { cf_add_cb(vm, h, "allof", Value::Null, Value::Null, down); }
            }
            return Ok(Some(Some(Value::Ref(down))));
        }
        _ => {}
    }
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    // Wall #15 TYPE-GATE: a CompletionStage chaining method MUST have a CompletableFuture
    // receiver. rava's dispatch routes by the bytecode's STATIC type, so a CORRUPTED
    // operand (a non-future value sitting where a CompletableFuture belongs) reached here
    // and silently got a dead callback registered → an endless `Util.blockUntilDone` spin
    // with no error. Reject it LOUDLY like the JVM (ClassCastException) so a future stack
    // corruption surfaces at the fault site instead of hanging. Gated → default unchanged.
    #[cfg(feature = "next-gen-gc")]
    {
        const CHAIN: &[&str] = &[
            "thenApply", "thenApplyAsync", "thenAccept", "thenAcceptAsync", "thenRun", "thenRunAsync",
            "thenCompose", "thenComposeAsync", "thenCombine", "thenCombineAsync", "whenComplete",
            "whenCompleteAsync", "handle", "handleAsync", "exceptionally", "exceptionallyAsync",
            "applyToEither", "applyToEitherAsync", "acceptEither", "acceptEitherAsync",
            "runAfterEither", "runAfterEitherAsync", "runAfterBoth", "runAfterBothAsync",
        ];
        // ⭐**THE TYPE-GATE BUILT ITS DIAGNOSTIC BEFORE ASKING WHETHER IT NEEDED ONE.** Every
        // chaining call — and MC's reload chains constantly — copied the receiver's class name
        // into a `String` so the test could read it and drop it. `class_of` borrows; the name
        // is only OWNED on the throw path now, where an allocation is free. The test itself is
        // SEMANTIC, not lexical, and STRICT — the same never-lenient walk
        // `Class.isAssignableFrom` uses (`name_is_subtype_strict`): the exact fast path keeps
        // the engine's own futures walk-free, a genuine CompletionStage implementor/subtype is
        // PROVEN through loaded classfiles or the JDK table, and everything else — a
        // deceptively-named impostor, or an unprovable chain the LENIENT walk used to admit
        // (FutureTask / the guava futures: not CompletionStages in the real JDK) — rejects.
        if CHAIN.contains(&name)
            && !match vm.heap.class_of(recv) {
                Some(c) if c == "java/util/concurrent/CompletableFuture" => true,
                Some(c) => crate::opcodes::name_is_subtype_strict(vm, c, "java/util/concurrent/CompletionStage"),
                None => false,
            }
        {
            let cls = vm.heap.class_of(recv).unwrap_or("").to_string();
            let msg = format!(
                "class {} cannot be cast to class java.util.concurrent.CompletableFuture (receiver of {name})",
                cls.replace('/', ".")
            );
            return Err(VmError::Throw(vm.make_exception("java/lang/ClassCastException", &msg)));
        }
    }
    // FORCE A PARKED SUPPLIER BEFORE ANY OBSERVATION (`RAVA_LAZY_CF`, see `scratch::lazy_cf`).
    //
    // This is the ONE place every instance method of a future passes through, which is what makes
    // the lazy fallback safe to state as a rule rather than as a list: `join`, `get`, `isDone`,
    // `thenApply` and every other chain method force first, so nothing can read a result or attach
    // a callback before the body has run.
    super::force_parked(vm, recv)?;
    // ⭐**EVERY METHOD THAT CONSUMES THE RESULT MUST WAIT, NOT JUST `join`/`get`** (row 422).
    //
    // `$result` is read unconditionally two lines below, for EVERY name — but only `join` and
    // `get` waited for a task that is genuinely still running. That was invisible for as long as
    // every future was born complete ("we run everything inline, so the returned future is already
    // complete"). The moment a body actually runs on a worker (row 421), `thenCombine` read
    // `$result` of a pending future, got `Null`, and MC's `ServerFunctionLibrary.lambda$reload$8`
    // took `NullPointerException: Cannot invoke "java.util.Map.forEach(...)"` on the Pair it built
    // — which is precisely the one failure left after the offload stopped hanging.
    //
    // ⚠**The exceptions are the observers that are DEFINED not to block**: `isDone` must be able to
    // answer "no", `getNow` must be able to return its default, and completing or cancelling a
    // future must never wait on it. Everything else — the whole `then*` family — runs its
    // continuation immediately in this engine, so it needs the value now.
    //
    // Blocking here is stricter than Java, which would register a callback. It is the honest
    // shape for an engine with no callback machinery: the chain becomes synchronous at the point
    // of consumption instead of silently consuming a null.
    const NON_BLOCKING: &[&str] = &[
        "isDone",
        "isCancelled",
        "isCompletedExceptionally",
        "getNow",
        "complete",
        "completeExceptionally",
        "completeAsync",
        "cancel",
        "obtrudeValue",
        "obtrudeException",
        "getNumberOfDependents",
        "toString",
    ];
    if !NON_BLOCKING.contains(&name)
        && !matches!(vm.heap.get_field(recv, "$done"), Value::Int(1))
        && vm.task_runtime.tracks_future(recv)
    {
        vm.wait_for_tracked_future(recv);
    }
    let result = vm.heap.get_field(recv, "$result");
    let error = match vm.heap.get_field(recv, "$error") { Value::Null => None, e => Some(e) };
    let arg1 = || -> Value { args.get(1).cloned().unwrap_or(Value::Null) };
    match name {
        // FutureTask(Callable) | FutureTask(Runnable, result): stash the wrapped
        // task so run() can execute it. A Runnable form also presets the result.
        "<init>" => {
            if let Some(task) = args.get(1).cloned() {
                vm.heap.put_field(recv, "$task", task);
            }
            if let Some(r) = args.get(2).cloned() {
                vm.heap.put_field(recv, "$result", r);
            }
            Ok(Some(None))
        }
        // FutureTask/ListenableFutureTask.run(): execute the wrapped task on THIS
        // thread (the contract — MC's SystemUtils drains its task queue this way),
        // capture a Callable's return as the result, mark done, fire listeners.
        // A Runnable returns nothing, so its preset $result (if any) is kept.
        "run" | "runAndReset" => {
            let task = vm.heap.get_field(recv, "$task");
            if task.is_fn_value() {
                let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::FutureTaskRun, &task);
                if let Some(v) = vm.apply(&task, vec![])? {
                    vm.heap.put_field(recv, "$result", v);
                }
            }
            vm.heap.put_field(recv, "$done", Value::Int(1));
            fire_future_listeners(vm, recv)?;
            // `RAVA_CF_SUBMIT` links a submitted wrapper to the CompletableFuture that
            // `supplyAsync` handed back. Completing the wrapper without completing that future
            // would leave every downstream stage waiting on a task that has already run.
            if let Value::Ref(cf) = vm.heap.get_field(recv, "$cf") {
                let r = vm.heap.get_field(recv, "$result");
                super::cf_complete(vm, cf, r, None)?;
            }
            if name == "runAndReset" { Ok(Some(Some(Value::Int(1)))) } else { Ok(Some(None)) }
        }
        // guava ListenableFuture.addListener(Runnable, Executor): run inline if the
        // future is already complete, else queue it to fire on completion.
        "addListener" => {
            let listener = arg1();
            if matches!(vm.heap.get_field(recv, "$done"), Value::Int(1)) {
                if listener.is_fn_value() { vm.apply(&listener, vec![])?; }
            } else if listener.is_fn_value() {
                // ⭐**A QUADRATIC APPEND.** Registering the k-th listener DECODED the whole
                // `$listeners` array into a `Vec<Value>`, pushed one element, and allocated a
                // FRESH array for the result — so queueing n listeners on one future copied
                // n(n+1)/2 values and allocated n arrays. `$listeners` is our own synthetic
                // slot, read by exactly one reader (`fire_future_listeners`, which detaches it
                // before running anything), and `Heap::array_push` is the in-place append these
                // growable backing arrays exist for. WAS O(listeners^2) in listeners queued on
                // one future; IS O(listeners). Order is unchanged — append is append.
                // REFUSE unless the handle really names a live array: `array_push` is a no-op on
                // a bad handle, which would silently DROP the listener, whereas the rebuild path
                // replaces the slot. In that case fall through invisibly to the old form.
                let existing = match vm.heap.get_field(recv, "$listeners") {
                    Value::Array(a) => Some(a),
                    _ => None,
                };
                let appendable =
                    existing.filter(|a| fast_reflect() && vm.heap.array_len(*a).is_some());
                match (appendable, existing) {
                    (Some(a), _) => vm.heap.array_push(a, listener),
                    (None, Some(a)) => {
                        let mut ls = vm.heap.array_values(a).unwrap_or_default();
                        ls.push(listener);
                        let arr = vm.heap.alloc_values("Ljava/lang/Object;", ls);
                        vm.heap.put_field(recv, "$listeners", Value::Array(arr));
                    }
                    (None, None) => {
                        let arr = vm.heap.alloc_values("Ljava/lang/Object;", vec![listener]);
                        vm.heap.put_field(recv, "$listeners", Value::Array(arr));
                    }
                }
            }
            Ok(Some(None))
        }
        // SettableFuture.set(v) / setException(t) / setFuture(f): complete the
        // future and fire listeners; returns whether it was newly completed.
        "set" | "setException" => {
            let already = matches!(vm.heap.get_field(recv, "$done"), Value::Int(1));
            if !already {
                if name == "set" { vm.heap.put_field(recv, "$result", arg1()); }
                else { vm.heap.put_field(recv, "$error", arg1()); }
                vm.heap.put_field(recv, "$done", Value::Int(1));
                fire_future_listeners(vm, recv)?;
            }
            Ok(Some(Some(Value::Int((!already) as i32))))
        }
        // Reads. join/get rethrow the captured exception (CompletionException-ish).
        // A FAILED FUTURE RAISES A WRAPPER, not the cause. `join` throws
        // `CompletionException` and `get` throws `ExecutionException`, both carrying the
        // original as `getCause()` — guest code catches the wrapper and unwraps, so
        // handing it the bare cause means the catch does not match and the unwrap NPEs
        // on a null cause. `completedFuture(x).join()` is unaffected; this is the
        // exceptional path only.
        "join" | "get" => match error {
            Some(e) => {
                let wrapper = if name == "join" {
                    "java/util/concurrent/CompletionException"
                } else {
                    "java/util/concurrent/ExecutionException"
                };
                let msg = vm.value_to_string(&e).unwrap_or_default();
                let w = vm.make_exception(wrapper, &msg);
                if let Value::Ref(wh) = &w {
                    vm.heap.put_field(*wh, "$cause", e);
                }
                Err(VmError::Throw(w))
            }
            None => Ok(Some(Some(result))),
        },
        // A future is done only once completed (factory/derived via make_cf, or
        // an explicit complete()). A bytecode-`new CompletableFuture()` is NOT
        // done until completed — ServerChunkCache/GenerationChunkHolder branch on
        // isDone() to decide whether to (re)schedule generation, so reporting a
        // fresh future as done dropped the chunk-gen task and left it null.
        "isDone" => Ok(Some(Some(Value::Int(matches!(vm.heap.get_field(recv, "$done"), Value::Int(1)) as i32)))),
        "getNow" => Ok(Some(Some(match (matches!(vm.heap.get_field(recv, "$done"), Value::Int(1)), &error) {
            (true, Some(_)) => arg1(),
            (true, None) => result,
            (false, _) => arg1(), // not complete → the supplied default
        }))),
        "isCancelled" => Ok(Some(Some(Value::Int(0)))),
        "isCompletedExceptionally" => Ok(Some(Some(Value::Int(error.is_some() as i32)))),
        "cancel" => Ok(Some(Some(Value::Int(1)))),
        "getDelay" => Ok(Some(Some(Value::Long(0)))),
        "compareTo" => Ok(Some(Some(Value::Int(0)))),
        "complete" => {
            let already = matches!(vm.heap.get_field(recv, "$done"), Value::Int(1));
            if !already { cf_complete(vm, recv, arg1(), None)?; }
            Ok(Some(Some(Value::Int((!already) as i32))))
        }
        "completeExceptionally" => {
            let already = matches!(vm.heap.get_field(recv, "$done"), Value::Int(1));
            if !already { cf_complete(vm, recv, Value::Null, Some(arg1()))?; }
            Ok(Some(Some(Value::Int((!already) as i32))))
        }
        "toCompletableFuture" | "copy" | "minimalCompletionStage" => Ok(Some(Some(Value::Ref(recv)))),
        // Transforms via the deferred-callback model: run now if the source is
        // complete, else register to fire on completion (cf_chain). `other` is
        // the second future (combine) where applicable.
        // The Async variant is passed through: only it may run the body off the calling
        // thread, and only it is offered to the lane (see `cf_chain_async`).
        "thenApply" => Ok(Some(Some(cf_chain(vm, recv, "apply", arg1(), Value::Null)?))),
        "thenApplyAsync" => {
            Ok(Some(Some(cf_chain_async(vm, recv, "apply", arg1(), Value::Null, true)?)))
        }
        "thenAccept" | "thenAcceptAsync" => Ok(Some(Some(cf_chain(vm, recv, "accept", arg1(), Value::Null)?))),
        "thenRun" | "thenRunAsync" => Ok(Some(Some(cf_chain(vm, recv, "run", arg1(), Value::Null)?))),
        "thenCompose" | "thenComposeAsync" => Ok(Some(Some(cf_chain(vm, recv, "compose", arg1(), Value::Null)?))),
        "thenCombine" | "thenCombineAsync" =>
            Ok(Some(Some(cf_chain(vm, recv, "combine", args.get(2).cloned().unwrap_or(Value::Null), arg1())?))),
        // *Either: complete from whichever of `this`/`other` finishes first. The
        // `other` future (args[1]) MUST be watched too — see `cf_either`.
        #[cfg(feature = "next-gen-gc")]
        "applyToEither" | "applyToEitherAsync" | "acceptEither" | "acceptEitherAsync"
        | "runAfterEither" | "runAfterEitherAsync" => {
            let kind = if name.starts_with("apply") { "apply" } else if name.starts_with("accept") { "accept" } else { "run" };
            let other = match args.get(1) { Some(Value::Ref(h)) => *h, _ => 0 };
            Ok(Some(Some(cf_either(vm, recv, other, kind, args.get(2).cloned().unwrap_or(Value::Null))?)))
        }
        #[cfg(not(feature = "next-gen-gc"))]
        "applyToEither" | "applyToEitherAsync" =>
            Ok(Some(Some(cf_chain(vm, recv, "apply", args.get(2).cloned().unwrap_or(Value::Null), Value::Null)?))),
        #[cfg(not(feature = "next-gen-gc"))]
        "acceptEither" | "acceptEitherAsync" =>
            Ok(Some(Some(cf_chain(vm, recv, "accept", args.get(2).cloned().unwrap_or(Value::Null), Value::Null)?))),
        // runAfterBoth completes only when BOTH complete; runAfterEither is the *Either
        // family. (next-gen-gc routes the Either names above.)
        #[cfg(not(feature = "next-gen-gc"))]
        "runAfterEither" | "runAfterEitherAsync" =>
            Ok(Some(Some(cf_chain(vm, recv, "run", args.get(2).cloned().unwrap_or(Value::Null), Value::Null)?))),
        "runAfterBoth" | "runAfterBothAsync" =>
            Ok(Some(Some(cf_chain(vm, recv, "run", args.get(2).cloned().unwrap_or(Value::Null), Value::Null)?))),
        "whenComplete" | "whenCompleteAsync" => Ok(Some(Some(cf_chain(vm, recv, "whenComplete", arg1(), Value::Null)?))),
        "handle" | "handleAsync" => Ok(Some(Some(cf_chain(vm, recv, "handle", arg1(), Value::Null)?))),
        "exceptionally" | "exceptionallyAsync" => Ok(Some(Some(cf_chain(vm, recv, "exceptionally", arg1(), Value::Null)?))),
        _ => Ok(None),
    }
}


