//! Which map family serves a call.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::*;

/// The MAP-shaped guava Immutable owners whose `copyOf` the arm below materialises natively —
/// hoisted to a `pub(crate)` const so [`crate::map::GUAVA_IMMUTABLE_PROVIDERS`] (the authoritative
/// union behind `FACT_GUAVA_IMMUTABLE`) can list its provider rows from the one place they are
/// written, and the drift test in `map/classes.rs` can prove the union never misses one. LIST/SET
/// shapes stay excluded here, exactly as the arm's comment demands.
pub(crate) const GUAVA_IMMUTABLE_COPYOF_OWNERS: &[&str] = &[
    "com/google/common/collect/ImmutableMap",
    "com/google/common/collect/ImmutableSortedMap",
    "com/google/common/collect/ImmutableBiMap",
];

/// One `Map.of`/`ofEntries`/`copyOf` entry, with the two checks the JDK's
/// factories run and ours did not: `Objects.requireNonNull` on the key and the
/// value, and `IllegalArgumentException` on a key already present. The duplicate
/// test reuses `map_put`'s own return (the displaced value), so "duplicate" means
/// exactly what a hit means to the backing table — including under a salted key.
fn put_checked(vm: &mut Vm, id: u32, map_obj: u32, k: Value, v: Value) -> Result<(), VmError> {
    if matches!(k, Value::Null) || matches!(v, Value::Null) {
        return Err(crate::immutable::npe(vm));
    }
    // The duplicate TEST stays the internal key — that is the point, it means exactly
    // what a hit means to the backing table, salt included. The MESSAGE must not: the
    // JDK prints the key's `toString()`, and printing the type-tagged form leaked
    // `duplicate key: s:k` where `java -Xint` says `duplicate key: k`. The `Set.of`
    // twin (`reject_duplicate_elements`) already renders for exactly this reason.
    let internal = crate::stream::ekey(vm, &k);
    if map_put_gc_safe_mk(vm, id, map_obj, k, v)?.is_some() {
        // Read the surviving key back OUT of the map rather than rendering a clone kept
        // across the put: the put can allocate, an arena key can move, and a stale
        // pointer would render garbage on the one path where the text is the product.
        let shown = vm
            .heap
            .map_entries(id)
            .into_iter()
            .map(|(mk, _)| mk)
            .find(|mk| crate::stream::ekey(vm, mk) == internal)
            .map(|mk| vm.value_to_string(&mk).unwrap_or_else(|_| mk.display()))
            .unwrap_or(internal);
        return Err(crate::immutable::iae(vm, &format!("duplicate key: {shown}")));
    }
    Ok(())
}

/// WHICH REPRESENTATION DOES THE `get` TRAFFIC ACTUALLY ARRIVE IN? (`RAVA_MAPGET_CENSUS`, inert
/// otherwise.)
///
/// `fastutil_array_get` — the identity-scan fast path that ledger commit `60dc98e` measured at
/// −10.4 s — admits `Value::Obj(p)` ONLY. `RAVA_BRIDGE_STAT` puts `java/util/Map#get` at **460,514
/// calls and 1.21 s of bridge self-time** a boot, at 2.6 µs each, which is three orders of
/// magnitude more than an identity scan over a handful of packed keys can cost. Either the fast
/// path is missing that traffic or the cost is somewhere else entirely, and **the two are
/// indistinguishable without a count**.
///
/// This is the same question that paid twice today (ledger rows 302-303): a mechanism that is
/// built, correct and admits the representation the traffic does not use. Buckets:
/// `obj_hit` = arena receiver, fast path answered · `obj_miss` = arena receiver, fast path declined
/// · `ref` = heap handle (fast path cannot even be attempted) · `other` = anything else.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod mapget_census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static OBJ_HIT: AtomicU64 = AtomicU64::new(0);
    pub(crate) static OBJ_MISS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static REF: AtomicU64 = AtomicU64::new(0);

    fn classes() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, u64>> {
        static M: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, u64>>> =
            std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
    }

    pub(crate) fn note_class(cls: &str) {
        REF.fetch_add(1, Relaxed);
        let mut g = classes().lock().unwrap_or_else(|e| e.into_inner());
        *g.entry(cls.to_string()).or_insert(0) += 1;
    }
    pub(crate) static OTHER: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_MAPGET_CENSUS").is_some())
    }

    pub fn report() -> String {
        if !enabled() {
            return String::new();
        }
        format!(
            "[MAPGET] obj_hit={} obj_miss={} ref={} other={}
{}",
            OBJ_HIT.load(Relaxed),
            OBJ_MISS.load(Relaxed),
            REF.load(Relaxed),
            OTHER.load(Relaxed),
            {
                let g = classes().lock().unwrap_or_else(|e| e.into_inner());
                let mut rows: Vec<(&String, &u64)> = g.iter().collect();
                rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
                rows.iter()
                    .take(10)
                    .map(|(c, n)| format!("[MAPGET]   {n:>9}  {c}"))
                    .collect::<Vec<_>>()
                    .join("
")
            }
        )
    }
}

/// ⭐**DERIVE EACH FACT OF A MAP OPERATION ONCE.** **DEFAULT ON**, kill switch
/// `RAVA_NO_FAST_MAPDERIVE=1`, and it also yields to `flags::emit_more_disabled()` so the whole
/// block prices as one A/B. What it gates, all of it byte-identical to what it replaces:
///
///  * `map::dispatch`'s `entrySet` route: the receiver's ArrayMap SHAPE — its class id, the
///    `(key, value, size)` field triple, the two backing array handles and the logical size —
///    was read by `entryview_shape` to build the cache word and then read AGAIN, all five facts,
///    by `drain_arena_array_map`. Five facts, twice, on every entry-view MISS.
///  * `map::dispatch`'s packed-array `get`: the key argument was CLONED before the layout test
///    that decides whether the arm can serve the call at all.
///  * `keys::arena_funnel_get`: the "is this class all-final" verdict was derived on the memo
///    MISS purely to feed a census that is off by default, and then derived a second time by
///    `arena_funnel_put` a render later, where it actually decides something.
pub(crate) fn fast_mapderive() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // MEASURED OPT-IN (round 31) -- see `heap::fields` HG_NODERIVE for the numbers.
        std::env::var_os("RAVA_FAST_MAPDERIVE").is_some() && !crate::flags::emit_more_disabled()
    })
}

