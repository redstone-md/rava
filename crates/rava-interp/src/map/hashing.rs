//! JDK `hashCode` folds and map-VIEW write-through, for the collections rava
//! serves natively.
//!
//! Two things live here because both are properties of the same object graph:
//! the SPEC-defined hash of a collection (a fold over its ELEMENTS' hashes) and
//! the rule that `keySet()`/`values()`/`entrySet()` are LIVE views of a map, so
//! a removal through one deletes the mapping from the map behind it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::*;

// --- element hashes ---------------------------------------------------------

/// Maximum nesting a collection hash descends. The JDK simply recurses (and
/// blows the stack on a self-referential collection); a bound keeps a cyclic
/// guest structure from taking the VM down, and beyond it the element falls back
/// to the identity hash — the value `Object.hashCode` already returns here.
const HASH_DEPTH: u32 = 8;

/// `element.hashCode()` for the values whose hash the JVM specifies from the
/// VALUE alone: null, the boxed primitives we can recognise, String, and the
/// natively-served collections (which fold their own elements).
///
/// `None` means "this element's hash is whatever its class says", which for a
/// guest class that declares `hashCode()` we cannot answer without running a
/// guest frame from inside a native — the one re-entry this tree has measured as
/// unsafe. The caller substitutes `identity_hash`, i.e. exactly what
/// `Object.hashCode` already returns for that element today, so the fold is
/// correct whenever the JDK's own answer is identity and self-consistent
/// (two collections over the SAME instances hash equal) otherwise.
fn spec_hash(vm: &mut Vm, v: &Value, depth: u32) -> Option<i32> {
    match v {
        Value::Null => Some(0),
        // Integer/Short/Byte/Character all hash to their value. A Boolean that
        // survived as a bare Int hashes 1/0 here where the JVM says 1231/1237 —
        // that is the boxed-primitive-type-retention gap, not this fold.
        Value::Int(i) => Some(*i),
        Value::Long(l) => Some((*l ^ ((*l as u64) >> 32) as i64) as i32),
        Value::Float(f) => Some(f.to_bits() as i32),
        Value::Double(d) => {
            let b = d.to_bits();
            Some((b ^ (b >> 32)) as i32)
        }
        Value::Str(s) => Some(string_hash(s)),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) =>
            Some(crate::nextgen::arena_str_hash(*p)),
        Value::Ref(h) => ref_hash(vm, *h, depth),
        _ => None,
    }
}

