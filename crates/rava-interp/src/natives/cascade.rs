//! The owner cascade: which native family serves a call, decided by the receiver's class.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// `gates` is the caller's ALREADY-LOADED gate word (see [`cascade_gates`] and the block comment
/// on `RAVA_NO_FAST_CASCADE5` there). It is not re-read here: `dispatch_owner` — this function's
/// only caller — has held it since the bridge prologue.
pub(crate) fn dispatch_owner_cascade(
    vm: &mut Vm,
    gates: u32,
    plan: u32,
    verify: bool,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
    family_fact: crate::owner_bits::NativeFamilyFact,
) -> Result<Option<Option<Value>>, VmError> {
    use crate::owner_bits as ob;
    // ── ONE GATE WORD, ONE FINGERPRINT (RAVA_NO_FAST_CASCADE2) ────────────────────────────────
    // Two kinds of per-call work below answer a question whose answer cannot change, and this
    // prologue answers both once.
    //
    // 1. EIGHT SEPARATE PROCESS-CONSTANT READS. Every fast path in this function opened with its
    //    own `OnceLock<bool>` — trivial, entry, iterator-create, iterator-step, the tail filter,
    //    its verify mode, the census — and each is a DISTINCT static, so each is its own load,
    //    its own cache line and its own branch, and the compiler cannot hoist any of them because
    //    the dispatchers called in between may write. They are folded into ONE word, read once
    //    here; every use below is a bit test on a register. Each bit is exactly the predicate it
    //    replaces, initialised from exactly the same env var, so no flag changes meaning.
    //
    // 2. ~35 OWNER STRING COMPARES ON A CALL NO INLINE ARM CAN SERVE. Everything in this function
    //    that keys on an owner keys on a LITERAL, so which literals could possibly match is
    //    decidable from the owner alone. `owner_sig` — length plus three bytes, ~10 instructions,
    //    no memo, no lock, no allocation — plus one `.rodata` bit test answers it. A rejected
    //    owner is not "probably not ours": it is provably in none of the literal sets, so every
    //    one of those arms would have fallen through to exactly where the gate lands. The prefix
    //    arms (netty transports, JFR) and the name-keyed arms (wait/notify, hasNext/next,
    //    iterator) are NOT gated — a prefix has no fingerprint and a name-keyed arm has no owner
    //    — so they run in place, in order, exactly as before.
    //
    // The fingerprint was ALREADY being computed further down for the two tail tables; hoisting
    // it means a call that reaches the tail pays for it exactly once, as it did before.
    //
    // ── AND THE WORD ITSELF IS NOW THE CALLER'S (RAVA_NO_FAST_CASCADE5) ───────────────────────
    // ONE FACT — *what did the environment say* — WAS READ FROM THREE SEPARATE `OnceLock`s PER
    // NATIVE CALL: `entry_gates()` in the bridge prologue, `gate_mode()` in `dispatch_owner`, and
    // this line. Three acquire loads from three statics, three branches, and none of them
    // hoistable or CSE-able across the dispatchers in between. They are one word now, read once
    // at the top of the bridge and threaded down through `try_invoke_inner` → `dispatch_owner` →
    // here → `try_last_arm`, exactly as `entry_gates()` was already threaded into
    // `canon_arena_strings` and for exactly the same reason. Every bit is the same predicate,
    // built from the same accessor, so no flag changes meaning.
    //
    // Under the kill switch each layer reads its own word again at its own site — the READS are
    // what this round removes, not the answers, which never differed.
    let gates = if gates & G_LEGACY5 != 0 { cascade_gates() } else { gates };
    // SELF TIME OF EVERY NATIVE. The re-priced map accounts for ~6% of the boot; the rest is
    // this — the Rust implementations of `java.*` — and no instrument had ever priced it as a
    // population. Meters now subtract their metered children, so a native that runs guest code
    // (`forEach`, `thenCompose`) charges the guest's interpretation to the interpreter's meter
    // and keeps only its own work here.
    //
    // ── FOUR INSTRUMENT HANDLES ON A ROUTING PATH THAT INSTRUMENTS NOTHING (RAVA_NO_FAST_CASCADE3)
    // `native_self`, `native_lastarm`, `native_stages` and `native_services` are the ONLY four
    // meters in this function, all four are read on EVERY cascade entry, and each read is a
    // `OnceLock<Meter>` — an acquire load from its own static, a branch, a 24-byte handle copy,
    // then `start()`'s branch on `armed` and a drop-glue branch on the way out. None of them can
    // be hoisted or merged: the dispatchers called in between are opaque to the optimiser, which
    // is the same reason one relaxed atomic on the interpreter's push path measured -15%.
    //
    // A meter is `armed` only if `RAVA_DIAG` selected its category, so when NOTHING is armed all
    // four guards are already no-ops — the work is the four reads that discover that. `G_METER`
    // is `diag::any_armed()`, ONE read folded into the gate word, and a clear bit proves every
    // one of the four is unarmed (`selected()` returns false unconditionally on an empty
    // selector), so not constructing them IS their behaviour, not an approximation of it. When
    // anything is armed the bit is set and all four exist exactly as before, in the same order,
    // with the same nesting — so the self-time subtraction arithmetic is untouched.
    //
    // ⚠The `native_self` region now begins AFTER the gate-word read rather than before it: an
    // armed run charges those ~10 instructions to the caller instead. Deliberate — the guard has
    // to be behind the bit that decides whether to build it.
    let _nat = (gates & G_METER != 0).then(|| crate::diag::native_self().start());
    let sig =
        if gates & (G_FAST1 | G_FAST2 | G_FAST3) != 0 { owner_sig(owner) } else { SIG_ADMIT };
    // ⭐**ONE ADMIT BIT OVER TWO LADDERS (RAVA_NO_FAST_CASCADE4).** The inline arms are NOT one
    // run: the five service stages and the name-keyed monitor arm sit between them, so the arms
    // form a head ladder and a tail ladder, and `inline_owner` — a bit over the UNION of their
    // literals — admits an owner to BOTH whenever it belongs to EITHER. A head-only owner
    // (`Boolean`, `Character`, `BitSet`, `Optional`, the `Map$Entry` shapes) whose name the head
    // does not serve then walks all twenty-one tail compares; a tail-only owner
    // (`java/lang/Object`, the `Reference` family, the `Random`s, the `Charset`s) pays the head's.
    // Two more `.rodata` probes on the fingerprint ALREADY IN HAND answer each segment
    // separately, and each list is smaller than their union, so each also rejects strictly more
    // jar classes. Every arm below carries the bit for the segment it is in; a `false` bit is the
    // proof that its `owner ==` compare would have failed.
    //
    // Gated on G_FAST2 as well as G_FAST4, so `RAVA_NO_FAST_CASCADE2=1` still restores the whole
    // unfiltered ladder in one run rather than leaving a NARROWER filter behind it.
    //
    // ⭐**AND THE UNION PROBE IS A THIRD DERIVATION OF A FACT THE TWO SEGMENT PROBES ALREADY
    // CARRY (RAVA_NO_FAST_CASCADE5).** With the split on, `inline_head` was
    // `inline_may_serve(sig) && inline_head_may_serve(sig)` and `inline_tail` the same with the
    // other segment — so the SAME fingerprint indexed THREE bitmaps on every one of the ~19 M
    // cascade entries a boot makes, and the first of the three could never change either answer.
    // `neither_segment_admits_what_the_union_rejects` below proves that exhaustively over all 512
    // fingerprints, and the two drift checks keep it true: every segment literal must also be in
    // `OWNERS_INLINE`, so each segment map's set bits are a SUBSET of the union map's.
    // head ⟹ union and tail ⟹ union, therefore `union && head == head`. Dropping the conjunct is
    // an identity, not an approximation, and the `debug_assert` re-checks it on every call of a
    // debug build.
    //
    // The union bit is still REQUIRED in two configurations and is computed there exactly as
    // before: with the split off (`RAVA_NO_FAST_CASCADE4`/`2`) it IS the whole filter, and with
    // `RAVA_CASCADE_STAT` armed it is a reported number (`CASCADE2_SKIP` counts entries the union
    // rejected, which is a different fact from the two segment counts beside it).
    let split = gates & (G_FAST4 | G_FAST2) == (G_FAST4 | G_FAST2);
    let inline_head;
    let inline_tail;
    if split && gates & (G_STAT | G_LEGACY5) == 0 {
        inline_head = inline_head_may_serve(sig);
        inline_tail = inline_tail_may_serve(sig);
        debug_assert!(
            (!inline_head && !inline_tail) || inline_may_serve(sig),
            "a segment filter admitted a fingerprint the union rejects: the segment lists have \
             drifted out of OWNERS_INLINE and `inline_owner` is no longer implied"
        );
    } else {
        let inline_owner = gates & G_FAST2 == 0 || inline_may_serve(sig);
        if gates & G_STAT != 0 && !inline_owner {
            CASCADE2_SKIP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        inline_head = inline_owner && (!split || inline_head_may_serve(sig));
        inline_tail = inline_owner && (!split || inline_tail_may_serve(sig));
        if gates & G_STAT != 0 && inline_owner {
            use std::sync::atomic::Ordering::Relaxed;
            if !inline_head {
                CASCADE4_SKIP_HEAD.fetch_add(1, Relaxed);
            }
            if !inline_tail {
                CASCADE4_SKIP_TAIL.fetch_add(1, Relaxed);
            }
        }
    }
    // ⚠**THESE RUN BEFORE THE LAST-ARM SHORTCUT, and that is the whole point.** Placed after it,
    // the `Class.cast` arm below NEVER EXECUTED: a loop that calls one native repeatedly hits
    // `try_last_arm` every time, so the shortcut kept serving `cast` from `reflect::dispatch` and
    // the measurement read 735 ns with the fast path both on and off. A shortcut to the OLD arm
    // is not a shortcut past it.
    // FAST-PATH the BOX UNWRAPPERS. `bench/SeamCost.java` prices `Boolean.booleanValue` at
    // **1,285 ns** against HotSpot's 0 and against this engine's own `Map.get` at 98 ns — for a
    // one-field read. The JIT-callback census puts it at 4.78% of the boot's 64,088,346
    // compiled-to-host crossings (~3.06M calls), so the route to its arm costs seconds.
    //
    // ⚠It is the ROUTE, not the arm: with `RAVA_DISPATCH_GATE` on (which skips sub-dispatchers an
    // owner cannot reach) the probe reads 1,315 ns instead of 1,400 — the gauntlet is not where
    // the time is, the *distance* to `cascade_table` is. Serving it here is byte-identical to
    // that arm (`Value::Int(bool_of(..))`, same `Ok(Some(Some(..)))` shape) and only shortens
    // the route, exactly as the `Map$Entry` and `Optional` fast paths above do.
    if gates & G_TRIVIAL != 0 && inline_head {
        match (owner, name) {
            ("java/lang/Boolean", "booleanValue") => {
                // ⭐**THE FIELD IS A CONSTANT OF THE SITE; THE LOOKUP WAS NOT.** Shortening the
                // ROUTE to this arm was the whole point of hoisting it here — and the arm it
                // reaches still resolves its field BY NAME. `bool_of` on a boxed receiver is
                // `get_field(h, "$value")`, which is `field_id_existing("$value")` (a TLS L1
                // probe plus a `memcmp` of the name, with the interner's `RwLock` and FxHash
                // behind it whenever that misses) followed by the slot walk — per call, on a
                // one-field read that is 4.78% of the boot's compiled→host crossings, ~3.06 M
                // calls, and whose receiver is one of the TWO cached handles `Vm::boolean_obj`
                // ever hands out.
                //
                // Resolved ONCE into a `OnceLock<u32>` and read through `get_field_hint`, exactly
                // as the `$elems`/`$pos` iterator arm below does and for exactly the same reason.
                // Byte-identical: the same id reaches the same slot, and the slot HINT is
                // VALIDATED inside `get_field_hint` (`ids[hi] == id`, else the same `find_id`
                // scan `get_field` performs), so a stale hint from a differently-shaped object is
                // a slower read, never a wrong field. Ids are cached only once RESOLVED — caching
                // a `None` taken before any `java/lang/Boolean` was born would disable this
                // forever — and a receiver that is not a handle (an already-unboxed
                // `Value::Int`, the common shape) still goes to `bool_of`, which reads it
                // straight off the `Value` and never touches a field at all.
                if gates & G_FAST4 != 0 {
                    if let Some(Value::Ref(h)) = args.first() {
                        static VALUE_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
                        static VALUE_HINT: std::sync::atomic::AtomicU32 =
                            std::sync::atomic::AtomicU32::new(0);
                        let id = match VALUE_ID.get() {
                            Some(&id) => Some(id),
                            None => match vm.heap.field_id_opt("$value") {
                                Some(id) => {
                                    let _ = VALUE_ID.set(id);
                                    Some(id)
                                }
                                None => None,
                            },
                        };
                        if let Some(id) = id {
                            use std::sync::atomic::Ordering::Relaxed;
                            let (v, hi) =
                                vm.heap.get_field_hint(*h, id, VALUE_HINT.load(Relaxed));
                            VALUE_HINT.store(hi, Relaxed);
                            return Ok(Some(Some(Value::Int(v.as_int().unwrap_or(0)))));
                        }
                    }
                }
                return Ok(Some(Some(Value::Int(crate::natives::bool_of(vm, args.first())))));
            }
            ("java/lang/Integer", "intValue")
            | ("java/lang/Short", "shortValue")
            | ("java/lang/Byte", "byteValue")
            | ("java/lang/Character", "charValue") => {
                if let Some(v) = args.first().and_then(Value::as_int) {
                    return Ok(Some(Some(Value::Int(v))));
                }
            }
            ("java/lang/Long", "longValue") => {
                if let Some(v) = args.first().and_then(Value::as_long) {
                    return Ok(Some(Some(Value::Long(v))));
                }
            }
            _ => {}
        }
    }
    // FAST-PATH `java.util.Objects`: 3.92% of the boot's crossings for `equals` alone plus 1.40%
    // for `requireNonNull`, and its arm is in `cascade_table` — the same distance the box
    // unwrappers were paying. Route only; `objects` IS the arm.
    if gates & G_TRIVIAL != 0 && inline_head && owner == "java/util/Objects" {
        if let Some(r) = crate::natives::misc::objects(vm, name, args)? {
            return Ok(Some(r));
        }
    }
    // FAST-PATH the route to `sb_dispatch`. `Appendable.append` is 1.83% of the boot's crossings
    // and `bench/SeamCost3.java` prices a setLength+append+length triple at **4,285 ns against
    // HotSpot's 2** — the largest single product left in the census. Route only; `sb_dispatch` IS
    // the arm, reached without the owner scan.
    if gates & G_TRIVIAL != 0
        && inline_head
        && matches!(owner, "java/lang/StringBuilder" | "java/lang/StringBuffer")
    {
        if let Some(r) = crate::natives::strings::sb_dispatch(vm, name, descriptor, args)? {
            return Ok(Some(r));
        }
    }
    // FAST-PATH `java.util.BitSet`: 5.83% of the boot's 64,088,346 crossings across
    // clear/set/get/nextClearBit/nextSetBit/<init>/clone, and its arm sits behind
    // `cascade_owners`' owner scan. This is a ROUTE change only — the same `bitset_dispatch`
    // that arm calls, reached without the walk.
    if gates & G_TRIVIAL != 0 && inline_head && owner == "java/util/BitSet" {
        if let Some(r) = crate::natives::buffers::bitset_dispatch(vm, name, descriptor, args) {
            return Ok(Some(r));
        }
    }
    // FAST-PATH `Class.cast`, the census's LARGEST single seam callee: 7.67% of 64,088,346
    // compiled-to-host crossings, ~4.9M calls, and its arm lives in `reflect::dispatch` —
    // further down the cascade than `cascade_table`, whose route the box unwrappers above
    // measured at ~950 ns. Byte-identical to that arm: the same `$name`, the same three-valued
    // `instance_of_tri` (Unknown passes), the same null rule (cast(null) is null, never a
    // throw), the same `ClassCastException` message.
    if gates & G_TRIVIAL != 0 && inline_head && owner == "java/lang/Class" && name == "cast" {
        return Ok(Some(Some(
            crate::natives::program::class_cast(vm, args)?,
        )));
    }

    // LAST-ARM SHORTCUT first: a hit skips six stage dispatchers and up to 42 owner string
    // comparisons. It re-checks the arm's own owner condition, so a miss is only a compare.
    // SPLIT THE 11.6%. `native_self` prices every Rust native at 7.95 M x 480.7 ns = 3.82 s, the
    // largest measured line in the engine — but "natives" is not a target, a named family is.
    // These sub-meters report SELF time, so a stage that delegates to another metered stage does
    // not absorb it.
    //
    // ⚠**AND SCOPE THE GUARD, NOT JUST THE SUBTREE.** The first version bound `_la` here without
    // a scope, so it lived to the END of this function and reported the whole remaining cascade
    // under the name of the shortcut. Same family of error as the callee trap, different cause:
    // there the guard covered code it called, here it covered code that merely followed it.
    //
    // ── AND THE SHORTCUT ITSELF IS NOW BEHIND THE OWNER WORD (RAVA_NO_FAST_CASCADE3) ──────────
    // `try_last_arm` is a LAYER, and it was the one layer of this function still consulted after
    // the fingerprint had already excluded it. Its own counters read `hit=1,867,032
    // miss=18,011,344`: **eighteen million calls pay the lookup to be told "not this arm"**, and
    // the lookup is not free — `arm_key` reads both string pointers, then `ARM_CACHE.with` is a
    // Windows TLS-index load, then a `RefCell` read borrow (a flag load, a check, a store, and a
    // restore on drop), plus its own `native_armlookup` handle and the `native_lastarm` handle
    // wrapping it. Five loads and a TLS access to reach `Ok(None)`.
    //
    // ⭐**AND THE ANSWER FOR MOST OF THEM IS DECIDABLE FROM THE OWNER ALONE.** Every arm the
    // shortcut can take re-checks its own owner LITERAL before running (`ARM_INT if owner ==
    // "java/lang/Integer" || ...`) and the match's only other exit is `_ => None`. Thirteen
    // literals, in `OWNERS_LASTARM` below, and `sig` is already in hand from the prologue — so
    // one `.rodata` bit test decides it. A rejected owner is not "probably not cached": the
    // cache may hold an arm for it (the key is a pointer pair, not a name) and that arm would
    // STILL fall through the owner re-check to `None`. Skipping the call IS its answer.
    //
    // `try_last_arm` is pure on that path — no stores, no counters of its own, no allocation —
    // so a skip has nothing to reproduce. `ARM_MISS` below is still incremented, so the
    // shortcut's own census reads exactly the number it read before.
    let la = gates & G_FAST3 == 0 || lastarm_may_serve(sig);
    let last = if la || gates & G_FAST3V != 0 {
        let _la = (gates & G_METER != 0).then(|| crate::diag::native_lastarm().start());
        // ⭐**THE FIFTH METER HANDLE — the one the G_METER fold missed (RAVA_NO_FAST_CASCADE5).**
        // `try_last_arm` opens with `crate::diag::native_armlookup().start()`, UNCONDITIONALLY, on
        // every one of the ~19.9 M calls that reach it (its own census: hit=1,867,032
        // miss=18,011,344). That is a sixth reading of "is any instrument armed at all" — a
        // three-dependent-load Windows TLS probe in `none_armed()` — on a call whose caller has
        // held the answer in `G_METER` since the prologue. The word goes in as an argument, the
        // same treatment `native_lastarm` above already has, and the shortcut also stops reading
        // `arena_map::enabled()` from its own atomic for the same reason.
        let r = try_last_arm(vm, gates, owner, name, descriptor, args)?;
        if !la && r.is_some() {
            fast_cascade_miss("try_last_arm", owner, name, descriptor);
        }
        r
    } else {
        // Proven unserved. This IS the `_ => None` arm of that match, reached without the key,
        // the TLS access and the borrow.
        if gates & G_STAT != 0 {
            CASCADE3_SKIP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        None
    };
    // Everything from here down is the stage gauntlet; `native_stream` and `native_map` are
    // metered inside it and subtract themselves, so this reports the OTHER stages.
    let _st = (gates & G_METER != 0).then(|| crate::diag::native_stages().start());
    // ⭐**THE LAST UNCONDITIONAL WRITE ON THE CROSSING (`RAVA_NO_CALLSEQ`).** `ARM_MISS` is a
    // `fetch_add` — a `lock xadd` — on EVERY cascade entry the shortcut does not serve, which its
    // own census puts at 18,011,344 of 19,878,376. Rounds 2-5 took the READS off this path one
    // word at a time and left the write, and a write is the worse of the two: an atomic RMW is
    // opaque to alias analysis, so it also pins every value the caller holds across it and blocks
    // the sinking of everything below. Nothing in a default run can observe it — see
    // [`G_ARMSTAT`], which is set whenever anything that could PRINT these two is armed, and
    // unconditionally under the kill switch.
    let armstat = gates & G_ARMSTAT != 0;
    match last {
        Some(r) => {
            if armstat {
                ARM_HIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return Ok(Some(r));
        }
        None => {
            if armstat {
                ARM_MISS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    // A stage runs when its plan bit is set — or always, under verify, so a bit
    // that is wrong shows up as a `gate_stage_miss` on a real workload.
    let stage = |bit: u32| plan & bit != 0 || verify;
    // NOTE: the arg buffer is GC-rooted by the caller's `NativeArgsGuard` (Wall #19)
    // and any arena-string args are already canonicalized — both live in the bridge
    // prologue so a super-chain walk pays them once, not once per owner tried.
    // FAST-PATH the trivial native Map$Entry accessors (getKey/getValue): during block-state
    // property iteration these are called ~4M+ times, and each otherwise scans the whole
    // nio/net/stream dispatch chain + the map-dispatch owner traversal before reaching
    // entry_method. Serve the `$key`/`$val` field read here directly — byte-identical to
    // entry_method (same field, same Ok(Some(Some(..))) shape; a non-Ref receiver still falls
    // through to the normal path, which entry_method maps to Ok(None)).
    if gates & G_ENTRY != 0
        && inline_head
        && matches!(name, "getKey" | "getValue")
        && matches!(
            owner,
            "java/util/Map$Entry"
                | "java/util/AbstractMap$SimpleEntry"
                | "java/util/AbstractMap$SimpleImmutableEntry"
        )
    {
        let f = if name == "getKey" { "$key" } else { "$val" };
        if let Some(Value::Ref(h)) = args.first() {
            return Ok(Some(Some(vm.heap.get_field(*h, f))));
        }
        // The arena form of the same object (row 437). Without this arm the fast path silently
        // stops firing for 48% of entries the moment they stop being handles — the shortcut
        // would still be "hit", just never taken.
        #[cfg(feature = "next-gen-gc")]
        if let Some(recv @ Value::Obj(_)) = args.first() {
            let recv = recv.clone();
            return Ok(Some(Some(vm.synth_get(&recv, f))));
        }
    }
    // FAST-PATH `Optional.of/ofNullable/empty`: 752K calls in the datapack load for two
    // heap writes of work, each otherwise walking the whole nio/net/stream owner scan to
    // reach `stream::dispatch`. Byte-identical to that arm (same `make_optional`, same
    // null→empty filter) — this only shortens the route to it.
    if gates & G_TRIVIAL != 0 && inline_head && owner == "java/util/Optional" {
        // ⭐**THE OWNER TEST THAT ALREADY GUARDED THE THREE WRITERS NOW ALSO CENSUSES THE
        // READERS.** `[OPTLITE]` needs the SHAPE of the Optional population — how much of it is
        // `of`/`ofNullable`/`empty` against `map`/`flatMap`/`filter`/`ifPresent`/`orElse*` —
        // and every one of those names walks past this exact point on its way to
        // `stream::dispatch`'s `optional_op`. Hoisting the name test INSIDE the owner test
        // costs a non-Optional call nothing (the three conjuncts above are the ones it already
        // evaluated) and gives the census the reader names for free.
        //
        // ⚠It counts ARRIVALS AT THE CASCADE, which is not the whole population: a compiled
        // caller whose site `jit::direct::fu_routable` admitted is served by `vm/fu_fastpath.rs`
        // and never gets here. The BIRTH counters in `crate::optlite` are the route-independent
        // half — `stream::make_optional` is the sole constructor — and the module doc says which
        // number answers which question. Inert unless `RAVA_OPT_CENSUS`/`RAVA_JIT_PIC_PROBE` is
        // armed: `note_call` opens with one `OnceLock` read and does nothing else.
        crate::optlite::note_call(name);
        if matches!(name, "of" | "ofNullable" | "empty") {
            // `of` REJECTS a null where `ofNullable` accepts one. This fastpath is meant to
            // be byte-identical to the `stream::dispatch` arm it shortcuts, and it was not:
            // the arm now throws and this quietly returned an empty Optional, so the
            // shortcut was the wrong answer and the long route the right one.
            if name == "of" && matches!(args.first(), Some(Value::Null) | None) {
                let e = vm.make_exception("java/lang/NullPointerException", "");
                return Err(VmError::Throw(e));
            }
            let v = if name == "empty" {
                None
            } else {
                args.first().cloned().filter(|x| !matches!(x, Value::Null))
            };
            return Ok(Some(Some(crate::stream::make_optional_value(vm, v))));
        }
    }
    // ⛔**`isEmpty()` HAS NO EARLY ARM, AND THE REASON IS MEASURED.** The census that made one
    // look urgent — 8.39 M `isEmpty` refusals on four fastutil collections, the largest single
    // population in the engine — counts RESOLVES, not slow calls. An arm here, byte-identical to
    // `coll_op`'s answer and verified against 4 gates and 21 probes, fired **9,115 times**:
    // `try_last_arm` above already serves the repeat, so a shortcut placed after it is not a
    // shortcut. Removing the resolves themselves (`vm/pic_fill.rs`) then measured **-0.28% CPU**.
    // See `docs/the-negative-verdict-was-not-cached.md`.
    // FAST-PATH `iterator()` ON AN `$elems`-BACKED COLLECTION.
    //
    // ⭐**AN EARLY ARM HAS NO ADMISSION SYMMETRY, WHICH IS WHY THIS IS THE ROUTE THAT WORKS.**
    // `fu_routable` decides from a call site's DECLARED owner at compile time, so admitting
    // `java/util/List.iterator()` there also admits every site whose receiver turns out to be a
    // guest list or a view — and that measured a 0.7 s LOSS on the boot, which per-site
    // retirement then converted into a wash and no better (see `fu_retire_enabled`). An arm HERE
    // is reached only after dispatch has already happened: it tests the RECEIVER's shape, a miss
    // costs one field read, and it can only shorten the route.
    //
    // The route it shortens is long: `iterator` reaches `stream::dispatch` at cascade stage
    // `S_STREAM`, behind the whole owner scan. `bridgestat` on `bench/StreamApplyCost.java` with
    // a COMPILED caller puts `java/util/List#iterator` at **780,000 calls, 1.46 s, 38.1% of the
    // probe, 1,872 ns each** — against `Iterator#next` at 333 ns and `hasNext` at 267 once those
    // two are served from machine code.
    //
    // Byte-identical to the arm it shortcuts: the same `stream::dispatch` call, same arguments.
    //
    // ⚠**AND THE MEASUREMENT IS HONEST RATHER THAN FLATTERING.** It fires 780,000 times on the
    // probe (counted, `RAVA_DIAG=natives`), and seven interleaved pairs read:
    //   ON  2661 2550 2863 2793 3008 2605 2741   median 2741
    //   OFF 3372 3275 2590 2971 2585 2818 2453   median 2818
    // Five pairs of seven favour it, median -2.7%, but the two biggest OFF readings carry most of
    // that and the probe's own spread is about ±10%. The boot cannot arbitrate either: a null
    // test the same hour put its floor at ±3.1%. Kept on the same basis as the other arms in this
    // block — it shortens a long route for a hit and costs two string compares for a miss — not
    // on the strength of a delta neither instrument can resolve.
    // NOT owner-gated: this arm keys on the receiver's SHAPE and on the method name, which is the
    // whole reason it works (see the note above) — there is no owner literal to fingerprint.
    if gates & (G_TRIVIAL | G_ITERNEW) == (G_TRIVIAL | G_ITERNEW)
        && name == "iterator"
        && descriptor == "()Ljava/util/Iterator;"
    {
        if let Some(Value::Ref(h)) = args.first() {
            let h = *h;
            if matches!(vm.heap.get_field_syn(h, crate::heap::Syn::Elems), Value::Array(_)) {
                crate::frames::native_depth_inc();
                let sr = crate::stream::dispatch(vm, owner, name, descriptor, args);
                crate::frames::native_depth_dec();
                if let Some(res) = sr? {
                    // One acquire `OnceLock` load to reach a counter that is `armed == false`
                    // whenever `G_METER` is clear — the same proof the four meter handles above
                    // are folded behind, applied to the one instrument below them. Reached
                    // unconditionally under the kill switch.
                    if gates & G_CALLSEQ == 0 || gates & G_METER != 0 {
                        crate::diag::iter_create_early().inc();
                    }
                    return Ok(Some(res));
                }
            }
        }
    }
    // FAST-PATH the native ($elems-backed) Iterator hasNext/next: 2M+ calls in collection
    // iteration. The general iter_op path calls elems_of, which MATERIALIZES the WHOLE $elems
    // array into a fresh Vec on EVERY call (hasNext only needs the length; next only one
    // element) — plus the nio/net/stream dispatch scan. Read array_len / array_get (O(1))
    // directly. Byte-identical to iter_op: hasNext = (pos < len); next = elems[pos] (or Null)
    // then pos+1. Only when $elems is a real Array (a jar iterator with no $elems falls
    // through so elems_of can drain it). `next` only (primitive variants keep the coercion path).
    // ⭐**ADMITTED BY THE RECEIVER'S SHAPE, not by the owner's NAME.** The owner list was
    // `java/util/Iterator | java/util/ListIterator`, which misses every fastutil iterator — and
    // the JIT-callback census puts `ObjectSet.iterator` at 4.94% and
    // `Reference2ObjectMap.reference2ObjectEntrySet` at 4.52% of the boot's 64,088,346 crossings,
    // i.e. **9.46% of the seam is an iteration this fast path declined to serve**.
    // `bench/SeamCost.java` prices one four-entry `Reference2ObjectArrayMap` entry-iteration at
    // 7,900 ns against HotSpot's 15.
    //
    // The condition below asks what it actually needs: is this receiver one of OUR synthetic
    // `$elems`+`$pos` iterators? That is a property of the object, decided by the field probe the
    // arm already performs, so widening it costs nothing on a miss and needs no name list at all.
    // A receiver without both fields falls through exactly as before.
    if gates & G_ITER != 0 && matches!(name, "hasNext" | "next") {
        let _ = owner;
        if let Some(Value::Ref(h)) = args.first() {
            // BY ID, NOT BY NAME. `get_field`/`put_field` hash the field NAME and probe the
            // interner under an RwLock on every call; this path runs 2.25M hasNext + 1.84M next
            // per boot (RAVA_FALLBACK_TALLY: the top four fallback sites are this exact loop,
            // 7.78M of 12.2M total), i.e. ~10M string hashes for two fixed synthetic names.
            // `field_id_opt` exists precisely for this and its doc comment names `$pos`/`$elems`.
            // The slot HINT is validated inside `get_field_hint` (`ids[hi] == id`, else a scan),
            // so a stale hint from a differently-shaped iterator is a miss, never a wrong field.
            // Ids are cached only once RESOLVED - caching a `None` would disable the fast path
            // forever if the first call arrived before the name was interned.
            static ELEMS_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            static POS_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            static ELEMS_HINT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            static POS_HINT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let resolve = |cell: &'static std::sync::OnceLock<u32>, nm: &str| -> Option<u32> {
                if let Some(&id) = cell.get() {
                    return Some(id);
                }
                let id = vm.heap.field_id_opt(nm)?;
                let _ = cell.set(id);
                Some(id)
            };
            if let (Some(eid), Some(pid)) = (resolve(&ELEMS_ID, "$elems"), resolve(&POS_ID, "$pos")) {
                use std::sync::atomic::Ordering::Relaxed;
                let (ev, eh) = vm.heap.get_field_hint(*h, eid, ELEMS_HINT.load(Relaxed));
                if let Value::Array(arr) = ev {
                    ELEMS_HINT.store(eh, Relaxed);
                    let len = vm.heap.array_len(arr).unwrap_or(0);
                    let (pv, ph) = vm.heap.get_field_hint(*h, pid, POS_HINT.load(Relaxed));
                    POS_HINT.store(ph, Relaxed);
                    let pos = pv.as_int().unwrap_or(0) as usize;
                    if name == "hasNext" {
                        return Ok(Some(Some(Value::Int((pos < len) as i32))));
                    }
                    // Exhausted: `next()` THROWS (see iter_op, which this mirrors) —
                    // handing back null let a loop that is not driven by hasNext read
                    // an endless run of nulls instead of terminating.
                    if pos >= len {
                        return Err(VmError::Throw(
                            vm.make_exception("java/util/NoSuchElementException", ""),
                        ));
                    }
                    // FAIL-FAST, on the fast path too — otherwise the check in `iter_op` is
                    // dead code for every enhanced-for loop, which is where a structural
                    // modification during iteration actually happens. Two extra field reads,
                    // both by CACHED id like the two above, and both skipped entirely for an
                    // iterator with no `$expsize` (every internal snapshot iterator this
                    // engine makes for its own natives).
                    static EXP_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
                    static SRC_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
                    if let (Some(xid), Some(sid)) =
                        (resolve(&EXP_ID, "$expsize"), resolve(&SRC_ID, "$source"))
                    {
                        if let Value::Int(expect) = vm.heap.get_field_hint(*h, xid, 0).0 {
                            if let Value::Ref(src) = vm.heap.get_field_hint(*h, sid, 0).0 {
                                if let Value::Array(sa) = vm.heap.get_field_hint(src, eid, 0).0 {
                                    if vm.heap.array_len(sa).unwrap_or(0) as i32 != expect {
                                        return Err(VmError::Throw(vm.make_exception(
                                            "java/util/ConcurrentModificationException",
                                            "",
                                        )));
                                    }
                                }
                            }
                        }
                    }
                    let v = vm.heap.array_get(arr, pos).unwrap_or(Value::Null);
                    let ph2 = vm.heap.put_field_hint(*h, pid, Value::Int(pos as i32 + 1), ph);
                    POS_HINT.store(ph2, Relaxed);
                    return Ok(Some(Some(v)));
                }
            }
        }
    }
    // java.nio.file / java.io stream IO dispatch first (descriptor-tolerant for
    // the many varargs-option forms).
    // A missing file makes the JDK's I/O openers throw NoSuchFileException; our
    // nio layer otherwise returns an empty stream, which broke fall-back-on-
    // missing logic (Eula.readFile relies on the throw to call saveDefaults and
    // create eula.txt).
    // Split the gauntlet in two: the SERVICE STAGES (nio, net, stream, reflect, map — each a call
    // into a dispatcher that may decline) and everything after them, which is a long tail of
    // inline arms that compare `owner` against string literals one after another. 265 ns of self
    // time is a lot for stages that mostly decline, and the two halves have different cures.
    let _svc = (gates & G_METER != 0).then(|| crate::diag::native_services().start());
    if stage(ob::S_NIO) {
        if inline_head
            && owner == "java/nio/file/Files"
            && matches!(name, "newInputStream" | "newBufferedReader" | "readAllBytes"
                | "readString" | "readAllLines" | "lines" | "size"
                | "readAttributes" | "getLastModifiedTime" | "getOwner" | "readSymbolicLink")
        {
            if let Some(p) = crate::nio::path_str(vm, args.first()) {
                if !std::path::Path::new(&p).exists() {
                    let e = vm.make_exception("java/nio/file/NoSuchFileException", &p);
                    return Err(VmError::Throw(e));
                }
            }
        }
        if let Some(res) = crate::nio::dispatch(vm, owner, name, args)? {
            if plan & ob::S_NIO == 0 {
                gate_stage_miss("nio", owner, name, descriptor);
            }
            return Ok(Some(res));
        }
    }
    // java.nio sockets + netty transport (inline event loop). Early so the
    // networking owners are served before the generic stream/collection layers.
    // The gate's `inEventLoop` and `execute` verdicts are structural (net/mod.rs's
    // provider tables: the EventExecutorGroup closure + the exact SslHandler and
    // ThreadPerTaskExecutor tuples), so their owners live in no fixed namespace —
    // the name enters the stage whatever the owner, and `net::dispatch` stays the
    // authority.
    if stage(ob::S_NET) || name == "inEventLoop" || name == "execute" {
        if let Some(res) = crate::net::dispatch(vm, owner, name, descriptor, args)? {
            if plan & ob::S_NET == 0 && name != "inEventLoop" && name != "execute" {
                gate_stage_miss("net", owner, name, descriptor);
            }
            return Ok(Some(res));
        }
    }
    // stream/map dispatchers re-enter the interpreter (vm.apply per element)
    // while holding accumulator `Value`s in Rust locals that live in no frame,
    // so GC must not run mid-callback — bump native_depth for their duration
    // (decrement before `?` so an error still balances it). Transparent
    // forwarders (reflect Method.invoke, lambda dispatch) need no guard: their
    // args pass into the callee frame and are scanned there.
    //
    // `stream::dispatch` also serves the Iterable DEFAULTS (spliterator/stream/
    // iterator/forEach) on ANY receiver, so those names enter it whatever the
    // owner — that arm is name-keyed, not owner-keyed.
    if stage(ob::S_STREAM) || stream_iterable_name(name) {
        let _s = crate::diag::native_stream().start();
        crate::frames::native_depth_inc();
        let sr = crate::stream::dispatch(vm, owner, name, descriptor, args);
        crate::frames::native_depth_dec();
        if let Some(res) = sr? {
            if plan & ob::S_STREAM == 0 && !stream_iterable_name(name) {
                gate_stage_miss("stream", owner, name, descriptor);
            }
            return Ok(Some(res));
        }
    }
    if stage(ob::S_REFLECT) {
        if let Some(res) = crate::reflect::dispatch(vm, owner, name, args)? {
            if plan & ob::S_REFLECT == 0 {
                gate_stage_miss("reflect", owner, name, descriptor);
            }
            return Ok(Some(res));
        }
    }
    // `map::dispatch` serves an ARENA-MAP receiver before it consults the owner
    // (RAVA_ARENA_MAPS, default OFF), so that receiver shape enters regardless.
    if stage(ob::S_MAP) || arena_map_receiver(args) {
        crate::frames::native_depth_inc();
        let _m = crate::diag::native_map().start();
        let mr = crate::map::dispatch(vm, owner, name, args);
        crate::frames::native_depth_dec();
        if let Some(res) = mr? {
            if plan & ob::S_MAP == 0 && !arena_map_receiver(args) {
                gate_stage_miss("map", owner, name, descriptor);
            }
            return Ok(Some(res));
        }
    }
    drop(_svc); // the inline owner-string arms begin here
    // `Object.wait`/`notify`/`notifyAll` on ANY receiver. They were unimplemented, so a
    // program that used the classic monitor idiom died with an `UnsatisfiedLinkError`
    // rather than running.
    //
    // ⭐**REAL now, whenever a real thread exists** (`monitor.rs`): `wait` drops the monitor and
    // the GIL and blocks until a `notify` bumps the generation or the timeout expires, and
    // `notify`/`notifyAll` wake every waiter to re-check its own predicate.
    //
    // ⚠**Still the historical no-op while the engine is single-threaded**, and that is not
    // laziness: with one guest thread nothing else can run to satisfy the condition, so a
    // blocking `wait` would be a guaranteed hang where returning at once at least makes
    // progress. `monitor::wait` reports `false` in that case (and for a `wait` outside a
    // synchronized block, which the JVM answers with `IllegalMonitorStateException` — still not
    // raised here, as `bench/ThreadSem.java` records).
    if matches!(name, "wait" | "notify" | "notifyAll") && descriptor.starts_with('(') {
        if let Some(k) = args.first().and_then(crate::monitor::key_of) {
            if name == "wait" {
                // wait() | wait(J) | wait(JI) — a 0 timeout means "no timeout".
                let ms = match args.get(1) {
                    Some(Value::Long(0)) | None => None,
                    Some(Value::Long(n)) => Some(*n as u64),
                    _ => None,
                };
                crate::monitor::wait(k, ms);
            } else {
                crate::monitor::notify(k);
            }
        }
        return Ok(Some(None));
    }
    // ── FROM HERE TO `owner_families` THE ARMS ARE A LADDER OF OWNER LITERALS ──────────────────
    // Each carries `inline_tail` as its first term — the TAIL half of the split filter, so an
    // owner that only a HEAD arm could have keyed on collapses this whole ladder to one register
    // test instead of walking it. When the fingerprint admitted, every arm runs its own compare
    // in its own place, in its own order, exactly as before. The prefix arms (netty, JFR) and the
    // name-keyed monitor arm above are deliberately NOT carried — a prefix cannot be
    // fingerprinted and a name-keyed arm has no owner to reject.
    if inline_tail && owner == "java/util/Objects" {
        // ⭐**THE SAME DISPATCHER, CALLED TWICE PER CALL.** The head fast path above IS
        // `misc::objects` (it was hoisted there because `Objects.equals` is 3.92% of the boot's
        // crossings and `requireNonNull` a further 1.40%), and this arm calls it again. When the
        // head ran it, only two outcomes reach here: it served and returned, or it threw and
        // returned — so the ONLY state in which control arrives at this line is "it already
        // answered `Ok(None)`". And that answer is REPRODUCIBLE, which is what makes skipping it
        // a proof rather than a guess: `objects`'s single decline is `_ => return Ok(None)`, the
        // wildcard of a `match name`, reached with no side effect performed and with nothing but
        // the method NAME consulted — and the name is the same `&str` on the second call.
        // `Ok(None)` IS that second call's answer, and it is also what this arm would
        // have returned, so the cascade still ends here rather than falling into the tail tables.
        // With `RAVA_NO_TRIVIAL_FASTPATH=1` (or a head the filter rejected) the head never ran
        // and this call is the real arm, unchanged.
        if gates & (G_TRIVIAL | G_FAST4) == (G_TRIVIAL | G_FAST4) && inline_head {
            return Ok(None);
        }
        return objects(vm, name, args);
    }
    // The three `*SummaryStatistics` are plain value carriers: `stream_op` fills their
    // five fields in one pass and these are the readers. `toString` matters as much as
    // the getters — guests log it, and the JDK's shape is exact.
    if inline_tail && matches!(owner,
        "java/util/IntSummaryStatistics"
        | "java/util/LongSummaryStatistics"
        | "java/util/DoubleSummaryStatistics")
    {
        return summary_statistics(vm, owner, name, args);
    }
    // sun.misc.Unsafe / jdk.internal.misc.Unsafe are unavailable. Real JVMs
    // throw SecurityException from getUnsafe() for non-bootstrap callers, and
    // libraries (fastutil, netty, ...) catch it and use their safe fallbacks.
    if inline_tail
        && (owner == "sun/misc/Unsafe" || owner == "jdk/internal/misc/Unsafe")
        && name == "getUnsafe"
    {
        let e = vm.make_exception("java/lang/SecurityException", "Unsafe is not available");
        return Err(VmError::Throw(e));
    }
    // Netty native transports (epoll=Linux, kqueue=macOS) have no JNI library on
    // this platform, exactly as on the real JVM here (Windows): the native probe
    // throws UnsatisfiedLinkError, netty's <clinit> catches it and records the
    // transport unavailable, and MC falls back to the NIO transport. Mirror that:
    // isAvailable() reports false and every native entry point throws ULE.
    if owner.starts_with("io/netty/channel/epoll/") || owner.starts_with("io/netty/channel/kqueue/") {
        match name {
            "isAvailable" => return Ok(Some(Some(Value::Int(0)))),
            "unavailabilityCause" => {
                return Ok(Some(Some(vm.make_exception(
                    "java/lang/UnsatisfiedLinkError", "native transport unavailable"))));
            }
            "ensureAvailability" => {
                let e = vm.make_exception("java/lang/UnsatisfiedLinkError", "native transport unavailable");
                return Err(VmError::Throw(e));
            }
            // Native.* JNI entry points (kqueueCreate/epollCreate/offsetof…).
            _ if owner.ends_with("/Native") => {
                let e = vm.make_exception("java/lang/UnsatisfiedLinkError", "no native transport library");
                return Err(VmError::Throw(e));
            }
            _ => {}
        }
    }
    // netty `DefaultChannelId.<clinit>` sets `MACHINE_ID = MacAddressUtil.defaultMachineId()`
    // and `DefaultChannelId.<init>` does `arraylength` on it. With no enumerable NICs the
    // guest random-MAC fallback yielded a null array here, so MACHINE_ID was null and the
    // first channel bind (ServerConnectionListener.startTcpServerListener) NPE'd on
    // `arraylength` (Wall #18). Serve a fixed, non-null 8-byte machine id directly — a
    // locally-administered MAC (bit 1 of byte 0 set), exactly the shape netty expects.
    if inline_tail
        && owner == "io/netty/util/internal/MacAddressUtil"
        && matches!(name, "defaultMachineId" | "bestAvailableMac")
    {
        static TAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if crate::jit::env_gate("RAVA_TRACE_ARRLEN", &TAL) {
            eprintln!("[MAC-STUB] {owner}.{name} -> fixed 8-byte MAC");
        }
        let a = vm.heap.alloc_bytes(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        return Ok(Some(Some(Value::Array(a))));
    }
    // Object.clone() (typically reached as super.clone() from a JDK/library
    // class such as a fastutil map, which then deep-copies its own arrays): a
    // shallow field copy. Gated on the Object owner so type-specific clones
    // (BitSet, MessageDigest, …) keep their deep-copy handlers.
    if inline_tail && owner == "java/lang/Object" && name == "clone" {
        if let Some(Value::Ref(h)) = args.first() {
            return Ok(Some(Some(Value::Ref(vm.heap.shallow_clone(*h)))));
        }
        // Arena object receiver (next-gen-gc): the instance is a `Value::Obj`, not a
        // heap handle, so the Ref-only path above missed it and pushed nothing — the
        // caller's `astore`/`checkcast` then underflowed (e.g. fastutil
        // Long2IntOpenHashMap.clone() in the light-engine init). Shallow-copy the inline
        // cells into a fresh arena object of the same class.
        #[cfg(feature = "next-gen-gc")]
        if let Some(Value::Obj(p)) = args.first() {
            let cid = crate::nextgen::arena_class_id(*p);
            let nf = crate::nextgen::arena_nfields(*p);
            // The alloc may collect (moving the source); the args buffer is GC-rooted
            // (NATIVE_ARGS), so re-read the forwarded source AFTER allocating.
            let np = match crate::nextgen::tlab_try_alloc(cid, nf) {
                Some(np) => np,
                None => {
                    vm.arena_collect_jit();
                    crate::nextgen::tlab_try_alloc(cid, nf)
                        .expect("arena clone alloc after collect")
                }
            };
            let src = match args.first() {
                Some(Value::Obj(q)) => *q,
                _ => *p,
            };
            for i in 0..nf as usize {
                let cell = crate::nextgen::arena_cell(src, i);
                crate::nextgen::arena_set_cell(np, i, cell);
            }
            return Ok(Some(Some(Value::Obj(np))));
        }
    }
    // JDK Flight Recorder is not modelled. Report it unavailable and no-op event
    // registration/commit so libraries (netty) that probe it fall back cleanly.
    if owner.starts_with("jdk/jfr/") {
        match name {
            "isAvailable" | "isEnabled" | "isRecording" | "shouldCommit" => return Ok(Some(Some(Value::Int(0)))),
            "register" | "unregister" | "commit" | "set" | "begin" | "end" | "<init>" => return Ok(Some(None)),
            "getEventType" | "newInstance" => return Ok(Some(Some(Value::Null))),
            _ => {}
        }
    }
    // AccessController.doPrivileged(action[, ctx]) — no security manager, so just
    // run the action's SAM (run():Object) directly.
    if inline_tail && owner == "java/security/AccessController" && name == "doPrivileged" {
        let action = args.first().cloned().unwrap_or(Value::Null);
        return Ok(Some(run_privileged(vm, &action)?));
    }
    // java.lang.ref.Reference family — we never GC, so a reference is simply a
    // strong `$referent` holder (set by <init>, returned by get, nulled by clear).
    if inline_tail && matches!(owner, "java/lang/ref/Reference" | "java/lang/ref/WeakReference"
        | "java/lang/ref/SoftReference" | "java/lang/ref/PhantomReference")
    {
        let recv = match args.first() { Some(Value::Ref(h)) => Some(*h), _ => None };
        match name {
            "<init>" => {
                if let (Some(h), Some(r)) = (recv, args.get(1)) {
                    vm.heap.put_field(h, "$referent", r.clone());
                }
                return Ok(Some(None));
            }
            "get" => return Ok(Some(Some(match recv {
                Some(h) => vm.heap.get_field(h, "$referent"),
                None => Value::Null,
            }))),
            "refersTo" => {
                let cur = recv.map(|h| vm.heap.get_field(h, "$referent")).unwrap_or(Value::Null);
                return Ok(Some(Some(Value::Int(ref_eq(Some(&cur), args.get(1)) as i32))));
            }
            "clear" => {
                if let Some(h) = recv { vm.heap.put_field(h, "$referent", Value::Null); }
                return Ok(Some(None));
            }
            "enqueue" | "isEnqueued" => return Ok(Some(Some(Value::Int(0)))),
            _ => {}
        }
    }
    if inline_tail && owner == "java/util/HexFormat" {
        if let Some(r) = hexformat_dispatch(vm, name, descriptor, args) {
            return Ok(Some(r));
        }
    }
    if inline_tail && owner == "java/util/SplittableRandom" {
        if let Some(r) = splittable_dispatch(vm, name, args) {
            return Ok(Some(r));
        }
    }
    if inline_tail && matches!(owner, "java/util/Random" | "java/security/SecureRandom"
        | "java/util/concurrent/ThreadLocalRandom") {
        if let Some(r) = random_dispatch(vm, name, args)? {
            return Ok(Some(r));
        }
    }
    if inline_tail && matches!(owner, "java/nio/charset/Charset" | "java/nio/charset/CharsetDecoder" | "java/nio/charset/CharsetEncoder") {
        let cs_name = |vm: &Vm, v: Option<&Value>| match v {
            Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) { Value::Str(s) => s.to_string(), _ => "UTF-8".into() },
            _ => "UTF-8".into(),
        };
        match name {
            "forName" | "lookup" => { // static
                let n = str_of(vm, args.first()).unwrap_or_else(|| "UTF-8".into());
                let h = vm.heap.alloc("java/nio/charset/Charset");
                vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(n.into()));
                return Ok(Some(Some(Value::Ref(h))));
            }
            "defaultCharset" => {
                let h = vm.heap.alloc("java/nio/charset/Charset");
                vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str("UTF-8".into()));
                return Ok(Some(Some(Value::Ref(h))));
            }
            "newDecoder" | "newEncoder" => {
                let n = cs_name(vm, args.first());
                let cls = if name == "newDecoder" { "java/nio/charset/CharsetDecoder" } else { "java/nio/charset/CharsetEncoder" };
                let h = vm.heap.alloc(cls);
                vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(n.into()));
                return Ok(Some(Some(Value::Ref(h))));
            }
            "name" | "displayName" | "toString" => return Ok(Some(Some(Value::Str(cs_name(vm, args.first()).into())))),
            // Buffer-sizing ratios (safe upper bounds; netty uses these to
            // allocate encode/decode scratch buffers).
            "maxBytesPerChar" => return Ok(Some(Some(Value::Float(4.0)))),
            "averageBytesPerChar" => return Ok(Some(Some(Value::Float(2.0)))),
            "maxCharsPerByte" | "averageCharsPerByte" => return Ok(Some(Some(Value::Float(1.0)))),
            // Chainable, side-effect-free config on a decoder/encoder.
            "onMalformedInput" | "onUnmappableCharacter" | "replaceWith" | "reset" | "charset" => {
                return Ok(Some(Some(args.first().cloned().unwrap_or(Value::Null))));
            }
            // decode(ByteBuffer in, CharBuffer out, boolean): UTF-8-decode the
            // input's remaining bytes into the output CharBuffer's $sb, drain the
            // input, and report UNDERFLOW. netty's ByteBufUtil.decodeString uses
            // this to read string fields (e.g. the handshake server address).
            "decode" if descriptor.starts_with("(Ljava/nio/ByteBuffer;Ljava/nio/CharBuffer;") => {
                if let (Some(Value::Ref(inb)), Some(Value::Ref(outb))) = (args.get(1), args.get(2)) {
                    let bytes = crate::nio::buffer_remaining(vm, *inb);
                    let s = String::from_utf8_lossy(&bytes).into_owned();
                    let lim = vm.heap.get_field(*inb, "$limit");
                    vm.heap.put_field_syn(*inb, crate::heap::Syn::Pos, lim);
                    vm.heap.sb_append(*outb, &s);
                }
                let cr = vm.heap.alloc("java/nio/charset/CoderResult");
                vm.heap.put_field(cr, "$underflow", Value::Int(1));
                return Ok(Some(Some(Value::Ref(cr))));
            }
            // decode(ByteBuffer) -> CharBuffer (one-shot convenience form).
            "decode" if descriptor.starts_with("(Ljava/nio/ByteBuffer;)") => {
                let s = match args.get(1) {
                    Some(Value::Ref(inb)) => {
                        let bytes = crate::nio::buffer_remaining(vm, *inb);
                        let lim = vm.heap.get_field(*inb, "$limit");
                        vm.heap.put_field_syn(*inb, crate::heap::Syn::Pos, lim);
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    _ => String::new(),
                };
                let cb = vm.heap.alloc("java/nio/CharBuffer");
                vm.heap.put_field_syn(cb, crate::heap::Syn::Sb, Value::Str(s.into()));
                return Ok(Some(Some(Value::Ref(cb))));
            }
            // flush(CharBuffer) after a decode run: nothing buffered, UNDERFLOW.
            "flush" => {
                let cr = vm.heap.alloc("java/nio/charset/CoderResult");
                vm.heap.put_field(cr, "$underflow", Value::Int(1));
                return Ok(Some(Some(Value::Ref(cr))));
            }
            _ => {}
        }
    }
    if let Some(h) = owner_families(vm, plan, verify, owner, name, descriptor, args, family_fact)? {
        return Ok(Some(h));
    }
    // ── THE TAIL IS A DISPATCH NOW, NOT A WALK (RAVA_NO_FAST_CASCADE) ──────────────────────
    // Below here are the two literal tables, ~120 arm heads of ordered `str` compares between
    // this point and `Ok(None)`. Both are keyed on the OWNER, so which of them can possibly
    // answer is decidable from the owner alone — ONE fingerprint (`owner_sig`: length plus three
    // bytes, no memo, no lock, no allocation) and two `.rodata` bit tests replace the walk. A
    // rejected table is not "probably empty": its arms are all owner-literal-keyed and its only
    // other exit is the wildcard, so skipping the call IS its answer. See the block comment on
    // `owner_sig` for why this is a fingerprint and not `owner_bits`.
    // `sig` and both flags now come from the prologue — the fingerprint is computed at most once
    // per cascade entry instead of once here, and the two `OnceLock`s are bits of the gate word.
    let fc = gates & G_FAST1 != 0;
    let fcv = gates & G_FAST1V != 0;
    let t1 = !fc || table1_may_serve(sig);
    if gates & G_STAT != 0 && !t1 {
        CASCADE_SKIP1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if t1 || fcv {
        if let Some(h) = owner_table(vm, plan, verify, owner, name, descriptor, args)? {
            if !t1 {
                fast_cascade_miss("owner_table", owner, name, descriptor);
            }
            return Ok(Some(h));
        }
    }
    let t2 = !fc || table2_may_serve(sig, owner);
    if gates & G_STAT != 0 && !t2 {
        CASCADE_SKIP2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if !t2 && !fcv {
        // Proven unserved. `owner_table2` would run its whole match to reach `_ => Ok(None)`;
        // this IS that arm, reached without it. No stores, no allocation, no mutation — a
        // refusal in this engine has to be invisible.
        if gates & G_STAT != 0 {
            CASCADE_NONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Ok(None);
    }
    let tail = owner_table2(vm, plan, verify, owner, name, descriptor, args);
    if !t2 && matches!(&tail, Ok(Some(_))) {
        fast_cascade_miss("owner_table2", owner, name, descriptor);
    }
    // `RAVA_CASCADE_STAT`: how many traversals reach the END and serve NOTHING.
    //
    // The shortcut's own counters say `hit=1,867,032 miss=18,011,344` (9.4%), and the served
    // census totals ~10.5 M — so up to a third of the traversals may be paying the whole scan to
    // answer "not a native". That is a different defect from a cold cache and wants a different
    // fix (a negative memo, not more arms), so it is COUNTED before either is built.
    if gates & G_STAT != 0 {
        match &tail {
            Ok(None) => CASCADE_NONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            _ => CASCADE_SERVED_TAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        };
    }
    tail
}

/// `RAVA_CASCADE_STAT` — count the traversals that fall through the whole cascade unserved.
pub(crate) static CASCADE_NONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// ...and those the LAST table served, which a negative memo must not swallow.
pub(crate) static CASCADE_SERVED_TAIL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Tail-table walks the owner filter removed (`RAVA_CASCADE_STAT`). An instrument that separates
/// "the filter did not help" from "the filter never fired" — the distinction four dead
/// instruments in this ledger were read without.
pub(crate) static CASCADE_SKIP1: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static CASCADE_SKIP2: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One line on the owner filter, printed beside the other end-of-run censuses. Silent unless
/// `RAVA_CASCADE_STAT` armed the counters or `RAVA_FAST_CASCADE_VERIFY` found a miss.
pub(crate) fn fast_cascade_report() {
    use std::sync::atomic::Ordering::Relaxed;
    let (a, b) = (CASCADE_SKIP1.load(Relaxed), CASCADE_SKIP2.load(Relaxed));
    let miss = FAST_CASCADE_MISSES.load(Relaxed);
    let c = CASCADE2_SKIP.load(Relaxed);
    let d = CASCADE3_SKIP.load(Relaxed);
    // The segment split's own two rows: entries the UNION bit admitted and a segment bit then
    // rejected. They are exactly the walks the coarse filter was paying for and this round does
    // not — a zero here says the split never fired, which is a different fact from "it did not
    // help" and the reason both are counted separately.
    let (e, f) = (CASCADE4_SKIP_HEAD.load(Relaxed), CASCADE4_SKIP_TAIL.load(Relaxed));
    if a + b + c + d + e + f + miss == 0 {
        return;
    }
    eprintln!(
        "[FAST-CASCADE] tail walks skipped: owner_table={a} owner_table2={b} \
         | inline ladders skipped={c} | head ladders skipped={e} tail ladders skipped={f} \
         | last-arm lookups skipped={d} | verify misses={miss}"
    );
}

pub(crate) fn fallthrough_stat() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CASCADE_STAT").is_some())
}

/// Kill switch for the early `iterator()` arm above.
fn iter_create_early_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_ITER_CREATE_EARLY").is_some())
}


/// Inline ladders skipped by the owner fingerprint (`RAVA_CASCADE_STAT`). Counted so that
/// "the filter did not help" can be told apart from "the filter never fired".
pub(crate) static CASCADE2_SKIP: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

// ── THE INLINE-ARM FILTER BEGINS HERE ────────────────────────────────────────────────────────
// Everything below this line is the gate word, the owner filter, their literal registry and the
// tests that keep the registry honest. The drift test reads only the part of this file ABOVE the
// marker text — the cascade itself — so the class-shaped strings in the lists below are the
// answer key rather than more questions.

/// The drift test reads only the source ABOVE the first occurrence of this text.
#[cfg(test)]
const FILTER_SECTION_MARKER2: &str = "THE INLINE-ARM FILTER BEGINS HERE";

/// Where the inline ladder is CUT IN TWO — the comment that opens the post-gauntlet ladder.
/// Everything above it in the cascade is head-segment source, everything below it (down to
/// [`FILTER_SECTION_MARKER2`]) is tail-segment source, and `every_inline_owner_is_in_its_segment`
/// uses that to check the two literal lists against the arms they actually guard. Both marker
/// constants are defined BELOW `FILTER_SECTION_MARKER2`, so neither string occurs in the region
/// being sliced.
#[cfg(test)]
const LADDER_SPLIT_MARKER: &str = "THE ARMS ARE A LADDER OF OWNER LITERALS";

// Bits of the cascade's gate word. Each is the process constant it replaces, nothing more: the
// word is built once, from the same predicates, so `RAVA_NO_TRIVIAL_FASTPATH` (and every other
// flag folded here) keeps working exactly as it did.
pub(crate) const G_TRIVIAL: u32 = 1 << 0;
pub(crate) const G_ENTRY: u32 = 1 << 1;
pub(crate) const G_ITER: u32 = 1 << 2;
pub(crate) const G_ITERNEW: u32 = 1 << 3;
pub(crate) const G_FAST1: u32 = 1 << 4;
pub(crate) const G_FAST1V: u32 = 1 << 5;
pub(crate) const G_FAST2: u32 = 1 << 6;
pub(crate) const G_STAT: u32 = 1 << 7;
/// The last-arm shortcut's own owner filter (`RAVA_NO_FAST_CASCADE3`) and its verify mode.
pub(crate) const G_FAST3: u32 = 1 << 8;
pub(crate) const G_FAST3V: u32 = 1 << 9;
/// Set when any instrument at all is armed. Clear means every meter in this function is
/// `armed == false`, so its guard is a no-op and need not be built. See the block comment at the
/// `_nat` binding for why that is a proof rather than an assumption.
pub(crate) const G_METER: u32 = 1 << 10;
/// The inline ladder's HEAD/TAIL split and the `$value` read by id (`RAVA_NO_FAST_CASCADE4`).
pub(crate) const G_FAST4: u32 = 1 << 11;

// ── THE OTHER TWO WORDS, FOLDED IN (RAVA_NO_FAST_CASCADE5) ───────────────────────────────────
// A native call used to load THREE process-constant words from THREE `OnceLock`s: `entry_gates()`
// at the bridge prologue, `gate_mode()` in `dispatch_owner`, and `cascade_gates()` here. Same
// fact, same immutability, three acquire loads and three branches per call — and, because
// `try_invoke_chain` runs `dispatch_owner` once per super-chain link, the second and third are
// paid per LINK. The bits below carry the other two words, so the whole native entry reads one.

/// `RAVA_DISPATCH_GATE` — the owner-plan stage gate. Was `gate_mode()`'s `GM_ON`.
pub(crate) const G_GATE: u32 = 1 << 12;
/// `RAVA_DISPATCH_GATE_VERIFY`. Was `gate_mode()`'s `GM_VERIFY`.
pub(crate) const G_GATEV: u32 = 1 << 13;
/// `crate::arena_map::enabled()` — read inside `arena_map_receiver` on every last-arm probe and
/// (behind a short-circuit) at the map stage. Default OFF, so the default answer is a register
/// test against a constant instead of a relaxed load and a three-way match.
pub(crate) const G_ARENAMAP: u32 = 1 << 14;
/// `RAVA_NATIVE_COUNT` — tally every native invocation. Was `entry_gates()`'s `EG_COUNT`.
pub(crate) const G_COUNT: u32 = 1 << 15;
/// `RAVA_NATIVE_HIST` — time the outermost invocation. Was `entry_gates()`'s `EG_HIST`.
pub(crate) const G_HIST: u32 = 1 << 16;
/// `RAVA_BRIDGE_STAT` — the per-(owner,name) bridge tally is armed. Was `EG_BRIDGESTAT`.
pub(crate) const G_BRIDGESTAT: u32 = 1 << 17;
/// The bridge prologue's arena-string canonicalization takes the pooled-scratch arm. Was
/// `EG_SCRATCH`. (Only read where the arena exists; harmless to define unconditionally.)
pub(crate) const G_SCRATCH: u32 = 1 << 18;
/// `RAVA_NO_FAST_DISPTLS=1` / `RAVA_NO_EMIT_MORE` — read every folded entry constant back at its
/// own site. Was `entry_gates()`'s `EG_LEGACY`; unchanged in meaning.
pub(crate) const G_DISPTLS_LEGACY: u32 = 1 << 19;
/// Every bit that used to live in `entry_gates()`'s own word.
pub(crate) const G_ENTRY_BITS: u32 =
    G_COUNT | G_HIST | G_BRIDGESTAT | G_SCRATCH | G_DISPTLS_LEGACY;
/// KILL SWITCH `RAVA_NO_FAST_CASCADE5=1` (or `RAVA_NO_EMIT_MORE`), DEFAULT OFF: every layer of
/// the native entry reads its own gate word at its own site again (`entry_gates()` at the bridge,
/// `gate_mode()` in `dispatch_owner`, `cascade_gates()` in the cascade), `try_last_arm` builds its
/// `native_armlookup` handle and calls `arena_map_receiver` unconditionally, and the inline filter
/// computes the union fingerprint probe alongside the two segment probes. The word is still built
/// — it has to be, to carry this bit — so the switch restores the READS, which is what this round
/// removes; the answers never differed.
pub(crate) const G_LEGACY5: u32 = 1 << 20;

// ── THE CROSSING'S OWN CENSUS TRAFFIC (RAVA_NO_CALLSEQ) ──────────────────────────────────────
// Everything above this point removed READS from the crossing. What survived them is a WRITE:
// `ARM_HIT`/`ARM_MISS` are `fetch_add`ed on EVERY cascade entry — that is a `lock xadd`, an
// uncontended one, but a read-modify-write all the same, and this tree has already measured what
// one such instruction costs on a path the optimiser would otherwise have room in (a single
// relaxed load on the interpreter's push path was worth 15%). Its cost is not the instruction: it
// is that LLVM may not sink it, may not hoist it out of the caller's loop, and must keep the
// caller's values materialised across it, because an atomic RMW is a memory barrier as far as the
// optimiser's alias analysis is concerned.
//
// The counters exist for ONE line of output apiece, printed by two instruments that are armed by
// process constants (`pc_profile::enabled()`, `lane_probe::enabled()`) and that both print only
// `if hit + miss > 0`. So a run in which neither is armed, and in which no census flag is set,
// cannot observe the difference between counting and not counting — which makes the bit below a
// PROOF that skipping the write is its behaviour, not an approximation of it. When anything that
// could read them is armed the bit is set and both counters are written exactly as before, in the
// same places, so an armed run's numbers are the numbers it always printed.
//
/// Keep the last-arm shortcut's hit/miss census. Set when `RAVA_ARM_STAT`, `RAVA_CASCADE_STAT`,
/// the PC profiler or the lane probe is armed — i.e. whenever anything in the process can READ
/// those two counters — and unconditionally under the kill switch.
pub(crate) const G_ARMSTAT: u32 = 1 << 21;
/// Keep the super-chain / served-owner censuses. Set when `RAVA_CHAIN_CENSUS` (or its `RAVA_DIAG`
/// selector) or `RAVA_SERVED_CENSUS` is armed, and unconditionally under the kill switch. Clear
/// proves BOTH `chain_census::on()` and `served_census::enabled()` are false, so the three census
/// calls on the chain walk are no-ops and not making them IS their behaviour.
pub(crate) const G_CENSUS: u32 = 1 << 22;
/// KILL SWITCH `RAVA_NO_CALLSEQ=1` (or `RAVA_NO_EMIT_MORE`), DEFAULT ON: this bit SET means the
/// crossing round is active. Clear restores the historic sequence exactly — both census bits are
/// forced set (so every counter and every census call runs unconditionally, where it always did),
/// the `iterator()` arm's diag counter is reached unconditionally, and `bridge_gates` recomposes
/// its word from two `OnceLock`s at its own site again.
pub(crate) const G_CALLSEQ: u32 = 1 << 23;

/// DEFAULT-ON. `RAVA_NO_CALLSEQ=1` (or `RAVA_NO_EMIT_MORE`) restores the historic native-entry
/// sequence — see [`G_CALLSEQ`]. Also consults `emit_more_disabled`, so it falls together with
/// the other emit-more switches.
pub(crate) fn callseq() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // Per-area switch -- see the note in `jit::emit::body`. `RAVA_NO_CALLSEQ` disables everything.
        // MEASURED OPT-IN. Unbundling this round by per-area switch priced the three members
        // separately: the SSA/branch construction is a win (+2.52% min / +4.27% median when
        // disabled) and the call-site emission is a win (+1.81% / +3.15%), but THIS area is a
        // REGRESSION (-1.54% / -3.59% -- disabling it is FASTER on both metrics), and it was
        // cancelling both. The bundle therefore measured -2% while two of its three members paid.
        // `RAVA_FAST_CALLSEQ_BRIDGES=1` arms it; the default is the historic crossing.
        !crate::flags::emit_more_disabled()
            && std::env::var_os("RAVA_NO_CALLSEQ").is_none()
            && std::env::var_os("RAVA_FAST_CALLSEQ_BRIDGES").is_some()
    })
}

/// OPT-IN (`RAVA_FAST_CASCADE5=1`) — the fold measured slower and was demoted, so the DEFAULT
/// configuration has [`G_LEGACY5`] SET. See [`G_LEGACY5`] for what that restores.
pub(crate) fn fast_cascade5() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // MEASURED OPT-IN (round 31) -- see `heap::fields` HG_NODERIVE for the numbers.
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_FAST_CASCADE5").is_some()
    })
}

