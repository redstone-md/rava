//! Which callees a compile may inline, and which statics it may fold.

use crate::*;
// ADDED: the refusal census. Every gate below now says WHICH rule it was, at compile time only.
use crate::vm::inline_census as cen;

impl Vm {
    /// Resolve each invokestatic target in `code`; capture those small + pure
    /// enough to inline (see `jit::is_inlinable`) so the JIT translates their body
    /// into the caller instead of emitting a call.
    pub(crate) fn collect_inline_targets(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashMap<u16, jit::InlineCallee> {
        let max_frame = code.max_locals as usize + code.max_stack as usize;
        // Bound recursive planning first, then bound the completed map's expansion
        // at every bytecode call site. A cp-keyed plan can be emitted many times.
        budget_reset();
        let mut targets = self.collect_inline_targets_depth(code, class, 0, false, max_frame);
        if deep_inline_enabled() {
            super::inline_expansion_budget::constrain(
                &code.bytecode, &mut targets, inline_tree_budget(),
            );
        }
        targets
    }

    /// Build the inline-target map for `code`'s invoke sites at inlining `depth`. When `require_all`
    /// (a NESTED callee, 16.2 depth>1), EVERY invoke site must inline or the whole map is refused
    /// (None) — an inlined body must fully collapse, never leave a real call mid-body. The top level
    /// (require_all=false) keeps best-effort: a non-inlinable site just stays a normal call.
    pub(crate) fn collect_inline_targets_depth(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
        depth: u8,
        require_all: bool,
        max_frame: usize,
    ) -> std::collections::HashMap<u16, jit::InlineCallee> {
        self.try_collect_inline_targets(code, class, depth, require_all, max_frame)
            .unwrap_or_default()
    }

    /// Loaded receiver classes that resolve to one shared body, within the type cap.
    /// The caller stores their IDs in emitted guards; an unlisted receiver follows
    /// ordinary dispatch. Receiver execution profiles do not select this result.
    #[cfg(feature = "next-gen-gc")]
    fn cha_sole_implementor(
        &mut self,
        class: &Arc<LoadedClass>,
        cp: u16,
        op: u8,
    ) -> Option<Vec<u32>> {
        use crate::vm::inline_census as cen;
        if !cha_devirt_enabled() {
            return None;
        }
        // Copy out of the constant pool first: the closure query needs `&mut self`, and the
        // `MemberRef` borrows `class`.
        let (owner, name, desc) = {
            let m = class.file.pool.member_ref(cp)?;
            (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string())
        };
        cen::note(cen::CHA_ASKED);
        cen::note_cha(if op == 0xb6 {
            cen::CHA_ASK_VIRTUAL
        } else {
            cen::CHA_ASK_INTERFACE
        });
        // Cap 4: "exactly one BODY" is the real question, and a body can be shared by several
        // types. A root like `java/lang/Object` still refuses on the fifth node instead of a walk
        // of the world.
        let Some(subs) = self.loader.concrete_subtypes(&owner, cha_type_cap()) else {
            cen::note(cen::CHA_POLY);
            cen::note_cha(cen::CHA_OVER_CAP);
            // Diagnostic only: distinguish the specific population the old coarse row called
            // "polymorphic" even though more than four receiver types may still share one exact
            // bytecode body. `virtual_dispatch_closure` compares stable `Code` identities over a
            // fixed-point loader epoch and deliberately refuses interface owners, whose lambda
            // implementors are not enumerable. Its answer is counted and never consumed here.
            if cen::cha_stats_enabled()
                && self.loader.concrete_subtypes(&owner, 4).is_none()
                && self
                    .virtual_dispatch_closure(&owner, &name, &desc)
                    .is_some_and(|targets| {
                        if targets.len() != 1 {
                            return false;
                        }
                        self.resolve_method(&targets[0], &name, &desc)
                            .ok()
                            .flatten()
                            .is_some_and(|decl| {
                                decl.file
                                    .method(&name, &desc)
                                    .and_then(|method| method.code.as_ref())
                                    .is_some()
                            })
                    })
            {
                cen::note_cha(cen::CHA_ONE_BODY_GT4);
            }
            return None;
        };
        if subs.is_empty() {
            cen::note(cen::CHA_UNPROVABLE);
            return None;
        }
        // ⭐**SEVERAL TYPES ARE NOT SEVERAL BODIES.** `concrete_subtypes` answers in TYPES, and the
        // first build refused every answer above one — 6,071 sites a run. What the splice actually
        // needs is one BODY: four subclasses that all inherit the same method have one. Resolving
        // each subtype and requiring ONE declaring class is that test, exactly and cheaply
        // (`resolve_method` is thread-local memoised). The extra ids then widen the GUARD, not the
        // body, so the speculation stays guarded and needs no invalidation.
        let mut decl_ptr: Option<usize> = None;
        let mut ids: Vec<u32> = Vec::with_capacity(subs.len());
        for c in subs.iter() {
            let Ok(Some(d)) = self.resolve_method(&c.file.this_class, &name, &desc) else {
                cen::note(cen::CHA_POLY);
                cen::note_cha(cen::CHA_RESOLVE_FAIL);
                return None;
            };
            let p = std::sync::Arc::as_ptr(&d) as usize;
            match decl_ptr {
                None => decl_ptr = Some(p),
                Some(q) if q == p => {}
                Some(_) => {
                    cen::note(cen::CHA_POLY);
                    cen::note_cha(cen::CHA_MULTI_BODY);
                    return None;
                }
            }
            // An arena class id exists only once the arena registry has SEEN the class. A subtype
            // that has never been allocated has no id, and admitting the rest without it would
            // build a guard that silently excludes a live receiver — so refuse the whole site.
            let Some(id) = crate::nextgen::class_id_by_name(&c.file.this_class) else {
                cen::note(cen::CHA_NOID);
                return None;
            };
            ids.push(id);
        }
        cen::note(cen::CHA_HIT);
        if ids.len() > 1 {
            cen::note(cen::CHA_MULTI);
        }
        Some(ids)
    }

    pub(crate) fn try_collect_inline_targets(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
        depth: u8,
        require_all: bool,
        max_frame: usize,
    ) -> Option<std::collections::HashMap<u16, jit::InlineCallee>> {
        let mut out = std::collections::HashMap::new();
        let mut add = |this: &mut Self, cp: u16, ws: bool, rf: bool, out: &mut std::collections::HashMap<u16, jit::InlineCallee>| -> Option<()> {
            match this.inline_target_for_call(class, cp, ws, rf, depth, max_frame) {
                Some(c) => { out.insert(cp, c); Some(()) }
                None => if require_all { None } else { Some(()) },
            }
        };
        for cp in jit::invokestatic_indices(code) {
            cen::note(cen::S_STATIC);
            add(self, cp, true, false, &mut out)?;
        }
        // invokespecial is statically bound (private/super/instance helper) → inlines with NO guard.
        for cp in jit::invokespecial_indices(code) {
            cen::note(cen::S_SPECIAL);
            add(self, cp, false, false, &mut out)?;
        }
        // invokevirtual to a FINAL-effective method is monomorphic-by-type → inline with no guard.
        for (cp, op) in jit::invoke_indices(code) {
            if op == 0xb6 {
                cen::note(cen::S_FINALVIRT);
                add(self, cp, false, true, &mut out)?;
            } else if require_all && matches!(op, 0xb9 | 0xba) {
                return None; // a nested callee with interface/dynamic dispatch can't fully collapse
            }
        }
        // Guarded targets come from resolved hierarchy facts, never receiver history.
        // The emitted guard preserves ordinary dispatch for other receiver classes.
        #[cfg(feature = "next-gen-gc")]
        let deep_devirt = deep_devirt_enabled();
        #[cfg(not(feature = "next-gen-gc"))]
        let deep_devirt = false;
        if devirt_inline_enabled() && (depth == 0 || deep_devirt) && !require_all {
            for (cp, op) in jit::invoke_indices(code) {
                if !matches!(op, 0xb6 | 0xb9) || out.contains_key(&cp) {
                    continue;
                }
                cen::note(cen::S_DEVIRT);
                let Some(ids) = self.cha_sole_implementor(class, cp, op) else {
                    cen::note(cen::X_DEVIRT_NOPROFILE);
                    continue;
                };
                let class_id = ids[0];
                let mut cha_more = ids[1..].to_vec();
                let cha_source = true;
                #[cfg(feature = "next-gen-gc")]
                {
                    let Some(concrete) = crate::nextgen::class_name(class_id) else {
                        cen::note(cen::X_DEVIRT_NAME);
                        continue;
                    };
                    let Some(member) = class.file.pool.member_ref(cp) else {
                        cen::note(cen::X_DEVIRT_NAME);
                        continue;
                    };
                    let (name, desc) =
                        (member.name.to_string(), member.descriptor.to_string());
                    // Resolve against the PROFILED class, not the declared interface: that is
                    // the whole move. `require_final` is off because the guard replaces it.
                    // ⚠The zero-argument restriction that lived here was a BISECTION step, kept
                    // long after the bisect found the real fault elsewhere (a `Declined` outcome
                    // returned as a bare `None`, which an emitter reads as "cannot compile this
                    // method"). Arguments were never the problem: the four shipped inline arms
                    // include `getDouble(I)` and handle them through the same diamond.
                    if let Some(mut callee) = self.inline_target_for_exact_opts(
                        class, &concrete, &name, &desc, false, false, depth, max_frame, None, true,
                        false, true,
                    ) {
                        if cha_source {
                            cen::note_cha(if op == 0xb6 {
                                cen::CHA_INLINABLE_VIRTUAL
                            } else {
                                cen::CHA_INLINABLE_INTERFACE
                            });
                            if op == 0xb6 || crate::jit::devirt_iface_enabled() {
                                cen::note_cha(cen::CHA_EMITTABLE_OPCODE);
                            }
                        }
                        callee.guard_class = Some(class_id);
                        callee.guard_class_more = std::mem::take(&mut cha_more);
                        cen::note(cen::A_DEVIRT);
                        if depth > 0 {
                            cen::note(cen::A_DEEP_DEVIRT);
                        }
                        crate::diag::devirt_admitted().inc();
                        out.insert(cp, callee);
                    } else {
                        cen::note(cen::X_DEVIRT_BODY);
                        crate::diag::devirt_refused().inc();
                    }
                }
            }
        }
        // Static lambda provenance may fill remaining sites without receiver history.
        // Existing descriptor, representation and GC proofs remain authoritative.
        #[cfg(feature = "next-gen-gc")]
        if depth == 0 && !require_all {
            self.apply_lambda_provenance(code, class, &mut out, depth, max_frame);
        }
        Some(out)
    }

    /// Cp indices of invoke sites whose (declared) callee is ALREADY compiled —
    /// such calls take the guarded `jit_direct_call` native path, not the
    /// interpreter, so they count as native work in `callback_heavy`. Lets a
    /// wrapper compile once its hot callees have (callees cross the invocation
    /// threshold before their callers, so the cascade flows bottom-up).
    pub(crate) fn collect_compiled_callee_sites(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashSet<u16> {
        let mut out = std::collections::HashSet::new();
        for (cp, _op) in jit::invoke_indices(code) {
            let Some(member) = class.file.pool.member_ref(cp) else { continue };
            let (owner, name, desc) =
                (member.owner.to_string(), member.name.to_string(), member.descriptor.to_string());
            let Ok(Some(decl)) = self.resolve_method(&owner, &name, &desc) else { continue };
            let Some(method) = decl.file.method(&name, &desc) else { continue };
            if let Some(ccode) = method.code.as_ref() {
                let ckey = ccode as *const Code as usize;
                if matches!(self.jit_cache.get(&ckey), Some(jit::JitStatus::Compiled(_))) {
                    out.insert(cp);
                }
            }
        }
        out
    }

    pub(crate) fn inline_target_for_call(
        &mut self,
        caller: &Arc<LoadedClass>,
        cp: u16,
        want_static: bool,
        require_final: bool,
        depth: u8,
        max_frame: usize,
    ) -> Option<jit::InlineCallee> {
        let member = caller.file.pool.member_ref(cp)?;
        // ⭐**THE RECURSIVE PATH NOW OFFERS WHAT THE OUTER ARMS OFFER.** This forwarder passed
        // `allow_iface_body: false` at every depth, which is why `iface_body_admissible`'s own
        // comment could say the widened arm *"cannot cascade — it is offered only at depth 0"*.
        // That restriction was written as a rooting argument (see `deep_inline_enabled`), and it
        // was ALSO load-bearing for a second reason that is NOT rooting: `field_loads_invariant`
        // stamps the caller's field loads `readonly` whenever every invoke site is in the map,
        // and it excludes an entry only when `leaves_gc_point()` says a real call survives. The
        // widened arm was safe there by ACCIDENT — its only caller set a receiver guard, and a
        // guard makes that predicate true. An UNGUARDED iface-body entry would have been stamped
        // invariant across a live dispatch.
        //
        // So the widening is paid for at the source: the arm below marks every entry it admits
        // `partial_gc_points`, which is the truth about it (an `invokeinterface` survives inside
        // the splice, by construction — the arm requires one). `field_loads_invariant`,
        // `method_no_alloc`, `promo_scan_body` and both partial-splice predicates then refuse it
        // through the predicate they already ask, with no edit to any of those files.
        //
        // `inline_target_for_exact` — the historic entry point used by `hof_fusion` — is left
        // passing `false`, so nothing outside the collection path changes.
        self.inline_target_for_exact_opts(
            caller,
            member.owner.as_ref(),
            member.name.as_ref(),
            member.descriptor.as_ref(),
            want_static,
            require_final,
            depth,
            max_frame,
            None,
            true,
            false,
            {
                #[cfg(feature = "next-gen-gc")]
                {
                    deep_iface_enabled()
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    false
                }
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn inline_target_for_exact(
        &mut self,
        caller: &Arc<LoadedClass>,
        owner: &str,
        name: &str,
        desc: &str,
        want_static: bool,
        require_final: bool,
        depth: u8,
        max_frame: usize,
        hof_sam_pc: Option<usize>,
        allow_hof_wrapper: bool,
    ) -> Option<jit::InlineCallee> {
        // The historical entry point, and its historical answer: a void callee is refused. Kept
        // as a forwarder so `hof_fusion` and every other caller is untouched by this round.
        self.inline_target_for_exact_opts(
            caller,
            owner,
            name,
            desc,
            want_static,
            require_final,
            depth,
            max_frame,
            hof_sam_pc,
            allow_hof_wrapper,
            false,
            false,
        )
    }

    /// `inline_target_for_exact` plus `allow_void`: admit a callee whose descriptor returns void.
    /// Passed `true` by exactly one caller — the lambda SAM chain above — because most functional
    /// interfaces are void (`Consumer.accept`, an `IntLineConsumer`) and the refusal that blocked
    /// them was `emit_inline`'s continuation shape, not a safety property (see
    /// `jit::is_inlinable_kind`). Every other path passes `false` and is byte-identical.
    /// ADMIT a rooted splice for a callee whose NESTED MAP IS ALREADY BUILT (the interface-body
    /// arm, which built one and then had `iface_partial_gc_safe` refuse it). Returns the window
    /// size in slots and the callee's own trivial-`<init>` plans, or `None` to keep the refusal.
    ///
    /// Every condition here is a REJECTION, never a relaxation of somebody else's proof:
    ///   * the round's kill switch, and the stage gate for surviving calls;
    ///   * DEPTH 0 — the window roots the callee and `locals_ptr` roots the caller, and
    ///     `locals_ptr` is the compiled method's own frame only at the outermost level;
    ///   * the straight-line shape walk (`gcpoint_body_shape`);
    ///   * every nested spliced callee GC-leaf, so no deeper frame needs a window;
    ///   * the callee's footprint within the window budget.
    #[cfg(feature = "next-gen-gc")]
    fn gcpoint_admit(
        &mut self,
        mcode: &Code,
        decl: &Arc<LoadedClass>,
        desc: &str,
        allow_void: bool,
        depth: u8,
        nested: &std::collections::HashMap<u16, jit::InlineCallee>,
    ) -> Option<(u16, std::collections::HashMap<u16, jit::CtorInline>)> {
        if !inline_gcpoint_enabled() {
            return None;
        }
        if depth != 0 && !deep_inline_enabled() {
            // OFFERED BUT NOT OUTERMOST. The largest single refusal of this arm, and the one
            // that is structural rather than conservative: rooting a nested splice needs its
            // PARENT's Cranelift variables rooted too, and those live in no buffer.
            //
            // ⭐Under `deep_inline_enabled()` there IS no window: `emit_inline`'s
            // `E_FREE_WINDOW` arm emits none at any depth, and the parent's variables the
            // sentence names are the ones the parent's own `declare_var_needs_stack_map` already
            // put in the maps. `RAVA_NO_DEEP_INLINE=1` reads this line exactly as it did.
            cen::note(cen::X_GP_DEPTH);
            return None;
        }
        // ⛔THE OTHER UNROOTED SIDE EXIT, closed by configuration rather than by opcode.
        // `native_getfield_const_unchecked` (`jit/emit/fields_unchecked.rs`) answers a NULL
        // receiver by calling the NPE callback and RETURNING from the compiled function with no
        // epilogue — the same window leak `gcpoint_body_shape` refuses array access for. It is
        // reachable only when `arena_inline_disabled()` sends `getfield` down that arm instead of
        // the arena dual/flat pair, which have no side exit at all; refusing the whole round in
        // that configuration is one condition instead of banning `getfield`, which is most of
        // this population's body.
        if jit::arena_inline_disabled() {
            return None;
        }
        let footprint = mcode.max_locals as usize + mcode.max_stack as usize;
        if footprint > jit::dispatch_callbuf_slots() {
            cen::note(cen::X_GP_FRAME);
            return None;
        }
        // ⭐**"EVERY NESTED SPLICED CALLEE MUST BE GC-LEAF" IS THE SAME SENTENCE ONE LEVEL DOWN.**
        // Its stated reason is *"a GC point inside a nested splice would need a window for the
        // nested frame"* — and under the maps no frame at any level gets a window. Kept verbatim
        // with the round off. 224 refusals a warm reload, the second-largest of this arm.
        if !gcpoint_nested_gc_leaf(nested) && !deep_inline_enabled() {
            cen::note(cen::X_GP_NESTED);
            return None;
        }
        let ctors = self.build_ctor_inlines(mcode, decl);
        let sh = match gcpoint_body_shape(mcode, desc, allow_void, nested, &ctors, &decl.file.pool) {
            Some(sh) => sh,
            None => {
                cen::note(cen::X_GP_SHAPE);
                return None;
            }
        };
        // Stage 1 is allocation only; stage 2 (a surviving real call inside the splice) is opt-in
        // until it has been measured on its own. The MECHANISM is identical either way.
        if sh.calls > 0 && !inline_gcpoint_calls_enabled() {
            cen::note(cen::X_GP_S2);
            return None;
        }
        if sh.allocs == 0 && sh.calls == 0 {
            // Nothing for the window to root — whatever refused this body, it was not a GC point,
            // so admitting it here would be widening some OTHER rule under this round's name.
            return None;
        }
        Some((footprint.max(1) as u16, ctors))
    }

    /// The same admission for a callee whose nested map has NOT been built — the structural
    /// refusal path. Builds the map (partial: a nested invoke that does not inline stays a real
    /// call, which `gcpoint_body_shape` then counts) and hands it back with the plan.
    #[cfg(feature = "next-gen-gc")]
    fn gcpoint_admit_fresh(
        &mut self,
        mcode: &Code,
        decl: &Arc<LoadedClass>,
        desc: &str,
        allow_void: bool,
        depth: u8,
        max_frame: usize,
    ) -> Option<(
        std::collections::HashMap<u16, jit::InlineCallee>,
        u16,
        std::collections::HashMap<u16, jit::CtorInline>,
    )> {
        // The same depth rule as `gcpoint_admit`, and the same retirement — this is the copy that
        // runs FIRST (it returns before `gcpoint_admit` is ever called), which is why
        // `gcpoint.not-outermost` reads zero on every census: the refusal it counts is
        // unreachable behind this line. Both are lifted together or neither is.
        if !inline_gcpoint_enabled()
            || (depth != 0 && !deep_inline_enabled())
            || depth + 1 >= jit::max_inline_depth()
        {
            return None;
        }
        // CHEAP FIRST. The opcode shape with EMPTY side maps can only OVER-count surviving calls,
        // never turn a refusal into an admission, so a body that fails it here would fail it with
        // the real maps too — and the map builds are then paid only by bodies that can qualify.
        let none_n = std::collections::HashMap::new();
        let none_c = std::collections::HashMap::new();
        gcpoint_body_shape(mcode, desc, allow_void, &none_n, &none_c, &decl.file.pool)?;
        let nested = self.try_collect_inline_targets(mcode, decl, depth + 1, false, max_frame)?;
        let (slots, ctors) = self.gcpoint_admit(mcode, decl, desc, allow_void, depth, &nested)?;
        Some((nested, slots, ctors))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn inline_target_for_exact_opts(
        &mut self,
        caller: &Arc<LoadedClass>,
        owner: &str,
        name: &str,
        desc: &str,
        want_static: bool,
        require_final: bool,
        depth: u8,
        max_frame: usize,
        hof_sam_pc: Option<usize>,
        allow_hof_wrapper: bool,
        allow_void: bool,
        // ADDED: may this callee's body keep a surviving `invokeinterface`? Passed `true` only by
        // the two chains whose entry carries a receiver GUARD (the SAM chain and guarded
        // devirtualisation), never by `inline_target_for_exact`. That is not taste — it is what
        // keeps `jit/analysis/range.rs::field_loads_invariant` correct: it stamps the CALLER's
        // arena field loads `readonly` whenever every invoke site is in `inline_targets`, and it
        // already excludes an entry carrying `guard_class`/`guard_ref_word` precisely because the
        // guarded shape leaves a real call live. An UNGUARDED entry whose spliced body still made
        // an interface dispatch would be stamped invariant across a GC-capable call. Restricting
        // the widening to guarded entries leaves that predicate true by construction, with no edit
        // to the file that asserts it.
        allow_iface_body: bool,
    ) -> Option<jit::InlineCallee> {
        let owner = owner.to_string();
        let name = name.to_string();
        let desc = desc.to_string();
        // Constructors / class-init carry side-effecting super/static chains and are
        // never a pure inlinable leaf — skip without resolving.
        if name == "<init>" || name == "<clinit>" {
            cen::note(cen::X_CTOR);
            return None;
        }
        let Ok(Some(decl)) = self.resolve_method(&owner, &name, &desc) else {
            cen::note(cen::X_RESOLVE);
            return None;
        };
        // JVM §5.5: first execution of a static method initializes its class. An
        // INLINED callee executes without any dispatch, so a not-yet-initialized
        // declaring class would have its <clinit> silently skipped — refuse the
        // inline (the normal invoke path initializes; a later compile of another
        // caller can still inline it once initialized).
        if !decl.initialized.load(std::sync::atomic::Ordering::Relaxed) {
            cen::note(cen::X_UNINIT);
            return None;
        }
        // A fastutil-object-map method must run native ($map) — never inline its
        // bytecode (it would read the never-built internal arrays).
        if opcodes::route_native_map(&decl) {
            cen::note(cen::X_NATIVEMAP);
            return None;
        }
        // A native-served class (guava Immutable*/collection/IO) keeps its state in
        // synthetic fields; inlining its bytecode reads them as null. Keep it on the
        // interpreter's native path (matches jit_direct_resolve's exclusion).
        //
        // A method-wide Compile relaxation is not safe merely because interpreted entry runs the
        // native cascade first: compiled callers can enter the compiled body without that cascade.
        // A profile-use product with only the Compile question relaxed crashed twice on fresh
        // boots; the same binary with RAVA_NO_SPLIT_NATIVE=1 reached Done. Keep this class gate
        // strict until every skipped-dispatch edge preserves native routing for the receiver
        // representation.
        //
        // Inlining is independently unsafe: it skips dispatch and may read synthetic native state
        // as ordinary fields. `jit_native_served_deep` stays strict for both questions.
        // ⛔**AND THE SOUND NARROWING — A RUNTIME RECEIVER GUARD INSTEAD OF THE EMIT-TIME CLASS
        // TEST — CANNOT BE BUILT AT THIS SITE. CHECKED, NOT ASSUMED.** Three reasons, in order
        // of how quickly they close the door:
        //
        //  1. THE GUARD THAT EXISTS PROVES THE WRONG PREDICATE. `InlineCallee` carries exactly
        //     two: `guard_class` (arena class id) and `guard_ref_word` (interned class-name
        //     pointer). Both mean "the receiver's class is exactly C". But the hazard is not the
        //     receiver's CLASS — the refusal already knows the class, that is what it tested —
        //     it is where that instance's STATE lives. `native_backing.rs` records the
        //     counter-example in as many words: *"a method can be interpreted for SOME receivers
        //     while the natives serve it for OTHERS — the two paths are chosen per RECEIVER
        //     REPRESENTATION, not per method"*, demonstrated by `RAVA_ARENA_BIRTH_NS_GUARD`
        //     moving classes between the arena and the handle heap and killing the boot in
        //     `AbstractIntList.toString`. A class-equality guard is satisfied on BOTH sides of
        //     that split, so it licences nothing.
        //
        //  2. ON THE ONE PATH THAT HAS A RECEIVER GUARD, THE GUARD IS ALREADY THE THING BEING
        //     TESTED. The devirt block above calls this function with `owner = &concrete`, the
        //     profiled class the guard will pin. So the emit-time test below is already
        //     evaluated on exactly the class the runtime guard proves — narrowing by that guard
        //     is a no-op by construction. The other three shapes that reach here have no
        //     receiver to guard at all: invokestatic (none), invokespecial and final-virtual
        //     (bound by type, and the test also runs on `decl.file.this_class`, the class that
        //     actually declares the body being spliced).
        //
        //  3. NOTHING HERE CAN EMIT A REPRESENTATION CHECK. The guard that WOULD work — "this
        //     cell is an arena object with a real header, not a handle whose fields the native
        //     layer keeps elsewhere" — is a cell-tag test on the receiver word, and it has to be
        //     emitted with a cold branch to the ordinary dispatch. That is `jit/emit/`'s
        //     surface, not this file's; `InlineCallee` has no field to carry it and this
        //     function has no way to request one. Half-building it (setting `guard_class` and
        //     calling that a representation proof) is precisely the "relaxed the shared
        //     predicate and answered both questions at once" mistake the paragraph above records
        //     crashing.
        //
        // ⇒ THE REFUSAL STANDS AS WRITTEN, and it is the strict namespace test on purpose.
        #[cfg(feature = "next-gen-gc")]
        if (crate::opcodes::native_served_prefix(&owner)
            && !crate::class_loader::has_bytecode(&owner))
            || (crate::opcodes::native_served_prefix(&decl.file.this_class)
                && !crate::class_loader::has_bytecode(&decl.file.this_class))
            || self.jit_native_served_deep(&owner)
            || self.jit_native_served_deep(&decl.file.this_class)
        {
            cen::note(cen::X_NATIVESERVED);
            return None;
        }
        let Some(method) = decl.file.method(&name, &desc) else {
            cen::note(cen::X_RESOLVE);
            return None;
        };
        let is_static = method.access_flags & 0x0008 != 0;
        if is_static != want_static {
            cen::note(cen::X_STATICNESS);
            return None; // static call must bind static; special must bind instance
        }
        // invokevirtual is only monomorphic-by-TYPE — safe to inline WITHOUT a
        // receiver-class guard — when the method can't be overridden: a final
        // method, or any method of a final class. Otherwise a subclass override
        // would make the inlined body wrong, so leave it to the call path.
        if require_final {
            let m_final = method.access_flags & 0x0010 != 0;
            let c_final = decl.file.access_flags & 0x0010 != 0;
            if !(m_final || c_final) {
                cen::note(cen::X_NOTFINAL);
                return None;
            }
        }
        let Some(mcode) = method.code.as_ref() else {
            cen::note(cen::X_NOCODE);
            return None;
        };
        // ⭐**THE TREE BUDGET — THE ONE CEILING A PER-CALLEE CEILING CANNOT EXPRESS.** Asked here,
        // before any nested map is built, so a refused parent costs no recursive work and charges
        // nothing. `RAVA_INLINE_BUDGET` sweeps it; `RAVA_INLINE_DEPTH` (`jit::max_inline_depth`)
        // sweeps the other axis. Inert with the round off.
        if deep_inline_enabled() && budget_would_exceed(mcode.bytecode.len()) {
            cen::note(cen::X_BUDGET);
            return None;
        }
        #[cfg(feature = "next-gen-gc")]
        let hof_fusions = if allow_hof_wrapper {
            self.build_hof_fusions(mcode, &decl, &desc, is_static)
        } else {
            std::collections::HashMap::new()
        };
        #[cfg(feature = "next-gen-gc")]
        let hof_wrapper = jit::is_hof_wrapper_inlinable(mcode, &desc, &hof_fusions);
        #[cfg(not(feature = "next-gen-gc"))]
        let hof_wrapper = false;
        // THE ROOTED SPLICE's two outputs, filled by the arm below and by nothing else. See
        // `gcpoint_body_shape`: `Some(window_slots)` makes `emit_inline` register a frame window
        // for the splice and root the caller across it, and the ctor map is the CALLEE's own
        // (a cp index means something different in the caller's pool).
        #[cfg(feature = "next-gen-gc")]
        let mut splice_roots: Option<u16> = None;
        #[cfg(feature = "next-gen-gc")]
        let mut gp_ctors: std::collections::HashMap<u16, jit::CtorInline> =
            std::collections::HashMap::new();
        // THE FOURTH SHAPE's one output. Set by the NESTED (tier-0 partial-map) arm alone — the
        // only band that admits a body with a surviving GC point while leaving both the guard and
        // the rooted window empty. See `InlineCallee::partial_gc_points`.
        #[cfg(feature = "next-gen-gc")]
        let mut partial_gc_points = false;
        // LEAF (depth-1): no invokes at all. NESTED (16.2 depth>1): the callee may itself invoke, but
        // only invokestatic/special/virtual (is_inlinable's allow_invokes gate), and — built below —
        // every one of those must recursively inline (try_collect_inline_targets require_all), so the
        // spliced body is fully collapsed with no real call left mid-body.
        let nested = if let Some(sam_pc) = hof_sam_pc {
            if !jit::is_hof_consumer_inlinable(mcode, &desc, sam_pc) {
                return None;
            }
            std::collections::HashMap::new()
        } else if cfg!(feature = "next-gen-gc") && hof_wrapper {
            std::collections::HashMap::new()
        } else if jit::is_inlinable_kind(mcode, &desc, false, allow_void) {
            cen::note(cen::A_LEAF);
            std::collections::HashMap::new()
        } else if depth + 1 < jit::max_inline_depth()
            && jit::is_inlinable_kind(mcode, &desc, true, allow_void)
            // 20.4 deep-inline: cap non-leaf callees at INLINE_MAX_BYTECODE so the depth-8 splice
            // can't explode code size (transit codecs / DFU lambdas / getters are all tiny).
            && (!crate::jit_tier0() || mcode.bytecode.len() <= jit::inline_max_bytecode())
        {
            // Deep-inline culprit trace: log the >45-byte callees the raised cap newly admits (the
            // suspects for the ≥50-byte deep-inline SIGSEGV). Gated, inert.
            if mcode.bytecode.len() > 45 && std::env::var_os("RAVA_INLINE_TRACE").is_some() {
                eprintln!(
                    "[INLINE-DEEP>45] {}#{}{} bytes={} depth={}",
                    decl.file.this_class, method.name, desc, mcode.bytecode.len(), depth + 1
                );
            }
            // 20.4 deep-inline (Tier-0): a PARTIAL nested map — a nested invoke that isn't itself
            // inlinable stays a REAL dispatch mid-body (emit_op emits the jit_call), instead of
            // refusing the whole inline. This is the C2-style non-leaf step: collapse the outer
            // call + prologue/epilogue even when the body still makes calls. Default (no Tier-0):
            // require_all — the body must fully collapse (the conservative, proven 16.x behavior).
            // 20.5 PARTIAL-MAP under Tier-0, GC-safe via Static Liveness Avoidance. Collect the partial
            // map (nested invokes that inline; the rest stay real jit_calls), then REFUSE the whole
            // inline unless partial_inline_gc_safe proves no reference is live across any real call
            // (else a GC during that call would collect an un-rooted callee Cranelift-var ref → crash,
            // the 20.4 blocker). The class-context fix (callee class_ptr in emit_inline's b_env) makes
            // the surviving real calls resolve correctly. Default (no Tier-0): require_all full collapse.
            let require_all = !crate::jit_tier0();
            let m = match self.try_collect_inline_targets(mcode, &decl, depth + 1, require_all, max_frame) {
                Some(m) => m,
                None => {
                    cen::note(cen::X_NESTED_NONE);
                    return None;
                }
            };
            let callee_footprint = mcode.max_locals as usize + mcode.max_stack as usize;
            if crate::jit_tier0() {
                match jit::partial_inline_gc_safe(
                    &mcode.bytecode,
                    &m,
                    max_frame,
                    callee_footprint,
                    &decl.file.pool,
                ) {
                    jit::PartialSplice::No => {
                        cen::note(cen::X_GCUNSAFE);
                        // a ref would be live across a real call — not GC-safe to partial-inline
                        return None;
                    }
                    jit::PartialSplice::Admit => {}
                    // ⭐**THE FOURTH SHAPE, RECORDED ON THE ENTRY.** A splice with no guard, no
                    // rooted window and a surviving GC point. `leaves_gc_point()` reports it, so
                    // every consumer that concluded "no GC point survives" from map membership
                    // refuses it — see `InlineCallee::partial_gc_points`.
                    jit::PartialSplice::AdmitGcPoint { free_only } => {
                        #[cfg(feature = "next-gen-gc")]
                        {
                            partial_gc_points = true;
                        }
                        cen::note(cen::A_PF_GCP);
                        // The round's own population: a body at least one historic clause would
                        // have refused. Counted where it is created, and NAMED under
                        // `RAVA_PARTIAL_TRACE` so the report can say which bodies moved.
                        if free_only {
                            cen::note(cen::A_PF_FREE);
                            if std::env::var_os("RAVA_PARTIAL_TRACE").is_some() {
                                eprintln!(
                                    "[PARTIAL-FREE] {}.{}{} bytes={} depth={}",
                                    decl.file.this_class,
                                    name,
                                    desc,
                                    mcode.bytecode.len(),
                                    depth + 1
                                );
                            }
                        }
                    }
                }
            }
            cen::note(cen::A_NESTED);
            m
        } else if allow_iface_body
            && depth + 1 < jit::max_inline_depth()
            && iface_body_admissible(mcode, &desc, allow_void)
        {
            // ⭐**THE WIDENED ARM — see `iface_body_admissible`.** The nested band above refused
            // this callee for exactly one reason: `is_inlinable_kind` returns false the moment it
            // sees a `0xb9`, so no body that dispatches through an interface has ever been
            // spliceable. That is the whole DFU pipeline (`Decoder`, `MapDecoder`, `DynamicOps`,
            // `DataResult`, `App`, `Function`), and it is why a lambda body of eleven bytes is a
            // host crossing here and straight-line code on HotSpot.
            let m = match self.try_collect_inline_targets(mcode, &decl, depth + 1, false, max_frame)
            {
                Some(m) => m,
                None => {
                    cen::note(cen::X_NESTED_NONE);
                    return None;
                }
            };
            let callee_footprint = mcode.max_locals as usize + mcode.max_stack as usize;
            // ⭐**THE ARM'S ENTRIES NOW TELL THE TRUTH ABOUT THEMSELVES.** `iface_body_admissible`
            // REQUIRES at least one `invokeinterface` in the body and refuses every folding
            // opcode, so an entry from this arm always leaves a real dispatch live inside the
            // splice. Until this round that was harmless by accident — the arm's only caller set
            // a receiver guard, and `leaves_gc_point()` already answered `true` for a guarded
            // entry. The recursive path has no guard, so the fact is recorded where it is
            // created instead of being inherited from an unrelated field.
            //
            // Unconditional, in BOTH arms of the round: for the guarded depth-0 population this
            // is a no-op (the guard already made the predicate true), so it changes nothing that
            // exists today and makes the widening sound rather than lucky.
            #[cfg(feature = "next-gen-gc")]
            {
                partial_gc_points = true;
            }
            if iface_partial_gc_safe(&mcode.bytecode, &m, max_frame, callee_footprint) {
                cen::note(cen::A_IFACE);
            } else {
                // ⭐**THE ROOTED SPLICE PICKS UP EXACTLY WHAT THIS PREDICATE JUST REFUSED.**
                // `iface_partial_gc_safe` says no for one reason — a second GC point would leave
                // the caller's references unrooted — and that is the reason the window removes.
                // `DataResult$Success.flatMap` is this branch: two interface calls with a field
                // read between them, monomorphic at hundreds of sites, refused by `real_calls != 1`
                // and by nothing else.
                #[cfg(feature = "next-gen-gc")]
                match self.gcpoint_admit(mcode, &decl, &desc, allow_void, depth, &m) {
                    Some((slots, ctors)) => {
                        splice_roots = Some(slots);
                        gp_ctors = ctors;
                        cen::note(cen::A_GP);
                    }
                    None => return None,
                }
                #[cfg(not(feature = "next-gen-gc"))]
                return None;
            }
            // Counted only once the arm has actually produced an entry — the `else` above can
            // still refuse, and an admission census that counts refusals is worse than none.
            if depth > 0 {
                cen::note(cen::A_DEEP_IFACE);
            }
            // THE CONTROL-FLOW POPULATION, on the same rule: counted where the arm produces an
            // entry, not where the shape gate passes. Split by BACK EDGE so "how much of what the
            // round admits is a loop" is a count and not an estimate. One walk per admitted
            // interface-arm callee, at compile time.
            {
                let (cf, back) = cf_shape(&mcode.bytecode);
                if cf {
                    cen::note(cen::A_IF_CF);
                    if back {
                        cen::note(cen::A_IF_CF_LOOP);
                    }
                    // NAMED under the existing gated trace, for the reason `[PARTIAL-FREE]`
                    // beside it is: a census row says HOW MANY moved and never WHICH, and the
                    // one thing a branchy splice has to be checked against is the body it came
                    // from. Diagnostic only — nothing routes on it.
                    if std::env::var_os("RAVA_INLINE_TRACE").is_some() {
                        eprintln!(
                            "[INLINE-CF] {}.{}{} bytes={} depth={} back_edge={}",
                            decl.file.this_class, name, desc, mcode.bytecode.len(), depth + 1, back
                        );
                    }
                }
            }
            m
        } else if let Some((m, slots, ctors)) =
            self.gcpoint_admit_fresh(mcode, &decl, &desc, allow_void, depth, max_frame)
        {
            // The same arm for every callee the shape walk refuses outright — `shape.new` above
            // all. `emit_inline` gives it a rooted window and the CALLEE's own `<init>` plans, so
            // `new X; dup; …; invokespecial X.<init>` collapses to a TLAB bump plus inline field
            // stores with no callback at all.
            #[cfg(feature = "next-gen-gc")]
            {
                splice_roots = Some(slots);
                gp_ctors = ctors;
            }
            cen::note(cen::A_GP);
            if depth > 0 {
                cen::note(cen::A_DEEP_GP);
            }
            m
        } else {
            // DIAGNOSTIC (RAVA_DBG_INLBAIL): this callee is structurally refused — record WHICH
            // gate did it. Getstatic-Inline showed these gates, not per-op codegen, hold the boot
            // time, so rank the remaining ones by measured blocking instead of by guesswork.
            #[cfg(feature = "next-gen-gc")]
            if jit::inlbail_enabled() {
                if let Some(r) = jit::inlinable_bail_reason(mcode, &desc, true) {
                    jit::inlbail_note(r, &format!("{owner}.{name}{desc}"));
                }
            }
            // ADDED, always on: the same question `RAVA_DBG_INLBAIL` answers, asked in the order
            // that makes the answer actionable (opcode blockers before budgets) and counted
            // without a gate — see `nested_blocker`.
            cen::note_shape(nested_blocker(mcode, &desc, allow_void, depth));
            return None;
        };
        // Pre-resolve the callee's getfield refs against ITS pool (its cp indices
        // differ from the caller's), so the inlined body reads fields natively.
        let Some(fields) = jit::resolve_fields(&mcode.bytecode, &decl.file.pool, &mut |o, n| {
            let id = self.resolve_field_id(o, n);
            let slot = self.layout_slot(o, id);
            (id, slot)
        }) else {
            cen::note(cen::X_FIELDS);
            return None;
        };
        // GETSTATIC-INLINE gate. `is_inlinable` admits getstatic structurally because
        // emit_inline hands the callee body its own class_ptr + an EMPTY statics map, so the
        // read lands on the generic rava_getstatic callback and resolves (callee_class_ptr,
        // callee_cp) against the CALLEE's pool. What it CANNOT check is VM state, and that is
        // where the only real hazard lives: that callback runs opcodes::getstatic, which runs
        // <clinit> — arbitrary Java, hence a moving GC. A spliced body spills only the CALLEE's
        // vars into the shared frame buffer, so the CALLER's live refs would go unrooted across
        // it (the 20.4 crash class). Class init is MONOTONIC, so proving the read's class is
        // already initialized here proves the callback can never run <clinit> from this site
        // later ⇒ it cannot GC ⇒ the splice is safe.
        // `clinit_done`, NOT `initialized`: the latter latches when <clinit> STARTS (to break
        // cycles), and under the trampoline there is a window where it is true while the body
        // is still pending — exactly the window that could still run Java from this read.
        // Conservative on inheritance: the field must be declared DIRECTLY in the named owner.
        // ensure_init initializes superclasses first (JVMS 5.5), so an owner would cover them,
        // but JVMS 5.5 does NOT initialize INTERFACES via an implementing class — an inherited
        // static could resolve into a still-uninitialized interface. Refuse rather than reason.
        let Some(gs_refs) = jit::getstatic_refs(&mcode.bytecode, &decl.file.pool) else {
            cen::note(cen::X_GETSTATIC);
            return None;
        };
        for (fowner, fname) in gs_refs {
            let Ok(fcls) = self.loader.get_or_load(fowner) else {
                cen::note(cen::X_GETSTATIC);
                return None;
            };
            let declared_static = fcls
                .file
                .fields
                .iter()
                .any(|f| f.name == fname && f.access_flags & 0x0008 != 0);
            if !(fcls.clinit_done.load(std::sync::atomic::Ordering::Relaxed) && declared_static) {
                cen::note(cen::X_GETSTATIC);
                return None;
            }
        }
        // 16.1 observability: this call is now confirmed inlinable → emit_inline will splice it.
        #[cfg(feature = "next-gen-gc")]
        if jit::jit_inliner_log_enabled() {
            let tag = if nested.is_empty() { "leaf" } else { "chain" };
            jit::jit_inline_log(&caller.file.this_class, &format!("{owner}::{name}{desc} [{tag} d{depth}]"));
        }
        // ⭐**THE ONE CHOKE POINT EVERY ARM RETURNS THROUGH.** Leaf, nested, widened-interface,
        // rooted-splice, guarded-devirt and the two SAM chains all reach a callee entry here, so
        // one note bucketed by depth is the whole admitted population — the number that is
        // directly comparable with `-XX:+PrintInlining`'s own histogram. Compile time only.
        cen::note_depth(depth);
        if deep_inline_enabled() {
            budget_charge(mcode.bytecode.len());
        }
        Some(jit::InlineCallee {
            guard_class: None,
            guard_class_more: Vec::new(),
            guard_ref_word: None,
            class: Arc::clone(&decl),
            code_key: mcode as *const Code as usize,
            bytecode: mcode.bytecode.clone(),
            max_locals: mcode.max_locals,
            descriptor: desc,
            is_static,
            nested,
            #[cfg(feature = "next-gen-gc")]
            hof_fusions,
            fields,
            // Filled by the jit_run driver (reserve/rollback protocol) under
            // RAVA_REF_STATIC_CELLS; empty = every inlined getstatic stays on
            // the callback (the historical behavior).
            statics: std::collections::HashMap::new(),
            // Set by the lambda SAM chain only, immediately after this returns. Everything else
            // leaves it `None`, which is what makes every existing site keep its exact shape.
            #[cfg(feature = "next-gen-gc")]
            sam: None,
            #[cfg(feature = "next-gen-gc")]
            splice_roots,
            #[cfg(feature = "next-gen-gc")]
            ctor_inlines: gp_ctors,
            #[cfg(feature = "next-gen-gc")]
            partial_gc_points,
        })
    }

    pub(crate) fn collect_static_intrinsics(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> std::collections::HashMap<u16, jit::StaticIntrinsic> {
        let mut out = std::collections::HashMap::new();
        let mut memo = std::collections::HashMap::new();
        let mut visiting = std::collections::HashSet::new();
        for cp in jit::invokestatic_indices(code) {
            if let Some(intrinsic) =
                self.static_intrinsic_for_call(class, cp, &mut memo, &mut visiting)
            {
                out.insert(cp, intrinsic);
            }
        }
        out
    }

    pub(crate) fn static_intrinsic_for_call(
        &mut self,
        caller: &Arc<LoadedClass>,
        cp: u16,
        memo: &mut std::collections::HashMap<String, Option<jit::StaticIntrinsic>>,
        visiting: &mut std::collections::HashSet<String>,
    ) -> Option<jit::StaticIntrinsic> {
        let member = caller.file.pool.member_ref(cp)?;
        let owner = member.owner.to_string();
        let name = member.name.to_string();
        let desc = member.descriptor.to_string();
        let decl = self.resolve_method(&owner, &name, &desc).ok()??;
        let key = format!("{}\0{}\0{}", decl.file.this_class, name, desc);
        if let Some(hit) = memo.get(&key) {
            return *hit;
        }
        if !visiting.insert(key.clone()) {
            return None;
        }

        let Some(method) = decl.file.method(&name, &desc) else {
            visiting.remove(&key);
            memo.insert(key, None);
            return None;
        };
        if method.access_flags & 0x0008 == 0 {
            visiting.remove(&key);
            memo.insert(key, None);
            return None;
        }
        let Some(code) = method.code.as_ref() else {
            visiting.remove(&key);
            memo.insert(key, None);
            return None;
        };
        let nested_owner = Arc::clone(&decl);
        let detected =
            jit::detect_static_intrinsic(code, &desc, &decl.file.pool, &mut |nested_cp| {
                self.static_intrinsic_for_call(&nested_owner, nested_cp, memo, visiting)
            });
        visiting.remove(&key);
        memo.insert(key, detected);
        detected
    }
}


/// How many concrete loaded subtypes a CHA answer may span before the site is refused. Every one
/// of them costs a compare in the emitted guard, so this is a code-size knob as much as a
/// coverage one.
const CHA_TYPE_CAP_DEFAULT: usize = 4;

/// `RAVA_CHA_TYPE_CAP=<n>` — tunable so the coverage/code-size trade can be bisected inside ONE
/// binary. `1` restores the shipped "sole implementor" behaviour exactly.
fn cha_type_cap() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RAVA_CHA_TYPE_CAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &usize| n >= 1)
            .unwrap_or(CHA_TYPE_CAP_DEFAULT)
    })
}

/// ✅**DEFAULT ON** (`RAVA_NO_CHA_DEVIRT=1` disables) — bind a virtual or interface site from the
/// LOADED HIERARCHY when the receiver profile is empty.
///
/// Priced on both workloads after the index was made incremental. Boot, two interleaved runs of
/// 3 and 4 rounds: CPU median **−2.82%** and **−3.55%**, min −7.72% and −0.30%. Warm reload, two
/// runs: `RELOAD2_MS` **−4.83/−2.78%** then **−2.46/−3.12%**. Not disjoint on a box that spent the
/// evening entering and leaving episodes, so this ships on the same terms as guarded devirt itself
/// — every reading favourable, correctness exact, and no reading against it.
///
/// ⚠The gate is read through the SAME function everywhere. Today's other default flip failed
/// because two derivations of one fact disagreed exactly when the variable was unset
/// (`frames.rs:877`), so the unset path is the one that gets verified.
fn cha_devirt_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_CHA_DEVIRT").is_none())
}

pub(crate) fn devirt_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DEVIRT").is_none())
}


// ---------------------------------------------------------------------------
// THE WIDENED ARM: a spliceable body may keep ONE surviving interface dispatch.
// ---------------------------------------------------------------------------
//
// ⭐**WHAT THE OLD RULE COST, AND WHY IT WAS NEVER A BUDGET QUESTION.** `is_inlinable_kind`
// (`jit/analysis/admission.rs`) returns false at the first `0xb9` in a callee, in BOTH bands. In
// the leaf band that is definitional — a leaf has no invoke at all. In the NESTED band it is the
// single rule that closes the door on this workload, because the decode pipeline this engine
// spends its warm reload in is interface-typed end to end: `Decoder`, `MapDecoder`, `DynamicOps`,
// `MapLike`, `DataResult`, `App`, and above all `java/util/function/Function`. So EVERY callee in
// that pipeline invokes through an interface, and every one of them was refused for that alone —
// including bodies of eleven bytes:
//
// ```text
// DataResult$Success.mapOrElse(Function,Function)  11 bytes
//   0 aload_1 ; 1 aload_0 ; 2 getfield value ; 5 invokeinterface Function.apply ; 10 areturn
// ```
//
// Nothing about that body is unsafe to splice. It has no branch, no allocation, no field write,
// no getstatic, and after its one call it does nothing but return the result. It was refused
// because the opcode of that call is `0xb9` rather than `0xb6`, and for no other reason — the
// nested band ALREADY ships bodies of exactly this shape when the surviving call is a virtual or
// static one (`partial_inline_gc_safe`, tier-0 partial maps, default ON).
//
// ⛔**AND THE REFUSAL WAS NOT MERELY CONSERVATIVE — IT GUARDED A REAL HAZARD, WHICH IS WHY THIS
// ARM BRINGS ITS OWN PROOF INSTEAD OF RELAXING THE OLD ONE.** `partial_inline_gc_safe` counts
// surviving real calls over `0xb6 | 0xb7 | 0xb8` ONLY. Admitting a `0xb9` into the band it
// governs would make a body carrying a live interface dispatch read as *fully collapsed*
// (`real_calls == 0`), which returns `true` immediately and skips every liveness rule below it —
// the 20.4 crash class, exactly as `admission.rs` records it. So this arm never enters that band:
// it is a separate `else if`, it never calls that predicate, and `iface_partial_gc_safe` below
// re-derives the same liveness argument over `0xb6 | 0xb7 | 0xb8 | 0xb9`.
//
// THE THREE CONTAINMENTS, each answering a specific thing this file cannot see:
//
//  1. **Guarded entries only** (`allow_iface_body`). `jit/analysis/range.rs::field_loads_invariant`
//     stamps the CALLER's arena field loads `readonly` when every invoke site of the caller is in
//     `inline_targets`, and it already refuses an entry carrying `guard_class`/`guard_ref_word`
//     because that shape leaves a real call live on the miss arm. Only the SAM chain and guarded
//     devirtualisation pass `allow_iface_body = true`, so every entry this arm produces is already
//     refused there. The invariant stays true with no edit to the file that asserts it.
//  2. **Depth 0 only, by construction.** The recursive collection below passes through
//     `inline_target_for_call` then `inline_target_for_exact`, which passes `false`. A nested
//     callee therefore never gets the widened body, so the splice cannot cascade: the widening
//     adds at most one interface dispatch to a caller, at a site that already had one.
//  3. **One surviving call, forwarding tail** (`iface_partial_gc_safe`), and `getstatic` refused
//     outright rather than only in combination with a surviving call. ⚠**The "straight-line"
//     third of this containment is GONE** — see `inline_cf_enabled`. Both halves of the liveness
//     argument it served were retired by `inline_free_enabled()` in the very predicate named
//     here, and `emit_inline` has emitted branchy splices since V4.2; what is left is the size
//     ceiling, the frame bound, the getstatic refusal and the decoder refusal of
//     switch/jsr/goto_w.
//
// Kill switch: `RAVA_NO_CODEC_INLINE=1` restores the previous admission exactly — the arm is an
// `else if` after the historic ones, so with it off no callee reaches it and every earlier verdict
// is byte-identical.

/// `RAVA_NO_CODEC_INLINE=1` restores the historic admission. Resolved once; also ANDed with the
/// global emit-more bisect gate, like every other widening in this tree.
pub(crate) fn codec_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_CODEC_INLINE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The structural half of the widened arm: is this body one the splice can even shape? Mirrors
/// `is_inlinable_kind`'s nested band with two differences, one looser and one tighter:
///   * `0xb9 invokeinterface` is ALLOWED (and at least one must be present — a body without one
///     was refused by the nested band for some other reason, and this arm must not silently
///     re-admit it under a different set of rules);
///   * ⭐**control flow is now ADMITTED** (`inline_cf_enabled`, kill switch `RAVA_NO_INLINE_CF=1`).
///     The historic text here read *"every branch opcode is REFUSED … the liveness argument in
///     `iface_partial_gc_safe` needs the single-linear-path premise unconditionally"* — and that
///     argument is the one `inline_free_enabled()` retired inside that very predicate, because
///     the stack maps name every root at every safepoint in every block. `emit_inline` never
///     needed the premise at all: its general path has built the callee's basic blocks, threaded
///     the operand stack across each edge as block parameters and joined every return in one
///     `cont` block since V4.2. What stays refused — `tableswitch`/`lookupswitch`/`jsr`/`ret`/
///     `goto_w`/`jsr_w` — is refused for a DECODER reason and is counted separately
///     (`X_IF_CF_UNDEC`). Back edges are admitted; the data-parallel analyzers stay irrelevant
///     for the reason the arm doc gives (every shape they detect is a counted arithmetic loop and
///     this arm requires an `invokeinterface` in the body).
///
/// `getstatic` is refused outright (its callback can run a `<clinit>`, i.e. a GC, and a surviving
/// real call is already the one GC point this shape is allowed).
pub(crate) fn iface_body_admissible(code: &Code, desc: &str, allow_void: bool) -> bool {
    let ok = iface_body_shape(code, desc, allow_void);
    // ⭐**THE `iface.control-flow` ROW WAS A COUNT OF OFFERS, NOT OF BODIES, AND LIFTING THE GATE
    // DOES NOT TURN IT INTO ADMISSIONS.** Split the branchy offers here — the one place that sees
    // every one of them and the verdict — so the next round reads WHY the headline shrinks
    // instead of guessing. Compile time only, and only while the round is live.
    #[cfg(feature = "next-gen-gc")]
    if codec_inline_enabled() && inline_cf_enabled() && cf_shape(&code.bytecode).0 {
        use crate::vm::inline_census as cen;
        cen::note(if ok { cen::A_IF_CF_SHAPE } else { cen::X_IF_CF_OTHER });
    }
    ok
}

/// The shape walk itself. Split out only so the census above can see one verdict per offer
/// instead of being transcribed into every `return false` below.
fn iface_body_shape(code: &Code, desc: &str, allow_void: bool) -> bool {
    use crate::vm::inline_census as cen;
    if !codec_inline_enabled() {
        cen::note(cen::X_IF_OFF);
        return false;
    }
    if !code.exceptions.is_empty() {
        return false;
    }
    let bc = &code.bytecode;
    // The nested band's budget, unchanged: `inline_size_admitted(_, true)` is `len <= 96`, and
    // tier-0 additionally caps a non-leaf splice at `inline_max_bytecode()`. This arm inherits
    // both rather than opening a second front on code size.
    // ⭐**THE 96-BYTE NON-LEAF BUDGET IS KEPT; THE TIER-0 35-BYTE SUB-CAP IS NOT, AND THE REASON
    // THAT CAP EXISTS DOES NOT APPLY HERE.** `INLINE_MAX_BYTECODE = 35` was set by a measurement
    // about CASCADE: raising it regressed a boot monotonically (86.9 -> 93.0 s) because depth-8
    // splicing turned `ShiftedNoise.compute` into 852 blocks for four lines of Java. This arm
    // cannot cascade — it is offered only at depth 0 (`allow_iface_body` is `false` on every
    // recursive path) and its body is straight-line with exactly one surviving call, so its code
    // growth is `bc.len()` at one site, once, with no multiplication. The 96-byte ceiling that
    // bounds every other non-leaf callee still binds.
    if bc.is_empty() || bc.len() > jit::INLINE_LEAF_BYTECODE {
        cen::note(cen::X_IF_SIZE);
        return false;
    }
    if jit::ret_jt(desc).is_none() && !allow_void {
        return false;
    }
    let mut saw_iface = false;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if jit::is_excluded(op) {
            return false;
        }
        let Some(len) = jit::insn_len_at(bc, pc) else { return false };
        match op {
            // ⛔**SWITCHES, `jsr`/`ret` AND THE WIDE GOTOS STAY REFUSED, AND NOT BECAUSE OF A
            // PROOF — BECAUSE OF A DECODER.** `emit_inline` walks a spliced body with
            // `insn_len(op)`, which has no arm for `0xaa`/`0xab` (variable-length payload),
            // `0xa8`/`0xa9` (`jsr`/`ret`) or `0xc8`/`0xc9` (`goto_w`/`jsr_w`). Its `None`
            // propagates out through `emit_op`'s `?`, so such a callee does not merely fail to
            // inline: THE WHOLE CALLER FAILS TO COMPILE and falls back to the interpreter.
            // `is_inlinable_kind` records this same landmine, in the same words, for `0xaa|0xab`.
            0xa8..=0xab | 0xc8 | 0xc9 => {
                cen::note(cen::X_IF_CF_UNDEC);
                return false;
            }
            // ⭐**THE REST OF THE CONTROL FLOW IS ADMITTED — THE STRAIGHT-LINE PREMISE WAS THE
            // LAST SURVIVING CLAUSE OF A RETIRED PROOF.** The doc above justified this refusal by
            // `iface_partial_gc_safe` needing "the single-linear-path premise unconditionally";
            // that predicate's own `inline_free_enabled()` arm has since retired the analysis the
            // premise served (`real_calls != 1`, the tail walk), because the stack maps name every
            // root at every safepoint in every block. Nothing in `emit_inline` ever needed it:
            // its general path builds the callee's basic blocks from `find_leaders` in the
            // CALLEE's own pc space, threads the operand stack across each edge as block
            // parameters (`jump_to`/`brif_to`), and joins every return in one `cont` block whose
            // single parameter is the return value (none for `void`). That path has been live
            // since V4.2 and the ordinary nested band exercises it today.
            //
            // BACK EDGES ARE ADMITTED TOO, deliberately. A splice emits each opcode of the callee
            // ONCE whether or not an edge returns to it — a loop is a back edge in the IR, never
            // an unrolling — so the emitted size is still bounded by the `INLINE_LEAF_BYTECODE`
            // ceiling this arm already checks, with no multiplication. The data-parallel
            // analyzers `is_inlinable_kind` defers to are irrelevant here for the reason the arm
            // doc already gives: every shape they detect is a counted arithmetic loop, and this
            // arm REQUIRES an `invokeinterface` in the body, which none of them match.
            0x99..=0xa7 | 0xc6 | 0xc7 => {
                if !inline_cf_enabled() {
                    cen::note(cen::X_IF_BRANCH);
                    return false;
                }
            }
            // The opcode list `is_inlinable_kind` refuses for invariants asserted outside this
            // file — putfield/putstatic (heap writes), every allocation, instanceof (spills and
            // can load a class), array stores, invokedynamic (needs a bootstrap the inliner
            // cannot perform) — plus getstatic, refused here for the reason above.
            0xb2 | 0xb3 | 0xb5 | 0xba | 0xbb..=0xbd | 0xc1 | 0x4f..=0x56 => {
                cen::note(cen::X_IF_SHAPE);
                return false;
            }
            0xb9 => saw_iface = true,
            _ => {}
        }
        pc += len;
    }
    if !saw_iface {
        return false;
    }
    jit::find_leaders(bc).is_some()
}

