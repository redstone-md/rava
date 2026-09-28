//! Map writes that stay safe across a collection, and the key-render census.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::*;

/// splitmix64 finalizer — bijective bit-mixer so an order-independent additive
/// combine of per-pair hashes has no structured collisions (e.g. {1,4} vs {2,3}).
#[inline]
pub(crate) fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn keyrender_stat(kind: u8) {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_KEYRENDER_STAT").is_some()) {
        return;
    }
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    // 0 = hit, 1 = miss (admitted, rendered and stored), 2 = REJECTED by the class rule.
    // The first version counted only 0 and 1, so rejected objects - the ones for which the memo is
    // pure overhead - were invisible, and the reported "99.7% hit rate" was a rate over admitted
    // objects only. A rate whose denominator excludes the cases you are worried about says nothing
    // about them.
    pub(crate) static C: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
    C[kind as usize].fetch_add(1, Relaxed);
    let (h, mi, rj) = (C[0].load(Relaxed), C[1].load(Relaxed), C[2].load(Relaxed));
    let tot = h + mi + rj;
    if tot % 250_000 == 0 {
        eprintln!(
            "[KEYRENDER] hit={h} miss={mi} rejected={rj} | hit of ALL renders {:.1}%",
            h as f64 * 100.0 / tot as f64
        );
    }
}

/// GC-safe `map_put`: `value_map_key` → `value_to_string` runs guest `toString()` which
/// allocates and can trigger a moving collection that evacuates the arena key/value. The
/// naive `let ks = value_map_key(&k); map_put(id, ks, k, v)` then stores the STALE pre-move
/// `k`/`v` (the caller's Rust-local clones are NOT forwarded) — the worldgen DensityFunction
/// map-key WRITEBACK MISS. FIX: root k+v in gc_protect (forwarded in place), compute the key
/// with them rooted, then re-read the forwarded copies before storing.
pub(crate) fn map_put_gc_safe(vm: &mut Vm, id: u32, k: Value, v: Value) -> Result<(), VmError> {
    #[cfg(feature = "next-gen-gc")]
    {
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, k);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, v);
        let kc = crate::vm::thread_context::root_index(&vm.gc_protect, base);
        // GAP-P04: this put targets map `id` directly — salt against it so the map is
        // never mixed-mode (guest throws propagate after the roots are released).
        let ks = value_map_key_in(vm, id, &kc);
        let k2 = crate::vm::thread_context::root_index(&vm.gc_protect, base);
        let v2 = crate::vm::thread_context::root_index(&vm.gc_protect, base + 1);
        crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, base);
        vm.heap.map_put(id, ks?, k2, v2);
        Ok(())
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let ks = value_map_key_in(vm, id, &k)?;
        vm.heap.map_put(id, ks, k, v);
        Ok(())
    }
}

/// GC-safe `map_put` for the general dispatch path that keys via `map_key(map_obj, k)` (which
/// also runs value_to_string → guest toString → can collect). Roots k+v, computes the key,
/// re-reads the forwarded copies, then stores. Returns the map_put result (prior value).
pub(crate) fn map_put_gc_safe_mk(vm: &mut Vm, id: u32, map_obj: u32, k: Value, v: Value) -> Result<Option<Value>, VmError> {
    #[cfg(feature = "next-gen-gc")]
    {
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, k);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, v);
        let kc = crate::vm::thread_context::root_index(&vm.gc_protect, base);
        let ks = map_key(vm, map_obj, &kc);
        let k2 = crate::vm::thread_context::root_index(&vm.gc_protect, base);
        let v2 = crate::vm::thread_context::root_index(&vm.gc_protect, base + 1);
        crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, base);
        Ok(vm.heap.map_put(id, ks?, k2, v2))
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let ks = map_key(vm, map_obj, &k)?;
        Ok(vm.heap.map_put(id, ks, k, v))
    }
}