/// `String.hashCode` — `s[0]*31^(n-1) + …`. Folds over CHARS, so a supplementary
/// (non-BMP) code point folds once where the JVM folds its two UTF-16 units.
fn string_hash(s: &str) -> i32 {
    s.chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

/// Read one of our fixed `$`-fields by a CACHED id.
///
/// These names never change, but a by-NAME `get_field` hashes the string under
/// the field interner's lock on EVERY probe — the exact cost `key_render.rs`
/// removed from the key path, and this runs per element of a hash/equality fold.
/// Only a RESOLVED id is cached: a `None` frozen before the name was ever
/// interned would misclassify every object created afterwards.
fn synth_field(
    vm: &mut Vm,
    h: u32,
    cell: &'static std::sync::OnceLock<u32>,
    name: &'static str,
) -> Value {
    let id = match cell.get() {
        Some(&id) => Some(id),
        None => vm.heap.field_id_opt(name).inspect(|&id| {
            let _ = cell.set(id);
        }),
    };
    match id {
        Some(id) => vm.heap.get_field_hint(h, id, 0).0,
        None => Value::Null,
    }
}

static STR_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static VALUE_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static KEY_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static VAL_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static MAP_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static ELEMS_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static MAPVIEW_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// The value-defined hash of a heap object: a String box, a boxed Boolean, a
/// `Map$Entry` (`key ^ value`), a native `$map`-backed Map (sum of entry hashes)
/// or an `$elems`-backed List/Set. Anything else → `None` (identity).
fn ref_hash(vm: &mut Vm, h: u32, depth: u32) -> Option<i32> {
    if let Value::Str(s) = synth_field(vm, h, &STR_ID, "$str") {
        return Some(string_hash(&s));
    }
    // `Boolean.valueOf` BOXES into a real java/lang/Boolean object (so
    // `instanceof Boolean` works), carrying the flag in `$value`. The JDK hashes
    // the PAYLOAD — 1231/1237 — where the wrapper would otherwise hash by handle.
    if vm.heap.class_of(h) == Some("java/lang/Boolean") {
        let set = synth_field(vm, h, &VALUE_ID, "$value").as_int().unwrap_or(0) != 0;
        return Some(if set { 1231 } else { 1237 });
    }
    // ⭐**AND THE SAME FOR THE THREE NARROW BOXES** (`crate::boxid`). `Character.hashCode()`,
    // `Short.hashCode()` and `Byte.hashCode()` are all `(int) value` in the JDK, and this
    // function is the by-VALUE hash a native collection uses — so without this arm a
    // `HashSet<Character>` hashed each box by its HANDLE, which is a fresh number per object
    // and made `contains` miss for every char outside the interning range. Reads the same
    // `$value` slot (and the same memoised field id) the Boolean arm above does.
    // All three hash to the payload, so the KIND is not needed once it is known to be one of
    // them — only that it IS one, which is what keeps the `$value` read off every other object.
    if vm.heap.class_of(h).and_then(crate::boxid::Kind::of_class).is_some() {
        return Some(synth_field(vm, h, &VALUE_ID, "$value").as_int().unwrap_or(0));
    }
    if depth >= HASH_DEPTH {
        return None;
    }
    if vm.heap.class_of(h).is_some_and(is_entry_class) {
        let k = synth_field(vm, h, &KEY_ID, "$key");
        let val = synth_field(vm, h, &VAL_ID, "$val");
        return Some(entry_hash_at(vm, &k, &val, depth + 1));
    }
    if let Value::Int(mid) = synth_field(vm, h, &MAP_ID, "$map") {
        let entries = vm.heap.map_entries(mid as u32);
        return Some(fold_map(vm, &entries, depth + 1));
    }
    if let Value::Array(arr) = synth_field(vm, h, &ELEMS_ID, "$elems") {
        let elems = vm.heap.array_values(arr).unwrap_or_default();
        let set = vm.heap.class_of(h).is_some_and(|c| c.contains("Set"))
            || matches!(synth_field(vm, h, &MAPVIEW_ID, "$mapview"), Value::Str(_));
        return Some(if set {
            fold_set(vm, &elems, depth + 1)
        } else {
            fold_list(vm, &elems, depth + 1)
        });
    }
    None
}

fn hash_at(vm: &mut Vm, v: &Value, depth: u32) -> i32 {
    spec_hash(vm, v, depth).unwrap_or_else(|| crate::natives::identity_hash(Some(v)))
}

/// `element.hashCode()` as the natively-served collections fold it.
pub(crate) fn element_hash(vm: &mut Vm, v: &Value) -> i32 {
    hash_at(vm, v, 0)
}

fn entry_hash_at(vm: &mut Vm, k: &Value, v: &Value, depth: u32) -> i32 {
    hash_at(vm, k, depth) ^ hash_at(vm, v, depth)
}

/// `Map.Entry.hashCode()` — `key.hashCode() ^ value.hashCode()` (null → 0).
pub(crate) fn entry_hash(vm: &mut Vm, k: &Value, v: &Value) -> i32 {
    entry_hash_at(vm, k, v, 0)
}

fn fold_map(vm: &mut Vm, entries: &[(Value, Value)], depth: u32) -> i32 {
    entries
        .iter()
        .fold(0i32, |acc, (k, v)| acc.wrapping_add(entry_hash_at(vm, k, v, depth)))
}

/// `AbstractMap.hashCode()` — the SUM of the entry hashes, so it is independent
/// of iteration order.
pub(crate) fn map_hash(vm: &mut Vm, entries: &[(Value, Value)]) -> i32 {
    fold_map(vm, entries, 0)
}

fn fold_set(vm: &mut Vm, elems: &[Value], depth: u32) -> i32 {
    elems.iter().fold(0i32, |acc, e| acc.wrapping_add(hash_at(vm, e, depth)))
}

fn fold_list(vm: &mut Vm, elems: &[Value], depth: u32) -> i32 {
    elems
        .iter()
        .fold(1i32, |acc, e| acc.wrapping_mul(31).wrapping_add(hash_at(vm, e, depth)))
}

/// `AbstractSet.hashCode()` — sum of the element hashes (order-independent).
pub(crate) fn set_hash(vm: &mut Vm, elems: &[Value]) -> i32 {
    fold_set(vm, elems, 0)
}

/// `AbstractList.hashCode()` — `31*h + e`, seeded 1 (order-dependent).
pub(crate) fn list_hash(vm: &mut Vm, elems: &[Value]) -> i32 {
    fold_list(vm, elems, 0)
}

// --- content keys for value-equality ----------------------------------------

/// `ekey_mut`, extended over the CONTAINER shapes rava serves natively — a
/// `Map$Entry`, a `$map`-backed Map, an `$elems`-backed List/Set — whose
/// equality the JDK defines by CONTENT where the element key gives identity.
/// Everything else is `ekey_mut` verbatim, so this changes the answer for
/// exactly those three shapes and for nothing else.
///
/// Used by the value-equality operations only. Its baseline is `ekey_mut`, the
/// same key the membership index and every `&mut Vm` scan derive, so the
/// `content_key == ekey` test below means exactly "this is NOT a container".
pub(crate) fn content_key(vm: &mut Vm, v: &Value) -> String {
    content_key_at(vm, v, 0)
}

/// `Some(content_key)` when `v` IS one of those container shapes, `None` when it
/// is anything else — i.e. exactly when `content_key` differs from `ekey`. Lets a
/// caller ask "can I answer equality for this by content?" without guessing.
pub(crate) fn container_key(vm: &mut Vm, v: &Value) -> Option<String> {
    let ck = content_key(vm, v);
    if ck == crate::stream::ekey_mut(vm, v) {
        None
    } else {
        Some(ck)
    }
}

fn content_key_at(vm: &mut Vm, v: &Value, depth: u32) -> String {
    let Value::Ref(h) = v else {
        return crate::stream::ekey_mut(vm, v);
    };
    if matches!(synth_field(vm, *h, &STR_ID, "$str"), Value::Str(_)) {
        return crate::stream::ekey_mut(vm, v);
    }
    // AT THE BOUND, KEY BY IDENTITY — the same answer `ref_hash` gives there.
    // Falling through to `ekey_mut` instead still rendered the nesting
    // structurally, so two structures that compared EQUAL past depth 8 hashed
    // differently: equal-but-unequal-hash is the one contract a collection may
    // never break.
    if depth >= HASH_DEPTH {
        return format!("@{h}");
    }
    if vm.heap.class_of(*h).is_some_and(is_entry_class) {
        let k = synth_field(vm, *h, &KEY_ID, "$key");
        let val = synth_field(vm, *h, &VAL_ID, "$val");
        return format!(
            "e:{}={}",
            content_key_at(vm, &k, depth + 1),
            content_key_at(vm, &val, depth + 1)
        );
    }
    if matches!(synth_field(vm, *h, &MAP_ID, "$map"), Value::Int(_)) {
        // The order-independent `m:{k=v,…}` render the maps themselves key by.
        return structural_key(vm, v);
    }
    if let Value::Array(arr) = synth_field(vm, *h, &ELEMS_ID, "$elems") {
        let elems = vm.heap.array_values(arr).unwrap_or_default();
        let set = vm.heap.class_of(*h).is_some_and(|c| c.contains("Set"))
            || matches!(synth_field(vm, *h, &MAPVIEW_ID, "$mapview"), Value::Str(_));
        let mut parts: Vec<String> =
            elems.iter().map(|e| content_key_at(vm, e, depth + 1)).collect();
        if set {
            parts.sort();
        }
        // `cs:` / `cl:`, NOT `s:` / `l:`. `key_str` already spends `s:` on a STRING,
        // so a set used to render `s:[a,b]` and the four-character string "[a,b]"
        // rendered `s:[a,b]` too — a collection compared EQUAL to a string of its
        // own rendering. The tags a content key may use are exactly those `key_str`
        // does not.
        return format!("{}:[{}]", if set { "cs" } else { "cl" }, parts.join(","));
    }
    crate::stream::ekey_mut(vm, v)
}

// --- map value-equality -----------------------------------------------------

/// Is this value a MAP? `Map.equals` is false against every non-map, so the test
/// must be positive rather than "drained to nothing".
pub(crate) fn is_map_value(vm: &mut Vm, v: &Value) -> bool {
    match v {
        Value::Ref(h) => {
            if matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_)) {
                return true;
            }
            match vm.heap.class_of(*h).map(str::to_string) {
                Some(c) => MAP_CLASSES.contains(&c.as_str())
                    || is_fastutil_objmap(&c)
                    || vm.implements_iface(&c, "java/util/Map"),
                None => false,
            }
        }
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            (crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p)) || {
                // A jar map whose state lives in its own bytecode fields (gson's
                // `LinkedTreeMap`, fastutil's ArrayMaps) IS a map; `drain_map_any` reads it.
                let cls = vm.value_class_name(v);
                !cls.is_empty() && vm.is_map_class(&cls)
            }
        }
        _ => false,
    }
}