/// The liveness half. `partial_inline_gc_safe`'s argument, re-derived over the FOUR invoke opcodes
/// instead of three, and without the relaxations that predicate grants a fully-collapsed body.
///
/// The hazard is exact: `emit_inline` splices a callee by declaring its locals as fresh Cranelift
/// `Variable`s and running its body through the CALLER's `locals_ptr`, so a `spill_for_gc` inside
/// the spliced body writes the CALLEE's vars from offset 0 of the caller's frame buffer. The
/// caller's own live references are not rooted across it. The only way a surviving GC-capable call
/// is safe is if nothing of the callee is live across it and nothing after it reads state — which
/// is what the tail rule proves.
pub(crate) fn iface_partial_gc_safe(
    bc: &[u8],
    inlined: &std::collections::HashMap<u16, jit::InlineCallee>,
    max_frame: usize,
    callee_footprint: usize,
) -> bool {
    use crate::vm::inline_census as cen;
    let mut real_calls = 0usize;
    let mut last_real_pc = 0usize;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = jit::insn_len_at(bc, pc) else { return false };
        if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) {
            if pc + 2 >= bc.len() {
                return false;
            }
            let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            // No `math_minmax_noncall` shortcut here: an intrinsic'd static would only ever REDUCE
            // this count, and this arm is not trying to serve that population.
            // As `partial_inline_gc_safe`: membership is not a fold when the entry itself
            // leaves a GC point live (`InlineCallee::leaves_gc_point`).
            let folded = matches!(inlined.get(&cp), Some(t) if !t.leaves_gc_point());
            if !folded {
                real_calls += 1;
                last_real_pc = pc;
            }
        }
        pc += len;
    }
    // ⭐**BOTH RULES BELOW ARE ONE ROOTING PROOF, AND `inline_free_enabled()` IS ITS RETIREMENT.**
    // "Exactly one surviving call" and "nothing after it reads state" are not two facts about this
    // body — they are the two halves of a single argument about the SHADOW FRAME: a second GC
    // point, or a read after the first one, would touch a caller reference that the splice's
    // `spill_for_gc` had overwritten with the callee's. Under the stack maps the caller's locals
    // and the caller's operand stack are both named at every safepoint inside the splice
    // (`emit_inline`'s `declare_stack_roots`), and the collector forwards them in place — so
    // neither half has a subject any more. With `RAVA_NO_INLINE_FREE=1`, or with the maps off,
    // this reads exactly as it did.
    //
    // `DataResult$Success.flatMap` is the whole point: two `invokeinterface`s with a field read
    // between them, refused by `real_calls != 1` and by nothing else.
    let free = inline_free_enabled();
    if real_calls != 1 && !free {
        cen::note(cen::X_IF_CALLS);
        return false;
    }
    // KEPT, and NOT rooting-derived in the sense above: this is the frame-buffer CAPACITY bound
    // that every self-registering callee is admitted against, so it stays a bound on the shape
    // rather than on the proof.
    if callee_footprint > max_frame {
        cen::note(cen::X_IF_FRAME);
        return false;
    }
    if free {
        if real_calls != 1 {
            cen::note(cen::A_IF_MULTI);
        }
        return true;
    }
    // TAIL: after the single surviving call, only ops that forward its result. Anything that reads
    // a local, a field or the heap would be reading state a GC inside that call may have moved.
    let Some(call_len) = jit::insn_len_at(bc, last_real_pc) else { return false };
    let mut pc = last_real_pc + call_len;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = jit::insn_len_at(bc, pc) else { return false };
        match op {
            0xc0 => {}        // checkcast (operates on the result already on the stack)
            0x85..=0x93 => {} // numeric conversions of the result
            0xac..=0xb1 => {} // ireturn..areturn, and `return` for a void SAM body
            0x57 => {}        // pop — a void SAM whose one call returns a value it discards
            _ => {
                cen::note(cen::X_IF_TAIL);
                return false;
            }
        }
        pc += len;
    }
    true
}

