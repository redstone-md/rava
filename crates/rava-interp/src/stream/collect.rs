//! Collectors: the collector object, its driver, and the inner reduction.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::stream::*;

pub(crate) fn sort_values(vm: &mut Vm, elems: &mut [Value], cmp: Option<&Value>) -> Result<(), VmError> {
    if let Some(done) = vm.sort_values_direct(elems, cmp) { return done; } // [CMP-CALLER] a handle-lambda comparator resolved once per sort, same merge schedule (kill switch RAVA_NO_CMPCALLER)
    let c = match cmp {
        Some(c) => c,
        None => {
            // Natural order. Compare longs as longs (as_int truncates), else text.
            elems.sort_by(|a, b| match (a.as_long(), b.as_long()) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ => a.display().cmp(&b.display()),
            });
            return Ok(());
        }
    };
    // Fast path: a `comparing*(keyFn)` comparator. Extract each element's key
    // ONCE (n applies) and sort on the cached keys, instead of re-running the
    // key-extraction bytecode on every comparison — the old O(n^2) insertion
    // sort did O(n^2) heavy comparator callbacks (the Climate RTree build, which
    // sorts ~1000 nodes 7x per level, dominated boot time because of this).
    if let Some((keyfn, rev)) = key_comparator(vm, c) {
        // Root the elements + extracted keys in HEAP `Object[]`s while `keyfn`
        // allocates (n applies); the sort itself runs no guest code, so a plain
        // forwarded snapshot is read back after extraction. (Wall #10.)
        let n = elems.len();
        let elemsarr = vm.heap.alloc_values(OBJ, elems.to_vec());
        let keysarr = vm.heap.alloc_values(OBJ, Vec::new());
        for i in 0..n {
            let e = vm.heap.array_get(elemsarr, i).unwrap_or(Value::Null);
            let k = vm.apply(&keyfn, vec![e])?.unwrap_or(Value::Null);
            vm.heap.array_push(keysarr, k);
        }
        let keys = vm.heap.array_values(keysarr).unwrap_or_default();
        let elems_now = vm.heap.array_values(elemsarr).unwrap_or_default();
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&a, &b| {
            let (ka, kb) = (&keys[a], &keys[b]);
            let o = match (ka.as_long(), kb.as_long()) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ if is_fp(ka) && is_fp(kb) =>
                    ka.as_f64().partial_cmp(&kb.as_f64()).unwrap_or(std::cmp::Ordering::Equal),
                _ => ka.display().cmp(&kb.display()),
            };
            if rev { o.reverse() } else { o }
        });
        for (slot, &i) in elems.iter_mut().zip(idx.iter()) { *slot = elems_now[i].clone(); }
        return Ok(());
    }
    // General comparator → O(n log n) stable bottom-up merge sort. The comparator
    // runs guest code that allocates, so the working set + scratch buffer live in
    // HEAP `Object[]`s (forwarded in place) rather than unrooted Rust `Vec`s; each
    // merge winner is RE-READ from the (forwarded) source after `compare`. (Wall #10.)
    let comparator_plan = crate::comparator_plan::ComparatorPlan::build(vm, c);
    // LAZY KEY MEMO (RAVA_SORT_KEY_MEMO): the same cache with the authoritative merge
    // schedule and short-circuiting kept — each clause key is memoised on first touch, so
    // the eager arm's unasked-for `(clauses − 1) × n` extractor applies are never run.
    if sort_key_memo_enabled() {
        if let Some(plan) = &comparator_plan {
            plan.sort_memoised(vm, elems)?;
            return Ok(());
        }
    }
    // KEY-CACHE (RAVA_SORT_KEYCACHE): for a composite (thenComparing) comparator, extract
    // each element's key TUPLE once (n × clause applies) and sort on the cached tuples
    // natively — instead of the O(n log n) heap-array merge sort that re-runs every clause's
    // extractor on every comparison (the ArrayList#sort self-time #1 boot sink). Same
    // key-caching pattern + heap-rooting as the single-key fast path above (proven bit-exact).
    if sort_keycache_enabled() {
        if let Some(plan) = &comparator_plan {
            let n = elems.len();
            if n > 1 {
                let nc = plan.clause_count();
                let elemsarr = vm.heap.alloc_values(OBJ, elems.to_vec());
                let keysarr = vm.heap.alloc_values(OBJ, Vec::new());
                for i in 0..n {
                    let e = vm.heap.array_get(elemsarr, i).unwrap_or(Value::Null);
                    for k in plan.extract_keys(vm, e)? {
                        vm.heap.array_push(keysarr, k);
                    }
                }
                let keys_flat = vm.heap.array_values(keysarr).unwrap_or_default();
                let elems_now = vm.heap.array_values(elemsarr).unwrap_or_default();
                let mut idx: Vec<usize> = (0..n).collect();
                idx.sort_by(|&a, &b| {
                    let ka = &keys_flat[a * nc..(a + 1) * nc];
                    let kb = &keys_flat[b * nc..(b + 1) * nc];
                    match plan.compare_keys(ka, kb) {
                        x if x < 0 => std::cmp::Ordering::Less,
                        0 => std::cmp::Ordering::Equal,
                        _ => std::cmp::Ordering::Greater,
                    }
                });
                for (slot, &i) in elems.iter_mut().zip(idx.iter()) {
                    *slot = elems_now[i].clone();
                }
            }
            return Ok(());
        }
    }
    let n = elems.len();
    if n > 1 {
        let mut src = vm.heap.alloc_values(OBJ, elems.to_vec());
        let mut dst = vm.heap.alloc_values(OBJ, elems.to_vec());
        let mut width = 1usize;
        while width < n {
            let mut i = 0usize;
            while i < n {
                let lo = i;
                let mid = (i + width).min(n);
                let hi = (i + 2 * width).min(n);
                let (mut a, mut b, mut k) = (lo, mid, lo);
                while a < mid && b < hi {
                    let va = vm.heap.array_get(src, a).unwrap_or(Value::Null);
                    let vb = vm.heap.array_get(src, b).unwrap_or(Value::Null);
                    let ordering = match &comparator_plan {
                        Some(plan) => plan.compare(vm, va, vb)?,
                        None => vm.compare(c, &va, &vb)?,
                    };
                    if ordering <= 0 {
                        let v = vm.heap.array_get(src, a).unwrap_or(Value::Null);
                        vm.heap.array_set(dst, k, v); a += 1;
                    } else {
                        let v = vm.heap.array_get(src, b).unwrap_or(Value::Null);
                        vm.heap.array_set(dst, k, v); b += 1;
                    }
                    k += 1;
                }
                while a < mid { let v = vm.heap.array_get(src, a).unwrap_or(Value::Null); vm.heap.array_set(dst, k, v); a += 1; k += 1; }
                while b < hi { let v = vm.heap.array_get(src, b).unwrap_or(Value::Null); vm.heap.array_set(dst, k, v); b += 1; k += 1; }
                i += 2 * width;
            }
            std::mem::swap(&mut src, &mut dst);
            width *= 2;
        }
        let sorted = vm.heap.array_values(src).unwrap_or_default();
        for (slot, v) in elems.iter_mut().zip(sorted) { *slot = v; }
    }
    Ok(())
}