/// `AbstractMap.equals` — same size and, for every key, an equal value on the
/// other side. Keys and values are compared through `structural_key`, the SAME
/// derivation the maps themselves key by, so two maps that would store a key
/// under the same slot compare its values.
pub(crate) fn map_entries_equal(
    vm: &mut Vm,
    a: &[(Value, Value)],
    b: &[(Value, Value)],
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut other: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(b.len());
    for (k, v) in b {
        let ks = structural_key(vm, k);
        let vs = structural_key(vm, v);
        other.insert(ks, vs);
    }
    // Distinct entries that collapse to one structural key mean the two sides
    // are not comparable entry-for-entry; report unequal rather than guess.
    if other.len() != a.len() {
        return false;
    }
    for (k, v) in a {
        let ks = structural_key(vm, k);
        let Some(want) = other.get(&ks).cloned() else {
            return false;
        };
        if want != structural_key(vm, v) {
            return false;
        }
    }
    true
}

// --- map views --------------------------------------------------------------

/// Which face of a map a view collection presents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ViewKind {
    Key,
    Value,
    Entry,
}

/// WHICH MAP a live view writes through to. A guest map lives in one of two
/// representations and a view has to be able to name either: a handle map (`$map`-backed,
/// the default) or an arena map (`RAVA_ARENA_MAPS`). The arena arm existed nowhere, which is
/// why `arena_map/dispatch.rs` handed back plain COPIES and every view-mediated removal
/// landed in the copy — `entrySet().iterator().remove()` reported 20 removals against a map
/// that still held 20 (`bench/AmRem.java`), and `RegistrySetBuilder` reported the survivors
/// as "Errors during registry creation".
#[derive(Clone, Copy)]
pub(crate) enum MapOwner {
    Ref(u32),
    #[cfg(feature = "next-gen-gc")]
    Arena(crate::arena::ArenaPtr),
    /// ⭐**A THIRD REPRESENTATION: A JAR MAP, WHOSE STATE IS ITS OWN BYTECODE FIELDS.**
    /// fastutil's `Reference2ObjectArrayMap` keeps `key[]` / `value[]` / `size` in fields the
    /// jar's own code owns — it is neither `$map`-backed nor a rava arena map — so both arms
    /// above are blind to it and `map_view_of` answered `None` for its views. `map/dispatch.rs`
    /// then handed back SNAPSHOTS, and every view-mediated mutation landed in the snapshot:
    /// `keySet().removeIf`, `keySet().remove`, `keySet().retainAll` and `values().remove` all
    /// reported success against a map that still held both entries (bench/CompSem.java), and
    /// with them `DataComponentPatch.forget(pred)` — whose whole body is
    /// `new Reference2ObjectArrayMap<>(map).keySet().removeIf(pred)` (javap) — FORGOT NOTHING.
    ///
    /// Neither arm above can write these fields, and nothing in this engine should: they are the
    /// jar's layout. So this arm writes through the map's OWN `remove`/`clear` bytecode, which
    /// is the only code that knows that layout — `Vm::call_by_name`, the same door `drain_map`
    /// reads them through.
    #[cfg(feature = "next-gen-gc")]
    Jar(crate::arena::ArenaPtr),
}

