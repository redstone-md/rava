//! Allocating an arena object identified by class name.

use crate::*;

impl Vm {
    /// FRAME-FREE arena birth route (`RAVA_ARENA_NATIVE_NEW`, default OFF).
    ///
    /// `new_object_value` asks four questions before it lets an object into the arena, but it
    /// is written against a frame and a call site and answers through a `NewSignal`, so no
    /// native can reuse it — and `reflect::new_instance` therefore does a bare
    /// `heap.alloc(cls)`. Every object a native mints is a handle regardless of any arena
    /// flag, which is why gson's `JsonPrimitive` (28,739 cells) and `JsonObject` (14,992)
    /// survive with all four flip flags armed.
    ///
    /// This asks the SAME four questions with none of the protocol: a name-matched
    /// native-backed class, an enum under a disarmed flag, a native-backed ancestor, and a
    /// layout that is not cached (partial super chain) each veto the arena, exactly as they
    /// do at the opcode. It never runs `<clinit>` — the caller has already done
    /// `ensure_init` — and `tlab_try_alloc` never collects, so the returned pointer cannot
    /// move before the caller writes to it.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn try_arena_alloc_by_name(&mut self, cls: &str) -> Option<crate::arena::ArenaPtr> {
        if !crate::nextgen::arena_native_new_enabled() {
            return None;
        }
        // ROUTE MEMO, keyed by CLASS NAME — the frame-free twin of `RAVA_ALLOC_ROUTE_MEMO`.
        //
        // That memo keys on the CALL SITE, and this route has none: it exists precisely so a
        // native can allocate without a frame. So every `new` served here re-derived the whole
        // name gauntlet — `synthetic_layout`, `birth_native_backed_alloc`, `get_or_load`,
        // `intern_class`, `class_field_layout` and a `class_layout_cache` probe under a
        // `RwLock` — five hash lookups and a lock, per allocation.
        //
        // Measured, and this is what pays for it: with the synthetic migration armed the PC
        // profile of the serial prefix shows `owner_bits::bits` DOUBLING (2.2% → 4.3%) and
        // `super_chain` 1.1% → 1.9% against the same block without it. Row 102 attributed the
        // migration's 22 s of wall to the flag but never said where it went; this is where.
        //
        // The decision is a pure function of the name, by the same argument the site memo
        // makes: one name is one class identity for the process lifetime, `super_chain` is
        // cached-frozen, and the deciding flags are process-constant `OnceLock`s. UNSTABLE
        // cases — a class that will not load yet, or whose layout chain is still incomplete —
        // are NEVER inserted and keep re-deriving, which is the same rule the site memo uses.
        #[derive(Clone, Copy)]
        enum Route {
            Handle,
            Arena { cid: u32, nfields: u32 },
        }
        thread_local! {
            static ROUTES: std::cell::RefCell<FastMap<Box<str>, Route>> =
                std::cell::RefCell::new(FastMap::default());
        }
        fn remember(cls: &str, r: Route) {
            ROUTES.with(|m| {
                m.borrow_mut().insert(cls.into(), r);
            });
        }
        if let Some(r) = ROUTES.with(|m| m.borrow().get(cls).copied()) {
            return match r {
                Route::Handle => None,
                Route::Arena { cid, nfields } => crate::nextgen::tlab_try_alloc(cid, nfields),
            };
        }
        // SYNTHETIC-ONLY CLASSES. A class rava implements natively may have no class file on
        // the path at all — `java/util/Optional` is ours end to end — so `get_or_load` below
        // fails and every route that starts from a declared layout is closed to exactly the
        // classes the synthetic injection targets. Their layout is not "declared fields plus
        // synthetics"; it IS the synthetics, and that is knowable without a class file.
        // A DECLARED synthetic set OVERRIDES the name heuristic. `birth_native_backed_alloc`
        // is a proxy for "we do not know where this class keeps its state, so it must stay a
        // handle" — and a class listed in `synthetic_layout` is precisely one where we DO
        // know: rava's own natives define it. Without this, `java/util/BitSet` can never be
        // arena-born no matter what its layout says, because the substring `Set` decides
        // first. The check stays conservative for every class NOT in the table.
        #[cfg(feature = "next-gen-gc")]
        let has_synth =
            crate::nextgen::arena_synth_enabled() && !crate::nextgen::synthetic_layout(cls).is_empty();
        #[cfg(not(feature = "next-gen-gc"))]
        let has_synth = false;
        #[cfg(feature = "next-gen-gc")]
        {
            let synth = crate::nextgen::synthetic_layout(cls);
            if has_synth && self.loader.get_or_load(cls).is_err() {
                let arc = self.heap.intern_class(cls);
                let layout: std::sync::Arc<[(u32, u8)]> = synth
                    .iter()
                    .map(|&(n, k)| (self.heap.intern_field(n), k))
                    .collect::<Vec<_>>()
                    .into();
                // Publish it so `slot_of` (which `synth_get`/`synth_put` consult) resolves,
                // and so a second allocation reuses the same class id.
                self.heap.set_layout(cls, layout.clone());
                let cid = crate::nextgen::register_class(&arc, &layout);
                let nf = layout.len() as u32;
                remember(cls, Route::Arena { cid, nfields: nf });
                return crate::nextgen::tlab_try_alloc(cid, nf);
            }
        }
        if !has_synth && crate::opcodes::birth_native_backed_alloc(cls) {
            remember(cls, Route::Handle);
            return None;
        }
        // NOT memoised: a class that will not load YET may load later, and freezing a Handle
        // verdict here would force it to a handle for the process lifetime.
        let loaded = self.loader.get_or_load(cls).ok()?;
        let enum_shaped = loaded.file.access_flags & 0x4000 != 0
            || loaded.file.super_class.as_deref() == Some("java/lang/Enum");
        drop(loaded);
        if enum_shaped && !crate::nextgen::arena_enum_enabled() {
            remember(cls, Route::Handle);
            return None;
        }
        // Same reasoning for the ancestor scan: it asks "does this class INHERIT state we
        // cannot see?", and a declared synthetic set answers that we can.
        if !has_synth
            && self.birth_native_backed_ancestor(cls)
            && !self.overrides_collection_backing(cls)
        {
            remember(cls, Route::Handle);
            return None;
        }
        let arc = self.heap.intern_class(cls);
        let layout = self.class_field_layout(&arc);
        // Absence from the COMPLETE-layout cache is the same partial-chain signal the opcode
        // reads; a short-celled arena object would panic on the first inherited putfield.
        if !self.runtime_shared().class_layout_contains(cls) {
            // NOT memoised either: an incomplete super chain completes as soon as the missing
            // class loads, and this verdict must be allowed to change with it.
            return None;
        }
        let cid = crate::nextgen::register_class(&arc, &layout);
        let nf = layout.len() as u32;
        remember(cls, Route::Arena { cid, nfields: nf });
        crate::nextgen::tlab_try_alloc(cid, nf)
    }
}
