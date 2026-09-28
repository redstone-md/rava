//! The compiled-call body: marshal, enter, unmarshal, and the GC discipline.
//!
//! OVER THE 500-LINE RULE ON PURPOSE. Same reasoning as vm/invoke.rs: this is the
//! interpreter-to-compiled boundary, taken on every compiled call. The one block big enough
//! to matter is the 145-line worker locked-execution guard, and lifting it means threading a
//! guard value whose type is built inline -- cost on the hot path for a cosmetic gain. Most of
//! what remains is prose: the code path itself is a gate, a plan lookup, a copy loop and a
//! dispatch.
//!
//! ⭐**THE CROSSING'S TWO STANDING RULES, both learned by measuring this file:**
//!
//! 1. **What is constant per SITE is not recomputed per CROSSING.** [`seam_plan`] holds the
//!    operand layout, the reference codes, the array bits, the return kind, whether the buffer
//!    can hold a root at all, and whether the callee could ever be a trivial getter. All of it is
//!    a pure function of `(owner, descriptor, kind)` — an immutable constant-pool entry — and all
//!    of it used to be re-derived from the descriptor on every one of ~8.4 M general-path
//!    crossings a boot.
//! 2. **What is constant per PROCESS is read once, not per crossing, and the cheap universal test
//!    comes before the expensive rare one.** Every instrument in this function is armed from an
//!    environment variable; [`seam_slow_gates`] is their disjunction, cached. And a fast path is
//!    entered only after the condition every one of its served shapes satisfies has been checked
//!    against a register.

use crate::*;

impl Vm {
    /// Convert only an arena reference array whose descriptor has already proved that the
    /// argument is an array.  Legacy handles, null, boxed values, and non-array arena objects
    /// retain their existing representation.  The per-call cache is lazy so the ordinary ABI
    /// path pays no allocation, while repeated arguments keep Java alias identity.
    #[inline]
    pub(crate) fn materialize_array_arg(
        &mut self,
        value: Value,
        declared_array: bool,
        materialized: &mut Option<Vec<(crate::arena::ArenaPtr, u32)>>,
    ) -> Value {
        #[cfg(feature = "next-gen-gc")]
        {
            if !declared_array {
                return value;
            }
            let source = match &value {
                Value::Obj(pointer) => *pointer,
                _ => return value,
            };
            if let Some(cache) = materialized.as_ref() {
                if let Some((_, handle)) = cache.iter().find(|(known, _)| *known == source) {
                    return Value::Array(*handle);
                }
            }
            let Some(array) = crate::heap::ArrayRef::from_value(&value) else {
                return value;
            };
            let Some(handle) = array.materialize(&mut self.heap) else {
                return value;
            };
            materialized
                .get_or_insert_with(Vec::new)
                .push((source, handle));
            Value::Array(handle)
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            let _ = (self, declared_array, materialized);
            value
        }
    }

