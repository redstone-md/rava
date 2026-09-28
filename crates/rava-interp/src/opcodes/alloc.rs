//! Object and array creation, with the replay path a routed allocation takes.
//!
//! Re-exported from the parent module, so no call site outside it changed.

#![allow(invalid_reference_casting)]
use crate::opcodes::*;

pub(crate) fn new_object(vm: &crate::vm::thread_context::SharedVm, frame_class: &std::sync::Arc<crate::class_loader::LoadedClass>, index: u16) -> Result<u32, VmError> {
    #[cfg(feature = "next-gen-gc")]
    let _alloc_guard = if crate::parallel::gilfree() {
        Some(GILFREE_ALLOC_LOCK.lock().unwrap_or_else(|e| e.into_inner()))
    } else {
        None
    };
    // Per-site cache: after the first alloc the class is loaded + initialised (a class
    // never un-inits) and its name is interned, so skip the name String alloc +
    // get_or_load + ensure_init + re-intern — just allocate with the cached Arc.
    let site = (std::sync::Arc::as_ptr(frame_class) as usize, index);
    // Bind the lookup to a `let` first: an `if let` scrutinee keeps its temporaries — here the
    // read guard — alive for the whole body, which then cannot take `&mut vm`.
    let cached_site_class = vm.runtime_shared().new_site_get(&site);
    if let Some(arc) = cached_site_class {
        // `alloc_interned` CONSUMES the name `Arc`, so this hit took TWO refcount pairs per birth:
        // one to lift the name out of the shared site map from under its read guard, and a second
        // to hand a copy to the allocator. `prepopulate_new` is inert unless RAVA_LAYOUT_ALLOC is
        // set, so in the default configuration nothing reads the name afterwards and the clone we
        // already own can just be handed over — one atomic increment/decrement pair per `new`.
        if !crate::layout_alloc_enabled() {
            let h = vm.heap.alloc_interned(arc);
            #[cfg(feature = "next-gen-gc")]
            if crate::parallel::gilfree() {
                crate::parallel::owned_handle_note(h);
            }
            return Ok(h);
        }
        let h = vm.heap.alloc_interned(arc.clone());
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() {
            crate::parallel::owned_handle_note(h);
        }
        prepopulate_new(vm, h, &arc);
        return Ok(h);
    }
    let name = class_name_at(frame_class, index)?;
    if let Ok(class) = vm.loader.get_or_load(&name) {
        // Unconditional, unlike the two arena birth sites, which gate on a relaxed `initialized`
        // load first — so this one really did take the out-of-line call and its SeqCst probe on
        // every miss of the site cache above. One acquire load decides the finished case.
        vm.ensure_init_shared(&class)?;
    }
    let arc = vm.heap.intern_class(&name);
    // GAP-S09 stage 2 — DOCUMENTED EXCEPTION to the plain not-gilfree guard: unlike
    // the other frozen tables, gil-free interp-arm workers legitimately reach this
    // insert TODAY (the fn doc above: "self.objects push + new_site_cache" are
    // serialised by GILFREE_ALLOC_LOCK, taken at entry whenever gilfree). The
    // executable spec here is therefore "no UNLOCKED gil-free mutation": the guard
    // taken at function entry IS the protection. The interp arms themselves are
    // slated for stage-5 quarantine, after which the plain guard can replace this.
    #[cfg(feature = "next-gen-gc")]
    debug_assert!(
        !crate::parallel::gilfree() || _alloc_guard.is_some(),
        "GAP-S09: gil-free new_site_cache insert without GILFREE_ALLOC_LOCK"
    );
    vm.runtime_shared().new_site_insert(site, arc.clone());
    let h = vm.heap.alloc_interned(arc.clone());
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel::gilfree() {
        crate::parallel::owned_handle_note(h);
    }
    prepopulate_new(vm, h, &arc);
    Ok(h)
}

