//! Which classes are served natively, and what that forbids.

use crate::*;

/// ⭐⭐**ONE PREDICATE HAS BEEN ANSWERING FIVE DIFFERENT QUESTIONS, AND THEY DO NOT HAVE THE
/// SAME ANSWER.**
///
/// `jit_native_served_deep` is consulted by five call sites that each mean something else by it:
///
/// | question | call site | what a WRONG (over-permissive) answer costs |
/// |---|---|---|
/// | `Compile`       | `jit_run::native_served_compile_refusal` | a body may be reached from compiled callers that skip the interpreter's native cascade, so compiling it needs an entry guard |
/// | `Inline`        | `inline_targets` | a fault — inlining SKIPS the dispatch where the native would have won, so the caller runs bytecode against state the native layer keeps elsewhere |
/// | `DirectBind`    | `jit_direct_resolve` (×4: invokespecial owner, lambda owner, unbound-ref arg0 class, virtual receiver class) | same as `Inline`: a baked direct call is a dispatch that no longer happens |
/// | `AotLink`       | `aot_link` | compiles before any runtime entry, with neither a native-decline proof nor an entry guard |
/// | `DispatchCache` | `dispatch::dispatch_resolve_cacheable_cls_id` | the site the fastutil crash actually came through — it binds a compiled caller straight to a served-namespace callee |
///
/// **The original Compile relaxation also crashed.** A profile-use product with the per-question
/// compile refusal relaxed segfaulted twice within 0.6 s of a fresh boot; the same binary with
/// `RAVA_NO_SPLIT_NATIVE=1` reached `Done` in 23.0 s.
/// Compiled callers can bypass the interpreter's native cascade, so an interpreted decline is not
/// proof. Keep each consumer gate strict until every skipped-dispatch edge structurally preserves
/// native routing for the receiver representation.

/// May a `true` (refused) verdict cached by CLASS ID be re-asked when the bytecode-override
/// registry has grown since it was taken? `RAVA_NO_OVERRIDE_RECHECK=1` restores the frozen
/// verdict. See `Vm::jit_native_served_deep_id` for why only that table can go stale.
///
/// Inert by construction on every run without an override classpath: the caller short-circuits
/// on `class_loader::any_override()`, one relaxed load that is false unless a real classfile has
/// actually been loaded for a natively-served name.
#[cfg(feature = "next-gen-gc")]
fn override_recheck_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled()
            && std::env::var_os("RAVA_NO_OVERRIDE_RECHECK").is_none()
    })
}

impl Vm {
    /// GAP-A01 (drill iter 3, task 1): may `invokespecial class.<init> desc` on a
    /// PRIVATE (fresh `new` / owned `this`) receiver be admitted through the
    /// Throwable constructor contract (`native_contract::throwable_ctor_contract`)?
    /// The pure contract table cannot key this (the class name is arbitrary — any
    /// Throwable subclass), so the sound key is fully mechanical against the live
    /// loader, chosen as the STRONGEST available condition:
    /// (1) the exact class resolves NO bytecode for this `<init>` anywhere in the
    ///     resolution walk — the runtime (opcodes::invokespecial resolves bytecode
    ///     FIRST, natives only on failure) can only serve the call natively;
    /// (2) the exact class is UNLOADABLE — no guest class file exists, so no
    ///     guest bytecode can ever serve any of its methods, and no guest naming
    ///     can gain admission (a loadable guest class fails here and its real
    ///     body is analysed instead);
    /// (3) the class is PROVABLY a `java/lang/Throwable` subtype via the strict
    ///     walk (`opcodes::name_is_subtype_strict`: loaded super-chain + the
    ///     fixed JDK_SUPERS table, never lenient). (2)+(3) close the admission
    ///     set to the JDK exception table, for which the `natives::try_invoke`
    ///     dispatch chain was audited: the `throwable()` arm (its predicate is
    ///     mirrored inside `throwable_ctor_contract`) is the first arm that can
    ///     serve `<init>` for every such name, and it mutates ONLY the receiver
    ///     (`$message`/`$cause` puts) — the declared RECV_MUT effect.
    #[allow(dead_code)]
    pub(crate) fn private_throwable_ctor_admissible(&mut self, class: &str, name: &str, desc: &str) -> bool {
        if crate::native_contract::throwable_ctor_contract(class, name, desc).is_none() {
            return false;
        }
        // (1) no bytecode anywhere in the walk (defensive — implied by (2), but a
        // resolvable body must NEVER be skipped by a contract).
        //
        // A `RegionAbort` here cannot be propagated (this answers a bool), and it must not be read
        // as "no bytecode": that is the permissive direction. Refuse instead — the worker's region
        // is already tearing down and its results are discarded, so a false negative costs nothing
        // while a false positive would admit a contract that a resolvable body should have blocked.
        match self.resolve_method(class, name, desc) {
            Err(_) => return false,
            Ok(Some(decl)) => {
                if decl.file.method(name, desc).and_then(|m| m.code.as_ref()).is_some() {
                    return false;
                }
            }
            Ok(None) => {}
        }
        // (2) unloadable JDK-boundary name.
        if self.loader.get_or_load(class).is_ok() {
            return false;
        }
        // (3) strictly provable Throwable subtype.
        crate::opcodes::name_is_subtype_strict(self, class, "java/lang/Throwable")
    }

    /// GAP-A01 (drill iter 3, task 2): SOUND fallback for a virtual/interface
    /// callee whose dispatch closure is unprovable because the DECLARED OWNER is
    /// UNLOADABLE (distinguished from a loadable interface owner, which stays
    /// rejected — lambda implementors are unenumerable). Admissible iff
    /// (a) the owner's own native service carries a PURE, callback-free contract
    ///     (`native_contract::contract_for(owner, name, desc, None)`; allocation
    ///     is fine — fresh objects are private), AND
    /// (b) NO loaded class possibly subtypes the owner (`unloadable_owner_closed`).
    /// SOUNDNESS: every runtime instance whose exact class is an unloadable
    /// JDK-boundary name is created and served by rava's own natives — no guest
    /// bytecode can exist for it — and (b) proves no loaded guest class can be
    /// the receiver either, so the virtual call can only ever land in the native
    /// family the owner's contract describes. The subtype scan uses the lenient
    /// `opcodes::name_is_subtype`, which is the CONSERVATIVE direction here: any
    /// pair it cannot refute (an inconclusive chain over a non-table JDK name)
    /// counts as a possible subtype and rejects the owner. The residual
    /// authority is the fixed JDK_SUPERS table — the same authority the
    /// runtime's own `instanceof`/dispatch already rests on.
    #[allow(dead_code)]
    pub(crate) fn unloadable_owner_contract_fallback(&mut self, owner: &str, name: &str, desc: &str) -> bool {
        if !matches!(
            crate::native_contract::contract_for(owner, name, desc, None),
            Some(crate::native_contract::NativeContract {
                effect: crate::native_contract::NativeEffect::Pure,
                may_callback: false,
                ..
            })
        ) {
            return false;
        }
        self.unloadable_owner_closed(owner)
    }

