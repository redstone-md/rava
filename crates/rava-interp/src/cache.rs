//! Generic Guava `Cache`/`LoadingCache` fast-path.
//!
//! Guava's `LocalCache` (size eviction, weak keys, reference entries, recency
//! queues) is a large, lock-and-queue-heavy machine whose internal invariants
//! assume a real JVM (e.g. `getNextEvictable` asserts a non-empty access queue
//! once `maximumSize` is exceeded). Rather than reproduce that machinery, we
//! model a Guava cache as exactly what callers observe: a memoising map keyed by
//! content, populated on miss by invoking the real `CacheLoader.load` bytecode.
//! This is a generic library fast-path (any Guava cache), not jar-specific.
//!
//! The cache object is a synthetic instance of `rava/GuavaLoadingCache` (loader
//! present) or `rava/GuavaCache` (manual). Its `$map` id addresses the heap map
//! table; `$cacheloader` holds the `CacheLoader`. We never evict — consistent
//! with the rest of the engine, which does not GC.

use crate::map::structural_key;
use crate::value::Value;
use crate::{Vm, VmError};

type Handled = Result<Option<Option<Value>>, VmError>;

pub(crate) const LOADING_CACHE_CLASS: &str = "rava/GuavaLoadingCache";
pub(crate) const CACHE_CLASS: &str = "rava/GuavaCache";

/// G4 — a fork's SHARED guava-cache memo table (keyed by cache-map id → structural key), used by
/// all workers during a parallel region instead of the heap `$map`. SHARED (not per-worker) so an
/// expensive, heavily-reused loader (SHAPE_FULL_BLOCK_CACHE) is computed ONCE across the whole
/// fork — per-worker tables recomputed it N× (a 15× blow-up that made the fork slower than serial).
/// Concurrent via a Mutex (brief critical sections; the loader runs OUTSIDE the lock). Holds region-
/// allocated Value handles for the region's lifetime (no GC runs in-region); `clear_fork_cache`
/// empties it at region end so it never dangles into the next region.
#[cfg(feature = "next-gen-gc")]
static FORK_CACHE: std::sync::Mutex<Option<std::collections::HashMap<u32, std::collections::HashMap<String, Value>>>> =
    std::sync::Mutex::new(None);

/// Empty the shared fork cache (called by the fork driver at region end, under the GIL).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn clear_fork_cache() {
    if let Ok(mut g) = FORK_CACHE.lock() {
        *g = None;
    }
}

/// `CacheBuilder.build(CacheLoader)` → a synthetic loading cache.
pub(crate) fn build_loading_cache(vm: &mut Vm, loader: Value) -> Value {
    let h = vm.heap.alloc(LOADING_CACHE_CLASS);
    let id = vm.heap.alloc_map();
    vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
    vm.heap.put_field(h, "$cacheloader", loader);
    Value::Ref(h)
}

/// `CacheBuilder.build()` → a synthetic manual cache (no loader).
pub(crate) fn build_cache(vm: &mut Vm) -> Value {
    let h = vm.heap.alloc(CACHE_CLASS);
    let id = vm.heap.alloc_map();
    vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
    Value::Ref(h)
}

/// True if `cls` is one of our synthetic cache classes.
pub(crate) fn is_cache_class(cls: &str) -> bool {
    cls == LOADING_CACHE_CLASS || cls == CACHE_CLASS
}

fn map_id(vm: &Vm, recv: u32) -> Option<u32> {
    match vm.heap.get_field_syn(recv, crate::heap::Syn::Map) {
        Value::Int(id) => Some(id as u32),
        _ => None,
    }
}

/// Call `CacheLoader.load(key)` (real bytecode) on the cache's loader.
fn load(vm: &mut Vm, recv: u32, key: &Value) -> Result<Value, VmError> {
    const SIG: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
    // The CacheLoader is an ARENA object (Value::Obj) when it is a real bytecode
    // class — an anonymous `CacheLoader` subclass, e.g. Block's SHAPE_FULL_BLOCK_CACHE
    // loader. The Ref-only match below dropped it → `getUnchecked` returned null →
    // `Boolean.booleanValue()` NPE in Block.isShapeFullBlock. Dispatch on its concrete
    // arena class with the Obj receiver. (Feature-gated; default build is untouched.)
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = vm.heap.get_field(recv, "$cacheloader") {
        let actual = match crate::nextgen::class_name(crate::nextgen::arena_class_id(p)) {
            Some(a) => a.to_string(),
            None => return Ok(Value::Null),
        };
        return match vm.resolve_method(&actual, "load", SIG)? {
            Some(decl) => Ok(vm.invoke(decl, "load", SIG, vec![Value::Obj(p), key.clone()])?.unwrap_or(Value::Null)),
            None => Ok(Value::Null),
        };
    }
    let loader = match vm.heap.get_field(recv, "$cacheloader") {
        Value::Ref(l) => l,
        _ => return Ok(Value::Null),
    };
    let actual = vm.heap.class_of(loader).unwrap_or("").to_string();
    match vm.resolve_method(&actual, "load", SIG)? {
        Some(decl) => Ok(vm
            .invoke(decl, "load", SIG, vec![Value::Ref(loader), key.clone()])?
            .unwrap_or(Value::Null)),
        None => Ok(Value::Null),
    }
}

