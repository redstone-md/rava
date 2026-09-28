//! The map method table.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::methods::*;

/// The current value at `k` iff it is PRESENT AND NON-NULL.
///
/// The whole `putIfAbsent`/`compute*`/`merge` family is specified on `get(key) == null`,
/// not on `containsKey`: a key mapped to null is "absent" for all of them. `map_get`
/// answers `Some(Value::Null)` for such an entry, so testing `is_some()` treated it as
/// present and skipped the write the JDK performs.
fn present_value(vm: &mut Vm, id: u32, k: &crate::heap::MapKey) -> Option<Value> {
    vm.heap.map_get(id, k).filter(|v| !matches!(v, Value::Null))
}

/// Apply a remapping result: a null new value REMOVES the entry, anything else stores it.
/// Shared by compute/computeIfPresent/merge, which are identical in this respect.
fn store_or_remove(vm: &mut Vm, id: u32, k: crate::heap::MapKey, kv: Value, v: &Value) {
    if matches!(v, Value::Null) {
        vm.heap.map_remove(id, &k);
    } else {
        vm.heap.map_put(id, k, kv, v.clone());
    }
}

/// The TEXT of a Java String key, in ANY of its three representations here.
///
/// The sort verdict must be keyed to what a key IS, not to which regime minted it: an
/// interpreted caller's inline `Value::Str`, a compiled caller's arena string and a
/// boxed heap handle are the same key to the guest, and one helper decides for both
/// `sorted_entries` and `natural_cmp` so the orderings cannot drift. A `Ref` is admitted
/// ONLY when its class really is `java/lang/String` — `BigInteger`/`BigDecimal`/`URL`
/// and the time classes carry a `$str` payload too, and they are NOT strings; sorting
/// them by it would invent an order this function is forbidden to invent.
fn string_key_text<'a>(vm: &crate::Vm, v: &'a Value) -> Option<std::borrow::Cow<'a, str>> {
    match v {
        Value::Str(s) => Some(std::borrow::Cow::Borrowed(s.as_str())),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
            // Lossy for the same reason every reader of these bytes is lossy: an invalid
            // sequence must render the same U+FFFD bytes the equality path renders.
            Some(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)))
        }
        // `Ref`: a String handle carries `$str`; a BigInteger/Decimal/URL/time carrier
        // must NOT sort as a string, so the class gate comes first.
        Value::Ref(h) if vm.heap.class_of(*h).is_some_and(|c| c == "java/lang/String") => {
            match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
                Value::Str(s) => Some(std::borrow::Cow::Owned(s.to_string())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The natural order of two keys, or `None` when this engine may not invent one.
///
/// Exactly `sorted_entries`' rule, factored out so the range and probe methods below cannot
/// drift from the ordering the views use: numbers compare numerically, strings compare as
/// `String.compareTo` does, and anything else — including a number against a string — has no
/// order here, because deciding it would mean calling the guest's `compareTo` from inside a
/// native (the re-entry this tree measured as a whole-engine break). A string is read through
/// [`string_key_text`], so both sides of the comparison see the same representations
/// `sorted_entries` admits.
fn natural_cmp(vm: &crate::Vm, a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    let num = |v: &Value| match v {
        Value::Int(i) => Some(*i as f64),
        Value::Long(l) => Some(*l as f64),
        Value::Float(f) => Some(*f as f64),
        Value::Double(d) => Some(*d),
        _ => None,
    };
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return x.partial_cmp(&y);
    }
    match (string_key_text(vm, a), string_key_text(vm, b)) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        _ => None,
    }
}

/// A fresh map of `owner`'s class holding `entries`, in the order given.
///
/// The range views (`headMap`/`tailMap`/`subMap`) and `descendingMap` return a COPY rather than a
/// live view. The JDK's are live in both directions, so a write through one of these is not seen
/// by the backing map — a real limitation, and a narrower one than the `UnsatisfiedLinkError`
/// they replace. Reads, which is what these are overwhelmingly used for, are exact.
fn new_map_from(vm: &mut Vm, owner: &str, entries: Vec<(Value, Value)>) -> Result<Value, VmError> {
    let h = vm.heap.alloc(owner);
    let id = vm.heap.alloc_map();
    vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
    for (k, v) in entries.into_iter() {
        let mk = value_map_key_in(vm, id, &k)?;
        vm.heap.map_put(id, mk, k, v);
    }
    Ok(Value::Ref(h))
}

/// A SORTED map's entries, in key order — for the receivers whose contract is order.
///
/// The backing store keeps insertion order for every map kind, which is right for HashMap and
/// LinkedHashMap and WRONG for TreeMap: `firstKey` returned whichever key happened to be inserted
/// first, and `toString`/iteration listed them unsorted. A differential probe against
/// `java -Xint` reads `{pear=3, apple=1, fig=2}` where the JVM reads `{apple=1, fig=2, pear=3}`,
/// and `firstKey=pear` against `firstKey=apple` — a silent wrong answer, not a missing feature.
///
/// ⚠ SORTED ONLY WHERE THE ORDER NEEDS NO GUEST FRAME. Java's order is the keys' `compareTo`, and
/// calling that means re-entering the interpreter from inside a native — which this tree measured
/// today as a whole-engine break (`Objects.hashCode`, four gates producing no hash at all). So
/// this sorts when EVERY key is a string or a number, where the natural order is the JDK's by
/// definition, and otherwise returns insertion order exactly as before. A TreeMap with
/// Comparable object keys is therefore still wrong — and still no more wrong than it was.
/// THE ELEMENTS OF A MAP VIEW, derived FRESH from the backing map.
///
/// `keySet()`, `values()` and `entrySet()` are specified as LIVE views: a `put` after the
/// view was taken must be visible through it. This engine materialised them once and handed
/// back a collection, so the view stopped tracking the moment it was created
/// (bench/MapViewSem.java, `keySet.live` / `values.live` / `entrySet.live`).
///
/// The three arms below call this, and so does `crate::view::read`, which is the choke point
/// every drain of every collection passes through — so one definition serves both the
/// creation and every later read, and they cannot answer differently.
pub(crate) fn map_view_elements(vm: &mut Vm, owner: u32, kind: &str) -> Option<Vec<Value>> {
    let Value::Int(id) = vm.heap.get_field_syn(owner, crate::heap::Syn::Map) else { return None };
    let id = id as u32;
    let owner_class = vm.heap.class_of(owner).unwrap_or("").to_string();
    let entries = sorted_entries(vm, id, &owner_class);
    Some(match kind {
        "key" => entries.into_iter().map(|(k, _)| k).collect(),
        "value" => entries.into_iter().map(|(_, v)| v).collect(),
        // ROOTED (ledger rows 340/341). `crate::map::entry_view` does not fit here — these
        // entries carry `$entryowner` and inherit immutability — so the same discipline is
        // applied in place: one FLAT, PRE-SIZED buffer registered once, keys and values read back
        // OUT of the rooted slots after each allocation because a collection forwards them there.
        "entry" => {
            let n = entries.len();
            let mut roots: Vec<Value> = Vec::with_capacity(n * 3);
            for (k, v) in entries {
                roots.push(k);
                roots.push(v);
            }
            roots.resize(n * 3, Value::Null);
            let base = crate::frames::native_args_push(roots.as_ptr(), roots.len());
            for i in 0..n {
                let e = vm.heap.alloc("java/util/Map$Entry");
                let (k, v) = (roots[i * 2].clone(), roots[i * 2 + 1].clone());
                vm.heap.put_field_syn(e, crate::heap::Syn::Key, k);
                vm.heap.put_field_syn(e, crate::heap::Syn::Val, v);
                // Same stamp the `entrySet` arm applies: an entry from a view is itself a
                // view of its mapping, and `setValue` writes through.
                vm.heap.put_field(e, "$entryowner", Value::Ref(owner));
                let ev = Value::Ref(e);
                crate::immutable::inherit(vm, owner, &ev);
                roots[n * 2 + i] = ev;
            }
            let out = roots[n * 2..].to_vec();
            crate::frames::native_args_truncate(base);
            out
        }
        _ => return None,
    })
}