    /// GAP-A01 (drill iter 3): is `owner` an UNLOADABLE name that NO loaded class
    /// possibly subtypes (class or interface edge)? Memoised per loader epoch
    /// beside `dispatch_closure_cache`; recomputed under the same fixed-point
    /// epoch-stability rule (the subtype walks can themselves load classes).
    #[allow(dead_code)]
    pub(crate) fn unloadable_owner_closed(&mut self, owner: &str) -> bool {
        if crate::ClassLoader::dry_run_loads_active() {
            loop {
                let before = self.loader.dry_run_staged_count();
                let computed = self.unloadable_owner_closed_uncached(owner);
                if self.loader.dry_run_staged_count() == before {
                    return computed;
                }
            }
        }
        self.offload_epoch_sync();
        if let Some(&hit) = self.unloadable_owner_closed_cache.get(owner) {
            return hit;
        }
        let computed = loop {
            let e = self.loader.epoch();
            let pass = self.unloadable_owner_closed_uncached(owner);
            if self.loader.epoch() == e {
                break pass;
            }
        };
        self.offload_epoch_sync();
        Self::debug_assert_not_gilfree("unloadable_owner_closed_cache");
        self.unloadable_owner_closed_cache.insert(owner.to_string(), computed);
        computed
    }

    #[allow(dead_code)]
    pub(crate) fn unloadable_owner_closed_uncached(&mut self, owner: &str) -> bool {
        if self.loader.get_or_load(owner).is_ok() {
            return false; // loadable owner (interface/abstract) — closure stays unprovable
        }
        let names = self.loader.loaded_class_names();
        !names
            .iter()
            .any(|cn| crate::opcodes::name_is_subtype(self, cn, owner))
    }