pub(crate) fn collect_inner(vm: &mut Vm, elems: Vec<Value>, collector: Option<&Value>) -> Result<Value, VmError> {
    let kind = match collector {
        Some(Value::Ref(h)) => match vm.heap.get_field(*h, "$kind") {
            Value::Str(s) => s.to_string(),
            // No marker: a real Collector object. Drive it via supplier /
            // accumulator / finisher bytecode so arbitrary collectors (guava's
            // toImmutableMap/toImmutableList, JDK toUnmodifiable*, ...) work.
            _ => match drive_collector(vm, *h, &elems)? {
                Some(result) => return Ok(result),
                None => "list".to_string(),
            },
        },
        _ => "list".to_string(),
    };
    // THE REDUCING COLLECTORS. Each one used to return the element LIST, because the
    // factory had no kind for it and fell into the list catch-all.
    if matches!(kind.as_str(), "toSet" | "counting" | "summing" | "averaging" | "minmaxBy" | "mapping" | "flatMapping" | "filtering") {
        let ch = match collector { Some(Value::Ref(h)) => *h, _ => return Ok(make_list(vm, elems)) };
        match kind.as_str() {
            "toSet" => {
                let deduped = crate::stream::dedup_by_ekey(vm, elems);
                return Ok(make_set(vm, deduped));
            }
            "counting" => return Ok(Value::Long(elems.len() as i64)),
            "summing" | "averaging" => {
                // The mapper runs guest code per element, so re-read it each time and
                // walk the ROOTED source rather than a Rust Vec.
                let n = elems.len();
                let src = vm.heap.alloc_values(OBJ, elems);
                let mut acc = 0.0f64;
                for i in 0..n {
                    let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
                    let f = vm.heap.get_field(ch, "$keyfn");
                    acc += vm.apply(&f, vec![e])?.unwrap_or(Value::Null).as_f64();
                }
                if kind == "averaging" {
                    return Ok(Value::Double(if n == 0 { 0.0 } else { acc / n as f64 }));
                }
                return Ok(match vm.heap.get_field(ch, "$numkind") {
                    Value::Str(k) if k.as_ref() == "summingDouble" => Value::Double(acc),
                    Value::Str(k) if k.as_ref() == "summingLong" => Value::Long(acc as i64),
                    _ => Value::Int(acc as i32),
                });
            }
            "minmaxBy" => {
                let want_max = vm.heap.get_field(ch, "$max").as_int() == Some(1);
                let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_MINMAX_BY, elems.len()); // [CMP-CALLER] entry mark
                let mut best: Option<Value> = None;
                for e in elems.into_iter() {
                    best = Some(match best {
                        None => e,
                        Some(b) => {
                            let c = vm.heap.get_field(ch, "$cmp2");
                            let r = vm.compare(&c, &e, &b)?;
                            if (want_max && r > 0) || (!want_max && r < 0) { e } else { b }
                        }
                    });
                }
                return Ok(crate::stream::make_optional(vm, best));
            }
            // mapping/filtering/flatMapping: transform, then hand to the downstream.
            _ => {
                let n = elems.len();
                let src = vm.heap.alloc_values(OBJ, elems);
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
                    let f = vm.heap.get_field(ch, "$keyfn");
                    match kind.as_str() {
                        "filtering" => {
                            let keep = vm.apply(&f, vec![e.clone()])?.and_then(|v| v.as_int()).unwrap_or(0);
                            if keep != 0 { out.push(e); }
                        }
                        "flatMapping" => {
                            let sub = vm.apply(&f, vec![e])?.unwrap_or(Value::Null);
                            out.extend(crate::stream::stream_elems(vm, Some(&sub))?);
                        }
                        _ => out.push(vm.apply(&f, vec![e])?.unwrap_or(Value::Null)),
                    }
                }
                let down = vm.heap.get_field(ch, "$downstream");
                let down = if matches!(down, Value::Null) { None } else { Some(down) };
                return collect_inner(vm, out, down.as_ref());
            }
        }
    }
    // `partitioningBy(predicate[, downstream])` — a two-key map that ALWAYS has both
    // keys, even when a side is empty. It had no application arm at all, so it returned
    // the element list.
    if kind == "partitioningBy" {
        let ch = match collector { Some(Value::Ref(h)) => *h, _ => return Ok(make_list(vm, elems)) };
        let n = elems.len();
        let src = vm.heap.alloc_values(OBJ, elems);
        let (mut yes, mut no) = (Vec::new(), Vec::new());
        for i in 0..n {
            let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
            let f = vm.heap.get_field(ch, "$keyfn");
            let t = vm.apply(&f, vec![e.clone()])?.and_then(|v| v.as_int()).unwrap_or(0);
            if t != 0 { yes.push(e) } else { no.push(e) }
        }
        let down = vm.heap.get_field(ch, "$downstream");
        let down = if matches!(down, Value::Null) { None } else { Some(down) };
        let no_v = collect_inner(vm, no, down.as_ref())?;
        let yes_v = collect_inner(vm, yes, down.as_ref())?;
        let mapobj = vm.heap.alloc("java/util/LinkedHashMap");
        // false first, then true — the JDK's `partitioningBy` map prints in that order.
        for (k, v) in [(false, no_v), (true, yes_v)] {
            let kv = vm.boolean_obj(k);
            crate::map::dispatch(vm, "java/util/HashMap", "put", &[Value::Ref(mapobj), kv, v])?;
        }
        return Ok(Value::Ref(mapobj));
    }
    // toMap(keyFn, valFn[, mergeFn]) / groupingBy(classifier) → build a Map.
    if kind == "groupingBy" {
        let ch = match collector { Some(Value::Ref(h)) => *h, _ => return Ok(make_list(vm, elems)) };
        let downstream = vm.heap.get_field(ch, "$downstream");
        // Bucket by classifier key (first-seen order), then collect each bucket
        // through the downstream collector (default: toList). Every accumulator
        // here (the classifier `keyfn`) runs guest code that allocates, so the
        // unprocessed elements, the per-bucket element lists, and the collected
        // keys all live in HEAP `Object[]`s (collector roots forwarded in place) —
        // never in unrooted Rust `Vec`s. (Wall #10.)
        let mut order: Vec<String> = Vec::new();
        let mut buckets: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let keysarr = vm.heap.alloc_values(OBJ, Vec::new()); // rooted, parallel to `order`
        let kslot = vm.heap.alloc_values(OBJ, vec![Value::Null]); // root `k` across structural_key
        rooted_each_idx(vm, elems, |vm, src, i| {
            let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
            // Re-read the classifier from the collector's (GC-forwarded FK_OBJ) field
            // each iteration — an arena-lambda keyfn moves under the per-element GC.
            let keyfn = vm.heap.get_field(ch, "$keyfn");
            let k = vm.apply(&keyfn, vec![e])?.unwrap_or(Value::Null);
            vm.heap.array_set(kslot, 0, k);
            let sk = {
                let k = vm.heap.array_get(kslot, 0).unwrap_or(Value::Null);
                crate::map::structural_key(vm, &k)
            };
            let bucket = match buckets.get(&sk) {
                Some(&a) => a,
                None => {
                    let a = vm.heap.alloc_values(OBJ, Vec::new());
                    buckets.insert(sk.clone(), a);
                    order.push(sk);
                    let k = vm.heap.array_get(kslot, 0).unwrap_or(Value::Null);
                    vm.heap.array_push(keysarr, k);
                    a
                }
            };
            // The bucketed element is stored AFTER the classifier ran guest code —
            // push the RE-READ element, not the pre-apply copy.
            let fresh = vm.heap.array_get(src, i).unwrap_or(Value::Null);
            vm.heap.array_push(bucket, fresh);
            Ok(std::ops::ControlFlow::Continue(()))
        })?;
        let mapobj = vm.heap.alloc("java/util/HashMap");
        for (i, sk) in order.iter().enumerate() {
            let bucket = vm.heap.array_values(buckets[sk]).unwrap_or_default();
            let v = if matches!(downstream, Value::Ref(_)) {
                collect(vm, bucket, Some(&downstream))?
            } else {
                make_list(vm, bucket)
            };
            let k = vm.heap.array_get(keysarr, i).unwrap_or(Value::Null);
            crate::map::dispatch(vm, "java/util/HashMap", "put", &[Value::Ref(mapobj), k, v])?;
        }
        return Ok(Value::Ref(mapobj));
    }
    if kind == "toMap" {
        let ch = match collector { Some(Value::Ref(h)) => *h, _ => return Ok(make_list(vm, elems)) };
        let mapobj = vm.heap.alloc("java/util/HashMap");
        // `keyfn` runs before `valfn`; `valfn`'s allocation would strand the
        // already-computed `k` (and the unprocessed elements) in unrooted Rust
        // locals. Hold k/v in a 2-slot HEAP array (forwarded in place). (Wall #10.)
        let kv = vm.heap.alloc_values(OBJ, vec![Value::Null, Value::Null]);
        rooted_each_idx(vm, elems, |vm, src, i| {
            let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
            // Re-read key/val/merge fns from the collector's (GC-forwarded FK_OBJ)
            // fields each iteration — arena-lambda fns move under the per-element GC.
            let keyfn = vm.heap.get_field(ch, "$keyfn");
            let k = vm.apply(&keyfn, vec![e])?.unwrap_or(Value::Null);
            vm.heap.array_set(kv, 0, k);
            // valfn's element is RE-READ: keyfn's guest code may have moved the arena,
            // and the pre-keyfn copy would be stale.
            let e = vm.heap.array_get(src, i).unwrap_or(Value::Null);
            let valfn = vm.heap.get_field(ch, "$valfn");
            let v = vm.apply(&valfn, vec![e])?.unwrap_or(Value::Null);
            vm.heap.array_set(kv, 1, v);
            let mergefn = vm.heap.get_field(ch, "$mergefn");
            if mergefn.is_fn_value() {
                let k = vm.heap.array_get(kv, 0).unwrap_or(Value::Null);
                let prev = crate::map::dispatch(vm, "java/util/HashMap", "get", &[Value::Ref(mapobj), k])?
                    .flatten().unwrap_or(Value::Null);
                let v = vm.heap.array_get(kv, 1).unwrap_or(Value::Null);
                let merged = if matches!(prev, Value::Null) { v } else {
                    vm.apply(&mergefn, vec![prev, v])?.unwrap_or(Value::Null)
                };
                let k = vm.heap.array_get(kv, 0).unwrap_or(Value::Null);
                crate::map::dispatch(vm, "java/util/HashMap", "put", &[Value::Ref(mapobj), k, merged])?;
            } else {
                // NO MERGE FUNCTION MEANS DUPLICATES ARE AN ERROR. `Collectors.toMap`
                // without one throws `IllegalStateException` on a repeated key, and ours
                // silently overwrote — so a stream with a duplicate produced a SMALLER
                // map and no complaint, which is the quiet kind of wrong answer.
                let k = vm.heap.array_get(kv, 0).unwrap_or(Value::Null);
                let prev = crate::map::dispatch(vm, "java/util/HashMap", "get", &[Value::Ref(mapobj), k])?
                    .flatten().unwrap_or(Value::Null);
                let v = vm.heap.array_get(kv, 1).unwrap_or(Value::Null);
                if !matches!(prev, Value::Null) {
                    let k = vm.heap.array_get(kv, 0).unwrap_or(Value::Null);
                    // The JDK names the key and both values it refused to merge.
                    let ks = vm.value_to_string(&k).unwrap_or_else(|_| k.display());
                    let ps = vm.value_to_string(&prev).unwrap_or_else(|_| prev.display());
                    let vs = vm.value_to_string(&v).unwrap_or_else(|_| v.display());
                    let e = vm.make_exception(
                        "java/lang/IllegalStateException",
                        &format!("Duplicate key {ks} (attempted merging values {ps} and {vs})"),
                    );
                    return Err(VmError::Throw(e));
                }
                let k = vm.heap.array_get(kv, 0).unwrap_or(Value::Null);
                crate::map::dispatch(vm, "java/util/HashMap", "put", &[Value::Ref(mapobj), k, v])?;
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })?;
        return Ok(Value::Ref(mapobj));
    }
    // Custom Collector.of(supplier, accumulator, combiner, finisher?).
    if kind == "custom" {
        let ch = match collector { Some(Value::Ref(h)) => *h, _ => return Ok(make_list(vm, elems)) };
        let supplier = vm.heap.get_field(ch, "$supplier");
        let accum = vm.heap.get_field(ch, "$accum");
        let finisher = vm.heap.get_field(ch, "$finisher");
        let container = vm.apply(&supplier, vec![])?.unwrap_or(Value::Null);
        // Root the (possibly arena) container + unprocessed elements in heap slots:
        // each accumulator allocates, and a moving collection would otherwise strand
        // the container or the elements in unrooted Rust locals. (Wall #10.)
        let cslot = vm.heap.alloc_values(OBJ, vec![container]);
        let _gp = gc_protect_array(vm, cslot);
        with_rooted_fn(vm, accum, |vm, fs| {
            rooted_each(vm, elems, |vm, e| {
                let accum = rooted_fn(vm, fs);
                let c = vm.heap.array_get(cslot, 0).unwrap_or(Value::Null);
                vm.apply(&accum, vec![c, e])?;
                Ok(std::ops::ControlFlow::Continue(()))
            })
        })?;
        let container = vm.heap.array_get(cslot, 0).unwrap_or(Value::Null);
        // Apply the finisher (e.g. ImmutableMap.Builder::build) — a heap OR arena
        // functional. IDENTITY_FINISH collectors (no finisher) return the container.
        let has_finisher = match &finisher {
            Value::Ref(_) => true,
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(_) => true,
            _ => false,
        };
        return Ok(if has_finisher {
            vm.apply(&finisher, vec![container])?.unwrap_or(Value::Null)
        } else {
            container
        });
    }
    Ok(match kind.as_str() {
        "joining" => {
            let (sep, pre, suf) = match collector {
                Some(Value::Ref(h)) => (
                    str_field(vm, *h, "$sep"),
                    str_field(vm, *h, "$pre"),
                    str_field(vm, *h, "$suf"),
                ),
                _ => (String::new(), String::new(), String::new()),
            };
            let body = elems.iter().map(Value::display).collect::<Vec<_>>().join(&sep);
            Value::Str(format!("{pre}{body}{suf}").into())
        }
        _ => make_list(vm, elems),
    })
}