// ---------------------------------------------------------------------------
// THE ROOTED SPLICE: one mechanism for two refusals.
// ---------------------------------------------------------------------------
//
// ⭐**`shape.new` AND `iface.calls!=1` ARE THE SAME RULE WRITTEN TWICE.** An allocation is a GC
// point and a surviving call is a GC point, and both are refused for one reason, stated verbatim
// by `iface_partial_gc_safe` and again by `partial_inline_gc_safe`: *"emit_inline splices a callee
// by declaring its locals as fresh Cranelift `Variable`s and running its body through the CALLER's
// `locals_ptr`, so a `spill_for_gc` inside the spliced body writes the CALLEE's vars from offset 0
// of the caller's frame buffer. The caller's own live references are not rooted across it."*
//
// So the cure is not two relaxations. It is to give the spliced region the rooting an ordinary
// compiled call already performs — a frame window of its own, registered with the collector for
// the duration of the splice, and the caller's frame spilled into its own buffer across it. That
// is `emit_inline`'s `splice_roots` arm; this file only decides WHICH callees may ask for it.
//
// The shape below is deliberately narrower than either historic arm, because every relaxation
// here is paid for by a rooting proof:
//   * STRAIGHT-LINE, one terminal return. The window is entered once and left once; nothing
//     branches around the registration or the caller's reload.
//   * OUTERMOST ONLY. The window roots the callee's frame and `locals_ptr` roots the CALLER's —
//     and `locals_ptr` is the compiled method's own frame only at depth 0. A splice nested inside
//     another splice would need its parent's Cranelift `Variable`s rooted too, and those live in
//     no buffer at all. Hence `allow_iface_body` (already false on every recursive path) gates
//     this arm exactly as it gates the interface-body one.
//   * EVERY NESTED SPLICED CALLEE MUST BE GC-LEAF (`jit::is_gc_leaf`). The same argument one
//     level down: a GC point inside a nested splice would need a window for the nested frame.
//     `is_gc_leaf` refuses every invoke, allocation, `ldc` of a reference, static access,
//     `instanceof`, `athrow` and monitor op, so a nested callee that passes it has no GC point
//     anywhere and needs no window.
//   * THE FRAME MUST FIT THE WINDOW BUDGET (`dispatch_callbuf_slots()`), the same bound a
//     self-registering callee's own `frame_len` is admitted against.
//
// What is NOT proved here and is proved at emit time instead: that the cp-indexed side maps a
// spliced body could read (`new_arena`, `ctor_inlines`, `static_intrinsics`, `array_fields`) are
// the CALLEE's and not the caller's. See `emit_inline`.