/// ONE READ FOR EIGHT PROCESS CONSTANTS.
///
/// Every predicate folded in here was already a `OnceLock<bool>` that could only be read at its
/// own call site, and a cascade entry touched all eight. They are separate statics, so that is
/// eight loads from eight addresses and eight branches, and none of them can be hoisted or merged
/// by the compiler because the dispatchers called in between are opaque to it. One word, read
/// once per cascade entry, replaces the lot; the individual accessors stay exactly as they were
/// and are called from here, so a flag still means what its own doc comment says it means.
///
/// This is the same shape as the fix that took three per-callback gate reads out of the seam —
/// the cost of a process constant is not the load, it is that it is a load AT ALL on a path the
/// optimiser would otherwise have nothing to do in.
#[inline]
pub(crate) fn cascade_gates() -> u32 {
    static W: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u32;
        if trivial_fastpath_enabled() {
            w |= G_TRIVIAL;
        }
        if entry_fastpath_enabled() {
            w |= G_ENTRY;
        }
        if iter_fastpath_enabled() {
            w |= G_ITER;
        }
        if !iter_create_early_disabled() {
            w |= G_ITERNEW;
        }
        if fast_cascade() {
            w |= G_FAST1;
        }
        if fast_cascade_verify() {
            w |= G_FAST1V;
        }
        if fast_cascade2() {
            w |= G_FAST2;
        }
        if fallthrough_stat() {
            w |= G_STAT;
        }
        if fast_cascade3() {
            w |= G_FAST3;
        }
        if fast_cascade3_verify() {
            w |= G_FAST3V;
        }
        // The meter guards are built when anything at all is armed — and unconditionally when
        // the kill switch is set, so `RAVA_NO_FAST_CASCADE3=1` restores this whole round in one
        // run: the four handle reads AND the unfiltered last-arm shortcut.
        if !fast_cascade3() || crate::diag::any_armed() {
            w |= G_METER;
        }
        if fast_cascade4() {
            w |= G_FAST4;
        }
        // ── THE OTHER TWO WORDS (RAVA_NO_FAST_CASCADE5) ───────────────────────────────────────
        // Every bit below is read from the SAME accessor that owned it, once, here. `entry_gates`
        // and `gate_mode` still exist and still answer identically; they are simply no longer on
        // the per-call path unless `G_LEGACY5` puts them back.
        let eg = crate::natives::entry::entry_gates();
        // ⭐**THE RECOMPOSITION `bridge_gates` DROPS IS AN IDENTITY, AND THIS IS WHERE IT IS
        // CHECKED.** Under `G_LEGACY5` the bridge prologue used to answer with
        // `(cascade_gates() & !G_ENTRY_BITS) | entry_gates()` — TWO acquire `OnceLock` loads per
        // crossing (and per super-chain CALL) to rebuild a word this one already contains. It is
        // equal to `cascade_gates()` for exactly one reason: every bit `entry_gates` can set lies
        // inside `G_ENTRY_BITS`, and the line below ORs all of them in. Asserted rather than
        // asserted-in-prose: this runs ONCE, while the word initialises, so it costs a boot
        // nothing and it fails loudly the day somebody gives `entry_gates` a bit of its own.
        assert_eq!(
            eg & !G_ENTRY_BITS,
            0,
            "entry_gates() set a bit outside G_ENTRY_BITS — `bridge_gates` composes its word on \
             the assumption that it cannot, and would now drop that bit on every native call"
        );
        w |= eg & G_ENTRY_BITS;
        w |= crate::natives::entry::gate_mode_bits();
        #[cfg(feature = "next-gen-gc")]
        if crate::arena_map::enabled() {
            w |= G_ARENAMAP;
        }
        if !fast_cascade5() {
            w |= G_LEGACY5;
        }
        // ── THE CROSSING ROUND (RAVA_NO_CALLSEQ) ──────────────────────────────────────────────
        let seq = callseq();
        if seq {
            w |= G_CALLSEQ;
        }
        // Every reader of `ARM_HIT`/`ARM_MISS` in the tree, plus the two flags that exist to
        // print them. All four are process constants of their own, read here ONCE; a clear bit is
        // the proof that no line of output depends on the two `fetch_add`s.
        if !seq
            || std::env::var_os("RAVA_ARM_STAT").is_some()
            || fallthrough_stat()
            || crate::pc_profile::enabled()
            || crate::lane_probe::enabled()
        {
            w |= G_ARMSTAT;
        }
        // Both censuses on the super-chain walk, whose own `on()`/`enabled()` are the acquire
        // loads this replaces — one per link served, one per call that serves nothing.
        if !seq
            || crate::natives::entry::chain_census::on()
            || crate::natives::served_census::enabled()
        {
            w |= G_CENSUS;
        }
        w
    })
}

