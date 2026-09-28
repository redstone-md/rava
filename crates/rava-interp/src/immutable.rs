//! The JDK's unmodifiable / fixed-size collection contracts, as a fail-fast marker.
//!
//! Our collections are all one representation (a `$elems` array, or a `$map` id),
//! so nothing distinguished `List.of("a")` from `new ArrayList<>()` and every
//! mutator silently succeeded where `java -Xint` throws
//! `UnsupportedOperationException`. This module adds ONE synthetic field —
//! `$immutable` — stamped by the factory sites that the JDK makes unmodifiable,
//! and a decision table the mutating arms consult before they write.
//!
//! The JDK has TWO families here and they are NOT the same contract. Measured
//! against `java -Xint`, not assumed:
//!
//!  * `ImmutableCollections` — `List.of` / `Set.of` / `Map.of` / `Map.ofEntries`
//!    / `*.copyOf` — and the `Collections.Unmodifiable*` wrappers override EVERY
//!    mutator to throw UNCONDITIONALLY. `List.of("a").remove("absent")` throws;
//!    so does `List.of().sort(null)` and `List.of("a").removeIf(x -> false)`.
//!
//!  * The `AbstractList`/`AbstractMap`-backed singletons — `Collections.emptyList`
//!    / `emptySet` / `emptyMap` / `singletonList` / `singleton` / `nCopies`, and
//!    `Arrays.asList` — inherit the DEFAULT bodies, which only reach the throwing
//!    primitive when the operation would actually mutate. So
//!    `Collections.emptyList().removeIf(p)` returns false and
//!    `Collections.emptyList().sort(c)` succeeds, while `emptyList().add(x)`
//!    throws. Of these only `Arrays.asList` also overrides `set`, which is why
//!    `Arrays.asList(a).set(0, z)` / `.sort(c)` / `.replaceAll(f)` succeed while
//!    `Collections.singletonList("a").set(0, "b")` throws.
//!
//! The marker is per-OBJECT, never per-class, so it says nothing about any
//! workload — a factory result carries it, a `new ArrayList<>(List.of(...))` copy
//! does not.

use crate::value::Value;
use crate::{Vm, VmError};

/// The synthetic marker field. Absent (or 0) = a normal mutable collection.
pub(crate) const FIELD: &str = "$immutable";

/// Not marked — every mutator proceeds.
pub(crate) const NONE: i32 = 0;
/// `ImmutableCollections` / `Collections.Unmodifiable*`: every mutator throws,
/// whatever the contents.
pub(crate) const IMMUTABLE: i32 = 1;
/// `Arrays.asList`: fixed SIZE only. `set`/`sort`/`replaceAll` are overridden and
/// succeed (they write through to the backing array); anything that would change
/// the size throws.
pub(crate) const FIXED_SET_OK: i32 = 2;
/// `Collections.emptyList/emptySet/emptyMap/nCopies`: fixed size AND no element
/// assignment, but only where the inherited default body would really have to
/// mutate.
pub(crate) const FIXED_SET_UOE: i32 = 3;
/// `Collections.singletonList/singleton/singletonMap` — the ONE-element
/// singletons, which are NOT the same contract as the empty ones even though
/// both are AbstractList-backed. Measured against `java -Xint`, all three lines
/// from `bench/ReviewEx.java`:
///
///   `Collections.singletonList("a").removeIf(x -> false)` throws
///   `Collections.emptyList().removeIf(x -> true)`        returns false
///   `Collections.nCopies(2, "x").removeIf(x -> false)`   returns false
///
/// because `SingletonList`/`SingletonSet` OVERRIDE `removeIf` to throw
/// unconditionally, `EmptyList` overrides it to return false, and `CopiesList`
/// overrides neither and inherits the conditional default. One kind cannot
/// express three answers, so the singletons get their own.
pub(crate) const SINGLETON: i32 = 4;
/// A MAP VIEW — `keySet()`, `values()`, `entrySet()`. The JDK's views support REMOVAL (it
/// deletes the mapping) and refuse ADDITION, because there is no key to add a lone value
/// under. No existing kind expresses that: `IMMUTABLE` refuses everything and
/// `FIXED_SET_OK` refuses removal too, so `map.keySet().add(x)` answered silently instead of
/// throwing (bench/MapViewSem.java, `keySet.add` / `values.add`).
pub(crate) const VIEW_NO_ADD: i32 = 5;