/// `RAVA_NO_INLINE_GCPOINT=1` restores the historic admission and the historic emission. Resolved
/// once, and ANDed with the global emit-more bisect gate like every other widening in this tree.
pub(crate) fn inline_gcpoint_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_INLINE_GCPOINT").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐⭐**THE ROOTING PROOFS HAVE NO SUBJECT LEFT: `RAVA_NO_INLINE_FREE=1` PUTS THEM BACK.**
///
/// Every refusal this round relaxes states ONE reason, and all of them state it in the same words
/// (`iface_partial_gc_safe`, `partial_inline_gc_safe`, `gcpoint_body_shape`, `emit_inline`'s
/// window): *"`emit_inline` splices a callee by declaring its locals as fresh Cranelift
/// `Variable`s and running its body through the CALLER's `locals_ptr`, so a `spill_for_gc` inside
/// the spliced body writes the CALLEE's vars from offset 0 of the caller's frame buffer. The
/// caller's own live references are not rooted across it."*
///
/// That sentence describes the SHADOW FRAME, and under
/// `jit::runtime::stackmaps::declaring()` the shadow frame is not emitted at all. What roots a
/// compiled frame now is a precise stack map per safepoint, and a map does not care whose
/// `locals_ptr` a body was handed:
///   * the CALLER's locals are Cranelift `Variable`s declared once at creation (`emit::body`), so
///     they are named at every safepoint in the function, including the ones a splice introduces;
///   * an inlined CALLEE's locals are declared the same way at creation (`emit::inline`);
///   * every ref OPERAND-STACK value is declared at `spill_for_gc`, the one choke point all ~40
///     GC-capable emitters share, and inside a spliced body that call names the CALLEE's stack;
///   * which leaves exactly ONE hole, and it is the only thing this round has to ADD rather than
///     remove: the CALLER's own ref operand-stack values at the splice point. `emit_inline`
///     declares them (`gcroots::declare_stack_roots`) — a declaration, not a store, so it emits
///     no instruction.
///
/// ⚠**ASK, DO NOT RE-DERIVE.** The environment variable is read here for THIS round's kill
/// switch only; whether the stack-map protocol is active is asked of `stackmaps::declaring()` on
/// every call and cached nowhere. `frames.rs` re-derived that fact from the raw variable once and
/// the engine ran with nothing rooted at all.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inline_free_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let flag = *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_INLINE_FREE").is_none() && !crate::flags::emit_more_disabled()
    });
    flag && crate::jit::runtime::stackmaps::declaring()
}