/// DEFAULT-ON. `RAVA_NO_FAST_CASCADE4=1` restores the SINGLE inline-owner bit (one ladder, so an
/// owner admitted for a head arm walks every tail compare and vice versa) and the by-name
/// `$value` read in the `Boolean.booleanValue` arm. Also consults `emit_more_disabled`, so it
/// falls together with the other emit-more switches.
pub(crate) fn fast_cascade4() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FAST_CASCADE4").is_none()
    })
}

/// DEFAULT-ON. `RAVA_NO_FAST_CASCADE3=1` restores the unfiltered last-arm shortcut (every native
/// call pays its key, its TLS access and its borrow again) and the four unconditional meter
/// handle reads. Also consults `emit_more_disabled`, so it falls together with the other
/// emit-more switches.
pub(crate) fn fast_cascade3() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FAST_CASCADE3").is_none()
    })
}

/// `RAVA_FAST_CASCADE3_VERIFY` (default OFF, inert): run the last-arm shortcut even for an owner
/// the filter REJECTED, and report it if it serves. The filter is a claim about the owner
/// literals guarding `try_last_arm`'s match arms, and this is how an edit that breaks the claim
/// is caught on a real workload rather than in review — the same discipline `RAVA_FAST_CASCADE_
/// VERIFY` applies to the two tail tables.
pub(crate) fn fast_cascade3_verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FAST_CASCADE3_VERIFY").is_some())
}

