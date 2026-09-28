//! Which arena-map operation a call names.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::arena_map::*;

/// Copy a source snapshot while keeping every not-yet-inserted pair forwardable. Each
/// `live_put` may render a key and collect before the following pair is visited.
fn put_source_entries(vm: &mut crate::Vm, map: &mut ArenaPtr, src: &Value) {
    let entries = source_entries(vm, src);
    let base = temps_mark();
    let _scope = TempScope(base);
    for (key, value) in entries {
        protect(key);
        protect(value);
    }
    let count = (temps_mark() - base) / 2;
    for index in 0..count {
        let key = protected(base + index * 2);
        let value = protected(base + index * 2 + 1);
        live_put(vm, map, key, value);
    }
}

/// Interpreter dispatch for a `java/util/HashMap` backed by an arena map. Mirrors
/// the `$map` surface (`map_method`) but reads/writes the flat table. Returns
/// `Ok(None)` for an unhandled method → the caller surfaces it (fail-closed: no
/// silent divergence). `args[0]` is the receiver (an arena map `Value::Obj`).
pub fn dispatch(vm: &mut crate::Vm, name: &str, args: &[Value]) -> Handled {
    if args.is_empty() {
        return Ok(None);
    }
    // Root every argument once. Compound operations call key derivation and callbacks before
    // reusing their key/value/function, so a clone from the original slice can already be stale.
    let args_base = temps_mark();
    let _args_scope = TempScope(args_base);
    for arg in args {
        protect(arg.clone());
    }
    let mut map = match protected(args_base) {
        Value::Obj(p) if is_arena_map(p) => p,
        _ => return Ok(None),
    };
    let map_root = args_base;
    let arg = |index: usize| {
        if index < args.len() { protected(args_base + index) } else { Value::Null }
    };
    let arg1 = || arg(1);
    let ret = match name {
        "<init>" => {
            // new HashMap(Map) / new HashMap(int): copy a source map if present; an
            // int initial-capacity arg is just a hint (we size on demand).
            if args.len() > 1 {
                put_source_entries(vm, &mut map, &arg1());
            }
            return Ok(Some(None));
        }
        "put" => live_put(vm, &mut map, arg1(), arg(2)),
        "putIfAbsent" => {
            let key = arg1();
            if contains(vm, &mut map, &key) {
                get(vm, &mut map, &arg1())
            } else {
                live_put(vm, &mut map, arg1(), arg(2));
                Value::Null
            }
        }
        "get" => get(vm, &mut map, &arg1()),
        "getOrDefault" => {
            let key = arg1();
            if contains(vm, &mut map, &key) {
                get(vm, &mut map, &arg1())
            } else {
                arg(2)
            }
        }
        "remove" => remove(vm, &mut map, &arg1()),
        "containsKey" => Value::Int(contains(vm, &mut map, &arg1()) as i32),
        "containsValue" => {
            let needle = vm.value_to_string(&arg1())?;
            map = protected_map(map_root);
            let mut found = false;
            for (_, v) in entries(map) {
                if vm.value_to_string(&v)? == needle {
                    found = true;
                    break;
                }
            }
            Value::Int(found as i32)
        }
        "size" => Value::Int(size(map)),
        "isEmpty" => Value::Int((size(map) == 0) as i32),
        // fastutil: set/get the value get() returns for an absent key (MC datafixers,
        // e.g. ItemIdFix, configure this). Setter (1 arg) stores + returns void.
        "defaultReturnValue" => {
            if args.len() > 1 {
                write(map, DEFAULT, encode_map_cell(&arg1()));
                return Ok(Some(None));
            }
            default_return(map)
        }
        "clear" => {
            clear(map);
            return Ok(Some(None));
        }
        "putAll" => {
            if args.len() > 1 {
                put_source_entries(vm, &mut map, &arg1());
            }
            return Ok(Some(None));
        }
        "forEach" => {
            // The lambda can allocate → collect → move an arena-object key/value. A
            // bare Rust Vec of decoded values would dangle, so materialise into a
            // HEAP array first: the arena collector forwards its ref cells in place
            // (it scans heap ref-arrays as roots), so each `array_get` post-GC reads
            // the forwarded value. The heap array itself is not arena-collected.
            let flat: Vec<Value> = entries(map).into_iter().flat_map(|(k, v)| [k, v]).collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", flat);
            let n = vm.heap.array_len(arr).unwrap_or(0);
            let mut i = 0;
            while i + 1 < n {
                let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                let v = vm.heap.array_get(arr, i + 1).unwrap_or(Value::Null);
                vm.apply(&arg1(), vec![k, v])?;
                i += 2;
            }
            return Ok(Some(None));
        }
        // ⚠**THESE WERE COPIES, AND A COPY IS NOT A VIEW.** The handle path
        // (`map/methods/map_method.rs`) stamps `$mapview` + `$mapowner` on the collection it
        // returns and registers it with `view::note_map_view`, which is what makes
        // `keySet().remove(k)`, `entrySet().iterator().remove()`, `removeIf` and `retainAll`
        // delete the MAPPING. This arm stamped nothing, so `map::map_view_of` answered `None`
        // and every one of those mutations landed in the throwaway collection: measured,
        // `entrySet().iterator().remove()` reported 20 removals against a map that still held
        // 20 (`bench/AmRem.java`), and `RegistrySetBuilder` reported the survivors as
        // "Errors during registry creation" — the failure that kept `RAVA_ARENA_MAPS` off all
        // four gates.
        //
        // The owner is stamped as `Value::Obj`, which is why `map_view_of` had to grow an
        // owner sum: the whole view path was handle-typed end to end.
        "keySet" => {
            let ks: Vec<Value> = entries(map).into_iter().map(|(k, _)| k).collect();
            let c = crate::map::collection(vm, ks);
            map = protected_map(map_root);
            stamp_view(vm, &c, map, "key");
            c
        }
        "values" => {
            let vs: Vec<Value> = entries(map).into_iter().map(|(_, v)| v).collect();
            let c = crate::map::collection(vm, vs);
            map = protected_map(map_root);
            stamp_view(vm, &c, map, "value");
            c
        }
        "entrySet" => {
            let es = crate::map::entry_view(vm, entries(map));
            let c = crate::map::collection(vm, es);
            map = protected_map(map_root);
            stamp_view(vm, &c, map, "entry");
            c
        }
        "toString" => {
            let mut parts = Vec::new();
            for (k, v) in entries(map) {
                parts.push(format!("{}={}", vm.value_to_string(&k)?, vm.value_to_string(&v)?));
            }
            Value::Str(format!("{{{}}}", parts.join(", ")).into())
        }
        // ─────────── Java 8 functional ops (guest callbacks → relocate-after) ───────────
        "computeIfAbsent" => {
            let key = arg1();
            let cur = get(vm, &mut map, &key);
            if !matches!(cur, Value::Null) {
                cur // present (non-null) → no compute
            } else {
                let key = arg1();
                let f = arg(2);
                let base = temps_mark();
                let (ki, mi) = (protect(key), protect(Value::Obj(map)));
                let nv = vm.apply(&f, vec![protected(ki)])?.unwrap_or(Value::Null);
                let vi = protect(nv);
                map = protected_map(mi);
                let r = finish_compute(vm, &mut map, ki, vi);
                unprotect_to(base);
                r
            }
        }
        "computeIfPresent" => {
            let key = arg1();
            let old = get(vm, &mut map, &key);
            if matches!(old, Value::Null) {
                Value::Null // absent / null-mapped → no compute
            } else {
                let key = arg1();
                let f = arg(2);
                let base = temps_mark();
                let (ki, mi, oi) = (protect(key), protect(Value::Obj(map)), protect(old));
                let nv = vm.apply(&f, vec![protected(ki), protected(oi)])?.unwrap_or(Value::Null);
                let vi = protect(nv);
                map = protected_map(mi);
                let r = finish_compute(vm, &mut map, ki, vi);
                unprotect_to(base);
                r
            }
        }
        "compute" => {
            let key = arg1();
            let old = get(vm, &mut map, &key);
            let key = arg1();
            let f = arg(2);
            let base = temps_mark();
            let (ki, mi, oi) = (protect(key), protect(Value::Obj(map)), protect(old));
            let nv = vm.apply(&f, vec![protected(ki), protected(oi)])?.unwrap_or(Value::Null);
            let vi = protect(nv);
            map = protected_map(mi);
            let r = finish_compute(vm, &mut map, ki, vi);
            unprotect_to(base);
            r
        }
        "merge" => {
            let key = arg1();
            let old = get(vm, &mut map, &key);
            let key = arg1();
            let value = arg(2);
            let base = temps_mark();
            // Protect key + map BEFORE the lambda — both are un-rooted Rust locals
            // (popped off the frame into `args`) that the callback's collect would
            // otherwise reclaim.
            let (ki, mi) = (protect(key), protect(Value::Obj(map)));
            let vi = if matches!(old, Value::Null) {
                protect(value) // no existing mapping → the supplied value
            } else {
                let f = arg(3);
                let (oi, vv) = (protect(old), protect(value));
                let computed = vm
                    .apply(&f, vec![protected(oi), protected(vv)])?
                    .unwrap_or(Value::Null);
                protect(computed)
            };
            map = protected_map(mi);
            let r = finish_compute(vm, &mut map, ki, vi);
            unprotect_to(base);
            r
        }
        "replace" if args.len() <= 3 => {
            // replace(key, value): only if currently mapped; returns the old value.
            let key = arg1();
            if contains(vm, &mut map, &key) {
                live_put(vm, &mut map, arg1(), arg(2))
            } else {
                Value::Null
            }
        }
        "replace" => {
            // replace(key, oldValue, newValue): swap only on a value match → boolean.
            let key = arg1();
            let cur = get(vm, &mut map, &key);
            let base = temps_mark();
            let _cur_scope = TempScope(base);
            let ci = protect(cur);
            let present = contains(vm, &mut map, &arg1());
            let cur_key = present.then(|| crate::map::value_map_key(vm, &protected(ci)));
            let matches_old = cur_key
                .is_some_and(|cur_key| cur_key == crate::map::value_map_key(vm, &arg(2)));
            if matches_old {
                map = protected_map(map_root);
                live_put(vm, &mut map, arg1(), arg(3));
                Value::Int(1)
            } else {
                Value::Int(0)
            }
        }
        "replaceAll" => {
            // Snapshot into a heap array (rooted: the collector forwards its cells),
            // then recompute + store each, relocating the map after every callback.
            let flat: Vec<Value> = entries(map).into_iter().flat_map(|(k, v)| [k, v]).collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", flat);
            let n = vm.heap.array_len(arr).unwrap_or(0);
            let mut i = 0;
            while i + 1 < n {
                let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                let v = vm.heap.array_get(arr, i + 1).unwrap_or(Value::Null);
                let base = temps_mark();
                let (ki, mi) = (protect(k), protect(Value::Obj(map)));
                let nv = vm.apply(&arg1(), vec![protected(ki), v])?.unwrap_or(Value::Null);
                let vi = protect(nv);
                map = protected_map(mi);
                live_put(vm, &mut map, protected(ki), protected(vi));
                unprotect_to(base);
                i += 2;
            }
            return Ok(Some(None));
        }
        // `AbstractMap.equals`/`hashCode` — ABSENT on this side, so an arena map fell to the
        // generic path and compared by IDENTITY. `Map.of("a",1).equals(arenaMap)` therefore
        // passed (the HANDLE receiver runs the real comparison) while the same test with the
        // arena map as RECEIVER answered false, and its `hashCode` disagreed with an equal
        // handle map — the last two divergent lines the flag showed (`CollSem` map.equals,
        // `EqualsSem` map.hashEq). Same two helpers the handle path uses, so there is one
        // definition of map equality and one of map hashing, not two.
        "hashCode" => Value::Int(crate::map::map_hash(vm, &entries(map))),
        "equals" => {
            let other = arg1();
            let same = matches!(&other, Value::Obj(p2) if p2.addr() == map.addr());
            let eq = if same {
                true
            } else if crate::map::is_map_value(vm, &other) {
                map = protected_map(map_root);
                let mine = entries(map);
                // `is_map_value` can load/resolve classes. Read the argument's forwarded
                // slot again before draining it.
                let theirs = crate::map::drain_any(vm, &arg1());
                crate::map::map_entries_equal(vm, &mine, &theirs)
            } else {
                false
            };
            Value::Int(eq as i32)
        }
        _ => return Ok(None), // unhandled → fail-closed at the caller
    };
    Ok(Some(Some(ret)))
}

/// Tag a collection as a LIVE view of `map`, exactly as the handle path does for a
/// `$map`-backed owner: the view marker, the owner, the view registry entry, and the
/// add-refusing immutability contract a view carries (`VIEW_NO_ADD` — a view permits
/// removal and refuses addition). Without all four the collection is a snapshot and every
/// view-mediated mutation is silently lost.
#[cfg(feature = "next-gen-gc")]
fn stamp_view(vm: &mut crate::Vm, c: &Value, map: ArenaPtr, kind: &'static str) {
    if let Value::Ref(ch) = c {
        vm.heap.put_field(*ch, "$mapview", Value::Str(kind.into()));
        vm.heap.put_field(*ch, "$mapowner", Value::Obj(map));
        crate::view::note_map_view(*ch);
    }
    crate::immutable::mark(vm, c, crate::immutable::VIEW_NO_ADD);
}