/// Default build: no arena, no stack maps, no rooted splice — the round is unreachable.
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn inline_free_enabled() -> bool {
    false
}

/// ⭐⭐**THE SAME RETIREMENT, ONE BAND OVER: `RAVA_NO_PARTIAL_FREE=1` PUTS THE PROOFS BACK.**
///
/// `iface_partial_gc_safe` and `partial_inline_gc_safe` were written from one sentence, and
/// `inline_free_enabled` retired that sentence for the first of them. This is the second.
/// `partial_inline_gc_safe` states it four times over — for `getstatic`, for control flow, for
/// `real_calls != 1` and for its tail walk — each time as some form of *"the CALLER's live refs
/// are NOT rooted across it, because `emit_inline` spills the callee's `bvars` from offset 0 of
/// the SHARED frame buffer"*. Under `stackmaps::declaring()` there is no shared frame buffer in
/// play: nothing is spilled anywhere, the caller's locals and the callee's locals are Cranelift
/// `Variable`s declared at creation, and the one root a splice's safepoints did not already name
/// — the CALLER's ref operand-stack values — is declared by `emit_inline` for EVERY splice
/// (`gcroots::declare_stack_roots`, zero instructions), not only for the interface arm.
///
/// ⚠**WHAT THIS DOES NOT LICENSE.** It discharges the ROOTING proof and nothing else. A
/// surviving call still moves and mutates the heap, so every consumer that concluded "no GC point
/// survives" from map membership must still refuse — which is why the band marks its entries
/// `InlineCallee::partial_gc_points` rather than hiding them. See that field.
///
/// ⚠**ASK, DO NOT RE-DERIVE.** The environment variable is this round's kill switch and nothing
/// else; whether the protocol is active is asked of `stackmaps::declaring()` on every call and
/// cached in no second place. `frames.rs` re-derived that fact from the raw variable once and the
/// engine ran with nothing rooted at all.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn partial_free_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let flag = *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_PARTIAL_FREE").is_none() && !crate::flags::emit_more_disabled()
    });
    flag && crate::jit::runtime::stackmaps::declaring()
}