pub(crate) fn dispatch(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Handled {
    let name = fastutil_alias(name);
    // ⭐**DECIDE FIRST, ROOT SECOND.** The rooting below is not free — a `Value` clone and a
    // `Vec` push per argument, plus the scope guard's truncate — and until this line it was paid
    // by every call the map stage DECLINES, which is most of them: the stage bit that admits
    // callers here (`owner_bits::S_MAP`) covers all of `java/util/*`, so `Objects.equals`,
    // `Random.nextInt` and `UUID.fromString` all rooted their arguments and then walked ~34 class
    // -name comparisons to be told no. `map_call_admissible` is a superset of every arm below and
    // reads only the first argument's DISCRIMINANT, so it can neither allocate nor collect — the
    // one property that makes it safe to run ahead of the roots. Kill switch
    // `RAVA_NO_FAST_MAPDISPATCH`.
    //
    // ⭐**THE ONE ATOMIC LOAD FOR THE WHOLE CALL** (`RAVA_NO_MS_GATEWORD=1` restores the per-site
    // loads). See `map_gates` in the parent module for the count: `get` went 4 → 1, a fastutil
    // `entrySet` 7 → 1, and the decline — which is most of the traffic the `S_MAP` stage bit
    // admits — 2 → 1. Everything below reads bits of `g`, which is a byte in a register.
    let g = crate::map::map_gates();
    if mg_fast(g) && !map_call_admissible(owner, name, args) {
        return Ok(None);
    }
    // Root all args for the dispatch: map ops re-enter the interpreter (mapping/merge fns,
    // forEach BiConsumer) and are a common ENCLOSING native for a stream op — so if a mid-
    // stream collection (RAVA_STREAM_GC) runs while this map op holds an arena arg (a key/
    // value / Holder) only in a Rust local, it would be swept. Pure hygiene otherwise.
    //
    // ⭐**ONLY A REFERENCE CAN BE SWEPT OR MOVED, SO ONLY A REFERENCE IS PUSHED.** The loop cloned
    // EVERY argument: an `Int`/`Long`/`Double`/`Null` bought a `Vec` push the collector will never
    // look at, and a `Value::Str` bought TWO ATOMIC READ-MODIFY-WRITES — the `Arc<String>` bump on
    // the clone and the matching decrement in `GpScope::drop` — for a string the collector does
    // not own at all (`keyroot`'s own test says exactly that: "`Str` is included because its `Arc`
    // is owned by the `Value`, not by the collector"). The caller's `args` slice holds that `Arc`
    // live for the whole call, so the second handle was pure refcount traffic. **A string-keyed
    // `Map#get`/`put`/`containsKey` — the shape nearly every map key in this engine has — loses
    // one Arc increment and one decrement per call**, plus one `Vec` push per scalar argument.
    // The identical predicate with the identical argument already stands on the hot invoke path in
    // `opcodes::statics` (`gc_protect.extend(args.iter().filter(|v| is_ref(v)).cloned())`); this is
    // that fix reaching the map dispatcher. Nothing reads these slots BY INDEX — `GpScope` only
    // truncates, and the nested rooters (`gcsafe`, `keyroot`, `keys`) take their own `base` at
    // their own entry — so pushing fewer of them cannot shift a reader.
    let _gpargs = {
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        for a in args {
            if crate::stream::is_gc_rootable(a) {
                crate::vm::thread_context::push_root(&mut vm.gc_protect, a.clone());
            }
        }
        crate::stream::GpScope(&mut vm.gc_protect as *mut _, base)
    };
    // LIVE ARENA-MAP CUT-OVER: an INSTANCE method whose receiver is an arena map
    // (RAVA_ARENA_MAPS java/util/HashMap) reads/writes the flat table instead of the
    // `$map` std::HashMap. arena_map::dispatch returns Ok(None) when it does not
    // handle the method — crucially including a STATIC factory (ImmutableSortedMap
    // .copyOf(srcMap), Map.copyOf(srcMap)) whose `args[0]` is the source MAP, not a
    // receiver — so we fall through to the factory handlers below in that case.
    // The JDK's `AbstractMap.equals` / `hashCode` for a map whose state lives in its OWN
    // bytecode fields. `map_method`'s arm (rightly) refuses a receiver with no `$map`, so a jar
    // map that inherits both — gson's `LinkedTreeMap`, hence every `JsonObject` — fell through to
    // `Object`: two empty `JsonObject`s compared UNEQUAL and hashed apart, and DFU's
    // `ExtraCodecs.dispatchOptionalValue` (`Objects.equals(encoded, ops.emptyMap())`) then
    // emitted `"conditions":{}` on every advancement criterion without conditions
    // (bench/ReloadBench.java; bench/CompSem.java `json.eq`). This arm fires only when the
    // resolved OWNER is `java/util/AbstractMap` itself — i.e. no class on the receiver's chain
    // declares its own `equals`/`hashCode` bytecode, so the JDK body is the one that would run —
    // and drains the receiver through its own `entrySet()` (`Vm::drain_map_any`). Same
    // comparison as the native arm: `map_entries_equal` / `map_hash`. Kill switch
    // `RAVA_NO_JARMAP_EQUALS=1`.
    if owner == "java/util/AbstractMap" && matches!(name, "equals" | "hashCode") && jarmap_equals_enabled() {
        if let Some(recv) = args.first() {
            let has_table = match recv {
                Value::Ref(h) => matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_)),
                _ => false,
            };
            if !has_table {
                let cls = vm.value_class_name(recv);
                if !cls.is_empty() && vm.is_map_class(&cls) {
                    if let Some(mine) = vm.drain_map_any(recv)? {
                        if name == "hashCode" {
                            return Ok(Some(Some(Value::Int(map_hash(vm, &mine)))));
                        }
                        let other = args.get(1).cloned().unwrap_or(Value::Null);
                        let same = match (recv, &other) {
                            (Value::Ref(a), Value::Ref(b)) => a == b,
                            #[cfg(feature = "next-gen-gc")]
                            (Value::Obj(a), Value::Obj(b)) => a.addr() == b.addr(),
                            _ => false,
                        };
                        let eq = if same {
                            true
                        } else {
                            let ocls = vm.value_class_name(&other);
                            let other_is_map = is_map_value(vm, &other)
                                || (!ocls.is_empty() && vm.is_map_class(&ocls));
                            if other_is_map {
                                let theirs = match vm.drain_map_any(&other)? {
                                    Some(t) => t,
                                    None => drain_any(vm, &other),
                                };
                                map_entries_equal(vm, &mine, &theirs)
                            } else {
                                false
                            }
                        };
                        return Ok(Some(Some(Value::Int(eq as i32))));
                    }
                }
            }
        }
    }
    #[cfg(feature = "next-gen-gc")]
    if mg_arena_map(g) {
        if let Some(Value::Obj(p)) = args.first() {
            if crate::arena_map::is_arena_map(*p) {
                let r = crate::arena_map::dispatch(vm, name, args)?;
                if r.is_some() {
                    return Ok(r);
                }
            }
        }
    }
    // Plan A: Reference2ObjectArrayMap entrySet-family served natively (materialize
    // packed key/value arrays) — skips the interpreted `$EntrySet$1` iterator that
    // `force_native_collection` (same gate) can't cover, so gate-off is byte-identical.
    // Default ON; opt-out RAVA_NO_R2OMAP=1 restores the guest iterator (kill-switch / A-B).
    #[cfg(feature = "next-gen-gc")]
    if owner == "it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap"
        && mg_r2omap(g)
        // ⭐**THE METHOD NAME DECIDES BEFORE THE MAP IS COPIED.** Three names below use `es`;
        // every other one — `get`, `put`, `containsKey`, `size`, `isEmpty`, the whole rest of the
        // Map surface — fell through the `match` to `None` AFTER `drain_arena_array_map` had
        // already cloned every key and every value of the receiver into a fresh `Vec`, which was
        // then dropped unread. On this owner that is the hot case: the comment on the `get` arm
        // below records `Map#get` as 37.4% of NATIVE_BRIDGES, and each of those calls built and
        // discarded a full copy of the map. **What grows: entries in the receiver, per call.**
        && (!mg_fast(g)
            || matches!(name, "entrySet" | "reference2ObjectEntrySet" | "keySet" | "values")
            || (name == "forEach" && r2omap_foreach_enabled()))
    {
        if let Some(recv) = args.first().cloned() {
            // ⭐**AND THE CACHE IS CONSULTED BEFORE THE COPY IT EXISTS TO AVOID.** The shape probe
            // and the memo lookup below stood on the far side of the drain, so a HIT — the case
            // the memo was built for, ~2.9M `reference2ObjectEntrySet` calls a boot — still paid
            // the full materialisation before finding the answer it did not need. Reading the
            // shape first is byte-identical: for an arena receiver the drain is a pure read of the
            // same three fields, so it cannot change what the shape word says; for a heap `Ref`
            // the shape is `None` either way.
            //
            // `entryview_shape` being `Some` implies `drain_arena_array_map` is `Some` (both need
            // the same `arraymap_ids` layout and the same two arrays), so returning a hit here can
            // never skip a fall-through the old order would have taken.
            let shape = if !mg_fast(g) {
                None
            } else if matches!(name, "entrySet" | "reference2ObjectEntrySet") && mg_entryview(g) {
                let sh = entryview_probe(vm, &recv);
                if let Some(sh) = &sh {
                    if let Some(hit) = entryview_cached(vm, &recv, sh) {
                        return Ok(Some(Some(hit)));
                    }
                } else {
                    super::entry_census::bump(&super::entry_census::NO_PROBE, 1);
                }
                sh
            } else {
                None
            };
            // Reference2ObjectArrayMap is allocated as an arena Obj (its packed
            // arrays) in the block-state path; a heap Ref falls back to drain_any.
            //
            // ⭐**THE SHAPE PROBE ABOVE ALREADY READ EVERYTHING THIS NEEDS.** `entryview_shape`
            // derived the class id, the `(key, value, size)` field triple through
            // `arraymap_ids`' thread-local memo, and all three field slots — and returned only a
            // folded `u64`, so `drain_arena_array_map` derived the identical five facts a second
            // time, on every entry-view MISS. `entryview_probe` carries them out; the drain then
            // reads only the two arrays. When the probe declined (`shape` is `None` — a
            // non-ArrayMap receiver, an off-layout one, a `keySet`/`values` call, or the
            // `!mg_fast` historic order) NOTHING was derived yet and the original function runs
            // unchanged, which is the cold arm. Kill switch `RAVA_NO_FAST_MAPDERIVE`.
            let entries: Option<Vec<(Value, Value)>> = match &recv {
                Value::Obj(p) => match &shape {
                    Some(sh) if fast_mapderive() => Some(drain_from_shape(vm, sh)),
                    _ => drain_arena_array_map(vm, *p),
                },
                Value::Ref(_) => Some(drain_any(vm, &recv)),
                _ => None,
            };
            if let Some(es) = entries {
                // Rooted throughout: `entry_view` and `collection` both hold their working set
                // under `native_args_push` (ledger rows 340/341). Every branch here keeps guest
                // `Value`s in a Rust local across an allocation point, and a Rust local is
                // invisible to the root walk.
                let res: Option<Value> = match name {
                    "entrySet" | "reference2ObjectEntrySet" => {
                        // ⭐**CACHE THE VIEW, AS `map_method`'s `entrySet` ALREADY DOES.** That arm
                        // caches on `$entryview` + a `MapObj::version`, and its comment prices the
                        // uncached form at 6,115 ns on a four-entry map. This PLAN-A route for
                        // `Reference2ObjectArrayMap` never reached it, so fastutil kept paying —
                        // and the JIT-callback census puts `reference2ObjectEntrySet` at 4.52% of
                        // the boot's 64,088,346 crossings (~2.9M calls), each materialising one
                        // `Map$Entry` handle per mapping (`Map$Entry` is 48.1% of all handle
                        // births).
                        //
                        // ⚠**The version here is the receiver's SHAPE, not a mutation counter.**
                        // An arena `Reference2ObjectArrayMap` has no `MapObj::version`; what it
                        // has is a packed `key` array, a `value` array and a `size`. Identical
                        // triple ⇒ the same mappings in the same order ⇒ the view already built
                        // IS the answer. The hole this leaves is an IN-PLACE overwrite of one
                        // `value[i]` without touching size or either array identity; that is a
                        // `put` on an existing key, which on this engine goes through the native
                        // map layer (these maps are `force_native_collection`, which is why this
                        // Plan-A route exists at all) and so replaces the value array. Kill
                        // switch `RAVA_NO_ENTRYVIEW_CACHE` restores the rebuild.
                        // The probe already ran ABOVE when the fast path is on — reaching here
                        // means it established a MISS, and `shape` carries the word to store
                        // under. With the fast path off, run the historic order: probe here,
                        // after the drain.
                        let shape = if mg_fast(g) {
                            shape
                        } else if !mg_entryview(g) {
                            None
                        } else {
                            let sh = entryview_probe(vm, &recv);
                            if let Some(sh) = &sh {
                                if let Some(hit) = entryview_cached(vm, &recv, sh) {
                                    return Ok(Some(Some(hit)));
                                }
                            }
                            sh
                        };
                        let evs = super::entry_view(vm, es);
                        let c = collection(vm, evs);
                        if let Value::Ref(ch) = c {
                            vm.heap.put_field(ch, "$mapview", super::view_marker("entry"));
                        }
                        if let Some(sh) = &shape {
                            entryview_store(vm, &recv, sh, &c);
                        }
                        Some(c)
                    }
                    // ⭐**A COPY IS NOT A VIEW — the lesson `arena_map/dispatch.rs` already
                    // records, one representation along.** These two handed back a detached
                    // snapshot, so `keySet().removeIf(p)`, `keySet().remove(k)`,
                    // `keySet().retainAll(c)` and `values().remove(v)` each mutated the snapshot
                    // and reported success while the backing map kept every entry — and
                    // `DataComponentPatch.forget(pred)`, whose entire body is
                    // `new Reference2ObjectArrayMap<>(map).keySet().removeIf(pred)`, therefore
                    // forgot nothing (bench/CompSem.java `forget`, `keySet.*`, `values.remove`).
                    //
                    // Stamped exactly as `arena_map::stamp_view` stamps its own: the view
                    // marker, the OWNER (a `Value::Obj`, read back as `MapOwner::Jar`), the
                    // view-registry bit, and the add-refusing contract a view carries. The READ
                    // path is untouched — the collection handed back holds the same elements it
                    // always did — so only a MUTATION through the view behaves differently.
                    // Kill switch `RAVA_NO_R2OMAP_VIEW_WRITETHROUGH=1` restores the snapshot.
                    "keySet" | "values" => {
                        let elems: Vec<Value> = if name == "keySet" {
                            es.into_iter().map(|(k, _)| k).collect()
                        } else {
                            es.into_iter().map(|(_, v)| v).collect()
                        };
                        let c = collection(vm, elems);
                        if let (Value::Obj(p), Value::Ref(ch)) = (&recv, &c) {
                            if r2omap_view_writethrough() {
                                let kind = if name == "keySet" { "key" } else { "value" };
                                vm.heap.put_field(*ch, "$mapview", super::view_marker(kind));
                                vm.heap.put_field(*ch, "$mapowner", Value::Obj(*p));
                                crate::view::note_map_view(*ch);
                                crate::immutable::mark(vm, &c, crate::immutable::VIEW_NO_ADD);
                            }
                        }
                        Some(c)
                    }
                    // The JDK's `Map.forEach` default — `for (e : entrySet()) action.accept(k, v)`
                    // — over the entries just materialised. fastutil declares no `forEach` on
                    // `Reference2ObjectMap`, `AbstractReference2ObjectMap` or the ArrayMap (javap
                    // 8.5.18), so `resolve_method` finds no bytecode for it and the natives own it;
                    // until now nothing here answered, and `stream::dispatch`'s generic Iterable
                    // drain "served" it by visiting nothing (see `is_map_recv` there). Same rooting
                    // as `map_method`'s `forEach`: each `apply` runs guest code that can allocate,
                    // a moving arena collection forwards heap-array cells in place, and a bare
                    // Rust `Vec` is invisible to every root walk — so the pairs ride in a heap
                    // `Object[]` and each is read back forwarded. The `BiConsumer` is re-read per
                    // pair for the same reason. Kill switch `RAVA_NO_R2OMAP_FOREACH=1`.
                    "forEach" if r2omap_foreach_enabled() => {
                        let mut flat = Vec::with_capacity(es.len() * 2);
                        for (k, v) in es {
                            flat.push(k);
                            flat.push(v);
                        }
                        let arr = vm.heap.alloc_values("Ljava/lang/Object;", flat);
                        let n = vm.heap.array_len(arr).unwrap_or(0);
                        let mut i = 0;
                        while i + 1 < n {
                            let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                            let v = vm.heap.array_get(arr, i + 1).unwrap_or(Value::Null);
                            let f = args.get(1).cloned().unwrap_or(Value::Null);
                            vm.apply(&f, vec![k, v])?;
                            i += 2;
                        }
                        return Ok(Some(None));
                    }
                    _ => None,
                };
                if let Some(res) = res {
                    return Ok(Some(Some(res)));
                }
            }
        }
    }
