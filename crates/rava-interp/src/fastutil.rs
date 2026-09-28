//! Fast-path for fastutil primitive int sets (a generic third-party library).
//!
//! fastutil's `IntRBTreeSet`/`IntAVLTreeSet`/`IntOpenHashSet`/… are
//! red-black-tree / open-addressing structures whose bytecode our interpreter
//! executes faithfully but slowly, and whose `addAll(Collection)` path depends
//! on `instanceof IntCollection` resolving exactly (it doesn't, at our JDK
//! boundary), so building one from a boxed `List<Integer>` could come out empty
//! (Minecraft: `new PerlinSimplexNoise(rng, ImmutableList.of(0))` → "Need some
//! octaves!"). Model these as a sorted-unique `$elems` int array instead.
//! Generic library behaviour — no jar specifics.

use crate::value::Value;
use crate::{Vm, VmError};

/// The ONE table `is_int_set` answers from — and the owner_bits `FACT_NATIVE_REPR`
/// word and its drift test read, so the fact cannot drift from the predicate.
pub(crate) const INT_SETS: &[&str] = &[
    "it/unimi/dsi/fastutil/ints/IntRBTreeSet",
    "it/unimi/dsi/fastutil/ints/IntAVLTreeSet",
    "it/unimi/dsi/fastutil/ints/IntOpenHashSet",
    "it/unimi/dsi/fastutil/ints/IntLinkedOpenHashSet",
    "it/unimi/dsi/fastutil/ints/IntArraySet",
];

pub(crate) fn is_int_set(cls: &str) -> bool {
    INT_SETS.contains(&cls)
}

fn ints(vm: &Vm, recv: u32) -> Vec<i32> {
    match vm.heap.get_field_syn(recv, crate::heap::Syn::Elems) {
        // Spare capacity is not elements — see `Heap::elems_values`.
        Value::Array(a) => vm.heap.elems_values(recv, a)
            .iter().filter_map(Value::as_int).collect(),
        _ => Vec::new(),
    }
}

fn store(vm: &mut Vm, recv: u32, mut v: Vec<i32>) {
    v.sort_unstable();
    v.dedup();
    let vals: Vec<Value> = v.into_iter().map(Value::Int).collect();
    let a = vm.heap.alloc_values("int", vals);
    vm.heap.put_field_syn(recv, crate::heap::Syn::Elems, Value::Array(a));
    // Same reason as `stream::store_elems`: this REPLACES the backing array, so a
    // `Collections.unmodifiable*` view over this set must follow it.
    crate::immutable::republish_backing(vm, recv, Value::Array(a));
}

/// The backing `$elems` int array handle and its length, if present. `$elems` is
/// maintained sorted-unique by every mutation arm (`store` sorts+dedups; the
/// incremental add/remove keep order), so it doubles as a binary-search index.
fn arr_of(vm: &Vm, recv: u32) -> Option<(u32, usize)> {
    match vm.heap.get_field_syn(recv, crate::heap::Syn::Elems) {
        Value::Array(a) => Some((a, vm.heap.elems_len(recv, a))),
        _ => None,
    }
}

/// Binary-search the sorted `$elems` for `k`: `Ok(pos)` = present at `pos`,
/// `Err(pos)` = absent, insert at `pos` to keep the array sorted. O(log n) reads
/// instead of the O(n) drain + scan the old per-op `ints()` did.
fn int_bsearch(vm: &Vm, arr: u32, len: usize, k: i32) -> Result<usize, usize> {
    let (mut lo, mut hi) = (0usize, len);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let v = vm.heap.array_get(arr, mid).and_then(|x| x.as_int()).unwrap_or(0);
        match v.cmp(&k) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(mid),
        }
    }
    Err(lo)
}

/// Collect ints from a constructor/addAll argument: an int[], another int set
/// (via `$elems`), or any boxed `Collection<Integer>` (drained generically).
fn ints_from_arg(vm: &mut Vm, arg: Option<&Value>) -> Result<Vec<i32>, VmError> {
    match arg {
        Some(Value::Array(a)) => Ok(vm.heap.array_values(*a).unwrap_or_default()
            .iter().filter_map(Value::as_int).collect()),
        Some(Value::Ref(h)) => Ok(vm.iterate_object(*h)?.iter().filter_map(Value::as_int).collect()),
        _ => Ok(Vec::new()),
    }
}

