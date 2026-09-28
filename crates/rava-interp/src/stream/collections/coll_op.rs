//! Collection operations.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::stream::collections::*;

/// `IndexOutOfBoundsException` in the exact shape `Objects.checkIndex` produces, which is
/// what `ArrayList.get`/`set`/`remove(int)` throw. The index is the SIGNED guest value:
/// widening a negative index to `usize` first is how "Index -1" turns into
/// "Index 18446744073709551615", and the text is guest-visible via `getMessage()`.
fn index_oob(vm: &mut Vm, i: i32, len: usize) -> VmError {
    VmError::Throw(vm.make_exception(
        "java/lang/IndexOutOfBoundsException",
        &format!("Index {i} out of bounds for length {len}"),
    ))
}

/// First (or last) index whose element equals `needle` under `ekey_mut`, with the
/// elements ROOTED across the scan.
///
/// WARNING: this is a GC boundary, and it became one the moment collection keys
/// stopped being `Value::display()`. `ekey_mut` escalates a value class to
/// `map::structural_key`, which renders the object's guest `toString()` - an
/// interpreter re-entry that allocates and can move the arena. Walking the
/// caller's bare `Vec<Value>` would compare stale pointers from the first
/// value-class element onwards. `elems` is therefore taken by value and handed
/// back FORWARDED, which is why every caller rebinds it.
///
/// A scalar needle short-circuits before any element key is derived, so the hot
/// `list.remove(5)` / `contains(7)` shape runs no guest code and pays no rooting.
fn find_pos(
    vm: &mut Vm,
    elems: Vec<Value>,
    needle: &Value,
    key: &str,
    last: bool,
) -> (Vec<Value>, Option<usize>) {
    if matches!(needle, Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_)) {
        let n = elems.len();
        for k in 0..n {
            let i = if last { n - 1 - k } else { k };
            if ekey_eq_mut(vm, needle, key, &elems[i]) {
                return (elems, Some(i));
            }
        }
        return (elems, None);
    }
    let (elems, keys) = rooted_keys(vm, elems);
    let n = keys.len();
    for k in 0..n {
        let i = if last { n - 1 - k } else { k };
        if keys[i] == key {
            return (elems, Some(i));
        }
    }
    (elems, None)
}

/// True when `h` is a Set-shaped collection — unordered, hash = sum of elements,
/// equal only to another Set. Element equality (`equals`) and duplicate rejection
/// differ between List and Set, and this file keys that distinction here.
///
/// A `keySet()`/`entrySet()` view is a Set even though the object handed back is
/// class `java/util/ArrayList`; a `values()` view is a bare Collection and so is
/// neither. Only the view tag can tell those apart, so it is consulted first.
fn is_set_recv(vm: &mut Vm, h: u32) -> bool {
    match crate::map::map_view_of(vm, h) {
        Some((_, crate::map::ViewKind::Value)) => false,
        Some(_) => true,
        None => vm.heap.class_of(h).is_some_and(|c| c.contains("Set")),
    }
}

/// `addAll` on a receiver whose own bytecode owns its state is served through that bytecode's
/// `add` (default ON); `RAVA_NO_JAR_ADDALL=1` restores the historic `$elems` write, so the
/// change prices as a same-binary A/B.
fn jar_addall_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_JAR_ADDALL").is_none())
}

/// Does `recv`'s class declare `add(Object)` WITH BYTECODE — i.e. does the receiver's own code
/// own its element storage?
///
/// This is the whole admission test for the arm above, and it is evidence rather than a name
/// list: a collection this engine backs natively (`ArrayList`, `HashSet`, `ArrayDeque`, …) has
/// no `add` bytecode anywhere on its chain, so it answers `false` and keeps the bulk `$elems`
/// path; a jar or guest collection that implements `add` itself answers `true`, and its `addAll`
/// must go through that implementation for the same reason the JDK's `AbstractCollection.addAll`
/// does. A resolution ERROR is `false` — the conservative direction, since it leaves the
/// historic path in place.
fn recv_declares_add(vm: &mut Vm, recv: u32) -> bool {
    let Some(cls) = vm.heap.class_of(recv).map(str::to_string) else { return false };
    matches!(
        vm.resolve_method(&cls, "add", "(Ljava/lang/Object;)Z"),
        Ok(Some(ref d)) if d.file.method("add", "(Ljava/lang/Object;)Z").is_some_and(|m| m.code.is_some())
    )
}

/// Whether a guarded mutator would really alter `recv` — the question the
/// `AbstractList`-backed contract kinds turn on, since their inherited default
/// bodies only reach the throwing primitive when there is work to do
/// (`Collections.emptyList().removeIf(p)` returns false; `.add(x)` throws).
///
/// `removeIf` applies the predicate up to the FIRST match and stops, which is
/// what `Collection.removeIf`'s default body does before it hits
/// `Iterator.remove` — so a refusal never evaluates more of a side-effecting
/// predicate than the JDK would. Elements are walked through `rooted_each`
/// because that apply runs guest bytecode.
fn would_change(
    vm: &mut Vm,
    recv: u32,
    name: &str,
    op: crate::immutable::Op,
    args: &[Value],
) -> Result<bool, VmError> {
    use crate::immutable::Op;
    Ok(match op {
        Op::Insert | Op::RemoveAlways => true,
        // addAll(Collection) and addAll(int, Collection) — the source is the LAST
        // argument in both forms, never `args[1]` in the indexed one.
        Op::InsertAll => !elems_of(vm, args.last().filter(|_| args.len() > 1)).is_empty(),
        // `set`/`replaceAll` reach `ListIterator.set`, and `sort` reaches it too on
        // the kinds that do not override `sort` — both only when there is an
        // element to assign.
        Op::Write | Op::Sort => !elems_of(vm, Some(&Value::Ref(recv))).is_empty(),
        Op::RemoveMaybe => {
            let elems = elems_of(vm, Some(&Value::Ref(recv)));
            match name {
                "clear" => !elems.is_empty(),
                // `removeIf` never reaches here: its guard moved into the arm
                // itself so the guest predicate is evaluated exactly once per
                // element, which is what the JDK does. Answering `true` would be a
                // refusal without having asked, so this states the invariant instead.
                "removeIf" => {
                    debug_assert!(false, "removeIf is guarded in its own arm");
                    true
                }
                "removeAll" | "retainAll" => {
                    // `content_key`, the same derivation the real arms use — a
                    // guard that answered "no change" on a key the arm would have
                    // matched would refuse nothing and then quietly mutate.
                    let src = elems_of(vm, args.get(1));
                    let (_, otherk) = rooted_content_keys(vm, src);
                    let other: crate::FastSet<String> = otherk.into_iter().collect();
                    let keep_when_present = name == "retainAll";
                    let (_, mine) = rooted_content_keys(vm, elems);
                    mine.iter().any(|k| other.contains(k) != keep_when_present)
                }
                // remove(Object) / removeFirstOccurrence / removeLastOccurrence:
                // only a HIT reaches Iterator.remove.
                _ => {
                    let nv = args.get(1).cloned().unwrap_or(Value::Null);
                    let needle = ekey_mut(vm, &nv);
                    find_pos(vm, elems, &nv, &needle, false).1.is_some()
                }
            }
        }
    })
}

/// Is this class one whose iteration order is its comparator's?
///
/// Ordered cheapest-test-first: everything here ends in `Set`, and a `List` or a `Map` —
/// which is nearly every drain — is rejected by a three-byte suffix compare before any
/// longer test runs.
/// `NoSuchElementException` with no message, as the JDK's head accessors raise it.
fn empty_element(vm: &mut Vm) -> VmError {
    VmError::Throw(vm.make_exception("java/util/NoSuchElementException", ""))
}

/// Does this collection throw on a null element? The JDK splits sharply here and the
/// split is not guessable from the interface: `ArrayDeque` and `PriorityQueue` reject
/// null, `LinkedList` and `ArrayList` accept it.
#[inline]
fn rejects_null_elements(c: &str) -> bool {
    c.ends_with("ArrayDeque")
        || c.ends_with("PriorityQueue")
        || c.ends_with("PriorityBlockingQueue")
        || c.ends_with("ConcurrentLinkedQueue")
        || c.ends_with("ConcurrentLinkedDeque")
        || c.ends_with("LinkedBlockingQueue")
        || c.ends_with("LinkedBlockingDeque")
        || c.ends_with("ArrayBlockingQueue")
        || c.ends_with("ConcurrentSkipListSet")
}