/// `RAVA_NO_ENTRYVIEW_CACHE=1` rebuilds the entry view on every call, as before.
///
/// Folded into `map::map_gates` as the AFFIRMATIVE bit `MG_ENTRYVIEW`; `pub(crate)` only so the
/// parent's word builder — and the kill-switch fallback in `mg_entryview` — can call it.
/// `Map.forEach` served over a `Reference2ObjectArrayMap`'s materialised entries (default ON);
/// `RAVA_NO_R2OMAP_FOREACH=1` restores the decline, so the change prices as a same-binary A/B.
pub(crate) fn r2omap_foreach_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_R2OMAP_FOREACH").is_none())
}

/// `keySet()`/`values()` on a `Reference2ObjectArrayMap` handed back as a LIVE VIEW that writes
/// through to the jar map (default ON); `RAVA_NO_R2OMAP_VIEW_WRITETHROUGH=1` restores the
/// detached snapshot, so the change prices as a same-binary A/B and, with the switch set,
/// `MapOwner::Jar` is unreachable.
fn r2omap_view_writethrough() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_R2OMAP_VIEW_WRITETHROUGH").is_none())
}

/// `AbstractMap.equals`/`hashCode` served for jar-bytecode maps (default ON);
/// `RAVA_NO_JARMAP_EQUALS=1` restores the identity answer.
fn jarmap_equals_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_JARMAP_EQUALS").is_none())
}