/// EVERY owner literal an arm of `try_last_arm` is keyed on — that match lives in `entry.rs`.
///
/// The shortcut's match is `ARM_INT if owner == ... => ...`, four arms plus `_ => None`, so an
/// owner in none of these sets can only reach the wildcard. This list is the only thing standing
/// between that fact and the filter built on it, so it is checked against `entry.rs`'s own source
/// by `no_class_literal_in_try_last_arm_is_unregistered` below rather than by review.
pub(crate) const OWNERS_LASTARM: &[&str] = &[
    // ARM_INT — the boxed-integer statics.
    "java/lang/Integer",
    "java/lang/Long",
    // ARM_STR.
    "java/lang/String",
    // ARM_WRITER — the eight character sinks `writer_dispatch` serves.
    "java/io/StringWriter",
    "java/io/PrintWriter",
    "java/io/CharArrayWriter",
    "java/io/BufferedWriter",
    "java/io/Writer",
    "java/io/PrintStream",
    "java/io/FileWriter",
    "java/io/OutputStreamWriter",
    // ARM_SB.
    "java/lang/StringBuilder",
    "java/lang/StringBuffer",
];

static MAP_LASTARM: [u64; SIG_WORDS] = sig_map(OWNERS_LASTARM);

/// Can any arm of the last-arm shortcut be keyed on this owner?
///
/// `false` is a PROOF OF ABSENCE: every non-wildcard arm of that match carries an `owner == ..`
/// guard drawn from [`OWNERS_LASTARM`], so a rejected owner reaches `_ => None` whatever the arm
/// cache happens to hold for it — the cache is keyed on POINTER identity, not on the name, and a
/// stale entry is exactly what those guards exist to reject. `true` says nothing at all, and the
/// shortcut runs exactly as before.
#[inline]
pub(crate) fn lastarm_may_serve(sig: u32) -> bool {
    sig_maybe(&MAP_LASTARM, sig)
}