/// What a collection method would do to its receiver. The conditional kinds need
/// this to decide whether the JDK's inherited body would reach its throwing
/// primitive at all.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Op {
    /// add / offer / push / addFirst / add(int,E) — always grows.
    Insert,
    /// addAll — grows iff the argument collection is non-empty.
    InsertAll,
    /// clear / remove(Object) / removeIf / removeAll / retainAll — shrinks only
    /// if it finds something to drop.
    RemoveMaybe,
    /// remove(int) / poll / pop / removeFirst / drainTo — `AbstractList.remove(int)`
    /// IS the UOE stub and the Deque heads have no default body, so these reach
    /// the throw whatever the contents.
    RemoveAlways,
    /// set / replaceAll — a non-structural write, reached through
    /// `ListIterator.set`, which the fixed-size kinds refuse.
    Write,
    /// sort — SEPARATE from `Write`, because `Collections.singletonList` and
    /// `Collections.emptyList` both OVERRIDE `sort` with an empty body while
    /// leaving `set` to throw. Folding the two together made
    /// `Collections.singletonList("a").sort(null)` throw where the JVM returns
    /// normally: a new exception in correct guest code, which is the worst thing
    /// this module can do.
    Sort,
}

/// Classify a collection method by descriptor and name only (no receiver class,
/// no workload identity). `None` = a pure read, which never needs a guard.
pub(crate) fn classify(name: &str, descriptor: &str) -> Option<Op> {
    let out = classify_inner(name, descriptor);
    crate::contract_stat::classify_call(out.is_some());
    out
}

#[inline]
fn classify_inner(name: &str, descriptor: &str) -> Option<Op> {
    Some(match name {
        "add" | "addElement" | "offer" | "offerFirst" | "offerLast" | "addFirst" | "addLast"
        | "push" => Op::Insert,
        "addAll" => Op::InsertAll,
        "clear" | "removeIf" | "removeAll" | "retainAll" => Op::RemoveMaybe,
        // `remove` is three methods behind one name: remove(int) (by index),
        // remove(Object) (by value), and Queue.remove() (the head). Only the
        // by-value form is conditional — the other two always reach the throw.
        "remove" | "removeFirstOccurrence" | "removeLastOccurrence" => {
            if name == "remove" && descriptor.starts_with("(I") {
                Op::RemoveAlways
            } else if descriptor.starts_with("(Ljava/lang/Object;") {
                Op::RemoveMaybe
            } else {
                Op::RemoveAlways
            }
        }
        "poll" | "pollFirst" | "pollLast" | "pop" | "removeFirst" | "removeLast" | "drainTo" => {
            Op::RemoveAlways
        }
        "sort" => Op::Sort,
        "set" | "replaceAll" => Op::Write,
        _ => return None,
    })
}

/// Stamp `v` with a contract kind. A no-op for a non-handle value: every factory
/// below builds its result with `heap.alloc`, so the marker always lands.
pub(crate) fn mark(vm: &mut Vm, v: &Value, kind: i32) {
    if let Value::Ref(h) = v {
        vm.heap.put_field(*h, FIELD, Value::Int(kind));
        // AFTER the field write, and with `Release`, so a thread that observes the
        // bit also observes the field it advertises.
        mark_bit_set(*h);
        return;
    }
    // ⭐**AN ARENA OBJECT CAN CARRY THE CONTRACT TOO, AND IT HAS TO.** The comment above was
    // true while every factory built its result with `heap.alloc`: a class this engine serves
    // natively is born as a handle. A class OVERRIDDEN BY BYTECODE is not native-served, so it
    // is born in the arena — and marking it silently did nothing, which is how
    // `Collections.unmodifiableList(...).add(x)` succeeded against a guest `ArrayList` while
    // `List.of(...).add(x)` (still handle-born) correctly threw.
    //
    // The bytecode side reads the field directly, so only the field write is needed here; the
    // side bit is a filter for the NATIVE mutators, which do not serve this object anyway.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = v {
        let id = vm.heap.field_id(FIELD);
        crate::opcodes::arena_cells::arena_putfield(*p, id, &Value::Int(kind));
    }
}