/// `anewarray` (interpreter 0xbd AND the compiled `jit_anewarray` callback):
/// allocate a reference array for the class at cp `index`. Under
/// RAVA_ALLOC_ROUTE_MEMO the (caller class ptr, cp) site memoises the INTERNED
/// ELEMENT ID, skipping the per-alloc element-name String + `L…;` descriptor
/// build + interner hash — the same O(string)/O(malloc) per-call bookkeeping
/// class as the `new` route. The id is process-permanent (the array-element
/// interner only grows). Insert is main-thread-only (mirrors alloc_route_insert).
pub(crate) fn anewarray_ref(
    vm: &crate::vm::thread_context::SharedVm,
    frame_class: &std::sync::Arc<crate::class_loader::LoadedClass>,
    index: u16,
    len: usize,
) -> Result<u32, VmError> {
    let site = (std::sync::Arc::as_ptr(frame_class) as usize, index);
    if alloc_route_memo_enabled() {
        if let Some(eid) = vm.runtime_shared().anewarray_get(&site) {
            return Ok(vm.heap.alloc_ref_array_by_id(eid, len));
        }
    }
    // Miss path: ANEWARRAY_SCRATCH borrows the class name straight from the
    // constant pool and builds `L<name>;` in a stack buffer - zero heap Strings
    // on a path that otherwise built two per call. Same lookup, same error.
    if crate::anewarray_scratch() {
        let elem = frame_class.file.pool.class_name(index).ok_or(VmError::BadConstant(index))?;
        crate::heap::array_intern_stats::count_ref_desc_built();
        return Ok(with_ref_desc(elem, |d| {
            let h = vm.heap.alloc_array(d, len);
            if alloc_route_memo_enabled() && !crate::parallel::gilfree() {
                if let Some(eid) = vm.heap.array_element_id_of(d) {
                    vm.runtime_shared().anewarray_insert(site, eid);
                }
            }
            h
        }));
    }
    let elem = class_name_at(frame_class, index)?;
    crate::heap::array_intern_stats::count_ref_desc_built();
    // `with_ref_desc`, not a raw wrap: an `anewarray` whose component is ITSELF an
    // array type (`new int[3][]` -> `anewarray [I`) must keep the bare descriptor.
    // See the note there.
    let desc = crate::opcodes::with_ref_desc(&elem, |d| d.to_string());
    let h = vm.heap.alloc_array(&desc, len);
    if alloc_route_memo_enabled() && !crate::parallel::gilfree() {
        if let Some(eid) = vm.heap.array_element_id_of(&desc) {
            vm.runtime_shared().anewarray_insert(site, eid);
        }
    }
    Ok(h)
}

/// Lever-#1 (step 2): when RAVA_LAYOUT_ALLOC is set, give a freshly allocated
/// object its full per-class field layout up front (fixed slots, JVM-default
/// values) so putfield never appends and getfield's slot is class-constant.
/// Default OFF (one cached env check) — the slot model is unchanged otherwise.
pub(crate) fn prepopulate_new(vm: &crate::vm::thread_context::SharedVm, handle: u32, class: &std::sync::Arc<str>) {
    if !crate::layout_alloc_enabled() {
        return;
    }
    let layout = vm.class_field_layout(class);
    vm.heap.prepopulate_layout(handle, &layout);
}