    /// Does any ANCESTOR of `name` model its state through synthetic heap-only
    /// fields (a collection/map/entry type)? Such a subclass inherits those
    /// `$fields`, so it MUST be a heap handle, never an arena object — the native
    /// accessors only accept `Value::Ref`. Transitive: guava's
    /// `ImmutableMapEntry$NonTerminalImmutableMapEntry` → `ImmutableMapEntry` →
    /// `AbstractMap$SimpleImmutableEntry` (a direct-super check would miss it).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn has_native_backed_ancestor(&self, name: &str) -> bool {
        self.super_chain(name)
            .iter()
            .any(|a| crate::opcodes::is_native_backed_alloc(a))
    }

    /// BIRTH-ONLY ancestor scan (`RAVA_ARENA_BIRTH_NS_GUARD`, default OFF →
    /// delegates verbatim to `has_native_backed_ancestor`). Same walk, but each
    /// link is judged by `opcodes::birth_native_backed_alloc`, so a jar class is
    /// no longer force-handled because its own — or an intermediate jar super's —
    /// NAME contains "List"/"Set"/"Stack"/…, while a GENUINE native-backed
    /// ancestor still wins: every type whose state rava models in synthetic
    /// `$elems`/`$map`/`$key`/`$buf` fields lives in `java/`, `javax/`, `jdk/`,
    /// `sun/`, `com/google/common/` or `it/unimi/` (see `native_served_namespace`
    /// and the exact-name lists), all of which stay guarded. `super_chain`
    /// includes the class itself, so this covers the direct test too — no
    /// `skip(1)` here, unlike `jit_native_served_deep`, because the birth rule
    /// applies its direct test with the same guarded predicate.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn birth_native_backed_ancestor(&self, name: &str) -> bool {
        // MEMOISED, and this is the call `owner_bits` names as the worst case of all: "the
        // allocator path — `birth_native_backed_ancestor` applies the collection predicate to
        // EVERY ancestor of the class being born, so one `new` pays seven substring searches
        // per super-chain level". The memo it introduced made each of those a hash instead of
        // a scan; this makes the whole per-ancestor loop ONE hash, paid once per class name.
        //
        // Same staleness argument as `jit_native_served_deep`: the answer derives from
        // `super_chain`, which is already cached by start name, so nothing new is frozen here.
        // Keyed only inside the guard branch — the other branch delegates, and giving one
        // memo two different definitions is how the flag arms drift.
        thread_local! {
            static MEMO: std::cell::RefCell<FastMap<Box<str>, bool>> =
                std::cell::RefCell::new(FastMap::default());
        }
        if !crate::nextgen::birth_ns_guard_enabled() {
            return self.has_native_backed_ancestor(name);
        }
        if let Some(v) = MEMO.with(|m| m.borrow().get(name).copied()) {
            return v;
        }
        let v = self
            .super_chain(name)
            .iter()
            .any(|a| crate::opcodes::birth_native_backed_alloc(a));
        MEMO.with(|m| {
            m.borrow_mut().insert(name.into(), v);
        });
        v
    }

    /// Is `class` actually served by the native layer (so the JIT must never
    /// compile/inline/direct-call its bytecode)? `opcodes::jit_native_served`
    /// matches by NAME SUBSTRING ("List"/"Set"/…), which is only meaningful for
    /// namespaces the natives implement (JDK/guava/fastutil). A user/jar class
    /// that merely name-matches (a bench class `Collections`, a jar `FooList`)
    /// resolves to its OWN bytecode with real fields — the interpreter is
    /// bytecode-first — so blanket-denying it the JIT silently interpreted every
    /// such class forever. Outside the native namespaces, deny only a class that
    /// INHERITS synthetic native state from a native-backed super (its resolved
    /// methods can land in the natives; skip(1) = strict ancestors, since
    /// super_chain includes the class itself and would re-introduce the
    /// name-match false positive).
    ///
    /// All production consumers share this cached refusal, so it remains conservative.
    /// The class-keyed memo does not encode receiver representation or call-site semantics;
    /// each consumer must preserve its own refusal until its skipped-dispatch path is proved.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_native_served_deep(&self, class: &str) -> bool {
        if opcodes::native_served_namespace(class) {
            // ⚠**THE EXCLUSION IS PER-NAMESPACE WHILE NATIVE COVERAGE IS PER-METHOD**, and a
            // method those namespaces do not actually serve is then interpreted forever. Measured
            // on `bench/ShapeOr.java` with `RAVA_PROFILE_BOOT_DISPATCH`:
            // `DoubleArrayList.equals(Object)` at **382,495 interpreted entries and 0 JIT**, plus
            // `getDouble(I)D` and `AbstractDoubleList.toString()` beside it. This is the same
            // defect the comment above records having fixed for classes OUTSIDE the namespaces;
            // the inside never got it.
            //
            // The reason it is not simply narrowed is in `aot_link.rs`: a native-BACKED class's
            // bytecode reads real fields the native layer never populates, so a compiled body
            // would run a broken one. But that argument does not obviously apply to every
            // name-matched class in the namespace — `getDouble` returns CORRECT values 13,838
            // times under interpretation, which is only possible if its real fields are
            // populated. `RAVA_JIT_NS_BYTECODE=1` judges an `it/unimi/` class by the same
            // ancestor test used outside the namespaces, so the question can be put to the
            // bit-exact gates instead of argued.
            // ⛔⛔**AND THE ANSWER IS: COMPILING THEM IS WORTH ZERO.** With the flag on,
            // `BOOT-DISPATCH` goes from `interpreted 830,351 / jit 51,674` with three stuck
            // methods to `interpreted 40,491 / jit 459,816` and "none — every >10k-entry method
            // reached a compiled tier", the hash stays `3cfa327f01e0c901` (so compiling this
            // bytecode is CORRECT here), and the time does not move: 10.663/10.788 against
            // 10.801/10.760, overlapping.
            //
            // **790,000 interpreted method entries became compiled ones and bought nothing.**
            // These are small bodies reached through a host bridge, so what they cost is the
            // INVOKE — resolve, marshalling, the frame — not the execution of their opcodes, and
            // compiling the body removes none of that. The same conclusion the frame work reached
            // from the other side, and the sharpest instance of "coverage is not time" this tree
            // has measured.
            //
            // The flag stays as a working measurement handle. It must not ship: the correctness
            // argument in `aot_link.rs` (a native-BACKED class's bytecode reads real fields the
            // native layer never populates) is unaffected by this null, and the gates only prove
            // it for the classes THIS probe touches.
            static NSB: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            // ✅**DEFAULT-ON** since the deep-IC channel was closed: `dispatch_resolve_cacheable_cls`
            // now refuses to bind a compiled caller directly to a served-namespace callee, which
            // is what the crash came through. The per-method proof licences the rest — a
            // threshold compile candidate reached `jit_run` through `invoke_framed`, so its
            // bytecode is what already ran.
            //
            // Measured at a 0.41% floor (null test, same session): enabling it reads -1.29%
            // median / -1.85% min in one series and -3.17% / -0.35% in the next, and on the boot
            // -2.67% median. It was default-ON for exactly that long.
            //
            // ⛔**AND THEN A COUNTER-EXAMPLE KILLED THE PROOF, so it is opt-in again
            // (`RAVA_NS_BYTECODE=1`).** The licence above is "a threshold compile candidate
            // reached `jit_run` through `invoke_framed`, so its bytecode already ran". The hole:
            // a method can be interpreted for SOME receivers while the natives serve it for
            // OTHERS — the two paths are chosen per RECEIVER REPRESENTATION, not per method.
            // Compiling it then makes the compiled body serve every receiver, including the ones
            // the natives were handling.
            //
            // Demonstrated: with `RAVA_ARENA_BIRTH_NS_GUARD=1` (which moves classes between the
            // arena and the handle heap) the boot dies with a named compiled stack —
            // `AbstractIntList.toString()` under guava's `ToStringHelper.toString()` under
            // `StateDefinition.lambda$new$3` — and the same run with this relaxation OFF is
            // clean. The birth guard alone is clean too: it is the INTERACTION.
            //
            // So the default configuration is safe today and any representation change can
            // expose it, silently in the general case. 2.67% is not worth an unsound default;
            // closing the hole means the compiled body must still let the natives claim a
            // receiver they own, which is an entry check this codegen does not have.
            let relaxed = *NSB.get_or_init(|| {
                let on = std::env::var_os("RAVA_NS_BYTECODE").is_some();
                // ⚠**NAME THE KNOWN-BAD COMBINATION AT THE POINT OF USE.** These two flags are
                // individually safe and together reproduce the counter-example above; the next
                // person to try them would otherwise meet an access violation in compiled code
                // and have to re-derive why. One line, printed once, on an opt-in path.
                if on && std::env::var_os("RAVA_ARENA_BIRTH_NS_GUARD").is_some() {
                    eprintln!(
                        "[WARN] RAVA_NS_BYTECODE with RAVA_ARENA_BIRTH_NS_GUARD is a KNOWN CRASH:                          moving classes between the arena and the handle heap changes which                          receivers the natives serve, and a compiled body then serves all of                          them (repro: AbstractIntList.toString under ToStringHelper.toString)."
                    );
                }
                if on {
                    // Arm the observation table: the gate in `jit_run` refuses any method the
                    // cascade has answered, which is what makes this relaxation per-METHOD.
                    crate::native_claimed::arm();
                }
                on
            });
            // ⭐**BISECT HANDLE.** The relaxation crashes a boot in compiled code, and "somewhere
            // in fastutil" is not a defect report. `RAVA_NSB_ONLY=a/b/,c/d/` narrows it to the
            // listed prefixes so the failing family can be halved instead of argued about.
            let nsb_ok = {
                static ONLY: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
                match ONLY.get_or_init(|| {
                    std::env::var("RAVA_NSB_ONLY").ok().map(|v| {
                        v.split(',').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
                    })
                }) {
                    Some(list) => list.iter().any(|p| class.starts_with(p.as_str())),
                    None => true,
                }
            };
            // With an explicit prefix list the list IS the scope — otherwise the experiment can
            // only ever ask about fastutil, and the interpreted census's remaining head is guava.
            // ✅**GUAVA COMPILES BY DEFAULT, fastutil does not — and the difference was measured,
            // not assumed.** The namespace exclusion is per-NAMESPACE while native coverage is
            // per-METHOD, so a method the natives never serve is interpreted forever. For
            // fastutil that cannot simply be lifted: `AbstractIntList` + `IntImmutableList`
            // compiled together die reading a slot whose data the native layer keeps elsewhere
            // (see the bisect in this file's history). For guava it can, and the evidence is the
            // usual three: `DataPackJson` hash unchanged, 4 bit-exact gates green, 21
            // differential probes matching `java -Xint`, with interpreted entries falling
            // **877,227 -> 697,891 (-20%)**.
            //
            // ⚠Not to be confused with the OVERRIDE registry, which makes a class's bytecode WIN
            // over the native of the same name — registering guava there took a boot from 40 s to
            // a 400 s timeout. This changes nothing about WHICH implementation runs; it only lets
            // the JIT compile bytecode the interpreter is already executing.
            //
            // `RAVA_NO_GUAVA_JIT=1` restores the refusal.
            // ✅**DEFAULT-ON AGAIN, and this time the argument is structural rather than a
            // workload's hash.** It was made opt-in on the strength of `aot_link.rs`'s warning
            // that a compiled body runs "with no native fallback" — true, but about a method
            // whose BYTECODE DOES NOT RUN, i.e. one the natives intercept. The threshold-driven
            // JIT only ever compiles a method that reached `jit_run` through `invoke_framed`,
            // and the natives are consulted only when resolution finds no body — so **a
            // compile candidate has interpreted entries, which is proof the natives declined
            // it**, and compiling it re-runs the same bytecode. AOT has no such proof and now
            // carries the strict test instead (`aot_link.rs`).
            //
            // `RAVA_NO_GUAVA_JIT=1` restores the refusal.
            //
            // ⚠**The earlier reversal is kept in the history on purpose**: the reasoning that
            // produced it — "green gates are evidence about a workload, not about a class of
            // change" — is right, and what it was missing was the per-method proof above.
            // ⛔**OPT-IN, by the SAME counter-example (`RAVA_GUAVA_JIT=1`).** The hole below is not
            // about fastutil: it is about any namespace whose classes the natives serve for SOME
            // receivers, and guava is on that list precisely because its collections are served.
            // Keeping guava default-ON while fastutil is opt-in would be inconsistent — the
            // demonstrated failure just happened to land on a fastutil `toString`.
            //
            // The counted result stays reachable and stands: interpreted entries
            // 877,227 -> 697,891 (-20%) with gates, probes and hash green.
            if class.starts_with("com/google/common/") {
                static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                if *ON.get_or_init(|| std::env::var_os("RAVA_GUAVA_JIT").is_some()) {
                    return false;
                }
            }
            let in_scope = {
                static HAS_LIST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                if *HAS_LIST.get_or_init(|| std::env::var_os("RAVA_NSB_ONLY").is_some()) {
                    nsb_ok
                } else {
                    class.starts_with("it/unimi/")
                }
            };
            if relaxed && in_scope {
                // ⚠**AND IT HAS TO SKIP THE ANCESTOR TEST TOO.** The first cut fell through to
                // the out-of-namespace path below, whose `is_native_backed_alloc` is the SAME
                // substring test on every ancestor — so `DoubleArrayList` matched "List" again
                // and the flag was inert: `BOOT-DISPATCH` still read 382,495 interpreted / 0 JIT
                // with it on. A flag that changes no behaviour measures nothing.
                return false;
            }
            return opcodes::jit_native_served(class);
        }
        // MEMOISED per class name, the same argument `owner_bits` makes and for a worse
        // starting point: the uncached path hashes `class` for the super-chain cache, then
        // asks `is_native_backed_alloc` about EVERY ancestor — and each of those is itself an
        // `owner_bits` hash — then hashes `class` again for the pure-value-override cache.
        // One name, N+2 hashes and two `RwLock` acquisitions, on a JIT-resolution path the
        // serial prefix runs constantly (`super_chain` 1.6% and `owner_bits::bits` 2.1% of it).
        //
        // NO NEW STALENESS. The answer is derived from `super_chain`, which is ALREADY cached
        // by start name and so already commits to whatever was loadable the first time it was
        // asked. Memoising the derived bool inherits exactly that, and adds nothing.
        //
        // Thread-local rather than shared: the only writer is the asking thread, so a worker
        // gets its own identical table instead of contending on one.
        thread_local! {
            static MEMO: std::cell::RefCell<FastMap<Box<str>, bool>> =
                std::cell::RefCell::new(FastMap::default());
        }
        if let Some(v) = MEMO.with(|m| m.borrow().get(class).copied()) {
            return v;
        }
        let v = self.jit_native_served_deep_uncached(class);
        MEMO.with(|m| {
            m.borrow_mut().insert(class.into(), v);
        });
        v
    }

    /// `jit_native_served_deep` keyed by the receiver's DENSE ARENA CLASS ID.
    ///
    /// ⭐**A VERDICT ABOUT A CLASS SHOULD NOT COST THE LENGTH OF ITS NAME.** The memo above is a
    /// `FastMap<Box<str>, bool>`: every ask hashes 40-60 bytes of class name and, on a hit,
    /// compares them again — for an answer that is a property of the class and, once computed,
    /// never changes for the life of the process. The arena dispatch path does not even have a
    /// name in hand; it has a `u32` class id and builds/borrows the name only to feed this memo.
    /// Was O(len(class name)) hash + compare per ask, is one bounds-checked byte load at a known
    /// index.
    ///
    /// **NO NEW STALENESS, and this is the whole soundness argument.** The name-keyed memo below
    /// already freezes this answer per class NAME. The arena class registry is APPEND-ONLY — a
    /// class id, once observed, names the same immutable record (and therefore the same name)
    /// forever, which is the same property `nextgen::with_class`'s per-thread mirror is built on.
    /// So the id table can only ever hold answers the name table would also hold; it never
    /// answers for a name the name table has not been asked about. A miss falls straight through
    /// to `jit_native_served_deep`, so both arms have exactly one definition of the verdict.
    ///
    /// **SIZE: one byte per class id, per thread**, grown only to the highest id actually asked
    /// about. An MC boot registers on the order of 2,000 arena classes ⇒ ~2 KB, and the hot
    /// working set is the handful of ids a compiled site dispatches on, i.e. a couple of cache
    /// lines. That is the point of keying by id rather than by inlined name bytes: this tree has
    /// already measured a 14 KB inline-key table as a REGRESSION against the allocation it
    /// removed, because it stopped fitting in L1. Capped at 2^20 ids (1 MB) so a pathological id
    /// space degrades to the name path instead of growing a table.
    ///
    /// Thread-local for the same reason the memo it fronts is: the only writer is the asking
    /// thread, so a worker gets its own identical table instead of contending on one.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_native_served_deep_id(&self, class_id: u32, class: &str) -> bool {
        if !super::dispatch::fast_dresolve() {
            return self.jit_native_served_deep(class);
        }
        // Highest id worth a table entry (1 MB); above it, fall through to the name path.
        const CAP: usize = 1 << 20;
        // Tri-state per class id: 0 = never asked, 1 = false, 2 = true.
        thread_local! {
            static BY_ID: std::cell::RefCell<Vec<u8>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        let i = class_id as usize;
        match BY_ID.with(|t| t.borrow().get(i).copied().unwrap_or(0)) {
            1 => return false,
            2 => {
                // ⭐**THE ONE VERDICT IN THIS FILE THAT CAN ACTUALLY GO STALE, AND IT IS THIS
                // TABLE ONLY.** The claim above ("no new staleness — the name memo already
                // freezes this per class name") is true of the name memo and NOT of this one,
                // because the two cache different sets of answers:
                //
                // `jit_native_served_deep`'s namespace branch RETURNS EARLY, before the name
                // memo — deliberately, since `native_served_namespace` re-reads
                // `class_loader::has_bytecode` on every ask so that a `java/*` class whose REAL
                // classfile has since been loaded stops being native-served the moment it is
                // registered. This table sits OUTSIDE `jit_native_served_deep` and caches its
                // return value whatever branch produced it, so it re-freezes exactly the answer
                // that branch refused to freeze.
                //
                // The flip is one-directional and monotone: the override registry only ever
                // GROWS, `has_bytecode` only goes false -> true, and that only ever turns a
                // `true` (refused) verdict into `false` (admitted). So a cached `1` can never be
                // stale and is returned above without a load; only `2` is re-asked, and only
                // while an override actually exists. A stale `2` is a PERMANENT refusal to
                // compile the only implementation the class has left — the migrated bytecode —
                // which is the same defect `jit_native_served_deep_uncached` records as the
                // reason the `java.util`-as-bytecode line was measured on a false premise.
                //
                // Cost on every ordinary run: one relaxed load of a static bool that is false,
                // on the REFUSE arm only. `any_override()` short-circuits before the flag read.
                if !(crate::class_loader::any_override() && override_recheck_enabled()) {
                    return true;
                }
                // Fall through: recompute live and overwrite the slot below, so the table
                // self-heals instead of needing invalidation.
            }
            _ => {}
        }
        let v = self.jit_native_served_deep(class);
        if i < CAP {
            BY_ID.with(|t| {
                let mut t = t.borrow_mut();
                if i >= t.len() {
                    t.resize(i + 1, 0);
                }
                t[i] = 1 + v as u8;
            });
        }
        v
    }

    /// The walk `jit_native_served_deep` memoises. Kept as its own function so the memo and
    /// the computation cannot drift — there is one definition of the answer.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_native_served_deep_uncached(&self, class: &str) -> bool {
        // ⭐**A CLASS WHOSE OWN BYTECODE OVERRODE THE NATIVE MUST BE COMPILABLE.**
        //
        // `has_bytecode` is true only for names in a natively-served namespace for which a REAL
        // classfile was loaded — the migration hook. For such a class the native no longer runs
        // at all; its bytecode does. Refusing to compile it is then refusing to compile the only
        // implementation there is.
        //
        // ⛔**And this is why the `java.util`-as-bytecode line was refuted on a false premise.**
        // That experiment measured a guest `ArrayList` at **12.2x on the operation** and a boot
        // **+8.46% SLOWER**, and concluded the line does not pay. But `native_served_prefix`
        // matches `java/`, so the migrated class could never reach the compiler: what the boot
        // compared was INTERPRETED guest bytecode against Rust natives, not compiled bytecode
        // against Rust natives. The comparison the experiment meant to run had not been run.
        //
        // Inert by construction when nothing is overridden: `has_bytecode` reads one relaxed
        // bool that is false for every run without an override classpath.
        if crate::class_loader::has_bytecode(class) {
            return false;
        }
        // ⭐**AN ANCESTOR IN `java/*` IS NOT A REASON TO REFUSE A CLASS'S OWN BYTECODE**
        // (`RAVA_JIT_GUEST_SUB=1`).
        //
        // The ancestor test refuses any guest class descending from a native-backed one, and that
        // is the entire stuck population on an MC boot: `gson/LinkedTreeMap extends
        // java/util/AbstractMap` (~830 k interpreted entries across find/get/put/rebalance),
        // `DoubleArrayList` under `AbstractCollection`, guava's `AbstractIndexedListIterator` —
        // 36 methods, 1,688,387 interpreted entries, zero compiled.
        //
        // The stated risk is that "a native-BACKED class's bytecode reads real fields the native
        // layer never populates". But those 830 k entries ARE that bytecode, running, correctly:
        // the natives do not serve these methods at all. Compiling the same bytecode the
        // interpreter already executes cannot change what it reads. Whether that reasoning holds
        // is a question for the bit-exact gates, which is why it is a flag.
        if crate::vm::guest_sub_jit_enabled() && !crate::opcodes::native_served_prefix(class) {
            return false;
        }
        if !self
            .super_chain(class)
            .iter()
            .skip(1)
            .any(|a| crate::opcodes::is_native_backed_alloc(a))
        {
            return false;
        }
        // A pure value-computer subclass (own accessor+size bytecode, primitive-only
        // fields — e.g. VoxelShape's CubePointRange over AbstractDoubleList) never
        // reads inherited synthetic native state: its methods are its own bytecode
        // on real fields. Denying it the JIT interpreted the hottest VoxelShape
        // bootstrap sinks forever (30% of interpreted self-time, ~25M per-call
        // dispatch misses in the RingScan setup phase).
        !self.pure_value_collection_override(class)
    }

    /// A native-collection SUBCLASS that fully overrides the backing with its OWN
    /// bytecode accessors (getDouble/getInt/get + size) does not read the synthetic
    /// `$elems` the native_super gate protects — so it can be arena-allocated (flat
    /// cells, no Box<Object>+3-Vec overhead), the RAM lever for CubePointRange (605k
    /// live, #1 RSS class: one `int parts` field + arithmetic getDouble, no $elems).
    /// Gated by RAVA_ARENA_OVERRIDE — a wrong relaxation silently no-ops a real native
    /// accessor, so it stays opt-in until a full boot bit-exact validation. Conservative:
    /// requires a compute accessor (getDouble/getInt/get) AND size, both declared with
    /// bytecode on the class itself (not inherited).
    ///
    /// ⭐**DEFAULT-ON since the value-computer round (`RAVA_NO_VALUECOMPUTER=1` restores the
    /// opt-in arm below verbatim).** The birth sites (`opcodes::new_object_value`,
    /// `arena_new_site_uncached`, `try_arena_alloc_by_name`) all ask `birth_native_backed_ancestor
    /// && !overrides_collection_backing`, so this is the ONE place the birth side consults, and it
    /// now answers with the structural predicate `value_computer_admits` — the same predicate the
    /// layout hook (`value_computer_layout_complete`) and the natives-side arena arm
    /// (`value_computer_coll_op`) answer with, so the three cannot disagree about a class.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn overrides_collection_backing(&self, name: &str) -> bool {
        if valuecomputer_enabled() {
            return self.value_computer_admits(name);
        }
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RAVA_ARENA_OVERRIDE").is_some()) {
            return false;
        }
        self.pure_value_collection_override(name)
    }

    /// The raw (env-independent) predicate behind `overrides_collection_backing`,
    /// memoized: a user subclass of a native-backed collection that declares its
    /// own compute accessor + size bytecode over PRIMITIVE-only instance fields.
    /// Such a class never touches the synthetic `$elems`/native state its super
    /// family is served by — its resolved methods are its own bytecode on real
    /// fields — so the JIT gate (`jit_native_served_deep`) can exempt it even
    /// when the arena-alloc migration (RAVA_ARENA_OVERRIDE) is off.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_value_collection_override(&self, name: &str) -> bool {
        let rs = self.runtime_shared();
        if let Some(v) = rs.pure_override_get(name) {
            return v;
        }
        let v = self.pure_value_collection_override_uncached(name);
        Self::debug_assert_not_gilfree("pure_value_override_cache");
        rs.pure_override_insert(name, v);
        v
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_value_collection_override_uncached(&self, name: &str) -> bool {
        let Ok(c) = self.loader.get_or_load(name) else { return false };
        let has = |n: &str, d: &str| c.file.method(n, d).and_then(|m| m.code.as_ref()).is_some();
        let has_accessor = has("getDouble", "(I)D") || has("getInt", "(I)I") || has("get", "(I)Ljava/lang/Object;");
        // SAFETY NARROWING: only migrate a PURE VALUE-COMPUTER — a class whose own
        // instance fields are ALL primitive (B/C/D/F/I/J/S/Z), like CubePointRange (one
        // int). A concrete collection (DoubleArrayList/ObjectArrayList/…) declares a REF
        // BACKING-ARRAY field (`double[] a`, `Object[] a`); migrating it corrupts the
        // backing under arena → wrong-type reads (the non-deterministic MapColor.size ULE
        // / Reference2ObjectArrayMap.size NPE at boot). A primitive-only class has no such
        // backing, so its arithmetic accessor stays correct on the arena.
        let all_prim_fields = c.file.fields.iter().filter(|f| f.access_flags & 0x0008 == 0).all(|f| {
            matches!(
                f.descriptor.as_bytes().first(),
                Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z')
            )
        });
        let ok = has_accessor && has("size", "()I") && all_prim_fields;
        if ok && std::env::var_os("RAVA_TRACE_OVERRIDE").is_some() {
            eprintln!("[OVERRIDE] arena-migrating collection-backed class {name}");
        }
        ok
    }

    // ═══════════ THE PURE VALUE-COMPUTER SUBCLASS (kill switch `RAVA_NO_VALUECOMPUTER=1`) ═══════════
    //
    // ⭐**THE BIRTH PREDICATE AND THE JIT PREDICATE DISAGREED ABOUT THE SAME CLASS.** The JIT
    // side already exempts a "pure value-computer subclass" from the native-served refusal
    // (`jit_native_served_deep_uncached` → `pure_value_collection_override`) and compiles its
    // bytecode; the birth side (`opcodes::new_object_value`, `arena_new_site_uncached`,
    // `try_arena_alloc_by_name`) still forced every subclass of a native-backed collection to a
    // heap handle, because `overrides_collection_backing` was opt-in (`RAVA_ARENA_OVERRIDE`).
    // Counted on the bootstrap bench: **2,480,066 of 2,695,044 handle births (92.02%) are ONE
    // class** — `net/minecraft/world/phys/shapes/CubePointRange`, one `int parts`, `getDouble(i)
    // = i / parts`, `size() = parts + 1` — each a `Box<Object>` + `FieldBlob`, each `parts` read
    // then `rava_getfield` → `class_field_layout` → `slot_of` through the host.
    //
    // ⛔**AND THE OPT-IN FLAG WAS INERT EVEN WHEN SET.** Admitting the class at the ancestor test
    // is not enough: `class_field_layout` walks the super chain and, on the first UNLOADABLE super,
    // marks the layout PARTIAL unless that super is `java/lang/Object` — and every native-backed
    // collection chain ends in an unloadable JDK base (`java/util/AbstractCollection` here). A
    // partial layout is barred from `class_layout_cache`, and all three birth sites fall back to a
    // handle on exactly that absence. So the class needs TWO admissions, and they must be the same
    // predicate: `overrides_collection_backing` (the ancestor term) and
    // `value_computer_layout_complete` (the layout terminator), both answered by
    // `value_computer_admits`.
    //
    // **THE PREDICATE IS STRUCTURAL — bytecode facts of the class and its chain, never a name:**
    //   1. loadable, concrete (not `ACC_INTERFACE`/`ACC_ABSTRACT`), not enum-shaped;
    //   2. every instance field it declares has a PRIMITIVE descriptor (no reference a native
    //      could mistake for `$elems`/`$map` state, and no reference cell the layout would have
    //      to type);
    //   3. it declares, WITH CODE, `size()I` and at least one indexed accessor — a non-static,
    //      non-`<init>` method of descriptor `(I)<non-void>` (the shape `getDouble(I)D`,
    //      `getInt(I)I`, `get(I)Ljava/lang/Object;` all have; chosen by DESCRIPTOR, not name);
    //   4. every LOADABLE strict ancestor is `ACC_ABSTRACT` and declares NO instance field — the
    //      "abstract base whose concrete storage the subclass never inherits" — and the chain's
    //      first UNLOADABLE link is a native-backed name (`birth_native_backed_alloc`), i.e. the
    //      JDK base the ancestor scan fired on. `java/lang/Object` as first unloadable link means
    //      the chain has no native-backed ancestor at all and the predicate is not in scope;
    //   5. the loadable chain declares, WITH CODE, the whole `java/util/Collection` MUTATOR
    //      protocol (`add`/`remove`/`clear`/`addAll`/`removeAll`/`retainAll`) — so no mutator can
    //      ever reach a native that would fabricate `$elems` on the receiver (fastutil's abstract
    //      bases declare all six, throwing `UnsupportedOperationException`);
    //   6. no `CONSTANT_Fieldref` in any loadable chain member's pool names a field of the class
    //      or of one of its ancestors that the loadable chain does not DECLARE — so no bytecode
    //      that can run on this receiver reads a slot the arena layout lacks (an off-layout
    //      `arena_getfield` is a panic, not a wrong answer, and this is what rules it out).
    //
    // **WHY THE GUARD CANNOT ADMIT AN UNSAFE SHAPE.** An arena object of an admitted class has
    // exactly its own primitive fields as cells. What can touch it: (a) its own and its loadable
    // ancestors' bytecode — by (6) every field they reference is in the layout; (b) the natives,
    // reached ONLY for a method with no body anywhere in the loadable chain (the interpreter and
    // `jit_call` are both bytecode-first: `invoke_virtual.rs` tries `try_invoke_chain` on the
    // `None` arm of resolution). By (3)+(5) that set excludes every accessor and every mutator;
    // what is left is the JDK's `AbstractCollection`/`Iterable` defaults. Of those,
    // `stream`/`spliterator`/`iterator`/`forEach` already decode an arena receiver
    // (`stream::dispatch` drains it through `iterate_recv`, which runs the class's own
    // `size()`/`get(int)` bytecode); `<init>` of the unloadable base is `Ok(None)` → no-op on both
    // representations (`invoke.rs` invokespecial fallback); `getClass`/identity arms read the
    // arena header. The one stage that REFUSES an arena receiver outright is `coll_op`
    // (`stream/collections/coll_op.rs`: `Some(Value::Ref(h)) => *h, _ => return Ok(None)`), where a
    // HANDLE receiver without `$elems` is served by draining `iterate_object` — so
    // `value_computer_coll_op` (below) is hooked at its entry and serves an arena receiver of an
    // admitted class by the identical drain for the names that stage answers with no body in
    // the chain: `isEmpty()Z`, `toArray()`, `toArray(T[])`. Any other name reaching it on an
    // arena value-computer is COUNTED (`[VALUECOMP] refused`) and declined, which surfaces as the
    // same missing-native the handle would have hit for a name `coll_op` has no arm for — never
    // a silently different answer.
    //
    // **REPRESENTATION TRAP, honoured:** nothing here dereferences a cell by guessing its tag.
    // The natives-side arm admits only `Value::Obj` that is not an arena string, reads the class id
    // from the arena header, and reaches the fields only through the class's own bytecode or
    // `iterate_recv`. The JIT's inline `getDouble`/`size` arms guard `hi == 1` (a heap handle)
    // before comparing `CUBE_CLASS_WORD`, so an arena receiver takes their callback edge into
    // `fu_fastpath`, whose `CubeRange` arm reads `parts` with `arena_getfield` on `Value::Obj` —
    // it has decoded both representations since row 244.

    /// The structural verdict, memoised per class name per thread (a transient loader refusal
    /// is never memoised). `valuecomputer::ADMIT` admits; every other code names the refusal.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn value_computer_verdict(&self, name: &str) -> u8 {
        thread_local! {
            static MEMO: std::cell::RefCell<FastMap<Box<str>, u8>> =
                std::cell::RefCell::new(FastMap::default());
        }
        if let Some(v) = MEMO.with(|m| m.borrow().get(name).copied()) {
            return v;
        }
        let v = self.value_computer_verdict_uncached(name);
        if v != valuecomputer::LOADER_REFUSED {
            // DIAGNOSTIC (profiler on only): name every ADMITTED class -- the census counts them
            // and a boot that admitted ONE class hung where the bench that admitted three did not.
            if v == valuecomputer::ADMIT && crate::pc_profile::enabled() {
                eprintln!("[VALUECOMP-ADMIT] {name}");
            }
            valuecomputer::bump(&valuecomputer::VERDICTS[v as usize]);
            MEMO.with(|m| {
                m.borrow_mut().insert(name.into(), v);
            });
        }
        v
    }

    /// `value_computer_verdict(name) == ADMIT`, behind the kill switch.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn value_computer_admits(&self, name: &str) -> bool {
        valuecomputer_enabled() && self.value_computer_verdict(name) == valuecomputer::ADMIT
    }

    #[cfg(feature = "next-gen-gc")]
    fn value_computer_verdict_uncached(&self, name: &str) -> u8 {
        use valuecomputer as vc;
        const ACC_STATIC: u16 = 0x0008;
        const ACC_INTERFACE: u16 = 0x0200;
        const ACC_ABSTRACT: u16 = 0x0400;
        const ACC_ENUM: u16 = 0x4000;
        let c = match self.loader.get_or_load(name) {
            Ok(c) => c,
            Err(LoadError::NotFound(_)) => return vc::NOT_LOADABLE,
            Err(_) => return vc::LOADER_REFUSED,
        };
        let f = &c.file;
        if f.access_flags & (ACC_INTERFACE | ACC_ABSTRACT) != 0 {
            return vc::ABSTRACT_OR_INTERFACE;
        }
        if f.access_flags & ACC_ENUM != 0 || f.super_class.as_deref() == Some("java/lang/Enum") {
            return vc::ENUM_SHAPED;
        }
        // (2) primitive-only instance fields.
        let primitive = |d: &str| {
            matches!(d.as_bytes().first(), Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z'))
        };
        if !f
            .fields
            .iter()
            .filter(|x| x.access_flags & ACC_STATIC == 0)
            .all(|x| primitive(&x.descriptor))
        {
            return vc::REF_FIELD;
        }
        // (3) the value protocol, declared with code on the class itself.
        let has_code = |file: &rava_classfile::ClassFile, n: &str, d: &str| {
            file.method(n, d).and_then(|m| m.code.as_ref()).is_some()
        };
        if !has_code(f, "size", "()I") {
            return vc::NO_SIZE;
        }
        if !f.methods.iter().any(|m| {
            m.code.is_some()
                && m.access_flags & ACC_STATIC == 0
                && m.name != "<init>"
                && m.descriptor.starts_with("(I)")
                && !m.descriptor.ends_with('V')
        }) {
            return vc::NO_INDEXED_ACCESSOR;
        }
        // (4) the chain: abstract, fieldless loadable ancestors up to a native-backed unloadable base.
        let chain = self.super_chain(name);
        let mut loadable: Vec<std::sync::Arc<LoadedClass>> = vec![std::sync::Arc::clone(&c)];
        let mut saw_native = false;
        for a in chain.iter().skip(1) {
            match self.loader.get_or_load(a) {
                Ok(ac) => {
                    if crate::opcodes::birth_native_backed_alloc(a) {
                        saw_native = true;
                    }
                    if ac.file.access_flags & ACC_ABSTRACT == 0 {
                        return vc::ANCESTOR_CONCRETE;
                    }
                    if ac.file.fields.iter().any(|x| x.access_flags & ACC_STATIC == 0) {
                        return vc::ANCESTOR_HAS_FIELDS;
                    }
                    loadable.push(ac);
                }
                Err(LoadError::NotFound(_)) => {
                    if &**a == "java/lang/Object" {
                        break;
                    }
                    if !crate::opcodes::birth_native_backed_alloc(a) {
                        return vc::TERMINATOR_NOT_NATIVE;
                    }
                    saw_native = true;
                    break;
                }
                Err(_) => return vc::LOADER_REFUSED,
            }
        }
        if !saw_native {
            return vc::NO_NATIVE_ANCESTOR;
        }
        // (5) every Collection mutator has a body in the loadable chain.
        const MUTATORS: [(&str, &str); 6] = [
            ("add", "(Ljava/lang/Object;)Z"),
            ("remove", "(Ljava/lang/Object;)Z"),
            ("clear", "()V"),
            ("addAll", "(Ljava/util/Collection;)Z"),
            ("removeAll", "(Ljava/util/Collection;)Z"),
            ("retainAll", "(Ljava/util/Collection;)Z"),
        ];
        for (n, d) in MUTATORS {
            if !loadable.iter().any(|lc| has_code(&lc.file, n, d)) {
                return vc::MUTATOR_NOT_DECLARED;
            }
        }
        // (6) no field reference into the chain that the loadable chain does not declare.
        let declared = |n: &str, d: &str| {
            loadable
                .iter()
                .any(|lc| lc.file.fields.iter().any(|x| x.name == n && x.descriptor == d))
        };
        for lc in &loadable {
            let pool = &lc.file.pool;
            let n = pool.len().min(u16::MAX as usize) as u16;
            for i in 0..n {
                if !matches!(pool.get(i), Some(rava_classfile::Constant::FieldRef { .. })) {
                    continue;
                }
                let Some(mr) = pool.member_ref(i) else {
                    return vc::FOREIGN_FIELD_REF;
                };
                let in_chain = mr.owner == name || chain.iter().any(|x| &**x == mr.owner);
                if in_chain && !declared(mr.name, mr.descriptor) {
                    return vc::FOREIGN_FIELD_REF;
                }
            }
        }
        vc::ADMIT
    }

    /// LAYOUT HOOK (`vm/layout.rs`, the `Err` arm of `class_field_layout`'s super walk): may the
    /// walk that stopped at unloadable `terminator` while laying out `class` call the layout
    /// COMPLETE? Only for an admitted value-computer whose FIRST definitively-unloadable
    /// (`LoadError::NotFound`) link is exactly `terminator` — the link the verdict examined. A
    /// transient refusal is not "unloadable" and never completes a layout; `java/lang/Object` is
    /// the historic arm's business and is refused here so the two arms never both claim a chain.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn value_computer_layout_complete(&self, class: &str, terminator: &str) -> bool {
        if !valuecomputer_enabled() || terminator == "java/lang/Object" {
            return false;
        }
        if !crate::opcodes::birth_native_backed_alloc(terminator) || !self.value_computer_admits(class) {
            return false;
        }
        for a in self.super_chain(class).iter().skip(1) {
            match self.loader.get_or_load(a) {
                Ok(_) => continue,
                Err(LoadError::NotFound(_)) => return &**a == terminator,
                Err(_) => return false,
            }
        }
        false
    }

    /// `value_computer_admits` keyed by the receiver's DENSE ARENA CLASS ID — the natives-side
    /// question, asked with a pointer in hand and no name. Same append-only-registry argument as
    /// `jit_native_served_deep_id`: an id names one immutable record forever, so the per-thread
    /// byte table can only hold answers the name memo would also hold. A transient loader
    /// refusal is not stored.
    #[cfg(feature = "next-gen-gc")]
    fn value_computer_admits_id(&self, class_id: u32) -> bool {
        const CAP: usize = 1 << 20;
        thread_local! {
            static BY_ID: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let i = class_id as usize;
        match BY_ID.with(|t| t.borrow().get(i).copied().unwrap_or(0)) {
            1 => return false,
            2 => return true,
            _ => {}
        }
        let Some(name) = crate::nextgen::class_name(class_id) else { return false };
        let v = self.value_computer_verdict(&name);
        if v != valuecomputer::LOADER_REFUSED && i < CAP {
            BY_ID.with(|t| {
                let mut t = t.borrow_mut();
                if i >= t.len() {
                    t.resize(i + 1, 0);
                }
                t[i] = 1 + (v == valuecomputer::ADMIT) as u8;
            });
        }
        v == valuecomputer::ADMIT
    }

    /// NATIVES-SIDE ARM, hooked at the entry of `coll_op` (`stream/collections/coll_op.rs`),
    /// which otherwise refuses every non-`Value::Ref` receiver. For an ARENA receiver whose class
    /// is an admitted value-computer, answer the `AbstractCollection` bodies the chain does not
    /// declare exactly as the handle path answers them for a handle without `$elems` — by the
    /// class's own `size()`/`get(int)` bytecode (`iterate_recv`, the arena twin of the
    /// `iterate_object` drain `elems_of` uses):
    ///   * `isEmpty()Z`    — `size() == 0` (the JDK's own definition; the handle path's drain
    ///                       is empty iff `size()` is 0);
    ///   * `toArray()`     — `coll_op`'s arm verbatim over the drained elements;
    ///   * `toArray(T[])`  — `coll_op`'s arm verbatim: fill the argument when it is long enough
    ///                       and null-terminate, else a fresh array of its component type.
    /// Anything else on such a receiver is counted as REFUSED and declined (`Ok(None)`), which is
    /// the missing-native outcome, never a fabricated answer. A `Value::Ref`, an arena string, or
    /// a class the predicate refuses returns `Ok(None)` after one tag test and one byte load, so
    /// the hook costs the historic `coll_op` nothing it can observe.
    ///
    /// GC: the receiver is rooted by the bridge's `NativeArgsGuard` for the whole call; the
    /// `Value::Obj` handed to `invoke` is rooted by the callee frame; `iterate_recv` accumulates
    /// in `gc_protect`. The `T[]` argument is a heap (non-moving) array handle.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn value_computer_coll_op(
        &mut self,
        name: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Result<Option<Option<Value>>, VmError> {
        use valuecomputer as vc;
        let Some(Value::Obj(p)) = args.first() else { return Ok(None) };
        let p = *p;
        if !valuecomputer_enabled() || crate::nextgen::is_arena_string(p) {
            return Ok(None);
        }
        let cid = crate::nextgen::arena_class_id(p);
        if !self.value_computer_admits_id(cid) {
            return Ok(None);
        }
        let recv = Value::Obj(p);
        match (name, descriptor) {
            // The native-backed base's constructor, reached by the admitted class's own `super()`
            // chain (`AbstractCollection.<init>()V` has no body to run and no `$elems` to mint for
            // an arena object). Void, done. Named by the `[VALUECOMP-DECLINE]` diagnostic as the
            // ONLY declined call on a boot that hung; served here so the cascade below it is
            // never reached for a constructor.
            ("<init>", "()V") => {
                vc::bump(&vc::SERVED_ISEMPTY);
                Ok(Some(None))
            }
            ("isEmpty", "()Z") => {
                let Some(cls) = crate::nextgen::class_name(cid) else { return Ok(None) };
                let Some(decl) = self.resolve_method(&cls, "size", "()I")? else {
                    vc::bump(&vc::REFUSED_ARENA);
                    return Ok(None);
                };
                let n = self.invoke(decl, "size", "()I", vec![recv])?.and_then(|v| v.as_int()).unwrap_or(0);
                vc::bump(&vc::SERVED_ISEMPTY);
                Ok(Some(Some(Value::Int((n == 0) as i32))))
            }
            ("toArray", "()[Ljava/lang/Object;") => {
                let elems = self.iterate_recv(&recv)?;
                vc::bump(&vc::SERVED_TOARRAY);
                Ok(Some(Some(Value::Array(self.heap.alloc_values(crate::stream::OBJ, elems)))))
            }
            ("toArray", _) if args.len() > 1 && matches!(args.get(1), Some(Value::Array(_))) => {
                let elems = self.iterate_recv(&recv)?;
                let elem = crate::stream::source_array_element(self, args.get(1));
                let n = elems.len();
                let v = match args.get(1) {
                    Some(Value::Array(h)) if self.heap.array_len(*h).unwrap_or(0) >= n => {
                        let h = *h;
                        let cap = self.heap.array_len(h).unwrap_or(0);
                        for (i, e) in elems.into_iter().enumerate() {
                            self.heap.array_set(h, i, e);
                        }
                        if cap > n {
                            self.heap.array_set(h, n, Value::Null);
                        }
                        Value::Array(h)
                    }
                    _ => Value::Array(self.heap.alloc_values(&elem, elems)),
                };
                vc::bump(&vc::SERVED_TOARRAY);
                Ok(Some(Some(v)))
            }
            _ => {
                vc::bump(&vc::REFUSED_ARENA);
                // DIAGNOSTIC (profiler on only): NAME the declined (method, descriptor) pairs, once
                // each -- the census counted 602 of them on a boot that then hung, and a count
                // cannot say which native the decline fell through to.
                if crate::pc_profile::enabled() {
                    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
                    let key = format!("{name}{descriptor}");
                    if let Ok(mut g) = SEEN.lock() {
                        if !g.contains(&key) && g.len() < 64 {
                            let cls = crate::nextgen::class_name(cid).unwrap_or_default();
                            eprintln!("[VALUECOMP-DECLINE] {cls}.{key}");
                            g.push(key);
                        }
                    }
                }
                Ok(None)
            }
        }
    }

    /// `[VALUECOMP]` — printed from the periodic profiler dump beside `compile_bail::report`.
    /// Class verdicts by reason (one row per class per deriving thread), and the arena-receiver
    /// population the natives-side arm served or refused.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn valuecomputer_report() -> String {
        use std::sync::atomic::Ordering::Relaxed;
        use valuecomputer as vc;
        let verdicts: Vec<u64> = vc::VERDICTS.iter().map(|c| c.load(Relaxed)).collect();
        let total: u64 = verdicts.iter().sum();
        let served = vc::SERVED_ISEMPTY.load(Relaxed) + vc::SERVED_TOARRAY.load(Relaxed);
        let refused = vc::REFUSED_ARENA.load(Relaxed);
        if total == 0 && served == 0 && refused == 0 {
            return String::new();
        }
        let mut out = format!(
            "[VALUECOMP] {} class verdicts under a native-backed ancestor, {} ADMITTED to the arena{} | arena receivers in coll_op: isEmpty {} toArray {} | REFUSED (arena value-computer, name with no arm) {}",
            total,
            verdicts[vc::ADMIT as usize],
            if valuecomputer_enabled() { "" } else { " (RAVA_NO_VALUECOMPUTER: predicate off)" },
            vc::SERVED_ISEMPTY.load(Relaxed),
            vc::SERVED_TOARRAY.load(Relaxed),
            refused
        );
        let mut rows: Vec<(u64, &str)> =
            verdicts.iter().copied().zip(vc::REASONS).filter(|r| r.0 > 0).collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        for (n, s) in rows {
            out.push_str(&format!("\n[VALUECOMP]   {n:>8}  {s}"));
        }
        out
    }

    /// Does `class` transitively implement/extend interface `target`?
    /// Walks the superclass chain and the (super-)interface closure.
    pub(crate) fn implements_iface(&mut self, class: &str, target: &str) -> bool {
        // The BFS this used to run lives in `ClassLoader::type_closure`, memoised. It was
        // the boot's single largest source of class lookups — 78.4% of a sampled census —
        // because the caller asks "is this a Map?" per key derivation and a NEGATIVE
        // answer costs the whole closure.
        self.loader.type_closure(class).contains(target)
    }
}