/// NEGATIVE FILTER over the contract marker.
///
/// The census (`RAVA_CONTRACT_STAT`) counted **4,000,000 `kind_of` calls in one
/// boot, 100% of them armed, and 0.5258% of those finding a contract**. Once any
/// factory has interned the name every guarded call pays a real field-blob probe
/// to be told "no" — 3.98M misses to serve 21k hits. `mark` above is the ONLY
/// writer of `$immutable` in the tree (the name is synthetic, so no bytecode can
/// name it), which is what makes a bit per marked handle a complete answer to the
/// negative case.
///
/// SOUND BY CONSTRUCTION. A CLEAR bit means `mark` was never called for that
/// handle, so the field cannot be set and `NONE` is the right answer without
/// touching the heap. A SET bit only sends the call down the probe it would have
/// taken anyway, so a handle the GC recycled onto a fresh object still reads the
/// field and still gets the right answer — the filter can be conservative but
/// never wrong.
///
/// Fixed capacity, no lock and no growth: a handle past the table reports
/// "maybe" and falls through to the probe. 512 KB of statics covers the first 4M
/// handles, and a boot allocates far fewer marked ones than that.
const MARK_CAP: u32 = 4 << 20;
const MARK_WORDS: usize = (MARK_CAP / 64) as usize;
static MARK_BITS: [std::sync::atomic::AtomicU64; MARK_WORDS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; MARK_WORDS];

fn mark_bit_set(h: u32) {
    if h < MARK_CAP {
        MARK_BITS[(h / 64) as usize]
            .fetch_or(1u64 << (h % 64), std::sync::atomic::Ordering::Release);
    }
}

/// `false` only when the handle is PROVABLY unmarked.
#[inline]
fn mark_bit_maybe(h: u32) -> bool {
    if h >= MARK_CAP {
        return true;
    }
    MARK_BITS[(h / 64) as usize].load(std::sync::atomic::Ordering::Acquire) & (1u64 << (h % 64)) != 0
}

/// Copy `src`'s contract kind onto `dst`, if it has one.
///
/// THE DERIVED OBJECT INHERITS THE CONTRACT. `Map.of(..).keySet()`,
/// `List.of(..).subList(..)`, `List.of(..).iterator()` and an `entrySet()` entry
/// are all views of an unmodifiable receiver, and the JDK refuses every mutator
/// through them exactly as it refuses one on the receiver itself. Ours handed
/// back a FRESH, unmarked collection, so `Map.of("k","v").keySet().remove("k")`
/// silently emptied a frozen map — a wrong answer with no exception at all.
///
/// The KIND is copied verbatim rather than forced to `IMMUTABLE`, because the
/// conditional contract propagates too: `Collections.emptyMap().keySet().remove(x)`
/// returns false on the JVM (nothing to remove) where `Map.of().keySet().add(x)`
/// throws.
pub(crate) fn inherit(vm: &mut Vm, src: u32, dst: &Value) {
    let kind = kind_of(vm, src);
    if kind != NONE {
        mark(vm, dst, kind);
    }
}

/// The contract kind stamped on `recv`, or `NONE`.
///
/// BY ID, for the same reason `republish_backing` is: this runs on EVERY guarded
/// collection and map call, and a by-NAME `get_field` hashes the field name under
/// the interner's RwLock each time — the cost `key_render.rs` took off the key
/// path. Reading it by name cost **wall 57.7 -> 68.1 s and Done 19.8 -> 22.9**
/// over three paired boots, DISJOINT, for a probe that MISSES on every collection
/// in the boot.
///
/// Until some factory has stamped a marker the field name is not interned at all,
/// so the whole guard is one `OnceLock` read and a return. The failure is never
/// memoised: the name is interned late, and freezing `None` before the first
/// `List.of` would disarm every guard for the rest of the process.
#[inline]
pub(crate) fn kind_of(vm: &Vm, recv: u32) -> i32 {
    static FIELD_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let fid = match FIELD_ID.get() {
        Some(&id) => id,
        None => match vm.heap.field_id_opt(FIELD) {
            Some(id) => {
                let _ = FIELD_ID.set(id);
                id
            }
            None => {
                crate::contract_stat::kind_of_probe(false, false);
                return NONE;
            }
        },
    };
    // The filter answers the 99.47% case without a field probe.
    if !mark_bit_maybe(recv) {
        crate::contract_stat::kind_of_probe(true, false);
        return NONE;
    }
    let kind = vm.heap.get_field_hint(recv, fid, 0).0.as_int().unwrap_or(NONE);
    crate::contract_stat::kind_of_probe(true, kind != NONE);
    kind
}