/// Dispatch a method on a synthetic cache object (receiver = args[0]).
pub(crate) fn dispatch(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let recv = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    let Some(id) = map_id(vm, recv) else { return Ok(None) };
    let key = args.get(1).cloned();
    match name {
        // Loading reads: memoised, populate on miss via the loader.
        "get" | "getUnchecked" | "apply" => {
            let key = key.unwrap_or(Value::Null);
            // G4: a GIL-free fork worker must NOT touch the shared cache $map (map_get/map_put
            // race). It memoises into a PER-WORKER thread-local table instead — race-free (own
            // table), and crucially still MEMOISED so an expensive, heavily-reused loader (e.g.
            // SHAPE_FULL_BLOCK_CACHE) is computed once per worker, not recomputed every call (a
            // plain bypass recompute-explodes). Deterministic loader → bit-exact. The worker
            // thread ends after the region, dropping its table.
            #[cfg(feature = "next-gen-gc")]
            if crate::parallel::gilfree() {
                let wks = structural_key(vm, &key);
                // Shared-cache hit (lock only for the lookup; loader runs outside the lock).
                if let Ok(g) = FORK_CACHE.lock() {
                    if let Some(v) = g.as_ref().and_then(|m| m.get(&id)).and_then(|m| m.get(&wks)).cloned() {
                        return Ok(Some(Some(v)));
                    }
                }
                let val = match args.get(2) {
                    Some(callable) => vm.apply(callable, Vec::new())?.unwrap_or(Value::Null),
                    None => load(vm, recv, &key)?,
                };
                if let Ok(mut g) = FORK_CACHE.lock() {
                    g.get_or_insert_with(Default::default).entry(id).or_default().insert(wks, val.clone());
                }
                return Ok(Some(Some(val)));
            }
            let ks = crate::map::keyroot::cache_key(vm, &key);
            if let Some(v) = vm.heap.map_get(id, &ks) {
                return Ok(Some(Some(v)));
            }
            // get(K, Callable) supplies the value via the callable; otherwise the
            // configured CacheLoader.
            let val = match args.get(2) {
                Some(callable) => vm.apply(callable, Vec::new())?.unwrap_or(Value::Null),
                None => load(vm, recv, &key)?,
            };
            vm.heap.map_put(id, ks, key, val.clone());
            Ok(Some(Some(val)))
        }
        "getIfPresent" => {
            let ks = crate::map::keyroot::cache_key(vm, &key.clone().unwrap_or(Value::Null));
            Ok(Some(Some(vm.heap.map_get(id, &ks).unwrap_or(Value::Null))))
        }
        "put" => {
            let key = key.unwrap_or(Value::Null);
            let val = args.get(2).cloned().unwrap_or(Value::Null);
            let ks = crate::map::keyroot::cache_key(vm, &key);
            vm.heap.map_put(id, ks, key, val);
            Ok(Some(None))
        }
        "invalidate" => {
            // Compare by the SAME MapKey the other arms store under, not by the raw rendered
            // string — under the identity rule a key may never be rendered at all.
            let ks = crate::map::keyroot::cache_key(vm, &key.unwrap_or(Value::Null));
            // Overwrite with a fresh empty mapping is unavailable; drop by
            // re-inserting nothing — emulate removal by putting a tombstone is
            // wrong, so rebuild without the key.
            let kept: Vec<(Value, Value)> = vm
                .heap
                .map_entries(id)
                .into_iter()
                .filter(|(k, _)| crate::map::keyroot::cache_key(vm, k) != ks)
                .collect();
            let fresh = vm.heap.alloc_map();
            for (k, v) in kept {
                let kk = crate::map::keyroot::cache_key(vm, &k);
                vm.heap.map_put(fresh, kk, k, v);
            }
            vm.heap.put_field_syn(recv, crate::heap::Syn::Map, Value::Int(fresh as i32));
            Ok(Some(None))
        }
        "invalidateAll" => {
            let fresh = vm.heap.alloc_map();
            vm.heap.put_field_syn(recv, crate::heap::Syn::Map, Value::Int(fresh as i32));
            Ok(Some(None))
        }
        "size" => Ok(Some(Some(Value::Long(vm.heap.map_entries(id).len() as i64)))),
        // No-ops that return nothing or the receiver.
        "cleanUp" | "refresh" => Ok(Some(None)),
        // asMap: expose the backing map as a HashMap view over the same id.
        "asMap" => {
            let h = vm.heap.alloc("java/util/HashMap");
            vm.heap.put_field_syn(h, crate::heap::Syn::Map, Value::Int(id as i32));
            Ok(Some(Some(Value::Ref(h))))
        }
        _ => Ok(None),
    }
}