/// `pub(crate)` because the ORDER RULE and the fast drains have to agree: `stream::mod`'s
/// `direct_elems` may only hand a caller a collection's raw backing array when the drain would
/// not have reordered it, and the only sound way to know that is to ask the same predicate
/// `order_sorted_set` asks. A second copy of this list would drift, and the drift would be a
/// silently mis-ordered iteration.
#[inline]
pub(crate) fn is_sorted_set_name(c: &str) -> bool {
    // Two cheap suffix gates first: a List or a Map — nearly every drain — is rejected
    // for the cost of three or five bytes before any longer test runs.
    if c.ends_with("Set") {
        return c.ends_with("TreeSet")
            || c.ends_with("SkipListSet")
            || c.ends_with("NavigableSet")
            || c.ends_with("SortedSet");
    }
    // A PRIORITY QUEUE IS ORDERED TOO, and it was not: `poll` returned the INSERTION
    // order, so `new PriorityQueue<>(List.of(5,1,4,2,3))` drained `5 1 4 2 3` where the
    // JVM drains `1 2 3 4 5`. Its head is its least element by the comparator.
    //
    // Matched EXACTLY, never by the `Queue` suffix: `ArrayBlockingQueue`,
    // `ConcurrentLinkedQueue` and friends are FIFO, and sorting one would break the
    // contract this fixes for the other. Iteration order is explicitly unspecified for a
    // priority queue ("not guaranteed to traverse in any particular order"), so ordering
    // the backing vector is a legal way to make the head correct.
    c.ends_with("PriorityQueue") || c.ends_with("PriorityBlockingQueue")
}

/// Put a SORTED collection's elements in its own order, in place — a sorted set or a
/// priority queue. A no-op for every other receiver.
///
/// ⭐SHARED, and it has to be: `stream::dispatch` drains any Iterable through
/// `iterator`/`stream`/`spliterator` **before it looks at the owner at all**, so this rule living
/// only in `coll_op` meant a `TreeSet` printed sorted through `toString` and iterated in INSERTION
/// order through a for-each — `toArray` right, `iterator` wrong, on the same object. Found by the
/// save-path gate (`bench/NbtSave.java`), whose canonical dump walks `new TreeSet<>(keySet())`.
/// How many elements a collection's backing array holds RIGHT NOW, without materialising it.
///
/// The fail-fast checks compare this before and after, so it has to be cheap and it has to
/// read the LIVE array rather than any snapshot: one field read and a length.
pub(crate) fn source_len(vm: &Vm, recv: u32) -> Option<usize> {
    match vm.heap.get_field_syn(recv, crate::heap::Syn::Elems) {
        // `elems_len`, not `array_len`: a guest-bytecode collection keeps spare capacity in
        // `$elems` and its real length in `$size`. Without `$size` the two are identical.
        Value::Array(a) => Some(vm.heap.elems_len(recv, a)),
        _ => None,
    }
}

pub(crate) fn order_sorted_set(vm: &mut Vm, recv: &Value, elems: &mut Vec<Value>) -> Result<(), VmError> {
    if elems.len() < 2 {
        return Ok(());
    }
    // FLAG-TESTABLE (`RAVA_NO_SORTED_SET_ORDER` opts out), because the boot's phase map
    // puts the session's whole regression in the first block — process start to
    // `Environment:`, +7.76% with 1 of 6 pairs going the other way — and that block is
    // class loading and static init, where hundreds of collections are built and drained.
    // This guard is the newest thing on that path. A flag makes it answerable by the one
    // design that cannot be confounded (ledger row 269): same binary, both arms.
    static ORDER_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ORDER_ON.get_or_init(|| std::env::var_os("RAVA_NO_SORTED_SET_ORDER").is_none()) {
        return Ok(());
    }
    // NO ALLOCATION AND NO SCAN on the common path. This runs on EVERY drain of EVERY
    // collection, and the first version called `value_class_name`, which `to_string()`s
    // the class — one heap allocation per drain, measured at **+15.9% of boot `Done`**
    // over 4 paired runs. `class_of` borrows the interned name instead, and the
    // discriminator leads with `ends_with("Set")`, which rejects every List and Map for
    // the cost of three bytes.
    let sorted = match recv {
        Value::Ref(h) => vm.heap.class_of(*h).is_some_and(is_sorted_set_name),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
            .is_some_and(|c| is_sorted_set_name(c.as_ref())),
        _ => false,
    };
    if !sorted {
        return Ok(());
    }
    // The receiver's own comparator wins where it has one; otherwise natural order.
    let cmp = match recv {
        Value::Ref(h) => vm.heap.get_field(*h, "$comparator"),
        _ => Value::Null,
    };
    let c = if matches!(cmp, Value::Null) { None } else { Some(&cmp) };
    let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_SORTED_SET_ORDER, elems.len()); // [CMP-CALLER] entry mark
    sort_values(vm, elems, c)
}

/// Kill switch for the shapes below that stop MATERIALISING a whole collection to answer a
/// question about one end of it, and stop re-deriving a Set's element keys once per element
/// added. `RAVA_NO_FAST_COLLOP=1` puts every one of them back on the general drain, which is
/// still present and still the fallback for every receiver whose precondition does not hold.
fn fast_collop() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_COLLOP").is_none() && !crate::flags::emit_more_disabled()
    })
}