/// Replay a memoised route: no name String, no `get_or_load`, no substring
/// scans, no interner/registry hashing. The dynamic parts keep their exact
/// per-call semantics: clinit state is re-checked (Cell read) and the
/// nursery-full collect/retry tail is byte-identical to the derive path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn new_route_replay(
    vm: &crate::vm::thread_context::SharedVm,
    frame_class: &std::sync::Arc<crate::class_loader::LoadedClass>,
    index: u16,
    route: NewRoute,
) -> Result<NewSignal, VmError> {
    match route {
        NewRoute::ArenaMap => {
            let p = match crate::arena_map::new_map() {
                Some(p) => p,
                None => {
                    let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
                    vm_mut.arena_collect_jit();
                    crate::arena_map::new_map().expect("nursery freed by collect")
                }
            };
            Ok(NewSignal::Value(Value::Obj(p)))
        }
        NewRoute::Handle => Ok(NewSignal::Value(Value::Ref(new_object(vm, frame_class, index)?))),
        NewRoute::Arena { class, cid, nf, pinned } => {
            // ⛔**NOT `ensure_init_handoff`, AND THE GUARD IS WHY.** `initialized` is stored
            // BEFORE `<clinit>` runs and `clinit_done` only after it returns, so `!initialized`
            // implies `!clinit_done`: inside this branch `ensure_init_fast`'s acquire load can
            // never be true, and switching would add a load while removing nothing. The relaxed
            // load below IS already the one-load fast-out this round is about. Same at the twin
            // site in `new_object_value`.
            if !class.initialized.load(std::sync::atomic::Ordering::Relaxed) {
                let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
                vm_mut.ensure_init(&class)?;
            }
            if pinned {
                let raw = crate::nextgen::pinned_object_alloc(cid, nf);
                let p = crate::arena::ArenaPtr::new(raw).expect("pinned object non-null");
                return Ok(NewSignal::Value(Value::Obj(p)));
            }
            match crate::nextgen::tlab_try_alloc(cid, nf) {
                Some(p) => Ok(NewSignal::Value(Value::Obj(p))),
                None if vm.trampoline => Ok(NewSignal::NurseryFull),
                None => {
                    let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
                    vm_mut.arena_collect_jit();
                    match crate::nextgen::tlab_try_alloc(cid, nf) {
                        Some(p) => Ok(NewSignal::Value(Value::Obj(p))),
                        None => {
                            eprintln!(
                                "rava[next-gen-gc]: nursery still full after safepoint collect \
                                 allocating {} ({} arena allocs) — tenured space exhausted. Hard exit.",
                                class.file.this_class,
                                crate::nextgen::ARENA_ALLOCS
                                    .load(std::sync::atomic::Ordering::Relaxed),
                            );
                            std::process::exit(3);
                        }
                    }
                }
            }
        }
    }
}