    pub(crate) fn jit_call_body(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Result<i64, VmError> {
        // ONE gate for every INERT diagnostic below. `jit_call_body` is the compiled→runtime
        // choke and carries 3.8% of the serial prefix as SELF-time — time inside this
        // function, not its callees — and the first thing it did was read six separate
        // `OnceLock`s that are all false in production. Each is an atomic acquire load plus a
        // branch on a path taken millions of times.
        //
        // `any_call_diag` is the OR of exactly those six, so when ANY of them is on this is
        // true and every original condition runs unchanged; when none is (the default) the
        // six atomic loads collapse to one, and the rest become tests of a bool already in a
        // register. Behaviour is identical in both directions by construction.
        // ⭐**PRICING THE SEAM WITHOUT PRICING THE CALLEE.** `docs/per-op-price-list.md` puts
        // compiled->host crossings at 8.5 M x ~300 ns = 2.6 s, an estimate from an isolated
        // bench, and it is the largest line the in-situ re-pricing could not touch: a meter
        // around this whole function would time the guest method it invokes, which is the
        // "self time includes the callee's subtree" trap that already misread `bridgestat`.
        //
        // So the crossing is metered in its two real halves and the callee is excluded by
        // construction: this guard is dropped explicitly just before the dispatch, and a second
        // one opens after `frames::pop()`. Every early return -- the cheaper service arms below
        // -- charges its own prologue and nothing else.
        //
        // ⭐**AND THE SAME ARITHMETIC APPLIES ONE LEVEL UP: `any_call_diag` WAS THE THIRD OF
        // THREE GATES, NOT THE FIRST OF ONE.** Before it ran, this crossing had already read the
        // prologue meter's handle (a `OnceLock` acquire load) and `regions_armed` (two relaxed
        // loads of cells written once, from `main`), and after the callee returns it reads the
        // epilogue meter and the region guard again. Six process-constant reads and six branches
        // per crossing, at ~21 M crossings a boot, to discover that nothing is instrumented —
        // which is the answer in every production run, because every one of them derives from an
        // environment variable that is read once and never written again.
        //
        // [`seam_slow_gates`] is the OR of exactly that set, cached in one cell. False (always,
        // in production) proves every instrument in this function is dead for the whole process,
        // so the meters are never constructed, the region guard is never entered, and `diag` is
        // false by proof rather than by another load. True runs the original sequence unchanged,
        // so behaviour is identical in both directions by construction.
        let slow = seam_slow_gates();
        let _pro = slow.then(|| crate::diag::seam_prologue().start());
        let _reg = if slow {
            crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::Seam)
        } else {
            None
        };
        // `slow` DOMINATES this: `any_call_diag()` is one of the gates it ORs, so `!slow` proves
        // it false. The `&&` is a compare against a bool already in a register, not a load.
        let diag = slow && Self::any_call_diag();
        // Makes the disjunction ENFORCED rather than asserted in prose: if a gate is armed
        // while `diag` reads false, its block below is dead and the diagnostic would report
        // nothing — indistinguishable from a diagnostic that found nothing. Debug-only, so
        // the production path is unchanged.
        #[cfg(feature = "next-gen-gc")]
        debug_assert!(
            diag
                || !(receiver_guard_enabled()
                    || crate::jitsig::enabled()
                    || crate::recordast::enabled()
                    || crate::shadow::flat_enabled()
                    || crate::shadow::flat_arm_enabled()
                    || crate::refpot::enabled()),
            "a per-call diagnostic is armed but any_call_diag() is false — add it there"
        );
        // RECEIVER GUARD (opt-in RAVA_RECEIVER_GUARD) — forensic trap for the stale
        // arena-receiver Heisenbug. A virtual/interface call (kind 1/3) whose arena
        // receiver's CONCRETE class cannot resolve the callee method is a `this` that
        // went stale across a GC (its slot was reused by an unrelated type, e.g. a
        // DoubleArrayList where a DiscreteVoxelShape was expected). The PIC's
        // machine-code class_id compare already routes such a wrong-type receiver here
        // (its class is never cached for the site) — this is the GC-safe fault point
        // (args already marshalled), so it adds no callback on the success path and
        // no GC-timing shift. Log the exact traitor before the dispatch throws its ULE.
        #[cfg(feature = "next-gen-gc")]
        if diag && kind != 0 && receiver_guard_enabled() {
            let v = jit::unbox_ref(self, raw.first().copied().unwrap_or(0), "");
            if matches!(v, Value::Ref(_) | Value::Obj(_)) {
                self.receiver_guard_check(caller_ptr, cp, &v);
            }
        }
        // 23.1 signature probe (RAVA_JIT_SIG_PROBE, inert): classify this call by its static callee
        // descriptor to size the primitive-register-ABI addressable set. Runs on every call — kept to
        // one member_ref + a descriptor scan, gated off in prod.
        if diag && crate::jitsig::enabled() {
            let caller: &LoadedClass = unsafe { &*caller_ptr };
            if let Some(m) = caller.file.pool.member_ref(cp) {
                crate::jitsig::record(m.descriptor, kind);
            }
        }
        // 24.2b RecordAst observer (RAVA_JIT_RECORDAST_PROBE, inert): jit_call is the compiled→runtime
        // choke. With the PIC kill-switches on (RAVA_NO_IC_DEEP/HEAPPIC/DIRECTMONO), every compiled decode
        // call falls through here (no machcode-PIC bypass) → the boot's decode stream is fully visible.
        // The guard is held across this call's whole subtree; nested FieldDecoder.decodes (their own
        // jit_calls) record their field into the top record frame.
        #[cfg(feature = "next-gen-gc")]
        let _recordast_guard = if diag && (crate::recordast::enabled() || crate::shadow::flat_enabled() || crate::shadow::flat_arm_enabled()) && kind != 0 && {
            // member_ref gives the SYMBOLIC owner (the MapCodec/Decoder interface); the decode's CONCRETE
            // class (RecordCodecBuilder$2 / FieldDecoder) is the receiver's runtime class. Filter on the
            // method name "decode" first (cheap), then resolve the concrete class from raw[0].
            let caller: &LoadedClass = unsafe { &*caller_ptr };
            caller.file.pool.member_ref(cp).map(|m| m.name == "decode").unwrap_or(false)
        } {
            let recv = jit::unbox_ref(self, raw.first().copied().unwrap_or(0), "");
            let concrete: Option<String> = match &recv {
                Value::Ref(h) => self.heap.class_of(*h).map(|s| s.to_string()),
                Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).map(|s| s.to_string()),
                _ => None,
            };
            match concrete {
                Some(cls) => {
                    if crate::codec_flatten::is_decode(&cls, "decode") {
                        crate::recordast::note_decode_frame();
                    }
                    if cls == "com/mojang/serialization/codecs/FieldDecoder" && crate::recordast::in_record() {
                        let fname = self.codec_field_string(
                            &recv,
                            "com/mojang/serialization/codecs/FieldDecoder",
                            "name",
                        );
                        let fkind = self.recordast_field_kind(&recv);
                        crate::recordast::on_field(fname, fkind);
                    }
                    if crate::recordast::is_record_decode(&cls, "decode") {
                        // Capture the decode `input` (raw[2] = the ops target) for the 24.3 shadow-flat.
                        let input_v = jit::unbox_ref(self, raw.get(2).copied().unwrap_or(0), "");
                        Some(crate::recordast::enter_record(input_v))
                    } else {
                        None
                    }
                }
                None => None,
            }
        } else {
            None
        };
        // 23.3 heap-Ref PIC potential (RAVA_JIT_REF_POTENTIAL, inert): every virtual/interface call
        // with a NON-ARENA receiver that reaches jit_call came here via a machine-code PIC MISS (the
        // PIC's emit_is_arena check rejects heap Refs). Classify the jit_direct_call outcome for exactly
        // this population: a HIT = the callee resolved to a compiled fptr = the CLEAN reducible slice a
        // heap-Ref PIC (23.4 lever A) would move into machine code; a miss = the native/tier floor.
        // Measured once per call at jit_direct_call's return ⇒ consistent weighting (no neg-cache bias).
        #[cfg(feature = "next-gen-gc")]
        let refpot_na = diag && crate::refpot::enabled() && (kind == 1 || kind == 3) && {
            let r = raw.first().copied().unwrap_or(0);
            r != 0 && (r as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 // heap Ref (non-arena) receiver
        };
        // G1.1 TRUST+GUARD fast short-circuit: once a GIL-free worker has requested the region
        // abort, do NO further work on any callback — the region is unwinding to a serial retry, so
        // every remaining callee-dispatch on this worker returns a discarded 0. Keeps the compiled
        // root's tail from touching more state after the abort point.
        // P3-S1 EXCEPTION: under RAVA_M3_WORKER_LOCKED_EXEC this short-circuit is DISABLED — a
        // worker caught MID-ELEMENT by a sibling's abort then feeds garbage 0s into its live
        // compiled loop, whose result-dependent exit never fires (the measured deterministic
        // stuck-worker: idx frozen, lock free, abort=true). Locked-exec workers instead FINISH
        // the current element normally (every callee executes correctly under the lock) and
        // exit at the next for-index abort check — bounded, garbage-free convergence.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree()
            && crate::parallel::abort_requested()
            && !worker_locked_exec_enabled()
        {
            crate::vm::aot_route_diag::callback_early();
            return Ok(0);
        }
        // Fast path: call to an already-compiled callee → direct native call (no
        // synthetic frame, no Value marshaling). Static + exact-receiver-class-
        // guarded virtual/interface; anything else falls through.
        if !self.trampoline {
            // GILFREE-RINGS: `jit_direct_call` RESOLVES and INSERTS into the shared
            // per-site direct-call cache through `&mut Vm`. In the M3 forks that is
            // sound because the coordinator is PARKED in the scope join for the whole
            // region; this lane has no such park — main keeps running guest code under
            // the GIL — so the insert is a live data race. MEASURED: a boot with the
            // lane armed died in resource loading with `recv=Str("2.0")` where a
            // FallbackResourceManager was expected. Route lane workers to the
            // read-only arm built for exactly this (`&self`: cached hit → invoke, any
            // miss → None, no resolve, no insert).
            #[cfg(feature = "next-gen-gc")]
            // ⚠**CLOSE THE PROLOGUE METER HERE, NOT AT THE RETURN.** `jit_direct_call` does not
            // merely decide — it RUNS the callee when it hits. A guard still alive across it
            // charges the whole guest method to "the crossing", which is the same
            // self-time-includes-the-subtree trap this file's other comment warns about, and the
            // first reading taken this way said the decision cost 59 ns x 21.3 M = 3.8% of the
            // boot when most of that was callees executing. Dropped here, the meter covers what
            // every crossing pays BEFORE any service level is chosen, and nothing else.
            drop(_pro);
            #[cfg(all(feature = "next-gen-gc", windows, target_arch = "x86_64"))]
            if let Some(result) = self.aot_direct_call(caller_ptr, cp, kind, raw) {
                return result;
            }
            let r = if crate::gilfree_cf::in_lane() {
                self.jit_direct_call_worker(caller_ptr, cp, kind, raw, 0)
            } else {
                self.jit_direct_call(caller_ptr, cp, kind, raw)
            };
            #[cfg(not(feature = "next-gen-gc"))]
            let r = self.jit_direct_call(caller_ptr, cp, kind, raw);
            #[cfg(feature = "next-gen-gc")]
            if refpot_na {
                crate::refpot::record(unsafe { &*caller_ptr }, cp, r.is_some());
            }
            if let Some(r) = r {
                // 24.3 shadow-flat: this jit_call was a record decode (guard pushed a frame) that took the
                // direct path — compare the native flat read to the DFU result before returning.
                #[cfg(feature = "next-gen-gc")]
                if _recordast_guard.is_some() {
                    if let Ok(bits) = &r {
                        crate::shadow::flat_at_return(self, *bits);
                    }
                }
                crate::vm::aot_route_diag::direct_jit();
                return r;
            }
            // Prefer the native JIT direct cache. The legacy prepared-AOT path re-enters
            // `aot_run_input`, so reserve it for static calls the JIT cache cannot serve.
            #[cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]
            if kind == 0 && crate::vm::aot_direct::linux_enabled() {
                crate::vm::aot_route_diag::aot_direct_static_attempt().inc();
                if let Some(result) = self.aot_direct_call(caller_ptr, cp, kind, raw) {
                    crate::vm::aot_route_diag::aot_direct_static_hit().inc();
                    return result;
                }
            }
        } else {
            // DIAGNOSTIC (3c): compiled code should never call back into the runtime
            // while `trampoline` is set — the synthetic dispatch below would STAGE the
            // callee as pending and return without running it, losing the result. Log
            // the first few offenders (this was the JIT×trampoline registry corruption).
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 10 {
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                let m = caller.file.pool.member_ref(cp);
                eprintln!(
                    "[JIT-TRAMP] jit_call with trampoline=true #{n}: {}.{} kind={kind}",
                    caller.file.this_class,
                    m.map(|m| m.name).unwrap_or("?"),
                );
            }
        }
        // G1.1 TRUST+GUARD worker guard: reaching HERE on a GIL-free worker means the callee is NOT
        // served by a compiled direct call (jit_direct_call returned None) — the synthetic path below
        // would run the INTERPRETER, which races shared Vm state (site_cache / gc_protect / cold-callee
        // compilation) and is the exact hazard the static all-compiled proof guarded against. Under
        // the optimistic dynamic model we abort the region instead: request a serial retry and return
        // a discarded 0. Over-conservative (a megamorphic/cache-missed COMPILED callee also lands here
        // and aborts) but always SOUND — the arbiter is bit-exactness, and the tree warms across
        // retries until the runtime concrete targets are all compiled and the fork sticks.
        //
        // PILOT CENSUS (RAVA_PILOT_CENSUS, inert when unset): everything from HERE to the return is
        // exactly what `RAVA_M3_WORKER_LOCKED_EXEC` executes while holding the ONE global resolve
        // lock. Bracketing it on the ordinary serial path measures the lever's Amdahl serial term
        // without running a worker.
        // FALLBACK TALLY (RAVA_FALLBACK_TALLY): this is THE compiled→interpreter
        // invoke fallback entry — every call from here runs the `opcodes::invoke*`
        // gauntlet (or a native serve) instead of a compiled direct call. Two-word
        // key, no allocation; symbol + WHY resolved only at dump.
        //
        // Under `slow` because `fallback_tally::enabled()` is one of the gates it ORs — the note
        // itself opens with that same read, so this is the identical test hoisted, not a new one.
        // ⚠**IT STAYS FIRST.** The tally's contract is that it counts ENTRIES into the fallback
        // path, whether or not the iterator shortcut below then serves them; moving it after any
        // early return would silently change what it measures.
        if slow {
            crate::fallback_tally::note(
                crate::fallback_tally::Kind::Invoke(crate::fallback_tally::invoke_kind_op(kind)),
                caller_ptr as usize,
                cp,
            );
        }
        // ⭐**THE CROSSING'S LAST PROCESS CONSTANTS, IN ONE PLAIN PER-THREAD BYTE.**
        //
        // `slow` above collapsed the eight INSTRUMENT gates. What it could not cover is the four
        // gates that steer real behaviour rather than diagnostics — `seam_plan::enabled`,
        // `trivial_getter::enabled`, the lane's own `gilfree_cf::enabled` and this round's kill
        // switch — because a disjunction cannot answer four separate questions. They are read
        // here instead, off a `Cell<u8>` that is `const`-initialised and destructor-free, which
        // is the one thread-local shape `std` lowers to a bare TLS-slot load with no
        // `LocalKey::with` call, no lazy-init state word and no optimisation barrier. See
        // [`seam3_gates`].
        let g3 = seam3_gates();
        // ⭐**THE SITE'S PLAN IS LOOKED UP BEFORE THE SHORTCUT, NOT AFTER IT.**
        //
        // It used to be read just above the frame build, several hundred lines down, because
        // that is where its argument codes are consumed. But the plan word also carries
        // [`seam_plan::P_NO_ITER`] — a refusal the crossing needs BEFORE it calls
        // `jit_iter_shortcut`, and the probe that answers it (one hash, one line, two atomic
        // loads) is strictly cheaper than what the refusal removes: a `member_ref` (four
        // constant-pool `get`s) plus a four-way `&str` match, run on every zero-argument
        // general-path crossing whose callee can never be one of the four shapes served.
        //
        // Nothing between here and the old position touches a plan, so the value is the same
        // one; `seam_prologue_full` no longer includes the probe, which is the only observable
        // difference and is a diagnostic's scope, not a behaviour.
        let plan_on = s3_bit(g3, S3_PLAN, seam_plan::enabled);
        let mut plan = if plan_on { seam_plan::get(caller_ptr as usize, cp, kind) } else { 0 };
        // GAUNTLET SHORTCUT. Serve the `$elems`-backed Iterator hasNext/next HERE, before the
        // synthetic frame is built and before `opcodes::invokevirtual` runs.
        //
        // ⚠**THIS IS DEFAULT-ON, WHATEVER THE PARAGRAPH BELOW STILL SAYS.** The gate is
        // `flags::jit_iter_shortcut_enabled` = `RAVA_NO_JIT_ITER_SHORTCUT.is_none()`; the
        // "kept OPT-IN pending an A/B" note is left as written because it records why it was
        // built, but the A/B evidently happened and the flag was inverted. That matters for
        // anyone reading the cost of this line: the call below is taken on EVERY general-path
        // crossing, not on an opted-in run.
        //
        // WHY HERE and not in the native layer, where the same fast path already exists: the
        // fallback tally counts 12,223,017 compiled->interpreter fallbacks per boot and these two
        // names are 4,093,552 of them, but VERIFIED caller attribution puts the cost UPSTREAM of
        // the native layer - `opcodes::ref_parts` is 81.2% reached from `invokevirtual` and
        // `ClassLoader::super_chain` 82.6%, and those four invoke-path leaves together are 6.5-7.2%
        // of wall. Reaching the native fast path sooner cannot remove them; not entering the
        // gauntlet at all can.
        //
        // The guard is the one the native path already relies on: `$elems` must decode to a real
        // Array. A receiver without it (a jar iterator that needs draining, a null, a non-Ref)
        // returns None and takes the normal route, so behaviour is unchanged - this only shortens
        // the route. Kept OPT-IN until a timed A/B separates it: five straight work-removal
        // changes in this engine have measured null, and a mechanism argument is not a result.
        //
        // ⭐**THE PRECONDITION IS `raw.len() == 1`, AND IT WAS THE LAST THING TESTED.** Every one
        // of the four shapes the shortcut can serve — `hasNext()Z`, `next()Ljava/lang/Object;`,
        // `getKey`, `getValue` — is a zero-argument instance method, so a served crossing carries
        // exactly one operand cell: the receiver. That is one compare against a length already in
        // a register. What the shortcut used to reach it through is a `member_ref(cp)` — four
        // constant-pool `get`s, the very lookup [`seam_plan`] exists to remove — followed by four
        // string compares, and it ran on EVERY general-path crossing including the ones carrying
        // arguments, which it can never serve. Hoisting the cheap universal test in front of the
        // expensive rare one cannot change the answer: a call with arguments returned `None` from
        // inside the shortcut before, and skips it entirely now. The one shape this refuses that
        // the old test did not is a MALFORMED crossing — a zero-argument descriptor arriving with
        // more than one cell — and refusing there is a fall to the general path, which is the
        // correct answer for it, never a wrong one.
        //
        // ⭐**AND THE SECOND PRECONDITION IS A CONSTANT OF THE SITE, SO IT IS READ OFF THE PLAN.**
        // `raw.len() == 1` admits every zero-argument instance call, which is a far wider family
        // than the four shapes this can serve: the shortcut then re-reads the caller's constant
        // pool (`member_ref` = four pool `get`s) and matches the callee's name against four
        // literals, only to refuse, on every crossing of every `size()I`, `toString()`,
        // `iterator()` and `getX()` site forever. [`seam_plan::P_NO_ITER`] answers it once per
        // site from the descriptor alone — all four shapes return `Z` or `Ljava/lang/Object;`
        // and take no parameters — so an ineligible site now costs one masked test against a
        // word already in a register.
        //
        // Refusal sense, for the same reason [`seam_plan::P_NO_GETTER`] is a refusal: a site with
        // no plan (its first crossing, a descriptor too wide to describe, either kill switch)
        // answers `true` and runs the shortcut exactly as it always did. A site the shortcut
        // SERVES returns before the derive below and therefore never acquires a plan at all, so
        // the served population can never be refused by a bit it never gets.
        // ⭐**THE RING-SIZE FAST PATH (`RAVA_NO_RING_SIZE=1` to withdraw).** A zero-argument
        // instance call whose RESOLVED target's own bytecode is the exact 25-byte ring-buffer
        // length shape — or the canonical `size()==0` delegation to one — is answered here,
        // with no synthetic frame and no `opcodes::invoke*` gauntlet. This is the crossing
        // the lighting BFS queue pays 3.2 M times a boot: its `size()`/`isEmpty()` are plain
        // guest bytecode (no `$elems`, no compiled direct entry), so every call re-enters
        // here after the PIC/native refusals and would build a frame to run three field
        // reads and an `isub`.
        //
        // ADMISSION IS STRUCTURAL (see `fu_fastpath` for the full argument): the site's own
        // `(name, descriptor)` selector is resolved against the receiver's actual class via
        // the same `resolve_method` the general path runs, and the resolved body must match
        // the byte pattern at every offset — no guest class, field, package or method name
        // is consulted, and no `fu_routable` owner admission exists for this arm. The
        // `kind`/`raw.len()` gate here is the cheapest universal test (a virtual/interface
        // call with only the receiver cell); every further question is answered inside.
        //
        // A refusal — kill switch, null receiver, non-matching body, a worker's region
        // abort — is a `None` and the crossing continues exactly as before, so behaviour is
        // unchanged in both directions by construction.
        #[cfg(feature = "next-gen-gc")]
        if (kind == 1 || kind == 3)
            && raw.len() == 1
            && super::fu_fastpath::ring_size_enabled()
            && !self.trampoline
        {
            if let Some(bits) = self.ring_size_fastpath(caller_ptr, cp, raw) {
                crate::vm::aot_route_diag::fastpath();
                return Ok(bits);
            }
        }
        // ⭐**THE `Reference2ObjectArrayMap.<init>(I)V` SERVE (`RAVA_NO_R2OMAP_CTOR=1` to
        // withdraw, or the block switch `RAVA_NO_EMIT_MORE=1`).**
        //
        // THE LARGEST BRIDGE-CALLER ROW OF THE BLOCK-STATE PREFIX: `R2OArrayMap @cp61 ->
        // <init>(I)V` is 42.8% of block-1 NATIVE_BRIDGES on the PGO wall profile, with
        // `StateHolder @cp206` and `StateDefinition @cp257` the same callee at other sites —
        // every tick inside the block-state/StateHolder-construction prefix. The callee is
        // NeverCacheable (`dispatch_resolve_cacheable_cls_id`: the body contains `anewarray`,
        // so it is not a GC leaf and takes no deep entry), so the site's direct-mono cache bakes
        // `-1` and EVERY compiled caller crosses `rava_direct_miss` → `jit_call` → here → the
        // synthetic frame → `opcodes::invokespecial` — to run a four-write body (javap,
        // fastutil 8.5.18):
        //
        //     aload_0; invokespecial AbstractReference2ObjectMap.<init>()V;
        //     aload_0; iload_1; anewarray Object; putfield key;
        //     aload_0; iload_1; anewarray Object; putfield value; return
        //
        // The super chain is transitively empty (it terminates at `Object.<init>`, a no-op on
        // both representations), `size` keeps the fresh receiver's born-zero cell, and the two
        // `anewarray`s allocate `Object[capacity]` each. This arm performs exactly those writes
        // on the fresh arena receiver — the `DoubleArrayList.wrap` precedent
        // (`Vm::dal_wrap_native`): build the object's state directly instead of running the
        // interpreted/compiled ctor chain, and a decline is a `None` and the byte-identical
        // route. Kind 2 only: `<init>` is invoked solely by `invokespecial`.
        //
        // ADMISSION (the assignment's guard chain, no serve-time name key):
        //   * `Value::Obj(p)` receiver — the class is arena-born (a real bytecode class with
        //     declared fields, outside every native-backed namespace predicate);
        //   * the class word equals the memoised `fk::REF_ARRAY_MAP` verdict —
        //     `fu_fastpath::fu_kind_of` derives it once per class per thread on the arena class
        //     id, the same classification the shipped refmap arms key on; a subclass that
        //     overrides the ctor gets a different class id, classifies as a refusal, and runs
        //     its own body;
        //   * the site's own selector is exactly `<init>`/`(I)V` — read from the caller's pool
        //     via `member_ref`, the same read the marshal below would perform;
        //   * the `(key, value, size)` field ids come from `map::arraymap_ids` — the class's
        //     own resolved fields with all three layout slots proven present, never a literal.
        //
        // GC-SAFETY: the two fresh arrays are heap handles held only in Rust locals across
        // the second allocation — a Rust local is invisible to the root walk (the
        // `stream/dispatch.rs` forEach hazard note), so each array is rooted in `gc_protect`
        // for the window and the receiver rides a `NativeArgsGuard` whose slice the collector
        // forwards in place (Wall #19); the `size=0` write is a primitive cell that roots
        // nothing. `heap.alloc_array` can collect, so the receiver is re-read from the guarded
        // cell after the second allocation, never from the raw `p`.
        #[cfg(feature = "next-gen-gc")]
        if kind == 2
            && raw.len() == 2
            && crate::map::r2omap_ctor_enabled()
            && !self.trampoline
        {
            if let Some(bits) = self.r2omap_ctor_fastpath(caller_ptr, cp, raw) {
                crate::vm::aot_route_diag::fastpath();
                return Ok(bits);
            }
        }
        // ⭐**THE STRUCTURAL TRIVIAL-GETTER FAST PATH (`RAVA_NO_GETTER_FASTPATH=1` to
        // withdraw, or the block switch `RAVA_NO_EMIT_MORE=1`).** A zero-argument `()I`/`()Z`
        // instance call whose RESOLVED target's own bytecode is the exact five-byte field
        // read (`aload_0; getfield; ireturn`) or a 2–4-byte non-negative constant body is
        // answered here, with no synthetic frame and no `opcodes::invoke*` gauntlet — the
        // predicate family of the JIT_CALLBACK path's population, which would otherwise
        // build a frame to run five bytes.
        //
        // ADMISSION IS STRUCTURAL (see `getter_fastpath` for the full argument): the site's
        // own `(name, descriptor)` selector is resolved against the receiver's actual class
        // via the same `resolve_method` the general path runs, and the resolved body must
        // match the byte pattern exactly, through the retained `trivial_getter`
        // recognisers. It runs AFTER the ring arm because the shapes are disjoint by length
        // alone — the ring bodies are 25/15 bytes, a getter body is 5 (field) or 2–4
        // (const) — so a ring-shaped target can never reach this arm. A refusal — kill
        // switch, null receiver, non-matching body, a worker's region abort, a non-`Int`
        // cell — is a `None` and the crossing continues exactly as before, so behaviour is
        // unchanged in both directions by construction.
        #[cfg(feature = "next-gen-gc")]
        if (kind == 1 || kind == 3)
            && raw.len() == 1
            && super::getter_fastpath::getter_fp_enabled()
            && !self.trampoline
        {
            if let Some(bits) = self.getter_fastpath(caller_ptr, cp, raw) {
                crate::vm::aot_route_diag::fastpath();
                return Ok(bits);
            }
        }
        // ⭐**THE STRUCTURAL PURE-BODY SAM FAST PATH (`RAVA_NO_PURE_SAM=1` to withdraw, or
        // the block switch `RAVA_NO_EMIT_MORE=1`).** A kind-1/3 SAM call whose receiver
        // is a heap `Ref` lambda (kind 6, no captures) and whose RESOLVED implementation —
        // taken from the indy linkage record, resolved via the same `resolve_method` the
        // general path runs — is a small verified `iconst/iload/ifeq/ifne/if_icmp/goto/
        // ireturn` body is answered here, with no synthetic frame and no
        // `opcodes::invoke*` gauntlet — the predicate family of the `BooleanOp`-style
        // single-abstract-method population, which would otherwise build a frame to run
        // at most sixteen bytes.
        //
        // ADMISSION IS STRUCTURAL (see `pure_sam_fastpath` for the full argument): the
        // body check is the whole admission — no exceptions, not synchronized, static,
        // at most 16 bytes, whitelist opcodes only (a whitelist with no field, method,
        // array or allocation access by construction), and a functional-interface
        // identity gate requires the FI's unique abstract method to BE the caller's
        // selector — a true SAM invocation — so anything the general path routes
        // elsewhere declines. It runs AFTER the ring and getter arms because the shapes are
        // disjoint: both earlier arms require `raw.len() == 1`, so they never steal the
        // n≥1 SAM population, and a declined crossing rebuilds the interpreted path
        // byte-identically. A refusal — kill switch, mismatched descriptor, an FI
        // identity mismatch, null receiver, non-lambda, captures, a non-`Int` arg cell,
        // a non-matching body — is a `None` and the crossing continues exactly as before,
        // so behaviour is unchanged in both directions by construction.
        #[cfg(feature = "next-gen-gc")]
        if (kind == 1 || kind == 3)
            && !raw.is_empty()
            && raw.len() <= 3
            && super::pure_sam_fastpath::pure_sam_enabled()
            && !self.trampoline
        {
            if let Some(bits) = self.pure_sam_fastpath(caller_ptr, cp, raw) {
                crate::vm::aot_route_diag::fastpath();
                return Ok(bits);
            }
        }
        #[cfg(feature = "next-gen-gc")]
        if raw.len() == 1 && seam_plan::iter_candidate(plan) {
            if let Some(bits) = self.jit_iter_shortcut(caller_ptr, cp, kind, raw) {
                crate::vm::aot_route_diag::fastpath();
                return Ok(bits);
            }
        }
        // WARM-COMPILE RECORDING (RAVA_LANE_WARM_COMPILE). This IS the general path — reaching
        // here means the callee was not direct-callable, which off-GIL means "held the lock and
        // ran interpreted". The pilot runs this same body serially on main, so recording the
        // CONCRETE callee here and compiling it before the region opens turns the measured list
        // of locked callees into a compile list, with no guessing about which ones matter.
        // The concrete receiver class is the key: `force_compile` needs the bytecode that will
        // actually run, and a virtual site's declared owner is usually abstract.
        //
        // ⭐**THE LANE'S OWN PROCESS GATE COMES BEFORE THE LANE'S DYNAMIC FLAG.**
        // `warm_recording()` is a relaxed load of a global `AtomicBool` — cheap, but a global
        // memory read and a branch on every general-path crossing, and it cannot be hoisted out
        // of the caller's tree because it is an atomic. It can only ever be TRUE inside the
        // lane's batch pilot, and the pilot is unreachable with the lane off: `lane.pending` is
        // filled solely by `gilfree_cf::defer_with_args`, which returns on `!enabled()` before
        // pushing, `drain` returns on an empty batch, and `run_batch` is the only caller of
        // `warm_record_begin`. So `S3_LANE` false PROVES the flag false, and the test becomes a
        // compare against a byte already in a register. Under the kill switch the disjunction's
        // first arm is true and the flag is read unconditionally, exactly as before.
        #[cfg(feature = "next-gen-gc")]
        if (g3 & (S3_LIVE | S3_LANE) != 0) && crate::gilfree_cf::warm_recording() {
            let caller: &LoadedClass = unsafe { &*caller_ptr };
            if let Some(m) = caller.file.pool.member_ref(cp) {
                // The recorded target is (class, name, descriptor) of the body that will actually
                // RUN, because that is what `force_compile` resolves.
                let target: Option<(String, String, String)> = if kind == 0 {
                    Some((m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()))
                } else {
                    match jit::unbox_ref(self, raw.first().copied().unwrap_or(0), m.owner) {
                        Value::Ref(h) => self
                            .heap
                            .class_of(h)
                            .map(|c| (c.to_string(), m.name.to_string(), m.descriptor.to_string())),
                        Value::Obj(p) => {
                            // ARENA LAMBDA: its arena class is SYNTHETIC and shared by every site
                            // with the same capture kinds, so it has no bytecode and
                            // `force_compile` can never resolve a method on it. Recording it
                            // poisoned the whole warm list: measured, RingLane went from
                            // "warm-compile 37/46 general-path callees" to NO LINE AT ALL, and
                            // LANE_MS from 2891 to 238098 - every once-per-task callee then ran
                            // interpreted inside the lock. The interned site names the IMPL the
                            // call will really enter, so record that instead.
                            crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(p))
                                .map(|site| {
                                    (
                                        site.owner.to_string(),
                                        site.name.to_string(),
                                        site.descriptor.to_string(),
                                    )
                                })
                                .or_else(|| {
                                    crate::nextgen::class_name(crate::nextgen::arena_class_id(p))
                                        .map(|s| {
                                            (
                                                s.to_string(),
                                                m.name.to_string(),
                                                m.descriptor.to_string(),
                                            )
                                        })
                                })
                        }
                        _ => None,
                    }
                };
                if let Some((c, n, d)) = target {
                    crate::gilfree_cf::warm_note(&c, &n, &d);
                }
            }
        }
        // `pilot_census::enabled()` is one of the gates `slow` ORs, and `general_scope` opens with
        // that same read — so `!slow` proves the scope would have returned `None`, and the
        // thread-local depth bookkeeping plus the guard's `Drop` disappear with it.
        let _pilot_general = if slow {
            crate::pilot_census::general_scope((caller_ptr as usize, cp), || {
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                caller.file.pool.member_ref(cp).map_or_else(
                    || "?".to_string(),
                    |m| format!("{}.{}{}", m.owner, m.name, m.descriptor),
                )
            })
        } else {
            None
        };
        #[cfg(feature = "next-gen-gc")]
        let _worker_exec_guard = if crate::parallel::gilfree() {
            // P3-S1/S2-lite (RAVA_M3_WORKER_LOCKED_EXEC): instead of ABORTING the region on a
            // non-direct callee (the G1.1 guard — measured: JDK native-served methods like
            // List.get / the spline search can NEVER become direct-callable, so the region
            // always died → silent serial), EXECUTE the general path UNDER the re-entrant
            // resolve lock. During a fork region main is PARKED at the scope join and other
            // workers block on this same lock ⇒ the holder has exclusive &mut Vm — the exact
            // safety envelope the locked dispatch_miss handler already relies on. Compute-heavy
            // dispatch stays lock-free via the freeze-resolve table; only the native/uncompiled
            // moments serialize. Workers still must not JIT-compile (enforced at the compile
            // site) and still abort on nursery overflow / clinit (unchanged paths).
            if worker_locked_exec_enabled() {
                // WS-NATIVE fast path (lock-free): pure-read natives a worker can run without
                // the lock (measured composition: List#get 914K + findIntervalStart 488K = ALL
                // of the locked traffic). ArrayList.get = read-only $elems + array_get; box_ref
                // routes to the worker's arena segment (same discipline as the frozen direct
                // cache path). Exact-class gate: only java/util/ArrayList (a subclass could
                // override get). Out-of-bounds falls to the locked general path (exception).
                #[cfg(feature = "next-gen-gc")]
                if kind == 1 || kind == 3 {
                    if let Some(bits) = self.ws_arraylist_get(caller_ptr, cp, raw) {
                        crate::vm::aot_route_diag::fastpath();
                        return Ok(bits);
                    }
                    // `Set.contains` on an already-synced membership index. The per-site
                    // freeze-coverage census puts it at 3249 calls per ring task, all
                    // `unknown`, i.e. all falling through to the one global lock — and the
                    // span each fall-through anchors is what keeps eight workers one-wide.
                    // Serves the synced case only; an index rebuild is a shared mutation and
                    // stays on the locked path.
                    if ws_set_contains_enabled() {
                        if let Some(bits) = self.ws_list_contains(caller_ptr, cp, raw) {
                            crate::vm::aot_route_diag::fastpath();
                            return Ok(bits);
                        }
                    }
                    // `ThreadLocal.get`/`set` — 825,246 locked acquisitions per region that stop
                    // being shared at all once the value is per-thread. See `ws_threadlocal`.
                    if let Some(bits) = self.ws_threadlocal(caller_ptr, cp, raw) {
                        crate::vm::aot_route_diag::fastpath();
                        return Ok(bits);
                    }
                    // `Optional.isPresent`/`isEmpty`/`get` — 113,117 calls EACH per ring task in
                    // the freeze census, every one of them `unknown` and therefore locked, on an
                    // object that is immutable after construction. See `ws_optional`.
                    if let Some(bits) = self.ws_optional(caller_ptr, cp, raw) {
                        crate::vm::aot_route_diag::fastpath();
                        return Ok(bits);
                    }
                }
                #[cfg(feature = "next-gen-gc")]
                if kind == 0 && spline_native_enabled() {
                    let caller: &LoadedClass = unsafe { &*caller_ptr };
                    if let Some(m) = caller.file.pool.member_ref(cp) {
                        // Exact owner, matching the vm/invoke.rs and jit_run.rs twins: a
                        // same-signature `findIntervalStart` on any other class runs bytecode.
                        if m.name == "findIntervalStart"
                            && m.descriptor == "([FF)I"
                            && m.owner == "net/minecraft/util/CubicSpline$Multipoint"
                        {
                            // Raw-cell decode (unbox_ref does not produce Array/Float here):
                            // arg0 = ARRAY-tagged cell, arg1 = f32 bits in the low 32.
                            if let Some(h) = jit::untag_array(raw.first().copied().unwrap_or(0)) {
                                let fv = f32::from_bits(raw.get(1).copied().unwrap_or(0) as u32);
                                if let Some(r) = self
                                    .spline_find_interval_start(&[Value::Array(h), Value::Float(fv)])
                                {
                                    if crate::jit::freeze_resolve::stats_on() {
                                        crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    crate::vm::aot_route_diag::fastpath();
                                    return Ok(jit::pack_local(&Value::Int(r)));
                                }
                            }
                        }
                    }
                }
                crate::jit::freeze_resolve::WORKER_LOCKED_CALLS
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // ⚠ RAVA_LANE_NOLOCK — DELIBERATELY UNSOUND, MEASUREMENT ONLY, never a default.
                //
                // The lane is a 35-45% LOSS today because this one lock is held across the
                // callee's whole EXECUTION, so eight workers run one at a time. Making it sound
                // means growing `&self` twins for the seventeen `&mut` leaves the interpreter
                // reaches (docs/vm-shared-exec-plan.md) — days of work whose payoff is unmeasured.
                //
                // This flag buys the payoff number FIRST, for the price of one boot: skip the
                // acquisition and read the wall. Results may be wrong and the run may crash, and
                // neither invalidates the timing — what is being measured is how fast eight
                // workers CAN go, not what they compute. If the wall does not move, the arc is
                // worthless and nobody spends the days.
                let g = crate::jit::resolve_lock_acquire();
                // OUTER ATTRIBUTION (RAVA_LANE_OUTER): name the callee that ANCHORS this
                // serialized span. Only on a genuine outer acquisition — a nested
                // pass-through runs inside somebody else's span and must not re-anchor it.
                #[cfg(feature = "next-gen-gc")]
                if crate::jit::outer_attrib::enabled() && g.is_outer() {
                    let caller: &LoadedClass = unsafe { &*caller_ptr };
                    let sym = caller.file.pool.member_ref(cp).map_or_else(
                        || format!("{}.?cp{cp}", caller.file.this_class),
                        |m| format!("{}.{}{}", m.owner, m.name, m.descriptor),
                    );
                    crate::jit::outer_attrib::refine_sym(&sym);
                }
                // Composition tally (under the lock — plain map is safe): WHICH callees
                // still serialize.
                //
                // Gated on the LANE PROBE rather than RAVA_FORK_TRACE. Two reasons: the tally
                // is only meaningful beside the probe's own lock numbers, and FORK_TRACE also
                // arms per-call eprintlns that make a lane boot exit early (measured twice —
                // 48.6s and 48.1s, both before Done), so it could never report on a run that
                // reached the ring. The env read is hoisted into a OnceLock because this sits
                // on the acquisition path.
                if lane_locked_tally_enabled() {
                    let caller: &LoadedClass = unsafe { &*caller_ptr };
                    if let Some(m) = caller.file.pool.member_ref(cp) {
                        crate::jit::freeze_resolve::locked_tally(m.owner, m.name);
                    }
                }
                // ⚠ RAVA_LANE_NOLOCK: drop the guard immediately instead of holding it across the
                // callee's execution. Execution then proceeds down exactly the same path, minus
                // the exclusion — which is the whole question this experiment prices.
                if lane_nolock_experiment() {
                    drop(g);
                    None
                } else {
                    Some(g)
                }
            } else {
                // Legacy G1.1 behavior: name the callee (diagnostic), abort to serial.
                if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
                        let caller: &LoadedClass = unsafe { &*caller_ptr };
                        let (o, nm, d) = caller.file.pool.member_ref(cp)
                            .map(|m| (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()))
                            .unwrap_or_default();
                        let recv = jit::unbox_ref(self, raw.first().copied().unwrap_or(0), "");
                        let concrete = match &recv {
                            Value::Ref(h) => self.heap.class_of(*h).map(|s| s.to_string()),
                            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).map(|s| s.to_string()),
                            _ => None,
                        };
                        let compiled = concrete.as_ref().and_then(|c| self.fork_leaf_key(c, &nm, &d))
                            .map(|k| matches!(self.jit_cache.get(&k), Some(jit::JitStatus::Compiled(_))));
                        eprintln!("[FORK-GUARD-CALLEE] kind={kind} sym={o}.{nm}{d} recv_concrete={concrete:?} target_compiled={compiled:?}");
                    }
                }
                crate::parallel::request_abort();
                crate::vm::aot_route_diag::callback_early();
                return Ok(0);
            }
        } else {
            None
        };
        // SPLIT THE PROLOGUE. The whole-prologue meter says 20,987,031 executions at 98.5 ns
        // trimmed, while the epilogue -- which only the general path reaches -- counts
        // 8,375,708. **So 12.6 M crossings return through an early arm, and every one of them
        // still pays whatever runs above this line before it can decide.** This second meter
        // covers only what the general path adds: the Arc round-trip, the member_ref lookup, the
        // argument unpack loop and the synthetic frame. Subtracting it from the total gives the
        // price of the decision itself, which is the part every crossing pays no matter which
        // service level ends up serving it.
        // CHOKE-RESIDUE census (`RAVA_CHOKE_RESIDUE`, observe-only, default OFF): which
        // crossings survive the WHOLE ladder above (direct call, ring, getter, SAM, iterator,
        // worker-exec) and reach the frame build. Necessarily AFTER every early arm and —
        // literal adjacency — BEFORE `seam_prologue_full` starts, so no early return can sit
        // between note and meter and the census total equals the meter's event count by
        // construction (the dump header MATCH-verifies it). `fallback_tally::note` above
        // counts ENTRIES into the fallback path; the difference is what the arms serve.