pub(crate) fn entryview_cache_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_ENTRYVIEW_CACHE").is_some())
}

/// The receiver's mapping SHAPE as one word: `(key array, value array, size)`. `None` for a
/// receiver whose shape cannot be read, which then takes the rebuild path.
#[cfg(feature = "next-gen-gc")]
fn entryview_shape(vm: &mut Vm, recv: &Value) -> Option<u64> {
    let Value::Obj(p) = recv else { return None };
    let cid = crate::nextgen::arena_class_id(*p);
    let (kid, vid, sid) = crate::map::classes::arraymap_ids(vm, cid)?;
    let ka = match crate::opcodes::arena_getfield(*p, kid) { Value::Array(a) => a as u64, _ => return None };
    let va = match crate::opcodes::arena_getfield(*p, vid) { Value::Array(a) => a as u64, _ => return None };
    let n = match crate::opcodes::arena_getfield(*p, sid) { Value::Int(s) => s.max(0) as u64, _ => return None };
    Some(ka ^ (va << 20) ^ (n << 44) ^ 1)
}

#[cfg(not(feature = "next-gen-gc"))]
fn entryview_shape(_vm: &mut Vm, _recv: &Value) -> Option<u64> { None }

/// ⭐**THE SHAPE PROBE'S FIVE FACTS, KEPT.** `entryview_shape` reads the receiver's class id,
/// asks `arraymap_ids` for the `(key, value, size)` field triple, and then reads all three
/// fields — and threw every one of them away, returning only the folded cache word. The caller
/// then handed the SAME receiver to `drain_arena_array_map`, which is the identical five
/// derivations again: `arena_class_id`, the `arraymap_ids` thread-local memo probe, and three
/// `arena_getfield` slot reads. **Five facts, derived twice, on every entry-view MISS** — and
/// the entry-view route is 4.52% of the boot's 64,088,346 JIT crossings (~2.9 M calls).
///
/// This is the same probe with its intermediate results carried out instead of discarded.
/// Unconditional (the fields are plain words) so the non-`next-gen-gc` stubs below can name it.
#[allow(dead_code)]
struct ArrayMapShape {
    /// Packed key array handle, as read from the `key` field.
    ka: u32,
    /// Packed value array handle, as read from the `value` field.
    va: u32,
    /// The `size` field, RAW (still possibly negative — `drain` clamps exactly as before).
    n: i32,
    /// The cache word `entryview_shape` returns, bit-for-bit.
    word: u64,
}