/// Does this map ORDER its keys? The same four classes `sorted_entries` orders, named once
/// so the ordering rule and the null-key rule cannot drift apart — a sorted map rejects a
/// null key precisely because it would have to compare it.
fn is_sorted_map(owner: &str) -> bool {
    matches!(
        owner,
        "java/util/TreeMap"
            | "java/util/TreeSet"
            | "java/util/concurrent/ConcurrentSkipListMap"
            | "java/util/concurrent/ConcurrentSkipListSet"
    )
}

fn sorted_entries(vm: &mut Vm, id: u32, owner: &str) -> Vec<(Value, Value)> {
    let entries = vm.heap.map_entries(id);
    // AN ENUM MAP IS ORDERED BY ORDINAL, not by insertion — that is what distinguishes it
    // from a `HashMap` keyed on the same constants. `EnumMap` put WEST, NORTH, SOUTH
    // printed in that order where the JVM prints NORTH, SOUTH, WEST. Its keys are enum
    // constants, so the order needs no guest frame: `$enumOrd` is stamped on each.
    if owner == "java/util/EnumMap" {
        let mut keyed: Vec<(i32, (Value, Value))> = entries
            .into_iter()
            .map(|(k, v)| {
                // `enum_field`, because a guest enum's constants are arena objects.
                let ord = crate::natives::enum_field(vm, Some(&k), "$enumOrd")
                    .and_then(|x| x.as_int())
                    .unwrap_or(i32::MAX);
                (ord, (k, v))
            })
            .collect();
        keyed.sort_by_key(|(o, _)| *o);
        return keyed.into_iter().map(|(_, e)| e).collect();
    }
    if !is_sorted_map(owner) {
        return entries;
    }
    let mut keyed: Vec<(Option<f64>, Option<String>, (Value, Value))> = Vec::with_capacity(entries.len());
    for (k, v) in entries.into_iter() {
        match &k {
            Value::Int(i) => keyed.push((Some(*i as f64), None, (k.clone(), v))),
            Value::Long(l) => keyed.push((Some(*l as f64), None, (k.clone(), v))),
            Value::Float(f) => keyed.push((Some(*f as f64), None, (k.clone(), v))),
            Value::Double(d) => keyed.push((Some(*d), None, (k.clone(), v))),
            // A Java String key in ANY of its three representations here — an inline
            // `Value::Str`, an arena string, a `java/lang/String` handle carrying `$str` —
            // sorts by its text via [`string_key_text`], the same helper `natural_cmp`
            // reads, so the views and the range/probe methods cannot drift. Matching only
            // `Value::Str` keyed the verdict to whichever regime minted the key: a map
            // mixing interpreted and compiled callers' strings answered insertion order.
            _ => match string_key_text(vm, &k) {
                Some(t) => keyed.push((None, Some(t.into_owned()), (k.clone(), v))),
                // Anything else: no order this function is allowed to invent.
                None => return vm.heap.map_entries(id),
            },
        }
    }
    let all_num = keyed.iter().all(|(n, _, _)| n.is_some());
    let all_txt = keyed.iter().all(|(_, t, _)| t.is_some());
    if !all_num && !all_txt {
        return vm.heap.map_entries(id); // mixed kinds — the JVM would not have accepted them either
    }
    if all_num {
        keyed.sort_by(|a, b| a.0.unwrap().partial_cmp(&b.0.unwrap()).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        // Java's String.compareTo is UTF-16 code-unit order; for the ASCII-and-above keys this
        // engine produces, Rust's byte order over UTF-8 agrees except on surrogate-range inputs,
        // which cannot appear in a well-formed key here.
        keyed.sort_by(|a, b| a.1.as_ref().unwrap().cmp(b.1.as_ref().unwrap()));
    }
    keyed.into_iter().map(|(_, _, e)| e).collect()
}

/// Whether a guarded map mutator would really alter the table — the question the
/// `AbstractMap`-backed kind (`Collections.emptyMap`) turns on, since
/// `Map.remove`/`clear`/`replace`'s inherited bodies only reach the throwing
/// primitive when the key (or any entry) is actually there.
fn would_change_map(
    vm: &mut Vm,
    id: u32,
    obj: u32,
    name: &str,
    op: crate::immutable::MapOp,
    args: &[Value],
) -> Result<bool, VmError> {
    use crate::immutable::MapOp;
    Ok(match op {
        MapOp::Store | MapOp::Always => true,
        MapOp::StoreAll => match args.get(1) {
            Some(Value::Ref(src)) => match vm.heap.get_field_syn(*src, crate::heap::Syn::Map) {
                Value::Int(sid) => vm.heap.map_size(sid as u32) > 0,
                _ => !drain_any(vm, &Value::Ref(*src)).is_empty(),
            },
            _ => false,
        },
        MapOp::DropMaybe => match name {
            "clear" | "replaceAll" => vm.heap.map_size(id) > 0,
            // remove/replace/computeIfPresent all key off a present entry.
            _ => match args.get(1) {
                Some(k) => {
                    let ks = map_key(vm, obj, &k.clone())?;
                    vm.heap.map_contains(id, &ks)
                }
                None => false,
            },
        },
    })
}

pub(crate) fn map_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let obj = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    // fastutil knobs with no native-$map equivalent (keep them off the error path).
    match name {
        "defaultReturnValue" => return Ok(Some(if args.len() > 1 { None } else { Some(Value::Null) })),
        "trim" => return Ok(Some(Some(Value::Int(1)))),
        _ => {}
    }
    // BiMap.inverse() views redirect to the forward map with swapped roles.
    if let Value::Ref(orig) = vm.heap.get_field(obj, "$inverseof") {
        return inverse_view_method(vm, orig, name, args);
    }
    // <init>: empty by default, but copy a source Map if given (new HashMap(map)).
    if name == "<init>" {
        // `LinkedHashMap(capacity, loadFactor, accessOrder)` — the three-arg form is the
        // LRU constructor and the third argument was being ignored, so an access-ordered
        // map iterated in INSERTION order and every LRU built on it evicted the wrong
        // entry. Recorded here; `get` below does the reordering.
        if matches!(args.get(3), Some(Value::Int(1))) {
            vm.heap.put_field(obj, "$accessOrder", Value::Int(1));
        }
        // EITHER REPRESENTATION (see `map::any_map_entries`): this matched `Value::Ref` only,
        // so `new TreeMap<>(arenaMap)` silently produced an empty map.
        if let Some(src) = args.get(1).cloned() {
            let entries = crate::map::any_map_entries(vm, &src);
            if !entries.is_empty() {
                let id = map_id(vm, obj);
                for (k, v) in entries {
                    map_put_gc_safe_mk(vm, id, obj, k, v)?;
                }
            }
        }
        return Ok(Some(None));
    }
    // `equals`/`hashCode` take a MAP (or nothing) where every other method takes a
    // KEY, so they are answered BEFORE the key-derivation block below — running
    // `map_key` over another whole map would render it as a key for nothing.
    //
    // Without these two arms both fell through to `Object`: two maps with equal
    // contents compared UNEQUAL and hashed differently, breaking every guest that
    // uses a Map as a value (a Map inside a record's equals, a Set<Map>, an
    // assertEquals over two configs).
    //
    // Served ONLY for a receiver that IS one of our native maps — it carries a
    // `$map`, or its own class (not the call site's static type) is one we back
    // natively. A jar class that merely INHERITS AbstractMap.equals arrives here
    // with owner `java/util/Map` and no backing table; answering for it would
    // lazily mint an empty map and report two unrelated instances equal, so it
    // keeps the identity answer it had.
    if matches!(name, "equals" | "hashCode") {
        let native_map = matches!(vm.heap.get_field_syn(obj, crate::heap::Syn::Map), Value::Int(_))
            || vm
                .heap
                .class_of(obj)
                .is_some_and(|c| MAP_CLASSES.contains(&c) || is_fastutil_objmap(c));
        if !native_map {
            return Ok(None);
        }
        let id = map_id(vm, obj);
        if name == "hashCode" {
            // AbstractMap.hashCode: the SUM of `key.hashCode() ^ value.hashCode()`,
            // so it is order-independent and equal maps hash equal.
            let entries = vm.heap.map_entries(id);
            return Ok(Some(Some(Value::Int(map_hash(vm, &entries)))));
        }
        // AbstractMap.equals: same size, and every key maps to an equal value.
        let other = args.get(1).cloned().unwrap_or(Value::Null);
        let eq = match &other {
            Value::Ref(oh) if *oh == obj => true,
            v if is_map_value(vm, v) => {
                let mine = vm.heap.map_entries(id);
                // A jar map (`LinkedTreeMap`, an `AbstractMap` subclass of the guest's own) is
                // drained through its own `entrySet()` bytecode; `drain_any` knows only ours.
                let theirs = match vm.drain_map_any(v)? {
                    Some(t) => t,
                    None => drain_any(vm, v),
                };
                map_entries_equal(vm, &mine, &theirs)
            }
            _ => false,
        };
        return Ok(Some(Some(Value::Int(eq as i32))));
    }
    let id = map_id(vm, obj);
    // FAIL-FAST CONTRACT (immutable.rs): a map from `Map.of`/`ofEntries`/`copyOf`
    // (unconditional) or `Collections.emptyMap` (only where the inherited default
    // body would really store) refuses its mutators. Read methods pay only the
    // `classify_map` name match.
    if let Some(op) = crate::immutable::classify_map(name) {
        let kind = crate::immutable::kind_of(vm, obj);
        if kind != crate::immutable::NONE {
            let changes = kind == crate::immutable::IMMUTABLE
                || would_change_map(vm, id, obj, name, op, args)?;
            if crate::immutable::refuses_map(kind, op, changes) {
                return Err(crate::immutable::uoe(vm));
            }
        }
    }
    // The receiver's class, for the arms whose CONTRACT is the iteration order (TreeMap and its
    // kin). Read once: `class_of` is a table index, and only the sorted arms consult it.
    let owner_class = vm.heap.class_of(obj).unwrap_or("").to_string();
    // Mutating the map invalidates any memoised structural key (see structural_key).
    if matches!(name, "put" | "remove" | "clear" | "setProperty" | "putIfAbsent"
        | "computeIfAbsent" | "compute" | "computeIfPresent" | "merge" | "putAll"
        | "replace" | "replaceAll" | "putAndMoveToFirst" | "addTo"
        | "removeBoolean" | "removeInt" | "removeLong" | "removeShort") {
        vm.heap.put_field_syn(obj, crate::heap::Syn::SKey, Value::Null);
    }
    // THE NULL-HOSTILE MAPS. `ConcurrentHashMap`, `Hashtable` and the concurrent
    // skip-list maps reject a null key AND a null value from every insertion; ours
    // accepted both and stored them, so a guest relying on the NPE as a contract check
    // (which is why those maps forbid null in the first place — a null cannot be told
    // apart from "absent" in a concurrent read) saw success instead.
    //
    // `HashMap`, `LinkedHashMap` and `TreeMap` all permit at least one of the two, so
    // this is keyed on the exact classes rather than on being a map.
    if matches!(name, "put" | "putIfAbsent" | "merge" | "replace")
        && matches!(
            owner_class.as_str(),
            "java/util/concurrent/ConcurrentHashMap"
                | "java/util/concurrent/ConcurrentSkipListMap"
                | "java/util/Hashtable"
                | "java/util/Properties"
        )
        && (matches!(args.get(1), Some(Value::Null)) || matches!(args.get(2), Some(Value::Null)))
    {
        return Err(VmError::Throw(vm.make_exception("java/lang/NullPointerException", "")));
    }
    let key_val = args.get(1).cloned();
    // GC-safety: `map_key` → `value_to_string` → the key's guest `toString()`. For a record
    // key (density functions, Holder$Direct, …) that compiles to an ObjectMethods
    // `makeConcatWithConstants` invokedynamic which ALLOCATES a String → a moving arena
    // collection mid-`map_key`. `key_val` (stored as the entry key by write ops) and the
    // value arg `args[2]` are arena Values held across it; without rooting, a write op stores
    // a stale (moved young / reused-slot) key or value — the worldgen density-tree intern
    // killer (a stale Ap2 later read as Holder$Direct.value → DensityFunction.minValue ULE).
    // Root both in a heap Object[] (visible to every root scan) across `map_key`, then read
    // them back FORWARDED. Same shield as `map_put_gc_safe` / `jit_map_put`.
    #[cfg(feature = "next-gen-gc")]
    let kv_root = key_val
        .as_ref()
        .map(|kv| {
            MapKeyRoot::new(
                vm,
                kv.clone(),
                args.get(2).cloned().unwrap_or(Value::Null),
                map_key_gc_protect_enabled(),
            )
        });
    #[cfg(feature = "next-gen-gc")]
    let key_input = kv_root.as_ref().map(|root| root.get(vm, 0));
    #[cfg(not(feature = "next-gen-gc"))]
    let key_input = key_val.clone();
    // GAP-P04: salted derivation runs guest hashCode/equals — throws propagate.
    let key = match key_input {
        Some(k) => Some(map_key(vm, obj, &k)?),
        None => None,
    };
    #[cfg(feature = "next-gen-gc")]
    let (key_val, value_reread) = match &kv_root {
        Some(arr) => (
            Some(arr.get(vm, 0)),
            Some(arr.get(vm, 1)),
        ),
        None => (key_val, None),
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let value_reread: Option<Value> = None;
    // Read the trace flag ONCE — this runs on every map op (env::var = global mutex +
    // alloc per call); cache it so the hot map path pays nothing when tracing is off.
    static TRACE_MAPKEY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *TRACE_MAPKEY.get_or_init(|| std::env::var_os("RAVA_TRACE_MAPKEY").is_some()) {
        if let Some(ks) = &key {
            let hit = vm.heap.map_get(id, ks).is_some();
            eprintln!("[MAPKEY] {name} obj={obj} id={id} hit={hit} ks={ks:?}");
        }
    }

    let ret = match name {
        // forcePut: guava BiMap.put / forcePut — same as put for our purposes.
        "put" | "forcePut" => {
            // A SORTED map has to COMPARE its keys, so a null key is a
            // NullPointerException on every JVM — `new TreeMap<>().put(null, 1)` throws
            // where this engine silently stored it and then had a key it could never
            // order (bench/MapViewSem.java, `treeMap.nullKey`). Keyed on the owner's own
            // sorted-ness, which is the same property `sorted_entries` above reads.
            if matches!(key_val, Some(Value::Null) | None) && is_sorted_map(&owner_class) {
                let ex = vm.make_exception("java/lang/NullPointerException", "");
                return Err(VmError::Throw(ex));
            }
            let k = key.unwrap_or_default();
            let kv = key_val.unwrap_or(Value::Null);
            let v = value_reread.clone().or_else(|| args.get(2).cloned()).unwrap_or(Value::Null);
            // TRIPWIRE (th=128 hunt): a NULL VALUE stored under a String key of the
            // DFU schema-template shape means the caller LOST the supplier before the
            // put — print the put-site backtrace once so the losing frame is named.
            // Gated: prints only on the anomaly (a legit null put is rare but legal —
            // hence trace-gated rather than unconditional).
            if matches!(v, Value::Null)
                && std::env::var_os("RAVA_DBG_NULLPUT").is_some()
                && matches!(&kv, Value::Str(s) if s.contains(':'))
            {
                eprintln!("[NULL-PUT] key={kv:?} into map#{id}\n{}", vm.backtrace());
            }
            vm.heap.map_put(id, k, kv, v).unwrap_or(Value::Null)
        }
        // guava BiMap.inverse(): a fresh native map with key/value swapped.
        "inverse" => {
            // A LIVE view over the forward map, not a snapshot: callers cache
            // the inverse once before any entries exist (MC's RegistryMaterials
            // grabs it in its constructor), so a copy would stay empty forever.
            // Marked with $inverseof; map_method redirects its operations.
            let inv = vm.heap.alloc("com/google/common/collect/HashBiMap");
            vm.heap.put_field(inv, "$inverseof", Value::Ref(obj));
            Value::Ref(inv)
        }
        "get" | "getProperty" | "getOrDefault" => {
            let k = key.unwrap_or_default();
            let got = vm.heap.map_get_access(obj, id, &k);
            got.unwrap_or_else(|| args.get(2).cloned().unwrap_or(Value::Null))
        }
        // guava `MapMakerInternalMap.getEntry(key)` — backs the Interner: it reads
        // `getEntry(sample).getKey()` to return the CANONICAL stored instance. Return a
        // Map$Entry over the stored key (getKey routes to entry_method like any entry),
        // or null when absent. Gated → default build byte-identical.
        #[cfg(feature = "next-gen-gc")]
        "getEntry" => match vm.heap.map_get_key(id, &key.clone().unwrap_or_default()) {
            Some(canon) => {
                let v = vm.heap.map_get(id, &key.unwrap_or_default()).unwrap_or(Value::Null);
                // A SNAPSHOT entry (no owner, no contract marker) — arena-born. The two
                // `entrySet` arms below stay on handles deliberately: a VIEW entry also carries
                // `$entryowner` and an immutability marker, and `immutable::mark` writes only
                // through `Value::Ref`, so migrating those would silently drop the contract that
                // makes `Map.of(..).entrySet()...setValue(v)` throw.
                crate::map::make_entry(vm, canon, v)
            }
            None => Value::Null,
        },
        "setProperty" => {
            let k = key.unwrap_or_default();
            let kv = key_val.unwrap_or(Value::Null);
            let v = args.get(2).cloned().unwrap_or(Value::Null);
            vm.heap.map_put(id, k, kv, v).unwrap_or(Value::Null)
        }
        // Properties.store(Writer|OutputStream, comment): serialize key=value lines.
        "store" | "save" | "storeToXML" => {
            let mut out = String::new();
            if let Some(Value::Str(c)) = args.get(2) { out.push_str(&format!("#{c}\n")); }
            for (k, v) in vm.heap.map_entries(id) {
                let ks = vm.value_to_string(&k)?;
                let vs = vm.value_to_string(&v)?;
                out.push_str(&format!("{ks}={vs}\n"));
            }
            if let Some(Value::Ref(w)) = args.get(1) {
                // Follow the writer/stream's $out chain to the buffering target.
                let mut t = *w;
                while let Value::Ref(o) = vm.heap.get_field(t, "$out") { t = o; }
                if vm.heap.class_of(t).unwrap_or("").contains("OutputStream") {
                    crate::nio::os_write(vm, Some(&Value::Ref(t)), out.as_bytes());
                } else {
                    // A char Writer buffers; its flush/close persists $sb to $path.
                    vm.heap.sb_append(t, &out);
                }
            }
            return Ok(Some(None));
        }
        "containsKey" => Value::Int(vm.heap.map_contains(id, &key.unwrap_or_default()) as i32),
        // remove(key) → the removed VALUE (1-arg), OR remove(key, value) → boolean
        // (2-arg: remove only if the current value matches, Java Map.remove(k,v) /
        // fastutil remove(K,V)Z). The 2-arg form returns Z, so returning the value
        // (or nothing) underflowed the caller's ifeq — ChunkMap.scheduleUnload's
        // `pendingUnloads.remove(key, holder)` crashed tick 1 this way.
        "remove" => {
            let k = key.unwrap_or_default();
            if args.len() >= 3 {
                let want = args.get(2).cloned().unwrap_or(Value::Null);
                let matches = vm
                    .heap
                    .map_get(id, &k)
                    .is_some_and(|cur| structural_key(vm, &cur) == structural_key(vm, &want));
                if matches {
                    vm.heap.map_remove(id, &k);
                    Value::Int(1)
                } else {
                    Value::Int(0)
                }
            } else {
                vm.heap.map_remove(id, &k).unwrap_or(Value::Null)
            }
        }
        "size" => Value::Int(vm.heap.map_size(id) as i32),
        "comparator" => Value::Null, // natural ordering
        "containsValue" => {
            let needle = args.get(1).map(|v| v.display()).unwrap_or_default();
            Value::Int(vm.heap.map_entries(id).iter().any(|(_, v)| v.display() == needle) as i32)
        }
        "isEmpty" => Value::Int((vm.heap.map_size(id) == 0) as i32),
        "clear" => { vm.heap.map_clear(id); return Ok(Some(None)); }
        "load" => {
            // Properties.load(InputStream | Reader): drain and parse key=value
            // lines. A Reader (InputStreamReader/BufferedReader) holds no bytes
            // itself — it links the underlying byte stream via $stream; follow
            // that chain to the object that actually carries $buf. (MC loads
            // server.properties via load(Reader), so without this the server
            // booted with zero properties = all defaults.)
            if let Some(Value::Ref(s0)) = args.get(1) {
                let mut s = *s0;
                let mut hops = 0;
                while !matches!(vm.heap.get_field(s, "$buf"), Value::Array(_))
                    && !matches!(vm.heap.get_field_syn(s, crate::heap::Syn::Path), Value::Str(_))
                    && hops < 6
                {
                    match vm.heap.get_field(s, "$stream") {
                        Value::Ref(n) => { s = n; hops += 1; }
                        _ => break,
                    }
                }
                let buf = match vm.heap.get_field(s, "$buf") { Value::Array(b) => Some(b), _ => None };
                let s = &s;
                // A FileInputStream is `$path`-backed (no in-memory `$buf`): read
                // the file. MC reads eula.txt as `new FileInputStream("eula.txt")`,
                // so without this `eula=true` was never parsed → server refused to
                // start ("You need to agree to the EULA").
                let text = if let Some(b) = buf {
                    let pos = vm.heap.get_field_syn(*s, crate::heap::Syn::Pos).as_int().unwrap_or(0) as usize;
                    let all = vm.heap.array_bytes(b).unwrap_or_default();
                    vm.heap.put_field_syn(*s, crate::heap::Syn::Pos, Value::Int(all.len() as i32));
                    String::from_utf8_lossy(if pos < all.len() { &all[pos..] } else { &[] }).into_owned()
                } else if let Value::Str(p) = vm.heap.get_field_syn(*s, crate::heap::Syn::Path) {
                    std::fs::read_to_string(&*p).unwrap_or_default()
                } else {
                    String::new()
                };
                {
                    for line in text.lines() {
                        let l = line.trim_start();
                        if l.is_empty() || l.starts_with('#') || l.starts_with('!') { continue; }
                        let sep = l.find(|c| c == '=' || c == ':');
                        if let Some(i) = sep {
                            let k = l[..i].trim().to_string();
                            let v = l[i + 1..].trim().to_string();
                            vm.heap.map_put(id, crate::heap::MapKey::str(format!("s:{k}")), Value::Str(k.into()), Value::Str(v.into()));
                        }
                    }
                }
            }
            return Ok(Some(None));
        }
        "putAll" => {
            // EITHER REPRESENTATION, same reason as the copy-constructor above.
            if let Some(src) = args.get(1).cloned() {
                for (k, v) in crate::map::any_map_entries(vm, &src) {
                    map_put_gc_safe_mk(vm, id, obj, k, v)?;
                }
            }
            return Ok(Some(None));
        }
        // `Map.replace` — ABSENT ENTIRELY until now, so a guest calling it died with
        // `UnsatisfiedLinkError: invokevirtual java/util/LinkedHashMap.replace` rather than
        // getting a wrong answer (bench/MapViewSem.java, which is how it was found: the
        // probe's output simply STOPPED at that line).
        //
        // Both overloads, and both use CONTAINS-KEY presence rather than putIfAbsent's
        // value-is-null rule — the JDK's default methods differ here on purpose: a key
        // mapped to null is ABSENT to `putIfAbsent` and PRESENT to `replace`.
        "replace" => {
            let k = key.unwrap_or_default();
            if !vm.heap.map_contains(id, &k) {
                // 2-arg form answers null, 3-arg form answers false.
                if args.len() >= 4 { Value::Int(0) } else { Value::Null }
            } else if args.len() >= 4 {
                // replace(key, oldValue, newValue): only when the current value matches.
                let want = args.get(2).cloned().unwrap_or(Value::Null);
                let cur = vm.heap.map_get(id, &k).unwrap_or(Value::Null);
                // Same comparison `remove(key, value)` twenty lines below uses, so the two
                // conditional mutators cannot disagree about what "the current value" means.
                let same = structural_key(vm, &cur) == structural_key(vm, &want);
                if same {
                    let kv = key_val.unwrap_or(Value::Null);
                    let v = args.get(3).cloned().unwrap_or(Value::Null);
                    vm.heap.map_put(id, k, kv, v);
                }
                Value::Int(same as i32)
            } else {
                let old = vm.heap.map_get(id, &k).unwrap_or(Value::Null);
                let kv = key_val.unwrap_or(Value::Null);
                let v = args.get(2).cloned().unwrap_or(Value::Null);
                vm.heap.map_put(id, k, kv, v);
                old
            }
        }
        "putIfAbsent" => {
            let k = key.unwrap_or_default();
            // `Map.putIfAbsent` is defined on the VALUE, not on key presence: a key
            // mapped to null counts as absent and IS overwritten (`if (v == null)
            // v = put(key, value)`). Reading presence alone left a null-valued entry
            // untouched and answered null, so the caller saw its own value rejected.
            if let Some(existing) = present_value(vm, id, &k) {
                existing
            } else {
                let kv = key_val.unwrap_or(Value::Null);
                let v = args.get(2).cloned().unwrap_or(Value::Null);
                vm.heap.map_put(id, k, kv, v);
                Value::Null
            }
        }
        "computeIfAbsent" => {
            let k = key.unwrap_or_default();
            // Same null-is-absent rule as putIfAbsent above.
            if let Some(existing) = present_value(vm, id, &k) {
                existing
            } else {
                let kv = key_val.clone().unwrap_or(Value::Null);
                let f = args.get(2).cloned().unwrap_or(Value::Null);
                // Root kv across the mapping fn (it runs guest bytecode → can move
                // the arena); read it back FORWARDED before storing it as the entry
                // key-value — else a moved young kv is written back stale.
                let arr = vm.heap.alloc_values("Ljava/lang/Object;", vec![kv]);
                let kv0 = vm.heap.array_get(arr, 0).unwrap_or(Value::Null);
                let v = vm.apply(&f, vec![kv0])?.unwrap_or(Value::Null);
                let kv = vm.heap.array_get(arr, 0).unwrap_or(Value::Null);
                // A null mapping result records NOTHING (`if (newValue != null)`).
                // Storing it inserted a phantom key, so a later containsKey/size/
                // iteration saw an entry the JDK never created.
                if !matches!(v, Value::Null) {
                    vm.heap.map_put(id, k, kv, v.clone());
                }
                v
            }
        }
        // compute / computeIfPresent: BiFunction(key, oldValue) → newValue, with a
        // null result REMOVING the entry (JDK: "if the function returns null, the
        // mapping is removed"). computeIfPresent additionally skips the function
        // entirely when there is no non-null current value.
        "compute" | "computeIfPresent" => {
            let k = key.unwrap_or_default();
            let cur = present_value(vm, id, &k);
            if name == "computeIfPresent" && cur.is_none() {
                Value::Null
            } else {
                let kv = key_val.clone().unwrap_or(Value::Null);
                let f = args.get(2).cloned().unwrap_or(Value::Null);
                // Root kv across the remapping fn (same moving-collection hazard as
                // computeIfAbsent above); the bi_args are consumed by apply (rooted in
                // the callee frame), only the kept-for-map_put kv needs the shield.
                let arr = vm.heap.alloc_values("Ljava/lang/Object;", vec![kv]);
                let kv0 = vm.heap.array_get(arr, 0).unwrap_or(Value::Null);
                let v = vm.apply(&f, vec![kv0, cur.unwrap_or(Value::Null)])?.unwrap_or(Value::Null);
                let kv = vm.heap.array_get(arr, 0).unwrap_or(Value::Null);
                store_or_remove(vm, id, k, kv, &v);
                v
            }
        }
        "merge" => {
            let k = key.unwrap_or_default();
            let given = value_reread.clone().or_else(|| args.get(2).cloned()).unwrap_or(Value::Null);
            let cur = present_value(vm, id, &k);
            let kv = key_val.clone().unwrap_or(Value::Null);
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", vec![kv]);
            let v = match cur {
                // JDK `merge`: with no (or a null) current value the remapping
                // function is NOT called — the given value is stored as-is. Calling
                // it with a null first argument made the standard counter idiom
                // `merge(k, 1, Integer::sum)` throw NPE unboxing that null on the
                // FIRST occurrence of every key.
                None => given,
                Some(old) => {
                    let f = args.get(3).cloned().unwrap_or(Value::Null);
                    vm.apply(&f, vec![old, given])?.unwrap_or(Value::Null)
                }
            };
            let kv = vm.heap.array_get(arr, 0).unwrap_or(Value::Null);
            store_or_remove(vm, id, k, kv, &v);
            v
        }
        "forEach" => {
            // Root the snapshot in a heap Object[] (k0,v0,k1,v1…): each apply runs
            // guest bytecode that can allocate → a MOVING arena collection mid-loop
            // must forward the REMAINING entries. A bare Rust Vec is invisible to
            // every root scan, so a moved young VALUE reached the next apply stale —
            // the post-recipes boot killer (Map<Identifier,Advancement>.forEach whose
            // per-entry validate allocates; the stale Advancement's slot was reset →
            // dispatch on class-id 0 → ULE). Same rooting as transformEntries above;
            // read each pair back FORWARDED from the array.
            //
            // `sorted_entries`, not the raw table: `Map.forEach` on a `TreeMap` (and on the
            // `ImmutableSortedMap` that `copyOf` now builds as one) visits keys in key order,
            // exactly as `entrySet()`/`keySet()`/`values()` above do. This arm was the one
            // iteration view still reading insertion order -- `bench/SortedSem.java` caught it
            // on its first run, the same probe that guards the block-state-id fix.
            let pairs = sorted_entries(vm, id, &owner_class);
            let mut flat = Vec::with_capacity(pairs.len() * 2);
            for (k, v) in pairs {
                flat.push(k);
                flat.push(v);
            }
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", flat);
            let n = vm.heap.array_len(arr).unwrap_or(0);
            let mut i = 0;
            while i + 1 < n {
                let k = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                let v = vm.heap.array_get(arr, i + 1).unwrap_or(Value::Null);
                // Re-read the BiConsumer per iteration: `args` is rooted in place by
                // try_invoke's NativeArgsGuard, so this is always the FORWARDED
                // functional — a pre-loop clone would go stale if an arena-Obj lambda
                // moved during an earlier iteration's apply.
                let f = args.get(1).cloned().unwrap_or(Value::Null);
                vm.apply(&f, vec![k, v])?;
                i += 2;
            }
            return Ok(Some(None));
        }
        // `Map.replaceAll(BiFunction)` — the write-back twin of `forEach`. Same rooting
        // for the same reason: the function runs guest bytecode that can allocate, so a
        // moving arena collection must forward the entries still to be visited, and a
        // bare Rust Vec is invisible to every root scan.
        //
        // The key is re-derived per entry rather than kept from the snapshot, because a
        // guest `apply` can move an arena key between the read and the store.
        "replaceAll" => {
            let pairs = vm.heap.map_entries(id);
            let mut flat = Vec::with_capacity(pairs.len() * 2);
            for (k, v) in pairs {
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
                let nv = vm.apply(&f, vec![k, v])?.unwrap_or(Value::Null);
                // Re-read the key FORWARDED after the apply, then store.
                let k2 = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                let mk = value_map_key_in(vm, id, &k2)?;
                vm.heap.map_put(id, mk, k2, nv);
                i += 2;
            }
            return Ok(Some(None));
        }
        "toString" => {
            let mut parts = Vec::new();
            // SORTED, like every other view below: `{pear=3, apple=1}` where the JVM
            // prints `{apple=1, pear=3}` is a wrong answer, not a formatting detail.
            for (k, v) in sorted_entries(vm, id, &owner_class) {
                parts.push(format!("{}={}", vm.value_to_string(&k)?, vm.value_to_string(&v)?));
            }
            Value::Str(format!("{{{}}}", parts.join(", ")).into())
        }
        "keySet" => {
            let ks: Vec<Value> = sorted_entries(vm, id, &owner_class).into_iter().map(|(k, _)| k).collect();
            let c = collection(vm, ks);
            // Tag as a live key-view of this map so Iterator.remove() deletes the
            // key from the backing map (MC's advancement build relies on it).
            if let Value::Ref(ch) = c {
                vm.heap.put_field(ch, "$mapview", Value::Str("key".into()));
                vm.heap.put_field(ch, "$mapowner", Value::Ref(obj));
            }
            // A view REFUSES addition and permits removal — see immutable::VIEW_NO_ADD.
            crate::immutable::mark(vm, &c, crate::immutable::VIEW_NO_ADD);
            if let Value::Ref(ch) = c { crate::view::note_map_view(ch); }
            // A view of an unmodifiable map is unmodifiable (immutable::inherit).
            crate::immutable::inherit(vm, obj, &c);
            c
        }
        "values" => {
            let vs: Vec<Value> = sorted_entries(vm, id, &owner_class).into_iter().map(|(_, v)| v).collect();
            let c = collection(vm, vs);
            // Live value-view: values().remove(v)/clear() delete the mapping that
            // holds v from the backing map, exactly as keySet()/entrySet() do.
            if let Value::Ref(ch) = c {
                vm.heap.put_field(ch, "$mapview", Value::Str("value".into()));
                vm.heap.put_field(ch, "$mapowner", Value::Ref(obj));
            }
            // A view REFUSES addition and permits removal — see immutable::VIEW_NO_ADD.
            crate::immutable::mark(vm, &c, crate::immutable::VIEW_NO_ADD);
            if let Value::Ref(ch) = c { crate::view::note_map_view(ch); }
            crate::immutable::inherit(vm, obj, &c);
            c
        }
        "entrySet" => {
            // ⭐**CACHE THE VIEW, THE WAY THE JVM DOES.** `HashMap.entrySet()` returns a field —
            // the same `EntrySet` object on every call, built once and lazily. This arm instead
            // MATERIALISES the whole map every time: one `Map$Entry` handle per mapping, three
            // string-keyed field writes each, an immutability inherit each, then a collection.
            // Measured on a FOUR-entry map (`bench/BridgeCost.java`): **6,115 ns net, against 0
            // on the JVM** — the largest single-native gap in that table, and the handle census
            // agrees about the volume (`Map$Entry` is 48.1% of 6.23 M handle births a boot).
            //
            // A cached derivation is only sound while the thing it was derived from has not
            // changed, so `MapObj::version` counts mutations — every path that can change what
            // `map_entries` returns lives in `heap::maps` and bumps it. Same version ⇒ the same
            // mapping set in the same order ⇒ the view already built IS the answer.
            //
            // The cache lives in a field on the MAP OBJECT rather than in a Rust side table, so
            // the collector already scans it: a view held only by a table the root walk does not
            // know would be freed and its handle reused, which is a silent wrong-object bug.
            // Returning the same object is also strictly closer to the JVM than returning a
            // fresh one — `m.entrySet() == m.entrySet()` is true there and was false here.
            let ver = vm.heap.map_version(id);
            let cached = match (
                vm.heap.get_field(obj, "$entryview"),
                vm.heap.get_field(obj, "$entryviewv"),
            ) {
                (Value::Ref(h), Value::Int(v)) if h != 0 && v as u32 == ver => Some(Value::Ref(h)),
                _ => None,
            };
            if let Some(hit) = cached {
                return Ok(Some(Some(hit)));
            }
            // ROOTED (ledger rows 340/341): the loop holds every already-built entry and every
            // key and value across `heap.alloc`, and a Rust local is invisible to the root walk.
            let src = sorted_entries(vm, id, &owner_class);
            let n = src.len();
            let mut roots: Vec<Value> = Vec::with_capacity(n * 3);
            for (k, v) in src {
                roots.push(k);
                roots.push(v);
            }
            roots.resize(n * 3, Value::Null);
            let base = crate::frames::native_args_push(roots.as_ptr(), roots.len());
            for i in 0..n {
                let e = vm.heap.alloc("java/util/Map$Entry");
                let (k, v) = (roots[i * 2].clone(), roots[i * 2 + 1].clone());
                vm.heap.put_field_syn(e, crate::heap::Syn::Key, k);
                vm.heap.put_field_syn(e, crate::heap::Syn::Val, v);
                // An entrySet() entry is a VIEW of its mapping: setValue writes
                // through to the map. Only this arm stamps the owner, so a
                // standalone Map.entry(k,v) stays the immutable snapshot it is.
                vm.heap.put_field(e, "$entryowner", Value::Ref(obj));
                // `Map.of(..).entrySet().iterator().next().setValue(v)` throws on
                // the JVM: the ENTRY carries the map's contract, not just the set.
                let ev = Value::Ref(e);
                crate::immutable::inherit(vm, obj, &ev);
                roots[n * 2 + i] = ev;
            }
            let entries: Vec<Value> = roots[n * 2..].to_vec();
            crate::frames::native_args_truncate(base);
            let c = collection(vm, entries);
            if let Value::Ref(ch) = c {
                vm.heap.put_field(ch, "$mapview", Value::Str("entry".into()));
                vm.heap.put_field(ch, "$mapowner", Value::Ref(obj));
            }
            // A view REFUSES addition and permits removal — see immutable::VIEW_NO_ADD.
            crate::immutable::mark(vm, &c, crate::immutable::VIEW_NO_ADD);
            if let Value::Ref(ch) = c { crate::view::note_map_view(ch); }
            crate::immutable::inherit(vm, obj, &c);
            // Publish the derivation beside the version it was derived at. Written AFTER every
            // stamp above, so a cache hit hands back a fully-formed view and never a half-marked
            // one.
            vm.heap.put_field(obj, "$entryview", c.clone());
            vm.heap.put_field(obj, "$entryviewv", Value::Int(ver as i32));
            c
        }
        // THE NAVIGABLE SURFACE. `navigableKeySet` was an UnsatisfiedLinkError — a loud
        // failure rather than a wrong answer, but still a hole, and every one of these
        // is a projection of `sorted_entries` so they cost nothing beyond the order
        // that arm already establishes. The descending forms reverse it; the range
        // forms cut it. `sorted_entries` returns insertion order for a map this tree
        // cannot order without a guest frame, and these inherit that limit exactly.
        "navigableKeySet" | "descendingKeySet" => {
            let mut ks: Vec<Value> =
                sorted_entries(vm, id, &owner_class).into_iter().map(|(k, _)| k).collect();
            if name.starts_with("descending") {
                ks.reverse();
            }
            let c = collection(vm, ks);
            if let Value::Ref(ch) = c {
                // NOT a live key-view: a DESCENDING set's removal semantics are not the
                // forward view's, and claiming otherwise would delete the wrong mapping.
                if name == "navigableKeySet" {
                    vm.heap.put_field(ch, "$mapview", Value::Str("key".into()));
                    vm.heap.put_field(ch, "$mapowner", Value::Ref(obj));
                }
            }
            crate::immutable::inherit(vm, obj, &c);
            c
        }
        "descendingMap" => {
            let mut entries = sorted_entries(vm, id, &owner_class);
            entries.reverse();
            new_map_from(vm, &owner_class, entries)?
        }
        // headMap(to[, inclusive]) / tailMap(from[, inclusive]) / subMap(from, to) and
        // subMap(from, fromInc, to, toInc). The bound test reuses `natural_cmp`, so a
        // range over keys this tree cannot order yields an EMPTY map rather than a
        // wrongly-populated one.
        "headMap" | "tailMap" | "subMap" => {
            let entries = sorted_entries(vm, id, &owner_class);
            let four = name == "subMap" && args.len() >= 5;
            let (lo, lo_inc, hi, hi_inc) = match name {
                "headMap" => (
                    None,
                    true,
                    args.get(1).cloned(),
                    matches!(args.get(2), Some(Value::Int(1))),
                ),
                "tailMap" => (
                    args.get(1).cloned(),
                    !matches!(args.get(2), Some(Value::Int(0))),
                    None,
                    false,
                ),
                _ if four => (
                    args.get(1).cloned(),
                    matches!(args.get(2), Some(Value::Int(1))),
                    args.get(3).cloned(),
                    matches!(args.get(4), Some(Value::Int(1))),
                ),
                _ => (args.get(1).cloned(), true, args.get(2).cloned(), false),
            };
            let kept: Vec<(Value, Value)> = entries
                .into_iter()
                .filter(|(k, _)| {
                    let above = match &lo {
                        Some(b) => match natural_cmp(vm, k, b) {
                            Some(std::cmp::Ordering::Less) => false,
                            Some(std::cmp::Ordering::Equal) => lo_inc,
                            Some(_) => true,
                            None => false,
                        },
                        None => true,
                    };
                    let below = match &hi {
                        Some(b) => match natural_cmp(vm, k, b) {
                            Some(std::cmp::Ordering::Greater) => false,
                            Some(std::cmp::Ordering::Equal) => hi_inc,
                            Some(_) => true,
                            None => false,
                        },
                        None => true,
                    };
                    above && below
                })
                .collect();
            new_map_from(vm, &owner_class, kept)?
        }
        // The four single-key probes. `higher`/`lower` are strict, `ceiling`/`floor`
        // are not; all four answer null when nothing qualifies, as the JDK does.
        "higherKey" | "lowerKey" | "ceilingKey" | "floorKey" => {
            let entries = sorted_entries(vm, id, &owner_class);
            let bound = args.get(1).cloned().unwrap_or(Value::Null);
            let want_up = name == "higherKey" || name == "ceilingKey";
            let strict = name == "higherKey" || name == "lowerKey";
            let mut best: Option<Value> = None;
            for (k, _) in entries.into_iter() {
                let Some(ord) = natural_cmp(vm, &k, &bound) else { continue };
                let ok = match (want_up, strict, ord) {
                    (true, true, std::cmp::Ordering::Greater) => true,
                    (true, false, std::cmp::Ordering::Greater | std::cmp::Ordering::Equal) => true,
                    (false, true, std::cmp::Ordering::Less) => true,
                    (false, false, std::cmp::Ordering::Less | std::cmp::Ordering::Equal) => true,
                    _ => false,
                };
                if !ok {
                    continue;
                }
                // The entries are already in order, so the FIRST qualifying key going up
                // and the LAST going down are the answers.
                if want_up {
                    best = Some(k);
                    break;
                }
                best = Some(k);
            }
            best.unwrap_or(Value::Null)
        }
        // First/last entry by iteration order (LinkedHashMap = insertion order,
        // TreeMap = sorted — our map preserves insertion order). poll* also
        // removes it. World-spawn selection drains a candidate map this way.
        "firstEntry" | "lastEntry" | "pollFirstEntry" | "pollLastEntry" => {
            let entries = sorted_entries(vm, id, &owner_class);
            let is_last = name.contains("last") || name.contains("Last");
            let pick = if is_last { entries.last() } else { entries.first() }.cloned();
            match pick {
                Some((k, v)) => {
                    if name.starts_with("poll") {
                        let sk = value_map_key_in(vm, id, &k)?;
                        vm.heap.map_remove(id, &sk);
                    }
                    crate::map::make_entry(vm, k, v)
                }
                None => Value::Null,
            }
        }
        // fastutil primitive-key first/last: firstLongKey()J / firstIntKey()I /
        // lastLongKey()J etc. Same insertion-order pick as firstKey, but the
        // stored key Value (Long/Int) is returned directly as the primitive.
        // ChunkTaskPriorityQueue drains its Long2ObjectLinkedOpenHashMap via
        // firstLongKey during world-spawn selection.
        "firstKey" | "lastKey"
        | "firstLongKey" | "lastLongKey" | "firstIntKey" | "lastIntKey" => {
            let entries = sorted_entries(vm, id, &owner_class);
            let is_last = name.starts_with("last");
            (if is_last { entries.last() } else { entries.first() })
                .map(|(k, _)| k.clone()).unwrap_or(Value::Null)
        }
        // fastutil increment map: addTo(key, incr) adds incr to the value at key
        // (absent == defaultReturnValue 0), returns the PREVIOUS value. Object2Int
        // (ServerLevel) and Int2Int (PalettedContainer) drive this.
        "addTo" => {
            let k = key.clone().unwrap_or_default();
            let kv = key_val.clone().unwrap_or(Value::Null);
            let incr = args.get(2).and_then(Value::as_int).unwrap_or(0);
            let old = vm.heap.map_get(id, &k).and_then(|v| v.as_int()).unwrap_or(0);
            vm.heap.map_put(id, k, kv, Value::Int(old + incr));
            Value::Int(old)
        }
        // fastutil primitive-valued removes: removeBoolean/removeInt/removeLong —
        // remove the key, return its value coerced to the primitive (absent ==
        // defaultReturnValue: 0 / false). PlayerMap uses Object2Boolean.removeBoolean.
        "removeBoolean" | "removeInt" | "removeLong" | "removeShort" => {
            let k = key.clone().unwrap_or_default();
            let removed = vm.heap.map_remove(id, &k);
            match name {
                "removeLong" => Value::Long(removed.and_then(|v| v.as_long()).unwrap_or(0)),
                _ => Value::Int(removed.and_then(|v| v.as_int()).unwrap_or(0)),
            }
        }
        // fastutil Linked-map LRU reorder. getAndMoveToFirst(key): return the
        // value AND move the entry to the front (MRU). putAndMoveToFirst(key,val):
        // insert/replace then move to front, returning the previous value.
        // RegionFileStorage uses these to keep its open-region-file cache in LRU
        // order (removeLast evicts the least-recently-used).
        "getAndMoveToFirst" => {
            let k = key.unwrap_or_default();
            vm.heap.map_move_to_first(id, &k).unwrap_or(Value::Null)
        }
        "putAndMoveToFirst" => {
            let k = key.unwrap_or_default();
            let kv = key_val.unwrap_or(Value::Null);
            let v = args.get(2).cloned().unwrap_or(Value::Null);
            let prev = vm.heap.map_put(id, k.clone(), kv, v).unwrap_or(Value::Null);
            vm.heap.map_move_to_first(id, &k);
            prev
        }
        // fastutil linked-map removeFirst/removeLast: remove the insertion-order
        // first/last entry and return its VALUE (Object). ChunkTaskPriorityQueue
        // drains its Long2ObjectLinkedOpenHashMap via removeFirst at world-spawn.
        "removeFirst" | "removeLast" => {
            let entries = vm.heap.map_entries(id);
            let is_last = name == "removeLast";
            match (if is_last { entries.last() } else { entries.first() }).cloned() {
                Some((k, v)) => {
                    let sk = value_map_key_in(vm, id, &k)?;
                    vm.heap.map_remove(id, &sk);
                    v
                }
                None => Value::Null,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