#[cfg(feature = "next-gen-gc")]
        if slow && super::choke_residue::enabled() {
            super::choke_residue::note(kind, caller_ptr as usize, cp, raw);
        }
        let _pro_full = slow.then(|| crate::diag::seam_prologue_full().start());
        // ⛔⭐**A REFCOUNT PAIR GUARDING A COUNT THAT PROVABLY CANNOT REACH ZERO.**
        //
        // The historic shape (still taken under the kill switch, and still the `false` arm
        // below): bump the strong count, then take ownership of one via `from_raw`, so the temp
        // `Arc` and any clones it spawns in the frame drop at end of scope and restore the
        // original count. Its own justification names the reason it is unnecessary — *"the
        // loader keeps the class alive throughout"*. That is not a hope: `ClassLoader` never
        // removes a loaded class (the only `clear()`s in it are the miss/super-chain/type-closure
        // CACHES), so a `LoadedClass` lives for the process, which is the same premise
        // [`seam_plan`] already relies on to key its rows by `caller_ptr` with no invalidation.
        //
        // So the pair is one `lock xadd` on entry and one `lock xsub` plus the is-this-the-last
        // branch on exit, per crossing, protecting a count whose other holder outlives the whole
        // call — at 8,375,708 general-path crossings a boot. `vmframe::FrameGuard::borrowed`
        // already removed exactly this pair from the interpreted call for exactly this reason;
        // this is the compiled crossing's copy of it.
        //
        // # Safety
        // * `caller_ptr` is a live `Arc<LoadedClass>` pointer — the precondition
        //   `Arc::increment_strong_count` demanded on this very line before, unchanged.
        // * The bitwise copy must never be DESTROYED. That is what [`SeamFrame`] enforces: the
        //   frame is wrapped in `ManuallyDrop` and the `Arc` field is reached only on the arm
        //   that owns a count, on the return path AND on the unwind path alike.
        // * Nothing may move the frame's `class` out by value. Every reader in the tree takes
        //   `&frame.class`, `Arc::as_ptr(&frame.class)` or `.clone()` — and a clone takes a REAL
        //   reference off the loader's live count, so the callee frames it builds are unaffected.
        //
        // ⚠**AND THE HANDLE IS MATERIALISED AT THE LAST MOMENT, not here.** Everything between
        // this point and the frame build reads the class, never owns it, and one of those reads
        // (`member_ref` on a pool entry that has gone) can `return Ok(0)`. An `Arc` local built
        // this early would be dropped on that return — releasing a count the borrowed arm never
        // took. A plain `&LoadedClass` cannot: its lifetime rests on the same
        // never-unloaded-class premise, and it is what the `&str`s in `undescribed` borrow from.
        let borrow_class = g3 & (S3_LIVE | S3_BORROW) == S3_BORROW;
        // SAFETY: as the refcount bump this replaces — `caller_ptr` is a live `LoadedClass` for
        // the whole crossing, and the loader holds it for the life of the process.
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        // ⭐**THE CROSSING'S SHAPE IS A CONSTANT OF THE SITE, AND IT WAS RE-DERIVED EVERY TIME.**
        //
        // Everything the frame build needs — how many operand slots, which of them are
        // references, whether the array bit is set on each, whether the return is a reference —
        // is a pure function of `(owner, descriptor, kind)`, i.e. of the constant-pool entry the
        // site names. The general arm nevertheless re-computed all of it per crossing: a
        // `member_ref` (four pool `get`s), `args_of` (two linear scans of the descriptor), a
        // byte-walk per argument that re-parses `L...;` / `[[I` shapes, a `&str` slice built per
        // reference argument purely so `unbox_ref` could read ONE bit off its first byte, and a
        // backwards scan of the whole descriptor for the return kind. `seam_prologue_full` counts
        // 8,375,708 general-path crossings a boot, so that is eight million re-derivations of
        // numbers that never change.
        //
        // [`seam_plan`] answers it once per site and hands back a 64-bit plan. The build below is
        // then a COPY AGAINST A PLAN: one table row, then a straight loop that reads a 3-bit code
        // per slot and calls `unbox_ref_kind`/`unpack_arg` with the answer already in hand — no
        // pool lookup, no descriptor, no substring. Kill switch `RAVA_NO_FAST_SEAM=1` (and
        // `RAVA_NO_EMIT_MORE=1`) leaves `plan` at 0 forever, which is byte-for-byte the old path.
        //
        // `plan` and `plan_on` are resolved far above, beside the iterator shortcut that reads
        // the same word — see the note there for why the probe moved and what it costs.
        // Borrow the owner/descriptor as &str (they live in the caller's constant
        // pool, kept alive below) instead of allocating a String per call — this path
        // runs on EVERY JIT→runtime invoke callback (~0.5B during a boot), so the two
        // per-call String allocations were a dominant source of GC churn.
        //
        // Only reached when the site has no plan yet (first crossing, a descriptor too wide to
        // describe, or the kill switch): the plan-served path never touches the pool at all.
        let mut undescribed: Option<(&str, &str)> = None;
        if plan == 0 {
            let (owner, desc) = match caller.file.pool.member_ref(cp) {
                Some(m) => (m.owner, m.descriptor),
                None => {
                    crate::vm::aot_route_diag::callback_early();
                    return Ok(0);
                }
            };
            match if plan_on { seam_plan::derive(owner, desc, kind) } else { None } {
                Some(p) => {
                    seam_plan::record(caller_ptr as usize, cp, kind, p);
                    plan = p;
                }
                None => undescribed = Some((owner, desc)),
            }
        }
        // ⭐**WHICH CALLS ARE THE 57.7 MILLION?** (`RAVA_JIT_CB_CENSUS`, inert otherwise.) Row 438
        // priced the compiled→host seam at ~14% and nine callbacks per compiled entry, but
        // nothing said WHICH callees they are — and a typed direct-call path has to be built for
        // a named family, not for "calls in general".
        #[cfg(feature = "next-gen-gc")]
        if slow && crate::jit_cb_census::enabled() {
            if let Some(m) = caller.file.pool.member_ref(cp) {
                crate::jit_cb_census::note(m.owner, m.name, kind);
            }
        }
        // Unpack args: receiver first for non-static (its static class is the
        // call's owner — needed to rebuild a boxed-primitive receiver, e.g.
        // Integer.intValue on a Value::Int), then each descriptor parameter typed
        // by its own class. Push onto the synthetic stack in the order invoke*'s
        // pop_args/pop expect (receiver pushed first → deepest).
        let is_static = kind == 0;
        // Pooled operand-stack buffer (returned below) — avoids a Vec allocation on
        // every one of the ~0.5B JIT→runtime invoke callbacks in a boot.
        let mut stack: Vec<Value> = self.take_vec();
        // Only the first arena array lazily allocates this cache.  It preserves aliasing when
        // the same raw array is supplied in more than one descriptor slot.
        let mut materialized_arrays: Option<Vec<(crate::arena::ArenaPtr, u32)>> = None;
        let ref_ret;
        // Does anything that will sit in this frame's operand buffer need to be visible to the
        // collector? See [`seam_plan::P_ROOTLESS`] — false unless PROVEN otherwise.
        let mut rootless = false;
        if let Some((owner, desc)) = undescribed {
            let mut ai = 0usize;
            if !is_static {
                let receiver = jit::unbox_ref(self, *raw.first().unwrap_or(&0), owner);
                stack.push(self.materialize_array_arg(
                    receiver,
                    owner.as_bytes().first() == Some(&b'['),
                    &mut materialized_arrays,
                ));
                ai = 1;
            }
            let inner = args_of(desc);
            let bytes = inner.as_bytes();
            let mut bi = 0usize;
            while bi < bytes.len() && ai < raw.len() {
                let start = bi;
                let k = jit_arg_kind(bytes, &mut bi);
                if k == b'L' {
                    let argument = jit::unbox_ref(
                        self,
                        raw[ai],
                        &inner[start..bi.min(inner.len())],
                    );
                    stack.push(self.materialize_array_arg(
                        argument,
                        bytes.get(start) == Some(&b'['),
                        &mut materialized_arrays,
                    ));
                } else {
                    stack.push(jit::unpack_arg(raw[ai], k));
                }
                ai += 1;
            }
            // Return-kind now, while `desc` is still borrowable. (It used to have to be read
            // here so `caller` could be MOVED into the frame rather than `Arc::clone`d; the
            // class is a plain `&LoadedClass` at this point now, so the ordering is merely
            // tidy rather than load-bearing.)
            let rb = desc.as_bytes();
            let rstart = byte_rfind(desc, b')').map(|p| p + 1).unwrap_or(desc.len());
            ref_ret = matches!(rb.get(rstart), Some(b'L') | Some(b'['));
        } else {
            // PLAN-SERVED. Identical values by construction: `derive` records exactly the kind
            // byte `jit_arg_kind` returned and exactly the `[`-prefix bit `unbox_ref` reads off
            // the type substring, and the `ai < raw.len()` truncation the old loop performed for
            // a short `raw` is reproduced verbatim.
            let n = seam_plan::nargs(plan);
            stack.reserve(n + 1);
            let mut ai = 0usize;
            if !is_static {
                let receiver = jit::unbox_ref_kind(
                    self,
                    *raw.first().unwrap_or(&0),
                    plan & seam_plan::P_RECV_ARRAY != 0,
                );
                stack.push(self.materialize_array_arg(
                    receiver,
                    plan & seam_plan::P_RECV_ARRAY != 0,
                    &mut materialized_arrays,
                ));
                ai = 1;
            }
            let mut k = 0usize;
            while k < n && ai < raw.len() {
                let code = seam_plan::arg_code(plan, k);
                let argument = if code >= seam_plan::K_REF {
                    jit::unbox_ref_kind(self, raw[ai], code == seam_plan::K_REF_ARRAY)
                } else {
                    jit::unpack_arg(raw[ai], seam_plan::PRIM_KIND[code as usize])
                };
                stack.push(self.materialize_array_arg(
                    argument,
                    code == seam_plan::K_REF_ARRAY,
                    &mut materialized_arrays,
                ));
                k += 1;
                ai += 1;
            }
            ref_ret = plan & seam_plan::P_REF_RET != 0;
            rootless = plan & seam_plan::P_ROOTLESS != 0;
        }
        // Synthetic caller frame: empty locals, receiver+args on the stack, the
        // caller's class for the constant-pool lookup + cache keys (so invoke*'s
        // call_cache/inline_cache entries match the interpreter's, keyed by the
        // same Arc::as_ptr(class)+cp). Register it as a GC root for the window
        // before invoke* moves the args into the callee frame.
        //
        // The class handle the frame carries, built HERE — no early return stands between this
        // line and the guard that decides its fate, so the count (when one is taken at all) is
        // released exactly once, on the return path and the unwind path alike.
        //
        // SAFETY: `caller_ptr` is a live `Arc<LoadedClass>` pointer. The `true` arm is the
        // historic bump-then-adopt; the `false` arm adopts WITHOUT a bump and is sound only
        // because [`SeamFrame`] never lets the resulting handle be destroyed — see the note on
        // `borrow_class` above for the full precondition.
        let class: Arc<LoadedClass> = unsafe {
            if !borrow_class {
                Arc::increment_strong_count(caller_ptr);
            }
            Arc::from_raw(caller_ptr)
        };
        let mut frame = SeamFrame::new(
            Frame {
                pc: 0,
                // Synthetic, as in `jit_natives`: one opcode, no method body.
                code: std::ptr::null(),
                buf: stack,
                nlocals: 0,
                class,
            },
            !borrow_class,
        );
        // ⭐**A ROOT REGISTRATION THAT CANNOT REGISTER A ROOT IS PURE OVERHEAD.** The push exists
        // for exactly one window — between filling this buffer and `invoke*` moving the values
        // into the callee's own frame, resolution can load a class and run a `<clinit>`, and a
        // moving collection there must find the arguments. When the plan proves the buffer holds
        // no reference at all (a static call, every parameter primitive, a primitive return, so
        // the value `invoke*` pushes back is primitive too) there is nothing in it for the
        // collector to find, and the registration is a TLS access, a `RefCell` borrow pair and a
        // `Vec` push/pop per crossing spent on an empty root set.
        //
        // Refusing is invisible: the synthetic frame carries `code: null`, so `stacktrace::
        // method_of` already skips it and no guest backtrace ever named it.
        // ⚠**THE ADDRESS REGISTERED IS THE `Frame`'S OWN, NOT THE GUARD'S.** The collector casts
        // this `usize` straight back to a `*const Frame`; `SeamFrame` is a plain Rust struct with
        // no layout guarantee, so its address is not the frame's. `frame_addr` goes through the
        // `ManuallyDrop` (which IS `repr(transparent)`) and hands back the field's address.
        if !rootless {
            frames::push(frame.frame_addr());
        }
        // Callee starts here: close the general-path meter BEFORE the guest method runs.
        // (`_pro` is already closed above, on the line before the direct-call attempt.)
        drop(_pro_full);
        // ⚠**THE REGION ENDS WITH THE MARSHALLING, NOT WITH THE CALL.** Regions nest and the
        // innermost wins, but a COMPILED callee runs with no region of its own — leaving this one
        // open across the dispatch would attribute the callee's object births to the seam.
        drop(_reg);
        // ⭐**RECORD THE TRIVIAL ARENA GETTER THIS CALL IS ABOUT TO PERFORM.**
        //
        // This is the one place that sees BOTH halves at once: the concrete receiver (so its
        // arena class id and layout are known) and the resolved callee (so its body can be
        // read). A site whose callee is exactly `aload_0; getfield f; <t>return` gets its
        // `(class id, slot)` written here, and the next compile of that site answers the call
        // from one cell load instead of this crossing.
        //
        // ⛔**"RUNS AT MOST ONCE PER SITE" WAS TRUE ONLY OF THE SITES THAT SUCCEED.** The header
        // above claimed the entry test is one relaxed load, and it is — for a site that gets
        // DESCRIBED, because `is_empty` is false ever after. A site that cannot be described is
        // never recorded, so `is_empty` stays true and the whole derivation re-runs on every
        // crossing, forever: a `member_ref`, an `Arc<str>` clone for the class name (an atomic
        // pair), two `String` allocations for the name and descriptor, and a `resolve_method`
        // hierarchy probe keyed on all three. That population is not small — it is every
        // zero-argument virtual call reaching the general path whose callee has a real body
        // (`iterator`, `toString`, `size` on a class that computes it), plus every site whose row
        // lost the 8192-way collision in `trivial_getter`'s own table.
        //
        // Two answers, both of which the shape already knows or can hold:
        //
        // * **[`seam_plan::P_NO_GETTER`]** is a constant of the constant-pool entry: only a
        //   non-static, zero-parameter, non-`void` descriptor can ever name a getter, so anything
        //   else is refused at derive time. Read off the plan word that is already in a register,
        //   so an ineligible site now costs one masked test instead of the pool lookup, the
        //   `Arc<str>` clone and the two `String`s that used to discover the same thing.
        // A receiver-body rejection cannot reject other classes at the same site.

        //
        // A site with no plan (undescribed, or the kill switch) keeps the old behaviour exactly.
        //
        // CROSSING2: THE RECEIVER'S `Value` WAS DERIVED TWICE ON EVERY CROSSING THAT GOT HERE
        // (kill switch `RAVA_NO_FAST_CROSSING2=1`). The marshal loop forty lines above decoded
        // operand cell 0 with `unbox_ref_kind` and pushed it into this frame's buffer -- that IS
        // the receiver, because `kind != 0` here and a non-static crossing pushes the receiver
        // first, at index 0. `record_trivial_getter` then decoded the SAME cell a second time,
        // because its signature carried the raw `i64` and not the value its caller already held:
        // the round's "a callee re-deriving what the caller already had" shape.
        //
        // The two derivations answer the recogniser's one question identically, and that is the
        // whole proof. `unbox_ref(vm, c, "")` IS `unbox_ref_kind(vm, c, false)`; the marshal used
        // `unbox_ref_kind(vm, c, P_RECV_ARRAY)`. The two differ in exactly ONE arm -- a `REF_TAG`
        // cell decodes to `Array(h)` under the array bit and `Ref(h)` without it -- and the
        // recogniser asks only `let Value::Obj(p) = .. else { return true }`, which both
        // spellings answer the same way. Every other arm (null, `BOX_TAG`, `ARRAY_TAG`, the bare
        // arena pointer, the defensive int) never reads the bit at all.
        //
        // AND THE ORDER FLIPPED, WHICH IS WHERE THE WORK ACTUALLY GOES. The receiver test rejects
        // the whole HANDLE-receiver population -- and that population is precisely the one that
        // is never latched (`record_trivial_getter` answers "ask again", so
        // `trivial_getter::is_empty` stays true and the site is re-examined on EVERY crossing,
        // forever). It used to be reached only after a `member_ref` (four constant-pool `get`s)
        // and two descriptor compares. Now it is one match on a `Value` already in a register.
        // See `record_trivial_getter` for why moving it cannot change an answer.
        //
        // COLD ARM: `None` -- the kill switch, or a buffer with no cell 0 -- takes the historic
        // body, in the historic order, re-deriving the receiver from `raw[0]` exactly as before.
        #[cfg(feature = "next-gen-gc")]
        if kind != 0
            && raw.len() == 1
            && seam_plan::getter_candidate(plan)
            && s3_bit(g3, S3_GETTER, crate::jit::trivial_getter::enabled)
            && crate::jit::trivial_getter::is_empty(caller_ptr as usize, cp)
        {
            self.record_trivial_getter(
                caller_ptr,
                cp,
                if g3 & S3_CROSS2 != 0 { frame.buf.first() } else { None },
                raw[0],
            );
        }
        let dispatch = match kind {
            0 => opcodes::invokestatic(self, &mut frame, cp),
            2 => opcodes::invokespecial(self, &mut frame, cp),
            _ => opcodes::invokevirtual(self, &mut frame, cp), // 1=virtual, 3=interface
        };
        // NOTE: `&mut frame` above coerces through `SeamFrame`'s `DerefMut`, so `invoke*` sees
        // exactly the `&mut Frame` it always saw and nothing about the dispatch changed.
        if !rootless {
            frames::pop();
        }
        let _reg2 = if slow {
            crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::Seam)
        } else {
            None
        };
        let _epi = slow.then(|| crate::diag::seam_epilogue().start());
        dispatch?;
        // invoke* pushed the return value (if any) onto the synthetic stack. Pack
        // per the return kind: reference result via `box_ref` (the compiled caller
        // holds it as a JT::Ref cell), primitive result flat via `pack_local`.
        let ret = frame.pop_opt();
        self.give_vec(std::mem::take(&mut frame.buf));
        let out = match ret {
            Some(v) => Ok(if ref_ret {
                jit::box_ref(self, &v)
            } else {
                jit::pack_local(&v)
            }),
            None => Ok(0),
        };
        // 24.3 shadow-flat: record decode via the synthetic frame — compare before returning.
        #[cfg(feature = "next-gen-gc")]
        if _recordast_guard.is_some() {
            if let Ok(bits) = &out {
                crate::shadow::flat_at_return(self, *bits);
            }
        }
        crate::vm::aot_route_diag::general_vm_if_unset();
        out
    }
}

