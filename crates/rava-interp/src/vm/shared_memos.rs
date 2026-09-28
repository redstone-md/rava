//! The synchronized memo/cache state of the [`Vm`], extracted as a real
//! published slice: an aggregate ([`SharedMemos`]) and a borrow-only safe
//! view over it ([`RuntimeShared`]).
//!
//! ⭐**WHY THIS EXISTS.** The context-or-fallback migration made `&SharedVm`
//! the execution signature, but `SharedVm == Vm` is still the WHOLE machine —
//! a borrow of it reaches call stacks, JIT caches, GC scheduling, and every
//! other unsynchronized field. This module is the next safe boundary: the
//! tables here are exactly the ones already synchronised behind an `RwLock`
//! (or a bare `AtomicU64`), i.e. exactly the state a concurrent reader may
//! touch on a shared reference without a further lock policy. [`RuntimeShared`]
//! borrows ONLY this aggregate — it cannot reach `Vm`, holds no pointer to
//! it, implements no `Deref`, and exposes no mutable execution API.
//!
//! Excluded by design: JIT caches, GC roots/scheduling, the loader, the heap,
//! task state, the JIT context, and all execution state (frames/stacks).

use crate::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// The aggregate of synchronized memo/cache tables, split out of [`Vm`]
/// (crate::vmstate::Vm). Constructed empty by [`Vm::new`] and borrowed
/// wholesale by [`Vm::runtime_shared`].
pub struct SharedMemos {
    /// Per-`new`-site cache: (caller class ptr, cp) -> the interned class name. Skips
    /// the per-alloc class-name String allocation + get_or_load + ensure_init + intern
    /// (all constant per site, hot in alloc-heavy code). A class never un-initialises.
    /// SHARED-READABLE (docs/vm-shared-exec-plan.md): read on every `new`, written once per
    /// site. Behind an `RwLock` so `new_object` — and `new_object_value` above it — take `&Vm`.
    new_site_cache: std::sync::RwLock<FastMap<(usize, u16), Arc<str>>>,
    /// `anewarray` per-site memo (same flag): (caller class ptr, cp) → interned
    /// array element id — skips the element-name String + `L…;` descriptor build
    /// + interner hash on every reference-array allocation.
    anewarray_site_cache: std::sync::RwLock<FastMap<(usize, u16), u32>>,
    /// Per-class instance-field LAYOUT: class name → ordered field-identity ids
    /// (superclass fields first, then this class's in declaration order), i.e.
    /// slot index → field id. This is the foundation for fixed per-class field
    /// offsets (lever #1 in docs/jvm-parity.md): once every object of a class
    /// shares this layout, a JIT getfield with a known class becomes a single
    /// constant-offset load (hoistable → field-LICM), instead of the branchy
    /// id-scan. Built lazily by `class_field_layout`; currently DORMANT (only the
    /// RAVA_VERIFY_LAYOUT consistency check reads it) until alloc + JIT are wired.
    /// SHARED-READABLE (the same treatment as the class loader and the field interner): read on
    /// the warm path of every `class_field_layout`, written once per class. The freeze tally
    /// measured workers touching this ZERO times across a ring region, so the lock is uncontended
    /// by construction — it exists to let `resolve_field_id`, and `getfield` above it, take `&Vm`.
    class_layout_cache: std::sync::RwLock<FastMap<Box<str>, std::sync::Arc<[(u32, u8)]>>>,
    /// PARTIAL layouts, valid only for the loader epoch they were computed at.
    ///
    /// `class_field_layout` deliberately does not cache a partial chain: a super that was
    /// transiently unloadable may load later, and a short layout committed then would under-size
    /// every object of that class. So every `new` of such a class re-walked up to 64 supers,
    /// calling `get_or_load` at each level — and verified caller attribution puts
    /// `class_field_layout < new_object < new_object_value < jit_new` at 64.4% of
    /// `ClassLoader::get_or_load` samples, with `get_or_load` itself 4.2% of the serial window.
    ///
    /// The walk's result is a pure function of what the loader currently holds, and that changes
    /// ONLY when the hierarchy epoch moves (`bump_epoch` on every load/register). So keying the
    /// partial result on the epoch is sound: same epoch means the recomputation would return the
    /// identical short layout. The AUTHORITATIVE cache is untouched, which matters because
    /// `new_object_value` reads its ABSENCE as "partial, use the heap fallback".
    partial_layout_cache: std::sync::RwLock<FastMap<Box<str>, (std::sync::Arc<[(u32, u8)]>, u64)>>,
    /// Bumped on every static-field write; the getstatic cache's generation guard
    /// (the cache itself is thread_local — see `tlcache::GETSTATIC`).
    static_gen: AtomicU64,
    /// Per-`new`-site allocation ROUTE memo (RAVA_ALLOC_ROUTE_MEMO, default OFF):
    /// (caller class ptr, cp) → the resolved route (arena-map / handle / arena +
    /// cid/nf/pinned). The route is a pure function of process-permanent state
    /// (one name = one class identity, `super_chain` cached-frozen, deciding
    /// flags process-constant), so a cached entry never changes; transient cases
    /// (unloadable class, incomplete layout) are never inserted. Removes the
    /// per-alloc name String + `get_or_load` + collection-substring scans +
    /// `register_class` lock/scan named by the RIP profile (`memchr`,
    /// `is_contained_in`, `mi_malloc_aligned`, `get_or_load`).
    #[cfg(feature = "next-gen-gc")]
    alloc_route_cache: std::sync::RwLock<FastMap<(usize, u16), opcodes::NewRoute>>,
    /// Memo for `pure_value_collection_override` (consulted by the hot
    /// `jit_native_served_deep` gate on every jit_run entry — the uncached
    /// predicate loads the class and scans its methods/fields).
    #[cfg(feature = "next-gen-gc")]
    pure_value_override_cache: std::sync::RwLock<FastMap<Box<str>, bool>>,
    /// Memo for `keeps_tag_by_declared_type` — a property of the class chain, asked once
    /// per getfield site per compiled method before this existed (ledger row 320).
    #[cfg(feature = "next-gen-gc")]
    tagkeep_memo: std::sync::RwLock<FastMap<String, bool>>,
}

