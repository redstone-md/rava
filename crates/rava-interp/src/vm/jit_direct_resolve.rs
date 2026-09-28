//! Resolving a direct-call target for a compiled site.

use crate::*;

impl Vm {
    /// Populate the direct-call cache for a site (one-time per site; misses are
    /// not cached since the callee may compile later). Returns the entry or None
    /// when the site must use the general path.
    pub(crate) fn jit_direct_resolve(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
        site: (usize, u16),
    ) -> Option<jit::DirectEntry> {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let mref = caller.file.pool.member_ref(cp)?;
        // ⭐**was 2 `String`s per RESOLVE, is 0** — where what grows is RESOLVES (row 163:
        // `dmiss_res` = the 99.1% of direct misses that are this function declining, and the
        // negative cache re-probes each refused (site, class) every 64th call, so this body
        // runs millions of times a boot). `name` and `desc` are CONSTANTS OF THE CALL SITE
        // sitting in the caller's constant pool, and they were materialised ABOVE the entire
        // refusal ladder — every refusal below allocated two heap strings, compared them a
        // handful of times, and dropped them. Borrow the pool instead. The only arms that
        // cannot borrow the pool are the lambda ones (their name/descriptor come from the
        // lambda's interned `LambdaSite`, not from `cp`), and they borrow `lam_hold` below.
        let (name, desc): (&str, &str) = (mref.name, mref.descriptor);
        // PERMANENT-REFUSAL CHANNEL (`RAVA_NO_FAST_DIRECTRES`). Cleared here; set by exactly
        // those refusals whose verdict is a function of the constant pool and the receiver
        // CLASS — both of which the negative cache's key already pins — so `jit_direct_call`
        // can stamp the entry and stop re-resolving an answer that cannot change. See
        // `jit_direct_call::perm_refusal_note` for which refusals qualify and why.
        super::jit_direct_call::perm_refusal_arm();
        // NATIVE-SERVED CHANNEL (`RAVA_NO_HR_SYNTH`): same discipline as the permanence channel
        // above — cleared here, set by exactly the refusals whose verdict is "the interpreter
        // would hand this call to the JDK native layer", consumed once by `jit_direct_call`'s
        // miss arm. See `jit_direct_call::hr_native_note` for what the mark may and may not
        // claim; the consumer re-derives the verdict itself and treats the mark as a hint only.
        super::jit_direct_call::hr_native_arm();
        // Owners of the borrowed names above: a decoded lambda and up to two interned class
        // names must outlive the `match` below, because the tuple it produces carries `&str`
        // into it. Each is assigned in at most one arm and is never read on a path that does
        // not assign it (the compiler proves both).
        let lam_hold;
        let cls_hold;
        let a0_hold;
        // λ-IMPL KEY (gated): set by the lambda arms below to the pinned call-site
        // identity this entry is keyed on; 0 keeps the incumbent identity/fp keying.
        let mut entry_site_key: u64 = 0;
        // (target class, receiver guard, target name, target desc, target has a
        // receiver, arg0 guard, drop-raw[0], lam_fp, captured-count,
        // captured-ref-mask) — lambda entries retarget the call; the last two are
        // non-zero only for a capturing kind-6 lambda (gated).
        let (target, guard, mname, mdesc, has_recv, arg0_guard, shift, lam_fp, cap_n, cap_ref_mask) = match kind {
            // Static: resolution comes from the interpreter-warmed call_cache.
            //
            // ⭐**"THE INTERPRETER NEVER RAN THIS SITE" IS NOT "THIS SITE RESOLVES TO NOTHING",
            // AND THE OUTER `?` USED TO SPELL THEM THE SAME WAY.** `call_get` is TRI-state:
            // `Some(Some(m))` = resolved, `Some(None)` = resolved to a forced-native, `None` =
            // NO ENTRY, i.e. nobody has ever interpreted this site. `call_put` has exactly two
            // writers, `opcodes::invoke`'s invokestatic and invokespecial cache-fill arms, so the
            // third state means "the interpreter has not been here" — and `??` collapsed it into
            // the same refusal as "forced native".
            //
            // That is a BOOTSTRAP TRAP, the same shape as `direct_mono_resolve`'s: the fact this
            // refusal waits for is published by the very calls it refuses. A compiled caller
            // reaches a static site the interpreter never executed — a cold branch, or under
            // `RAVA_JIT_TIER0` (compile on first entry) EVERY site in the program — and the site
            // then has no path to an entry at all. The negative cache re-probes every 64th call
            // and gets the identical `None` forever, because nothing on the compiled path writes
            // `call_cache`.
            //
            // The resolution is a pure function of the symbolic ref sitting in the caller's
            // constant pool. Resolve it here (see `cold_site_resolve`) instead of waiting for an
            // interpreter visit that may never come.
            0 => {
                let decl = match crate::tlcache::call_get(site) {
                    Some(m) => m?,
                    None => self.cold_site_resolve(mref.owner, name, desc)?,
                };
                (decl, 0usize, name, desc, false, 0usize, false, 0u64, 0u16, 0u64)
            }
            // invokespecial: monomorphic per site (the callee is the resolved symbolic
            // ref, not the receiver's dynamic class), so no receiver-class guard. Match
            // opcodes::invokespecial's native-serving exclusions exactly, then take the
            // interpreter-warmed resolution — the receiver cell passes through unchanged
            // (the compiled callee reads its own `this`).
            2 => {
                // All four exclusions below are pure functions of the constant pool entry
                // (owner/name) or of a resolution that cannot change, so each marks the
                // refusal PERMANENT before returning.
                if opcodes::force_native_collection(self, mref.owner, name, desc) {
                    super::jit_direct_call::perm_refusal_note();
                    return crate::dresolve::refuse(0);
                }
                if crate::owner_bits::facts(mref.owner) & crate::owner_bits::FACT_NATIVE_REPR != 0
                    && name == "<init>"
                {
                    super::jit_direct_call::perm_refusal_note();
                    return crate::dresolve::refuse(1);
                }
                // was 1 `String` per invokespecial resolve, is 0: `mref.owner` is already a
                // `&str` into the caller's constant pool and this predicate is READ-ONLY.
                // (The identical defect was measured out of `vm/dispatch.rs`, on a path
                // carrying 4.3 M resolutions a boot.)
                #[cfg(feature = "next-gen-gc")]
                if self.jit_native_served_deep(mref.owner) {
                    super::jit_direct_call::perm_refusal_note();
                    return crate::dresolve::refuse(2);
                }
                // Same cold-site fallback as the static arm above, and for the same reason: an
                // invokespecial resolution is monomorphic per site (the symbolic ref, not the
                // receiver's dynamic class), so it is derivable from the constant pool alone and
                // does not need an interpreter visit to exist. The three exclusions the
                // interpreter's own fill arm applies are already here — `force_native_collection`
                // and `is_int_set` `<init>` above, `route_native_map` immediately below — so a
                // cold entry and a warm one name the same target.
                let decl = match crate::tlcache::call_get(site) {
                    Some(m) => m?,
                    None => self.cold_site_resolve(mref.owner, name, desc)?,
                };
                if opcodes::route_native_map(&decl) {
                    super::jit_direct_call::perm_refusal_note();
                    return crate::dresolve::refuse(3);
                }
                (decl, 0usize, name, desc, true, 0usize, false, 0u64, 0u16, 0u64)
            }
            // Virtual/interface: resolve on the receiver's exact class, excluding
            // every class/owner an interpreter special case could claim.
            1 | 3 => {
                if matches!(
                    &*mref.owner,
                    "org/slf4j/Logger" | "org/apache/logging/log4j/Logger" | "com/google/common/cache/CacheBuilder"
                ) {
                    // A constant of the constant pool — permanently refused.
                    super::jit_direct_call::perm_refusal_note();
                    return crate::dresolve::refuse(4);
                }
                // Read-barrier the receiver (its cell may carry a stale young pointer
                // evacuated since the caller computed it — the arena stale-receiver
                // class of bug), then decode its class. HANDLE (Ref) → interned-name
                // pointer guard; arena Obj (untagged low pointer) → its class id + 1
                // guard (never 0, disjoint from the always-large Ref pointer guard).
                let cell = crate::nextgen::forward_if_moved(*raw.first()?);
                // ⭐**was 1 `String` per virtual/interface RESOLVE, is 0.** Both sources hand
                // out a borrow already — `class_of` a `&str` into the heap's interned name and
                // `class_name` the registry's `Arc<str>` — and the copy existed only so the
                // five `starts_with`/substring tests below could read it. What grows is
                // resolves; the name's LENGTH grows the memcpy on top of the malloc.
                let (cls, guard): (&str, usize) =
                    if (cell as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
                        let h = cell as u32;
                        let g = self.heap.class_name_ptr(h);
                        if g == 0 {
                            return crate::dresolve::refuse(5);
                        }
                        (self.heap.class_of(h)?, g)
                    } else if cell != 0 && (cell as u64) >> 40 == 0 && cell & 7 == 0 {
                        // Arena Obj: aligned, in the low arena window (disjoint from the
                        // high-word Ref/Array/Box tags). Its class is invariant, so the
                        // header read is safe after the forward barrier above.
                        let cid = unsafe { crate::nextgen::obj_class(cell as *mut u8) };
                        cls_hold = crate::nextgen::class_name(cid)?;
                        (&*cls_hold, cid as usize + 1)
                    } else {
                        if crate::picprobe::enabled() { crate::picprobe::refuse(2); } // receiver shape
                        return None; // null / boxed / array / other → general path
                    };
                // LAMBDA receiver: call THROUGH a non-capturing method-ref lambda to
                // its compiled target. Class name can't guard (every lambda is
                // "$Lambda") — the entry guards on the lambda's IDENTITY cell bits
                // (heap Ref handles are stable) and drops raw[0] on the call:
                // an unbound instance ref's SAM arg0 IS the target receiver, a
                // static ref's SAM args map 1:1, so raw[1..] are the target locals
                // either way. The hottest refused directs are exactly these
                // functional-interface singletons (Climate$DistanceMetric.distance,
                // BooleanOp.apply, IndexMerger$IndexConsumer.merge).
                // RAVA_ARENA_LAMBDA: a CAPTURING closure born in the arena is a raw
                // 0b00 receiver whose class is the synthetic "[arena-lambda:…]", so it
                // reaches neither the "$Lambda" arm below nor a resolvable virtual —
                // every such call fell all the way to the general interpreter dispatch.
                // It is admitted here as a THIRD receiver representation, not a special
                // case: its identity is the interned SITE INDEX (stable, content-deduped,
                // never recycled), which is strictly stronger than the handle path's
                // `lambda_fp` hash and needs no pinning.
                #[cfg(feature = "next-gen-gc")]
                let arena_lam = crate::nextgen::arena_lambda_of_cell(cell);
                #[cfg(not(feature = "next-gen-gc"))]
                let arena_lam: Option<(crate::arena::ArenaPtr, u32)> = None;
                #[cfg(feature = "next-gen-gc")]
                if arena_lam.is_some() && crate::comparator_plan::cmp_plan_census_enabled() {
                    crate::comparator_plan::AL_DIRECT_ENTRY
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if cls == "$Lambda" || arena_lam.is_some() {
                    // Bisect gate: RAVA_LAMBDA_DIRECT=0 keeps every lambda on the
                    // general path (vm.apply → invoke_lambda), isolating this
                    // optimization when hunting a dispatch-semantics regression.
                    static LAMBDA_DIRECT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    if !*LAMBDA_DIRECT.get_or_init(|| {
                        std::env::var("RAVA_LAMBDA_DIRECT").map(|v| v != "0").unwrap_or(true)
                    }) {
                        return crate::dresolve::refuse(6);
                    }
                    crate::jit_lambda_scratch::set(guard); // teach jit_direct_call the interned ptr
                    // (lambda snapshot, per-call re-check token, λ-IMPL-KEY site key).
                    // The arena arm's re-check token is `site_idx + 1` — an identity, so
                    // it subsumes what `lambda_fp` only approximates; `site_key` stays 0
                    // because the guard already IS the site.
                    #[cfg(feature = "next-gen-gc")]
                    let (lam, lam_fp, site_key) = match arena_lam {
                        Some((p, si)) => (crate::opcodes::arena_lambda_to_struct(p)?, si as u64 + 1, 0u64),
                        None => self.jit_lambda_handle_ident(cell)?,
                    };
                    #[cfg(not(feature = "next-gen-gc"))]
                    let (lam, lam_fp, site_key) = self.jit_lambda_handle_ident(cell)?;
                    entry_site_key = site_key;
                    // was 2 `String`s per LAMBDA resolve, is 0: the decoded lambda owns an
                    // `Arc<LambdaSite>` whose owner/name/descriptor were interned ONCE at the
                    // invokedynamic. Park it here so the tuple below borrows those three
                    // instead of copying two of them per resolve.
                    lam_hold = lam;
                    let lam = &lam_hold;
                    match lam.kind {
                        // Static ref: for a NON-capturing lambda the impl desc EQUALS
                        // the call-site (SAM) desc. For a CAPTURING lambda (gated) the
                        // impl desc = (captured types) ++ (SAM params) and the captured
                        // values are prepended as the target's leading locals.
                        6 => {
                            // ⚠**THE SAME NAME APPROXIMATION AS CLAUSE 0 BELOW, AND IT WAS
                            // MISSING CLAUSE 0's ESCAPE.** `java/` here stands for "the natives
                            // own this implementation", but `class_loader::has_bytecode` is
                            // exactly the registry that says a real classfile WON over the native
                            // of that name — for such a class the native does not run at all and
                            // its bytecode is the only implementation there is, so refusing it is
                            // refusing the only thing that could be called. Byte-identical
                            // whenever nothing is overridden: `has_bytecode` short-circuits on one
                            // relaxed `OVERRIDE_ANY` load that is false for every run without an
                            // override classpath. The two structural exclusions that actually
                            // protect native state (`force_native_collection`,
                            // `jit_native_served_deep`) still run immediately below, and the
                            // target must additionally be COMPILED to be admitted at all.
                            if lam.owner().starts_with("java/")
                                && !crate::class_loader::has_bytecode(lam.owner())
                            {
                                return crate::dresolve::refuse(7);
                            }
                            // A native-served owner's bytecode reads synthetic state
                            // the native layer owns — calling it directly diverges.
                            if opcodes::force_native_collection(self, lam.owner(), lam.name(), lam.descriptor()) {
                                return crate::dresolve::refuse(8);
                            }
                            // was 1 `String` per capturing/static-lambda resolve, is 0 —
                            // `lam.owner()` is a `&str` into the interned site and the
                            // predicate is read-only.
                            #[cfg(feature = "next-gen-gc")]
                            if self.jit_native_served_deep(lam.owner()) {
                                return crate::dresolve::refuse(9);
                            }
                            let cap_n = lam.captured.len();
                            let (cn, crm) = if cap_n == 0 {
                                // Non-capturing: exact bridge-free desc match.
                                if lam.descriptor() != desc {
                                    return crate::dresolve::refuse(10);
                                }
                                (0u16, 0u64)
                            } else {
                                // CAPTURING (gated): verify impl = captures ++ SAM
                                // (params AND return), then classify each captured slot.
                                if !cap_lambda_enabled() || cap_n > 63 {
                                    return crate::dresolve::refuse(11);
                                }
                                let ip = param_types(lam.descriptor());
                                let sp = param_types(desc);
                                if ip.len() != cap_n + sp.len()
                                    || ip[cap_n..] != sp[..]
                                    || ret_of(lam.descriptor()) != ret_of(desc)
                                {
                                    return crate::dresolve::refuse(12);
                                }
                                let mut m = 0u64;
                                for (i, t) in ip[..cap_n].iter().enumerate() {
                                    if t.starts_with('L') || t.starts_with('[') {
                                        m |= 1u64 << i;
                                    }
                                }
                                (cap_n as u16, m)
                            };
                            // The lambda object exists ⇒ the indy site ran ⇒ its
                            // owner class is loaded+initialized.
                            let c = self.loader.get_or_load(lam.owner()).ok()?;
                            // V3: guard a CAPTURING impl by lam_fp (matches jit_direct_call's
                            // recv_guard); a non-capturing kind-6 lambda stays identity-guarded (cell).
                            // λ-IMPL KEY (gated): both collapse to the pinned call-site identity —
                            // for a capturing site that is the SAME key for every instantiation
                            // (the point of the lever), for a non-capturing one the per-site
                            // singleton means identity and impl key are already 1:1.
                            // An arena-lambda keys on its SITE: the receiver address is
                            // fresh per instantiation and the arena class is shared by
                            // every site with the same capture kinds, so neither can
                            // guard. The site index can, and it makes a capturing site
                            // MONOMORPHIC — the same property RAVA_LAMBDA_IMPL_KEY buys
                            // for handles, here for free.
                            let guard_key = match arena_lam {
                                Some((_, si)) => crate::nextgen::arena_lambda_guard(si),
                                None if site_key != 0 => site_key as usize,
                                None if cn == 0 => cell as usize,
                                None => lam_fp as usize,
                            };
                            (c, guard_key, lam.name(), lam.descriptor(), false, 0usize, true, lam_fp, cn, crm)
                        }
                        // Unbound instance/interface ref: SAM arg0 is the receiver;
                        // the call-site desc minus its first param must equal the
                        // target desc. Resolve on arg0's DYNAMIC class and guard it.
                        // (Capturing unbound refs are NOT handled — general path.)
                        5 | 9 => {
                            if !lam.captured.is_empty() {
                                return crate::dresolve::refuse(13);
                            }
                            let rest = strip_first_param(desc)?;
                            if lam.descriptor() != rest {
                                return crate::dresolve::refuse(14);
                            }
                            let a0 = crate::nextgen::forward_if_moved(*raw.get(1)?);
                            // Same borrow-don't-copy treatment as the receiver class above:
                            // was 1 `String` per unbound-ref resolve, is 0.
                            let (a0cls, a0guard): (&str, usize) =
                                if (a0 as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
                                    let g = self.heap.class_name_ptr(a0 as u32);
                                    if g == 0 {
                                        return crate::dresolve::refuse(15);
                                    }
                                    (self.heap.class_of(a0 as u32)?, g)
                                } else if a0 != 0 && (a0 as u64) >> 40 == 0 && a0 & 7 == 0 {
                                    let cid = unsafe { crate::nextgen::obj_class(a0 as *mut u8) };
                                    a0_hold = crate::nextgen::class_name(cid)?;
                                    (&*a0_hold, cid as usize + 1)
                                } else {
                                    return crate::dresolve::refuse(16);
                                };
                            // Same exclusions as a plain virtual on arg0's class — INCLUDING
                            // clause 0's `has_bytecode` escape, which this copy of the ladder had
                            // dropped. See the note on the kind-6 arm above; inert unless a
                            // classfile has actually overridden a native-served name.
                            if (a0cls.starts_with("java/")
                                && !crate::class_loader::has_bytecode(a0cls))
                                || a0cls.starts_with('$')
                                || crate::cache::is_cache_class(a0cls)
                                || crate::fastutil::is_int_set(a0cls)
                                || opcodes::force_native_collection(self, a0cls, lam.name(), lam.descriptor())
                            {
                                return crate::dresolve::refuse(17);
                            }
                            #[cfg(feature = "next-gen-gc")]
                            if self.jit_native_served_deep(a0cls) {
                                return crate::dresolve::refuse(18);
                            }
                            let decl = self.resolve_method(a0cls, lam.name(), lam.descriptor()).ok()??;
                            if opcodes::route_native_map(&decl) {
                                return crate::dresolve::refuse(19);
                            }
                            // λ-IMPL KEY (gated): an unbound ref keys on the impl too — the
                            // TARGET still depends on arg0's dynamic class, which `arg0_guard`
                            // re-checks per call exactly as before (impl identity alone does
                            // NOT imply the callee here, so that guard is load-bearing).
                            let gk = if site_key != 0 { site_key as usize } else { cell as usize };
                            (decl, gk, lam.name(), lam.descriptor(), true, a0guard, true, lam_fp, 0u16, 0u64)
                        }
                        _ => return None, // bound/special/ctor refs — general path
                    }
                } else {
                    // WHICH clause refuses, and for WHICH class. Row 164 measured this one
                    // site at 87% of all `jit_direct_resolve` refusals and 28.9 M excluded
                    // calls per boot, but "excluded" is not "correctly excluded": the test is
                    // a NAME approximation of a structural property (is this class natively
                    // served?). The split says which approximation to attack; the name tally
                    // says whether the classes behind it are really served by a native.
                    // The name approximation this comment calls out becomes simply WRONG for a
                    // class with real bytecode: it is no longer natively served, so excluding it
                    // from direct resolution excludes a body that could have been called
                    // directly.
                    let clause = if cls.starts_with("java/")
                        && !crate::class_loader::has_bytecode(cls)
                    {
                        Some(0)
                    } else if cls.starts_with('$') {
                        Some(1)
                    } else if crate::cache::is_cache_class(cls) {
                        Some(2)
                    } else if crate::fastutil::is_int_set(cls) {
                        Some(3)
                    } else if opcodes::force_native_collection(self, cls, name, desc) {
                        Some(4)
                    } else {
                        None
                    };
                    if let Some(c) = clause {
                        // ⚠**CLAUSE 0 IS THE ONE VERDICT HERE THAT CAN CHANGE, so it is the one
                        // that is NOT marked permanent.** `has_bytecode` flips false→true the
                        // moment a `java/` classfile is loaded from the classpath
                        // (`class_loader::note_bytecode`), and a class refused today can carry
                        // real bytecode an hour later. Clauses 1-4 are pure functions of the
                        // receiver's class NAME and the call site's method name — and the
                        // negative cache is keyed by (caller, cp, receiver-class guard), which
                        // pins both — so their answer is fixed for the life of the entry.
                        if c != 0 {
                            super::jit_direct_call::perm_refusal_note();
                        }
                        // Clauses 0 (`java/*`, no bytecode) and 4 (`force_native_collection`)
                        // are the two whose interpreter twin is "resolve to nothing / forced
                        // native → `natives::try_invoke_chain`". Clauses 1-3 (`$*`, the guava
                        // cache, the fastutil int-set) have their OWN dispatch arms in
                        // `opcodes::invokevirtual` and are deliberately NOT marked.
                        if c == 0 || c == 4 {
                            super::jit_direct_call::hr_native_note(1);
                        }
                        if crate::picprobe::enabled() { crate::picprobe::refuse(1); } // native/java-served
                        crate::dresolve::note_clause(c, cls, name);
                        return crate::dresolve::refuse(20);
                    }
                    // A native-served class (guava Immutable*/collection/IO) keeps its
                    // entire state in synthetic heap fields the native layer reads; its
                    // own bytecode reads those fields as null. Compiling/direct-calling
                    // it (e.g. ImmutableSortedMap.values → ImmutableCollection.iterator)
                    // NPEs — route to the interpreter's native collection layer instead.
                    // Native-servedness is derived from the super chain and only ever turns
                    // ON, so this refusal is permanent for this receiver class.
                    #[cfg(feature = "next-gen-gc")]
                    if self.jit_native_served_deep(cls) {
                        super::jit_direct_call::perm_refusal_note();
                        super::jit_direct_call::hr_native_note(2);
                        if crate::picprobe::enabled() { crate::picprobe::refuse(1); } // native-served (deep)
                        return crate::dresolve::refuse(21);
                    }
                    // Was `.ok()??`: both `Ok(None)` and `Err` returned `None` with no id, and
                    // still do. `Ok(None)` is split out only to MARK it — "the receiver's
                    // hierarchy declares no body for this name" is exactly the verdict the
                    // interpreter answers with the JDK native layer.
                    let decl = match self.resolve_method(cls, name, desc) {
                        Ok(Some(d)) => d,
                        Ok(None) => {
                            super::jit_direct_call::hr_native_note(4);
                            return None;
                        }
                        Err(_) => return None,
                    };
                    // Inherited fastutil-object-map method (subclass receiver) → keep it
                    // off the compiled fast path so the interpreter routes it to $map.
                    // The resolution is a function of (receiver class, name, desc): permanent.
                    if opcodes::route_native_map(&decl) {
                        super::jit_direct_call::perm_refusal_note();
                        super::jit_direct_call::hr_native_note(3);
                        if crate::picprobe::enabled() { crate::picprobe::refuse(1); } // native map route
                        return crate::dresolve::refuse(22);
                    }
                    (decl, guard, name, desc, true, 0usize, false, 0u64, 0u16, 0u64)
                }
            }
            _ => return None, // invokespecial — keep on the general path
        };
        // ⚠**RE-ENTRANCY FENCE.** Everything above that resolves or loads a class can run a
        // `<clinit>`, i.e. guest code, i.e. a NESTED `jit_direct_resolve` that arms and marks the
        // same thread-local. Every refusal that marks itself permanent does so immediately before
        // returning, so it cannot be affected; the refusals BELOW this line are the ones that
        // could inherit a nested verdict — and one of them is `refuse(23)`, the single refusal the
        // 1-in-64 re-probe exists for. Clear here so the tail can only ever report TRANSIENT.
        super::jit_direct_call::perm_refusal_arm();
        // Same fence for the native-served channel: a nested resolve inside `resolve_method` /
        // `get_or_load` above may have marked it, and nothing below this line is native-served.
        super::jit_direct_call::hr_native_arm();
        let method = target.file.method(mname, mdesc)?;
        let code = method.code.as_ref()?;
        let key = code as *const Code as usize;
        let f = match self.jit_cache.get(&key) {
            Some(jit::JitStatus::Compiled(f)) => f,
            _ => {
                if crate::picprobe::enabled() { crate::picprobe::refuse(0); } // tier lag: not compiled yet
                // ⛔NOT permanent, and this is the refusal the 1-in-64 re-probe exists for:
                // the callee compiles later and the site must be picked up when it does.
                return crate::dresolve::refuse(23);
            }
        };
        // Wide (long/double) params occupy 2 locals slots in the callee but
        // arrive as 1 cell each; record which RAW positions need a pad slot
        // (receiver, when present, shifts the params by one and is never wide).
        // Positions index the EFFECTIVE arg slice (raw[1..] for shift entries).
        let inner = args_of(mdesc);
        let bytes = inner.as_bytes();
        let (mut bi, mut pi, mut wide_mask) = (0usize, has_recv as usize, 0u64);
        while bi < bytes.len() {
            if pi >= 64 {
                return None; // >64 locals-slots of params — don't bother
            }
            let k = jit_arg_kind(bytes, &mut bi);
            if matches!(k, b'J' | b'D') {
                wide_mask |= 1 << pi;
            }
            pi += 1;
        }
        let rb = mdesc.as_bytes();
        let rstart = mdesc.rfind(')').map(|p| p + 1).unwrap_or(0);
        let ret_ref = matches!(rb.get(rstart), Some(b'L') | Some(b'['));
        // Buffer length = max_locals + max_stack: the callee's compiled code
        // spills live refs above max_locals before GC-capable callbacks.
        let buf_len = code.max_locals.saturating_add(code.max_stack);
        // GC-leafness of the callee BODY — the one fact a machine-code hit arm needs
        // that `self_reg` does not carry (see `DirectEntry::leaf`). Same test
        // `dispatch_resolve_cacheable_cls` applies to a class-word entry, so a lambda
        // and a plain virtual are admitted to the leaf arm on identical evidence.
        #[cfg(feature = "next-gen-gc")]
        let leaf = jit::is_gc_leaf(&code.bytecode)
            && code.max_locals as usize <= jit::dispatch_callbuf_slots();
        #[cfg(not(feature = "next-gen-gc"))]
        let leaf = false;
        Some(jit::DirectEntry {
            fptr: f.raw() as usize,
            ret: f.ret,
            buf_len,
            wide_mask,
            ret_ref,
            guard,
            arg0_guard,
            shift,
            lam_fp,
            site_key: entry_site_key,
            self_reg: f.self_reg,
            leaf,
            cap_n,
            cap_ref_mask,
        })
    }

    /// COLD-SITE resolution for a static / invokespecial direct call: the answer
    /// `tlcache::call_get` would be holding if the INTERPRETER had ever executed this site.
    ///
    /// Byte-identical to `opcodes::invoke`'s own cache-fill arm — `force_native_collection`
    /// first (a forced-native site caches `None`), otherwise `resolve_method` — so a cold site
    /// and a warm one name the same target. Deliberately PUBLISHES NOTHING: `call_put` writes
    /// the process-wide `shared_call_cache`, and this function runs under `&self` on gil-free
    /// workers as well as on main (`jit_direct_call_worker`). The direct cache keeps the answer
    /// anyway, and `resolve_method` is itself memoised per thread, so the re-probe is a hash
    /// lookup rather than a hierarchy walk.
    ///
    /// ⚠**AND IT MUST RE-PROVE THE ONE THING THE WARM PATH GOT FOR FREE: JVMS §5.5 CLASS
    /// INIT.** A warm entry exists only because the interpreter ran the site, which means
    /// `ensure_init` ran with it; a cold entry carries no such history, and a direct call jumps
    /// into machine code with no initialization barrier — a class whose only entry point is a
    /// static method would execute against default statics and its `<clinit>` would never throw.
    /// `clinit_done` is the right latch (`initialized` latches when `<clinit>` STARTS, which is
    /// exactly the window that can still run Java), it is MONOTONIC, and the refusal is left
    /// TRANSIENT so the 1-in-64 re-probe picks the site up the moment init completes.
    fn cold_site_resolve(&self, owner: &str, name: &str, desc: &str) -> Option<Arc<LoadedClass>> {
        // ⚠ These refusal ids are APPENDED, not positional. `dresolve`'s ids are ordinals of the
        // `return` sites in `jit_direct_resolve` and renumbering them would silently invalidate
        // every report row 163/164 is read against, so this block claims fresh ids at the end of
        // the range instead of inserting into the middle of it.
        if !cold_site_direct_enabled() {
            return crate::dresolve::refuse(24);
        }
        if opcodes::force_native_collection(self, owner, name, desc) {
            // The verdict the warm cache would be holding, and a pure function of the
            // constant-pool (owner, name, descriptor) the negative-cache key already pins ⇒ permanent.
            super::jit_direct_call::perm_refusal_note();
            return crate::dresolve::refuse(25);
        }
        let decl = self.resolve_method(owner, name, desc).ok()??;
        // ⚠ The resolve above can LOAD a class, which can run guest code, which can re-enter
        // `jit_direct_resolve` and set the permanence channel. Both tails below are TRANSIENT,
        // so re-arm rather than inherit a nested verdict (the same fence the resolver's tail
        // installs for `refuse(23)`).
        super::jit_direct_call::perm_refusal_arm();
        if !decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
            return crate::dresolve::refuse(26);
        }
        Some(decl)
    }