#[cfg(feature = "next-gen-gc")]
impl Vm {
    /// Describe a site as a trivial arena getter, or say why not.
    ///
    /// A false result describes this resolution attempt only. Callers must not persist it
    /// as a refusal for every receiver class at the site. Positive entries retain their
    /// representation and receiver-class guards.
    ///
    /// ⭐**NOTHING IS ALLOCATED TO ASK A QUESTION.** `resolve_method` and `MethodInfo::method`
    /// both take `&str`, and the name and descriptor already ARE `&str` borrowed from the
    /// caller's pool (which the loader keeps alive) — the two `String`s this used to build per
    /// crossing existed to answer a question neither function asked.
    ///
    /// ⭐**ORDER (CROSSING2): THE RECEIVER FIRST, BECAUSE THE CALLER ALREADY DECODED IT.** The
    /// note this replaces said "the pool before the heap", and it was right while the receiver
    /// cost a heap decode of its own: the descriptor test rejected the widest family for four
    /// constant-pool `get`s, so it ran first. It is no longer true that the receiver costs
    /// anything. `Vm::jit_call_body`'s marshal loop decodes operand cell 0 into the synthetic
    /// frame's buffer on the very same crossing, and that value is now passed in — so the test
    /// that rejects EVERY handle receiver (the population `trivial_getter` never latches, and
    /// therefore re-examines on every crossing forever) is one match on a `Value` in a register,
    /// and it belongs in front of the pool lookup rather than behind it. The proof that the swap
    /// cannot change an answer is written at the fast arm below; the `None` arm keeps the old
    /// order verbatim for the kill switch.
    fn record_trivial_getter(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        recv: Option<&Value>,
        recv_raw: i64,
    ) -> bool {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let Some(recv) = recv else {
            // COLD ARM: `RAVA_NO_FAST_CROSSING2=1`, or a crossing whose operand buffer has no
            // cell 0. Byte for byte the historic body -- the pool first, the descriptor second,
            // and the receiver decoded HERE a second time from the raw cell.
            let Some(m) = caller.file.pool.member_ref(cp) else { return false };
            if !m.descriptor.starts_with("()") || m.descriptor.ends_with(")V") {
                return false;
            }
            let recv = jit::unbox_ref(self, recv_raw, "");
            let Value::Obj(p) = recv else { return true };
            return self.describe_trivial_getter(caller_ptr, cp, p, m.name, m.descriptor);
        };
        // FAST ARM: THE RECEIVER IS ALREADY DECODED, AND IT IS TESTED FIRST.
        //
        // Recognition publishes proven positive entries only. A negative result is
        // not persisted across receiver classes.
        //
        // A handle receiver has no fixed field offset, so the recogniser cannot serve it -- but
        // the same SITE may see an arena receiver on its next crossing, so `true` ("examine me
        // again") is the correct answer, exactly as it was when this test sat lower down.
        let Value::Obj(p) = recv else {
            // THE PREFILTER IS CHECKED AGAINST THE DERIVATION IT REPLACES. In a debug build,
            // decode the raw cell the historic body would have decoded and require the two to
            // agree on the ONE question asked of them. Debug-only, so production is unchanged;
            // `unbox_ref` is a pure function of the cell and the box table.
            debug_assert!(
                !matches!(jit::unbox_ref(self, recv_raw, ""), Value::Obj(_)),
                "threaded receiver is not an arena object but the raw cell decodes to one"
            );
            return true;
        };
        let p = *p;
        debug_assert!(
            matches!(jit::unbox_ref(self, recv_raw, ""), Value::Obj(q) if q.addr() == p.addr()),
            "threaded receiver disagrees with the raw cell record_trivial_getter used to decode"
        );
        let Some(m) = caller.file.pool.member_ref(cp) else { return false };
        // Zero arguments and a value back — the shape the recogniser can serve. Immutable, so a
        // site that fails here fails for the life of the process.
        if !m.descriptor.starts_with("()") || m.descriptor.ends_with(")V") {
            return false;
        }
        self.describe_trivial_getter(caller_ptr, cp, p, m.name, m.descriptor)
    }