/// `entryview_shape` with its working set returned. `None` for exactly the receivers
/// `entryview_shape` answers `None` for, which the `debug_assert` below checks on EVERY call in
/// a debug build against the original predicate.
#[cfg(feature = "next-gen-gc")]
fn entryview_probe(vm: &mut Vm, recv: &Value) -> Option<ArrayMapShape> {
    #[cfg(feature = "next-gen-gc")]
    fn probe(vm: &mut Vm, recv: &Value) -> Option<ArrayMapShape> {
        let Value::Obj(p) = recv else { return None };
        let cid = crate::nextgen::arena_class_id(*p);
        let (kid, vid, sid) = crate::map::classes::arraymap_ids(vm, cid)?;
        let ka = match crate::opcodes::arena_getfield(*p, kid) { Value::Array(a) => a, _ => return None };
        let va = match crate::opcodes::arena_getfield(*p, vid) { Value::Array(a) => a, _ => return None };
        let n = match crate::opcodes::arena_getfield(*p, sid) { Value::Int(s) => s, _ => return None };
        // Byte-for-byte `entryview_shape`'s fold: `ka ^ (va << 20) ^ (n.max(0) << 44) ^ 1`.
        let word = (ka as u64) ^ ((va as u64) << 20) ^ ((n.max(0) as u64) << 44) ^ 1;
        Some(ArrayMapShape { ka, va, n, word })
    }
    let out = probe(vm, recv);
    // The original predicate is still here and is still the authority: a debug build checks the
    // carried word against it on every call, so a divergence is a test failure, not a silent
    // cache alias.
    debug_assert_eq!(
        out.as_ref().map(|s| s.word),
        entryview_shape(vm, recv),
        "entryview_probe disagreed with entryview_shape"
    );
    out
}

/// `drain_arena_array_map` for a receiver whose shape was ALREADY probed — same two arrays, same
/// clamp, same order, same pairs.
///
/// ⚠**Why the carried handles cannot be stale.** Between `entryview_probe` and this call the only
/// thing that runs is `entryview_cached`, which is a thread-local table read plus a
/// `heap.class_of` liveness test: no guest code, no allocation, therefore no collection and no
/// mutation of the receiver. So `ka`/`va`/`n` still say what the receiver says.
///
/// The clamp is `drain_arena_array_map`'s, verbatim. Its "size field is not an `Int`" fallback
/// (`keys.len().min(vals.len())`) cannot be reached from here because `entryview_probe` returns
/// `None` in exactly that case, and the caller then takes the original function.
///
/// The pairing is `zip`, not two indexed clones: the two `Vec`s are freshly allocated and owned
/// here, so moving the elements out is the same `Value`s without the per-element clone the
/// original paid (an `Arc` bump and matching drop for every `Str` key or value in the map).
#[cfg(feature = "next-gen-gc")]
fn drain_from_shape(vm: &mut Vm, sh: &ArrayMapShape) -> Vec<(Value, Value)> {
    let keys = vm.heap.array_values(sh.ka).unwrap_or_default();
    let vals = vm.heap.array_values(sh.va).unwrap_or_default();
    let n = (sh.n.max(0) as usize).min(keys.len()).min(vals.len());
    keys.into_iter().zip(vals).take(n).collect()
}

#[cfg(feature = "next-gen-gc")]
std::thread_local! {
    /// ((gc epoch, handle epoch), arena identity -> (shape, view handle)). Keyed on the monotonic
    /// identity stamp, which survives evacuation; the epoch tag drops the table across a
    /// collection so a recycled handle can never answer for a different view.
    static ENTRYVIEW: std::cell::RefCell<((u64, u64), crate::FastMap<u32, (u64, u32, Box<[i64]>)>)> =
        std::cell::RefCell::new(((0, 0), crate::FastMap::default()));
}

/// The receiver's live mappings as RAW cells — `key[0..n]` then `value[0..n]` — the exact
/// content the cached view was built from, or `None` when either array cannot be read.
///
/// ⛔**THE SHAPE WORD IS NOT A MUTATION COUNTER, and the comment that named the hole under-sold
/// it.** `(key array, value array, size)` is unchanged by an in-place `value[i] = v` — the
/// documented gap — but ALSO by `clear()` followed by a refill to the same size, which is what
/// `PatchedDataComponentMap.restorePatch` (`patch.clear(); patch.putAll(other)`) does on every
/// call, and both run as the jar's own bytecode on the packed arrays, never through a native
/// arm. bench/CompSem.java `refill.after`: a map cleared and refilled with two DIFFERENT keys
/// still answered `entrySet()` with the two OLD ones. So a hit is confirmed against the cells
/// themselves: one slice compare of `2n` words, against the per-entry `Map$Entry` allocation a
/// rebuild costs. Encoded cells are stable for as long as the tag is — an arena address until
/// the next arena collection, a handle until the next handle sweep — and both epochs are in the
/// tag, so equal cells mean equal mappings. `RAVA_NO_ENTRYVIEW_CELLS=1` restores the shape-only
/// test.
#[cfg(feature = "next-gen-gc")]
fn entryview_cells(vm: &Vm, sh: &ArrayMapShape) -> Option<Box<[i64]>> {
    let ks = vm.heap.array_ref_cells(sh.ka)?;
    let vs = vm.heap.array_ref_cells(sh.va)?;
    let n = (sh.n.max(0) as usize).min(ks.len()).min(vs.len());
    let mut out = Vec::with_capacity(2 * n);
    out.extend_from_slice(&ks[..n]);
    out.extend_from_slice(&vs[..n]);
    Some(out.into_boxed_slice())
}

/// `true` when `snap` is exactly what `entryview_cells` would return now — without building it.
#[cfg(feature = "next-gen-gc")]
fn entryview_cells_match(vm: &Vm, sh: &ArrayMapShape, snap: &[i64]) -> bool {
    let (Some(ks), Some(vs)) = (vm.heap.array_ref_cells(sh.ka), vm.heap.array_ref_cells(sh.va)) else {
        return false;
    };
    let n = (sh.n.max(0) as usize).min(ks.len()).min(vs.len());
    snap.len() == 2 * n && snap[..n] == ks[..n] && snap[n..] == vs[..n]
}

#[cfg(feature = "next-gen-gc")]
fn entryview_cells_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ENTRYVIEW_CELLS").is_none())
}