impl Default for SharedMemos {
    fn default() -> Self {
        Self {
            new_site_cache: std::sync::RwLock::new(FastMap::default()),
            anewarray_site_cache: std::sync::RwLock::new(FastMap::default()),
            class_layout_cache: std::sync::RwLock::new(FastMap::default()),
            partial_layout_cache: std::sync::RwLock::new(FastMap::default()),
            static_gen: AtomicU64::new(0),
            #[cfg(feature = "next-gen-gc")]
            alloc_route_cache: std::sync::RwLock::new(FastMap::default()),
            #[cfg(feature = "next-gen-gc")]
            pure_value_override_cache: std::sync::RwLock::new(FastMap::default()),
            #[cfg(feature = "next-gen-gc")]
            tagkeep_memo: std::sync::RwLock::new(FastMap::default()),
        }
    }
}

/// A safe, borrow-only view of the [`Vm`]'s synchronized memo/cache state —
/// the published boundary of this module.
///
/// Holds ONLY a `&'a SharedMemos`. No `Deref`, no `Vm` reference, no raw
/// pointer to the owner, no mutable execution API: every method names one
/// synchronized table and one operation on it. `Send + Sync` is proven by the
/// in-crate test at the bottom of this file.
pub struct RuntimeShared<'a> {
    memos: &'a SharedMemos,
}

impl<'a> RuntimeShared<'a> {
    pub(crate) fn new(memos: &'a SharedMemos) -> Self {
        Self { memos }
    }

    /// Per-site `new` cache: get (clone the interned name out, as every
    /// consumer did before) or insert under the write guard.
    pub(crate) fn new_site_get(&self, site: &(usize, u16)) -> Option<Arc<str>> {
        self.memos
            .new_site_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(site)
            .cloned()
    }

    pub(crate) fn new_site_insert(&self, site: (usize, u16), name: Arc<str>) {
        self.memos
            .new_site_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(site, name);
    }