/// The `($mapowner, $mapview)` tag `keySet`/`values`/`entrySet` stamp on the
/// collection they hand back, or `None` for a collection that is not a view.
pub(crate) fn map_view_of(vm: &mut Vm, coll: u32) -> Option<(MapOwner, ViewKind)> {
    static OWNER_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let Value::Str(view) = synth_field(vm, coll, &MAPVIEW_ID, "$mapview") else {
        return None;
    };
    let owner = match synth_field(vm, coll, &OWNER_ID, "$mapowner") {
        Value::Ref(h) => MapOwner::Ref(h),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::arena_map::is_arena_map(p) => MapOwner::Arena(p),
        // Any OTHER arena object stamped as an owner is a jar map (see `MapOwner::Jar`). Only
        // `map/dispatch.rs`'s `Reference2ObjectArrayMap` arm stamps one, and only while
        // `r2omap_view_writethrough()` is on, so with the kill switch set nothing reaches here
        // and this arm is unreachable — which is what makes the switch exact.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => MapOwner::Jar(p),
        _ => return None,
    };
    let kind = match view.as_ref() {
        "key" => ViewKind::Key,
        "value" => ViewKind::Value,
        "entry" => ViewKind::Entry,
        _ => return None,
    };
    Some((owner, kind))
}