/// Last-arm lookups the owner filter removed (`RAVA_CASCADE_STAT`). Counted so that "the filter
/// did not help" can be told apart from "the filter never fired".
pub(crate) static CASCADE3_SKIP: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// DEFAULT-ON. `RAVA_NO_FAST_CASCADE2=1` restores the unfiltered ladder — every inline owner arm
/// runs its own string compare for every call, exactly as it did — so a boot that goes wrong
/// bisects this whole family in one run. Also consults `emit_more_disabled`, so it falls together
/// with the other emit-more switches.
pub(crate) fn fast_cascade2() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FAST_CASCADE2").is_none()
    })
}

/// EVERY owner literal an inline arm of `dispatch_owner_cascade` is keyed on — the head fast
/// paths, the entry and optional route shorteners, the missing-file pre-test inside the nio
/// stage, and the whole ladder between the stage gauntlet and `owner_families`.
///
/// This list is the only thing standing between an arm and the filter that guards it, so it is
/// checked against this file's own source by `no_class_literal_in_the_cascade_is_unregistered`
/// below rather than by review. The arms that are NOT in it are the ones that cannot be: the
/// prefix tests (netty native transports, JFR) and the name-keyed arms (`wait`/`notify`,
/// `hasNext`/`next`, `iterator`, and the enum accessors the stages serve owner-agnostically).
/// Those carry no `inline_owner` term and run unconditionally, in place.
///
/// Since the segment split (`RAVA_NO_FAST_CASCADE4`) this is the UNION of
/// [`OWNERS_INLINE_HEAD`] and [`OWNERS_INLINE_TAIL`], and it keeps two jobs: it is what the
/// coarse `inline_owner` bit is built from (the arm the kill switch restores), and it is the set
/// the drift check polices, so a new arm still has to be registered here first and is then placed
/// in its segment by `every_inline_owner_is_in_its_segment`.
pub(crate) const OWNERS_INLINE: &[&str] = &[
    // Head fast paths: box unwrappers, the null/equality helpers, the two builders, the bit set,
    // and the cast intrinsic.
    "java/lang/Boolean",
    "java/lang/Integer",
    "java/lang/Short",
    "java/lang/Byte",
    "java/lang/Character",
    "java/lang/Long",
    "java/util/Objects",
    "java/lang/StringBuilder",
    "java/lang/StringBuffer",
    "java/util/BitSet",
    "java/lang/Class",
    // Post-shortcut route shorteners.
    "java/util/Map$Entry",
    "java/util/AbstractMap$SimpleEntry",
    "java/util/AbstractMap$SimpleImmutableEntry",
    "java/util/Optional",
    // The missing-file pre-test that runs ahead of the nio dispatcher.
    "java/nio/file/Files",
    // The ladder between the stage gauntlet and `owner_families`.
    "java/util/IntSummaryStatistics",
    "java/util/LongSummaryStatistics",
    "java/util/DoubleSummaryStatistics",
    "sun/misc/Unsafe",
    "jdk/internal/misc/Unsafe",
    "io/netty/util/internal/MacAddressUtil",
    "java/lang/Object",
    "java/security/AccessController",
    "java/lang/ref/Reference",
    "java/lang/ref/WeakReference",
    "java/lang/ref/SoftReference",
    "java/lang/ref/PhantomReference",
    "java/util/HexFormat",
    "java/util/SplittableRandom",
    "java/util/Random",
    "java/security/SecureRandom",
    "java/util/concurrent/ThreadLocalRandom",
    "java/nio/charset/Charset",
    "java/nio/charset/CharsetDecoder",
    "java/nio/charset/CharsetEncoder",
];

