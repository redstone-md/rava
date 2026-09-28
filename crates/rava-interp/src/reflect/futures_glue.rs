//! Chaining, firing and protecting CompletableFuture callbacks.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

pub(crate) fn has_resident_task_capability(vm: &Vm, value: Option<&Value>) -> bool {
    use crate::heap::FK_REF;
    let Some(Value::Ref(h)) = value else { return false };
    // Direct check: executor itself carries the marker.
    if matches!(vm.heap.get_field(*h, RESIDENT_TASK_CAPABILITY), Value::Int(1)) {
        return true;
    }
    // 1-level structural delegation: scan ref-typed fields of the wrapper.
    // TracingExecutor is a Java Record with one field `service:ExecutorService`;
    // Executors.newCachedThreadPool returns an ExecutorService marked in
    // reflect/atomics.rs:134. This transitive check admits the wrapper WITHOUT
    // depending on the field name "service" (obfuscation-proof) or the class
    // name "TracingExecutor"/"Util" (no-adapt).
    // Phase 5: through the `&self` facade, so the handle table stays private to `heap/`.
    if let Some(obj) = vm.heap.get(*h) {
        let ids = obj.fields.ids();
        let cells = obj.fields.cells();
        let kinds = obj.fields.kinds();
        for i in 0..ids.len() {
            if kinds[i] == FK_REF {
                let inner_h = cells[i] as u32;
                if inner_h != 0 && matches!(vm.heap.get_field(inner_h, RESIDENT_TASK_CAPABILITY), Value::Int(1)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Executors$RunnableAdapter (from Executors.callable): call()/run() execute the
/// wrapped runnable and return the captured result.
pub(crate) fn runnable_adapter(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let h = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    match name {
        "call" | "run" => {
            let r = vm.heap.get_field(h, "$runnable");
            if r.is_fn_value() { vm.apply(&r, vec![])?; }
            Ok(Some(Some(vm.heap.get_field(h, "$result"))))
        }
        _ => Ok(Some(Some(Value::Null))),
    }
}

/// Run and clear any queued ListenableFuture listeners (added via addListener
/// before completion). Each is a Runnable executed inline on completion.
pub(crate) fn fire_future_listeners(vm: &mut Vm, recv: u32) -> Result<(), VmError> {
    let listeners = match vm.heap.get_field(recv, "$listeners") {
        Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(),
        _ => return Ok(()),
    };
    if listeners.is_empty() {
        return Ok(());
    }
    // Clear first so a listener that re-enters can't re-run the same list.
    vm.heap.put_field(recv, "$listeners", Value::Null);
    for l in listeners {
        if l.is_fn_value() {
            vm.apply(&l, vec![])?;
        }
    }
    Ok(())
}

/// Build a completed CompletableFuture carrying `$result` (or `$error`).
pub(crate) fn make_cf(vm: &mut Vm, result: Value, error: Option<Value>) -> Value {
    let f = vm.heap.alloc("java/util/concurrent/CompletableFuture");
    vm.heap.put_field(f, "$result", result);
    if let Some(e) = error { vm.heap.put_field(f, "$error", e); }
    vm.heap.put_field(f, "$done", Value::Int(1)); // a factory/derived future is complete
    Value::Ref(f)
}

/// CompletableFuture continuation: build a downstream future for `kind`. If the
/// source is already complete, run the continuation now; otherwise register it
/// to fire when the source completes (the chunk pipeline registers thenApply on
/// futures that the generation task completes later, so eager evaluation
/// produced null results — "chunk futures were null").
/// `RAVA_LANE_ASYNC_CHAIN` (default OFF): offer an `*Async` continuation whose source is
/// already COMPLETE to the deferral lane instead of running it inline.
///
/// This is the seam `[PC-DEFER] offered=7 | DEFERRED=7` named. `gilfree_cf::defer` has one
/// caller — the `supplyAsync` arm — while MC's reload is built from `thenApplyAsync` and
/// friends, all of which land in `cf_chain` and run inline (`cf_run_cb`'s own comment:
/// "continuation actions always run INLINE today — there is no resident routing for them").
/// So the parallelisable work of the 18 s reload phase passes the only mechanism that could
/// defer it without ever being offered.
///
/// Only `apply`, and only on a NON-exceptional complete source: `accept`/`run` return void,
/// which the region's publication path has no value to complete the downstream with, and
/// `compose`'s function BUILDS the reload graph rather than computing a value — exempting
/// that one from the drain broke datapack loading once already. An exceptional source must
/// keep the inline propagation it has.
pub(crate) fn lane_async_chain_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LANE_ASYNC_CHAIN").is_some())
}

pub(crate) fn cf_chain(vm: &mut Vm, src: u32, kind: &str, f: Value, other: Value) -> Result<Value, VmError> {
    cf_chain_async(vm, src, kind, f, other, false)
}

pub(crate) fn cf_chain_async(
    vm: &mut Vm,
    src: u32,
    kind: &str,
    f: Value,
    other: Value,
    is_async: bool,
) -> Result<Value, VmError> {
    // A parked body is "not started", not "not done" — force before deciding which branch below
    // applies, or the continuation is registered against a future that will never complete.
    crate::reflect::force_parked(vm, src)?;
    crate::reflect::force_parked_value(vm, &other)?;
    let down = vm.heap.alloc("java/util/concurrent/CompletableFuture");
    if matches!(vm.heap.get_field(src, "$done"), Value::Int(1)) {
        #[cfg(feature = "next-gen-gc")]
        if is_async
            && kind == "apply"
            && lane_async_chain_enabled()
            && matches!(vm.heap.get_field(src, "$error"), Value::Null)
        {
            let v = vm.heap.get_field(src, "$result");
            if crate::gilfree_cf::defer_with_args(vm, &f, &Value::Ref(down), vec![v]) {
                return Ok(Value::Ref(down));
            }
        }
        // `cf_run_cb` runs the user action INLINE; for a deep one (e.g. a thenCompose
        // whose function builds the whole reload graph) that allocates and triggers a GC
        // while `down` is reachable ONLY from this Rust local → it would be swept and its
        // slot reused (handing the caller a wrong-typed object). Protect it across the call.
        run_cb_protected(vm, src, kind, &f, &other, down)?;
    } else {
        cf_add_cb_async(vm, src, kind, f, other, down, is_async);
    }
    Ok(Value::Ref(down))
}

/// `cf_run_cb` with `down` kept GC-reachable across the (possibly allocation-heavy,
/// GC-triggering) inline user action. See [`Vm::gc_protect`].
pub(crate) fn run_cb_protected(vm: &mut Vm, src: u32, kind: &str, f: &Value, other: &Value, down: u32) -> Result<(), VmError> {
    #[cfg(feature = "next-gen-gc")]
    crate::vm::thread_context::push_root(&mut vm.gc_protect, Value::Ref(down));
    let r = cf_run_cb(vm, src, kind, f, other, down);
    #[cfg(feature = "next-gen-gc")]
    {
        let _ = crate::vm::thread_context::pop_root(&mut vm.gc_protect);
    }
    r
}

/// `applyToEither`/`acceptEither`/`runAfterEither`: complete `down` from WHICHEVER of
/// `a`/`b` finishes first (applying `f` per `kind`). Wall #15: the old handler chained
/// only on `a` and dropped `b`, so `Util.sequenceFailFast` — `fallibleSequence(...)
/// .applyToEither(result, identity)`, whose fail-fast path completes `result` (= `b`) —
/// never propagated a failed listener, and `Util.blockUntilDone` hot-spun on the never-
/// completing reload future (the worldgen "softlock"). Register on BOTH; `cf_complete`
/// is idempotent so the loser's later fire is a no-op.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn cf_either(vm: &mut Vm, a: u32, b: u32, kind: &str, f: Value) -> Result<Value, VmError> {
    let down = vm.heap.alloc("java/util/concurrent/CompletableFuture");
    if matches!(vm.heap.get_field(a, "$done"), Value::Int(1)) {
        run_cb_protected(vm, a, kind, &f, &Value::Null, down)?;
    } else if b != 0 && matches!(vm.heap.get_field(b, "$done"), Value::Int(1)) {
        run_cb_protected(vm, b, kind, &f, &Value::Null, down)?;
    } else {
        cf_add_cb(vm, a, kind, f.clone(), Value::Null, down);
        if b != 0 { cf_add_cb(vm, b, kind, f, Value::Null, down); }
    }
    Ok(Value::Ref(down))
}

/// Append a callback record to `src.$cbs`.
pub(crate) fn cf_add_cb(vm: &mut Vm, src: u32, kind: &str, f: Value, other: Value, down: u32) {
    cf_add_cb_async(vm, src, kind, f, other, down, false)
}

/// `cf_add_cb`, remembering whether the registering call was an `*Async` variant.
///
/// A fan-out chains onto INCOMPLETE futures by construction, so `cf_chain`'s complete-source
/// branch is the rare one and the body almost always runs later, inline, from `cf_fire`.
/// Whether it MAY run off the calling thread is decided at REGISTRATION (`thenApply` vs
/// `thenApplyAsync`) and is not recoverable at firing time — so it is recorded here.
pub(crate) fn cf_add_cb_async(
    vm: &mut Vm,
    src: u32,
    kind: &str,
    f: Value,
    other: Value,
    down: u32,
    is_async: bool,
) {
    let cb = vm.heap.alloc("$CFCallback");
    if is_async {
        vm.heap.put_field(cb, "$async", Value::Int(1));
    }
    vm.heap.put_field(cb, "$k", Value::Str(kind.into()));
    vm.heap.put_field(cb, "$f", f);
    vm.heap.put_field(cb, "$o", other);
    vm.heap.put_field(cb, "$d", Value::Ref(down));
    let mut list = match vm.heap.get_field(src, "$cbs") {
        Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(), _ => Vec::new(),
    };
    list.push(Value::Ref(cb));
    let arr = vm.heap.alloc_values("Ljava/lang/Object;", list);
    vm.heap.put_field(src, "$cbs", Value::Array(arr));
    // TRANSITIVE OBSERVATION. `src` may be a future the off-GIL lane still owes a result
    // for; `down` then completes only when the batch runs, so a join on `down` has to be
    // able to force it. Recording the edge here is what lets a chaining call return
    // without draining (see `gilfree_cf::observes_deferred`).
    #[cfg(feature = "next-gen-gc")]
    if crate::gilfree_cf::lane_linked(vm, src) {
        crate::gilfree_cf::note_dependent(vm, down);
    }
}