/// The `UnsupportedOperationException` every refusal throws. The JDK's carries no
/// message (`ImmutableCollections.uoe()` / `Collections$Unmodifiable*`), so ours
/// does not either — a message would show up in guest `getMessage()` output.
pub(crate) fn uoe(vm: &mut Vm) -> VmError {
    VmError::Throw(vm.make_exception("java/lang/UnsupportedOperationException", ""))
}

/// The `NullPointerException` the `of()` factories raise from their
/// `Objects.requireNonNull` on every element / key / value.
pub(crate) fn npe(vm: &mut Vm) -> VmError {
    VmError::Throw(vm.make_exception("java/lang/NullPointerException", ""))
}

/// The `IllegalArgumentException` `Set.of` / `Map.of` raise on a duplicate.
/// The JDK's message is `"duplicate element: <e>"` / `"duplicate key: <k>"`.
pub(crate) fn iae(vm: &mut Vm, message: &str) -> VmError {
    VmError::Throw(vm.make_exception("java/lang/IllegalArgumentException", message))
}

/// Decide a guarded call, given whether the operation would actually change the
/// receiver. `would_change` is only consulted for the conditional kinds, so the
/// caller may compute it lazily.
pub(crate) fn refuses(kind: i32, op: Op, would_change: bool) -> bool {
    match kind {
        IMMUTABLE => true,
        SINGLETON => match op {
            Op::Insert | Op::RemoveAlways => true,
            // `SingletonList.removeIf` / `SingletonSet.removeIf` throw whatever the
            // predicate answers — they do not consult it at all.
            Op::RemoveMaybe => true,
            Op::InsertAll => would_change,
            // `set` throws (no override, so it reaches `ListIterator.set`);
            // `sort` does not (overridden with an empty body).
            Op::Write => true,
            Op::Sort => false,
        },
        // A map view: additions throw, removals are the whole point of it, and `set`/`sort`
        // never reach a view at all (no view is a List).
        VIEW_NO_ADD => matches!(op, Op::Insert | Op::InsertAll),
        FIXED_SET_OK | FIXED_SET_UOE => match op {
            Op::Insert | Op::RemoveAlways => true,
            Op::InsertAll | Op::RemoveMaybe => would_change,
            // Arrays.asList overrides set; the emptyX family does not, so there it
            // drives `ListIterator.set` — reached only when there is an element to
            // assign, which for an EMPTY receiver is never.
            Op::Write => kind == FIXED_SET_UOE && would_change,
            // Both families override `sort` with an empty body.
            Op::Sort => false,
        },
        _ => false,
    }
}

/// The back-link a wrapped collection carries: the `Collections.unmodifiable*`
/// views that SHARE its backing `$elems` array.
pub(crate) const VIEWS: &str = "$unmodviews";