/// Class-shaped literals in the cascade that are NOT owner keys — exceptions a body raises and
/// classes a body allocates. Listed so the drift test can tell "not an owner" from "an owner
/// nobody registered": a NEW literal must be classified by whoever adds it, and until it is, the
/// build fails.
#[cfg(test)]
const NOT_OWNERS_INLINE: &[&str] = &[
    "java/lang/ClassCastException",
    "java/lang/NullPointerException",
    "java/lang/SecurityException",
    "java/lang/UnsatisfiedLinkError",
    "java/util/NoSuchElementException",
    "java/util/ConcurrentModificationException",
    "java/nio/file/NoSuchFileException",
    "java/nio/CharBuffer",
    "java/nio/charset/CoderResult",
];

static MAP_INLINE: [u64; SIG_WORDS] = sig_map(OWNERS_INLINE);

/// Can any inline arm of `dispatch_owner_cascade` be keyed on this owner?
///
/// `false` is a PROOF OF ABSENCE: the owner is in none of the arms' literal sets, so every arm
/// carrying `inline_owner` would have compared and fallen through. Skipping the compares IS their
/// answer, byte for byte. `true` says nothing at all, and the ladder runs exactly as before.
#[inline]
pub(crate) fn inline_may_serve(sig: u32) -> bool {
    sig_maybe(&MAP_INLINE, sig)
}

