//! Building a stream and driving its lazily appended stages.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::stream::*;

/// Build a Stream from a Spliterator. Our synthetic spliterators carry $elems;
/// a real one (e.g. log4j's ServiceLoaderSpliterator) is drained by repeatedly
/// invoking its `tryAdvance(Consumer)` with a collecting consumer.
pub(crate) fn stream_from_spliterator(vm: &mut Vm, sp: Option<&Value>) -> Result<Value, VmError> {
    let h = match sp {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(make_stream(vm, Vec::new())),
    };
    if matches!(vm.heap.get_field_syn(h, crate::heap::Syn::Elems), Value::Array(_)) {
        let elems = elems_of(vm, sp);
        return Ok(make_stream(vm, elems));
    }
    // Generic: drive tryAdvance into a collecting consumer.
    let sink = vm.heap.alloc("java/util/ArrayList");
    let empty = vm.heap.alloc_values(OBJ, Vec::new());
    vm.heap.put_field_syn(sink, crate::heap::Syn::Elems, Value::Array(empty));
    let consumer = vm.heap.alloc("java/util/function/Consumer");
    vm.heap.put_field_syn(consumer, crate::heap::Syn::FnKind, Value::Str("collect".into()));
    vm.heap.put_field(consumer, "$sink", Value::Ref(sink));

    let class = vm.heap.class_of(h).unwrap_or("").to_string();
    let mut guard = 0u32;
    while guard < 5_000_000 {
        guard += 1;
        match vm.resolve_by_args(&class, "tryAdvance", "Ljava/util/function/Consumer;")? {
            Some((decl, desc, _)) => {
                let r = vm.invoke(decl, "tryAdvance", &desc, vec![Value::Ref(h), Value::Ref(consumer)])?;
                if r.and_then(|v| v.as_int()).unwrap_or(0) == 0 {
                    break;
                }
            }
            None => break, // no tryAdvance bytecode — give up with what we have
        }
    }
    let elems = elems_of(vm, Some(&Value::Ref(sink)));
    Ok(make_stream(vm, elems))
}

// --- stream operations ------------------------------------------------------

/// Append a deferred op onto `recv` (eager `$elems` stream OR an existing lazy `$ops`
/// stream), yielding a new lazy stream that SHARES the immutable source array. No
/// per-element work runs here — that is the whole point.
pub(crate) fn lazy_append(vm: &mut Vm, recv: u32, kind: i32, func: Option<Value>) -> Value {
    let fnv = func.unwrap_or(Value::Null);
    let (src_arr, mut ops) = match vm.heap.get_field(recv, "$ops") {
        Value::Array(ops_arr) => {
            let src = match vm.heap.get_field(recv, "$src") {
                Value::Array(a) => a,
                _ => vm.heap.alloc_values(OBJ, Vec::new()),
            };
            (src, vm.heap.array_values(ops_arr).unwrap_or_default())
        }
        _ => {
            // Eager receiver: its already-materialised elements become the source.
            let src = match vm.heap.get_field_syn(recv, crate::heap::Syn::Elems) {
                // An overriding class may hold spare capacity, and the trailing slots are not
                // elements — re-pack rather than stream them.
                Value::Array(a) if vm.heap.elems_len(recv, a) == vm.heap.array_len(a).unwrap_or(0) => a,
                Value::Array(a) => {
                    let e = vm.heap.elems_values(recv, a);
                    vm.heap.alloc_values(OBJ, e)
                }
                _ => {
                    let e = vm.iterate_object(recv).unwrap_or_default();
                    vm.heap.alloc_values(OBJ, e)
                }
            };
            (src, Vec::new())
        }
    };
    ops.push(Value::Int(kind));
    ops.push(fnv);
    let ops_arr = vm.heap.alloc_values(OBJ, ops);
    // Root both backing arrays across the stream-object alloc: they live only in local
    // handles here, and alloc() may run a moving collection that would otherwise reclaim
    // them before put_field roots them in the new object's fields.
    let _gs = gc_protect_array(vm, src_arr);
    let _go = gc_protect_array(vm, ops_arr);
    let h = vm.heap.alloc("java/util/stream/Stream");
    vm.heap.put_field(h, "$src", Value::Array(src_arr));
    vm.heap.put_field(h, "$ops", Value::Array(ops_arr));
    Value::Ref(h)
}

