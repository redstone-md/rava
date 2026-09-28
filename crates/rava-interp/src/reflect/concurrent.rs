//! Executors, futures, atomics and thread locals.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

pub(crate) fn comparator(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // A comparator is a heap object tagged with $cmp (kind); Vm::compare reads it.
    let make = |vm: &mut Vm, kind: &str| -> u32 {
        let h = vm.heap.alloc("java/util/Comparator");
        vm.heap.put_field(h, "$cmp", Value::Str(kind.into()));
        h
    };
    match name {
        // static factories
        "naturalOrder" => Ok(Some(Some(Value::Ref(make(vm, "natural"))))),
        "reverseOrder" => Ok(Some(Some(Value::Ref(make(vm, "reverse"))))),
        "comparing" | "comparingInt" | "comparingLong" | "comparingDouble" => {
            let h = make(vm, "key");
            if let Some(f) = args.first().cloned() { vm.heap.put_field(h, "$keyfn", f); }
            Ok(Some(Some(Value::Ref(h))))
        }
        // instance ops (receiver = args[0])
        "compare" => {
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_BRIDGE, 0); // [CMP-CALLER] entry mark
            let r = vm.compare(&args.first().cloned().unwrap_or(Value::Null),
                &args.get(1).cloned().unwrap_or(Value::Null),
                &args.get(2).cloned().unwrap_or(Value::Null))?;
            Ok(Some(Some(Value::Int(r))))
        }
        // `Comparator.nullsFirst(cmp)` / `nullsLast(cmp)` — a wrapper that orders null
        // against everything else and DELEGATES the rest. The wrapped comparator may be
        // null itself, in which case the JDK treats two non-nulls as equal; `$inner`
        // absent gives exactly that, since `Vm::compare` on a `Value::Null` comparator
        // falls back to natural order — so the absent case stores a marker instead.
        "nullsFirst" | "nullsLast" => {
            let h = make(vm, if name == "nullsFirst" { "nullsfirst" } else { "nullslast" });
            match args.first().cloned() {
                Some(Value::Null) | None => {
                    vm.heap.put_field(h, "$nocmp", Value::Int(1));
                }
                Some(inner) => {
                    vm.heap.put_field(h, "$inner", inner);
                }
            }
            Ok(Some(Some(Value::Ref(h))))
        }
        "reversed" => {
            let h = make(vm, "reversed");
            if let Some(inner) = args.first().cloned() { vm.heap.put_field(h, "$inner", inner); }
            Ok(Some(Some(Value::Ref(h))))
        }
        "thenComparing" | "thenComparingInt" | "thenComparingLong" | "thenComparingDouble" => {
            let h = make(vm, "then");
            if let Some(first) = args.first().cloned() { vm.heap.put_field(h, "$first", first); }
            if let Some(second) = args.get(1).cloned() {
                // thenComparing has two 1-arg overloads: (Comparator other) chains
                // the comparator directly; (Function keyExtractor) — and the
                // thenComparingInt/Long/Double forms — wrap a key fn in a
                // key-comparator. Distinguish by whether the arg is itself a
                // comparator (it would be applied as a function otherwise →
                // "apply on Comparator (no SAM)").
                let is_cmp = name == "thenComparing" && value_is_comparator(vm, &second);
                let inner = if is_cmp {
                    second
                } else {
                    let c = vm.heap.alloc("java/util/Comparator");
                    vm.heap.put_field(c, "$cmp", Value::Str("key".into()));
                    vm.heap.put_field(c, "$keyfn", second);
                    Value::Ref(c)
                };
                vm.heap.put_field(h, "$second", inner);
            }
            Ok(Some(Some(Value::Ref(h))))
        }
        _ => Ok(None),
    }
}

// --- java.util.EnumSet (backed by an ArrayList of constants) ----------------