    /// Allocation-route memo: get (clone the route) or insert.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn alloc_route_get(&self, site: &(usize, u16)) -> Option<opcodes::NewRoute> {
        self.memos
            .alloc_route_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(site)
            .cloned()
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn alloc_route_insert(&self, site: (usize, u16), route: opcodes::NewRoute) {
        self.memos
            .alloc_route_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(site, route);
    }

    /// `anewarray` element-id memo: get or insert.
    pub(crate) fn anewarray_get(&self, site: &(usize, u16)) -> Option<u32> {
        self.memos
            .anewarray_site_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(site)
            .copied()
    }

    pub(crate) fn anewarray_insert(&self, site: (usize, u16), element_id: u32) {
        self.memos
            .anewarray_site_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(site, element_id);
    }

    /// Class-layout cache: get (clone the `Arc` slice), contains, or insert.
    pub(crate) fn class_layout_get(
        &self,
        class: &str,
    ) -> Option<std::sync::Arc<[(u32, u8)]>> {
        self.memos
            .class_layout_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(class)
            .cloned()
    }

    pub(crate) fn class_layout_contains(&self, class: &str) -> bool {
        self.memos
            .class_layout_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(class)
    }

    pub(crate) fn class_layout_insert(
        &self,
        class: &str,
        layout: std::sync::Arc<[(u32, u8)]>,
    ) {
        self.memos
            .class_layout_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(class.into(), layout);
    }

    /// Layout scan that must run UNDER the read guard (the cached-complete path
    /// never clones the `Arc`): returns the slot index of field `id`, if any.
    pub(crate) fn class_layout_slot(&self, class: &str, id: u32) -> Option<u32> {
        self.memos
            .class_layout_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(class)
            .and_then(|l| l.iter().position(|&(i, _)| i == id).map(|p| p as u32))
    }

    /// Partial-layout memo (epoch-keyed): get, or insert.
    pub(crate) fn partial_layout_get(
        &self,
        class: &str,
    ) -> Option<(std::sync::Arc<[(u32, u8)]>, u64)> {
        self.memos
            .partial_layout_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(class)
            .cloned()
    }

    pub(crate) fn partial_layout_insert(
        &self,
        class: &str,
        layout: std::sync::Arc<[(u32, u8)]>,
        epoch: u64,
    ) {
        self.memos
            .partial_layout_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(class.into(), (layout, epoch));
    }

    /// `pure_value_collection_override` memo: get or insert.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_override_get(&self, name: &str) -> Option<bool> {
        self.memos
            .pure_value_override_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied()
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_override_insert(&self, name: &str, verdict: bool) {
        self.memos
            .pure_value_override_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.into(), verdict);
    }

    /// `keeps_tag_by_declared_type` memo: get or insert.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn tagkeep_get(&self, cls: &str) -> Option<bool> {
        self.memos
            .tagkeep_memo
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cls)
            .copied()
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn tagkeep_insert(&self, cls: &str, verdict: bool) {
        self.memos
            .tagkeep_memo
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cls.to_string(), verdict);
    }

    /// The static-write generation — the getstatic value cache's guard.
    #[inline]
    pub(crate) fn static_gen(&self) -> u64 {
        self.memos.static_gen.load(Ordering::Relaxed)
    }

}
impl Vm {
    /// Bump the static-write generation and refresh the native mirror.
    /// Keep both actions together: a stale mirror can serve an old cached root.
    #[inline]
    pub(crate) fn bump_static_gen(&self) {
        crate::parallel::assert_not_gilfree("static_gen");
        let next = self
            .shared_memos
            .static_gen
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.heap.set_static_gen_mirror(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::marker::{Send, Sync};

    // Type-level contract: the aggregate and the borrow-only view are both
    // shareable across threads. This compiles ⇒ proven; it would fail to
    // compile the moment a `Vm` reference, a raw pointer, or any
    // non-synchronized state leaked into either type.
    fn assert_send_sync<T: Send + Sync>() {}
    #[test]
    fn shared_memos_and_view_are_send_sync() {
        assert_send_sync::<SharedMemos>();
        assert_send_sync::<RuntimeShared<'static>>();
    }
}