/// Drive a real `java.util.stream.Collector` (one without our `$kind` marker)
/// over `elems` by invoking its `supplier()` → container, `accumulator()` for
/// each element, then `finisher()`. Returns None if it isn't a Collector with
/// bytecode (so the caller falls back to toList).
pub(crate) fn drive_collector(vm: &mut Vm, ch: u32, elems: &[Value]) -> Result<Option<Value>, VmError> {
    let cls = vm.heap.class_of(ch).unwrap_or("").to_string();
    let supplier = match vm.resolve_method(&cls, "supplier", "()Ljava/util/function/Supplier;")? {
        Some(d) => vm.invoke(d, "supplier", "()Ljava/util/function/Supplier;", vec![Value::Ref(ch)])?.unwrap_or(Value::Null),
        None => return Ok(None),
    };
    let accumulator = match vm.resolve_method(&cls, "accumulator", "()Ljava/util/function/BiConsumer;")? {
        Some(d) => vm.invoke(d, "accumulator", "()Ljava/util/function/BiConsumer;", vec![Value::Ref(ch)])?.unwrap_or(Value::Null),
        None => return Ok(None),
    };
    let container = vm.apply(&supplier, Vec::new())?.unwrap_or(Value::Null);
    // Root the input elements AND the (possibly arena) container in heap slots: each
    // accumulator runs guest code that allocates (e.g. the Climate RTree's leaf
    // builder), and a moving collection would otherwise strand the unprocessed
    // elements or the container in unrooted Rust locals.
    let csrc = vm.heap.alloc_values(OBJ, elems.to_vec());
    let cslot = vm.heap.alloc_values(OBJ, vec![container]);
    let n = vm.heap.array_len(csrc).unwrap_or(0);
    // Root the accumulator (an arena-lambda BiConsumer — e.g. the Climate RTree leaf
    // builder — MOVES under each element's guest GC) and re-read the forwarded fn.
    with_rooted_fn(vm, accumulator, |vm, aslot| {
        for i in 0..n {
            let e = vm.heap.array_get(csrc, i).unwrap_or(Value::Null);
            let c = vm.heap.array_get(cslot, 0).unwrap_or(Value::Null);
            let accumulator = rooted_fn(vm, aslot);
            vm.apply(&accumulator, vec![c, e])?;
        }
        Ok::<(), VmError>(())
    })?;
    let container = vm.heap.array_get(cslot, 0).unwrap_or(Value::Null);
    // finisher() is optional in spirit but always present on the interface;
    // IDENTITY_FINISH collectors return the container unchanged.
    let result = match vm.resolve_method(&cls, "finisher", "()Ljava/util/function/Function;")? {
        Some(d) => {
            let fin = vm.invoke(d, "finisher", "()Ljava/util/function/Function;", vec![Value::Ref(ch)])?.unwrap_or(Value::Null);
            vm.apply(&fin, vec![container.clone()])?.unwrap_or(container)
        }
        None => container,
    };
    Ok(Some(result))
}