pub(crate) fn atomic_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    // ⭐**TWO SUBSTRING QUESTIONS ON THE RECEIVER'S CLASS NAME, PER ATOMIC OPERATION.**
    // `contains("AtomicLong")` / `contains("Reference")` are exactly the shape the
    // census killed on `Thread.start` (a "Reference" token matches jctools'
    // `AtomicReferenceArrayQueue`, an "AtomicLong" token matches any name that
    // merely embeds it) — and the answers are two `bool`s the exact table
    // [`scalar_atomic_storage`] answers without allocating. `AtomicBoolean`
    // folds into `Int` (its cell stores Int 0/1 through the same `num()` path),
    // so the uninitialised-cell default below is unchanged.
    let storage = scalar_atomic_storage(vm.heap.class_of(recv).unwrap_or(""));
    let (is_long, is_ref) = match storage {
        AtomicStorage::Long => (true, false),
        AtomicStorage::Ref => (false, true),
        AtomicStorage::Int | AtomicStorage::Double => (false, false),
    };
    let num = |n: i64| -> Value { if is_long { Value::Long(n) } else { Value::Int(n as i32) } };
    let cur = vm.heap.get_field_syn(recv, crate::heap::Syn::Val);
    let cur = if matches!(cur, Value::Null) && !is_ref { num(0) } else { cur };

    let ret = match name {
        "<init>" => {
            let init = args.get(1).cloned().unwrap_or(if is_ref { Value::Null } else { num(0) });
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, init);
            return Ok(Some(None));
        }
        "get" | "getPlain" | "getAcquire" | "getOpaque" | "getAndSet" => {
            if name == "getAndSet" {
                vm.heap.put_field_syn(recv, crate::heap::Syn::Val, args.get(1).cloned().unwrap_or(Value::Null));
            }
            cur
        }
        "set" | "lazySet" | "setPlain" | "setRelease" | "setOpaque" => {
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, args.get(1).cloned().unwrap_or(Value::Null));
            return Ok(Some(None));
        }
        "compareAndSet" | "weakCompareAndSet" | "weakCompareAndSetPlain"
        | "weakCompareAndSetVolatile" | "weakCompareAndSetAcquire" | "weakCompareAndSetRelease" => {
            let expected = args.get(1).cloned().unwrap_or(Value::Null);
            let newv = args.get(2).cloned().unwrap_or(Value::Null);
            if val_eq(&cur, &expected) { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, newv); Value::Int(1) } else { Value::Int(0) }
        }
        "compareAndExchange" => {
            let expected = args.get(1).cloned().unwrap_or(Value::Null);
            let newv = args.get(2).cloned().unwrap_or(Value::Null);
            if val_eq(&cur, &expected) { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, newv); }
            cur
        }
        "incrementAndGet" => { let v = num(cur.as_i64() + 1); vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v.clone()); v }
        "decrementAndGet" => { let v = num(cur.as_i64() - 1); vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v.clone()); v }
        "getAndIncrement" => { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, num(cur.as_i64() + 1)); cur }
        "getAndDecrement" => { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, num(cur.as_i64() - 1)); cur }
        "addAndGet" => { let v = num(cur.as_i64() + args.get(1).map(Value::as_i64).unwrap_or(0)); vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v.clone()); v }
        "getAndAdd" => { vm.heap.put_field_syn(recv, crate::heap::Syn::Val, num(cur.as_i64() + args.get(1).map(Value::as_i64).unwrap_or(0))); cur }
        "updateAndGet" => {
            let f = args.get(1).cloned().unwrap_or(Value::Null);
            let v = vm.apply(&f, vec![cur])?.unwrap_or(Value::Null);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v.clone()); v
        }
        "getAndUpdate" => {
            let f = args.get(1).cloned().unwrap_or(Value::Null);
            let v = vm.apply(&f, vec![cur.clone()])?.unwrap_or(Value::Null);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v); cur
        }
        "accumulateAndGet" => {
            let f = args.get(2).cloned().unwrap_or(Value::Null);
            let v = vm.apply(&f, vec![cur, args.get(1).cloned().unwrap_or(Value::Null)])?.unwrap_or(Value::Null);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Val, v.clone()); v
        }
        "intValue" => Value::Int(cur.as_i32()),
        "longValue" => Value::Long(cur.as_i64()),
        "doubleValue" | "floatValue" => cur,
        "toString" => Value::Str(cur.display().into()),
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