/// Default build: no arena, no stack maps — the round is unreachable.
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn partial_free_enabled() -> bool {
    false
}

/// ⭐⭐**`iface.control-flow` — THE LARGEST REFUSAL IN THE CENSUS — HAD NO SUBJECT IN THE
/// EMITTER. `RAVA_NO_INLINE_CF=1` PUTS IT BACK.**
///
/// `emit_inline` does not require a straight line and never has. Its general path
/// (`jit/emit/inline.rs`, the `find_leaders` block after the fast-path `return`) creates one
/// Cranelift block per callee basic block in the CALLEE's own pc space, threads the operand stack
/// across every edge as block parameters (`support/stack.rs::ensure_block_shape`/`jump_to`/
/// `brif_to`), and joins every `*return` in one `cont` block carrying the result as its single
/// parameter — no parameter at all for a `void` body. The straight-line test above it
/// (`straight_line_return`) is a FAST PATH whose own comment says it "changes no emitted
/// instruction"; it exists to keep a getter from costing a phi that `regalloc2` must colour.
///
/// The requirement lived entirely in `iface_body_admissible`, and its stated reason was
/// `iface_partial_gc_safe`'s *"single-linear-path premise"* — the premise under which that
/// predicate's `real_calls != 1` and tail-walk clauses reasoned about which references are live
/// across the one surviving call. `inline_free_enabled()` retired those clauses: under
/// `stackmaps::declaring()` there is no liveness analysis left to keep sound, because every root
/// — the caller's locals, the callee's locals, both operand stacks — is NAMED at every safepoint
/// in every block, and `emit_inline` declares the one that no other declaration reaches (the
/// caller's ref operand stack at the splice point). The straight-line rule was the last clause of
/// that argument still standing, kept only because it lived in a different `match` arm.
///
/// ⚠**ASK, DO NOT RE-DERIVE.** The environment variable is this round's kill switch and nothing
/// else; whether the protocol is active is asked of `stackmaps::declaring()` on every call and
/// cached in no second place. `frames.rs` re-derived that fact from the raw variable once and the
/// engine ran with nothing rooted at all.
///
/// ⛔**AND IT IS ANDed WITH `inline_free_enabled()`, NOT ONLY WITH THE PROTOCOL.** The relaxation
/// rests on TWO facts and only one of them is unconditional. `emit_inline` can emit a branchy
/// splice in every configuration; but the liveness argument the straight-line premise served —
/// `iface_partial_gc_safe`'s `real_calls != 1` and its linear TAIL walk — is still evaluated when
/// `RAVA_NO_INLINE_FREE=1` puts the shadow-frame proofs back, and that walk reads the body in
/// LINEAR order. Handing it a body whose edges leave that order would be asking a straight-line
/// question about a graph. So the switch that retires the proof is the switch that licences this,
/// and it is ASKED here rather than re-derived (it asks `stackmaps::declaring()` itself, on every
/// call, cached nowhere).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inline_cf_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let flag = *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_INLINE_CF").is_none() && !crate::flags::emit_more_disabled()
    });
    flag && crate::jit::runtime::stackmaps::declaring() && inline_free_enabled()
}