    /// The half of [`Vm::record_trivial_getter`] that runs once the receiver is known to be an
    /// arena object and the site's descriptor has passed the shape test. Shared by both orders
    /// above so the fast arm and the kill switch's arm cannot drift; moved verbatim, including
    /// every refusal's sense.
    fn describe_trivial_getter(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        p: crate::arena::ArenaPtr,
        name: &str,
        desc: &str,
    ) -> bool {
        let cid = crate::nextgen::arena_class_id(p);
        let Some(cls) = crate::nextgen::class_name(cid) else { return true };
        let Ok(Some(owner)) = self.resolve_method(&cls, name, desc) else { return false };
        let Some(mi) = owner.file.method(name, desc) else { return false };
        let Some(code) = mi.code.as_ref() else { return false };
        // A literal answer is the second shape, and the one DataFixerUpper's templates use.
        if let Some(v) = crate::jit::trivial_getter::trivial_const(&code.bytecode) {
            if m_is_int_return(desc) {
                crate::jit::trivial_getter::record_const(caller_ptr as usize, cp, cid, v);
                return true;
            }
            // A wide constant return the recogniser must not describe — a property of the
            // callee's own descriptor, so it will not become describable later.
            return false;
        }
        let Some(fidx) = crate::jit::trivial_getter::trivial_getfield(&code.bytecode) else {
            return false;
        };
        let Some(fr) = owner.file.pool.member_ref(fidx) else { return false };
        // The field id is the interner's COMPOSITE `Owner#name` for a declared field and the
        // bare name for a `$` synthetic — `resolve_field_id` is the one function that knows
        // which, and getting it wrong is how the hand-written version of this arm spent a day
        // resolving `None`.
        let fid = self.resolve_field_id(fr.owner, fr.name);
        // The layout of a loaded arena class is fixed, so a field this class does not have it
        // will never have — the callee reads an inherited slot this receiver's layout omits.
        let Some(slot) = crate::nextgen::slot_of(cid, fid) else { return false };
        crate::jit::trivial_getter::record(caller_ptr as usize, cp, cid, slot);
        true
    }
}