/// Build the `Collections.unmodifiable*` view of `src`.
///
/// This one factory cannot use the marker directly: `unmodifiable*` returns a
/// live VIEW, so stamping the argument would freeze the BACKING collection too —
/// and `list.add(x)` after `Collections.unmodifiableList(list)` is legal Java.
/// The view therefore needs its own object identity, which is the whole reason
/// the other nine factory sites are one line each and this one is not.
///
/// The wrapper is a fresh handle of the SOURCE'S OWN CLASS — not a synthetic
/// "Unmodifiable*" class — because every class-driven decision downstream must
/// come out the same: `coll_op`'s Set-vs-List dedup test, the sorted-set ordering
/// test, and `map_key_inner`'s identity-vs-value keying (an
/// `unmodifiableMap(identityHashMap)` derived under a different class name would
/// key by VALUE and miss every entry the source stored by identity).
///
/// It SHARES the backing rather than copying it, so the view stays live: the
/// `$elems` array handle for a collection (`store_elems` re-points registered
/// views when it re-points the source — see `republish_backing`), the `$map` id
/// for a map (which never changes once allocated, so it needs no republishing).
///
/// A source with NEITHER — a real jar collection that is drained through its own
/// bytecode, or a not-yet-populated one — has no backing to alias, so it is
/// handed back unchanged exactly as before. That is a strictly conservative
/// fallback: it can only fail to throw, never read the wrong elements.
pub(crate) fn unmodifiable_view(vm: &mut Vm, src: &Value) -> Value {
    // ⭐**AN OVERRIDDEN CLASS IS ARENA-BORN, AND THIS FUNCTION USED TO HAND IT STRAIGHT BACK.**
    // The `Value::Ref` bail below (and the `size()I`-has-no-bytecode test further down) both
    // encode "a collection of ours is handle-born and has no bytecode", which an overridden
    // `java.util` class violates on both counts — so `unmodifiableList(guestList) == guestList`
    // and `un.add(x)` simply succeeded.
    //
    // For such a source the wrapper DELEGATES rather than sharing a backing array: the class's
    // own bytecode follows `$usrc`, which stays live across a growth that replaces `$elems`.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = src {
        let cls = crate::nextgen::class_name(crate::nextgen::arena_class_id(*p));
        if let Some(cls) = cls.filter(|c| crate::class_loader::has_bytecode(c)) {
            let w = vm.heap.alloc(&cls);
            vm.heap.put_field(w, "$usrc", src.clone());
            vm.heap.put_field_syn(w, crate::heap::Syn::Init, Value::Int(1));
            let v = Value::Ref(w);
            mark(vm, &v, IMMUTABLE);
            return v;
        }
    }
    let Value::Ref(sh) = src else { return src.clone() };
    let sh = *sh;
    let mut backing = vm.heap.get_field_syn(sh, crate::heap::Syn::Elems);
    let table = vm.heap.get_field_syn(sh, crate::heap::Syn::Map);
    let cls = vm.heap.class_of(sh).unwrap_or("java/util/ArrayList").to_string();
    // A collection of ours that has never held an element has no `$elems` yet
    // (`coll_op`'s `<init>` only stores one when it copies a source), so there
    // would be nothing to alias and `unmodifiableList(new ArrayList<>())` would
    // silently stay mutable. Give it the empty backing it will get on its first
    // add — but ONLY when the class has no `size()I` bytecode, i.e. it really is
    // one of ours. For a real jar collection (drained through its own bytecode by
    // `iterate_object`) an empty `$elems` would SHADOW that drain and read it as
    // empty, so those keep the conservative fallback below. Resolution failure
    // counts as "has bytecode".
    let mut table = table;
    if !matches!(backing, Value::Array(_)) && !matches!(table, Value::Int(_)) {
        let synthetic = matches!(vm.resolve_method(&cls, "size", "()I"), Ok(None));
        if !synthetic {
            return src.clone();
        }
        // A MAP that has not been written to yet has no `$map` id either, and the
        // collection branch below would stamp an empty `$elems` ON THE MAP —
        // giving it a second, contradictory backing — and hand back a wrapper with
        // no table at all, so `unmodifiableMap(new HashMap<>()).get(k)` read from
        // nothing. Mint the id instead, which is exactly what the map's own first
        // write would have done.
        if crate::map::is_map_value(vm, src) {
            table = Value::Int(crate::map::map_id(vm, sh) as i32);
        } else {
            let empty = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
            vm.heap.put_field_syn(sh, crate::heap::Syn::Elems, Value::Array(empty));
            backing = Value::Array(empty);
        }
    }
    let w = vm.heap.alloc(&cls);
    if matches!(backing, Value::Array(_)) {
        vm.heap.put_field_syn(w, crate::heap::Syn::Elems, backing);
        register_view(vm, sh, w);
    }
    if matches!(table, Value::Int(_)) {
        vm.heap.put_field_syn(w, crate::heap::Syn::Map, table);
    }
    // A sorted set/map reads its ordering off the receiver, so the view must
    // carry the same comparator to iterate in the same order.
    let cmp = vm.heap.get_field(sh, "$comparator");
    if !matches!(cmp, Value::Null) {
        vm.heap.put_field(w, "$comparator", cmp);
    }
    let v = Value::Ref(w);
    mark(vm, &v, IMMUTABLE);
    v
}