    /// FREEZE-COVERAGE CENSUS probe (`RAVA_FREEZE_CENSUS`): would the FROZEN direct
    /// cache serve this call on a gil-free worker? Exactly the predicate
    /// `jit_direct_call_worker` applies, WITHOUT invoking anything — the probe is
    /// `&self` and inserts nothing, so running it on main changes no state.
    pub(crate) fn direct_probe_hit(&self, caller_ptr: *const LoadedClass, cp: u16, kind: u8, raw: &[i64]) -> bool {
        matches!(self.jit_direct_probe(caller_ptr, cp, kind, raw), jit::DirectProbe::Hit { .. })
    }

    /// GAP-S09 stage 4 moved block (read-only): receiver-guard decode + the
    /// per-site direct-cache scan of `jit_direct_call`, verbatim — only the
    /// early `return None`s became `DirectProbe` variants so both the serial
    /// (`&mut`) and worker (`&self`) arms share one probe.
    /// Is ANY inert per-call diagnostic armed? Computed once, so the default path pays a
    /// single atomic load instead of six.
    ///
    /// ⚠ THIS MUST STAY THE EXACT DISJUNCTION of the gates it short-circuits in
    /// `jit_call_body`. A gate added there and forgotten here would be silently DEAD — the
    /// flag would read false and its block would never run, which looks exactly like a
    /// diagnostic that found nothing. `any_call_diag_is_the_full_disjunction` asserts it.
    pub(crate) fn any_call_diag() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            #[cfg(feature = "next-gen-gc")]
            {
                receiver_guard_enabled()
                    || crate::jitsig::enabled()
                    || crate::recordast::enabled()
                    || crate::shadow::flat_enabled()
                    || crate::shadow::flat_arm_enabled()
                    || crate::refpot::enabled()
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                crate::jitsig::enabled()
            }
        })
    }
}

/// ✅**DEFAULT ON** (`RAVA_NO_WIDE_INLINETGT=1` disables, as does `flags::emit_more_disabled()`).
///
/// Gates `cold_site_resolve` — the static/invokespecial direct arm's fallback for a site the
/// interpreter has never executed. With it off, a cold site behaves exactly as it did: the
/// missing `call_cache` entry is reported as "resolves to nothing" and the call falls to the
/// general path, which is always correct and merely slower.
///
/// Read once through a `OnceLock` because the miss path it sits on runs millions of times a boot
/// and an inline `var_os` there is a Win32 environment-block query — the defect already measured
/// at 2.5% of wall in `jit_direct_call`'s `RAVA_TRACE_DMISS` arm, with the flag unset.
fn cold_site_direct_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_WIDE_INLINETGT").is_none() && !crate::flags::emit_more_disabled()
    })
}
