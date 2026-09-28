//! WRITE-THROUGH LIST VIEWS — `List.subList(from, to)`.
//!
//! `subList` is specified as a VIEW: `base.subList(1, 4).set(0, 99)` stores into `base`,
//! `sub.clear()` removes that range FROM `base`, and reads through the view see whatever
//! the backing list holds now. This engine returned a COPY, which answers every read
//! correctly and loses only the write-back — so nothing caught it until a differential
//! probe looked through the ORIGINAL (bench/CollSem.java, `sublist.write-through` and
//! `sublist.clear-through`). That is the dangerous shape of bug: a silently wrong answer
//! rather than an error.
//!
//! ## Why the view still carries a `$elems` snapshot
//!
//! The obvious design gives the view NO storage of its own and resolves every access
//! through the parent. It is not available here: **69 sites across ~25 modules read
//! `$elems` directly** — `fu_fastpath`, `pipeline`, `reflect::functional`, `fastutil`,
//! `apply` — and a view without `$elems` would read as EMPTY in all of them. Trading three
//! wrong lines for sixty-nine is not a fix.
//!
//! So a view keeps a snapshot in `$elems` AND the link to its source, and the two choke
//! points every collection access already passes through keep them agreed:
//!
//! * `Vm::iterate_object` — re-slices from the parent and refreshes the snapshot, so a
//!   read through the view sees the parent's current contents.
//! * `stream::store_elems` — splices the view's new contents back into the parent's range,
//!   so a write through the view lands in the parent.
//!
//! ⚠**The residual:** a direct `$elems` reader that runs after the parent was mutated and
//! that never touches either choke point sees the stale snapshot. That is strictly better
//! than the copy it replaces (which was stale from birth, in both directions), and it is
//! the honest limit of a fix that cannot rewrite 69 call sites.
//!
//! ## What is deliberately not modelled
//!
//! The JDK's `SubList` carries a `modCount` and throws `ConcurrentModificationException`
//! when the backing list is structurally modified through anything but the view. There is
//! no modCount here, so such a view keeps working against the resliced parent instead of
//! throwing. Failing to throw is the permissive direction, and the alternative — inventing
//! a throw the guest did not earn — is how a wrong `PatternSyntaxException` once killed
//! the server before its first log line (bench/PlatSem.java).

use crate::*;
use crate::stream::OBJ;

/// The backing list a view reads and writes through.
const SRC: &str = "$view_of";
/// Its range in that list, as `[from, to)`.
const FROM: &str = "$view_from";
const TO: &str = "$view_to";

/// Handle-indexed filter, the same design `immutable::mark_bit_maybe` uses and for the same
/// reason: `is_view` is called from `Vm::iterate_object`, which every drain of every
/// collection passes through. A by-NAME field probe there is not free — reading
/// `$immutable` by name on a guarded path once cost **wall 57.7 → 68.1 s** — and even with
/// the L1 now caching misses, an atomic load and a bit test is the honest cost for a
/// question that answers "no" for essentially every object in the process.
const VIEW_WORDS: usize = 1 << 14;
const VIEW_CAP: u32 = (VIEW_WORDS * 64) as u32;
static VIEW_BITS: [std::sync::atomic::AtomicU64; VIEW_WORDS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; VIEW_WORDS];

fn view_bit_set(h: u32) {
    if h < VIEW_CAP {
        VIEW_BITS[(h / 64) as usize]
            .fetch_or(1u64 << (h % 64), std::sync::atomic::Ordering::Release);
    }
}

/// `false` only when the handle is PROVABLY not a view.
#[inline]
fn view_bit_maybe(h: u32) -> bool {
    if h >= VIEW_CAP {
        return true;
    }
    VIEW_BITS[(h / 64) as usize].load(std::sync::atomic::Ordering::Acquire) & (1u64 << (h % 64)) != 0
}

/// The map a `keySet()` / `values()` / `entrySet()` view is a view OF, and which kind.
const MAP_KIND: &str = "$mapview";
const MAP_OWNER: &str = "$mapowner";

/// Is this object a view over another list?
///
/// The bit is a FILTER, not the answer: a handle freed and reused keeps its bit, so a
/// "maybe" is always confirmed against the field. False positives cost one probe; false
/// negatives cannot happen, because the bit is set before the field is.
pub(crate) fn is_view(vm: &Vm, obj: u32) -> bool {
    if !view_bit_maybe(obj) {
        return false;
    }
    matches!(vm.heap.get_field(obj, SRC), Value::Ref(_))
        || matches!(vm.heap.get_field(obj, MAP_OWNER), Value::Ref(_))
}

/// Mark a map view so `is_view` can find it without a field probe, and so `coll_op`'s O(1)
/// arms — which read the view's own snapshot — are bypassed in favour of the general path
/// that goes through `iterate_object` and therefore re-derives.
pub(crate) fn note_map_view(obj: u32) {
    view_bit_set(obj);
}