/// The BACKING-map key a view element denotes: itself for a key view, the
/// entry's `$key` for an entry view, and for a values view the key of the FIRST
/// entry holding that value (`AbstractCollection.remove` removes one mapping).
fn view_key(vm: &mut Vm, owner: MapOwner, kind: ViewKind, target: &Value) -> Option<Value> {
    match kind {
        ViewKind::Key => Some(target.clone()),
        ViewKind::Entry => match target {
            Value::Ref(eh) => Some(vm.heap.get_field_syn(*eh, crate::heap::Syn::Key)),
            _ => Some(target.clone()),
        },
        ViewKind::Value => {
            // The entries of whichever representation backs this view.
            let entries: Vec<(Value, Value)> = match owner {
                MapOwner::Ref(h) => {
                    let mid = map_id(vm, h);
                    vm.heap.map_entries(mid)
                }
                #[cfg(feature = "next-gen-gc")]
                MapOwner::Arena(p) => crate::arena_map::entries(p),
                // Read through the jar's own `entrySet()` (`drain_map_any`), the same door the
                // view was BUILT from — so the element that was chosen in `coll_op` and the
                // mapping removed here are derived from one set of entries. A failure to drain
                // is `None`, i.e. "no mapping denoted", never a wrong mapping.
                #[cfg(feature = "next-gen-gc")]
                MapOwner::Jar(p) => match vm.drain_map_any(&Value::Obj(p)) {
                    Ok(Some(es)) => es,
                    _ => return None,
                },
            };
            // `ekey_mut`, the SAME derivation that chose this element in `coll_op`.
            // `structural_key` keys an identity class carrying a `$name` by that
            // name where `ekey_mut` keys it by handle, so the two could pick
            // DIFFERENT mappings — the view would drop element X and the map would
            // lose the entry holding an equal-looking Y.
            let want = crate::stream::ekey_mut(vm, target);
            let mut found = None;
            for (k, v) in entries {
                if crate::stream::ekey_mut(vm, &v) == want {
                    found = Some(k);
                    break;
                }
            }
            found
        }
    }
}

