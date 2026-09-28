//! Map methods and the inverse view over one.
//!
//! Re-exported from the parent module, so no call site outside it changed.


mod map_method;
pub(crate) use map_method::*;
use crate::map::*;

/// fastutil `*ArrayMap#get` served natively: identity-scan the packed key
/// array `[0..size)` and return the paired value — no guest bytecode, no PIC
/// dispatch. Mirrors `findKey` exactly: reference identity (`opcodes::ref_equal`,
/// the same `if_acmp*` semantics the guest uses) and a size-1 → 0 walk, so the
/// FIRST stored occurrence wins. Some(value) on a hit, Some(Value::Null) on a
/// miss (fastutil returns null either way), None when `p` is not an arena
/// ArrayMap of that layout — the caller then falls back to the guest path.
///
/// ⭐**THE LAYOUT IS A PROPERTY OF THE CLASS, SO IT IS DERIVED ONCE PER CLASS.** This function
/// used to re-derive it on every `get`: `class_name(cid)` (a thread-local mirror lookup plus an
/// `Arc` clone and drop), a `contains("ArrayMap")` substring scan, three `resolve_field_id` calls
/// that each hash a 50-character class name, and three `slot_of` lookups — all to recompute three
/// constants. `map::arraymap_ids` already memoises exactly that triple by class id and is what
/// `fu_fastpath`'s `Reference2ObjectArrayMap` arm reads; asking it here means the two natives that
/// scan the same three arrays can no longer disagree about which fields they are, which is the
/// failure this file's own `$elems`-vs-owner-qualified-id comments keep recording.
///
/// ⭐**AND THE SCAN NO LONGER COPIES THE MAP TO READ ONE ENTRY.** `array_values` clones the WHOLE
/// backing array into a fresh `Vec<Value>` — twice, keys and values — before comparing the first
/// element, so a four-entry map paid two allocations and eight `Value` clones per lookup. The walk
/// is `array_get` per probed index now: same order (`size-1 → 0`, fastutil's `findKey`), same
/// `ref_equal`, same stopping point, and it touches only the elements it actually compares.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fastutil_array_get(
    vm: &mut Vm,
    p: crate::arena::ArenaPtr,
    key: &Value,
) -> Option<Value> {
    let cid = crate::nextgen::arena_class_id(p);
    // Memoised per class id, and it carries the same three layout guards this function used to
    // repeat — an off-layout `arena_getfield` is a PANIC, not a wrong answer.
    let (kid, vid, sid) = arraymap_ids(vm, cid)?;
    let Value::Array(ka) = crate::opcodes::arena_getfield(p, kid) else {
        return None;
    };
    let Value::Array(va) = crate::opcodes::arena_getfield(p, vid) else {
        return None;
    };
    let n = match crate::opcodes::arena_getfield(p, sid) {
        Value::Int(s) => s.max(0) as usize,
        _ => 0,
    };
    // Spare capacity is not entries: the logical size may exceed neither backing.
    let n = n
        .min(vm.heap.array_len(ka)?)
        .min(vm.heap.array_len(va)?);
    for i in (0..n).rev() {
        let hit = vm
            .heap
            .array_get(ka, i)
            .is_some_and(|k| crate::opcodes::ref_equal(&k, key));
        if hit {
            return Some(vm.heap.array_get(va, i).unwrap_or(Value::Null));
        }
    }
    Some(Value::Null)
}

/// Operations on a BiMap.inverse() view: roles swapped against the LIVE forward
/// map. get/containsKey search by VALUE (identity for objects — registry values
/// are identity-equals and their toString may itself reverse-lookup, which must
/// not recurse); put writes the swapped pair into the forward map.
pub(crate) fn inverse_view_method(vm: &mut Vm, orig: u32, name: &str, args: &[Value]) -> Handled {
    let id = map_id(vm, orig);
    let val_matches = |vm: &mut Vm, a: &Value, b: &Value| -> bool {
        match (a, b) {
            (Value::Ref(x), Value::Ref(y)) => x == y,
            (x, y) => structural_key(vm, x) == structural_key(vm, y),
        }
    };
    let ret = match name {
        "inverse" => Value::Ref(orig),
        "size" => Value::Int(vm.heap.map_entries(id).len() as i32),
        "isEmpty" => Value::Int(vm.heap.map_entries(id).is_empty() as i32),
        "get" | "getProperty" => {
            let target = args.get(1).cloned().unwrap_or(Value::Null);
            let mut found = Value::Null;
            for (k, v) in vm.heap.map_entries(id) {
                if val_matches(vm, &v, &target) {
                    found = k;
                    break;
                }
            }
            found
        }
        "containsKey" => {
            let target = args.get(1).cloned().unwrap_or(Value::Null);
            let mut hit = false;
            for (_, v) in vm.heap.map_entries(id) {
                if val_matches(vm, &v, &target) {
                    hit = true;
                    break;
                }
            }
            Value::Int(hit as i32)
        }
        "containsValue" => {
            let target = args.get(1).cloned().unwrap_or(Value::Null);
            let mut hit = false;
            for (k, _) in vm.heap.map_entries(id) {
                if val_matches(vm, &k, &target) {
                    hit = true;
                    break;
                }
            }
            Value::Int(hit as i32)
        }
        // inverse.put(value, key) == forward.put(key, value).
        "put" | "forcePut" => {
            let v = args.get(1).cloned().unwrap_or(Value::Null);
            let k = args.get(2).cloned().unwrap_or(Value::Null);
            vm.heap.put_field_syn(orig, crate::heap::Syn::SKey, Value::Null);
            map_put_gc_safe_mk(vm, id, orig, k, v)?.unwrap_or(Value::Null)
        }
        // keySet of the inverse = the forward map's VALUES; values = its keys.
        "keySet" => {
            let vals: Vec<Value> = vm.heap.map_entries(id).into_iter().map(|(_, v)| v).collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", vals);
            let h = vm.heap.alloc("java/util/HashSet");
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Value::Ref(h)
        }
        "values" => {
            let keys: Vec<Value> = vm.heap.map_entries(id).into_iter().map(|(k, _)| k).collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", keys);
            let h = vm.heap.alloc("java/util/ArrayList");
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Value::Ref(h)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}