/// Dispatch a method on a fastutil int set (receiver = args[0]). Returns
/// `Ok(Some(ret))` when handled.
pub(crate) fn dispatch(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Option<Option<Value>>, VmError> {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(None) };
    match name {
        "<init>" => {
            // IntXxxSet() | (int capacity) | (int[] | Collection | IntCollection)
            let init = match args.get(1) {
                Some(Value::Int(_)) | None => Vec::new(),
                other => ints_from_arg(vm, other)?,
            };
            store(vm, recv, init);
            Ok(Some(None))
        }
        // O(log n) dedup + in-place sorted insert — no per-add drain/sort/realloc
        // (the old path was O(n log n)/add → O(n^2 log n) to build an N-set).
        "add" => {
            let k = args.get(1).and_then(Value::as_int).unwrap_or(0);
            match arr_of(vm, recv) {
                Some((arr, len)) => match int_bsearch(vm, arr, len, k) {
                    Ok(_) => Ok(Some(Some(Value::Int(0)))),
                    Err(pos) => {
                        vm.heap.array_insert(arr, pos, Value::Int(k));
                        Ok(Some(Some(Value::Int(1))))
                    }
                },
                None => { store(vm, recv, vec![k]); Ok(Some(Some(Value::Int(1)))) }
            }
        }
        "addAll" => {
            let mut v = ints(vm, recv);
            let before = v.len();
            v.extend(ints_from_arg(vm, args.get(1))?);
            let n0 = { v.sort_unstable(); v.dedup(); v.len() };
            store(vm, recv, v);
            Ok(Some(Some(Value::Int((n0 != before) as i32))))
        }
        "rem" | "remove" => {
            let k = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let had = match arr_of(vm, recv) {
                Some((arr, len)) => match int_bsearch(vm, arr, len, k) {
                    Ok(pos) => { vm.heap.array_remove(arr, pos); true }
                    Err(_) => false,
                },
                None => false,
            };
            Ok(Some(Some(Value::Int(had as i32))))
        }
        "contains" => {
            let k = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let present = matches!(arr_of(vm, recv), Some((arr, len)) if int_bsearch(vm, arr, len, k).is_ok());
            Ok(Some(Some(Value::Int(present as i32))))
        }
        "isEmpty" => Ok(Some(Some(Value::Int((arr_of(vm, recv).map(|(_, n)| n).unwrap_or(0) == 0) as i32)))),
        "size" => Ok(Some(Some(Value::Int(arr_of(vm, recv).map(|(_, n)| n).unwrap_or(0) as i32)))),
        "clear" => { store(vm, recv, Vec::new()); Ok(Some(None)) }
        // Sorted-set endpoints ($elems is kept sorted) — read the ends directly.
        "firstInt" | "first" => {
            let f = arr_of(vm, recv).filter(|(_, n)| *n > 0)
                .and_then(|(arr, _)| vm.heap.array_get(arr, 0)).and_then(|x| x.as_int()).unwrap_or(0);
            Ok(Some(Some(Value::Int(f))))
        }
        "lastInt" | "last" => {
            let l = arr_of(vm, recv).filter(|(_, n)| *n > 0)
                .and_then(|(arr, n)| vm.heap.array_get(arr, n - 1)).and_then(|x| x.as_int()).unwrap_or(0);
            Ok(Some(Some(Value::Int(l))))
        }
        "toIntArray" => {
            let v: Vec<Value> = ints(vm, recv).into_iter().map(Value::Int).collect();
            Ok(Some(Some(Value::Array(vm.heap.alloc_values("int", v)))))
        }
        "iterator" | "intIterator" => {
            let v: Vec<Value> = ints(vm, recv).into_iter().map(Value::Int).collect();
            Ok(Some(Some(crate::stream::iterator_of(vm, v))))
        }
        _ => Ok(None),
    }
}
