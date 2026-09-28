//! Atomic arrays, adders and the executor statics.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

pub(crate) fn atomic_array(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    if name == "<init>" {
        let arr = match args.get(1) {
            Some(Value::Int(n)) => vm.heap.alloc_array("Ljava/lang/Object;", (*n).max(0) as usize),
            Some(Value::Array(src)) => { let vals = vm.heap.array_values(*src).unwrap_or_default(); vm.heap.alloc_values("Ljava/lang/Object;", vals) }
            _ => vm.heap.alloc_array("Ljava/lang/Object;", 0),
        };
        vm.heap.put_field(recv, "$arr", Value::Array(arr));
        return Ok(Some(None));
    }
    let arr = match vm.heap.get_field(recv, "$arr") { Value::Array(a) => a, _ => return Ok(Some(Some(Value::Null))) };
    let idx = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
    let cur = vm.heap.array_get(arr, idx).unwrap_or(Value::Null);
    let ret = match name {
        "length" => Value::Int(vm.heap.array_len(arr).unwrap_or(0) as i32),
        "get" => cur,
        "set" | "lazySet" => { vm.heap.array_set(arr, idx, args.get(2).cloned().unwrap_or(Value::Null)); return Ok(Some(None)); }
        "getAndSet" => { vm.heap.array_set(arr, idx, args.get(2).cloned().unwrap_or(Value::Null)); cur }
        "compareAndSet" | "weakCompareAndSet" => {
            let exp = args.get(2).cloned().unwrap_or(Value::Null);
            let new = args.get(3).cloned().unwrap_or(Value::Null);
            if val_eq(&cur, &exp) { vm.heap.array_set(arr, idx, new); Value::Int(1) } else { Value::Int(0) }
        }
        // compareAndExchange(i, expect, new): set if cur==expect, always return
        // the value that was there (the "witness"). Level.getChunk's last-chunk
        // AtomicReferenceArray cache uses this.
        "compareAndExchange" | "compareAndExchangeAcquire" | "compareAndExchangeRelease" => {
            let exp = args.get(2).cloned().unwrap_or(Value::Null);
            let new = args.get(3).cloned().unwrap_or(Value::Null);
            if val_eq(&cur, &exp) { vm.heap.array_set(arr, idx, new); }
            cur
        }
        "getAndUpdate" | "updateAndGet" => {
            let f = args.get(2).cloned().unwrap_or(Value::Null);
            let v = vm.apply(&f, vec![cur.clone()])?.unwrap_or(Value::Null);
            vm.heap.array_set(arr, idx, v.clone());
            if name == "updateAndGet" { v } else { cur }
        }
        "incrementAndGet" => { let v = Value::Int(cur.as_i32() + 1); vm.heap.array_set(arr, idx, v.clone()); v }
        "getAndIncrement" => { vm.heap.array_set(arr, idx, Value::Int(cur.as_i32() + 1)); cur }
        "addAndGet" => { let v = Value::Int(cur.as_i32() + args.get(2).map(Value::as_i32).unwrap_or(0)); vm.heap.array_set(arr, idx, v.clone()); v }
        "getAndAdd" => { vm.heap.array_set(arr, idx, Value::Int(cur.as_i32() + args.get(2).map(Value::as_i32).unwrap_or(0))); cur }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

// --- java.util.concurrent.atomic (single-threaded) --------------------------

/// Storage kind of a scalar atomic's `$val` cell; `AtomicBoolean` folds into
/// `Int` (its cell stores an Int 0/1 via the same `num()` path as Integer).
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum AtomicStorage { Int, Long, Ref, Double }

/// Exact receiver→storage table — atomic names appear ONLY here. Census
/// license: bench/mc-cp.txt, 40 jars / 29,760 effective classes / 0 parse
/// errors (2026-09-17, git bc908a7e); no classpath jar shadows the
/// java/util/concurrent/atomic package — the needle-matched jctools queues
/// (`AtomicQueueUtil`, `AtomicReferenceArrayQueue`) are impostors.
/// ⚠ Re-run the census on any classpath/jar change.
pub(crate) fn scalar_atomic_storage(owner: &str) -> AtomicStorage {
    match owner {
        "java/util/concurrent/atomic/AtomicLong" => AtomicStorage::Long,
        "java/util/concurrent/atomic/AtomicReference" | "java/util/concurrent/atomic/AtomicStampedReference" | "java/util/concurrent/atomic/AtomicMarkableReference" => AtomicStorage::Ref,
        _ => AtomicStorage::Int, // Integer/Boolean + unknown/impostors: historic default
    }
}

/// `DoubleAdder` is the only double cell; `LongAdder` and unknowns keep Long.
pub(crate) fn adder_storage(owner: &str) -> AtomicStorage {
    match owner {
        "java/util/concurrent/atomic/DoubleAdder" => AtomicStorage::Double,
        _ => AtomicStorage::Long,
    }
}

/// java.util.concurrent.atomic.LongAdder / DoubleAdder — striped counters that
/// accumulate into a single `$val` cell here (the striping is a contention
/// optimisation the single-threaded engine doesn't need). Paper's
/// CachedSizeConcurrentLinkedQueue tracks queue size with a LongAdder and reads
/// it via intValue() in the post-Done tick loop.
pub(crate) fn adder_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    let is_double = matches!(adder_storage(vm.heap.class_of(recv).unwrap_or("")), AtomicStorage::Double);
    let zero = if is_double { Value::Double(0.0) } else { Value::Long(0) };
    let cur = vm.heap.get_field_syn(recv, crate::heap::Syn::Val);
    let cur_l = cur.as_i64();
    let cur_d = cur.as_f64();
    let ret = match name {
        "<init>" | "reset" => { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, zero); return Ok(Some(None)); }
        "add" => {
            let v = if is_double {
                Value::Double(cur_d + args.get(1).map(Value::as_f64).unwrap_or(0.0))
            } else {
                Value::Long(cur_l + args.get(1).map(Value::as_i64).unwrap_or(0))
            };
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v);
            return Ok(Some(None));
        }
        "increment" => { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, Value::Long(cur_l + 1)); return Ok(Some(None)); }
        "decrement" => { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, Value::Long(cur_l - 1)); return Ok(Some(None)); }
        "sum" => if is_double { Value::Double(cur_d) } else { Value::Long(cur_l) },
        "sumThenReset" => {
            let v = if is_double { Value::Double(cur_d) } else { Value::Long(cur_l) };
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, zero);
            v
        }
        "longValue" => Value::Long(cur_l),
        "intValue" => Value::Int(cur_l as i32),
        "doubleValue" => Value::Double(cur_d),
        "floatValue" => Value::Float(cur_d as f32),
        "toString" => Value::Str(if is_double { cur_d.to_string() } else { cur_l.to_string() }.into()),
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