/// `RAVA_NO_FAST_SEAM2=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores the crossing's
/// previous shape: every instrument gate read separately and in place, the iterator shortcut
/// entered before its zero-argument precondition is tested, and the trivial-getter recogniser
/// re-derived on every crossing at every site it can never describe.
///
/// Read once — a `var_os` per crossing is precisely the defect these removals are about.
#[inline]
fn fast_seam2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_SEAM2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// **THE SYNTHETIC SEAM FRAME, PLUS THE ONE THING THAT MUST HAPPEN WHEN IT DIES ON ANY PATH.**
///
/// It exists to make the BORROWED class safe. A frame whose `class` is a bitwise copy of an
/// `Arc` the class loader owns must never run `Arc`'s destructor: that would decrement a strong
/// count this frame never incremented, and the second such crossing would free a `LoadedClass`
/// that is still linked into every compiled method's constant-pool references. The dispatch below
/// can return `Err` (a guest throw crosses the seam with `dispatch?`) and `opcodes::invoke*` can
/// panic, so "forget the frame at the bottom of the function" is not enough — an unwind would
/// skip it. `ManuallyDrop` plus a decision in `Drop` makes the return path and the unwind path
/// take the same branch.
///
/// Same shape and same argument as `vmframe::FrameGuard`, which does this for the INTERPRETED
/// call; kept local because the two frames are built differently (this one is synthetic: null
/// `code`, no locals, and a buffer already filled by the marshalling loop, so
/// `FrameGuard::borrowed`'s `Frame::build` is the wrong constructor).
///
/// Cost when the borrowed form is off: one `bool` and a predictable branch in a destructor the
/// crossing was already running.
struct SeamFrame {
    f: std::mem::ManuallyDrop<Frame>,
    /// `true` = `f.class` is a real owning `Arc` and the frame drops normally.
    owns_class: bool,
}