/// Run a lazy stream's deferred op chain over its source in ONE fused pass.
/// GC discipline mirrors the eager filter: the in-flight element rides in a rooted
/// 1-slot heap array (`cur`) and is RE-READ after every guest apply (which can move the
/// arena); op fns are re-read fresh from the rooted `$ops` array; `src`/`ops`/`cur`/`out`
/// are all GC roots for the whole pass.
pub(crate) fn fuse_materialize(vm: &mut Vm, h: u32) -> Result<Vec<Value>, VmError> {
    let _gph = gc_protect_value(vm, Value::Ref(h)); // roots reachable $src + $ops
    let src = match vm.heap.get_field(h, "$src") {
        Value::Array(a) => a,
        _ => return Ok(Vec::new()),
    };
    let ops = match vm.heap.get_field(h, "$ops") {
        Value::Array(a) => a,
        _ => return Ok(Vec::new()),
    };
    let n = vm.heap.array_len(src).unwrap_or(0);
    let nops = vm.heap.array_len(ops).unwrap_or(0) / 2;
    let cur = vm.heap.alloc_values(OBJ, vec![Value::Null]);
    let _gpc = gc_protect_array(vm, cur);
    let out = vm.heap.alloc_values(OBJ, Vec::new());
    let _gpo = gc_protect_array(vm, out);
    for i in 0..n {
        let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
        vm.heap.array_set(cur, 0, e);
        let mut dropped = false;
        for j in 0..nops {
            let kind = vm.heap.array_get(ops, 2 * j).and_then(|v| v.as_int()).unwrap_or(LAZY_MAP);
            let fnv = vm.heap.array_get(ops, 2 * j + 1).unwrap_or(Value::Null);
            let c = vm.heap.array_get(cur, 0).unwrap_or(Value::Null);
            match kind {
                LAZY_FILTER => {
                    if !truthy(&vm.apply(&fnv, vec![c])?) {
                        dropped = true;
                        break;
                    }
                }
                LAZY_PEEK => {
                    vm.apply(&fnv, vec![c])?;
                }
                _ => {
                    let r = vm.apply(&fnv, vec![c])?.unwrap_or(Value::Null);
                    vm.heap.array_set(cur, 0, r);
                }
            }
        }
        if !dropped {
            let keep = vm.heap.array_get(cur, 0).unwrap_or(Value::Null);
            vm.heap.array_push(out, keep);
        }
        stream_maybe_gc(vm);
    }
    Ok(vm.heap.array_values(out).unwrap_or_default())
}