/// ⭐**Frame-free so the hot loop can run `0xbb`** (row 379): every use of the frame here was
/// `frame.class`. Returns a `NewSignal`, and a caller that cannot handle `NurseryFull` or
/// `ClinitPending` must fall back rather than guess — the hot arm does exactly that.
pub(crate) fn new_object_value(vm: &crate::vm::thread_context::SharedVm, frame_class: &std::sync::Arc<crate::class_loader::LoadedClass>, index: u16) -> Result<NewSignal, VmError> {
    #[cfg(not(feature = "next-gen-gc"))]
    {
        Ok(NewSignal::Value(Value::Ref(new_object(vm, frame_class, index)?)))
    }
    #[cfg(feature = "next-gen-gc")]
    {
        // ALLOC-ROUTE SITE MEMO (RAVA_ALLOC_ROUTE_MEMO): replay a previously derived
        // route for this exact `new` site — the whole name-keyed derivation below is
        // per-class-permanent (see alloc_route_memo_enabled).
        let site = (std::sync::Arc::as_ptr(frame_class) as usize, index);
        if alloc_route_memo_enabled() {
            let memoised = vm.runtime_shared().alloc_route_get(&site);
            if let Some(route) = memoised {
                return new_route_replay(vm, frame_class, index, route);
            }
        }
        let name = class_name_at(frame_class, index)?;
        // LIVE ARENA-MAP CUT-OVER (RAVA_ARENA_MAPS): a `java/util/HashMap` allocates
        // a flat open-addressing table straight on the arena instead of the native
        // `$map` (std::HashMap) backend. A construction-time nursery overflow can
        // collect freely — the map isn't a root until we return it.
        // Int2ObjectOpenHashMap (MC chunk maps / registries) gets the same arena flat
        // table: its int keys resolve to MapKey::Int and fastutil_alias maps its method
        // surface (int2ObjectEntrySet→entrySet, getIntKey→getKey) onto the same
        // arena_map::dispatch HashMap already uses. EXACT class only — Paper subclasses
        // (LoadingIntMap) keep their own routing. Unblocks inline-int values + the
        // int-key probe for MC's hottest maps.
        if crate::arena_map::enabled()
            && matches!(name.as_str(), "java/util/HashMap" | "it/unimi/dsi/fastutil/ints/Int2ObjectOpenHashMap")
        {
            alloc_route_insert(vm, site, NewRoute::ArenaMap);
            let p = match crate::arena_map::new_map() {
                Some(p) => p,
                None => {
                    let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
                    vm_mut.arena_collect_jit();
                    crate::arena_map::new_map().expect("nursery freed by collect")
                }
            };
            return Ok(NewSignal::Value(Value::Obj(p)));
        }
        // Dual contract (Phase 4): a class is HANDLE-backed (served by the native
        // layer through synthetic `$elems`/`$map`/`$path`/… fields that only exist
        // on a heap object) when it is a collection/map OR has no bytecode in the
        // classpath at all — a pure JDK class (java/io/File, …) whose entire state
        // lives in native synthetic fields. Only a real bytecode class with a
        // declared field layout becomes a pointer-based ARENA object.
        let loaded = vm.loader.get_or_load(&name);
        // Enums extend java.lang.Enum — a native-backed super whose name/ordinal
        // live in synthetic $enumName/$enumOrd fields that only exist on a heap
        // handle (Enum.<init>/ordinal/name handle Value::Ref only). An arena enum
        // would read ordinal()==0 for EVERY constant, corrupting any ordinal switch
        // (e.g. guava AbstractIterator's state machine → infinite hasNext loop). So
        // route enums to a handle like any other native-backed class.
        // ENUM MIGRATION (RAVA_ARENA_ENUM, default OFF): when the flag is set an
        // enum is BORN in the arena — its name/ordinal ride two synthetic layout
        // cells ($enumName/$enumOrd, injected by class_field_layout), and the enum
        // natives (Enum.<init>/ordinal/name/toString/compareTo) route through
        // arena_get/putfield on a Value::Obj receiver. Flag OFF keeps the historic
        // forced-handle route below (byte-identical). Note: enum-constant-body
        // subclasses (E$1 extends E) already fall through to the arena path today
        // (their direct super is E, not Enum, and they lack ACC_ENUM); the flag
        // additionally makes the plain enum class itself arena-born, and the layout
        // injection is keyed off the WHOLE super chain so both forms get the cells.
        let is_enum = !crate::nextgen::arena_enum_enabled()
            && loaded.as_ref().is_ok_and(|c| {
                c.file.access_flags & 0x4000 != 0
                    || c.file.super_class.as_deref() == Some("java/lang/Enum")
            });
        // A subclass of a native-backed JDK type inherits fields rava models as
        // synthetic heap-only `$fields` (e.g. guava `ImmutableMapEntry` extends
        // `AbstractMap$SimpleImmutableEntry`, whose key/value are `$key`/`$val`).
        // The arena layout has no cell for them, so the instance MUST be a handle —
        // else the native accessor rejects the arena receiver and returns a no-op,
        // underflowing the caller's `areturn`. Mirrors the enum super-routing.
        // BIRTH NAMESPACE GUARD (RAVA_ARENA_BIRTH_NS_GUARD, default OFF): both the
        // direct test and the ancestor scan go through the birth-only predicate, so
        // the collection SUBSTRING heuristic stops force-handling jar classes that
        // merely contain "List"/"Set"/"Stack"/… while a real native-backed ancestor
        // (always inside a native namespace) still routes the subclass to a handle.
        let native_super = loaded.is_ok()
            && vm.birth_native_backed_ancestor(&name)
            && !vm.overrides_collection_backing(&name);
        let native_state = loaded.as_ref().ok().map(|class| class.native_state);
        let by_name = native_state
            .and_then(crate::native_state::NativeStateFact::slots)
            .map(|_| true)
            .unwrap_or_else(|| birth_native_backed_alloc(&name));
        // A DECLARED synthetic set overrides both name and ancestor, for the same reason it
        // does in `try_arena_alloc_by_name`: those predicates are proxies for "we do not know
        // where this class keeps its state", and a class in `synthetic_layout` is one where
        // rava's own natives define it. `java/util/BitSet` is otherwise unreachable — the
        // substring `Set` decides before any arena route is consulted.
        #[cfg(feature = "next-gen-gc")]
        let has_synth = crate::nextgen::arena_synth_enabled()
            && !crate::nextgen::synthetic_layout(&name).is_empty();
        #[cfg(not(feature = "next-gen-gc"))]
        let has_synth = false;
        let born_handle = (!has_synth && (by_name || native_super)) || is_enum;
        // Birth-route census for enum-shaped classes (RAVA_ENUM_BIRTH_CENSUS, inert otherwise).
        // This site has no `!initialized` term — it runs <clinit> and continues — so a split
        // between "interp" and "jit" here localises why RAVA_ARENA_ENUM migrates nothing.
        #[cfg(feature = "next-gen-gc")]
        if crate::enum_birth::enabled() {
            let looks_enum = loaded.as_ref().is_ok_and(|c| {
                c.file.access_flags & 0x4000 != 0
                    || c.file.super_class.as_deref() == Some("java/lang/Enum")
            });
            // NO `looks_enum` filter. The census was written for the enum question and answered
            // it; the same three terms decide EVERY class's birth route, and the remaining
            // ~790 k blocking cells are gson, VoxelShape and the fastutil collections, not
            // enums. Keeping the filter would mean re-deriving the same instrument per target.
            // `looks_enum` still selects the site label so the enum rows stay separable.
            // `by_name` is REUSED from `born_handle` above, never recomputed: unfiltered this
            // runs on every object birth, and `birth_native_backed_alloc` is the substring
            // scan that costs ~7 passes per `new` — recomputing it here is what stopped the
            // census boot from reaching Done inside a 400 s timeout.
            crate::enum_birth::note(
                if looks_enum { "interp-enum" } else { "interp" },
                &name,
                is_enum,
                native_super,
                by_name,
            );
        }
        // SYNTHETIC-ONLY CLASS. `loaded.is_err()` sends a class with no class file to a
        // handle, which is right when we know nothing about it — and wrong for exactly the
        // classes rava implements END TO END: `java/util/BitSet` has no class file precisely
        // BECAUSE we are its implementation, and its state is the `$bits` cell the table
        // declares. Their layout is not "declared fields plus synthetics"; it IS the
        // synthetics, and that is knowable without the file. Same branch as
        // `try_arena_alloc_by_name`'s, at the opcode birth site where the volume is.
        #[cfg(feature = "next-gen-gc")]
        if has_synth && loaded.is_err() {
            let synth = crate::nextgen::synthetic_layout(&name);
            let arc = vm.heap.intern_class(&name);
            let layout: std::sync::Arc<[(u32, u8)]> = synth
                .iter()
                .map(|&(n, k)| (vm.heap.intern_field(n), k))
                .collect::<Vec<_>>()
                .into();
            vm.heap.set_layout(&name, layout.clone());
            let cid = crate::nextgen::register_class(&arc, &layout);
            let nf = layout.len() as u32;
            if let Some(p) = crate::nextgen::tlab_try_alloc(cid, nf) {
                return Ok(NewSignal::Value(Value::Obj(p)));
            }
            // Nursery full: fall through to the handle path, which is always correct.
        }
        if born_handle || loaded.is_err() {
            // Cache only the PERMANENT handle decision (name/enum/ancestor). The
            // bare not-loadable fallback is transient — a synthesized class can
            // clear the negative cache later — so it keeps re-deriving.
            if born_handle {
                alloc_route_insert(vm, site, NewRoute::Handle);
            }
            return Ok(NewSignal::Value(Value::Ref(new_object(vm, frame_class, index)?)));
        }
        let class = loaded.expect("loaded.is_err() handled above");
        // Class init BEFORE the first arena instance. In trampoline mode, signal
        // the driver to run <clinit> as flat frames (its allocs then safepoint
        // too); a recursive invoke (trampoline=false) has to init inline.
        // Left on the plain `ensure_init` deliberately — see `new_route_replay`: `!initialized`
        // already implies `!clinit_done`, so the fast test could only ever add a load here.
        if !class.initialized.load(std::sync::atomic::Ordering::Relaxed) {
            if vm.trampoline {
                return Ok(NewSignal::ClinitPending(class));
            }
            let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
            vm_mut.ensure_init(&class)?;
        }
        let arc = vm.heap.intern_class(&name);
        let layout = vm.class_field_layout(&arc);
        // class_field_layout only CACHES a complete layout (full super chain loadable).
        // If it isn't cached, the chain was partial (a super transiently unloadable) →
        // the layout misses inherited fields → an arena putfield would panic. Fall back
        // to a heap handle (interp's behavior) rather than commit a short-celled object.
        if !vm.runtime_shared().class_layout_contains(name.as_str()) {
            #[cfg(feature = "next-gen-gc")]
            crate::enum_birth::note_layout_fallback(&name);
            return Ok(NewSignal::Value(Value::Ref(new_object(vm, frame_class, index)?)));
        }
        let cid = crate::nextgen::register_class(&arc, &layout);
        let nf = layout.len() as u32;
        // BORN-PINNED (RAVA_PINNED_ENUM): a long-lived enum singleton is born in the
        // NON-MOVING pinned region — the moving Cheney collector never copies it, so
        // the STEP-3 interim cost (every-GC copy of every enum: +130MB peakWS/+1.8s)
        // is recovered. Detect an enum by the injected `$enumOrd` layout cell (added by
        // `class_field_layout` for the whole java/lang/Enum super chain, incl. E$1
        // constant bodies), so the pin routing matches exactly the classes the enum
        // natives handle. Only enums are pinned — every other arena class still takes
        // the moving-arena `tlab_try_alloc` below. The pinned object carries the SAME
        // header + inline cells, so get/putfield and identity are unchanged; its ref
        // cells are traced as roots by `arena_collect_jit`'s pinned-region root walk.
        // `pinned_object_alloc` never triggers a collection (pure bump), so the
        // in-flight receiver at this callsite cannot move — GC-safe like the arena path.
        let pinned = crate::nextgen::pinned_enum_enabled() && {
            // Cache the `$enumOrd` field id (interned by `class_field_layout` for the
            // enum whose layout was just built above) so this runs at most one
            // no-intern hash lookup, then a plain compare, per arena `new` — no
            // per-alloc string hashing on the hot birth path.
            use std::sync::atomic::{AtomicU32, Ordering};
            static ORD_ID: AtomicU32 = AtomicU32::new(u32::MAX);
            let mut ord_id = ORD_ID.load(Ordering::Relaxed);
            if ord_id == u32::MAX {
                if let Some(id) = vm.heap.field_id_opt("$enumOrd") {
                    ORD_ID.store(id, Ordering::Relaxed);
                    ord_id = id;
                }
            }
            ord_id != u32::MAX && layout.iter().any(|&(id, _)| id == ord_id)
        };
        // The whole route (flag + layout + cid/nf) is permanent from here on.
        alloc_route_insert(
            vm,
            site,
            NewRoute::Arena { class: std::sync::Arc::clone(&class), cid, nf, pinned },
        );
        if pinned {
            let raw = crate::nextgen::pinned_object_alloc(cid, nf);
            let p = crate::arena::ArenaPtr::new(raw).expect("pinned object non-null");
            return Ok(NewSignal::Value(Value::Obj(p)));
        }
        match crate::nextgen::tlab_try_alloc(cid, nf) {
            Some(p) => Ok(NewSignal::Value(Value::Obj(p))),
            None if vm.trampoline => Ok(NewSignal::NurseryFull),
            None => {
                // TASK 2 — recursive/compiled path: run a native Cheney safepoint
                // (forwards JIT cell buffers + frames + statics in place), then
                // retry once into the freed nursery. No trampoline needed.
                let vm_mut = unsafe { &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm) };
                vm_mut.arena_collect_jit();
                match crate::nextgen::tlab_try_alloc(cid, nf) {
                    Some(p) => Ok(NewSignal::Value(Value::Obj(p))),
                    None => {
                        eprintln!(
                            "rava[next-gen-gc]: nursery still full after safepoint collect \
                             allocating {arc} ({} arena allocs{}) — tenured space exhausted. Hard exit.",
                            crate::nextgen::ARENA_ALLOCS.load(std::sync::atomic::Ordering::Relaxed),
                            if crate::parallel::arena_allocs_counted() { "" } else { ", NOT COUNTED — set RAVA_JIT_STATS" },
                        );
                        std::process::exit(3);
                    }
                }
            }
        }
    }
}