impl SeamFrame {
    #[inline]
    fn new(f: Frame, owns_class: bool) -> Self {
        Self { f: std::mem::ManuallyDrop::new(f), owns_class }
    }

    /// The address the collector scans. `ManuallyDrop` is `repr(transparent)`, so this is the
    /// `Frame`'s own address — which is what `frames::all()` casts back to a `*const Frame`.
    #[inline]
    fn frame_addr(&self) -> usize {
        std::ptr::addr_of!(*self.f) as usize
    }
}

impl std::ops::Deref for SeamFrame {
    type Target = Frame;
    #[inline(always)]
    fn deref(&self) -> &Frame {
        &self.f
    }
}
impl std::ops::DerefMut for SeamFrame {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Frame {
        &mut self.f
    }
}

impl Drop for SeamFrame {
    #[inline]
    fn drop(&mut self) {
        if self.owns_class {
            // The historic shape, byte for byte: `Frame`'s own destructor decides the buffer's
            // fate (`RAVA_NO_FAST_POOL2`) and the `class` field drop releases the count this
            // frame took. Reached under the kill switch, and on every crossing before this round.
            //
            // SAFETY: the frame is live and owns its class count; this is the drop that used to
            // happen at the end of the enclosing scope.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.f) };
            return;
        }
        // Borrowed class: `Frame`'s destructor must not run at all, because the field drop behind
        // it would release a count this frame never took. That leaves the buffer, which the
        // destructor would otherwise have handed back — so it is handed back HERE, exactly as
        // `vmframe::FrameGuard` does and for the same reason: a guard's hand-back cannot become
        // conditional on a switch whose only hook is the destructor it has just replaced.
        //
        // On the success path the crossing has already taken the buffer with `mem::take`, so this
        // is a capacity load and a not-taken branch; the give here is what covers the `dispatch?`
        // early return and the unwind.
        let buf = std::mem::take(&mut self.f.buf);
        crate::frames::pool_give(buf);
    }
}

/// `RAVA_NO_FAST_SEAM3=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores this round's
/// starting shape exactly: the four behaviour gates read one at a time through their own
/// `OnceLock`s at their own sites, the lane's `warm_recording()` global read unconditionally,
/// the iterator shortcut entered without consulting the site's plan, and the synthetic frame's
/// class reconstructed through the refcount pair.
///
/// ── THE WORD ─────────────────────────────────────────────────────────────────────────────
///
/// `seam_slow_gates` collapsed the eight INSTRUMENT gates into one cached disjunction, and that
/// is as far as a disjunction goes: the four gates left steer BEHAVIOUR, so each needs its own
/// answer and an OR cannot give four answers. They are therefore a per-thread WORD, the shape
/// `opcodes::gates::op_gates`, `vmframe::frame_gates` and `natives::entry::entry_gates` already
/// use — a `Cell<u8>` with a `const` initialiser and no destructor, the one combination `std`
/// lowers to a bare `#[thread_local]` slot (on windows-msvc: the `_tls_index` load, the
/// `gs:[0x58]` TEB read and a `movzx`). No `LocalKey::with` call, no lazy-init state word.
///
/// **The cost removed is not the `mov`, it is the BARRIER.** `*OnceLock::get_or_init(..)` reaches
/// its value through an acquire load; on x86 that is a plain `mov`, but as an ATOMIC it is an
/// optimisation barrier — LLVM may not hoist it out of the caller's tree, may not fold two reads
/// of the same gate into one, and must assume memory it can otherwise track is clobbered across
/// it. A plain per-thread byte has none of those properties, so the four branches can be folded
/// and scheduled with everything around them.
const S3_RESOLVED: u8 = 1 << 0;
/// The kill switch is engaged: distrust every other bit, and let each site reach its own
/// `OnceLock` in its own order, as the historic code did.
const S3_LIVE: u8 = 1 << 1;
/// `seam_plan::enabled` (`RAVA_NO_FAST_SEAM`).
const S3_PLAN: u8 = 1 << 2;
/// `jit::trivial_getter::enabled` (`RAVA_NO_TRIVIAL_GETTER`).
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const S3_GETTER: u8 = 1 << 3;
/// `gilfree_cf::enabled` (`RAVA_GILFREE_CF`) — the lane's own process gate, which DOMINATES its
/// dynamic `warm_recording()` flag. See the call site for the proof.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const S3_LANE: u8 = 1 << 4;
/// The synthetic frame borrows the caller's class `Arc` instead of taking a count of its own.
const S3_BORROW: u8 = 1 << 5;
/// **CROSSING2 (`RAVA_NO_FAST_CROSSING2=1`): the trivial-getter recogniser is handed the receiver
/// the marshal already decoded, instead of re-decoding the raw operand cell itself.**
///
/// Resolved OUTSIDE the [`S3_LIVE`] early return below, deliberately: this is a different round
/// with a different switch, and `RAVA_NO_FAST_SEAM3` must not silently disable it. Its own switch
/// is the only thing that does.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const S3_CROSS2: u8 = 1 << 6;

thread_local! {
    /// This thread's resolved seam gates. `Cell<u8>`, `const`-initialised, no destructor —
    /// deliberately the same shape as `vmframe::FRAME_GATES` and `opcodes::gates::OP_GATES`.
    static SEAM3_GATES: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One plain per-thread byte. Never 0 on return.
#[inline(always)]
fn seam3_gates() -> u8 {
    let g = SEAM3_GATES.get();
    if g != 0 { g } else { seam3_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the ONLY
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cold]
#[inline(never)]
fn seam3_gates_cold() -> u8 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_SEAM3").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = S3_RESOLVED;
    // CROSSING2's bit is resolved BEFORE the `S3_LIVE` early return: it is a later round with its
    // own kill switch, and `RAVA_NO_FAST_SEAM3` restoring THIS round's shape must not also revert
    // one that is independent of it. Its own switch is what reverts it.
    static CROSS2_LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*CROSS2_LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CROSSING2").is_some() || crate::flags::emit_more_disabled()
    }) {
        g |= S3_CROSS2;
    }
    if live {
        // Resolve NOTHING else: with the switch engaged each site must reach its own `OnceLock`
        // from its own position, in its own order, exactly as before.
        g |= S3_LIVE;
        SEAM3_GATES.set(g);
        return g;
    }
    if seam_plan::enabled() {
        g |= S3_PLAN;
    }
    #[cfg(feature = "next-gen-gc")]
    {
        if crate::jit::trivial_getter::enabled() {
            g |= S3_GETTER;
        }
        if crate::gilfree_cf::enabled() {
            g |= S3_LANE;
        }
    }
    g |= S3_BORROW;
    SEAM3_GATES.set(g);
    g
}

/// Read one bit of an ALREADY-LOADED word. `live` is the historic `OnceLock` reader, called only
/// under the kill switch — the `op_gate_of` discipline, so a caller that needs several gates
/// takes one load rather than several acquire loads.
#[inline(always)]
fn s3_bit(g: u8, bit: u8, live: fn() -> bool) -> bool {
    if g & S3_LIVE != 0 { live() } else { g & bit != 0 }
}

/// **IS ANY INSTRUMENT INSIDE THE CROSSING WIRED AT ALL?**
///
/// [`Vm::jit_call_body`] carries six separate instruments — two region guards, three meters, the
/// per-call diagnostic disjunction, the fallback tally, the pilot census and the callback census
/// — and each opened with its own gate read. Every one of those gates is derived from an
/// environment variable consulted once, so their disjunction is as constant as they are and may
/// be cached the same way. Reading it costs ONE acquire load instead of eight, and a false answer
/// is a proof that no instrument in this function can fire for the rest of the process.
///
/// The cost of an atomic gate is not the `mov`: an acquire load is an optimisation barrier that
/// LLVM may not hoist out of the caller's loop nor fold branches across, which is why collapsing
/// eight of them into one is worth more than the eight loads it removes.
///
/// ⚠**LATE ARMING IS THE ONE HAZARD, AND IT IS COVERED BY CONSTRUCTION.** `regions_armed()` reads
/// two cells that are not themselves `OnceLock`s: `RAVA_ALLOC_SIZES` is stored from `main` before
/// any guest code runs, and the region-time flag is set by `pc_profile::start_sampler_if_on`,
/// which the interpreter calls lazily — but only ever `if pc_profile::enabled()`. Including that
/// process-constant in the disjunction makes the late store unreachable while this gate is false.
///
/// TABLE SIZE: one `OnceLock<bool>`. It replaces reads rather than adding a lookup, so there is
/// no probe here to lose against — the failure mode that cost this tree 6.8% on the field
/// interner cannot occur.
#[inline]
fn seam_slow_gates() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // Kill switch: take the original per-instrument sequence forever, so one binary can A/B
        // both arms.
        if !fast_seam2_enabled() {
            return true;
        }
        // Constructing the three meters here REGISTERS them exactly as the first crossing used
        // to, so a `RAVA_DIAG=price` run still lists all three rows whichever way this goes.
        let metered = crate::diag::seam_prologue().armed()
            || crate::diag::seam_prologue_full().armed()
            || crate::diag::seam_epilogue().armed();
        if metered {
            return true;
        }
        if crate::alloc_sites::regions_armed() || crate::pc_profile::enabled() {
            return true;
        }
        if crate::Vm::any_call_diag() {
            return true;
        }
        if crate::fallback_tally::enabled() || crate::pilot_census::enabled() {
            return true;
        }
        #[cfg(feature = "next-gen-gc")]
        if super::choke_residue::enabled() {
            return true;
        }
        #[cfg(feature = "next-gen-gc")]
        if crate::jit_cb_census::enabled() {
            return true;
        }
        false
    })
}

/// `()I`-shaped, i.e. an `ireturn` body's answer is what the caller expects. `trivial_const`
/// only recognises `ireturn`, so a `()Z`/`()S`/`()B`/`()C` site is equally served — but a
/// `()J`/`()D` one is not, and must not be described from an int constant.
#[cfg(feature = "next-gen-gc")]
fn m_is_int_return(desc: &str) -> bool {
    matches!(desc.as_bytes().get(2), Some(b'I' | b'Z' | b'S' | b'B' | b'C')) && desc.len() == 3
}

/// **THE SHAPE OF A CROSSING, DERIVED ONCE PER SITE.**
///
/// A compiled→host crossing that the PIC could not bind builds a synthetic frame: an operand
/// buffer filled from the raw `i64` cells the machine code passed, a root registration, the
/// dispatch, then a decode of the return. Every one of those steps used to re-read the callee's
/// constant-pool entry and re-parse its descriptor, and none of those answers can change: the
/// constant pool is immutable once the class is loaded, so the number of slots, the kind of each
/// slot, the `[`-vs-`L` bit each reference slot needs and the return kind are CONSTANTS OF THE
/// SITE. `seam_prologue_full` counts 8,375,708 general-path crossings a boot; this table turns
/// eight million derivations into eight million table hits.
///
/// ## The row
///
/// Direct-mapped, key beside value so a hit reads ONE cache line rather than two. **Total size:
/// 4096 sites x 16 bytes = 64 KB of ADDRESS SPACE — but the resident set is four sites per
/// 64-byte line, so a boot's few hundred hot general-path sites occupy a few KB.** That
/// distinction is the one this tree paid 6.8% to learn: the field interner's L1 regression came
/// from a table whose WHOLE footprint was probed on every lookup, not from a table that was
/// merely large. Here a site touches exactly one line, always the same one.
///
/// First writer wins, exactly as `jit::trivial_getter` does. Two sites that hash to one row is a
/// collision, and the loser simply keeps taking the old path forever — a refusal, never a wrong
/// answer, because the row carries the site's identity and is only believed when it matches.
/// ⚠**The key is not optional**: without it a collision would decode one site's arguments with
/// another site's descriptor, which is silent corruption rather than a crash.
///
/// ## What it deliberately does not do
///
/// * **No names.** Every bit is read out of the descriptor and the invoke kind — permitted
///   evidence. The owner string is consulted for exactly one bit (`[`-prefixed array receiver).
/// * **No wide descriptors.** Above [`MAX_ARGS`] parameters the site is left undescribed and
///   takes the byte-walking path it always took. A `(...)` of nine arguments is rare enough that
///   widening the encoding would cost every other site a second cache line to serve it.
/// * **No overwrite, no eviction, no invalidation.** A `LoadedClass` lives for the process, so a
///   `caller_ptr` is never recycled while its sites are live.
mod seam_plan {
    use std::sync::atomic::{AtomicU64, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};