pub(crate) fn stream_op(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // Lazy fast-path: a linear intermediate op just records a deferred op (no walk).
    if lazy_streams_enabled() {
        if let Some(kind) = lazy_defer_kind(name) {
            if let Some(Value::Ref(recv)) = args.first() {
                let recv = *recv;
                return Ok(Some(Some(lazy_append(vm, recv, kind, args.get(1).cloned()))));
            }
        }
    }
    // A GENERATED STREAM HAS NO ELEMENTS UNTIL SOMETHING BOUNDS IT. `Stream.iterate`
    // and `Stream.generate` record their function on the receiver instead of
    // materialising, because they are infinite; `limit` is what turns them into values.
    // Any other operation on an unbounded generator would run forever on a real JVM,
    // so this reports the limitation rather than silently returning the empty stream
    // the `$elems` walk below would otherwise produce — which would be a WRONG ANSWER,
    // and the quiet kind.
    if let Some(Value::Ref(recv)) = args.first() {
        let recv = *recv;
        if let Value::Str(kind) = vm.heap.get_field(recv, "$genkind") {
            let kind = kind.to_string();
            if name != "limit" {
                let e = vm.make_exception(
                    "java/lang/UnsupportedOperationException",
                    &format!("{name} on an unbounded Stream.{kind} — bound it with limit(n) first"),
                );
                return Err(VmError::Throw(e));
            }
            let n = args.get(1).and_then(Value::as_long).unwrap_or(0).max(0) as usize;
            if n > GENERATOR_CAP {
                let e = vm.make_exception(
                    "java/lang/UnsupportedOperationException",
                    &format!("limit({n}) on an unbounded Stream.{kind} exceeds this engine's cap"),
                );
                return Err(VmError::Throw(e));
            }
            let f = vm.heap.get_field(recv, "$genfn");
            let mut out = Vec::with_capacity(n);
            if kind == "generate" {
                for _ in 0..n {
                    out.push(vm.apply(&f, Vec::new())?.unwrap_or(Value::Null));
                }
            } else {
                let mut cur = vm.heap.get_field(recv, "$genseed");
                for _ in 0..n {
                    out.push(cur.clone());
                    cur = vm.apply(&f, vec![cur])?.unwrap_or(Value::Null);
                }
            }
            return Ok(Some(Some(make_stream(vm, out))));
        }
    }
    let elems = stream_elems(vm, args.first())?;
    let func = args.get(1).cloned();
    let ret = match name {
        "map" | "mapToObj" | "mapToInt" | "mapToLong" | "mapToDouble" => {
            // STREAM-MAP CENSUS (`RAVA_STREAM_MAP_STAT`, default OFF, inert).
            //
            // The fork probes match a classic `iterator()` + hasNext/next loop, so a stream
            // pipeline is invisible to them: a 38,357-line RAVA_FORK_TRACE contains ZERO mentions
            // of makeShapes / getShapeForEachState / populateNeighbours, while initCache (a real
            // iterator loop) appears 1943 times. That absence was read as "compiled, so the
            // detector cannot see it"; the actual reason is that MC builds shapes with
            // `.stream().map(..).collect(..)` and the shape never matched.
            //
            // THIS loop is ours, which is what makes it interesting: forking it needs no bytecode
            // analysis of the caller, only a decision about the mapper. So count the population
            // first - how many map calls, over how many elements, with which mapper - because the
            // fork is only worth building for pipelines whose element count justifies it.
            // Counts only; no behaviour change.
            stream_map_stat(elems.len());
            let out = with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_pipe(vm, elems, |vm, e, out| {
                    let f = rooted_fn(vm, fs);
                    out.push(vm.apply(&f, vec![e])?.unwrap_or(Value::Null));
                    Ok(())
                })
            })?;
            make_stream(vm, out)
        }
        "boxed" | "asLongStream" | "asDoubleStream" | "sequential" | "parallel"
        | "unordered" | "onClose" => make_stream(vm, elems),
        // Our streams are eager and hold no resources; close() is a void no-op.
        "close" => return Ok(Some(None)),
        "filter" => {
            // The kept element is pushed AFTER the predicate ran guest code (which can
            // move the arena) — push the RE-READ element, not the pre-apply copy. `f`
            // (the predicate) is a moving arena-lambda too → re-read it from gc_protect.
            let out = vm.heap.alloc_values(OBJ, Vec::new());
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each_idx(vm, elems, |vm, src, i| {
                    let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
                    let f = rooted_fn(vm, fs);
                    if truthy(&vm.apply(&f, vec![e])?) {
                        let fresh = vm.heap.array_get(src, i).unwrap_or(Value::Null);
                        vm.heap.array_push(out, fresh);
                    }
                    Ok(std::ops::ControlFlow::Continue(()))
                })
            })?;
            make_stream(vm, vm.heap.array_values(out).unwrap_or_default())
        }
        "peek" => {
            // Same re-read rule as filter: the pass-through element is pushed after
            // the consumer's guest code ran.
            let out = vm.heap.alloc_values(OBJ, Vec::new());
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each_idx(vm, elems, |vm, src, i| {
                    let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
                    let f = rooted_fn(vm, fs);
                    vm.apply(&f, vec![e])?;
                    let fresh = vm.heap.array_get(src, i).unwrap_or(Value::Null);
                    vm.heap.array_push(out, fresh);
                    Ok(std::ops::ControlFlow::Continue(()))
                })
            })?;
            make_stream(vm, vm.heap.array_values(out).unwrap_or_default())
        }
        "limit" => {
            let n = func.as_ref().and_then(Value::as_long).unwrap_or(0) as usize;
            make_stream(vm, elems.into_iter().take(n).collect())
        }
        "skip" => {
            let n = func.as_ref().and_then(Value::as_long).unwrap_or(0) as usize;
            make_stream(vm, elems.into_iter().skip(n).collect())
        }
        "distinct" => {
            let mut seen = Vec::new();
            let mut out = Vec::new();
            for e in elems {
                let k = e.display();
                if !seen.contains(&k) {
                    seen.push(k);
                    out.push(e);
                }
            }
            make_stream(vm, out)
        }
        "sorted" => {
            let mut out = elems;
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_STREAM_SORTED, out.len()); // [CMP-CALLER] entry mark
            sort_values(vm, &mut out, func.as_ref())?;
            make_stream(vm, out)
        }
        // mapMulti(BiConsumer<T, Consumer<R>>): lambda emits 0..N via a sink
        // consumer it is HANDED — not a Function returning a Stream. Drive a
        // collecting consumer and pass it as the 2nd arg per element.
        "mapMulti" | "mapMultiToInt" | "mapMultiToLong" | "mapMultiToDouble" => {
            let sink = vm.heap.alloc("java/util/ArrayList");
            let empty = vm.heap.alloc_values(OBJ, Vec::new());
            vm.heap.put_field_syn(sink, crate::heap::Syn::Elems, Value::Array(empty));
            let consumer = vm.heap.alloc("java/util/function/Consumer");
            vm.heap.put_field_syn(consumer, crate::heap::Syn::FnKind, Value::Str("collect".into()));
            vm.heap.put_field(consumer, "$sink", Value::Ref(sink));
            // Root the sink + consumer across the sweep — they live only in Rust locals
            // here, and rooted_each's mid-sweep GC would otherwise reclaim them (losing the
            // collected output); the INPUT elements are rooted inside rooted_each.
            let _gpc = gc_protect_value(vm, Value::Ref(consumer));
            let _gps = gc_protect_value(vm, Value::Ref(sink));
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each(vm, elems, |vm, e| {
                    let f = rooted_fn(vm, fs);
                    vm.apply(&f, vec![e, Value::Ref(consumer)])?;
                    Ok(std::ops::ControlFlow::Continue(()))
                })
            })?;
            let out = elems_of(vm, Some(&Value::Ref(sink)));
            make_stream(vm, out)
        }
        "flatMap" => {
            let n_in = elems.len();
            let out = with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_pipe(vm, elems, |vm, e, out| {
                    let f = rooted_fn(vm, fs);
                    let sub = vm.apply(&f, vec![e])?.unwrap_or(Value::Null);
                    out.extend(elems_of(vm, Some(&sub)));
                    Ok(())
                })
            })?;
            let _ = n_in;
            make_stream(vm, out)
        }

        // terminal
        "forEach" | "forEachOrdered" => {
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each(vm, elems, |vm, e| {
                    let f = rooted_fn(vm, fs);
                    vm.apply(&f, vec![e])?;
                    Ok(std::ops::ControlFlow::Continue(()))
                })
            })?;
            return Ok(Some(None));
        }
        "count" => Value::Long(elems.len() as i64),
        // `summaryStatistics()` — one pass, five numbers, and the JDK's exact `toString`
        // shape because guests log it. The element type is inferred the same way `sum`
        // below infers it: the pipeline does not track a primitive kind.
        //
        // Empty is not an error: the JDK reports count 0, sum 0, average 0 and the
        // extremes as the type's MAX/MIN — deliberately inverted so that folding more
        // elements in always narrows them.
        "summaryStatistics" => {
            let has_float = elems.iter().any(|v| matches!(v, Value::Double(_) | Value::Float(_)));
            let has_long = elems.iter().any(|v| matches!(v, Value::Long(_)));
            let n = elems.len() as i64;
            let h = vm.heap.alloc(if has_float {
                "java/util/DoubleSummaryStatistics"
            } else if has_long {
                "java/util/LongSummaryStatistics"
            } else {
                "java/util/IntSummaryStatistics"
            });
            let avg = if n == 0 { 0.0 } else { elems.iter().map(|v| v.as_f64()).sum::<f64>() / n as f64 };
            vm.heap.put_field(h, "$count", Value::Long(n));
            vm.heap.put_field(h, "$average", Value::Double(avg));
            if has_float {
                let mn = elems.iter().map(|v| v.as_f64()).fold(f64::INFINITY, f64::min);
                let mx = elems.iter().map(|v| v.as_f64()).fold(f64::NEG_INFINITY, f64::max);
                vm.heap.put_field(h, "$sum", Value::Double(elems.iter().map(|v| v.as_f64()).sum()));
                vm.heap.put_field(h, "$min", Value::Double(mn));
                vm.heap.put_field(h, "$max", Value::Double(mx));
            } else if has_long {
                let vals: Vec<i64> = elems.iter().map(|v| v.as_long().unwrap_or(0)).collect();
                vm.heap.put_field(h, "$sum", Value::Long(vals.iter().sum()));
                vm.heap.put_field(h, "$min", Value::Long(vals.iter().copied().min().unwrap_or(i64::MAX)));
                vm.heap.put_field(h, "$max", Value::Long(vals.iter().copied().max().unwrap_or(i64::MIN)));
            } else {
                let vals: Vec<i32> = elems.iter().map(|v| v.as_int().unwrap_or(0)).collect();
                vm.heap.put_field(h, "$sum", Value::Long(vals.iter().map(|&x| x as i64).sum()));
                vm.heap.put_field(h, "$min", Value::Int(vals.iter().copied().min().unwrap_or(i32::MAX)));
                vm.heap.put_field(h, "$max", Value::Int(vals.iter().copied().max().unwrap_or(i32::MIN)));
            }
            Value::Ref(h)
        }
        "sum" => {
            // IntStream.sum()→int, LongStream.sum()→long, DoubleStream.sum()→double.
            // The pipeline does not track the primitive element type, so infer it
            // from the (homogeneous) elements: any float/double ⇒ double, else any
            // long ⇒ long, else int. Previously this always used as_int(), which
            // returns None for Long/Double elements → a silent 0 for every
            // LongStream/DoubleStream sum (e.g. mapToLong(...).sum()).
            let has_float = elems
                .iter()
                .any(|v| matches!(v, Value::Double(_) | Value::Float(_)));
            let has_long = elems.iter().any(|v| matches!(v, Value::Long(_)));
            if has_float {
                Value::Double(elems.iter().map(|v| v.as_f64()).sum())
            } else if has_long {
                Value::Long(elems.iter().map(|v| v.as_long().unwrap_or(0)).sum())
            } else {
                Value::Int(elems.iter().map(|v| v.as_int().unwrap_or(0)).sum())
            }
        }
        "toArray" => Value::Array(vm.heap.alloc_values(OBJ, elems)),
        "toList" => make_list(vm, elems),
        "iterator" => {
            let arr = vm.heap.alloc_values(OBJ, elems);
            let it = vm.heap.alloc("java/util/Iterator");
            vm.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
            vm.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
            Value::Ref(it)
        }
        "collect" => {
            // 3-arg mutable collect(supplier, accumulator, combiner) vs collect(Collector).
            if args.len() >= 4 {
                let supplier = func.unwrap_or(Value::Null);
                let container = vm.apply(&supplier, vec![])?.unwrap_or(Value::Null);
                // Root BOTH the (possibly arena) container and the input elements in
                // heap slots so the allocating accumulator can't strand the container
                // or the unprocessed elements in unrooted Rust locals. The accumulator
                // itself may be a moving arena-lambda → re-read it from gc_protect.
                let cslot = vm.heap.alloc_values(OBJ, vec![container]);
                let _gp = gc_protect_array(vm, cslot);
                with_rooted_fn(vm, args.get(2).cloned().unwrap_or(Value::Null), |vm, fs| {
                    rooted_each(vm, elems, |vm, e| {
                        let accumulator = rooted_fn(vm, fs);
                        let c = vm.heap.array_get(cslot, 0).unwrap_or(Value::Null);
                        vm.apply(&accumulator, vec![c, e])?;
                        Ok(std::ops::ControlFlow::Continue(()))
                    })
                })?;
                vm.heap.array_get(cslot, 0).unwrap_or(Value::Null)
            } else {
                collect(vm, elems, func.as_ref())?
            }
        }
        "findFirst" | "findAny" => make_optional(vm, elems.into_iter().next()),
        "max" | "min" => {
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_STREAM_MINMAX, elems.len()); // [CMP-CALLER] entry mark
            // `best` rides in a 1-slot rooted heap array — `compare` runs the
            // comparator, which may allocate and move the held value. The comparator
            // itself may be a moving arena-lambda → re-read it from gc_protect.
            let acc = vm.heap.alloc_values(OBJ, vec![Value::Null]);
            let _gp = gc_protect_array(vm, acc);
            let mut has = false;
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each(vm, elems, |vm, e| {
                    if !has {
                        has = true;
                        vm.heap.array_set(acc, 0, e);
                    } else {
                        let cmp = rooted_fn(vm, fs);
                        let b = vm.heap.array_get(acc, 0).unwrap_or(Value::Null);
                        let r = vm.compare(&cmp, &b, &e)?;
                        if (name == "max" && r < 0) || (name == "min" && r > 0) {
                            vm.heap.array_set(acc, 0, e);
                        }
                    }
                    Ok(std::ops::ControlFlow::Continue(()))
                })
            })?;
            make_optional(vm, if has { vm.heap.array_get(acc, 0) } else { None })
        }
        "reduce" => {
            // reduce(BinaryOperator) -> Optional ; reduce(identity, BinaryOperator) -> value.
            // The accumulator rides in a 1-slot rooted heap array across each apply.
            if func.as_ref().is_some_and(|f| vm.value_is_lambda(f)) {
                let acc = vm.heap.alloc_values(OBJ, vec![Value::Null]);
                let _gp = gc_protect_array(vm, acc);
                let mut has = false;
                with_rooted_fn(vm, func.unwrap(), |vm, fs| {
                    rooted_each(vm, elems, |vm, e| {
                        if !has {
                            has = true;
                            vm.heap.array_set(acc, 0, e);
                        } else {
                            let f = rooted_fn(vm, fs);
                            let a = vm.heap.array_get(acc, 0).unwrap_or(Value::Null);
                            let r = vm.apply(&f, vec![a, e])?.unwrap_or(Value::Null);
                            vm.heap.array_set(acc, 0, r);
                        }
                        Ok(std::ops::ControlFlow::Continue(()))
                    })
                })?;
                make_optional(vm, if has { vm.heap.array_get(acc, 0) } else { None })
            } else {
                let acc = vm.heap.alloc_values(OBJ, vec![func.unwrap_or(Value::Null)]); // identity
                let _gp = gc_protect_array(vm, acc);
                with_rooted_fn(vm, args.get(2).cloned().unwrap_or(Value::Null), |vm, fs| {
                    rooted_each(vm, elems, |vm, e| {
                        let f = rooted_fn(vm, fs);
                        let a = vm.heap.array_get(acc, 0).unwrap_or(Value::Null);
                        let r = vm.apply(&f, vec![a, e])?.unwrap_or(Value::Null);
                        vm.heap.array_set(acc, 0, r);
                        Ok(std::ops::ControlFlow::Continue(()))
                    })
                })?;
                vm.heap.array_get(acc, 0).unwrap_or(Value::Null)
            }
        }
        "anyMatch" => {
            let mut hit = false;
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each(vm, elems, |vm, e| {
                    let f = rooted_fn(vm, fs);
                    if truthy(&vm.apply(&f, vec![e])?) {
                        hit = true;
                        Ok(std::ops::ControlFlow::Break(()))
                    } else {
                        Ok(std::ops::ControlFlow::Continue(()))
                    }
                })
            })?;
            Value::Int(hit as i32)
        }
        "allMatch" | "noneMatch" => {
            let mut any = false;
            let mut all = true;
            with_rooted_fn(vm, func.unwrap_or(Value::Null), |vm, fs| {
                rooted_each(vm, elems, |vm, e| {
                    let f = rooted_fn(vm, fs);
                    if truthy(&vm.apply(&f, vec![e])?) { any = true; } else { all = false; }
                    Ok(std::ops::ControlFlow::Continue(()))
                })
            })?;
            let r = if name == "allMatch" { all } else { !any };
            Value::Int(r as i32)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