/// Delete from the backing map the mapping this view element denotes.
/// Returns whether a mapping went away.
pub(crate) fn map_view_remove(
    vm: &mut Vm,
    owner: MapOwner,
    kind: ViewKind,
    target: &Value,
) -> Result<bool, VmError> {
    let Some(key) = view_key(vm, owner, kind, target) else {
        return Ok(false);
    };
    match owner {
        MapOwner::Ref(h) => {
            let mid = map_id(vm, h);
            // Derive against the BACKING map so a salted map's entry is removed under
            // its salted key (GAP-P04); guest throws propagate.
            let ks = value_map_key_in(vm, mid, &key)?;
            let gone = vm.heap.map_remove(mid, &ks).is_some();
            // The map changed, so its memoised structural key is stale (see structural_key).
            vm.heap.put_field_syn(h, crate::heap::Syn::SKey, Value::Null);
            Ok(gone)
        }
        // The arena map derives its own key inside `ops::remove` — the same derivation its
        // `put` used — so the salted/structural choice stays in one place per representation
        // rather than being made twice here.
        #[cfg(feature = "next-gen-gc")]
        MapOwner::Arena(mut p) => Ok(!matches!(
            crate::arena_map::ops::remove(vm, &mut p, &key),
            Value::Null
        )),
        // THE JAR'S OWN `remove`. `containsKey` is asked FIRST because the return value of
        // `remove` cannot answer "was there a mapping": fastutil's `Reference2ObjectArrayMap`
        // returns its `defRetValue` (null) for an absent key AND for a key mapped to null, and
        // MC maps `DataComponentType` to `Optional` — a present mapping whose value is
        // `Optional.empty()` is a real object, but nothing here may rely on that. Two calls into
        // guest bytecode per removed element, on a path that is a mutation and already runs the
        // guest's own iterator.
        #[cfg(feature = "next-gen-gc")]
        MapOwner::Jar(p) => {
            let recv = Value::Obj(p);
            let had = vm
                .call_by_name(recv.clone(), "containsKey", "(Ljava/lang/Object;)Z", vec![key.clone()])?
                .as_int()
                .unwrap_or(0)
                != 0;
            if !had {
                return Ok(false);
            }
            vm.call_by_name(recv, "remove", "(Ljava/lang/Object;)Ljava/lang/Object;", vec![key])?;
            Ok(true)
        }
    }
}

/// `keySet().clear()` / `values().clear()` / `entrySet().clear()` empty the map.
pub(crate) fn map_view_clear(vm: &mut Vm, owner: MapOwner) {
    match owner {
        MapOwner::Ref(h) => {
            let mid = map_id(vm, h);
            vm.heap.map_clear(mid);
            vm.heap.put_field_syn(h, crate::heap::Syn::SKey, Value::Null);
        }
        #[cfg(feature = "next-gen-gc")]
        MapOwner::Arena(p) => crate::arena_map::clear(p),
        // The jar's own `clear()`. This function cannot report an error (its callers have no
        // channel), and a throw out of `clear` is not a case this engine can produce for the one
        // owner that reaches here — a fastutil `ArrayMap.clear` nulls its arrays and zeroes
        // `size`. Dropped rather than swallowed silently: a failure leaves the map unchanged,
        // which is the same state a `None` from `map_view_of` used to leave it in.
        #[cfg(feature = "next-gen-gc")]
        MapOwner::Jar(p) => {
            let _ = vm.call_by_name(Value::Obj(p), "clear", "()V", vec![]);
        }
    }
}