// ── THE LADDER IS TWO LADDERS (RAVA_NO_FAST_CASCADE4) ────────────────────────────────────────
// `inline_may_serve` is ONE bit over the UNION of every inline arm's owner literals, and the arms
// it guards are not one run: the five SERVICE STAGES (nio, net, stream, reflect, map) and the
// name-keyed monitor arm sit in the middle of them. So the ladder is really two, either side of
// that gauntlet, and the single bit admits an owner to BOTH whenever it belongs to EITHER.
//
// What that costs is not hypothetical. `java/lang/Boolean`, `java/lang/Character`,
// `java/lang/Short`, `java/lang/Byte`, `java/util/BitSet`, `java/lang/StringBuilder`,
// `java/lang/Class`, `java/util/Optional` and the three `Map$Entry` shapes are HEAD owners only —
// every one of their calls that the head does not serve (a name outside the fast arms; a
// `Boolean.valueOf`, a `Character.isDigit`, a `BitSet` method the arm declines) then walks all
// twenty-one TAIL compares on its way to `owner_families` and the tail tables. `java/lang/Object`,
// the `java/lang/ref/Reference` family, the three `Random`s and the three `Charset`s are TAIL
// owners only, and pay the head's compares the same way.
//
// Two maps, one `.rodata` cache line each, indexed by the SAME fingerprint the prologue already
// computed — no second hash, no memo, no allocation, nothing to invalidate. And each is a
// SMALLER literal set than their union (16 and 21 against 36), so each also rejects strictly more
// jar classes than the union does: a jar name that collides with a head literal still skips the
// tail ladder unless it collides there too.
//
// SOUNDNESS is the same argument as `inline_may_serve`, applied per segment: every arm in a
// segment carries `owner == <literal>` with that literal in the segment's list, so a rejected
// fingerprint proves the compare would have failed. Both lists are checked against THIS FILE's
// source, split at [`LADDER_SPLIT_MARKER`], by `every_inline_owner_is_in_its_segment` below — an
// arm that moves across the stage gauntlet, or a new arm registered in `OWNERS_INLINE` and in
// neither segment, fails the build rather than silently losing its fast path.

/// Owner literals of the inline arms that run BEFORE the service-stage gauntlet: the box
/// unwrappers, `Objects`, the two builders, `BitSet`, `Class.cast`, the `Map$Entry` accessors,
/// `Optional`, and the missing-file pre-test that runs inside the nio stage.
pub(crate) const OWNERS_INLINE_HEAD: &[&str] = &[
    "java/lang/Boolean",
    "java/lang/Integer",
    "java/lang/Short",
    "java/lang/Byte",
    "java/lang/Character",
    "java/lang/Long",
    "java/util/Objects",
    "java/lang/StringBuilder",
    "java/lang/StringBuffer",
    "java/util/BitSet",
    "java/lang/Class",
    "java/util/Map$Entry",
    "java/util/AbstractMap$SimpleEntry",
    "java/util/AbstractMap$SimpleImmutableEntry",
    "java/util/Optional",
    "java/nio/file/Files",
];

/// Owner literals of the inline arms that run AFTER the service-stage gauntlet and the
/// name-keyed monitor arm — the ladder between it and `owner_families`.
pub(crate) const OWNERS_INLINE_TAIL: &[&str] = &[
    "java/util/Objects",
    "java/util/IntSummaryStatistics",
    "java/util/LongSummaryStatistics",
    "java/util/DoubleSummaryStatistics",
    "sun/misc/Unsafe",
    "jdk/internal/misc/Unsafe",
    "io/netty/util/internal/MacAddressUtil",
    "java/lang/Object",
    "java/security/AccessController",
    "java/lang/ref/Reference",
    "java/lang/ref/WeakReference",
    "java/lang/ref/SoftReference",
    "java/lang/ref/PhantomReference",
    "java/util/HexFormat",
    "java/util/SplittableRandom",
    "java/util/Random",
    "java/security/SecureRandom",
    "java/util/concurrent/ThreadLocalRandom",
    "java/nio/charset/Charset",
    "java/nio/charset/CharsetDecoder",
    "java/nio/charset/CharsetEncoder",
];

static MAP_INLINE_HEAD: [u64; SIG_WORDS] = sig_map(OWNERS_INLINE_HEAD);
static MAP_INLINE_TAIL: [u64; SIG_WORDS] = sig_map(OWNERS_INLINE_TAIL);

/// Can an inline arm BEFORE the stage gauntlet be keyed on this owner? `false` is a proof of
/// absence over [`OWNERS_INLINE_HEAD`]; `true` says nothing and the head ladder runs as before.
#[inline]
pub(crate) fn inline_head_may_serve(sig: u32) -> bool {
    sig_maybe(&MAP_INLINE_HEAD, sig)
}

/// Can an inline arm AFTER the stage gauntlet be keyed on this owner? `false` is a proof of
/// absence over [`OWNERS_INLINE_TAIL`]; `true` says nothing and the tail ladder runs as before.
#[inline]
pub(crate) fn inline_tail_may_serve(sig: u32) -> bool {
    sig_maybe(&MAP_INLINE_TAIL, sig)
}