/// Without the feature no arena exists, so the report has nothing to say (the dump site is
/// feature-agnostic, like `birth_census_report`'s stub).
#[cfg(not(feature = "next-gen-gc"))]
impl Vm {
    pub(crate) fn valuecomputer_report() -> String {
        String::new()
    }
}

/// Is the value-computer admission active? `RAVA_NO_VALUECOMPUTER=1` (or the block switch
/// `RAVA_NO_EMIT_MORE=1`) restores the historic behaviour EXACTLY: `overrides_collection_backing`
/// falls back to its opt-in `RAVA_ARENA_OVERRIDE` arm, the layout hook answers `false`, and the
/// natives-side arm answers `Ok(None)` before touching the receiver. Resolved once.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn valuecomputer_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_VALUECOMPUTER").is_none()
    })
}

/// Verdict codes and the `[VALUECOMP]` counters. COUNTERS ARE PLAIN CELLS (`load` + `store`,
/// never a locked RMW): the verdict cells tick once per class per deriving thread, the serve
/// cells only on the arena arm, which is reached by no call the historic path can observe.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod valuecomputer {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub(crate) const ADMIT: u8 = 0;
    pub(crate) const NOT_LOADABLE: u8 = 1;
    pub(crate) const ABSTRACT_OR_INTERFACE: u8 = 2;
    pub(crate) const ENUM_SHAPED: u8 = 3;
    pub(crate) const REF_FIELD: u8 = 4;
    pub(crate) const NO_SIZE: u8 = 5;
    pub(crate) const NO_INDEXED_ACCESSOR: u8 = 6;
    pub(crate) const ANCESTOR_CONCRETE: u8 = 7;
    pub(crate) const ANCESTOR_HAS_FIELDS: u8 = 8;
    pub(crate) const TERMINATOR_NOT_NATIVE: u8 = 9;
    pub(crate) const NO_NATIVE_ANCESTOR: u8 = 10;
    pub(crate) const MUTATOR_NOT_DECLARED: u8 = 11;
    pub(crate) const FOREIGN_FIELD_REF: u8 = 12;
    /// A transient loader refusal (GIL-free worker) — never memoised, never counted.
    pub(crate) const LOADER_REFUSED: u8 = 13;

    pub(crate) const REASONS: [&str; 13] = [
        "ADMIT: own size()+indexed accessor bytecode, primitive fields, abstract fieldless chain",
        "refused: class not loadable",
        "refused: class is abstract or an interface",
        "refused: enum-shaped",
        "refused: declares a reference-typed instance field",
        "refused: no size()I body on the class",
        "refused: no (I)<value> accessor body on the class",
        "refused: a loadable ancestor is concrete",
        "refused: a loadable ancestor declares an instance field",
        "refused: first unloadable ancestor is not native-backed",
        "refused: no native-backed ancestor at all (not in scope)",
        "refused: a Collection mutator has no body in the chain",
        "refused: a Fieldref into the chain names an undeclared field",
    ];
    pub(crate) static VERDICTS: [AtomicU64; 13] = [const { AtomicU64::new(0) }; 13];
    pub(crate) static SERVED_ISEMPTY: AtomicU64 = AtomicU64::new(0);
    pub(crate) static SERVED_TOARRAY: AtomicU64 = AtomicU64::new(0);
    pub(crate) static REFUSED_ARENA: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub(crate) fn bump(c: &AtomicU64) {
        c.store(c.load(Relaxed) + 1, Relaxed);
    }
}