pub(crate) fn collector(vm: &mut Vm, name: &str, args: &[Value]) -> Value {
    let h = vm.heap.alloc("java/util/stream/Collector");
    match name {
        "joining" => {
            vm.heap.put_field(h, "$kind", Value::Str("joining".into()));
            let g = |i: usize| match args.get(i) { Some(Value::Str(s)) => s.to_string(), _ => String::new() };
            vm.heap.put_field(h, "$sep", Value::Str(g(0).into()));
            vm.heap.put_field(h, "$pre", Value::Str(g(1).into()));
            vm.heap.put_field(h, "$suf", Value::Str(g(2).into()));
        }
        // toMap(keyMapper, valueMapper [, mergeFunction]) — collect into a Map.
        "toMap" | "toUnmodifiableMap" | "toConcurrentMap" => {
            vm.heap.put_field(h, "$kind", Value::Str("toMap".into()));
            vm.heap.put_field(h, "$keyfn", args.first().cloned().unwrap_or(Value::Null));
            vm.heap.put_field(h, "$valfn", args.get(1).cloned().unwrap_or(Value::Null));
            if let Some(m) = args.get(2) { vm.heap.put_field(h, "$mergefn", m.clone()); }
        }
        // groupingBy(classifier [, mapFactory], downstream) — Map<K, downstream>.
        // The downstream collector (last Collector arg) is applied to each group;
        // absent → the default toList.
        "groupingBy" | "groupingByConcurrent" => {
            vm.heap.put_field(h, "$kind", Value::Str("groupingBy".into()));
            vm.heap.put_field(h, "$keyfn", args.first().cloned().unwrap_or(Value::Null));
            // The downstream is the last arg when more than just the classifier
            // was passed (a 3-arg form's middle arg is a map supplier we ignore).
            if let Some(d @ Value::Ref(_)) = args.last() {
                if args.len() > 1 { vm.heap.put_field(h, "$downstream", d.clone()); }
            }
        }
        // A SET DEDUPES. `toSet` fell into the list catch-all below, so
        // `Collectors.toSet()` over five elements with one repeat reported FIVE.
        "toSet" | "toUnmodifiableSet" => {
            vm.heap.put_field(h, "$kind", Value::Str("toSet".into()));
        }
        // The numeric reductions, all of which returned the LIST unchanged.
        "counting" => { vm.heap.put_field(h, "$kind", Value::Str("counting".into())); }
        "summingInt" | "summingLong" | "summingDouble" => {
            vm.heap.put_field(h, "$kind", Value::Str("summing".into()));
            vm.heap.put_field(h, "$numkind", Value::Str(name.into()));
            vm.heap.put_field(h, "$keyfn", args.first().cloned().unwrap_or(Value::Null));
        }
        "averagingInt" | "averagingLong" | "averagingDouble" => {
            vm.heap.put_field(h, "$kind", Value::Str("averaging".into()));
            vm.heap.put_field(h, "$keyfn", args.first().cloned().unwrap_or(Value::Null));
        }
        "minBy" | "maxBy" => {
            vm.heap.put_field(h, "$kind", Value::Str("minmaxBy".into()));
            vm.heap.put_field(h, "$max", Value::Int((name == "maxBy") as i32));
            vm.heap.put_field(h, "$cmp2", args.first().cloned().unwrap_or(Value::Null));
        }
        // `partitioningBy` is a groupingBy whose classifier is a predicate and whose map
        // ALWAYS has both keys, even when a side is empty.
        "partitioningBy" => {
            vm.heap.put_field(h, "$kind", Value::Str("partitioningBy".into()));
            vm.heap.put_field(h, "$keyfn", args.first().cloned().unwrap_or(Value::Null));
            if let Some(d @ Value::Ref(_)) = args.get(1) {
                vm.heap.put_field(h, "$downstream", d.clone());
            }
        }
        // `mapping(f, downstream)` transforms each element, then collects.
        "mapping" | "flatMapping" | "filtering" => {
            vm.heap.put_field(h, "$kind", Value::Str(name.into()));
            vm.heap.put_field(h, "$keyfn", args.first().cloned().unwrap_or(Value::Null));
            if let Some(d @ Value::Ref(_)) = args.get(1) {
                vm.heap.put_field(h, "$downstream", d.clone());
            }
        }
        _ => {
            // toList / toUnmodifiableList / toCollection → list-like.
            vm.heap.put_field(h, "$kind", Value::Str("list".into()));
        }
    }
    Value::Ref(h)
}

// --- collections ------------------------------------------------------------