/// Head-ladder walks the segment split removed (`RAVA_CASCADE_STAT`) — counted so "the split did
/// not help" can be told apart from "the split never fired", the distinction four dead
/// instruments in this ledger were read without.
pub(crate) static CASCADE4_SKIP_HEAD: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// ...and tail-ladder walks it removed.
pub(crate) static CASCADE4_SKIP_TAIL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod inline_filter_tests {
    use super::*;

    /// Every registered owner must pass its own filter — the one direction in which a bug here is
    /// silent wrong execution rather than lost speed.
    #[test]
    fn every_inline_owner_is_admitted() {
        for o in OWNERS_INLINE {
            assert!(inline_may_serve(owner_sig(o)), "{o} rejected by its own filter");
        }
    }

    /// ⭐**THE DRIFT CHECK READS THE CASCADE'S OWN SOURCE.** The filter's whole safety argument is
    /// that `OWNERS_INLINE` lists every owner an `inline_owner`-guarded arm keys on. Review cannot
    /// hold that invariant across future edits; this can. A newly added arm's owner is a
    /// class-shaped literal above the marker, and this test fails the build until it is classified
    /// as an owner (`OWNERS_INLINE`) or as something a body merely names (`NOT_OWNERS_INLINE`).
    #[test]
    fn no_class_literal_in_the_cascade_is_unregistered() {
        let src = include_str!("cascade.rs");
        let body = src.split(FILTER_SECTION_MARKER2).next().unwrap_or(src);
        for lit in crate::natives::class_literals(body) {
            let known =
                OWNERS_INLINE.contains(&lit.as_str()) || NOT_OWNERS_INLINE.contains(&lit.as_str());
            assert!(
                known,
                "class literal {lit:?} appears in cascade.rs but is in neither OWNERS_INLINE nor \
                 NOT_OWNERS_INLINE. If it is the owner of a new inline arm it MUST go in \
                 OWNERS_INLINE, or `inline_may_serve` will skip that arm for every call."
            );
        }
    }

    /// A jar class must be REJECTED — otherwise the filter admits everything and buys nothing.
    /// Not a correctness assertion (admitting is always safe); a smoke test that the fingerprint
    /// still discriminates across a 36-literal set, whose 36 names light 30 distinct bits of 512
    /// for a ~5.9% false-positive rate.
    ///
    /// ⚠**AND ONE OF THE SIX BELOW IS A FALSE POSITIVE** (the gson tree map collides with a
    /// registered owner). It is left in deliberately: a false positive costs exactly the ladder
    /// that ran before this filter existed, so the honest bound is 5 of 6, not 6 of 6.
    #[test]
    fn representative_jar_classes_are_rejected() {
        let rejected = [
            "net/minecraft/world/level/levelgen/NoiseChunk",
            "net/minecraft/core/Holder$Reference",
            "it/unimi/dsi/fastutil/objects/Object2ObjectOpenHashMap",
            "com/mojang/serialization/codecs/RecordCodecBuilder",
            "net/minecraft/resources/ResourceLocation",
            "com/google/gson/internal/LinkedTreeMap",
        ]
        .iter()
        .filter(|o| !inline_may_serve(owner_sig(o)))
        .count();
        assert!(rejected >= 5, "the fingerprint is not discriminating: only {rejected}/6 rejected");
    }

    /// The map is `.rodata`, not a cache — one cache line, const-evaluated at compile time. This
    /// tree has measured a +6.8% REGRESSION from a front cache that grew past L1; a filter that
    /// costs one line cannot repeat it.
    #[test]
    fn the_inline_map_is_one_cache_line() {
        assert_eq!(std::mem::size_of_val(&MAP_INLINE), 64);
    }

    /// Every registered last-arm owner must pass its own filter — the one direction in which a
    /// bug here is silent wrong execution (the shortcut silently stops firing) rather than lost
    /// speed.
    #[test]
    fn every_lastarm_owner_is_admitted() {
        for o in OWNERS_LASTARM {
            assert!(lastarm_may_serve(owner_sig(o)), "{o} rejected by its own filter");
        }
    }

    /// ⭐**THE DRIFT CHECK READS `try_last_arm`'s OWN SOURCE — which lives in another file.**
    /// The filter's whole safety argument is that `OWNERS_LASTARM` lists every owner literal
    /// guarding an arm of that match. The match is in `entry.rs`, so the check reads `entry.rs`
    /// at compile time and slices out exactly that function: a new arm's owner is a class-shaped
    /// literal inside it, and this fails the build until it is registered here. Nothing outside
    /// the function is read, so unrelated edits to `entry.rs` cannot trip it.
    #[test]
    fn no_class_literal_in_try_last_arm_is_unregistered() {
        let src = include_str!("entry.rs");
        let start = src.find("pub(crate) fn try_last_arm(").expect(
            "try_last_arm's signature moved or was renamed in entry.rs — `lastarm_may_serve` \
             claims to know every owner literal in its match and can no longer check that claim.",
        );
        let rest = &src[start..];
        // A Rust fn ends at the first `}` in COLUMN 0; every construct inside this one is
        // indented, closures included. Scanned line by line rather than by searching for
        // `"\n}\n"` because `entry.rs` is checked out CRLF on this platform and that needle
        // would never match — a miss there would silently widen the slice to the rest of the
        // file and turn this check into a build break on somebody else's unrelated edit.
        let mut end = rest.len();
        let mut off = 0usize;
        for line in rest.split_inclusive('\n') {
            off += line.len();
            if line.starts_with('}') {
                end = off;
                break;
            }
        }
        let body = &rest[..end];
        for lit in crate::natives::class_literals(body) {
            assert!(
                OWNERS_LASTARM.contains(&lit.as_str()),
                "class literal {lit:?} appears in try_last_arm but is not in OWNERS_LASTARM. If \
                 it is the owner of a new shortcut arm it MUST go there, or `lastarm_may_serve` \
                 will skip that arm for every call."
            );
        }
    }

    /// A jar class must be REJECTED — otherwise the filter admits everything and buys nothing.
    /// Not a correctness assertion (admitting is always safe); a smoke test that thirteen
    /// literals still discriminate.
    #[test]
    fn representative_jar_classes_are_rejected_by_the_lastarm_filter() {
        let rejected = [
            "net/minecraft/world/level/block/state/BlockBehaviour$BlockStateBase",
            "it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap",
            "net/minecraft/world/phys/shapes/BitSetDiscreteVoxelShape",
            "com/mojang/serialization/codecs/RecordCodecBuilder",
        ]
        .iter()
        .filter(|o| !lastarm_may_serve(owner_sig(o)))
        .count();
        assert!(rejected >= 3, "the fingerprint is not discriminating: only {rejected}/4 rejected");
    }

    /// One more cache line of `.rodata`, and the module comment says so — this tree has measured
    /// a +6.8% REGRESSION from a front cache that grew past L1, so every table here states its
    /// size and asserts it.
    #[test]
    fn the_lastarm_map_is_one_cache_line() {
        assert_eq!(std::mem::size_of_val(&MAP_LASTARM), 64);
    }

    /// Every registered head/tail owner must pass ITS OWN segment filter — the one direction in
    /// which a bug here is silent wrong execution (an arm that stops firing) rather than lost
    /// speed.
    #[test]
    fn every_segment_owner_is_admitted_by_its_segment() {
        for o in OWNERS_INLINE_HEAD {
            assert!(inline_head_may_serve(owner_sig(o)), "{o} rejected by the head filter");
        }
        for o in OWNERS_INLINE_TAIL {
            assert!(inline_tail_may_serve(owner_sig(o)), "{o} rejected by the tail filter");
        }
    }

    /// ⭐**THE SEGMENT LISTS ARE CHECKED AGAINST THE ARMS' OWN POSITION IN THE SOURCE.** The split
    /// filter's safety argument is not "these owners exist somewhere in the cascade" — it is
    /// "this owner's arm is on THIS side of the service-stage gauntlet". Review cannot hold that
    /// across future edits; this can. The cascade's source is sliced at [`LADDER_SPLIT_MARKER`],
    /// and every registered owner literal appearing in a half must be in that half's list. An arm
    /// moved across the gauntlet, or a new arm added to `OWNERS_INLINE` and to neither segment,
    /// fails the build instead of silently losing its fast path.
    #[test]
    fn every_inline_owner_is_in_its_segment() {
        let src = include_str!("cascade.rs");
        let body = src.split(FILTER_SECTION_MARKER2).next().unwrap_or(src);
        let mut halves = body.splitn(2, LADDER_SPLIT_MARKER);
        let head = halves.next().unwrap_or("");
        let tail = halves.next().expect(
            "the post-gauntlet ladder's header comment moved or was reworded — the head/tail \
             filters claim to know which side of the service stages each arm is on and can no \
             longer check that claim.",
        );
        for lit in crate::natives::class_literals(head) {
            if OWNERS_INLINE.contains(&lit.as_str()) {
                assert!(
                    OWNERS_INLINE_HEAD.contains(&lit.as_str()),
                    "owner {lit:?} keys an arm BEFORE the service-stage gauntlet but is not in \
                     OWNERS_INLINE_HEAD, so `inline_head` will skip that arm for every call."
                );
            }
        }
        for lit in crate::natives::class_literals(tail) {
            if OWNERS_INLINE.contains(&lit.as_str()) {
                assert!(
                    OWNERS_INLINE_TAIL.contains(&lit.as_str()),
                    "owner {lit:?} keys an arm AFTER the service-stage gauntlet but is not in \
                     OWNERS_INLINE_TAIL, so `inline_tail` will skip that arm for every call."
                );
            }
        }
        // And nothing may be registered in a segment that the union does not know: the union is
        // what the `no_class_literal_in_the_cascade_is_unregistered` drift check polices, so a
        // segment entry outside it would be a filter with no drift check behind it.
        for o in OWNERS_INLINE_HEAD.iter().chain(OWNERS_INLINE_TAIL) {
            assert!(OWNERS_INLINE.contains(o), "{o} is in a segment list but not in OWNERS_INLINE");
        }
    }

    /// Each segment map is one `.rodata` cache line, like the other three in this family — the
    /// tree has measured a +6.8% REGRESSION from a front cache that grew past L1, so every table
    /// here states its size and asserts it. Two more lines takes the owner-filter family to six.
    #[test]
    fn the_segment_maps_are_one_cache_line_each() {
        assert_eq!(std::mem::size_of_val(&MAP_INLINE_HEAD), 64);
        assert_eq!(std::mem::size_of_val(&MAP_INLINE_TAIL), 64);
    }

    /// A segment filter must be at least as tight as the union it splits — otherwise it admits
    /// something the union rejects, which would be a widening, not a narrowing. Not a correctness
    /// assertion (admitting is always safe); a proof that the split can only ever remove walks.
    #[test]
    fn neither_segment_admits_what_the_union_rejects() {
        for sig in 0..SIG_BITS {
            if !inline_may_serve(sig) {
                assert!(!inline_head_may_serve(sig), "head admits sig {sig} the union rejects");
                assert!(!inline_tail_may_serve(sig), "tail admits sig {sig} the union rejects");
            }
        }
    }

    /// The gate word must carry one distinct bit per folded predicate and nothing else.
    #[test]
    fn gate_bits_are_distinct() {
        let all = [
            G_TRIVIAL, G_ENTRY, G_ITER, G_ITERNEW, G_FAST1, G_FAST1V, G_FAST2, G_STAT, G_FAST3,
            G_FAST3V, G_METER, G_FAST4, G_GATE, G_GATEV, G_ARENAMAP, G_COUNT, G_HIST,
            G_BRIDGESTAT, G_SCRATCH, G_DISPTLS_LEGACY, G_LEGACY5, G_ARMSTAT, G_CENSUS,
            G_CALLSEQ,
        ];
        let mut seen = 0u32;
        for b in all {
            assert_eq!(b.count_ones(), 1, "a gate bit is not a single bit");
            assert_eq!(seen & b, 0, "a gate bit is used twice");
            seen |= b;
        }
        // The entry-bit mask must name exactly the five bits that used to be `entry_gates()`'s
        // own word — `cascade_gates` masks with it, so a bit dropped from here is a flag that
        // silently stops working.
        assert_eq!(
            G_ENTRY_BITS,
            G_COUNT | G_HIST | G_BRIDGESTAT | G_SCRATCH | G_DISPTLS_LEGACY,
            "G_ENTRY_BITS no longer covers the folded entry-gate bits"
        );
        assert_eq!(G_ENTRY_BITS.count_ones(), 5);
    }
}