/// Default build: no arena, no stack maps — the round is unreachable.
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn inline_cf_enabled() -> bool {
    false
}

/// Does this body branch, and does any of its edges go BACKWARDS? One walk, decoded with
/// `insn_len_at` (the admission decoder). Used only by the census, at the one point the interface
/// arm has actually produced an entry — an admission census that counts refusals is worse than
/// none, which is the rule `A_DEEP_IFACE` beside it already follows.
pub(crate) fn cf_shape(bc: &[u8]) -> (bool, bool) {
    let (mut any, mut back) = (false, false);
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = jit::insn_len_at(bc, pc) else { return (any, back) };
        if matches!(op, 0x99..=0xa7 | 0xc6 | 0xc7) {
            any = true;
            if (pc as i32 + jit::s16(bc, pc + 1)) < pc as i32 {
                back = true;
            }
        }
        pc += len;
    }
    (any, back)
}

// ═══════════════════════ DEPTH: THE LAST ROOTING ARGUMENT ═══════════════════════
//
// ⭐⭐**EVERY REMAINING DEPTH-0 RESTRICTION IN THIS FILE IS A SENTENCE ABOUT THE SHADOW FRAME.**
// They are quoted verbatim in the three places they live and they all say one thing: *"the window
// roots the CALLEE's frame and `locals_ptr` roots the CALLER's, and `locals_ptr` is the compiled
// method's own frame only at depth 0 — a splice nested inside another splice would need its
// PARENT's Cranelift variables rooted too, and those live in no buffer at all."*
//
// Under `stackmaps::declaring()` there is no window and no buffer. `emit_inline` declares the
// callee's locals as stack-map variables at creation, one level at a time, and declares the
// CALLER's ref operand-stack values at every splice it emits — including a splice emitted from
// inside another splice, because that inner `emit_inline` sees the outer body's stack as its own
// caller stack. The parent's Cranelift variables the sentence worries about are exactly the
// variables the parent's own `declare_var_needs_stack_map` already named. So the premise is gone
// at EVERY level, not only at the first.
//
// ⚠**WHAT THIS DOES NOT DISCHARGE, AND WHY THE BUDGET EXISTS.** Depth multiplies CODE, and this
// tree has a measured verdict about that: `INLINE_MAX_BYTECODE = 35` was set because raising it
// regressed a boot monotonically (86.9 → 93.0 s) after a depth-8 splice turned
// `ShiftedNoise.compute` into 852 blocks. Rooting proofs are discharged by the maps; SIZE proofs
// are not, so this round replaces the depth BOOLEAN with a depth BUDGET and keeps every size
// ceiling that was ever measured.
//
// ⚠**ASK, DO NOT RE-DERIVE.** One `OnceLock` for the environment handle; whether the protocol is
// active is asked of `stackmaps::declaring()` on every call and cached nowhere else. `frames.rs`
// re-derived that fact once and the engine ran with nothing rooted at all.
/// `RAVA_NO_DEEP_INLINE=1` (or `RAVA_NO_EMIT_MORE`, or the maps off) restores every depth-0
/// refusal in this file and in `emit_inline` verbatim.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn deep_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let flag = *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_DEEP_INLINE").is_none() && !crate::flags::emit_more_disabled()
    });
    flag && crate::jit::runtime::stackmaps::declaring()
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn deep_inline_enabled() -> bool {
    false
}

/// SUB-HANDLE (`RAVA_NO_DEEP_IFACE=1`): the widened interface-body arm offered on the RECURSIVE
/// path as well as at the top level. Separate from the master switch for one reason — a rooting
/// fault is intermittent, so the two populations this round opens must be separable by bisection
/// without a rebuild.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn deep_iface_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let flag = *ON.get_or_init(|| std::env::var_os("RAVA_NO_DEEP_IFACE").is_none());
    flag && deep_inline_enabled()
}

/// SUB-HANDLE (`RAVA_NO_DEEP_DEVIRT=1`): guarded devirtualisation offered inside a nested callee's
/// own body, not only in the top-level method. This is the arm that first puts a GUARDED entry
/// into a `nested` map, so it is the one most worth bisecting on its own.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn deep_devirt_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let flag = *ON.get_or_init(|| std::env::var_os("RAVA_NO_DEEP_DEVIRT").is_none());
    flag && deep_inline_enabled()
}

// ─────────────────────────── THE TREE BUDGET ───────────────────────────
//
// Per-callee size ceilings bound ONE splice. They do not bound a TREE: twenty callees of thirty
// bytes each pass every per-callee ceiling in this file and are still six hundred bytes of body
// spliced into one site. C2 has the same two-level structure (`MaxInlineSize` 35 /
// `FreqInlineSize` 325 per callee, `MaxInlineLevel` 15 / `MaxRecursiveInlineLevel` 1 per tree),
// and this is our second level.
//
// The counter is per-THREAD and reset at the one entry point a compile starts from
// (`collect_inline_targets`), because a compile's target collection runs to completion on the
// thread that started it. It charges the callee's bytecode length at ADMISSION and never refunds:
// a subtree built for a parent that is then refused stays charged, which makes the bound an
// UPPER estimate of emitted bytes and therefore conservative in the only direction that matters.
//
// INERT unless `deep_inline_enabled()` — with the round off, nothing charges and nothing refuses,
// so the OFF arm is byte-identical.
thread_local! {
    static TREE_BUDGET_USED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `RAVA_INLINE_BUDGET=<bytes>` — total callee bytecode one compiled method's whole inline tree
/// may absorb. The default is deliberately loose enough that today's trees never touch it: the
/// point of the number is that the parent can SWEEP it, and that a runaway cascade has a ceiling
/// that is not "whatever the per-callee caps happen to multiply out to".
pub(crate) fn inline_tree_budget() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_INLINE_BUDGET")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(6000)
    })
}

pub(crate) fn budget_reset() {
    TREE_BUDGET_USED.with(|c| c.set(0));
}

/// Would admitting `bytes` more of callee body exceed this compile's tree budget?
pub(crate) fn budget_would_exceed(bytes: usize) -> bool {
    TREE_BUDGET_USED.with(|c| c.get().saturating_add(bytes) > inline_tree_budget())
}

pub(crate) fn budget_charge(bytes: usize) {
    TREE_BUDGET_USED.with(|c| c.set(c.get().saturating_add(bytes)));
}

/// STAGE SPLIT, kept as its own bisect handle (`RAVA_NO_INLINE_GCPOINT_CALLS=1`). The two stages
/// share ONE rooting mechanism and differ only in which population reaches it:
///   * stage 1 — the splice ALLOCATES and nothing else survives (150 spliced sites on a warm
///     reload, all of them cold `XxxArgument.foo()` factories: measured, and worth nothing on its
///     own — the number is in the round's report);
///   * stage 2 — one or more real calls survive inside the splice, which is where the population
///     is (`DataResult$Success.flatMap`, `map`, `DataResult.success`/`error`, `Registry.register`,
///     `ValidationContext.forChild`).
///
/// Separate because stage 1 was verified on its own first (23/23 probes, 5 gates, 3 × Bootstrap +
/// 3 × Reload) before stage 2 was allowed to reach the same code — a rooting bug is intermittent
/// by nature, so the two populations were not turned on together.
pub(crate) fn inline_gcpoint_calls_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_INLINE_GCPOINT_CALLS").is_none())
}

/// What a rooted splice would contain: how many allocations, and how many real calls survive.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GcSpliceShape {
    pub(crate) allocs: u32,
    pub(crate) calls: u32,
}