/// Build `parent.subList(from, to)` as a live view, or `None` when the parent cannot back
/// one.
///
/// The parent must own an `$elems` array: this engine also holds lists whose state lives in
/// real bytecode fields, and splicing a range into one of those through `store_elems` would
/// write somewhere the guest's own code does not read. Those keep the copy, which is what
/// they got before — narrow blast radius over a broad but unproven fix.
pub(crate) fn make_sublist(vm: &mut Vm, parent: u32, from: usize, to: usize, slice: Vec<Value>) -> Option<Value> {
    if !matches!(vm.heap.get_field_syn(parent, crate::heap::Syn::Elems), Value::Array(_)) {
        return None;
    }
    let arr = vm.heap.alloc_values(OBJ, slice);
    let h = vm.heap.alloc("java/util/ArrayList");
    view_bit_set(h);
    vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
    vm.heap.put_field(h, SRC, Value::Ref(parent));
    vm.heap.put_field(h, FROM, Value::Int(from as i32));
    vm.heap.put_field(h, TO, Value::Int(to as i32));
    Some(Value::Ref(h))
}

fn range(vm: &Vm, obj: u32) -> (u32, usize, usize) {
    let Value::Ref(parent) = vm.heap.get_field(obj, SRC) else {
        return (u32::MAX, 0, 0);
    };
    let from = vm.heap.get_field(obj, FROM).as_int().unwrap_or(0).max(0) as usize;
    let to = vm.heap.get_field(obj, TO).as_int().unwrap_or(0).max(0) as usize;
    (parent, from, to)
}

/// Every element of `obj`, resolved through the chain of views above it.
///
/// Recursive, so a `subList` of a `subList` resolves to the root list. Non-fallible on
/// purpose: `store_elems` has no way to report an error and must not be able to leave a
/// write half-applied.
fn resolve(vm: &mut Vm, obj: u32) -> Vec<Value> {
    if is_view(vm, obj) {
        let (parent, from, to) = range(vm, obj);
        let all = resolve(vm, parent);
        let from = from.min(all.len());
        let to = to.clamp(from, all.len());
        return all[from..to].to_vec();
    }
    match vm.heap.get_field_syn(obj, crate::heap::Syn::Elems) {
        // `elems_values`, not `array_values`: an overriding class keeps spare capacity in
        // `$elems` and the trailing slots are not elements. Identical without an override.
        Value::Array(a) => vm.heap.elems_values(obj, a),
        _ => Vec::new(),
    }
}

/// SIZE WITHOUT MATERIALISING. A map view answers `size` and `isEmpty` from the backing
/// map's own count, which is what makes the JDK's views O(1) — and what keeps this engine's
/// newly-live views from re-deriving the whole map to answer "how many".
///
/// Returns `None` for anything else, including a list view, so every other question keeps
/// its existing path.
pub(crate) fn map_view_size(vm: &Vm, obj: u32) -> Option<usize> {
    if !view_bit_maybe(obj) {
        return None;
    }
    let Value::Ref(owner) = vm.heap.get_field(obj, MAP_OWNER) else { return None };
    let Value::Int(id) = vm.heap.get_field_syn(owner, crate::heap::Syn::Map) else { return None };
    Some(vm.heap.map_size(id as u32))
}

/// Read through a view: the parent's current contents for this range, with the view's own
/// `$elems` snapshot refreshed to match.
///
/// Returns `None` for a plain list so the caller keeps its existing path untouched.
pub(crate) fn read(vm: &mut Vm, obj: u32) -> Option<Vec<Value>> {
    if !is_view(vm, obj) {
        return None;
    }
    // A MAP view re-derives from the backing map, which is what makes a `put` after the
    // view was taken visible through it.
    if let Value::Ref(owner) = vm.heap.get_field(obj, MAP_OWNER) {
        let kind = match vm.heap.get_field(obj, MAP_KIND) {
            Value::Str(s) => s.to_string(),
            _ => return None,
        };
        let cur = crate::map::map_view_elements(vm, owner, &kind)?;
        let arr = vm.heap.alloc_values(OBJ, cur.clone());
        vm.heap.put_field_syn(obj, crate::heap::Syn::Elems, Value::Array(arr));
        return Some(cur);
    }
    let cur = resolve(vm, obj);
    let arr = vm.heap.alloc_values(OBJ, cur.clone());
    vm.heap.put_field_syn(obj, crate::heap::Syn::Elems, Value::Array(arr));
    vm.heap.put_field(obj, TO, Value::Int((range(vm, obj).1 + cur.len()) as i32));
    Some(cur)
}

/// Write through a view: splice `elems` into the parent's range and shrink or grow the
/// range to match.
///
/// Returns `false` for a plain list, so `store_elems` falls through to its normal store.
pub(crate) fn write(vm: &mut Vm, obj: u32, elems: &[Value]) -> bool {
    if !is_view(vm, obj) {
        return false;
    }
    let (parent, from, to) = range(vm, obj);
    let mut all = resolve(vm, parent);
    let from = from.min(all.len());
    let to = to.clamp(from, all.len());
    all.splice(from..to, elems.iter().cloned());
    vm.heap.put_field(obj, TO, Value::Int((from + elems.len()) as i32));
    // The parent may itself be a view — a `subList` of a `subList` writes through to the
    // root — so this recurses rather than storing directly.
    if !write(vm, parent, &all) {
        let arr = vm.heap.alloc_values(OBJ, all);
        vm.heap.put_field_syn(parent, crate::heap::Syn::Elems, Value::Array(arr));
        // Same reason `store_elems` does it: a `Collections.unmodifiable*` view SHARES the
        // backing array, and replacing the array would strand that view on the old one.
        crate::immutable::republish_backing(vm, parent, Value::Array(arr));
    }
    true
}