pub(crate) fn executor_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // ⭐**A BY-NAME FIELD PROBE ON EVERY EXECUTOR CALL, FOR AN ANSWER TWO ARMS READ.**
    // `has_resident_task_capability` is `get_field(recv, "$ravaResidentTasks")` — a MISSING
    // synthetic field on every executor the guest itself built, and a by-name probe that misses
    // re-hashes the name through the interner. This tree has already measured one such probe on
    // a hot path at wall 57.7 s → 68.1 s (see `reflect::mod`'s parked-supplier note), and it was
    // being paid by `run`, `runAll`, `pollTask`, `shutdown`, `isShutdown`, `awaitTermination`,
    // the eight `set*` no-ops and both `schedule` forms — none of which look at it. **What grows
    // is executor calls**: the datapack reload drives the whole chunk/recipe pipeline through
    // ConsecutiveExecutor/TaskScheduler, so the drain and configuration arms dominate the
    // population. Was one probe per call, is one per `execute`/`submit`.
    let eager = if fast_reflect2() {
        None
    } else {
        Some(has_resident_task_capability(vm, args.first()))
    };
    match name {
        // ThreadPoolExecutor configuration — we run tasks inline (no real pool),
        // so these are no-ops / benign defaults (CraftBukkit's async scheduler
        // calls them while setting up).
        "allowCoreThreadTimeOut" | "setKeepAliveTime" | "setThreadFactory"
        | "setRejectedExecutionHandler" | "setCorePoolSize" | "setMaximumPoolSize"
        | "setRemoveOnCancelPolicy" | "setContinueExistingPeriodicTasksAfterShutdownPolicy"
        | "setExecuteExistingDelayedTasksAfterShutdownPolicy" => return Ok(Some(None)),
        "prestartAllCoreThreads" | "getActiveCount" | "getPoolSize"
        | "getCorePoolSize" => return Ok(Some(Some(Value::Int(0)))),
        "getMaximumPoolSize" => return Ok(Some(Some(Value::Int(1)))),
        "execute" => {
            let resident = match eager {
                Some(r) => r,
                None => has_resident_task_capability(vm, args.first()),
            };
            let task = args.get(1).cloned().unwrap_or(Value::Null);
            if resident && vm.enqueue_guest_task(task.clone(), None, None) {
                return Ok(Some(None));
            }
            let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::ExecutorExecute, &task);
            vm.apply(&task, vec![])?;
            Ok(Some(None))
        }
        "submit" => {
            let resident = match eager {
                Some(r) => r,
                None => has_resident_task_capability(vm, args.first()),
            };
            let task = args.get(1).cloned().unwrap_or(Value::Null);
            if resident {
                let future = make_pending_future(vm, "java/util/concurrent/FutureTask");
                let preset = args.get(2).cloned();
                if vm.enqueue_guest_task(task.clone(), Some(future.clone()), preset) {
                    return Ok(Some(Some(future)));
                }
            }
            let res = {
                let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::ExecutorSubmit, &task);
                vm.apply(&task, vec![])?.unwrap_or(Value::Null)
            };
            // submit(Runnable, result) returns the provided result.
            let res = args.get(2).cloned().unwrap_or(res);
            Ok(Some(Some(make_future(vm, res))))
        }
        "invokeAll" => {
            let tasks = collection_elems(vm, args.get(1));
            let mut futures = Vec::new();
            for t in tasks {
                let r = {
                    let _probe =
                        taskgraph_probe::inline_scope(vm, InlineSeam::ExecutorInvokeAll, &t);
                    vm.apply(&t, vec![])?.unwrap_or(Value::Null)
                };
                futures.push(make_future(vm, r));
            }
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", futures);
            let list = vm.heap.alloc("java/util/ArrayList");
            vm.heap.put_field_syn(list, crate::heap::Syn::Elems, Value::Array(arr));
            Ok(Some(Some(Value::Ref(list))))
        }
        // MC's ConsecutiveExecutor/TaskScheduler.schedule(task) — a SINGLE-arg
        // immediate task (the chunk-generation pipeline runs through this). Run
        // it inline so the task's CompletableFuture actually completes (queuing
        // it for an unpumped worker thread left chunk futures null). The task is
        // a Runnable / StrictQueue$RunnableWithPriority — apply() invokes run().
        "schedule" if args.len() == 2 => {
            let task = args.get(1).cloned().unwrap_or(Value::Null);
            let _probe = taskgraph_probe::inline_scope(vm, InlineSeam::ExecutorSchedule, &task);
            vm.apply(&task, vec![])?;
            Ok(Some(None))
        }
        // TaskScheduler.scheduleWithResult([priority,] Consumer<CompletableFuture>):
        // make the future, run the consumer inline (it completes the future),
        // return it. Backs the chunk-generation step futures.
        "scheduleWithResult" => {
            let f = make_cf(vm, Value::Null, None);
            let consumer = args.last().cloned().unwrap_or(Value::Null);
            let _probe =
                taskgraph_probe::inline_scope(vm, InlineSeam::ExecutorScheduleWithResult, &consumer);
            vm.apply(&consumer, vec![f.clone()])?;
            Ok(Some(Some(f)))
        }
        // Delayed/periodic tasks (ScheduledExecutorService: cmd, delay, unit). We
        // have no scheduler thread, and at boot these are watchdogs/keepalives/
        // flushers meant to fire LATER — running them inline now could (e.g. a
        // boot-timeout watchdog) abort startup. Return an unresolved scheduled
        // future and drop the task. (Needed periodic work resurfaces as a wall.)
        "schedule" | "scheduleAtFixedRate" | "scheduleWithFixedDelay" => {
            let f = vm.heap.alloc("java/util/concurrent/FutureTask");
            vm.heap.put_field(f, "$result", Value::Null);
            vm.heap.put_field(f, "$scheduled", Value::Int(1));
            Ok(Some(Some(Value::Ref(f))))
        }
        // Single-consumer queue executors: tasks ran inline at schedule(), so the
        // drain loop and managed-block waits are no-ops.
        "run" | "runAll" | "pollTask" | "dispatchTasks" | "runAllTasks" => Ok(Some(None)),
        "managedBlock" | "waitForTasks" | "driveUntil" | "driveOne" => Ok(Some(None)),
        "shutdownNow" => {
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
            let list = vm.heap.alloc("java/util/ArrayList");
            vm.heap.put_field_syn(list, crate::heap::Syn::Elems, Value::Array(arr));
            Ok(Some(Some(Value::Ref(list))))
        }
        "shutdown" | "close" => Ok(Some(None)),
        "awaitTermination" => Ok(Some(Some(Value::Int(1)))),
        "isShutdown" | "isTerminated" => Ok(Some(Some(Value::Int(1)))),
        _ => Ok(None),
    }
}