// --- java.util.concurrent ---------------------------------------------------

pub(crate) fn executors_static(vm: &mut Vm, name: &str, _args: &[Value]) -> Handled {
    if name.starts_with("new") {
        let executor = vm.heap.alloc("java/util/concurrent/ExecutorService");
        // Capability proof, not a class-name optimization gate. Executors created
        // by this native factory have resident scheduling semantics; unknown
        // executor-like objects remain on the legacy inline path (fail closed).
        vm.heap
            .put_field(executor, RESIDENT_TASK_CAPABILITY, Value::Int(1));
        return Ok(Some(Some(Value::Ref(executor))));
    }
    match name {
        "defaultThreadFactory" | "privilegedThreadFactory" => {
            Ok(Some(Some(Value::Ref(vm.heap.alloc("java/util/concurrent/ThreadFactory")))))
        }
        // callable(Runnable[, result]) -> a Callable adapter whose call() runs the
        // runnable and returns the result. Returning the RAW Runnable broke MC's
        // main-thread task queue: it invokes call() on the stored Callable, and a
        // bare Runnable (PlayerConnection$2) has no call() — crashing the player's
        // packet task right after join.
        "callable" => {
            let a = vm.heap.alloc("java/util/concurrent/Executors$RunnableAdapter");
            vm.heap.put_field(a, "$runnable", _args.first().cloned().unwrap_or(Value::Null));
            vm.heap.put_field(a, "$result", _args.get(1).cloned().unwrap_or(Value::Null));
            Ok(Some(Some(Value::Ref(a))))
        }
        _ => Ok(None),
    }
}

pub(crate) fn complete_queued_task(
    vm: &mut Vm,
    future: Option<Value>,
    preset: Option<Value>,
    outcome: Result<Result<Option<Value>, VmError>, Box<dyn std::any::Any + Send>>,
) {
    let Some(Value::Ref(future)) = future else {
        if let Ok(Err(error)) = outcome {
            eprintln!("[EXECUTOR] uncaught guest task error: {error:?}");
        } else if outcome.is_err() {
            eprintln!("[EXECUTOR] guest task panicked");
        }
        return;
    };
    let (result, error) = match outcome {
        Ok(Ok(result)) => (preset.unwrap_or_else(|| result.unwrap_or(Value::Null)), None),
        Ok(Err(VmError::Throw(error))) => (Value::Null, Some(error)),
        Ok(Err(other)) => {
            let message = format!("executor task failed: {other:?}");
            (
                Value::Null,
                Some(vm.make_exception("java/lang/InternalError", &message)),
            )
        }
        Err(_) => (
            Value::Null,
            Some(vm.make_exception(
                "java/lang/InternalError",
                "executor task panicked",
            )),
        ),
    };
    if let Err(error) = cf_complete(vm, future, result, error) {
        eprintln!("[EXECUTOR] completion callback failed: {error:?}");
    }
    if let Err(error) = fire_future_listeners(vm, future) {
        eprintln!("[EXECUTOR] future listener failed: {error:?}");
    }
}

