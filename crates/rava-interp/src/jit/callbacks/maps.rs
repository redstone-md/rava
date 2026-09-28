//! Map bridges: the guest HashMap surface and its arena-backed twin.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


/// COLLECTION INTRINSIC (next-gen-gc): `HashMap.put` direct from compiled code,
/// bypassing the `rava_invoke` synthetic-frame dispatch. `recv` is the receiver
/// operand cell (a REF handle); key/val are operand cells.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_map_put(recv: i64, key: i64, val: i64) -> i64 {
    cb(0);
    s09census::note(s09census::IDX_MAP_MUT);
    // GAP-S09 stage 3: put mutates the shared native `$map` table (not COW-
    // shadowed) — worker-illegal. Abort BEFORE any decode/publication; locked-exec
    // runs the real put under the resolve lock (exclusive &mut Vm).
    let _locked_exec = match worker_illegal_gate(s09census::IDX_MAP_MUT, "map_put") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    let Some(vm) = vm_ref() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_ref(recv) else { return 0 };
    vm.jit_map_put(h, key, val)
}

/// COLLECTION INTRINSIC (next-gen-gc): `HashMap.get` direct from compiled code,
/// bypassing the `rava_invoke` synthetic-frame dispatch — the partner of
/// `rava_map_put`. `recv` is the receiver operand cell (a REF handle to a native
/// `$map` map); `key` is an operand cell. Returns the boxed value cell (null = 0).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_map_get(recv: i64, key: i64) -> i64 {
    cb(0);
    // jit_map_get checks the backing's access policy: access-order reads use
    // the mutation gate; ordinary reads leave the shared table untouched.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_ref(recv) else { return 0 };
    vm.jit_map_get(h, key)
}

/// COLLECTION INTRINSIC (next-gen-gc): `HashMap.remove` direct (mutating partner).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_map_remove(recv: i64, key: i64) -> i64 {
    cb(0);
    s09census::note(s09census::IDX_MAP_MUT);
    // GAP-S09 stage 3: mutator (see rava_map_put); the get/contains partners are
    // reads and stay un-gated.
    let _locked_exec = match worker_illegal_gate(s09census::IDX_MAP_MUT, "map_remove") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    let Some(vm) = vm_ref() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_ref(recv) else { return 0 };
    vm.jit_map_remove(h, key)
}

/// COLLECTION INTRINSIC (next-gen-gc): `HashMap.containsKey` direct → 1/0.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_map_contains(recv: i64, key: i64) -> i64 {
    cb(0);
    // GAP-S09 stage 4 SANCTIONED: read-only `$map` probe — see rava_map_get.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let Some(h) = untag_ref(recv) else { return 0 };
    vm.jit_map_contains(h, key)
}

// ─────────── Step 3: arena-map intrinsics (RAVA_ARENA_MAPS java/util/HashMap) ───────────
// Under the flag a HashMap is an arena ArenaMap (a raw `Value::Obj` pointer, not a
// `$map` handle), so the receiver operand is its low arena address. These helpers
// drive the flat-table probe in arena_map.rs straight from compiled code, replacing
// the heavy interpreter invokevirtual+dispatch the flag otherwise pays. They also
// dispatch a non-arena receiver (a LinkedHashMap subclass is still `$map`) to the
// legacy path, so the intrinsic is sound for any HashMap-typed receiver. put/remove
// are GC-safe via arena_map's protected-temps handle scope (live_put/remove).

#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_amap_get(recv: i64, key: i64) -> i64 {
    cb(0);
    // The arena probe is read-only. The handle fallback checks access order
    // in jit_map_get and gates a reorder before any shared publication.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    if let Some(mut p) = amap_recv(recv) {
        let k = unbox_ref(vm, key, "");
        let v = crate::arena_map::get(vm, &mut p, &k);
        return box_ref(vm, &v);
    }
    untag_ref(recv).map_or(0, |h| vm.jit_map_get(h, key))
}

/// COLD-PATH for the int-probe's get-MISS: the value get() returns for an absent key —
/// `Value::Null` for a java.util map, the fastutil `defaultReturnValue` otherwise.
/// Boxed for the JIT. A miss is the rare path, so a callback here keeps the hot HIT
/// path zero-FFI while honouring a configured default (MC datafixers set one).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_amap_default(recv: i64) -> i64 {
    // GAP-S09 stage 4 SANCTIONED: read-only default-return decode + TLS boxing.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    match amap_recv(recv) {
        Some(p) => {
            let v = crate::arena_map::default_return(p);
            box_ref(vm, &v)
        }
        None => 0,
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_amap_contains(recv: i64, key: i64) -> i64 {
    cb(0);
    // GAP-S09 stage 4 SANCTIONED: read-only probe — see rava_amap_get.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    if let Some(mut p) = amap_recv(recv) {
        let k = unbox_ref(vm, key, "");
        return crate::arena_map::contains(vm, &mut p, &k) as i64;
    }
    untag_ref(recv).map_or(0, |h| vm.jit_map_contains(h, key))
}

#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_amap_put(recv: i64, key: i64, val: i64) -> i64 {
    cb(0);
    s09census::note(s09census::IDX_MAP_MUT);
    // GAP-S09 stage 3: mutator on either arm (arena flat-table live_put OR the
    // legacy `$map` fallback) — worker-illegal (see rava_map_put).
    let _locked_exec = match worker_illegal_gate(s09census::IDX_MAP_MUT, "amap_put") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    let Some(vm) = vm_ref() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    if let Some(mut p) = amap_recv(recv) {
        let k = unbox_ref(vm, key, "");
        let v = unbox_ref(vm, val, "");
        let prev = crate::arena_map::live_put(vm, &mut p, k, v);
        return box_ref(vm, &prev);
    }
    untag_ref(recv).map_or(0, |h| vm.jit_map_put(h, key, val))
}

#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_amap_remove(recv: i64, key: i64) -> i64 {
    cb(0);
    s09census::note(s09census::IDX_MAP_MUT);
    // GAP-S09 stage 3: mutator on either arm (see rava_amap_put).
    let _locked_exec = match worker_illegal_gate(s09census::IDX_MAP_MUT, "amap_remove") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    let Some(vm) = vm_ref() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    if let Some(mut p) = amap_recv(recv) {
        let k = unbox_ref(vm, key, "");
        let v = crate::arena_map::remove(vm, &mut p, &k);
        return box_ref(vm, &v);
    }
    untag_ref(recv).map_or(0, |h| vm.jit_map_remove(h, key))
}

/// Convert an ABOX-tagged table value cell to a JIT operand (the inline IR probe
/// calls this ONLY on a hit whose value is a boxed primitive/String). GC-free.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_amap_box_value(cell: i64) -> i64 {
    // GAP-S09 stage 4 SANCTIONED: pure value-cell decode + TLS boxing.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    crate::arena_map::jit_box_value_cell(vm, cell)
}