/// Record `view` on `src` so a later re-point of `src`'s backing array reaches it.
fn register_view(vm: &mut Vm, src: u32, view: u32) {
    /// Past this many registered views the list stops growing.
    ///
    /// `Collections.unmodifiableList(this.items)` inside a GETTER is idiomatic
    /// Java and mints a fresh wrapper per call, and every one of them was appended
    /// here — an array the source keeps alive forever, walked by
    /// `republish_backing` on every write, so a hot getter turned a collection's
    /// writes quadratic and leaked. Dropping later registrations costs a stale
    /// READ through a wrapper past the cap; keeping them costs the process.
    const MAX_VIEWS: usize = 64;
    match vm.heap.get_field(src, VIEWS) {
        Value::Array(a) => {
            let n = vm.heap.array_len(a).unwrap_or(0);
            // Already registered? The same wrapper handle can only appear once.
            for i in 0..n {
                if matches!(vm.heap.array_get(a, i), Some(Value::Ref(h)) if h == view) {
                    return;
                }
            }
            if n < MAX_VIEWS {
                vm.heap.array_push(a, Value::Ref(view));
            }
        }
        _ => {
            let a = vm.heap.alloc_values("Ljava/lang/Object;", vec![Value::Ref(view)]);
            vm.heap.put_field(src, VIEWS, Value::Array(a));
        }
    }
}

/// Re-point every registered `Collections.unmodifiable*` view of `obj` at its new
/// backing array. Called from the two places that REPLACE a live collection's
/// `$elems` (`stream::store_elems` and the fastutil int-set store); the O(1)
/// arms mutate the array in place, so the shared handle stays valid there with
/// nothing to do. Without this the view would freeze at the array it was built
/// over — a stale read, which is worse than the missing throw it exists to add.
pub(crate) fn republish_backing(vm: &mut Vm, obj: u32, arr: Value) {
    // BY ID: `get_field` hashes the field name under the interner's RwLock, and
    // this sits on the general collection write path. The id only exists once some
    // `unmodifiable*` view has been built, so a program that never calls it pays a
    // OnceLock read. (Never memoise the failure — the name is interned late.)
    static VIEWS_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vid = match VIEWS_ID.get() {
        Some(&id) => id,
        None => match vm.heap.field_id_opt(VIEWS) {
            Some(id) => {
                let _ = VIEWS_ID.set(id);
                id
            }
            None => {
                crate::contract_stat::views_probe(false, false);
                return;
            }
        },
    };
    let Value::Array(views) = vm.heap.get_field_hint(obj, vid, 0).0 else {
        crate::contract_stat::views_probe(true, false);
        return;
    };
    crate::contract_stat::views_probe(true, true);
    let n = vm.heap.array_len(views).unwrap_or(0);
    for i in 0..n {
        if let Some(Value::Ref(v)) = vm.heap.array_get(views, i) {
            vm.heap.put_field_syn(v, crate::heap::Syn::Elems, arr.clone());
        }
    }
}

// --- maps -------------------------------------------------------------------

/// What a map method would do to its receiver, in the same two-family shape.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum MapOp {
    /// put / putIfAbsent / merge / compute / computeIfAbsent / … — always stores.
    Store,
    /// putAll — stores iff the source map is non-empty.
    StoreAll,
    /// remove / clear / computeIfPresent / replace / replaceAll — touches the
    /// table only when the key (or any entry) is there.
    DropMaybe,
    /// The fastutil order/head mutators, which have no JDK default body to
    /// inherit and so always reach the throw.
    Always,
}

/// Classify a map method by name only. `None` = a read.
pub(crate) fn classify_map(name: &str) -> Option<MapOp> {
    Some(match name {
        "put" | "forcePut" | "setProperty" | "putIfAbsent" | "merge" | "compute"
        | "computeIfAbsent" | "load" => MapOp::Store,
        "putAll" => MapOp::StoreAll,
        "remove" | "clear" | "computeIfPresent" | "replace" | "replaceAll"
        | "removeBoolean" | "removeInt" | "removeLong" | "removeShort" => MapOp::DropMaybe,
        "addTo" | "putAndMoveToFirst" | "getAndMoveToFirst" | "removeFirst" | "removeLast"
        | "pollFirstEntry" | "pollLastEntry" => MapOp::Always,
        _ => return None,
    })
}

/// Decide a guarded map call. Same contract split as `refuses`.
pub(crate) fn refuses_map(kind: i32, op: MapOp, would_change: bool) -> bool {
    match kind {
        IMMUTABLE => true,
        // `Collections.singletonMap` is a one-entry AbstractMap: every mutator
        // reaches the throwing primitive because there is always an entry there.
        SINGLETON => true,
        FIXED_SET_OK | FIXED_SET_UOE => match op {
            MapOp::Store | MapOp::Always => true,
            MapOp::StoreAll | MapOp::DropMaybe => would_change,
        },
        _ => false,
    }
}