/// Is this body one a ROOTED splice can shape? Returns what it contains, or `None`.
///
/// A `Some` answer says only "the splice is expressible and its GC points are all at THIS frame's
/// level". Whether the caller wants to pay for it is decided by the caller.
pub(crate) fn gcpoint_body_shape(
    code: &Code,
    desc: &str,
    allow_void: bool,
    nested: &std::collections::HashMap<u16, jit::InlineCallee>,
    ctors: &std::collections::HashMap<u16, jit::CtorInline>,
    pool: &rava_classfile::ConstantPool,
) -> Option<GcSpliceShape> {
    if !code.exceptions.is_empty() {
        return None;
    }
    let bc = &code.bytecode;
    if bc.is_empty() || bc.len() > jit::INLINE_LEAF_BYTECODE {
        return None;
    }
    let void = jit::ret_jt(desc).is_none();
    if void && !allow_void {
        return None;
    }
    let mut sh = GcSpliceShape { allocs: 0, calls: 0 };
    let mut ends_in_return = false;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        if jit::is_excluded(op) {
            return None;
        }
        // `insn_len`, the decoder `emit_inline` walks a spliced body with — NOT `insn_len_at`.
        // A body this decoder cannot read would be refused there, so it is refused here, and
        // acceptance stays decided by one function.
        let Some(len) = jit::insn_len(op) else { return None };
        match op {
            // Anything that begins, ends or leaves a basic block: the window is entered once and
            // left once, so the splice must be one straight line.
            0x99..=0xab | 0xbf | 0xc6..=0xc9 => return None,
            // The one terminal return, agreeing with the descriptor (see `straight_line_return`).
            0xac..=0xb1 => {
                if (op == 0xb1) != void || pc + len != bc.len() {
                    return None;
                }
                ends_in_return = true;
            }
            // get/putstatic (a `<clinit>` is arbitrary Java), putfield, invokedynamic, the array
            // allocations and `multianewarray` (handle births through a callback this arm has not
            // priced), `instanceof` (loads a class), monitors.
            0xb2 | 0xb3 | 0xb5 | 0xba | 0xbc | 0xbd | 0xc1 | 0xc2 | 0xc3 | 0xc5 => return None,
            0x4f..=0x56 => return None, // array stores
            // ⛔**EVERY ARRAY ACCESS, FOR THE POP AND NOT FOR THE LOAD.** The window is
            // unregistered by `leave_splice_roots` at the splice's single exit, and the ONE way
            // to leave a spliced body without reaching it is an emitted SIDE EXIT — a `return_`
            // that skips the epilogue. `jit/emit/arrays.rs`'s fault arm is exactly that: a null
            // or out-of-bounds access calls `rava_array_fault` and returns from the whole
            // compiled function with no epilogue, which would leave this splice's window
            // registered on the thread's cell-root stack pointing at a dead frame. (That path
            // already leaks the method's OWN self-registered frame the same way — a pre-existing
            // hole this round must not widen.) So no element load, no `arraylength`.
            0x2e..=0x35 | 0xbe => return None,
            // `ldc` of a String/Class routes to `env.ldc`, which allocates. Numeric only.
            0x12 | 0x13 => {
                let idx = if op == 0x12 {
                    *bc.get(pc + 1)? as u16
                } else {
                    ((*bc.get(pc + 1)? as u16) << 8) | *bc.get(pc + 2)? as u16
                };
                match pool.get(idx)? {
                    Constant::Integer(_) | Constant::Float(_) => {}
                    _ => return None,
                }
            }
            // THE POPULATION. An object birth inside the splice.
            0xbb => sh.allocs += 1,
            0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                if pc + 2 >= bc.len() {
                    return None;
                }
                let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let m = pool.member_ref(cp)?;
                // `Object.<init>()V` emits nothing at all (see the 0xb7 arm in emit_op).
                let empty_root = op == 0xb7
                    && m.owner == "java/lang/Object"
                    && m.name == "<init>"
                    && m.descriptor == "()V";
                if empty_root || (op == 0xb7 && ctors.contains_key(&cp)) || nested.contains_key(&cp)
                {
                    // Collapses to inline field stores or a spliced body — no call survives.
                } else {
                    sh.calls += 1;
                }
            }
            _ => {}
        }
        pc += len;
    }
    if !ends_in_return {
        return None;
    }
    jit::find_leaders(bc).is_some().then_some(sh)
}

/// Every callee this splice would nest must be GC-LEAF: its own frame gets no window, so it must
/// have no GC point to need one. `is_gc_leaf` refuses every invoke, so the check does not recurse.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn gcpoint_nested_gc_leaf(
    nested: &std::collections::HashMap<u16, jit::InlineCallee>,
) -> bool {
    nested.values().all(|c| jit::is_gc_leaf(&c.bytecode))
}

/// WHICH RULE BLOCKS THIS CALLEE, asked in the order that makes the answer actionable.
///
/// `jit::inlinable_bail_reason` answers the same question in the order `is_inlinable_kind` happens
/// to test it, which tests SIZE first — so a 138-byte body whose real blocker is an interface call
/// is reported as `size`, and a census built on it ranks a budget above a structural refusal. That
/// mis-ranking is exactly what this round had to unpick by hand, so the census asks the opcode
/// questions first and reports the budgets only when the body would otherwise fit.
pub(crate) fn nested_blocker(code: &Code, desc: &str, allow_void: bool, depth: u8) -> &'static str {
    if !code.exceptions.is_empty() {
        return "exception-table";
    }
    let bc = &code.bytecode;
    let mut pc = 0;
    let mut first: Option<&'static str> = None;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = jit::insn_len_at(bc, pc) else { return "undecodable" };
        let r = match op {
            0xb9 => Some("invokeinterface"),
            0xba => Some("invokedynamic"),
            0xbb..=0xbd => Some("new"),
            0xb5 => Some("putfield"),
            0xb3 => Some("putstatic"),
            0xc1 => Some("instanceof"),
            0xaa | 0xab => Some("switch"),
            0x4f..=0x56 => Some("array-store"),
            _ => None,
        };
        if first.is_none() {
            first = r;
        }
        pc += len;
    }
    if let Some(r) = first {
        return r;
    }
    if jit::ret_jt(desc).is_none() && !allow_void {
        return "void-return";
    }
    if jit::find_leaders(bc).is_none() {
        return "bad-leaders";
    }
    if bc.len() > jit::INLINE_LEAF_BYTECODE {
        return "size>96";
    }
    if crate::jit_tier0() && bc.len() > jit::inline_max_bytecode() {
        return "size>tier0";
    }
    if depth + 1 >= jit::max_inline_depth() {
        return "depth";
    }
    // Nothing structural left: the body is a counted data-parallel shape, which must compile as
    // its own method for the fork to fire (`is_inlinable_kind`'s five analyzers).
    "data-parallel"
}

// ---------------------------------------------------------------------------
// THE LEARN PATH: a compiled SAM site writes the profile the interpreter never will.
// ---------------------------------------------------------------------------
//
// ⭐**THE RULE THAT REFUSES THE MOST CALLS IN THIS POPULATION IS NOT AN OPCODE RULE. IT IS
// "THERE IS NO PROFILE", AND IT WAS UNMEASURED UNTIL `[INLCEN]` EXISTED.** One warm
// `ReloadBench` pass, product configuration:
//
// ```text
// [INLCEN] 75,277 sites offered, 5,756 admitted (7.6%)
//    14,458  site.sam(0xb9)            14,415  sam.no-profile      = 99.7% of SAM offers
//    23,155  site.devirt(0xb6|0xb9)    18,404  devirt.no-profile   = 79.5% of devirt offers
//         6  ADMIT.sam
// ```
//
// Six SAM sites admitted in a whole run, out of 14,458 offered, and 14,415 of the refusals are
// the SAME refusal: `jit::lambda_site_mono` answered `None`. Not the MethodHandle kind (17), not
// the descriptors (0), not the body (20) — the profile simply is not there. Every other rule in
// the SAM chain is arguing about a population of forty-three sites.
//
// ⭐**AND THE REASON IS STRUCTURAL, NOT STATISTICAL.** `jit::lambda_site_note` had exactly ONE
// writer in the engine — `opcodes/invoke_virtual.rs`, the INTERPRETED arena-SAM apply. Under
// `RAVA_JIT_TIER0=1` a method is compiled on its FIRST ENTRY, so its SAM sites execute in machine
// code from the second call onward and that writer is never reached again. A site that is hot is
// therefore a site that is UNPROFILED, permanently, for the life of the process: the profile can
// only be written by the arm that tier-0 exists to stop running. `sitemono` hit exactly this and
// was cured exactly this way (`Vm::note_recv_profile`, "written from compiled code — because
// under RAVA_JIT_TIER0=1 nothing else ever writes it"); the lambda half of the profile was never
// given the same cure. This is that cure.
//
// **WHERE, AND WHY THERE.** `rava_dispatch_miss` is the compiled site's own dispatch falling to
// the host — it carries `(caller_ptr, cp)`, which IS the site key, and its receiver word. It is
// a recording point only; profile movement no longer schedules recompilation.
// `Vm::note_recv_profile` was the other candidate and is
// the WRONG one: it runs only where a direct-call entry is BUILT, and its own census reads
// `w_lambda 42` in a warm run against ~2.0 M arena-lambda applies — a lambda apply from compiled
// code does not build a direct entry, it falls to the general path.
//
// **WHAT IS WRITTEN, AND WHAT IT MAY NOT BE TRUSTED FOR.** The same four values the interpreter's
// writer records — `(class id, SITE index, MethodHandle kind, capture count)` — and nothing is
// concluded from them here. `try_collect_inline_targets` re-derives the implementation from the
// site index, re-checks both descriptors position by position, re-runs every body gate, and the
// emitted diamond re-proves the class id, the site index AND every capture cell at run time
// (`emit_sam_site_guard`). A wrong profile costs a guard miss, which is the ordinary dispatch.
//
// ⚠**KINDS 6 AND 7 ONLY, AND THAT IS A CORRECTNESS RESTRICTION, NOT A FILTER.** The interpreter's
// writer sits below two branches that dispatch a SAM-shaped call to something that is NOT the
// closure's implementation — an `$fi`/owner CONCRETE default method (`Function.andThen`,
// `Predicate.negate`) and a `build_fi_default` combinator — and its comment records that writing
// above them would "claim a SAM apply for a receiver that never performs one". This callback sits
// ABOVE that split and cannot see it. Kinds 5, 8 and 9 are the ones that dispatch virtually on
// `call_args[0]` or run an `<init>`, i.e. the ones whose apply is not a statically bound body;
// restricting to 6/7 keeps the recorded identity to the shape whose implementation the site index
// names outright, which is also the only shape `try_collect_inline_targets` will accept. A site
// that mixes a kind-6 closure with a default-method call still cannot be mis-spliced: the
// class-id half of the guard fails for any receiver that is not an arena closure of that capture
// signature, and the site-index half fails for any closure minted at a different `invokedynamic`.
//
// **COST.** No allocation, no lock, and NO unconditional atomic: the steady state is a shape test
// on the receiver word, two field loads off the closure, and `lambda_site_mono`'s three relaxed
// loads, all on a path that has already left machine code and is about to perform a host
// dispatch. The two counters and the epoch bump fire only on a TRANSITION — a site is claimed
// once and poisoned at most once — so their cost is bounded by the number of sites, not calls.
//
// Kill switch: the whole chain's own `RAVA_NO_LAMBDA_INLINE_CHAIN=1`, plus `RAVA_NO_CODEC_INLINE=1`
// for this round's half of it.

/// Record that a COMPILED call site `(caller, cp)` dispatched onto an arena closure.
/// `recv` is the raw receiver word as machine code passes it.
///
/// ⚠**REPRESENTATION TRAP.** The JIT cell encoding and the arena cell encoding agree on exactly
/// two things: a raw arena pointer and null. The shape test below is the same one
/// `Vm::note_recv_profile` uses to isolate "an ordinary arena object" — every tagged form (handle
/// `h|1<<40`, array `h|1<<42`, box `idx|1<<41`) fails `(w >> 40) == 0`, and the alignment test
/// rejects the arena-side low tags. Nothing is dereferenced before both hold.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn note_sam_site_from_compiled(caller: usize, cp: u16, recv: i64) {
    use crate::vm::inline_census as cen;
    cen::note(cen::L_HOOK_REACHED);
    if caller == 0 || !codec_inline_enabled() || !crate::jit::lambda_inline_chain_enabled() {
        cen::note(cen::L_HOOK_NO_CALLER);
        return;
    }
    let w = recv as u64;
    if recv == 0 || (w >> 40) != 0 || (w & 7) != 0 {
        cen::note(cen::L_HOOK_NOT_ARENA);
        return;
    }
    let Some(p) = crate::arena::ArenaPtr::new(recv as usize as *mut u8) else {
        cen::note(cen::L_HOOK_NOT_ARENA);
        return;
    };
    if !crate::nextgen::is_arena_lambda(p) {
        cen::note(cen::L_HOOK_NOT_LAMBDA);
        return;
    }
    let kind = crate::nextgen::arena_lambda_kind(p);
    if !matches!(kind, 6 | 7) {
        cen::note(cen::L_HOOK_KIND);
        return;
    }
    let before = crate::jit::lambda_site_mono(caller, cp).is_some();
    crate::jit::lambda_site_note(
        caller,
        cp,
        crate::nextgen::arena_class_id(p),
        crate::nextgen::arena_lambda_site_idx(p),
        kind,
        crate::nextgen::arena_lambda_cap_n(p) as u32,
    );
    let after = crate::jit::lambda_site_mono(caller, cp).is_some();
    if before == after {
        return; // the steady state: no transition, no counter, no epoch bump
    }
    if after {
        cen::note(cen::L_LEARNED);
    } else {
        cen::note(cen::L_POISONED);
    }
}