    /// Rows, power of two. 4096 x (key + plan) x 8 bytes = 64 KB of address space; see the
    /// module note on why the RESIDENT set is a few KB.
    const SITES: usize = 4096;
    /// The most descriptor parameters a plan can describe: 8 x 3 bits = 24 bits of the word.
    const MAX_ARGS: usize = 8;

    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY: AtomicU64 = AtomicU64::new(0);
    /// Interleaved: row `i` is key at `2i`, plan at `2i+1`. Adjacent so one line serves both.
    static ROWS: [AtomicU64; SITES * 2] = [EMPTY; SITES * 2];

    /// Set on every described plan, so a plan word is never 0 and 0 always means "unknown".
    const P_PRESENT: u64 = 1 << 63;
    /// Bits 0..=3: parameter count (0..=[`MAX_ARGS`]).
    const P_NARGS_MASK: u64 = 0xF;
    /// The return descriptor is `L...;` or `[...` — the caller packs the result with `box_ref`.
    pub(super) const P_REF_RET: u64 = 1 << 4;
    /// The owner is `[`-prefixed, i.e. the receiver is an array (`[I.clone()`); the one bit
    /// `unbox_ref` used to slice a `&str` to read.
    pub(super) const P_RECV_ARRAY: u64 = 1 << 5;
    /// **Nothing that will sit in the synthetic operand buffer can be a collector root.** Set
    /// only for a static call whose every parameter AND whose return are primitive: no receiver,
    /// no reference argument, and the value `invoke*` pushes back afterwards is primitive too.
    /// Anything less and the frame is registered as it always was.
    pub(super) const P_ROOTLESS: u64 = 1 << 6;
    /// Mirrors `kind == 0`. Only a defensive cross-check — the invoke kind is already part of the
    /// row index — but a plan whose static-ness disagrees with the call is refused rather than
    /// believed, and a refusal costs one predictable compare.
    const P_STATIC: u64 = 1 << 7;
    /// Bits 8..=31: eight 3-bit argument codes, parameter `k` at bit `8 + 3k`.
    const P_ARGS_SHIFT: u32 = 8;
    /// **This site's constant-pool entry can NEVER name a trivial getter.** Static, or it takes
    /// parameters, or it returns `void` — none of which `Vm::record_trivial_getter` can describe,
    /// and all of which are constants of an immutable pool entry that the crossing used to
    /// rediscover by re-reading the pool every time.
    ///
    /// ⚠**THE SENSE OF THIS BIT IS LOAD-BEARING.** Written as a positive "could be a getter" it
    /// would be absent from every plan built while `RAVA_NO_FAST_SEAM2` is set, and the kill
    /// switch would then SUPPRESS the recogniser instead of restoring it. As a refusal, an unset
    /// bit means "nothing known", which is exactly the old behaviour.
    pub(super) const P_NO_GETTER: u64 = 1 << 32;
    /// **THIS SITE'S CONSTANT-POOL ENTRY CAN NEVER NAME ONE OF THE ITERATOR SHORTCUT'S SHAPES.**
    ///
    /// The shortcut serves exactly `hasNext()Z`, `next()Ljava/lang/Object;`,
    /// `getKey()Ljava/lang/Object;` and `getValue()Ljava/lang/Object;`. Every one is an instance
    /// method that takes no parameters and returns either `Z` or `Ljava/lang/Object;` — so the
    /// descriptor and the invoke kind, which is all this module is permitted to read, refuse the
    /// whole rest of the zero-argument family for free: `size()I`, `toString()Ljava/lang/String;`,
    /// `iterator()Ljava/util/Iterator;`, every `()V`, every `()D`, every getter returning a
    /// concrete type.
    ///
    /// ⚠**NO NAMES ARE READ HERE**, and that is deliberate: the shortcut itself matches on names
    /// (it must — it serves a JDK contract), but the REFUSAL is derived from shape alone, so this
    /// bit adapts the engine to nothing. A survivor of the test is handed to the shortcut, which
    /// then applies its own unchanged name and owner tests, verbatim, in its own order.
    ///
    /// ⚠**THE SENSE IS A REFUSAL, for the same reason [`P_NO_GETTER`] is.** Written positively it
    /// would be absent from every plan built under `RAVA_NO_FAST_SEAM3` and the kill switch would
    /// SUPPRESS the shortcut instead of restoring it. Unset means "nothing known", which is the
    /// old behaviour.
    #[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
    pub(super) const P_NO_ITER: u64 = 1 << 34;

    /// Argument codes. 0..=3 index [`PRIM_KIND`] and go to `unpack_arg`; >= [`K_REF`] go to
    /// `unbox_ref_kind` with the array bit that distinguishes 4 from 5.
    pub(super) const K_REF: u64 = 4;
    pub(super) const K_REF_ARRAY: u64 = 5;
    /// The exact kind byte `jit_arg_kind` returned, so `unpack_arg` sees what it always saw.
    /// `I` stands for the whole `I/Z/B/C/S` family because `unpack_arg` maps all of them to
    /// `Value::Int` through its default arm — substituting `b'I'` is bit-exact, not a widening.
    pub(super) const PRIM_KIND: [u8; 4] = [b'I', b'J', b'F', b'D'];

    /// `RAVA_NO_FAST_SEAM=1` (or the bisect-everything `RAVA_NO_EMIT_MORE=1`) leaves every plan
    /// at 0, and the crossing re-derives its shape exactly as it did before this table existed.
    #[inline]
    pub(super) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_NO_FAST_SEAM").is_none() && !crate::flags::emit_more_disabled()
        })
    }

    /// The row this site owns. `kind == 0` participates so a constant-pool entry reached by both
    /// an `invokestatic` and a non-static opcode gets a row each instead of one of them being
    /// permanently refused.
    #[inline]
    fn row(caller: usize, cp: u16, kind: u8) -> usize {
        let k = (caller as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (((cp as u64) << 1 | (kind == 0) as u64).wrapping_mul(0x517C_C1B7));
        (((k >> 17) as usize) & (SITES - 1)) * 2
    }

    /// The identity stored beside the plan and compared before the plan is believed.
    #[inline]
    fn key(caller: usize, cp: u16) -> u64 {
        (caller as u64) ^ ((cp as u64) << 48)
    }

    /// This site's plan, or 0. One line, one compare.
    #[inline]
    pub(super) fn get(caller: usize, cp: u16, kind: u8) -> u64 {
        let i = row(caller, cp, kind);
        // Acquire pairs with the Release key store in `record`: seeing the key means the plan
        // beside it is already published. Free on x86, correct everywhere else.
        if ROWS[i].load(Acquire) != key(caller, cp) {
            return 0;
        }
        let p = ROWS[i + 1].load(Relaxed);
        // Defensive: a plan built for the other static-ness is not this call's plan.
        if (p & P_STATIC != 0) != (kind == 0) {
            return 0;
        }
        p
    }

    /// Publish a plan. The PLAN cell is claimed first with a CAS so exactly one site owns the
    /// row, and only that winner writes the key — the ordering `trivial_getter` learned to use
    /// after a version without it read one site's value under another site's identity.
    pub(super) fn record(caller: usize, cp: u16, kind: u8, plan: u64) {
        let i = row(caller, cp, kind);
        if ROWS[i + 1].compare_exchange(0, plan, Relaxed, Relaxed).is_ok() {
            ROWS[i].store(key(caller, cp), Release);
        }
    }

    /// Should the trivial-getter recogniser look at this site at all?
    ///
    /// Only the immutable descriptor can reject the entire call site.
    #[inline]
    #[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
    pub(super) fn getter_candidate(plan: u64) -> bool {
        plan & P_NO_GETTER == 0
    }

    /// Should the iterator/entry shortcut look at this site at all? See [`P_NO_ITER`]: a
    /// refusal, so a site with no plan answers `true` and takes the route it always took.
    #[inline]
    #[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
    pub(super) fn iter_candidate(plan: u64) -> bool {
        plan & P_NO_ITER == 0
    }

    #[inline]
    pub(super) fn nargs(plan: u64) -> usize {
        (plan & P_NARGS_MASK) as usize
    }

    #[inline]
    pub(super) fn arg_code(plan: u64, k: usize) -> u64 {
        (plan >> (P_ARGS_SHIFT + 3 * k as u32)) & 0b111
    }

    /// Read the site's shape off its descriptor, or refuse.
    ///
    /// Refusal is normal and cheap: more than [`MAX_ARGS`] parameters, or a descriptor whose
    /// parameter list does not terminate cleanly. An undescribed site keeps the byte-walking
    /// path forever, which is exactly the code that ran before this table existed.
    pub(super) fn derive(owner: &str, desc: &str, kind: u8) -> Option<u64> {
        let is_static = kind == 0;
        let inner = crate::args_of(desc);
        let bytes = inner.as_bytes();
        let mut plan = P_PRESENT;
        let mut n = 0usize;
        let mut bi = 0usize;
        // `any_ref` starts true for a non-static call: the receiver is always a reference slot.
        let mut any_ref = !is_static;
        while bi < bytes.len() {
            if n == MAX_ARGS {
                return None;
            }
            let start = bi;
            let k = crate::jit_arg_kind(bytes, &mut bi);
            // A descriptor whose type never terminates (`(Lfoo` with no `;`) walks `bi` past the
            // end. The old loop tolerated it by clamping the slice; describing it would bake the
            // damage into a permanent row, so refuse and let the byte walk keep clamping.
            if bi > bytes.len() {
                return None;
            }
            let code = match k {
                b'L' => {
                    any_ref = true;
                    if bytes[start] == b'[' {
                        K_REF_ARRAY
                    } else {
                        K_REF
                    }
                }
                b'J' => 1,
                b'F' => 2,
                b'D' => 3,
                _ => 0,
            };
            plan |= code << (P_ARGS_SHIFT + 3 * n as u32);
            n += 1;
        }
        plan |= n as u64;
        if is_static {
            plan |= P_STATIC;
        } else if owner.as_bytes().first() == Some(&b'[') {
            plan |= P_RECV_ARRAY;
        }
        let rstart = crate::byte_rfind(desc, b')').map(|p| p + 1).unwrap_or(desc.len());
        let rk = desc.as_bytes().get(rstart).copied();
        let ref_ret = matches!(rk, Some(b'L') | Some(b'['));
        if ref_ret {
            plan |= P_REF_RET;
        }
        if is_static && !any_ref && !ref_ret {
            plan |= P_ROOTLESS;
        }
        // The trivial-getter shape, decided here so the crossing never re-reads the pool to
        // decide it. The recogniser can only ever describe an instance method that takes nothing
        // and returns something, so anything else is refused permanently and for free. `rk ==
        // None` means the descriptor had no `)` at all — not a shape to describe either.
        if super::fast_seam2_enabled()
            && (is_static || n != 0 || rk.is_none() || rk == Some(b'V'))
        {
            plan |= P_NO_GETTER;
        }
        // The iterator/entry shortcut's shape, decided here so the crossing never re-reads the
        // pool to decide it. See [`P_NO_ITER`]: instance, zero parameters, and a return of
        // exactly `Z` or `Ljava/lang/Object;`. `desc` is compared whole rather than by its return
        // byte because both survivors are three- and nineteen-character constants, so this is two
        // length compares and at most one `memcmp` against a literal.
        if super::seam3_gates() & super::S3_LIVE == 0
            && (is_static
                || n != 0
                || !(desc == "()Z" || desc == "()Ljava/lang/Object;"))
        {
            plan |= P_NO_ITER;
        }
        Some(plan)
    }
}