/// Compute the downstream value for one callback against the completed `src` and
/// complete `down` (or, for chaining kinds, wire `down` to a further source).
pub(crate) fn threadlocal(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    if name == "withInitial" {
        // static: the Supplier is the first (only) argument.
        let h = vm.heap.alloc("java/lang/ThreadLocal");
        if let Some(s) = args.first().cloned() {
            vm.heap.put_field(h, "$supplier", s);
        }
        return Ok(Some(Some(Value::Ref(h))));
    }
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Null))) };
    match name {
        "<init>" => Ok(Some(None)),
        // PER-THREAD ON A GIL-FREE WORKER (see crate::tl_worker). The synthetic-field model
        // below is a SHARED cell, which is only correct while one thread runs guest code; the
        // ring lane runs six. A worker keeps its value in its own table and never touches the
        // object, so the fields stay exactly what main published — and the non-gil-free path is
        // byte-identical because it never reaches these arms.
        "set" if crate::tl_worker::enabled() && crate::parallel::gilfree() => {
            crate::tl_worker::set(recv, args.get(1).cloned().unwrap_or(Value::Null));
            Ok(Some(None))
        }
        "remove" if crate::tl_worker::enabled() && crate::parallel::gilfree() => {
            crate::tl_worker::remove(recv);
            Ok(Some(None))
        }
        "get" if crate::tl_worker::enabled()
            && crate::parallel::gilfree()
            && crate::tl_worker::get(recv).is_some() =>
        {
            Ok(Some(crate::tl_worker::get(recv)))
        }
        "set" => { if let Some(v) = args.get(1).cloned() { vm.heap.put_field(recv, "$tlval", v); } vm.heap.put_field(recv, "$tlset", Value::Int(1)); Ok(Some(None)) }
        "remove" => { vm.heap.put_field(recv, "$tlset", Value::Int(0)); Ok(Some(None)) }
        "get" => {
            if vm.heap.get_field(recv, "$tlset").as_int().unwrap_or(0) != 0 {
                return Ok(Some(Some(vm.heap.get_field(recv, "$tlval"))));
            }
            // Compute initial: supplier, else an overridden initialValue(), else null.
            let sup = vm.heap.get_field(recv, "$supplier");
            let v = match sup {
                _ if sup.is_fn_value() => vm.apply(&sup, vec![])?.unwrap_or(Value::Null),
                _ => {
                    let actual = vm.heap.class_of(recv).unwrap_or("").to_string();
                    match vm.resolve_by_args(&actual, "initialValue", "")? {
                        Some((decl, desc, _)) => vm.invoke(decl, "initialValue", &desc, vec![Value::Ref(recv)])?.unwrap_or(Value::Null),
                        None => Value::Null,
                    }
                }
            };
            // A worker that had to COMPUTE the initial value publishes it to its own table, not
            // to the shared object — writing the fields here would hand every sibling this
            // thread's supplier result, which is the same shared-cell bug one level down.
            if crate::tl_worker::enabled() && crate::parallel::gilfree() {
                crate::tl_worker::set(recv, v.clone());
            } else {
                vm.heap.put_field(recv, "$tlval", v.clone());
                vm.heap.put_field(recv, "$tlset", Value::Int(1));
            }
            Ok(Some(Some(v)))
        }
        _ => Ok(None),
    }
}