/// Fire every callback registered on the now-completed `src`.
pub(crate) fn cf_fire(vm: &mut Vm, src: u32) -> Result<(), VmError> {
    let cbs = match vm.heap.get_field(src, "$cbs") {
        Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(), _ => return Ok(()),
    };
    vm.heap.put_field(src, "$cbs", Value::Null);
    // The `$cbs` array is detached (nulled above) so the not-yet-fired callbacks AND
    // their downstream futures (`$d`) now live ONLY in this Rust Vec — not a GC root.
    // A callback's inline action can allocate + trigger a GC, which would sweep the
    // remaining callbacks/downstreams and corrupt the chain. Keep them reachable.
    #[cfg(feature = "next-gen-gc")]
    let protect_base = {
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        crate::vm::thread_context::extend_roots(&mut vm.gc_protect, cbs.iter().cloned());
        base
    };
    let mut ran = Ok(());
    for cb in &cbs {
        if let Value::Ref(cbh) = cb {
            let cbh = *cbh;
            let k = match vm.heap.get_field(cbh, "$k") { Value::Str(s) => s.to_string(), _ => continue };
            let f = vm.heap.get_field(cbh, "$f");
            let o = vm.heap.get_field(cbh, "$o");
            let d = match vm.heap.get_field(cbh, "$d") { Value::Ref(d) => d, _ => continue };
            // ⭐**THE HOOK THE LANE WAS MISSING.** A body registered by an `*Async` variant
            // may run on any thread by the `CompletableFuture` contract, and at THIS point
            // its source is complete, so its argument is a known value — structurally
            // identical to the `supplyAsync` supplier that was the only thing the lane was
            // ever offered. `apply` only: `accept`/`run` are void (the region's publication
            // has no value to complete the downstream with) and `compose` builds the reload
            // graph rather than computing a value. An exceptional source keeps its inline
            // propagation. Gated by `RAVA_LANE_ASYNC_CHAIN`, default OFF.
            #[cfg(feature = "next-gen-gc")]
            {
                let is_async = matches!(vm.heap.get_field(cbh, "$async"), Value::Int(1));
                if is_async
                    && k == "apply"
                    && crate::reflect::lane_async_chain_enabled()
                    && matches!(vm.heap.get_field(src, "$error"), Value::Null)
                {
                    let v = vm.heap.get_field(src, "$result");
                    if crate::gilfree_cf::defer_with_args(vm, &f, &Value::Ref(d), vec![v]) {
                        continue;
                    }
                }
            }
            if let Err(e) = cf_run_cb(vm, src, &k, &f, &o, d) { ran = Err(e); break; }
        }
    }
    #[cfg(feature = "next-gen-gc")]
    crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, protect_base);
    ran
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-driven pin of the exact rows and the fail-closed defaults.
    /// jctools' `AtomicReferenceArrayQueue` carries BOTH historic tokens
    /// ("/atomic/Atomic" + "Reference") yet is a queue — the substring form
    /// would have handed it the Ref cell; "" is the dead-receiver (None
    /// class) arm every consumer passes.
    #[test]
    fn storage_tables_exact_and_fail_closed() {
        let scalar: &[(&str, AtomicStorage)] = &[
            ("java/util/concurrent/atomic/AtomicLong", AtomicStorage::Long),
            ("java/util/concurrent/atomic/AtomicReference", AtomicStorage::Ref),
            ("java/util/concurrent/atomic/AtomicStampedReference", AtomicStorage::Ref),
            ("java/util/concurrent/atomic/AtomicMarkableReference", AtomicStorage::Ref),
            ("java/util/concurrent/atomic/AtomicInteger", AtomicStorage::Int),
            ("java/util/concurrent/atomic/AtomicBoolean", AtomicStorage::Int),
            ("io/netty/util/internal/shaded/org/jctools/queues/atomic/AtomicReferenceArrayQueue", AtomicStorage::Int),
            ("com/example/MyAtomicLongReference", AtomicStorage::Int),
            ("", AtomicStorage::Int),
        ];
        for (owner, want) in scalar {
            assert_eq!(&scalar_atomic_storage(owner), want, "{owner}");
        }
        let adders: &[(&str, AtomicStorage)] = &[
            ("java/util/concurrent/atomic/DoubleAdder", AtomicStorage::Double),
            ("java/util/concurrent/atomic/LongAdder", AtomicStorage::Long),
            ("com/example/DoubleAdderLike", AtomicStorage::Long),
            ("", AtomicStorage::Long),
        ];
        for (owner, want) in adders {
            assert_eq!(&adder_storage(owner), want, "{owner}");
        }
    }
}