pub(crate) fn coll_op(vm: &mut Vm, name: &str, descriptor: &str, args: &[Value]) -> Handled {
    #[cfg(feature = "next-gen-gc")] if let Some(r) = vm.value_computer_coll_op(name, descriptor, args)? { return Ok(Some(r)); } // VALUECOMPUTER: an ARENA receiver of an admitted pure value-computer, served by its own size()/get(int) bytecode (vm/native_backing.rs)
    let recv = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    // Read elements from $elems (our collections) or by draining a real
    // (jar) collection's own iterator().
    // <init> handled before touching receiver elements — the receiver may be
    // half-constructed (e.g. a guava list whose `array` field isn't set yet),
    // and AbstractCollection.<init> routes here via the "Collection" match.
    if name == "<init>" {
        // A COMPARATOR ARGUMENT IS NOT A SOURCE COLLECTION. `new PriorityQueue<>(cmp)`
        // and `new TreeSet<>(cmp)` pass one, and draining it as an Iterable silently
        // produced an EMPTY collection with NO comparator — so a reverse-ordered queue
        // came back in insertion order and nothing said why.
        if let Some(arg) = args.get(1) {
            if crate::reflect::value_is_comparator(vm, arg) {
                vm.heap.put_field(recv, "$comparator", arg.clone());
                return Ok(Some(None));
            }
        }
        // An ARENA object is a source collection too — an overridden `java.util` class is not
        // native-served and so is not handle-born. Testing only `Value::Ref` made every copy
        // constructor over one return empty (`new LinkedList<>(guestList)` == `[]`).
        #[allow(unused_mut)]
        let mut src_is_coll = matches!(args.get(1), Some(Value::Ref(_)));
        #[cfg(feature = "next-gen-gc")]
        if matches!(args.get(1), Some(Value::Obj(_))) {
            src_is_coll = true;
        }
        if src_is_coll {
            let src = elems_of(vm, args.get(1));
            if !src.is_empty() {
                // `new HashSet<>(coll)` is addAll, so it DEDUPES; copying the source
                // verbatim left a Set holding duplicates, which then reported the
                // wrong size and iterated the same element twice.
                let src = if is_set_recv(vm, recv) { dedup_by_ekey(vm, src) } else { src };
                store_elems(vm, recv, src);
            }
        }
        return Ok(Some(None));
    }
    // NULL-HOSTILE COLLECTIONS. `ArrayDeque`, `PriorityQueue` and the concurrent
    // queues throw `NullPointerException` from every insertion; ours accepted a null and
    // then handed it back, so `deque.addFirst(null)` succeeded where `java -Xint` throws.
    //
    // Named EXACTLY, because the neighbours are the opposite: `LinkedList` and
    // `ArrayList` permit null elements, and refusing one there would be a new wrong
    // answer in place of the old one.
    if matches!(name, "add" | "addFirst" | "addLast" | "offer" | "offerFirst" | "offerLast" | "push")
        && matches!(args.get(1), Some(Value::Null))
        && vm.heap.class_of(recv).is_some_and(rejects_null_elements)
    {
        return Err(crate::immutable::npe(vm));
    }
    // FAIL-FAST CONTRACT (immutable.rs): a receiver stamped by one of the JDK's
    // unmodifiable/fixed-size factories refuses its mutators exactly as the JDK's
    // does — BEFORE any write path below touches `$elems`. `classify` is a name +
    // descriptor test, so the (dominant) read methods pay nothing but a match, and
    // the `$immutable` field probe only happens on a mutator.
    // MEASUREMENT HANDLE for the whole unmodifiable-contract feature
    // (`RAVA_NO_IMMUTABLE_CONTRACTS=1` skips it). Row 273 puts the session's entire
    // regression in the boot's first block — class loading and static init — and this is
    // the largest per-call thing living there: `classify` runs 4.75M times a boot and
    // 45.5% of those go on to probe `kind_of`. Skipping the block prices the FEATURE, not
    // a micro-optimisation of it, by the one design that cannot be confounded (row 269).
    //
    // ⚠TURNING IT OFF IS WRONG BEHAVIOUR, NOT A FAST PATH: every mutator on a `List.of`
    // silently succeeds again, which is the bug the contracts exist to fix. It is a
    // measurement flag and nothing else.
    static CONTRACTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let contracts_on =
        *CONTRACTS.get_or_init(|| std::env::var_os("RAVA_NO_IMMUTABLE_CONTRACTS").is_none());
    if let Some(op) = if contracts_on { crate::immutable::classify(name, descriptor) } else { None } {
        let kind = crate::immutable::kind_of(vm, recv);
        // `removeIf` is deliberately NOT probed here. Its "would this change
        // anything?" question can only be answered by RUNNING the guest predicate,
        // and the arm below then runs it again over every element — so a predicate
        // with a side effect (a counter, a log line, a `System.out.print`) fired
        // twice for the elements before the first match. The JDK evaluates it
        // exactly once per element. The arm below therefore evaluates once and
        // refuses AFTER, before it stores anything.
        if kind != crate::immutable::NONE && name != "removeIf" {
            // The unconditional kind never asks the question; the AbstractList-backed
            // kinds do, and only they pay for the element scan.
            let changes = kind == crate::immutable::IMMUTABLE
                || would_change(vm, recv, name, op, args)?;
            if crate::immutable::refuses(kind, op, changes) {
                return Err(crate::immutable::uoe(vm));
            }
        }
    }
    // --- O(1) fast paths over the backing $elems array -----------------------
    // The general path below CLONES the entire backing array (iterate_object →
    // store_elems rebuild) on EVERY call. For a collection built/scanned
    // element-by-element that is O(n^2) (guava Lists/ImmutableSet/ObjectArrays
    // building a ~100k-element list dominated boot). Serve the cheap operations
    // directly off the heap array — no full drain, no rebuild — when the receiver
    // is one of our $elems-backed collections (a real jar collection has no
    // $elems and falls through to the generic drain).
    // A `subList` VIEW is excluded from every O(1) arm below, because they all read and
    // write the view's own `$elems` snapshot directly: `sub.set(0, 99)` landed in the
    // snapshot and never reached the backing list, and `sub.get(0)` would answer from it
    // after the parent had moved on. The general path resolves through `iterate_object`
    // and stores through `store_elems`, which are the two points `crate::view` hooks.
    // `is_view` is a handle bit-test for everything that is not one.
    // A MAP VIEW answers `size`/`isEmpty` from the backing map's count. Without this the
    // liveness fix would re-derive every key or entry to answer "how many", on a path MC
    // walks constantly — the JDK's views are O(1) here for exactly this reason.
    if matches!((name, descriptor), ("size", "()I") | ("isEmpty", "()Z")) {
        if let Some(n) = crate::view::map_view_size(vm, recv) {
            return Ok(Some(Some(Value::Int(if name == "size" {
                n as i32
            } else {
                (n == 0) as i32
            }))));
        }
    }
    let elems_field = if crate::view::is_view(vm, recv) {
        Value::Null
    } else {
        vm.heap.get_field_syn(recv, crate::heap::Syn::Elems)
    };
    if let Value::Array(arr) = elems_field {
        let len = vm.heap.elems_len(recv, arr);
        match name {
            "size" => return Ok(Some(Some(Value::Int(len as i32)))),
            "isEmpty" => return Ok(Some(Some(Value::Int((len == 0) as i32)))),
            // get/set: an index outside [0, size) is an EXCEPTION, not a null. The
            // `as usize` cast below used to send a negative index to a huge unsigned
            // one, so `array_get` missed and the caller silently received null where
            // the JVM unwinds — a wrong ANSWER instead of a wrong error.
            // Only the INDEXED overload is range-checked: the descriptor is the only
            // admissible discriminator, and this arm is also reached by 0-arg and
            // Object-arg `get`s (Optional/AtomicReference/Supplier shapes) whose
            // routing must not move — throwing for those would be a new wrong answer.
            "get" if descriptor.starts_with("(I") => {
                let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
                if i < 0 || (i as usize) >= len {
                    return Err(index_oob(vm, i, len));
                }
                return Ok(Some(Some(vm.heap.array_get(arr, i as usize).unwrap_or(Value::Null))));
            }
            "get" => {
                let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
                return Ok(Some(Some(
                    usize::try_from(i).ok().and_then(|u| vm.heap.array_get(arr, u)).unwrap_or(Value::Null),
                )));
            }
            "set" => {
                let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
                if i < 0 || (i as usize) >= len {
                    return Err(index_oob(vm, i, len));
                }
                let old = vm.heap.array_get(arr, i as usize).unwrap_or(Value::Null);
                vm.heap.array_set(arr, i as usize, args.get(2).cloned().unwrap_or(Value::Null));
                return Ok(Some(Some(old)));
            }
            // List tail-append: in-place push, no clone. (Index-insert add(I,E),
            // Set dedup, and Deque head ops keep the general path below.)
            "add" | "offer" | "addLast" | "offerLast"
                if !(descriptor.starts_with("(I") && args.len() > 2) =>
            {
                let is_set = vm.heap.class_of(recv).is_some_and(|c| c.contains("Set"));
                if !is_set {
                    vm.heap.array_push(arr, args.get(1).cloned().unwrap_or(Value::Null));
                    return Ok(Some(Some(Value::Int(1))));
                }
                // Set tail-append with O(1) dedup via the membership index (no
                // clone, no O(n) scan → element-by-element builds go O(n) not
                // O(n^2)). ekey-based, matching the general path's dedup exactly;
                // ordering for sorted sets is still applied on read there.
                let v = args.get(1).cloned().unwrap_or(Value::Null);
                let k = ekey_mut(vm, &v);
                if set_index_contains(vm, recv, arr, len, &k) {
                    return Ok(Some(Some(Value::Int(0))));
                }
                vm.heap.array_push(arr, v);
                set_index_insert(vm, recv, k, len + 1);
                return Ok(Some(Some(Value::Int(1))));
            }
            // Set membership: O(1) index lookup instead of cloning + scanning the
            // whole backing. Lists keep the general-path scan (order-sensitive
            // contains semantics live there); only $elems-backed sets index here.
            "contains" if vm.heap.class_of(recv).is_some_and(|c| c.contains("Set")) => {
                let nv = args.get(1).cloned().unwrap_or(Value::Null);
                let k = ekey_mut(vm, &nv);
                // A CONTAINER needle (Map / Map$Entry / collection) compares by
                // CONTENT, which this ekey-keyed membership index cannot answer —
                // fall through to the general scan below, which can. `ekey` and
                // `content_key` agree for every other needle, so nothing else moves.
                if crate::map::content_key(vm, &nv) == k {
                    let present = set_index_contains(vm, recv, arr, len, &k);
                    return Ok(Some(Some(Value::Int(present as i32))));
                }
            }
            // List index-remove: native Vec::remove (O(n) memmove), no clone/rebuild
            // of the whole backing (the general path's cost). remove(int) returns the
            // removed element; descriptor "(I..." disambiguates it from
            // remove(Object) (a Set/value remove, which keeps the general path).
            "remove" if descriptor.starts_with("(I") => {
                let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
                if i >= 0 && (i as usize) < len {
                    let old = vm.heap.array_remove(arr, i as usize).unwrap_or(Value::Null);
                    return Ok(Some(Some(old)));
                }
                return Err(index_oob(vm, i, len));
            }
            // A HEAD/TAIL READ IS ONE ARRAY SLOT, AND IT MATERIALISED THE WHOLE COLLECTION.
            // `peek`/`peekFirst`/`element`/`getFirst`/`first` and `peekLast`/`getLast`/`last`
            // had no O(1) arm, so every one of them fell to the general path below, which
            // drains `$elems` into a fresh `Vec<Value>` (`iterate_object` -> `elems_values`)
            // and then reads ONE end of it and drops the rest.
            //   was O(size) copy + O(size) allocation per call, is O(1) — one array read.
            // What grows: ELEMENTS IN THE RECEIVER. The shape that pays for it is the one
            // every queue drain is written in — `while (!q.isEmpty()) { q.peek(); q.poll(); }`
            // — where the per-call O(size) makes the walk O(size^2).
            //
            // REFUSED, invisibly, wherever the precondition cannot be proved:
            //   * a SORTED receiver (`is_sorted_set_name`: TreeSet/NavigableSet/SortedSet/
            //     SkipListSet, PriorityQueue): the general path runs `order_sorted_set`
            //     FIRST, so its head is the least element by the comparator, not slot 0.
            //     Falls through and is answered exactly as it is today.
            //   * anything carrying an argument. The descriptor is the only admissible
            //     discriminator and these names are overloaded elsewhere (`Stream.peek`
            //     takes a Consumer), so a non-`()` form keeps the path it has.
            // A `subList`/map view never reaches here at all — `elems_field` is Null for one.
            //
            // The empty-receiver split is copied arm-for-arm from the general path and is the
            // whole reason the Queue and Deque families both exist: `element`/`getFirst`/
            // `getLast` THROW NoSuchElementException, `peek`/`peekFirst`/`peekLast` answer
            // null — and `first`/`last` answer null here too, deliberately (see the note on
            // the general-path arm: raising there killed the boot before its first log line).
            "peek" | "peekFirst" | "element" | "getFirst" | "first" | "peekLast" | "getLast"
            | "last"
                if fast_collop()
                    && args.len() == 1
                    && descriptor.starts_with("()")
                    && !vm.heap.class_of(recv).is_some_and(is_sorted_set_name) =>
            {
                if len == 0 {
                    if matches!(name, "element" | "getFirst" | "getLast") {
                        return Err(empty_element(vm));
                    }
                    return Ok(Some(Some(Value::Null)));
                }
                let i = if matches!(name, "peekLast" | "getLast" | "last") { len - 1 } else { 0 };
                return Ok(Some(Some(vm.heap.array_get(arr, i).unwrap_or(Value::Null))));
            }
            "clear"
                if fast_collop()
                    && args.len() == 1
                    && descriptor == "()V"
                    && crate::map::map_view_of(vm, recv).is_none() =>
            {
                store_elems(vm, recv, Vec::new());
                return Ok(Some(None));
            }
            _ => {}
        }
    }
    // ⭐THE SORTED ORDER WAS APPLIED TWICE PER DRAIN. `iterate_object` runs
    // `order_sorted_set` itself on the `$elems` branch — the branch it takes for exactly the
    // receivers that are not a view and do own an `$elems` array — and the call below then
    // sorted the same `Vec` again. For a TreeSet or a PriorityQueue that was two full sorts,
    // i.e. 2 x O(n log n) GUEST comparator invocations, for every drained operation; the
    // second one is a re-sort of an already-sorted vector and cannot move an element.
    //   was 2 orderings per drain, is 1. What grows: ELEMENTS IN A SORTED RECEIVER.
    // The flag is READ FRESH rather than reused from `elems_field` above: a fall-through out
    // of the O(1) block can run guest `toString` (the `contains` needle key), and only the
    // state `iterate_object` itself sees decides whether it sorted. `view::read` gates on the
    // same `is_view`, so "not a view and has `$elems`" is exactly its sorting branch.
    let preordered = fast_collop()
        && !crate::view::is_view(vm, recv)
        && matches!(vm.heap.get_field_syn(recv, crate::heap::Syn::Elems), Value::Array(_));
    // ⚠**RESOLVED BEFORE `elems` EXISTS, AND THAT ORDER IS THE ROOTING RULE.**
    // `recv_declares_add` calls `resolve_method`, which can load a class and therefore
    // allocate — and every `Vec<Value>` below (`elems`, and `add` in the `addAll` arm) is a
    // bare Rust local that no root walk sees, so a collection between binding one and using it
    // would leave stale arena pointers behind. Asking here, where nothing is live yet, removes
    // the window entirely rather than arguing about whether the resolve can really collect.
    // Guarded on the NAME, so no other method pays for it.
    let jar_add = name == "addAll" && jar_addall_enabled() && recv_declares_add(vm, recv);
    let mut elems = vm.iterate_object(recv)?;
    let arg1 = args.get(1).cloned();
    // A keySet()/values()/entrySet() collection is a LIVE VIEW of its map, so a
    // removal through it must delete the mapping too — the JDK contract, and the
    // reason `map.keySet().remove(k)` silently did nothing here before. Probed
    // only for the mutating names, so every read path pays nothing extra.
    let view = if matches!(name, "remove" | "removeAll" | "retainAll" | "clear" | "removeIf"
        | "removeFirstOccurrence" | "removeLastOccurrence")
    {
        crate::map::map_view_of(vm, recv)
    } else {
        None
    };
    // Drop `dropped` from the backing map. The elements are ROOTED in a heap
    // array first: `map_view_remove` derives a key, which can run guest
    // `toString` and move the arena out from under a bare Rust Vec.
    macro_rules! write_through {
        ($dropped:expr) => {{
            if let Some((owner, kind)) = view {
                let darr = vm.heap.alloc_values(OBJ, $dropped);
                let n = vm.heap.array_len(darr).unwrap_or(0);
                for i in 0..n {
                    let d = vm.heap.array_get(darr, i).unwrap_or(Value::Null);
                    crate::map::map_view_remove(vm, owner, kind, &d)?;
                }
            }
        }};
    }
    // TreeSet / NavigableSet / ConcurrentSkipListSet keep their elements in the
    // comparator's order, so first/last/poll/ceiling/floor must observe that
    // order rather than insertion order. Sort the working copy up front (natural
    // ordering unless a `$comparator` was stored); the existing head/tail arms
    // then read least=first / greatest=last correctly. Covers CraftBukkit's
    // HashTreeSet, whose TreeSet<Long> drives the post-Done chunk-unload schedule.
    // `preordered` (above) is true exactly when `iterate_object` already did this.
    if !preordered {
        order_sorted_set(vm, &Value::Ref(recv), &mut elems)?;
    }
    // The ceiling/floor arms below compare against the receiver's own comparator.
    let set_cmp = vm.heap.get_field(recv, "$comparator");
    macro_rules! save { () => {{ store_elems(vm, recv, elems); }}; }
    let ret = match name {
        "stream" => make_stream(vm, elems),
        "size" => Value::Int(elems.len() as i32),
        "isEmpty" => Value::Int(elems.is_empty() as i32),
        "clear" => {
            elems.clear();
            save!();
            // A view's clear() empties the MAP (JDK: Map.keySet().clear() ==
            // map.clear()); no per-element key derivation needed.
            if let Some((owner, _)) = view { crate::map::map_view_clear(vm, owner); }
            return Ok(Some(None));
        }
        // Same bounds contract as the `$elems` fast path above — a jar-backed
        // collection drained here must not answer null for an index the JVM rejects.
        "get" => {
            let i = arg1.as_ref().and_then(Value::as_int).unwrap_or(0);
            match usize::try_from(i).ok().and_then(|u| elems.get(u).cloned()) {
                Some(v) => v,
                // Same descriptor guard as the fast path: only `get(int)` throws.
                None if descriptor.starts_with("(I") => {
                    let n = elems.len();
                    return Err(index_oob(vm, i, n));
                }
                None => Value::Null,
            }
        }
        "set" => {
            let i = arg1.as_ref().and_then(Value::as_int).unwrap_or(0);
            if i < 0 || (i as usize) >= elems.len() {
                return Err(index_oob(vm, i, elems.len()));
            }
            let old = elems[i as usize].clone();
            elems[i as usize] = args.get(2).cloned().unwrap_or(Value::Null);
            save!();
            old
        }
        "contains" => {
            let nv = arg1.clone().unwrap_or(Value::Null);
            let needle = ekey_mut(vm, &nv);
            // A CONTAINER needle (Map / Map$Entry / collection) must compare by
            // CONTENT — its `ekey` is an identity tag, so an equal-by-value
            // element would never match. `content_key` differs from `ekey` for
            // exactly those shapes, so a plain needle keeps the old scan and its
            // cost verbatim.
            let ckey = crate::map::content_key(vm, &nv);
            let found = if ckey == needle {
                let (back, pos) = find_pos(vm, elems, &nv, &needle, false);
                elems = back;
                pos.is_some()
            } else {
                let (back, keys) = rooted_content_keys(vm, elems);
                elems = back;
                keys.iter().any(|k| *k == ckey)
            };
            Value::Int(found as i32)
        }
        "containsAll" => {
            let (back, mine) = rooted_content_keys(vm, elems);
            elems = back;
            let have: crate::FastSet<String> = mine.into_iter().collect();
            let other = elems_of(vm, args.get(1));
            let (_, theirs) = rooted_content_keys(vm, other);
            Value::Int(theirs.iter().all(|k| have.contains(k)) as i32)
        }
        // Collection value-equality, split by KIND. `AbstractList.equals` is
        // positional and refuses anything that is not a List; `AbstractSet.equals`
        // is membership-based and refuses anything that is not a Set. The old code
        // tried ordered compare and then fell back to a SORTED compare for every
        // receiver, so `[a,b].equals([b,a])` — and even a List/Set cross-compare —
        // answered true. MC's own `Objects.equals(listA, listB)` checks then passed
        // on lists that are not equal.
        //
        // Element compare via `content_key`, not `ekey`: an entrySet() holds
        // Map$Entry objects and a list can hold Maps, both of which `ekey` renders
        // as an identity tag — so equal-by-value collections of them compared
        // unequal, and `Map.equals` through entrySet with them.
        "equals" => {
            let eq = if let Some(Value::Ref(oh)) = args.get(1) {
                let oh = *oh;
                let recv_set = is_set_recv(vm, recv);
                let other_is_coll = vm.heap.class_of(oh).is_some_and(is_collection);
                let other_set = is_set_recv(vm, oh);
                let other = elems_of(vm, args.get(1));
                if !other_is_coll || recv_set != other_set || other.len() != elems.len() {
                    false
                } else {
                    let (back, mut ad) = rooted_content_keys(vm, elems);
                    elems = back;
                    let (_, mut bd) = rooted_content_keys(vm, other);
                    // AbstractSet.equals is membership, AbstractList.equals is
                    // positional: sorting is what makes the first order-independent.
                    if recv_set {
                        ad.sort();
                        bd.sort();
                    }
                    ad == bd
                }
            } else {
                false
            };
            Value::Int(eq as i32)
        }
        // JDK element-hash folds: AbstractList `31*h + e` (order-dependent),
        // AbstractSet the SUM (order-independent). Without them a collection
        // hashed by identity, so two equal lists disagreed.
        "hashCode" if descriptor == "()I" => {
            // values() is a bare AbstractCollection, which does NOT override
            // hashCode — leave it the identity answer it has.
            if matches!(crate::map::map_view_of(vm, recv), Some((_, crate::map::ViewKind::Value))) {
                return Ok(None);
            }
            if is_set_recv(vm, recv) {
                Value::Int(crate::map::set_hash(vm, &elems))
            } else {
                Value::Int(crate::map::list_hash(vm, &elems))
            }
        }
        // indexOf scans forward, lastIndexOf backward. lastIndexOf had NO arm at all,
        // so it fell out of the native layer as an UnsatisfiedLinkError.
        "indexOf" | "lastIndexOf" => {
            let nv = arg1.clone().unwrap_or(Value::Null);
            let needle = ekey_mut(vm, &nv);
            // Same container-needle rule as `contains` above.
            let ckey = crate::map::content_key(vm, &nv);
            let last = name == "lastIndexOf";
            let pos = if ckey == needle {
                let (back, p) = find_pos(vm, elems, &nv, &needle, last);
                elems = back;
                p
            } else {
                let (back, keys) = rooted_content_keys(vm, elems);
                elems = back;
                if last { keys.iter().rposition(|k| *k == ckey) } else { keys.iter().position(|k| *k == ckey) }
            };
            Value::Int(pos.map(|i| i as i32).unwrap_or(-1))
        }
        "subList" => {
            // `AbstractList.subListRangeCheck`: fromIndex < 0 and toIndex > size are
            // IndexOutOfBounds (each with its OWN message naming the offending bound),
            // from > to is an IllegalArgumentException. Clamping both to the list
            // instead handed back a silently truncated view, so a caller computing a
            // bad range got plausible-looking data rather than the JVM's throw.
            let n = elems.len();
            let from = arg1.as_ref().and_then(Value::as_int).unwrap_or(0);
            let to = args.get(2).and_then(Value::as_int).unwrap_or(0);
            if from < 0 {
                let ex = vm.make_exception(
                    "java/lang/IndexOutOfBoundsException", &format!("fromIndex = {from}"));
                return Err(VmError::Throw(ex));
            }
            if to < 0 || (to as usize) > n {
                let ex = vm.make_exception(
                    "java/lang/IndexOutOfBoundsException", &format!("toIndex = {to}"));
                return Err(VmError::Throw(ex));
            }
            if from > to {
                let ex = vm.make_exception(
                    "java/lang/IllegalArgumentException",
                    &format!("fromIndex({from}) > toIndex({to})"));
                return Err(VmError::Throw(ex));
            }
            let slice = elems[from as usize..to as usize].to_vec();
            // A LIVE VIEW, not a copy: `sub.set(0, x)` must be visible as `base.get(from)`
            // and `sub.clear()` must remove the range from `base`. `crate::view` falls back
            // to `None` when the receiver's elements do not live in a `$elems` array, and
            // then this keeps the copy it always returned.
            let sub = crate::view::make_sublist(vm, recv, from as usize, to as usize, slice.clone())
                .unwrap_or_else(|| make_list(vm, slice));
            // `List.of(..).subList(..)` is an ImmutableCollections.SubList and
            // refuses every mutator (immutable::inherit).
            crate::immutable::inherit(vm, recv, &sub);
            return Ok(Some(Some(sub)));
        }
        // tail insertion: add / offer / addLast / offerLast
        // List.sort(Comparator) — in-place; null comparator = natural order.
        // RegistryDataLoader sorts its pending entries this way before decoding.
        "sort" => {
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_LIST_SORT, elems.len()); // [CMP-CALLER] entry mark
            sort_values(vm, &mut elems, args.get(1))?;
            save!();
            return Ok(Some(None));
        }
        // List.add(int index, E element) / addElement at index — VOID, inserts the
        // ELEMENT (arg2) at the index (arg1). Treating it as add(E) added the index
        // (a boxed int) instead, which is why snakeyaml's token list held Int(0):
        // ScannerImpl.fetchValue does tokens.add(idx, new KeyToken(...)).
        "add" | "addElement" if descriptor.starts_with("(I") && args.len() > 2 => {
            // Insert position is [0, size] INCLUSIVE; anything else throws. Clamping
            // it (the old `.max(0).min(len)`) silently moved the element to the head
            // or the tail, so a caller's index bug became a reordered list instead of
            // a stack trace. `ArrayList.rangeCheckForAdd` words this one differently
            // from `checkIndex` — "Index: i, Size: n" — and the text is guest-visible.
            let idx = args.get(1).and_then(Value::as_int).unwrap_or(0);
            if idx < 0 || (idx as usize) > elems.len() {
                let ex = vm.make_exception(
                    "java/lang/IndexOutOfBoundsException",
                    &format!("Index: {idx}, Size: {}", elems.len()),
                );
                return Err(VmError::Throw(ex));
            }
            let el = args.get(2).cloned().unwrap_or(Value::Null);
            elems.insert(idx as usize, el);
            save!();
            return Ok(Some(None));
        }
        "add" | "offer" | "addLast" | "offerLast" => {
            let v = arg1.unwrap_or(Value::Null);
            // Set semantics: reject a duplicate (by the same value-equality the
            // `contains` arm uses). Without this a `$elems`-backed HashSet behaved
            // as a list, so MC code that builds a Set of items from many recipes
            // (StatisticList craft stats) iterated each item repeatedly and
            // double-registered → "Duplicate stat id".
            let is_set = vm.heap.class_of(recv).is_some_and(|c| c.contains("Set"));
            if is_set {
                let nd = ekey_mut(vm, &v);
                let (back, dup) = find_pos(vm, elems, &v, &nd, false);
                elems = back;
                if dup.is_some() {
                    Value::Int(0)
                } else {
                    elems.push(v);
                    save!();
                    Value::Int(1)
                }
            } else {
                elems.push(v);
                save!();
                Value::Int(1)
            }
        }
        // head insertion: addFirst / offerFirst / push (Deque)
        "addFirst" | "offerFirst" | "push" => {
            elems.insert(0, arg1.unwrap_or(Value::Null));
            save!();
            Value::Int(1)
        }
        "addAll" => {
            let add = elems_of(vm, args.get(1));
            // ⛔**THE JDK'S OWN DEFAULT BODY, RUN OVER A JAR'S OWN STATE — AND WRITTEN TO THE
            // WRONG PLACE.** `addAll(Collection)` is declared by `java.util.AbstractCollection`,
            // whose bytecode this engine does not hold, so for a jar collection that inherits it
            // `resolve_method` finds nothing and the call arrives HERE — while `add`, `size`,
            // `iterator` and `toString`, all declared by the jar itself, keep running the jar's
            // own bytecode over its own fields. The arm below then wrote `$elems`, which that
            // bytecode never reads: measured on fastutil's hash sets,
            // `new ReferenceLinkedOpenHashSet<>().addAll(List.of("x","y"))` returned TRUE and
            // left `size() == 0`, and the Collection CONSTRUCTOR (`this(); addAll(c)`) therefore
            // built an EMPTY set from a non-empty source.
            //
            // ⭐**AND THAT ONE NO-OP WAS THE LAST DIVERGENCE IN bench/ReloadBench.java's HASH.**
            // `TooltipDisplay.CODEC` decodes `hidden_components` through
            // `Codec.listOf().xmap(list -> new Reference…Set<>(list), …)`, so the ominous
            // banner's set came out empty; empty makes the record equal to
            // `TooltipDisplay.DEFAULT`, `optionalFieldOf("hidden_components", emptySet())`
            // elided the field, the encoded component became `{}`, and
            // `ExtraCodecs.dispatchOptionalValue` dropped `minecraft:tooltip_display` from two
            // advancements entirely. One missing `add` at the bottom, two wrong JSON documents
            // at the top.
            //
            // THE FIX IS THE JDK'S OWN BODY: `for (T e : c) if (add(e)) modified = true`, with
            // `add` dispatched the way `invokevirtual` would — so the jar's own `add` writes the
            // jar's own fields. Admitted ONLY when the receiver's class declares `add` WITH
            // bytecode, which is exactly the evidence that its state is its own: `ArrayList`,
            // `HashSet` and every other collection this engine backs natively have no `add`
            // bytecode, so they keep the bulk path below (including its `dedup_by_ekey` and the
            // `bulk_key` re-keying fix) unchanged. A guest subclass that overrides `add` is
            // covered by the same rule, and correctly — the JDK calls the override too.
            //
            // Kill switch `RAVA_NO_JAR_ADDALL=1` restores the `$elems` write.
            if jar_add && !add.is_empty() {
                // Each `add` runs guest bytecode that can allocate and therefore MOVE arena
                // objects, so the pending elements ride in a heap `Object[]` — a real root —
                // and are read back forwarded, exactly as `map::dispatch`'s `forEach` arm does.
                // A bare Rust `Vec` is invisible to every root walk.
                let arr = vm.heap.alloc_values("Ljava/lang/Object;", add);
                let n = vm.heap.array_len(arr).unwrap_or(0);
                let mut modified = false;
                for i in 0..n {
                    let e = vm.heap.array_get(arr, i).unwrap_or(Value::Null);
                    let r = vm.call_by_name(
                        Value::Ref(recv),
                        "add",
                        "(Ljava/lang/Object;)Z",
                        vec![e],
                    )?;
                    modified |= r.as_int().unwrap_or(0) != 0;
                }
                return Ok(Some(Some(Value::Int(modified as i32))));
            }
            let is_set = is_set_recv(vm, recv);
            let before = elems.len();
            // A SET's `addAll` RE-KEYED THE WHOLE RECEIVER ONCE PER ELEMENT ADDED. `find_pos`
            // short-circuits only for a SCALAR needle; for anything else it calls
            // `rooted_keys` over the receiver, deriving `ekey_mut` — a guest `toString()`
            // re-entry for a value class — for every element it passes. Run inside this loop,
            // over an `elems` that GROWS with each accepted element, that is
            //   was O(existing x added) key derivations, is O(existing + added).
            // What grows: ELEMENTS ALREADY IN THE SET times ELEMENTS ADDED. Tag/registry
            // building fills one set by repeated `addAll`, which is the quadratic shape.
            //
            // Admitted only when at least TWO added elements are non-scalar, which is the
            // condition under which today's loop derives the receiver's keys at least twice
            // over — so the keyed path is strictly fewer derivations, never more. An
            // all-scalar `addAll` keeps the old loop VERBATIM: there `find_pos` allocates
            // nothing and compares raw bits, and a hash table over two to four entries the
            // caller already holds is the losing trade this round has measured twice.
            //
            // The derivation is `ekey_mut`, the same key `find_pos`'s miss path and the O(1)
            // `add` membership index probe with, and `dedup_by_ekey` — the `new HashSet<>(c)`
            // route into the very same Set — already answers this identical question this
            // identical way. The two bulk routes now agree instead of one being quadratic.
            let bulk_key = is_set
                && fast_collop()
                && add
                    .iter()
                    .filter(|v| {
                        !matches!(
                            v,
                            Value::Int(_) | Value::Long(_) | Value::Float(_) | Value::Double(_)
                        )
                    })
                    // `nth(1)`, not `count()`: the question is "are there two", and it stops
                    // at the second one instead of walking the rest of the source.
                    .nth(1)
                    .is_some();
            if bulk_key {
                // BOTH SIDES STAY ROOTED for the whole derivation. `ekey_mut` runs guest
                // bytecode and a guest call can move the arena, so neither the survivors nor
                // the pending additions may sit in a bare Rust `Vec` across one — the silent
                // stale-pointer class `rooted_keys` exists for. The receiver's array is also
                // what accumulates, so each accepted element is pushed into it rather than
                // into an unrooted local.
                let mine = vm.heap.alloc_values(OBJ, elems);
                let _gpm = gc_protect_array(vm, mine);
                let n = vm.heap.array_len(mine).unwrap_or(0);
                let mut seen: crate::FastSet<String> =
                    crate::FastSet::with_capacity_and_hasher(n + add.len(), Default::default());
                for i in 0..n {
                    let e = vm.heap.array_get(mine, i).unwrap_or(Value::Null);
                    seen.insert(ekey_mut(vm, &e));
                }
                let addarr = vm.heap.alloc_values(OBJ, add);
                let _gpa = gc_protect_array(vm, addarr);
                let m = vm.heap.array_len(addarr).unwrap_or(0);
                for i in 0..m {
                    let v = vm.heap.array_get(addarr, i).unwrap_or(Value::Null);
                    // `seen.insert` is the duplicate test AND the record of it, so an element
                    // added here dedupes the rest of this same batch — which is what the old
                    // loop got from pushing into the `elems` it was scanning.
                    if seen.insert(ekey_mut(vm, &v)) {
                        // Re-read FORWARDED: the derivation above may have moved it.
                        let v = vm.heap.array_get(addarr, i).unwrap_or(Value::Null);
                        vm.heap.array_push(mine, v);
                    }
                }
                elems = vm.heap.array_values(mine).unwrap_or_default();
            } else if is_set {
                for v in add {
                    let nd = ekey_mut(vm, &v);
                    let (back, dup) = find_pos(vm, elems, &v, &nd, false);
                    elems = back;
                    if dup.is_none() {
                        elems.push(v);
                    }
                }
            } else {
                elems.extend(add);
            }
            // `Collection.addAll` returns whether THIS collection changed — false for
            // an empty (or all-duplicate) source. Returning a constant true made the
            // common `if (set.addAll(x)) { recompute(); }` guard always fire.
            let changed = elems.len() != before;
            save!();
            Value::Int(changed as i32)
        }
        // Collection/Set.remove(Object): drop first element equal by value,
        // return whether one was removed (boolean). Distinct from Queue.remove()
        // (no arg -> head element) and List.remove(int) (by index -> element).
        "remove" | "removeFirstOccurrence" | "removeLastOccurrence"
            if descriptor.starts_with("(Ljava/lang/Object;") || descriptor.starts_with("(Ljava/lang/Object;)Z") =>
        {
            let nv = arg1.clone().unwrap_or(Value::Null);
            let needle = ekey_mut(vm, &nv);
            let last = name == "removeLastOccurrence";
            let (back, pos) = find_pos(vm, elems, &nv, &needle, last);
            elems = back;
            let removed = pos.is_some();
            let mut dropped = Vec::new();
            if let Some(i) = pos { dropped.push(elems.remove(i)); save!(); }
            write_through!(dropped);
            Value::Int(removed as i32)
        }
        // List.remove(int): by index, returns the removed element. Out of range
        // throws (the `$elems` fast path already did; this drained path returned
        // null, so the same call diverged depending on the backing).
        "remove" if descriptor.starts_with("(I)") => {
            let i = arg1.as_ref().and_then(Value::as_int).unwrap_or(-1);
            if i < 0 || (i as usize) >= elems.len() {
                return Err(index_oob(vm, i, elems.len()));
            }
            let v = elems.remove(i as usize);
            save!();
            v
        }
        // head removal: poll / remove() / removeFirst / pollFirst / pop
        "poll" | "pollFirst" | "removeFirst" | "pop" | "remove" if !elems.is_empty() => {
            let v = elems.remove(0);
            save!();
            v
        }
        // EMPTY SPLITS THE FAMILY IN TWO, and we answered null for all of it. The JDK's
        // `Queue`/`Deque` contract is that the `poll`/`peek` forms return null and the
        // `pop`/`remove`/`element`/`get*` forms THROW `NoSuchElementException` — the whole
        // reason both families exist. `new ArrayDeque<>().pop()` returned null here where
        // `java -Xint` throws, so a guest draining a queue by `pop` until it threw looped
        // on nulls instead.
        //
        // `remove` is two methods behind one name: no-arg `remove()` is the throwing head
        // removal, `remove(Object)` returns a boolean and is not this arm's business.
        // ⚠`elems.is_empty()` IS PART OF THE GUARD, and leaving it out killed the boot.
        // This arm sits ABOVE the peek family, so without the emptiness test
        // `element`/`getFirst`/`getLast`/`removeLast` threw on every call, empty or not —
        // a match-arm ordering mistake, not a contract one.
        // ⚠`elems.is_empty()` IS PART OF THE GUARD. This arm sits ABOVE the peek family,
        // so without the emptiness test `element`/`getFirst`/`getLast`/`removeLast` threw
        // on EVERY call — the boot died before its first log line, and no probe caught it
        // because every probe exercised the empty case this arm was written for.
        "pop" | "removeFirst" | "removeLast" | "element" | "getFirst" | "getLast"
            if elems.is_empty() =>
        {
            return Err(empty_element(vm));
        }
        "remove" if descriptor.starts_with("()") && elems.is_empty() => {
            return Err(empty_element(vm))
        }
        "poll" | "pollFirst" | "remove" => Value::Null,
        // tail removal
        "pollLast" | "removeLast" if !elems.is_empty() => { let v = elems.pop().unwrap(); save!(); v }
        "pollLast" => Value::Null,
        // head peek
        "peek" | "peekFirst" | "element" | "getFirst" => elems.first().cloned().unwrap_or(Value::Null),
        "peekLast" | "getLast" => elems.last().cloned().unwrap_or(Value::Null),
        // NavigableSet/SortedSet queries over the (now sorted) elements. first/
        // last are the least/greatest; ceiling/floor/higher/lower scan by the
        // comparator. Empty → null (callers guard with isEmpty, as HashTreeSet does).
        // ⛔`first`/`last` DO NOT THROW HERE, and the boot is why. `SortedSet.first` does
        // throw on empty, but this arm is reached by every collection that gets here —
        // including classes where `first()` is not the `SortedSet` method at all — so
        // raising turned a normal early-boot call into a fatal `NoSuchElementException`
        // before the server printed its first line. The prior note (callers guard with
        // `isEmpty`) was a decision, and overriding it needed evidence this did not have.
        "first" => elems.first().cloned().unwrap_or(Value::Null),
        "last" => elems.last().cloned().unwrap_or(Value::Null),
        "ceiling" | "higher" => {
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_NAVIGABLE, elems.len()); // [CMP-CALLER] entry mark
            let x = arg1.clone().unwrap_or(Value::Null);
            let strict = name == "higher";
            let mut out = Value::Null;
            for e in &elems {
                let r = vm.compare(&set_cmp, e, &x)?;
                if r > 0 || (!strict && r == 0) { out = e.clone(); break; }
            }
            out
        }
        "floor" | "lower" => {
            let _cmp_ep = crate::Vm::cmp_entry(crate::Vm::CMP_EP_NAVIGABLE, elems.len()); // [CMP-CALLER] entry mark
            let x = arg1.clone().unwrap_or(Value::Null);
            let strict = name == "lower";
            let mut out = Value::Null;
            for e in elems.iter().rev() {
                let r = vm.compare(&set_cmp, e, &x)?;
                if r < 0 || (!strict && r == 0) { out = e.clone(); break; }
            }
            out
        }
        "drainTo" => {
            let max = args.get(2).and_then(Value::as_int).map(|v| v as usize).unwrap_or(elems.len());
            let n = max.min(elems.len());
            let drained: Vec<Value> = elems.drain(0..n).collect();
            save!();
            if let Some(Value::Ref(t)) = args.get(1) {
                let mut te = elems_of(vm, args.get(1));
                te.extend(drained);
                store_elems(vm, *t, te);
            }
            Value::Int(n as i32)
        }
        // fastIterator = fastutil's reuse-iterator; semantically identical to
        // iterator() for our purposes (TicketStorage iterates its ObjectList via
        // fastIterator during world-spawn ticket scan).
        // `descendingSet` — a NavigableSet in reverse order. It was an
        // UnsatisfiedLinkError, i.e. fatal, and bench/IterSem.java found it by simply
        // STOPPING there: ten observables behind it had never run.
        "descendingSet" => {
            elems.reverse();
            let c = make_list(vm, elems);
            crate::immutable::inherit(vm, recv, &c);
            return Ok(Some(Some(c)));
        }
        // `spliterator()` — another UnsatisfiedLinkError that truncated bench/IterSem.java.
        // Served as the same `$elems`/`$pos` cursor the iterator uses, which is enough for
        // the surface a guest actually reaches (`estimateSize`, `forEachRemaining`,
        // `tryAdvance`); SPLITTING is not modelled and `trySplit` answers null, which is a
        // legal answer for any spliterator that declines to split.
        "spliterator" => {
            let n = elems.len();
            let arr = vm.heap.alloc_values(OBJ, elems);
            let it = vm.heap.alloc("java/util/Spliterator");
            vm.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
            vm.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
            vm.heap.put_field_syn(it, crate::heap::Syn::Source, Value::Ref(recv));
            let _ = n;
            let iv = Value::Ref(it);
            crate::immutable::inherit(vm, recv, &iv);
            return Ok(Some(Some(iv)));
        }
        "iterator" | "listIterator" | "descendingIterator" | "fastIterator" => {
            if name == "descendingIterator" { elems.reverse(); }
            let n = elems.len();
            let arr = vm.heap.alloc_values(OBJ, elems);
            let it = vm.heap.alloc("java/util/Iterator");
            vm.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
            // `listIterator(int)` STARTS AT THAT INDEX. The argument was ignored, so
            // `list.listIterator(list.size())` came back positioned at 0 — every backward
            // walk saw an empty sequence, and `listIterator(2).next()` answered the FIRST
            // element (bench/IterSem.java, `listIter.backwards` / `listIter.fromIndex`).
            let start = if name == "listIterator" {
                arg1.as_ref().and_then(Value::as_int).unwrap_or(0).clamp(0, n as i32)
            } else {
                0
            };
            vm.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(start));
            // Remember the backing collection so Iterator.remove() can delete from
            // it (MC's advancement topological sort removes resolved entries via
            // the iterator; a no-op remove never shrank the set → O(n²) hang).
            vm.heap.put_field_syn(it, crate::heap::Syn::Source, Value::Ref(recv));
            // FAIL-FAST BOOKKEEPING. The JDK's iterators carry the collection's `modCount`
            // and throw ConcurrentModificationException when it moves under them; this
            // engine let a structural modification during iteration pass silently
            // (bench/IterSem.java, `cme.add` / `cme.remove` / `cme.forEachLoop`), and the
            // iterator intrinsic's own comment admitted as much.
            //
            // ⚠What is recorded is the source's SIZE, not a modification counter. It catches
            // every shape that changes the length — which is what `add` and `remove` during
            // iteration do — and MISSES a balanced add-plus-remove, which the JDK would
            // catch. A counter would need every mutator in the tree to maintain it; a size
            // is already there and cannot fall out of step with itself.
            if let Some(n) = source_len(vm, recv) {
                vm.heap.put_field_syn(it, crate::heap::Syn::ExpSize, Value::Int(n as i32));
            }
            // `List.of("a").iterator().remove()` throws: the iterator carries its
            // source's contract, and `Iterator.remove` is the one mutator it has.
            let iv = Value::Ref(it);
            crate::immutable::inherit(vm, recv, &iv);
            iv
        }
        "forEach" => {
            let f = arg1.unwrap_or(Value::Null);
            // `Iterable.forEach` is fail-fast too: `ArrayList.forEach` compares modCount
            // AFTER the loop, so a body that adds to the list it is walking throws.
            let before = source_len(vm, recv);
            rooted_each(vm, elems, |vm, e| {
                vm.apply(&f, vec![e])?;
                Ok(std::ops::ControlFlow::Continue(()))
            })?;
            if before.is_some() && source_len(vm, recv) != before {
                return Err(VmError::Throw(
                    vm.make_exception("java/util/ConcurrentModificationException", ""),
                ));
            }
            return Ok(Some(None));
        }
        // List.replaceAll(UnaryOperator): assign f(e) at every position. There was
        // NO arm for it, so the call fell through to an UnsatisfiedLinkError. It is
        // the one NON-structural bulk write, which is why `Arrays.asList` accepts it
        // where it refuses add/remove — the guard above can only be honest about
        // that distinction if the allowed case actually works.
        "replaceAll" => {
            let f = arg1.unwrap_or(Value::Null);
            // Root the growing result: the operator runs guest bytecode that allocates.
            let out = vm.heap.alloc_values(OBJ, Vec::new());
            let _gp = gc_protect_array(vm, out);
            rooted_each(vm, elems, |vm, e| {
                let nv = vm.apply(&f, vec![e])?.unwrap_or(Value::Null);
                vm.heap.array_push(out, nv);
                Ok(std::ops::ControlFlow::Continue(()))
            })?;
            elems = vm.heap.array_values(out).unwrap_or_default();
            save!();
            return Ok(Some(None));
        }
        // removeIf(Predicate): drop matching elements, return whether any removed.
        "removeIf" => {
            let f = arg1.unwrap_or(Value::Null);
            let before = elems.len();
            // `rooted_each` consumes `elems`; a map view needs the ORIGINAL list to
            // work out which mappings went away, so keep a copy for that case only.
            let orig: Vec<Value> = if view.is_some() { elems.clone() } else { Vec::new() };
            // Root both the unprocessed elements and the kept survivors in heap
            // arrays — the predicate allocates. (Wall #10.)
            let keptarr = vm.heap.alloc_values(OBJ, Vec::new());
            let _gp = gc_protect_array(vm, keptarr);
            rooted_each(vm, elems, |vm, e| {
                if !truthy(&vm.apply(&f, vec![e.clone()])?) {
                    vm.heap.array_push(keptarr, e);
                }
                Ok(std::ops::ControlFlow::Continue(()))
            })?;
            let kept = vm.heap.array_values(keptarr).unwrap_or_default();
            let removed = before != kept.len();
            // THE GUARD FOR `removeIf`, moved here so the predicate runs once (see
            // the note at the classify block). Nothing has been stored yet, so
            // refusing now leaves the receiver exactly as the JDK leaves it.
            let kind = crate::immutable::kind_of(vm, recv);
            if kind != crate::immutable::NONE
                && crate::immutable::refuses(kind, crate::immutable::Op::RemoveMaybe, removed)
            {
                return Err(crate::immutable::uoe(vm));
            }
            // The survivors are indexed by ekey; anything in the original that is
            // not among them was dropped and must leave the backing map too.
            let dropped: Vec<Value> = if view.is_some() {
                let (_, keptk) = rooted_keys(vm, kept.clone());
                let keep: crate::FastSet<String> = keptk.into_iter().collect();
                let (origv, origk) = rooted_keys(vm, orig);
                origv
                    .into_iter()
                    .zip(origk.into_iter())
                    .filter(|(_, k)| !keep.contains(k))
                    .map(|(e, _)| e)
                    .collect()
            } else {
                Vec::new()
            };
            elems = kept;
            save!();
            write_through!(dropped);
            Value::Int(removed as i32)
        }
        // removeAll/retainAll(Collection): set-difference / intersection by content.
        "removeAll" | "retainAll" => {
            let other_elems = elems_of(vm, args.get(1));
            // `content_key`, not `ekey`: entrySet().removeAll(otherEntrySet) is the
            // canonical bulk map edit, and Map$Entry elements are identity under
            // `ekey` — so it matched nothing.
            let (_, otherk) = rooted_content_keys(vm, other_elems);
            let other: crate::FastSet<String> = otherk.into_iter().collect();
            let before = elems.len();
            let keep_when_present = name == "retainAll";
            // Partition rather than `retain`: the DROPPED elements are what a map
            // view has to delete from the backing map. Keys are derived FIRST, with
            // the elements rooted, so the partition below holds no Value across a
            // guest call.
            let (backv, mine) = rooted_content_keys(vm, elems);
            let mut dropped = Vec::new();
            let mut kept = Vec::with_capacity(before);
            for (e, k) in backv.into_iter().zip(mine.into_iter()) {
                if other.contains(&k) == keep_when_present { kept.push(e); }
                else { dropped.push(e); }
            }
            elems = kept;
            let changed = before != elems.len();
            save!();
            write_through!(dropped);
            Value::Int(changed as i32)
        }
        // `toArray(T[] a)` returns an array of a's RUNTIME component type, and when
        // `a` is long enough it fills `a` itself and nulls the slot after the last
        // element (JDK contract — callers size a buffer and read the null terminator).
        // Ignoring `a` entirely returned an `Object[]` of exactly `size` elements, so
        // `list.toArray(new String[0])` was not a `String[]` (its getClass lied and
        // the checked cast the caller writes had nothing to check against), and
        // `toArray(new String[4])` came back length 2 instead of 4.
        "toArray" if args.len() > 1 && matches!(args.get(1), Some(Value::Array(_))) => {
            let elem = crate::stream::source_array_element(vm, args.get(1));
            let n = elems.len();
            match args.get(1) {
                Some(Value::Array(h)) if vm.heap.array_len(*h).unwrap_or(0) >= n => {
                    let h = *h;
                    let cap = vm.heap.array_len(h).unwrap_or(0);
                    for (i, v) in elems.into_iter().enumerate() { vm.heap.array_set(h, i, v); }
                    if cap > n { vm.heap.array_set(h, n, Value::Null); }
                    Value::Array(h)
                }
                _ => Value::Array(vm.heap.alloc_values(&elem, elems)),
            }
        }
        "toArray" | "elements" | "toIntArray" | "toLongArray" | "toDoubleArray" | "toByteArray" => {
            Value::Array(vm.heap.alloc_values(OBJ, elems))
        }
        // Rendered into ONE buffer. The `Vec<String>` + `join` + `format!` built the whole
        // text three times over — the parts vector, the join's buffer, then the format's —
        // to hand back a string only the last of them is. Was 3 buffers proportional to the
        // TOTAL RENDERED LENGTH (which grows with elements and with each element's own
        // rendering), is 1; the per-element `String` that `value_to_string` returns is
        // unavoidable and unchanged, as is the order of the guest `toString` calls.
        "toString" if fast_collop() => {
            let mut out = String::with_capacity(2 + elems.len() * 8);
            out.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&vm.value_to_string(e)?);
            }
            out.push(']');
            Value::Str(out.into())
        }
        "toString" => {
            let mut parts = Vec::with_capacity(elems.len());
            for e in &elems { parts.push(vm.value_to_string(e)?); }
            Value::Str(format!("[{}]", parts.join(", ")).into())
        }
        "spliterator" => {
            let arr = vm.heap.alloc_values(OBJ, elems);
            let sp = vm.heap.alloc("java/util/Spliterator");
            vm.heap.put_field_syn(sp, crate::heap::Syn::Elems, Value::Array(arr));
            Value::Ref(sp)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