// --- java.lang.Thread (single-threaded model: run synchronously) ------------

/// The EXACT provider set for the S1 `Thread.start` skip below: every Netty
/// class on the classpath that IS a `java/lang/Thread` subclass. Licensed by
/// the census at /tmp/netty_thread_subclass_census.json (2026-09-17, git
/// bc908a7e, 40 jars / 29,760 effective classes / 0 parse errors): exactly
/// THREE Netty Thread descendants exist — `FastThreadLocalThread` (what
/// `DefaultThreadFactory` allocates for the event loops this skip exists for)
/// plus two anonymous single-use helpers. `tcls` is the object's allocation
/// tag, and this classpath defines no classes dynamically and builds no
/// proxies, so the set is closed: a Netty name not in this table is NOT a
/// Thread subclass. ⚠ Whenever the classpath (bench/mc-cp.txt) or any Netty
/// jar changes, RE-RUN that census and re-derive this table — a new Netty
/// Thread subclass would otherwise be silently started, a removed one
/// pointlessly skipped.
const NETTY_THREAD_SUBCLASSES: [&str; 3] = [
    "io/netty/handler/codec/http/multipart/DeleteFileOnExitHook$1",
    "io/netty/util/concurrent/FastThreadLocalThread",
    "io/netty/util/internal/ThreadLocalRandom$1",
];

/// S1: skip `start` for exactly the census names above — NOT for every
/// `io/netty/` class (the old prefix form is gone). The runnable-class check
/// (S2) further below is a separate question and keeps its own prefix.
fn is_netty_thread_subclass(tcls: &str) -> bool {
    NETTY_THREAD_SUBCLASSES.contains(&tcls)
}

