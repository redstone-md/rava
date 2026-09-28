//! The Collections and Arrays static methods.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::stream::*;

/// `Collections.sort/reverse/shuffle/addAll` mutate their FIRST argument through
/// that collection's own `set`/`add`, so an unmodifiable target refuses them just
/// as the direct call does (`Collections.sort(List.of("b","a"))` throws UOE).
/// Without this the guard in `coll_op` was bypassed entirely — these arms write
/// `$elems` directly.
fn guard_target(
    vm: &mut Vm,
    target: Option<&Value>,
    op: crate::immutable::Op,
    would_change: bool,
) -> Result<(), VmError> {
    if let Some(Value::Ref(h)) = target {
        let kind = crate::immutable::kind_of(vm, *h);
        if kind != crate::immutable::NONE && crate::immutable::refuses(kind, op, would_change) {
            return Err(crate::immutable::uoe(vm));
        }
    }
    Ok(())
}

/// java.util.Collections static helpers.
pub(crate) fn collections_static(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let ret = match name {
        // The empty/singleton/nCopies singletons are the AbstractList-backed
        // contract kind: `add` throws, but the inherited `removeIf`/`sort`/`clear`
        // bodies never reach their throwing primitive on an empty receiver.
        "emptyList" => {
            let v = make_list(vm, Vec::new());
            crate::immutable::mark(vm, &v, crate::immutable::FIXED_SET_UOE);
            v
        }
        // The empty SET family builds a Set-classed receiver for the same reason
        // `Set.of` does — see `make_set`.
        "emptySet" | "emptySortedSet" | "emptyNavigableSet" => {
            let v = make_set(vm, Vec::new());
            crate::immutable::mark(vm, &v, crate::immutable::FIXED_SET_UOE);
            v
        }
        // Set view backed by a Map. Callers use it as a Set (add/contains/
        // iterate); our element-backed Set covers that. The passed map is empty.
        "newSetFromMap" => {
            let h = vm.heap.alloc("java/util/HashSet");
            let arr = vm.heap.alloc_values(OBJ, Vec::new());
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Value::Ref(h)
        }
        "emptyMap" | "emptySortedMap" | "emptyNavigableMap" => {
            let v = Value::Ref(vm.heap.alloc("java/util/HashMap"));
            crate::immutable::mark(vm, &v, crate::immutable::FIXED_SET_UOE);
            v
        }
        "singletonList" | "singleton" => {
            let e = vec![args.first().cloned().unwrap_or(Value::Null)];
            // `Collections.singleton` is a SET, `singletonList` is a List.
            let v = if name == "singleton" { make_set(vm, e) } else { make_list(vm, e) };
            crate::immutable::mark(vm, &v, crate::immutable::SINGLETON);
            v
        }
        // singletonMap(k, v) had no arm at all, so it left the native layer as an
        // UnsatisfiedLinkError. A one-entry native map is the whole contract here
        // (the immutability half is the gap noted for the List.of family).
        "singletonMap" => {
            let h = vm.heap.alloc("java/util/LinkedHashMap");
            let id = vm.heap.alloc_map();
            vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
            let k = args.first().cloned().unwrap_or(Value::Null);
            let v = args.get(1).cloned().unwrap_or(Value::Null);
            crate::map::map_put_gc_safe(vm, id, k, v)?;
            let m = Value::Ref(h);
            crate::immutable::mark(vm, &m, crate::immutable::SINGLETON);
            m
        }
        // binarySearch(list, key [, comparator]): the JDK contract is exact —
        // the element index when found, else `-(insertionPoint) - 1`. Missing
        // entirely before, so every call was an UnsatisfiedLinkError.
        "binarySearch" => {
            let elems = elems_of(vm, args.first());
            let key = args.get(1).cloned().unwrap_or(Value::Null);
            let cmp = args.get(2).cloned().unwrap_or(Value::Null);
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_BINARY_SEARCH, elems.len()); // [CMP-CALLER] entry mark
            let (mut lo, mut hi) = (0i32, elems.len() as i32 - 1);
            let mut found = None;
            while lo <= hi {
                let mid = lo + (hi - lo) / 2;
                let c = vm.compare(&cmp, &elems[mid as usize], &key)?;
                if c < 0 {
                    lo = mid + 1;
                } else if c > 0 {
                    hi = mid - 1;
                } else {
                    found = Some(mid);
                    break;
                }
            }
            Value::Int(found.unwrap_or(-(lo + 1)))
        }
        // Collections.addAll(Collection, T... elements) — append the ARRAY's
        // elements (varargs arrive as one array value), return changed.
        "addAll" => {
            let add = elems_of(vm, args.get(1));
            let changed = !add.is_empty();
            // `Collections.addAll` is a LOOP of `c.add(e)` — not a call to
            // `c.addAll` — so with no elements it never reaches a mutator and never
            // throws, even on a `List.of` receiver. That is why the guard is skipped
            // outright here rather than passed `would_change`: for an `IMMUTABLE`
            // receiver `refuses` is unconditional, and rightly so, because a DIRECT
            // `List.of("a").addAll(List.of())` does throw. `java -Xint` separates the
            // two and so must we (bench/ImmutEx.java, `collections-addAll-none`).
            if changed {
                guard_target(vm, args.first(), crate::immutable::Op::InsertAll, true)?;
            }
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                e.extend(add);
                crate::stream::store_elems_value(vm, target, e);
            }
            Value::Int(changed as i32)
        }
        "list" => { let e = elems_of(vm, args.first()); make_list(vm, e) } // Collections.list(Enumeration)
        "emptyIterator" | "emptyListIterator" => iterator_of(vm, Vec::new()),
        "nCopies" => {
            let n = args.first().and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let v = args.get(1).cloned().unwrap_or(Value::Null);
            let l = make_list(vm, vec![v; n]);
            crate::immutable::mark(vm, &l, crate::immutable::FIXED_SET_UOE);
            l
        }
        // disjoint(c1, c2): true iff they share no element (by display key).
        "disjoint" => {
            let a = elems_of(vm, args.first());
            let b: std::collections::HashSet<String> = elems_of(vm, args.get(1)).iter().map(Value::display).collect();
            Value::Int(a.iter().all(|e| !b.contains(&e.display())) as i32)
        }
        "frequency" => {
            let needle = args.get(1).map(Value::display).unwrap_or_default();
            let n = elems_of(vm, args.first()).iter().filter(|e| e.display() == needle).count();
            Value::Int(n as i32)
        }
        // `unmodifiable*` is a LIVE VIEW with its own identity — see
        // `immutable::unmodifiable_view` for why it cannot simply mark the argument.
        n if n.starts_with("unmodifiable") => {
            let src = args.first().cloned().unwrap_or(Value::Null);
            crate::immutable::unmodifiable_view(vm, &src)
        }
        // synchronized*/checked* wrappers keep no extra contract we can observe
        // single-threaded, so they still just return the underlying object.
        n if n.starts_with("synchronized") || n == "checkedList" || n == "checkedMap" => {
            args.first().cloned().unwrap_or(Value::Null)
        }
        "sort" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                // sort/reverse/shuffle all drive `ListIterator.set`, which is only
                // reached when there is an element to assign.
                guard_target(vm, args.first(), crate::immutable::Op::Write, !e.is_empty())?;
                let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_COLLECTIONS_SORT, e.len()); // [CMP-CALLER] entry mark
                sort_values(vm, &mut e, args.get(1))?;
                crate::stream::store_elems_value(vm, target, e);
            }
            return Ok(Some(None));
        }
        // `swap(list, i, j)` and `rotate(list, n)` — neither existed, and both are the
        // kind of one-liner a guest reaches for without a second thought.
        // `Collections.fill(list, obj)` — every slot set to the same value, the list's
        // size unchanged. Absent.
        "fill" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let e = elems_of(vm, args.first());
                guard_target(vm, args.first(), crate::immutable::Op::Write, !e.is_empty())?;
                let v = args.get(1).cloned().unwrap_or(Value::Null);
                let filled: Vec<Value> = std::iter::repeat_n(v, e.len()).collect();
                crate::stream::store_elems_value(vm, target, filled);
            }
            return Ok(Some(None));
        }
        "swap" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                guard_target(vm, args.first(), crate::immutable::Op::Write, !e.is_empty())?;
                let (i, j) = (
                    args.get(1).and_then(Value::as_int).unwrap_or(0),
                    args.get(2).and_then(Value::as_int).unwrap_or(0),
                );
                match (usize::try_from(i).ok(), usize::try_from(j).ok()) {
                    (Some(i), Some(j)) if i < e.len() && j < e.len() => {
                        e.swap(i, j);
                        crate::stream::store_elems_value(vm, target, e);
                    }
                    _ => {
                        let bad = if usize::try_from(i).ok().is_none_or(|u| u >= e.len()) { i } else { j };
                        let ex = vm.make_exception(
                            "java/lang/IndexOutOfBoundsException",
                            &format!("Index {bad} out of bounds for length {}", e.len()),
                        );
                        return Err(VmError::Throw(ex));
                    }
                }
            }
            return Ok(Some(None));
        }
        // `rotate(list, distance)` moves each element `distance` places to the RIGHT,
        // wrapping; a negative distance rotates left. The JDK's own definition.
        "rotate" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                guard_target(vm, args.first(), crate::immutable::Op::Write, !e.is_empty())?;
                let n = e.len();
                if n > 1 {
                    let d = args.get(1).and_then(Value::as_int).unwrap_or(0);
                    let k = (((d % n as i32) + n as i32) % n as i32) as usize;
                    e.rotate_right(k);
                    crate::stream::store_elems_value(vm, target, e);
                }
            }
            return Ok(Some(None));
        }
        "reverse" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                guard_target(vm, args.first(), crate::immutable::Op::Write, !e.is_empty())?;
                e.reverse();
                crate::stream::store_elems_value(vm, target, e);
            }
            return Ok(Some(None));
        }
        // shuffle(List [, Random]) — JDK Fisher-Yates: for i=n..2, swap(i-1,
        // rnd.nextInt(i)). MC's EnderDragonBattle (End spike layout) needs the
        // exact stream off the passed Random.
        "shuffle" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                guard_target(vm, args.first(), crate::immutable::Op::Write, !e.is_empty())?;
                let n = e.len();
                let rnd = match args.get(1) {
                    Some(Value::Ref(r)) => *r,
                    _ => vm.heap.alloc("java/util/Random"),
                };
                for i in (2..=n).rev() {
                    let j = crate::natives::random_next_int_bound(vm, rnd, i as i32).max(0) as usize;
                    if j < n { e.swap(i - 1, j); }
                }
                crate::stream::store_elems_value(vm, target, e);
            }
            return Ok(Some(None));
        }
        "max" | "min" => {
            let e = elems_of(vm, args.first());
            // An EMPTY collection has no minimum, and the JDK says so with
            // `NoSuchElementException`. Answering null instead handed the caller a value
            // it never checked for — the emptiness was supposed to be the loud case.
            if e.is_empty() {
                let ex = vm.make_exception("java/util/NoSuchElementException", "");
                return Err(VmError::Throw(ex));
            }
            let cmp = args.get(1).cloned().unwrap_or(Value::Null);
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_COLLECTIONS_MINMAX, e.len()); // [CMP-CALLER] entry mark
            let mut best: Option<Value> = None;
            for x in e {
                match &best {
                    None => best = Some(x),
                    Some(b) => { let r = vm.compare(&cmp, b, &x)?; if (name == "max" && r < 0) || (name == "min" && r > 0) { best = Some(x); } }
                }
            }
            best.unwrap_or(Value::Null)
        }
        "addAll" => {
            if let Some(target) = args.first().filter(|v| matches!(v, Value::Ref(_)) || cfg!(feature = "next-gen-gc")) {
                let mut e = elems_of(vm, args.first());
                e.extend(args[1..].iter().cloned());
                crate::stream::store_elems_value(vm, target, e);
            }
            Value::Int(1)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

/// Element-type descriptor of an array argument, for a factory that must give its
/// result the SAME runtime component type (`Arrays.copyOf`, `Collection.toArray(T[])`).
/// Falls back to `Object` when the argument is not an array we can read.
pub(crate) fn source_array_element(vm: &Vm, v: Option<&Value>) -> String {
    match v {
        Some(Value::Array(h)) => vm.heap.array_element(*h).as_deref().unwrap_or(OBJ).to_string(),
        _ => OBJ.to_string(),
    }
}

/// java.util.Arrays static helpers.
/// `Arrays.hashCode` / `Arrays.deepHashCode` — the 31-fold over the ELEMENTS'
/// own hashes.
///
/// `deep` recurses into a nested array ALL THE WAY, which is what
/// `Arrays.deepHashCode` specifies. Descending exactly one level (the first cut)
/// hashed a doubly-nested array's inner arrays through `element_hash`, which has
/// no `Value::Array` arm and answers the identity hash — so two equal `int[][][]`
/// hashed differently and a `deepEquals`-equal pair broke the hash contract.
///
/// Bounded by `DEEP_DEPTH` for the same reason `arrays_elements_equal` is: a
/// self-referential array is legal Java and must not take the VM down. Beyond it
/// the element falls back to its identity hash.
fn array_hash(vm: &mut Vm, arr: Option<&Value>, deep: bool, depth: u32) -> i32 {
    let vals = match arr {
        Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
        _ => return 0,
    };
    let mut h = 1i32;
    for v in &vals {
        let eh = match v {
            Value::Null => 0,
            Value::Array(_) if deep && depth < DEEP_DEPTH => array_hash(vm, Some(v), true, depth + 1),
            _ => crate::map::element_hash(vm, v),
        };
        h = h.wrapping_mul(31).wrapping_add(eh);
    }
    h
}

/// Element-wise equality for `Arrays.equals` / `Arrays.deepEquals`.
///
/// Elements are compared by `map::content_key` — the same derivation the
/// collections compare by, so `Arrays.equals` and `List.equals` cannot disagree
/// about two elements. A nested array's content key is its identity, which is
/// exactly `Arrays.equals`'s contract; `deep` recurses into it instead.
fn arrays_elements_equal(vm: &mut Vm, a: Option<&Value>, b: Option<&Value>, deep: bool) -> bool {
    arrays_elements_equal_at(vm, a, b, deep, 0)
}

/// Maximum nesting `deepEquals` descends. `Object[] a = new Object[1]; a[0] = a;`
/// is legal Java and `Arrays.deepEquals(a, a)` terminates there because the JDK
/// short-circuits on reference identity first; ours recursed forever and took the
/// whole VM down with it. Both guards are here: the identity short-circuit (which
/// is also what the JDK does, and is free) and a bound for the shapes identity
/// cannot catch — two DISTINCT arrays that point at each other.
const DEEP_DEPTH: u32 = 32;

fn arrays_elements_equal_at(
    vm: &mut Vm,
    a: Option<&Value>,
    b: Option<&Value>,
    deep: bool,
    depth: u32,
) -> bool {
    // `Arrays.deepEquals0`: `e1 == e2` before anything else.
    if let (Some(Value::Array(x)), Some(Value::Array(y))) = (a, b) {
        if x == y {
            return true;
        }
    }
    if depth >= DEEP_DEPTH {
        return false;
    }
    let vals = |vm: &Vm, v: Option<&Value>| -> Vec<Value> {
        match v { Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(), _ => Vec::new() }
    };
    let (x, y) = (vals(vm, a), vals(vm, b));
    if x.len() != y.len() {
        return false;
    }
    for (p, q) in x.iter().zip(y.iter()) {
        let same = match (p, q) {
            (Value::Array(_), Value::Array(_)) if deep => {
                arrays_elements_equal_at(vm, Some(p), Some(q), true, depth + 1)
            }
            _ => crate::map::content_key(vm, p) == crate::map::content_key(vm, q),
        };
        if !same {
            return false;
        }
    }
    true
}

pub(crate) fn arrays_static(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let read = |vm: &Vm, v: Option<&Value>| -> Vec<Value> {
        match v { Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(), _ => Vec::new() }
    };
    let ret = match name {
        "stream" => from_array(vm, args.first()),
        // binarySearch(arr, key) / binarySearch(arr, from, to, key): index of key
        // or -(insertion point) - 1, JDK semantics (array must be sorted).
        "binarySearch" => {
            let vals = read(vm, args.first());
            let (lo, hi, key) = if args.len() >= 4 {
                let lo = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
                let hi = (args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize).min(vals.len());
                (lo, hi, args.get(3))
            } else {
                (0, vals.len(), args.get(1))
            };
            let key = key.map(|v| v.as_f64()).unwrap_or(0.0);
            let slice = &vals[lo.min(hi)..hi];
            let r = match slice.binary_search_by(|v| v.as_f64().partial_cmp(&key).unwrap_or(std::cmp::Ordering::Equal)) {
                Ok(i) => (lo + i) as i32,
                Err(i) => -((lo + i) as i32) - 1,
            };
            Value::Int(r)
        }
        "asList" => {
            // asList(T...) — single array arg, or several elements.
            //
            // The array case SHARES the caller's array instead of copying it, because the
            // JDK's `Arrays$ArrayList` is a write-through view over exactly that array:
            // `asList(arr).set(0, x)` has to be visible as `arr[0]`. Copying answered every
            // read correctly and lost only the write-back, so it read as working.
            //
            // The loose-elements case has no array to be a view OF — javac passes varargs
            // as one array, so this arm is reached from reflective and engine-internal
            // callers — and stays a copy, which is the correct answer there.
            let l = match args.first() {
                Some(Value::Array(h)) => make_list_over(vm, *h),
                _ => make_list(vm, args.to_vec()),
            };
            // `Arrays$ArrayList` is FIXED-SIZE, not immutable: it overrides `set`
            // (and so `sort`/`replaceAll` work) but inherits `AbstractList`'s
            // throwing `add`/`remove`.
            crate::immutable::mark(vm, &l, crate::immutable::FIXED_SET_OK);
            l
        }
        // copyOf/copyOfRange PRESERVE the source's runtime element type (the JDK
        // allocates `newInstance(a.getClass().getComponentType(), n)`). Hardcoding
        // Object[] made `Arrays.copyOf(new String[]{..}, 1)` an Object[] — so its
        // getClass() lied, `instanceof String[]` was false, and `Arrays.copyOf(int[])`
        // came back as an Object[] rather than an int[].
        "copyOf" => {
            // ⛔A NEGATIVE LENGTH PANICKED THE PROCESS. `as usize` on -1 is
            // 18446744073709551615, and `Vec::resize` to that aborts the whole VM with
            // "capacity overflow" — a guest calling `Arrays.copyOf(a, -1)`, which the JDK
            // answers with `NegativeArraySizeException`, took the engine down with it.
            let n_raw = args.get(1).and_then(Value::as_int).unwrap_or(0);
            if n_raw < 0 {
                let e = vm.make_exception("java/lang/NegativeArraySizeException", &n_raw.to_string());
                return Err(VmError::Throw(e));
            }
            let elem = source_array_element(vm, args.first());
            let mut e = read(vm, args.first());
            e.resize(n_raw as usize, Value::Null);
            Value::Array(vm.heap.alloc_values(&elem, e))
        }
        "copyOfRange" => {
            // `from > to` is an `IllegalArgumentException`, and a negative `from` an
            // `ArrayIndexOutOfBoundsException`; both used to silently produce an empty
            // array, which is a wrong answer rather than a crash but still a wrong one.
            let from_raw = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let to_raw = args.get(2).and_then(Value::as_int).unwrap_or(0);
            if from_raw > to_raw {
                let e = vm.make_exception(
                    "java/lang/IllegalArgumentException",
                    &format!("{from_raw} > {to_raw}"),
                );
                return Err(VmError::Throw(e));
            }
            if from_raw < 0 {
                let e = vm.make_exception(
                    "java/lang/ArrayIndexOutOfBoundsException",
                    &from_raw.to_string(),
                );
                return Err(VmError::Throw(e));
            }
            let elem = source_array_element(vm, args.first());
            let e = read(vm, args.first());
            let (from, to) = (from_raw as usize, to_raw as usize);
            let mut slice: Vec<Value> =
                e.into_iter().skip(from).take(to.saturating_sub(from)).collect();
            // The JDK pads with the element default when `to` runs past the source.
            slice.resize(to.saturating_sub(from), Value::Null);
            Value::Array(vm.heap.alloc_values(&elem, slice))
        }
        // `setAll(a, fn)` / `parallelSetAll` — fill each slot from its own INDEX. Absent,
        // and it is the idiomatic way to build a lookup table, so a guest that used it got
        // an unnamed failure rather than an array.
        //
        // The generator runs guest code per element, so the array handle is re-read each
        // time rather than held: an `apply` can allocate and move the arena.
        // A null array is a `NullPointerException` for every one of these, and they
        // quietly did nothing — so a guest bug became a missing side effect instead of
        // the exception it was written to surface.
        // ⚠NOT `hashCode`: `Arrays.hashCode(null)` is defined to return 0, and adding it
        // here turned a specified answer into an exception — caught by the probe on the
        // very next run.
        "sort" | "parallelSort" | "setAll" | "parallelSetAll" | "fill"
            if matches!(args.first(), Some(Value::Null) | None) =>
        {
            let e = vm.make_exception("java/lang/NullPointerException", "");
            return Err(VmError::Throw(e));
        }
        "setAll" | "parallelSetAll" => {
            if let Some(Value::Array(h)) = args.first() {
                let h = *h;
                let len = vm.heap.array_len(h).unwrap_or(0);
                for i in 0..len {
                    let f = args.get(1).cloned().unwrap_or(Value::Null);
                    let v = vm.apply(&f, vec![Value::Int(i as i32)])?.unwrap_or(Value::Null);
                    vm.heap.array_set(h, i, v);
                }
            }
            return Ok(Some(None));
        }
        // `mismatch(a, b)` — the first index at which they differ, or -1 when equal.
        // `compare(a, b)` — lexicographic, by the first difference and then by length.
        "mismatch" | "compare" => {
            let (x, y) = match (args.first(), args.get(1)) {
                (Some(Value::Array(x)), Some(Value::Array(y))) => (*x, *y),
                _ => return Ok(Some(Some(Value::Int(-1)))),
            };
            let (nx, ny) = (vm.heap.array_len(x).unwrap_or(0), vm.heap.array_len(y).unwrap_or(0));
            let mut at = None;
            for i in 0..nx.min(ny) {
                let (a, b) = (vm.heap.array_get(x, i), vm.heap.array_get(y, i));
                if a.as_ref().and_then(Value::as_long) != b.as_ref().and_then(Value::as_long) {
                    at = Some(i);
                    break;
                }
            }
            let v = match (name, at) {
                ("mismatch", Some(i)) => i as i32,
                ("mismatch", None) if nx == ny => -1,
                ("mismatch", None) => nx.min(ny) as i32,
                (_, Some(i)) => {
                    let (a, b) = (
                        vm.heap.array_get(x, i).and_then(|v| v.as_long()).unwrap_or(0),
                        vm.heap.array_get(y, i).and_then(|v| v.as_long()).unwrap_or(0),
                    );
                    if a < b { -1 } else { 1 }
                }
                _ => (nx as i32) - (ny as i32),
            };
            return Ok(Some(Some(Value::Int(v))));
        }
        "fill" => {
            if let Some(Value::Array(h)) = args.first() {
                let len = vm.heap.array_len(*h).unwrap_or(0);
                // Two forms: fill(a, value) fills the whole array; fill(a, from,
                // to, value) fills a[from..to]. The value is always the LAST arg.
                let (from, to, val) = if args.len() >= 4 {
                    (args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize,
                     (args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize).min(len),
                     args.get(3).cloned().unwrap_or(Value::Null))
                } else {
                    (0, len, args.get(1).cloned().unwrap_or(Value::Null))
                };
                for i in from..to { vm.heap.array_set(*h, i, val.clone()); }
            }
            return Ok(Some(None));
        }
        "sort" | "parallelSort" => {
            if let Some(Value::Array(h)) = args.first() {
                // Arrays.sort/parallelSort(a) | (a, cmp) | (a, from, to[, cmp]).
                let cmp = args.iter().skip(1).find(|v| v.is_fn_value());
                // Native auto-parallel fast path: a primitive INTEGER array with no
                // comparator sorts in place (in parallel when large) on the rayon
                // pool — pure data, no Value boxing, no bytecode callback. Honours
                // the (from, to) range form. Float/double/reference arrays and any
                // comparator fall through to the generic Value sort below.
                if cmp.is_none() {
                    let ints: Vec<i32> = args.iter().skip(1).filter_map(|v| v.as_int()).collect();
                    let len = vm.heap.array_len(*h).unwrap_or(0);
                    let (from, to) = if ints.len() >= 2 {
                        (ints[0].max(0) as usize, (ints[1].max(0) as usize).min(len))
                    } else {
                        (0, len)
                    };
                    if vm.heap.sort_primitive(*h, from, to) {
                        return Ok(Some(None));
                    }
                }
                let mut e = vm.heap.array_values(*h).unwrap_or_default();
                let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_ARRAYS_SORT, e.len()); // [CMP-CALLER] entry mark
                sort_values(vm, &mut e, cmp)?;
                for (i, v) in e.into_iter().enumerate() { vm.heap.array_set(*h, i, v); }
            }
            return Ok(Some(None));
        }
        "toString" => {
            // A NULL array renders "null" (JDK), not "[]" — `read` yields an empty
            // vec for both, so a caller could not tell an absent array from an empty
            // one. `Class.getEnumConstants()` on a non-enum returns null and printed
            // "[]" through here.
            if matches!(args.first(), None | Some(Value::Null)) {
                return Ok(Some(Some(Value::Str("null".into()))));
            }
            let e = read(vm, args.first());
            // Elements go through the guest `String.valueOf`, i.e. their own
            // `toString()`. `Value::display()` is the DEBUG rendering — it prints a
            // reference as `<object@7>` and an arena object as `<arena-object@0x…>`,
            // so `Arrays.toString` of any object array (records, enums, anything but
            // a boxed primitive or a String) leaked internal addresses to the guest.
            let mut parts = Vec::with_capacity(e.len());
            for v in &e {
                parts.push(vm.value_to_string(v).unwrap_or_else(|_| v.display()));
            }
            Value::Str(format!("[{}]", parts.join(", ")).into())
        }
        // deepToString recurses into nested arrays (e.g. String[][]); a plain
        // null arg renders "null", matching the JDK.
        "deepToString" => match args.first() {
            Some(v @ Value::Array(_)) => Value::Str(deep_to_string(vm, v).into()),
            Some(Value::Null) | None => Value::Str("null".into()),
            Some(other) => Value::Str(other.display().into()),
        },
        // `Arrays.hashCode` is the 31-fold over the ELEMENTS' OWN hashes, which for a
        // primitive array is the value and for an object array is `e.hashCode()`.
        // Folding `as_int` alone answered 1 for every String or object array — the
        // same value for all of them, so a hash-keyed lookup on one collided with
        // every other. A null array is 0, not 1.
        "hashCode" | "deepHashCode" => {
            if matches!(args.first(), None | Some(Value::Null)) {
                return Ok(Some(Some(Value::Int(0))));
            }
            let deep = name == "deepHashCode";
            Value::Int(array_hash(vm, args.first(), deep, 0))
        }
        // Both had NO arm, so every call left the native layer as an
        // UnsatisfiedLinkError — a hard failure on the idiomatic way to compare two
        // arrays. `equals` compares elements with their own `equals` (so a NESTED
        // array compares by identity); `deepEquals` recurses into them.
        "equals" | "deepEquals" => {
            let deep = name == "deepEquals";
            // Java 9 added `equals(a, aFrom, aTo, b, bFrom, bTo)`. Reading the second
            // array from `args[1]` unconditionally compared the first array against
            // an INT — always unequal, silently. Six arguments is the only shape that
            // can be the range form, and the array then lives at args[3].
            let ranged = args.len() >= 6
                && matches!(args.first(), Some(Value::Array(_)))
                && matches!(args.get(3), Some(Value::Array(_)));
            if ranged {
                let cut = |v: Option<&Value>, f: Option<&Value>, t: Option<&Value>| -> Vec<Value> {
                    let all = match v {
                        Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    let lo = f.and_then(Value::as_int).unwrap_or(0).max(0) as usize;
                    let hi = (t.and_then(Value::as_int).unwrap_or(0).max(0) as usize).min(all.len());
                    all.get(lo..hi).map(|s| s.to_vec()).unwrap_or_default()
                };
                let x = cut(args.first(), args.get(1), args.get(2));
                let y = cut(args.get(3), args.get(4), args.get(5));
                if x.len() != y.len() {
                    return Ok(Some(Some(Value::Int(0))));
                }
                let mut eq = true;
                for (p, q) in x.iter().zip(y.iter()) {
                    if crate::map::content_key(vm, p) != crate::map::content_key(vm, q) {
                        eq = false;
                        break;
                    }
                }
                return Ok(Some(Some(Value::Int(eq as i32))));
            }
            let (a, b) = (args.first(), args.get(1));
            let a_null = matches!(a, None | Some(Value::Null));
            let b_null = matches!(b, None | Some(Value::Null));
            let eq = if a_null || b_null {
                a_null && b_null
            } else {
                arrays_elements_equal(vm, a, b, deep)
            };
            Value::Int(eq as i32)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