/// The invalidation tag of the entry-view table: EVERY collector that can make a cached
/// `Value::Ref` mean something else.
///
/// ⛔**The view handle is held in a thread-local Rust table, which no root walk sees.** So the
/// moment the guest drops its last reference to the view, the next HANDLE-heap sweep
/// (`Vm::gc` → `Heap::gc`, `vm/gc.rs`) retires the slot into `free_obj`, and the next
/// `put_object` re-issues it to an unrelated object. The tag was the ARENA epoch alone
/// (`nextgen::gc_epoch`, bumped by `tlab_collect` and the major), and the handle sweep runs on
/// its own cadence (`maybe_gc` on handle-heap pressure) WITHOUT bumping it. So after a handle
/// sweep with no arena collection in between, `entryview_cached` found the same epoch, the same
/// shape, a handle `class_of` still reported live — and handed back whatever now lived in that
/// slot. On a datapack reload that was a gson `JsonObject`'s entry view: `DataComponentPatch`'s
/// encoder read `[arena-string]` keys and string values out of a
/// `Reference2ObjectMap<DataComponentType, Optional>` and died in `checkcast`
/// (bench/ReloadBench.java, `ClassCastException: class [arena-string] cannot be cast to class
/// DataComponentType`; bench/CompSem.java is the standalone form). Intermittent by nature: it
/// needs a handle sweep to land between two `entrySet()` calls on one unchanged map.
///
/// `heap::handle_epoch` is the signal this tree already defines for exactly this — "any cache
/// keyed by `Value::Ref` handle can alias from this moment on" (`retire_object_slot`) — bumped
/// per retired slot, so it moves iff some handle became re-issuable. Folding it into the tag
/// makes the table drop on every sweep that freed anything, which is the invariant the
/// comment above always claimed. A sweep that frees nothing keeps the table, correctly.
///
/// `RAVA_NO_ENTRYVIEW_HANDLE_EPOCH=1` restores the arena-only tag (the historic behaviour) so
/// the change prices as a same-binary A/B.
#[cfg(feature = "next-gen-gc")]
fn entryview_tag() -> (u64, u64) {
    static ARENA_ONLY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let arena_only =
        *ARENA_ONLY.get_or_init(|| std::env::var_os("RAVA_NO_ENTRYVIEW_HANDLE_EPOCH").is_some());
    let handles = if arena_only { 0 } else { crate::heap::handle_epoch() };
    (crate::nextgen::gc_epoch(), handles)
}

#[cfg(feature = "next-gen-gc")]
fn entryview_cached(vm: &Vm, recv: &Value, sh: &ArrayMapShape) -> Option<Value> {
    let Value::Obj(p) = recv else { return None };
    let id = crate::nextgen::arena_identity_hash(*p) as u32;
    let tag = entryview_tag();
    let cells = entryview_cells_enabled();
    let got = ENTRYVIEW.with(|c| {
        let m = c.borrow();
        if m.0 != tag {
            super::entry_census::bump(&super::entry_census::MISS_EPOCH, 1);
            return None;
        }
        match m.1.get(&id) {
            Some((shape, h, snap))
                if *shape == sh.word && *h != 0 && (!cells || entryview_cells_match(vm, sh, snap)) =>
            {
                Some(Value::Ref(*h))
            }
            other => {
                // WHICH of the four things a memo can fail on actually fails. Behind the census
                // gate, so a production run evaluates none of it.
                if super::entry_census::enabled() {
                    let c = match other {
                        None => &super::entry_census::MISS_FIRST,
                        Some((shape, h, _)) if *shape != sh.word || *h == 0 => {
                            &super::entry_census::MISS_SHAPE
                        }
                        _ => &super::entry_census::MISS_CELLS,
                    };
                    super::entry_census::bump(c, 1);
                }
                None
            }
        }
    });
    let had = got.is_some();
    let live = got.filter(|v| matches!(v, Value::Ref(h) if vm.heap.class_of(*h).is_some()));
    if super::entry_census::enabled() {
        if had && live.is_none() {
            super::entry_census::bump(&super::entry_census::MISS_DEAD, 1);
        }
        super::entry_census::bump(
            if live.is_some() {
                &super::entry_census::EV_HIT
            } else {
                &super::entry_census::EV_MISS
            },
            1,
        );
    }
    live
}

#[cfg(feature = "next-gen-gc")]
fn entryview_store(vm: &Vm, recv: &Value, sh: &ArrayMapShape, view: &Value) {
    let (Value::Obj(p), Value::Ref(h)) = (recv, view) else { return };
    let id = crate::nextgen::arena_identity_hash(*p) as u32;
    let tag = entryview_tag();
    let snap = if entryview_cells_enabled() {
        match entryview_cells(vm, sh) {
            Some(s) => s,
            None => return,
        }
    } else {
        Box::default()
    };
    ENTRYVIEW.with(|c| {
        let mut m = c.borrow_mut();
        if m.0 != tag { m.0 = tag; m.1.clear(); }
        if m.1.len() >= 64 * 1024 { m.1.clear(); }
        m.1.insert(id, (sh.word, *h, snap));
    });
}