pub(crate) fn thread_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    match name {
        // Thread.getAllStackTraces() — an empty Thread->StackTraceElement[] map
        // (MC's crash-report builder calls it; we have no real thread registry).
        "getAllStackTraces" => return Ok(Some(Some(Value::Ref(vm.heap.alloc("java/util/HashMap"))))),
        "<init>" => {
            // Capture the Runnable argument (any heap object among the args).
            let runnable = args.iter().skip(1).find(|a| a.is_fn_value()).cloned();
            if let (Some(Value::Ref(t)), Some(r)) = (args.first(), runnable) {
                vm.heap.put_field(*t, "$runnable", r);
            }
            // Capture the thread name (the String arg, if any) for debugging.
            if let Some(Value::Ref(t)) = args.first() {
                if let Some(Value::Str(n)) = args.iter().skip(1).find(|a| matches!(a, Value::Str(_))) {
                    vm.heap.put_field_syn(*t, crate::heap::Syn::Name, Value::Str(n.clone()));
                }
            }
            Ok(Some(None))
        }
        "start" => {
            // `start` twice is `IllegalThreadStateException`, and it silently ran again.
            if let Some(Value::Ref(t)) = args.first() {
                if matches!(vm.heap.get_field(*t, "$started"), Value::Int(1)) {
                    let e = vm.make_exception("java/lang/IllegalThreadStateException", "");
                    return Err(VmError::Throw(e));
                }
                vm.heap.put_field(*t, "$started", Value::Int(1));
            }
            // The FIRST started thread (MC's "Server thread" — the whole boot+tick
            // loop) runs INLINE as the recursive main: currentThread() returns it
            // for the duration so MC's `currentThread()==primaryThread` async-guards
            // ("Asynchronous block remove!" etc.) pass. Any thread started WHILE it
            // runs is a daemon → a cooperative green thread (its run() advances one
            // slice each time the main thread yields in `sleep`), so an infinite
            // daemon loop (FileIOThread chunk-saver, Watchdog, Snooper) no longer
            // hangs the VM and no MC-class-specific skip list is needed.
            if let Some(Value::Ref(t)) = args.first() {
                let tcls = vm.heap.class_of(*t).unwrap_or("").to_string();
                let r = vm.heap.get_field(*t, "$runnable");
                // netty event-loop / task-runner threads run an infinite for(;;)
                // poll; with inEventLoop()=true netty runs those tasks inline on the
                // caller, so the poller thread is unnecessary — skip starting it.
                if is_netty_thread_subclass(&tcls) { return Ok(Some(None)); }
                if let Value::Ref(rh) = r {
                    if vm.heap.class_of(rh).unwrap_or("").starts_with("io/netty/") {
                        return Ok(Some(None));
                    }
                }
                // run() lives on the Runnable if given, else on the Thread subclass.
                let receiver = if matches!(r, Value::Null) { Value::Ref(*t) } else { r };
                if matches!(receiver, Value::Null) { return Ok(Some(None)); }
                // Daemon threads (and any thread started once the primary is already
                // running) become cooperative green threads; the first non-daemon
                // thread runs inline as the recursive main (the server thread).
                let is_daemon = matches!(vm.heap.get_field(*t, "$daemon"), Value::Int(1));
                // The server's "Server thread" (spin's `()->ref.get().runServer()`) is a
                // NON-daemon started AFTER the boot/"ServerMain" thread already runs
                // inline. It IS the server loop and must run — so a non-daemon nests
                // inline (recursive main) even when one is active; only DAEMONS become
                // cooperative green threads. (Was `is_daemon || inline_thread_active`,
                // which routed the Server thread to a green thread the inline main
                // never drove → runServer never executed.)
                if is_daemon {
                    // The real OS thread (the "Moonshot" multicore daemon) RACES the
                    // main thread on the moving arena heap during MinecraftServer.<init>
                    // (a wild write that corrupts live objects → boot crash). Keep all
                    // daemons cooperative green by default — correct, single-core. Opt
                    // back into the racy OS thread with RAVA_OS_THREAD once the GIL hand-
                    // off across the arena collector is made data-race-free.
                    if !vm.os_thread_spawned && std::env::var_os("RAVA_OS_THREAD").is_some() {
                        if let Value::Ref(rh) = receiver {
                            vm.os_thread_spawned = true;
                            vm.spawn_os_thread(rh, *t);
                            return Ok(Some(None));
                        }
                    }
                    vm.spawn_green_thread(receiver, *t);
                } else {
                    let prev = vm.main_thread;
                    vm.main_thread = Some(*t);
                    vm.inline_thread_active = true;
                    // TRAMPOLINE (checkpoint machine): stage run() as the pending call
                    // so the spawned main (MC's ServerMain — the ENTIRE boot) executes
                    // on the EXPLICIT frame stack, giving drive_stack its checkpoint
                    // boundaries, instead of recursing here. Synchronous either way
                    // (the trampoline runs the staged frame to completion before the
                    // caller resumes). main_thread/inline_thread_active deliberately
                    // stay set — this thread IS the main for its whole (process-long)
                    // life; the recursive arm restores them only at shutdown anyway.
                    if vm.trampoline {
                        if let Some(staged) = vm.stage_runnable(&receiver) {
                            vm.pending = Some(staged);
                            return Ok(Some(None));
                        }
                    }
                    let res = vm.apply(&receiver, vec![]);
                    vm.inline_thread_active = false;
                    vm.main_thread = prev;
                    // AN EXCEPTION IN A THREAD DOES NOT REACH THE STARTER. Java routes it
                    // to the thread's uncaught handler (or the group's, which prints it)
                    // and the starter continues. Propagating it here meant a failing
                    // background task killed whoever called `start` — the boot, in this
                    // engine, since a started thread runs inline.
                    if let Err(crate::vmframe::VmError::Throw(thrown)) = res {
                        let handler = vm.heap.get_field(*t, "$uncaught");
                        if handler.is_fn_value() {
                            let _ = vm.apply(&handler, vec![Value::Ref(*t), thrown]);
                        } else {
                            let name = match vm.heap.get_field_syn(*t, crate::heap::Syn::Name) {
                                Value::Str(n) => n.to_string(),
                                _ => "Thread-0".to_string(),
                            };
                            let text = vm.value_to_string(&thrown).unwrap_or_else(|_| thrown.display());
                            eprintln!("Exception in thread \"{name}\" {text}");
                        }
                    } else {
                        res?;
                    }
                }
            }
            Ok(Some(None))
        }
        // Direct run() call (not start()) = synchronous by contract: run inline.
        "run" => {
            if let Some(Value::Ref(t)) = args.first() {
                let r = vm.heap.get_field(*t, "$runnable");
                let receiver = if matches!(r, Value::Null) { Value::Ref(*t) } else { r };
                if !matches!(receiver, Value::Null) {
                    vm.apply(&receiver, vec![])?;
                }
            }
            Ok(Some(None))
        }
        // The MC main loop calls Thread.sleep once per tick to pace to 20 TPS. The
        // netty event-loop poll is neutralised, so piggy-back the network pump here:
        // accept new clients + read their bytes one iteration per tick.
        // Main (recursive) thread's pacing sleep: advance every cooperative green
        // thread one slice (daemons make progress exactly when main idle-waits on
        // them — e.g. FileIOThread's chunk-saver vs the `while(queued!=written)`
        // waiter), then pump the network. Green threads' own Thread.sleep is the
        // yield path in opcodes (trampoline), never this native.
        "sleep" => {
            // Same rendezvous as the network pump: the guest has given up the thread.
            super::drain_parked(vm)?;
            // A REAL daemon OS thread must NOT drive the green-thread scheduler or
            // the network pump: those mutate shared per-thread execution state
            // (`trampoline`/`yield_request`) that belongs to the MAIN recursive
            // thread. If the worker set `trampoline = true` (via
            // run_green_threads_slice) and a driven green thread then staged a
            // `yield_request`, the worker's own recursive `apply` would observe a
            // stray `RunExit::Yield` and panic ("recursive invoke must not yield").
            // Instead the worker just blocks for real: drop the GIL (so MAIN runs),
            // sleep a bounded slice, re-acquire. Bounded so a "wait forever" daemon
            // keeps polling its wake condition (~50 Hz) rather than parking dead.
            if crate::gil::is_worker() {
                let ms = match args.first() {
                    Some(Value::Long(m)) => *m,
                    Some(Value::Int(m)) => *m as i64,
                    _ => 0,
                };
                crate::gil::release();
                let slice = ms.clamp(0, 20) as u64;
                if slice > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(slice));
                } else {
                    std::thread::yield_now();
                }
                crate::gil::acquire();
                return Ok(Some(None));
            }
            // Green threads + network pump run UNDER the GIL (they touch the heap).
            vm.run_green_threads_slice();
            crate::net::pump_network(vm);
            // Drop the GIL so a real daemon OS thread can run, then re-acquire.
            // Skipped entirely when no real thread exists (boot stays identical).
            if crate::gil::has_real_threads() {
                crate::gil::release();
                std::thread::yield_now();
                crate::gil::acquire();
            }
            Ok(Some(None))
        }
        // Record the daemon flag: `start` routes daemon threads (FileIOThread,
        // Watchdog, Snooper, Infinisleeper, console handler — all setDaemon(true))
        // to the cooperative green-thread scheduler, and runs the first NON-daemon
        // thread (MC's "Server thread") inline as the recursive main. Generic — no
        // MC class names.
        "setDaemon" => {
            if let Some(Value::Ref(t)) = args.first() {
                let d = matches!(args.get(1), Some(Value::Int(1)));
                vm.heap.put_field(*t, "$daemon", Value::Int(d as i32));
            }
            Ok(Some(None))
        }
        "setUncaughtExceptionHandler" => {
            if let (Some(Value::Ref(t)), Some(h)) = (args.first(), args.get(1)) {
                vm.heap.put_field(*t, "$uncaught", h.clone());
            }
            Ok(Some(None))
        }
        "join" | "setContextClassLoader" | "setPriority" | "yield" | "onSpinWait" => Ok(Some(None)),
        "getStackTrace" => Ok(Some(Some(Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", Vec::new()))))),
        // A THREAD HAS ITS OWN NAME. This answered "main" for every thread, including
        // ones constructed with an explicit name, so any log line or diagnostic keyed on
        // the thread name reported the wrong one.
        "getName" => {
            let n = match args.first() {
                Some(Value::Ref(t)) => match vm.heap.get_field_syn(*t, crate::heap::Syn::Name) {
                    Value::Str(s) => s.to_string(),
                    _ => "main".to_string(),
                },
                _ => "main".to_string(),
            };
            Ok(Some(Some(Value::Str(n.into()))))
        }
        "setName" => {
            if let (Some(Value::Ref(t)), Some(v)) = (args.first(), args.get(1)) {
                let n = crate::natives::str_of(vm, Some(v)).unwrap_or_default();
                vm.heap.put_field_syn(*t, crate::heap::Syn::Name, Value::Str(n.into()));
            }
            Ok(Some(None))
        }
        "getId" | "threadId" => Ok(Some(Some(Value::Long(1)))),
        // THE CURRENT THREAD IS ALIVE. `isAlive` answered 0 for every thread including
        // the one asking, which is the one thing it can never be.
        //
        // ⚠A thread that has been started is TERMINATED here rather than RUNNABLE,
        // because `start` runs it inline or hands it to the green scheduler — the design
        // recorded at `start` above. `NEW` before, `TERMINATED` after, and the current
        // thread `RUNNABLE`: that is the whole of what this engine can honestly report.
        "isAlive" => {
            let alive = match args.first() {
                Some(Value::Ref(t)) => *t == vm.current_thread(),
                _ => false,
            };
            Ok(Some(Some(Value::Int(alive as i32))))
        }
        "getState" => {
            let st = match args.first() {
                Some(Value::Ref(t)) if *t == vm.current_thread() => "RUNNABLE",
                Some(Value::Ref(t)) if matches!(vm.heap.get_field(*t, "$started"), Value::Int(1)) => {
                    "TERMINATED"
                }
                _ => "NEW",
            };
            let h = vm.heap.alloc("java/lang/Thread$State");
            vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(st.into()));
            vm.heap.put_field(h, "$enumName", Value::Str(st.into()));
            Ok(Some(Some(Value::Ref(h))))
        }
        // The interrupt FLAG is per thread and `Thread.interrupted()` CLEARS it, while
        // `isInterrupted()` does not — code that polls the static form to decide whether
        // to stop saw a flag that never set and never cleared.
        "interrupt" => {
            if let Some(Value::Ref(t)) = args.first() {
                vm.heap.put_field(*t, "$interrupted", Value::Int(1));
            }
            Ok(Some(None))
        }
        "isInterrupted" => {
            let f = match args.first() {
                Some(Value::Ref(t)) => vm.heap.get_field(*t, "$interrupted").as_int().unwrap_or(0),
                _ => 0,
            };
            Ok(Some(Some(Value::Int(f))))
        }
        "interrupted" => {
            let t = vm.current_thread();
            let f = vm.heap.get_field(t, "$interrupted").as_int().unwrap_or(0);
            vm.heap.put_field(t, "$interrupted", Value::Int(0));
            Ok(Some(Some(Value::Int(f))))
        }
        "isDaemon" | "holdsLock" => Ok(Some(Some(Value::Int(0)))),
        "getPriority" => Ok(Some(Some(Value::Int(5)))),
        "getThreadGroup" | "getContextClassLoader2" => Ok(Some(Some(Value::Null))),
        "currentThread" => Ok(Some(Some(Value::Ref(vm.current_thread())))),
        "getContextClassLoader" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/lang/ClassLoader"))))),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S1 is licensed by EXACTLY the three census names — the census itself
    /// flagged two `io/netty/` Thread subclasses beyond `FastThreadLocalThread`,
    /// which is why a prefix can never license this set. Pin the table itself:
    /// a name edited, added or dropped here must fail the build, not silently
    /// change which threads get skipped.
    #[test]
    fn s1_table_is_exactly_the_three_census_thread_subclasses() {
        assert_eq!(
            NETTY_THREAD_SUBCLASSES,
            [
                "io/netty/handler/codec/http/multipart/DeleteFileOnExitHook$1",
                "io/netty/util/concurrent/FastThreadLocalThread",
                "io/netty/util/internal/ThreadLocalRandom$1",
            ]
        );
        for name in NETTY_THREAD_SUBCLASSES {
            assert!(is_netty_thread_subclass(name), "{name} must be skipped by S1");
        }
    }

    /// A Netty name that is NOT a Thread subclass must fall through S1 to the
    /// S2 runnable-target check below — event loops and task runners are
    /// Runnable targets, not Thread subclasses.
    #[test]
    fn s1_rejects_netty_names_outside_the_thread_subclass_set() {
        assert!(
            !is_netty_thread_subclass("io/netty/util/concurrent/SyntheticThread"),
            "an io/netty/ class that is not a Thread subclass must not match S1"
        );
    }

    /// Non-Netty Thread classes (the JDK's own `java/lang/Thread`, the census's
    /// ten non-Netty descendants such as JNA's CleanerThread) must run the
    /// daemon/inline machinery — matching one would silently drop the thread.
    #[test]
    fn s1_rejects_non_netty_thread_subclasses() {
        for name in ["java/lang/Thread", "com/sun/jna/internal/Cleaner$CleanerThread"] {
            assert!(!is_netty_thread_subclass(name), "{name} must not match S1");
        }
    }
}

// --- generic type signatures (JVMS 4.7.9.1) ----------------------------------