#[cfg(not(feature = "next-gen-gc"))]
fn entryview_cached(_vm: &Vm, _r: &Value, _s: &ArrayMapShape) -> Option<Value> { None }
#[cfg(not(feature = "next-gen-gc"))]
fn entryview_store(_vm: &Vm, _r: &Value, _s: &ArrayMapShape, _v: &Value) {}

    // fastutil `*ArrayMap#get`: identity-scan the packed key array `[0..size)`
    // directly — the single largest serial bridge in block C (Map#get = 37.4%
    // of NATIVE_BRIDGES). fastutil's `findKey` compares by reference identity
    // and walks size-1 → 0, so mirror both exactly. Receiver-shaped (any owner,
    // incl. the java/util/Map interface sites), layout-guarded, falls back to
    // the guest path on any mismatch. Same gate as the entrySet materializer.
    #[cfg(feature = "next-gen-gc")]
    if name == "get" && mg_r2omap(g) {
        if let Some(recv) = args.first() {
            if let Value::Obj(p) = recv {
                // ⭐**THE KEY WAS CLONED BEFORE THE TEST THAT DECIDES IF IT IS NEEDED.**
                // `fastutil_array_get` takes the key BY REFERENCE and its first act is the layout
                // admission (`arraymap_ids`), which declines every arena receiver that is not an
                // `*ArrayMap` — and this arm is entered for EVERY arena-`Obj` receiver of a `get`
                // on any owner, which is the same shape as the `_gpargs` fix at the top of this
                // function. For a `Value::Str` key the clone is two atomic read-modify-writes (the
                // `Arc` bump plus the matching drop) on a string the callee only reads, and the
                // caller's `args` slice already holds it live for the whole call. Same `&Value`
                // either way, so the answer cannot move; `&Value::Null` is a statement-lifetime
                // temporary standing in for the absent argument exactly as `unwrap_or` did.
                let got = if fast_mapderive() {
                    fastutil_array_get(vm, *p, args.get(1).unwrap_or(&Value::Null))
                } else {
                    // COLD ARM (`RAVA_NO_FAST_MAPDERIVE=1`): the historic clone, untouched.
                    let key = args.get(1).cloned().unwrap_or(Value::Null);
                    fastutil_array_get(vm, *p, &key)
                };
                if mg_census(g) {
                    let b = if got.is_some() { &mapget_census::OBJ_HIT } else { &mapget_census::OBJ_MISS };
                    b.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if let Some(v) = got {
                    return Ok(Some(Some(v)));
                }
            } else if mg_census(g) {
                // WHO are these receivers? The counts already say the arm serves 0 of 1,005,617;
                // only the class names say which native owns them, and therefore which fast path
                // could ever have covered this traffic.
                let cls = match recv {
                    Value::Ref(h) => vm.heap.class_of(*h).unwrap_or("?").to_string(),
                    other => format!("<{}>", other.display()),
                };
                mapget_census::note_class(&cls);
            }
        }
    }
    if owner == "java/util/Map$Entry" && matches!(name, "comparingByKey" | "comparingByValue") {
        // Static: a Comparator over entries, by $key/$val. An explicit
        // Comparator arg (overload) is kept for completeness but our natural
        // compare suffices for the registry/tag sorts that need this.
        let h = vm.heap.alloc("java/util/Comparator");
        vm.heap.put_field(h, "$cmp", Value::Str(
            if name == "comparingByValue" { "entryval" } else { "entrykey" }.into()));
        return Ok(Some(Some(Value::Ref(h))));
    }
    if owner == "java/util/Map$Entry" || owner == "java/util/AbstractMap$SimpleEntry"
        || owner == "java/util/AbstractMap$SimpleImmutableEntry"
    {
        return entry_method(vm, name, args);
    }
    // guava's interner reads its entry through the InternalEntry interface; the object
    // we hand back from `getEntry` is a native Map$Entry, so its getKey/getValue
    // resolve here regardless of the (interface) static call type. Gated → default
    // build byte-identical.
    #[cfg(feature = "next-gen-gc")]
    if owner == "com/google/common/collect/MapMakerInternalMap$InternalEntry" {
        return entry_method(vm, name, args);
    }
    // guava MapMaker builder. The fluent knobs (weakKeys/weakValues/softValues/
    // concurrencyLevel/initialCapacity/maximumSize/...) just return the builder;
    // makeMap()/makeComputingMap() yield a fresh native-backed map. This skips
    // guava's segmented MapMakerInternalMap (which needs sun.misc.Unsafe).
    if owner == "com/google/common/collect/MapMaker" {
        if matches!(name, "makeMap" | "makeComputingMap") {
            let h = vm.heap.alloc("com/google/common/collect/MapMakerInternalMap");
            let id = vm.heap.alloc_map();
            vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
            return Ok(Some(Some(Value::Ref(h))));
        }
        // Every other MapMaker method is a fluent builder setter → return `this`.
        if let Some(this) = args.first().cloned() {
            return Ok(Some(Some(this)));
        }
    }
    // guava `MapMakerInternalMap.create(MapMaker)` / `createWithDummyValues(MapMaker)`
    // — the static factories the segmented map (and Interners) build through. Like
    // `makeMap`, hand back a fresh native-backed map and skip the Unsafe-dependent
    // guava segment/entry bytecode. `createWithDummyValues` backs a guava Interner
    // (a Set keyed by the interned objects, values are MapMaker$Dummy). Gated to the
    // next-gen-gc MC-boot path so the default build stays byte-identical.
    #[cfg(feature = "next-gen-gc")]
    if owner == "com/google/common/collect/MapMakerInternalMap"
        && matches!(name, "create" | "createWithDummyValues")
    {
        let h = vm.heap.alloc("com/google/common/collect/MapMakerInternalMap");
        let id = vm.heap.alloc_map();
        vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
        return Ok(Some(Some(Value::Ref(h))));
    }
    // Static set factory.
    if owner == "java/util/concurrent/ConcurrentHashMap" && name == "newKeySet" {
        let arr = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
        let h = vm.heap.alloc("java/util/concurrent/ConcurrentHashMap$KeySetView");
        vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
        return Ok(Some(Some(Value::Ref(h))));
    }
    // fastutil entry-set view utilities: Long2ObjectMaps.fastIterable(map),
    // Int2ObjectMaps.fastIterable, Object2ObjectMaps.fastIterable — static methods
    // returning an ObjectIterable over the map's entries (a zero-alloc reuse view
    // in fastutil; for us a plain entry collection). TicketStorage walks its
    // Long2ObjectMap this way during world-spawn ticket scan. Each entry supports
    // getLongKey/getValue via fastutil_alias + entry_method.
    // fastutil synchronized/unmodifiable wrappers: *Maps.synchronize(map),
    // *Sets.synchronize(set), *Lists.synchronize(list), and their unmodifiable
    // variants — static methods returning a view over the argument. We are
    // effectively single-threaded for these, so hand back the underlying
    // collection unchanged (DistanceManager wraps a Long2IntMap this way).
    if matches!(name, "synchronize" | "unmodifiable") && owner.contains("fastutil")
        && (owner.ends_with("Maps") || owner.ends_with("Sets") || owner.ends_with("Lists"))
    {
        return Ok(Some(Some(args.first().cloned().unwrap_or(Value::Null))));
    }
    if name == "fastIterable" && owner.ends_with("Maps") && owner.contains("fastutil") {
        let entries = drain_any(vm, &args.first().cloned().unwrap_or(Value::Null));
        let refs = super::entry_view(vm, entries);
        return Ok(Some(Some(collection(vm, refs))));
    }
    // Immutable map factories: Map.of(k,v,...), Map.ofEntries(...), Map.copyOf, Map.entry.
    if owner == "java/util/Map" && matches!(name, "of" | "ofEntries" | "copyOf" | "entry") {
        let h = vm.heap.alloc("java/util/HashMap");
        let id = vm.heap.alloc_map();
        vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
        match name {
            "entry" => {
                let (k, v) = (args.first().cloned().unwrap_or(Value::Null),
                              args.get(1).cloned().unwrap_or(Value::Null));
                // Map.entry(k,v) is requireNonNull on both.
                if matches!(k, Value::Null) || matches!(v, Value::Null) {
                    return Err(crate::immutable::npe(vm));
                }
                return Ok(Some(Some(crate::map::make_entry(vm, k, v))));
            }
            "of" => {
                let mut i = 0;
                while i + 1 < args.len() {
                    // FAIL-FAST: `Map.of` is requireNonNull on every key AND value,
                    // and rejects a DUPLICATE KEY with IllegalArgumentException.
                    // Ours silently kept the last write for a duplicate and stored
                    // nulls, so a guest bug the JDK names at the factory vanished.
                    put_checked(vm, id, h, args[i].clone(), args[i + 1].clone())?;
                    i += 2;
                }
            }
            "copyOf" => {
                if let Some(src) = args.first().cloned() {
                    // A Map source cannot carry a duplicate key, so copyOf only has
                    // the null check (put_checked's duplicate arm never fires).
                    for (k, v) in drain_any(vm, &src) {
                        put_checked(vm, id, h, k, v)?;
                    }
                }
            }
            "ofEntries" => {
                let entries = match args.first() { Some(Value::Array(a)) => vm.heap.array_values(*a).unwrap_or_default(), _ => args.to_vec() };
                for e in entries {
                    if let Value::Ref(eh) = e {
                        let k = vm.heap.get_field_syn(eh, crate::heap::Syn::Key);
                        let v = vm.heap.get_field_syn(eh, crate::heap::Syn::Val);
                        put_checked(vm, id, h, k, v)?;
                    }
                }
            }
            _ => {}
        }
        // Every one of these is an `ImmutableCollections` map: EVERY mutator throws,
        // whatever the contents (`Map.of("k","v").remove("absent")` throws too).
        let m = Value::Ref(h);
        crate::immutable::mark(vm, &m, crate::immutable::IMMUTABLE);
        return Ok(Some(Some(m)));
    }
    // guava Immutable{,Bi,Sorted}Map.copyOf(srcMap) — materialise a native map by
    // DRAINING the source (drain_map handles a fastutil/jar map with no $map).
    // MAP-shaped Immutable owners only: ImmutableList/ImmutableSet.copyOf must
    // NOT become a VM map (lists/sets are not maps — hijacking them corrupted
    // e.g. block-property registration on older MC, which iterates an
    // ImmutableList copy of property values).
    if crate::map::GUAVA_IMMUTABLE_COPYOF_OWNERS.contains(&owner) && name == "copyOf"
    {
        // ⛔**`ImmutableSortedMap.copyOf` IS SORTED BY NATURAL KEY ORDER, NOT BY THE SOURCE.** This
        // arm materialised all three factories as a `LinkedHashMap`, i.e. in SOURCE order, and the
        // native map layer decides sorted iteration by the object's class (`is_sorted_map` in
        // `map_method.rs`), so an `ImmutableSortedMap` built here iterated in insertion order.
        // Guava's contract (and `java` on this box, even for a descending source) is natural
        // ordering of the keys. Found by `bench/BootstrapBench.java`: the state-graph HASH
        // disagreed with java on 238 of 1,166 blocks while every state and every `initCache`
        // scalar agreed — ORDER only. `StateDefinition` iterates `ImmutableSortedMap.copyOf(
        // properties).values()` to build the cartesian product of states, and a block-state's
        // index in that list IS its protocol id, so this was a wire-visible defect on every block
        // whose properties were registered in non-alphabetical order.
        //
        // A `TreeMap` is the same `$map` storage with the sorted-iteration verdict already
        // attached (`firstKey`, `lastKey`, `headMap`, iteration and `values()` all read through
        // `sorted_entries`), so the sorted factory allocates one; the two unsorted factories keep
        // the `LinkedHashMap` they had.
        let cls = if owner == "com/google/common/collect/ImmutableSortedMap" {
            "java/util/TreeMap"
        } else {
            "java/util/LinkedHashMap"
        };
        let h = vm.heap.alloc(cls);
        let id = vm.heap.alloc_map();
        vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
        if let Some(src) = args.first().cloned() {
            for (k, v) in drain_any(vm, &src) {
                map_put_gc_safe(vm, id, k, v)?;
            }
        }
        return Ok(Some(Some(Value::Ref(h))));
    }
    // guava Maps.transformValues(map, fn) / transformEntries(map, entryFn) return a
    // LAZY transformed VIEW over the source map. rava's generic stream/iterator
    // interception (iterate_recv) cannot drain that guava AbstractMap view, so its
    // entrySet()/stream() came back EMPTY — which silently emptied
    // MappedRegistry.entrySet() (= mapValuesLazy(byKey, Holder::value).entrySet()),
    // so RegistryAccess.registries() yielded nothing → the static built-in registries
    // (sound_event, block, …) never reached the datapack RegistryOps lookup → every
    // datapack codec referencing one failed "Registry does not exist". Evaluate the
    // transform EAGERLY into a native map: the source map is frozen by the time it is
    // viewed and the transform is pure, so the eager result is observationally
    // identical. Gated → default build byte-identical. (Wall #11.)
    #[cfg(feature = "next-gen-gc")]
    if owner == "com/google/common/collect/Maps" && matches!(name, "transformValues" | "transformEntries") {
        let h = vm.heap.alloc("java/util/LinkedHashMap");
        let id = vm.heap.alloc_map();
        vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
        let src = args.first().cloned().unwrap_or(Value::Null);
        let f = args.get(1).cloned().unwrap_or(Value::Null);
        // Root the drained (key,value) pairs in a heap Object[] (flattened
        // k0,v0,k1,v1,…): the per-entry transform runs guest code that allocates, and
        // a moving collection would otherwise strand the unprocessed arena entries in
        // an unrooted Rust Vec (Wall #10). Read every value back FORWARDED.
        let pairs = drain_any(vm, &src);
        let mut flat = Vec::with_capacity(pairs.len() * 2);
        for (k, v) in pairs { flat.push(k); flat.push(v); }
        let arr = vm.heap.alloc_values("Ljava/lang/Object;", flat);
        let n = vm.heap.array_len(arr).unwrap_or(0);
        let mut i = 0;
        while i + 1 < n {
            let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
            let v = vm.heap.array_get(arr, i + 1).unwrap_or(Value::Null);
            let nv = if name == "transformEntries" {
                vm.apply(&f, vec![k, v])?.unwrap_or(Value::Null)
            } else {
                vm.apply(&f, vec![v])?.unwrap_or(Value::Null)
            };
            vm.heap.array_set(arr, i + 1, nv); // root the transformed value
            let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
            let ks = value_map_key_in(vm, id, &k)?; // may allocate → nv stays rooted in `arr`
            let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
            let nv = vm.heap.array_get(arr, i + 1).unwrap_or(Value::Null);
            vm.heap.map_put(id, ks, k, nv);
            i += 2;
        }
        return Ok(Some(Some(Value::Ref(h))));
    }
    // guava HashBiMap.create()/create(int)/create(Map) — a fresh native map.
    if owner.starts_with("com/google/common/collect/") && matches!(name, "create") {
        let h = vm.heap.alloc("com/google/common/collect/HashBiMap");
        let id = vm.heap.alloc_map();
        vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
        if let Some(Value::Ref(src)) = args.first() {
            if let Value::Int(sid) = vm.heap.get_field_syn(*src, crate::heap::Syn::Map) {
                for (k, v) in vm.heap.map_entries(sid as u32) {
                    map_put_gc_safe(vm, id, k, v)?;
                }
            }
        }
        return Ok(Some(Some(Value::Ref(h))));
    }
    if !MAP_CLASSES.contains(&owner) && !is_fastutil_objmap(owner) {
        return Ok(None);
    }
    map_method(vm, name, args)
}
