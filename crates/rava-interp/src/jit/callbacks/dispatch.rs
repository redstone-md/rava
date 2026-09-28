//! Dispatch-miss bridges: what compiled code falls back to when its inline cache and its direct-call cache both refuse the callee.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


pub(crate) extern "C" fn rava_dispatch_miss(
    cache_ptr: i64,
    recv: i64,
    caller_ptr: i64,
    cp: i64,
    kind: i64,
    buf: i64,
    n: i64,
    dispatch_site: i64,
) -> i64 {
    cb(0); // counts as an invoke callback (it IS the dispatch fallback)
    // Which receiver SHAPE is producing the ~290M misses — the question the table's own
    // 0.54%-reached counter forced. Inert unless picprobe is armed.
    #[cfg(feature = "next-gen-gc")]
    dispatch_table::note_miss_shape(recv);
    // LANE PROBE: remember the SITE this worker is dispatching. A per-worker count
    // says a worker is busy; the site says WHICH call it is grinding, which is the
    // difference between "the lane is slow" and a named cause.
    crate::lane_probe::note_site(caller_ptr as usize, cp as u16);
    // PILOT CENSUS (RAVA_PILOT_CENSUS, inert when unset): on a worker under
    // RAVA_M3_WORKER_LOCKED_EXEC the global resolve lock is held for this ENTIRE
    // handler (see the 11.4/11.6 note below), so this is the SECOND serialized
    // span of the lever — the first being the non-direct `jit_call` tail. Both
    // feed one depth-counted accumulator, so a miss nested inside a general
    // `jit_call` is billed once, exactly as the re-entrant lock behaves.
    // The site key is offset by 1 (pointers are aligned) so the two surfaces stay
    // distinguishable in the per-site breakdown.
    let _pilot = crate::pilot_census::general_scope((caller_ptr as usize | 1, cp as u16), || {
        let caller = unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
        caller.file.pool.member_ref(cp as u16).map_or_else(
            || "dmiss:?".to_string(),
            |m| format!("dmiss:{}.{}{}", m.owner, m.name, m.descriptor),
        )
    });
    // 23.2 PIC probe: a machine-code PIC site MISSED (cold class / non-arena recv) and fell here (inert).
    if crate::picprobe::enabled() {
        crate::picprobe::pic_miss();
    }
    if crate::bootdisp::on() {
        crate::bootdisp::miss(); // 14.1: compiled-code IC fallback (context for the dispatch dump)
        // WHOSE method is on the far side? A `java/*` callee has no bytecode to compile, so no
        // amount of dependency-order compilation converts that crossing into a direct call;
        // a guest callee is exactly what such a scheme could convert. This split decides whether
        // that scheme is worth building, and it is the crossing count that matters — the resolve
        // count is memoised and 1500x smaller.
        let caller = unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
        if let Some(m) = caller.file.pool.member_ref(cp as u16) {
            crate::bootdisp::miss_owner(&m.owner);
        }
    }
    // GAP-S09 stage 4 SANCTIONED (read-only pre-lock arms + locked envelope):
    // on a gil-free worker the pre-lock fast paths mutate nothing — the freeze
    // table is an append-only atomic probe, the frozen direct cache is served
    // via `jit_direct_call`'s gilfree read-only arm (no neg-bump/resolve/insert),
    // and `ws_arraylist_get` is a pure read; every mutating continuation
    // (dispatch_miss resolve, jit_pending publication) runs under
    // `resolve_lock_acquire()`, which is taken UNCONDITIONALLY for workers
    // below — the exclusive envelope — and an uncompiled-callee continuation
    // aborts inside `jit_call`'s worker guard before any publication.
    // `[INVCEN]` — name the far side of this crossing. Inert unless RAVA_INVOKE_CENSUS; see
    // `vm::invoke_census`. `rava_invoke` carries only a fraction of the invoke family, and this
    // callback is the largest single column in `[CB]` (`dmiss_res` ~16.3 M a warm reload).
    crate::vm::invoke_census::note(caller_ptr, cp as u16);
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    #[cfg(feature = "next-gen-gc")]
    let _dispatch_miss =
        crate::jit::dispatch_diag::MissGuard::begin(dispatch_site, cache_ptr, recv, vm);
    #[cfg(not(feature = "next-gen-gc"))]
    let _ = dispatch_site;
    // FREEZE-COVERAGE CENSUS (RAVA_FREEZE_CENSUS, default OFF; see pilot_census).
    // On the SERIAL boot, classify this miss by the pre-lock arm that would serve it
    // on a worker, in the worker's own probe order: freeze table (positive fill) →
    // frozen direct cache → otherwise the global lock. Both probes are pure reads,
    // so main's behaviour is unchanged; what it measures is the COVERAGE CEILING of
    // the lock-free arms against the real miss profile, with both caches already
    // warmed by main (i.e. the best a pilot-warmed worker could do).
    #[cfg(feature = "next-gen-gc")]
    if crate::pilot_census::freeze_census_on()
        && crate::pilot_census::in_task()
        && !crate::gil::is_worker()
    {
        let raw_u = recv as u64;
        let ck = if recv != 0 && raw_u >> 40 == 0 {
            Some(unsafe { *(recv as *const u32) } as i64)
        } else if raw_u >> 40 == 1 {
            let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
            vm.heap.object_class_word(handle)
        } else {
            None
        };
        let table = ck.and_then(|ck| freeze_resolve::get(caller_ptr | (cp << 48), ck));
        let arm = if matches!(table, Some(f) if f > 0) {
            crate::pilot_census::Arm::Table
        } else {
            let raw =
                unsafe { std::slice::from_raw_parts(buf as *const i64, n.max(0) as usize) };
            let caller = caller_ptr as *const crate::class_loader::LoadedClass;
            if vm.direct_probe_hit(caller, cp as u16, kind as u8, raw) {
                crate::pilot_census::Arm::Direct
            } else if table.is_some() {
                crate::pilot_census::Arm::Neg
            } else if ck.is_none() {
                crate::pilot_census::Arm::NoCk
            } else {
                crate::pilot_census::Arm::Unknown
            }
        };
        crate::pilot_census::note_arm((caller_ptr as usize | 1, cp as u16), arm, || {
            let caller = unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
            caller.file.pool.member_ref(cp as u16).map_or_else(
                || "?".to_string(),
                |m| format!("{}.{}{}", m.owner, m.name, m.descriptor),
            )
        });
    }
    // 11.4/11.6: on a pool worker, hold the RE-ENTRANT resolution lock for the ENTIRE handler so the
    // &mut Vm resolve + any jit_pending/fault write are serialised across workers, while a nested
    // dispatch-miss re-entered from this miss's own jit_call (same thread) passes through lock-free
    // (11.5 self-deadlock fix). RAII → released only when the count returns to 0 at the top frame.
    // Main thread never locks ⇒ the serial path is byte-identical. Guard formed before any Vm access.
    // P3-S1 FREEZE-RESOLVE fast path: before taking the global lock, probe the append-only
    // (site, class-key) → fptr table. On a compiled hit, execute the target directly — the
    // exact machcode hit-arm call (`fptr&!1`, args buf + JitCtx) — with the operand buffer
    // rooted across it. This is what makes a worker's megamorphic steady-state LOCK-FREE:
    // the 4-way PIC evicts forever on such sites, but the table remembers every class.
    // WORKER-ONLY, and that is measured rather than assumed. Letting MAIN consult this table on a
    // PIC miss looks obvious — the 4-way machine-code PIC evicts forever on a megamorphic site and
    // this table has no way limit — and the polymorphism census seemed to point straight at it:
    // over the ring workload, 1640 compiled dispatch sites are 79% MONOMORPHIC, 14%
    // oligomorphic(2-3), 3% polymorphic(4-15) and 2% MEGAMORPHIC(16+), so a WIDER PIC would buy
    // almost nothing while those 39 megamorphic sites are the only plausible source of 246M PIC
    // misses.
    //
    // It still loses. RingScan(8): 2778ms worker-only vs 3795ms with main consulting it (+37%),
    // bit-exact both ways. Main already has the 16-way per-site DIRECT cache in front of the same
    // gauntlet, and it serves main's access pattern more cheaply than an open-addressing probe;
    // adding the table in front is pure added cost. The unbounded table earns its keep on a worker
    // because a worker may not fill the direct cache, not because the table is faster.
    if crate::gil::is_worker() && freeze_resolve::enabled() {
        // Receiver class key, read-only: arena header u32, or the heap-Ref class word.
        //
        // READ-BARRIER FIRST. The cell can carry a young pointer that has been evacuated since
        // the compiled caller computed it, and dereferencing it then reads the class id of
        // WHATEVER now occupies that address — a valid id, for the wrong class. This key selects
        // which compiled body to call, so a wrong id is not a miss, it is a silent call to
        // another class's method: the result stays plausible and is simply wrong.
        //
        // `jit_direct_resolve` barriers its receiver for exactly this reason and says so ("the
        // arena stale-receiver class of bug"). This path, which is the one a gil-free worker
        // takes, did not — and it is the difference between the lane's kept result matching the
        // JVM and not (bench/RingLane.java).
        let recv = crate::nextgen::forward_if_moved(recv);
        let raw_u = recv as u64;
        let ck = if recv != 0 && raw_u >> 40 == 0 {
            Some(unsafe { *(recv as *const u32) } as i64)
        } else if raw_u >> 40 == 1 {
            let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
            vm.heap.object_class_word(handle)
        } else {
            None
        };
        // Abort visibility: compiled code has NO safepoint polls, and this fast path BYPASSES
        // jit_call's gilfree short-circuit — without this check a freeze-served worker is
        // abort-BLIND and grinds its whole range after a sibling aborted (measured: the
        // rotating "stuck" worker). Return a discarded 0 — the garbage-continuation
        // discipline; the region is tearing down and every worker result is thrown away.
        if crate::parallel::abort_requested() {
            return 0;
        }
        // Step 0a — FREEZE TABLE FIRST: the cheapest hit path there is (one open-addressing
        // probe + a direct `f(buf, ctx)` — the buf is passed AS-IS, no cells copy, no guard
        // scan). Measured: routing everything through jit_direct_call first cost ~8µs/dispatch
        // (FastMap + 16-way scan + slot-laid copy per call) and saturated at ~250 el/s; the
        // table serves the arena-receiver majority at a fraction of that.
        // STABLE site key: (caller_ptr | cp<<48) — cache_ptr is the PER-THREAD effective table
        // address under IC_PARALLEL (different per worker/region), so keying by it made every
        // warmup entry invisible to workers (measured hit=0). caller_ptr's top 16 bits are 0
        // on x64 user space, so the pack is lossless.
        let site_key = caller_ptr | (cp << 48);
        if let Some(ck) = ck {
            if let Some(fill) = freeze_resolve::get(site_key, ck) {
                if fill > 0 {
                    if freeze_resolve::stats_on() {
                        freeze_resolve::WORKER_HIT
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    // BACKFILL the worker's per-thread 16-way PIC (cache_ptr IS the per-thread
                    // slice under IC_PARALLEL — this worker owns it, no races): the fast paths
                    // otherwise STARVE the PIC (the locked fill runs only on real misses, ~20
                    // per region) and every repeat dispatch re-pays the machcode-exit callback.
                    if ic_parallel_enabled() && cache_ptr != 0 {
                        unsafe {
                            let cache = cache_ptr as *mut i64;
                            let ways = IC_WORKER_WAYS;
                            let mut slot = None;
                            for k in 0..ways {
                                if *cache.add(k * 2 + 1) == 0 || *cache.add(k * 2) == ck {
                                    slot = Some(k);
                                    break;
                                }
                            }
                            let k = slot.unwrap_or((ck as usize) & (ways - 1));
                            *cache.add(k * 2) = ck;
                            *cache.add(k * 2 + 1) = fill;
                        }
                    }
                    let fclean = (fill & !1i64) as usize;
                    // FREEZE VERIFY (`RAVA_FREEZE_VERIFY`, default OFF): re-derive, with pure
                    // reads only, the body the LOCKED path would have entered for this exact
                    // (site, receiver class) and compare it against what the table is about to
                    // call. A mismatch is a silent wrong-method dispatch — the failure mode that
                    // yields a plausible but wrong answer instead of a crash — and no existing
                    // counter can see it, because a table HIT is tallied as a success whichever
                    // body it hands over.
                    if freeze_verify_enabled() {
                        freeze_verify(vm, caller_ptr, cp, recv, fclean);
                    }
                    // THIS is where the population is. ~258M of the ~300M PIC misses return
                    // through this arm and the frozen-direct one below, before the resolve is
                    // reached at all ([IC-MISS] ENTRY is only 42.4M). The answer is already known
                    // here — `fill` is a compiled fptr — so the whole cost of these misses is the
                    // machine-code EXIT taken to come and read it. Mirror it into the site's
                    // direct-mapped table so the emitted probe can serve the next one without
                    // leaving compiled code. Note `fill`, not `fclean`: the low deep/leaf bit is
                    // part of what the hit arm decodes.
                    dispatch_table::note_serve(cache_ptr, recv, fill);
                    {
                    let ctx = vm.heap.array_desc_cell_ptr();
                    let nn = n.max(0) as usize;
                    crate::frames::cells_push((buf as *const i64, nn));
                    let f: extern "C" fn(*mut i64, i64) -> i64 =
                        unsafe { std::mem::transmute(fclean) };
                    let bits = f(buf as *mut i64, ctx);
                    crate::frames::cells_pop();
                    return bits;
                    }
                }
            }
        }
        // Step 0b — WORKER DIRECT CACHE, lock-free: the shared 16-way cache serves warmed
        // virtual/interface and lambda identity/fp entries, while a miss may resolve into the
        // worker's TLS-owned cache. The `&self` API proves that neither arm mutates shared Vm
        // state; an unserviceable call returns None and keeps the existing locked continuation.
        {
            let raw_args =
                unsafe { std::slice::from_raw_parts(buf as *const i64, n.max(0) as usize) };
            match vm.jit_direct_call_worker(
                caller_ptr as *const crate::class_loader::LoadedClass,
                cp as u16,
                kind as u8,
                raw_args,
                // THE site whose machine code took this exit. Everything this arm answers is a
                // verdict the compiled caller could have used and never learns; publishing it here
                // is what gives the promoted table something to serve.
                cache_ptr,
            ) {
                Some(Ok(bits)) => {
                    if freeze_resolve::stats_on() {
                        freeze_resolve::WORKER_DIRECT_HIT
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    return bits;
                }
                Some(Err(e)) => {
                    // Guest exception from a direct-served callee: publish under the lock
                    // (shared jit_pending), same discipline as the locked handler.
                    let _g = resolve_lock_acquire();
                    if !vm.jit_pending_is_set() {
                        if let crate::VmError::Throw(x) = e {
                            vm.jit_pending_set(x);
                        }
                    }
                    vm.heap.set_jit_fault();
                    return 0;
                }
                None => {}
            }
        }
        // Step 0.5 — WS-native pre-lock probe: ArrayList.get (the measured 914K residual
        // dispatch-lock population) served as a pure read, no lock.
        if kind == 1 || kind == 3 {
            let raw_args =
                unsafe { std::slice::from_raw_parts(buf as *const i64, n.max(0) as usize) };
            if let Some(bits) = vm.ws_arraylist_get(
                caller_ptr as *const crate::class_loader::LoadedClass,
                cp as u16,
                raw_args,
            ) {
                return bits;
            }
            // Same pre-lock arm, primitive twin: `AtomicLong.get()J` is 19% of the ring's
            // locked callees and a pure read. Returns the raw i64 cell, which IS the `J`
            // return convention (`abi_ret_from_cell`) — deliberately NOT through `box_ref`.
            if let Some(bits) = vm.ws_atomic_long_get(
                caller_ptr as *const crate::class_loader::LoadedClass,
                cp as u16,
                raw_args,
            ) {
                freeze_resolve::WORKER_WSNATIVE_HIT
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return bits;
            }
        }
        if let Some(ck) = ck {
            // THE TABLE'S KEY IS `caller|cp<<48`, NOT `cache_ptr`. Both writers say so in as
            // many words — "STABLE key (caller|cp<<48): cache_ptr is per-thread under
            // IC_PARALLEL and never matches across threads" — and `put` itself decodes the site
            // that way to run its wide-param refusal (`site & ((1<<48)-1)`, `site >> 48`). This
            // read was still passing `cache_ptr`, so no probe could ever match a stored row.
            //
            // The lane probe named it exactly: `stored=6113` with `table_hit=0`. Not a low hit
            // rate — ZERO, next to 2,201,297 misses, each of which then took the resolve lock.
            // ★A count of what a cache HOLDS says nothing about what it SERVES; only the serve
            // counter does, and a zero there next to a non-zero fill is a key mismatch until
            // proven otherwise.
            if let Some(fill) = freeze_resolve::get((caller_ptr as i64) | ((cp as i64) << 48), ck) {
                if fill > 0 {
                    freeze_resolve::WORKER_HIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let fclean = (fill & !1i64) as usize;
                    let ctx = vm.heap.array_desc_cell_ptr();
                    let nn = n.max(0) as usize;
                    // Root the operand cells across the call (a deep callee can trigger the
                    // worker abort machinery / forwarding — same discipline as the resolve path).
                    crate::frames::cells_push((buf as *const i64, nn));
                    let f: extern "C" fn(*mut i64, i64) -> i64 =
                        unsafe { std::mem::transmute(fclean) };
                    let bits = f(buf as *mut i64, ctx);
                    crate::frames::cells_pop();
                    return bits;
                }
                // fill == -1 (NeverCacheable): the call itself needs the general &mut Vm
                // path → fall through to the locked handler (resolve is skipped there by
                // the site's negative entry, but execution still serialises — S2 territory).
                freeze_resolve::WORKER_NEG_HIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        freeze_resolve::WORKER_MISS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let _resolve_guard = if crate::gil::is_worker() {
        crate::gil::worker_miss_inc(); // force-compile thesis arbiter: worker PIC-miss ⇒ global-lock take
        resolve_lock_acquire()
    } else {
        ResolveGuard::None
    };
    // Kill-switch restoring the OLD subtree-wide envelope (hold the resolve lock
    // across the callee's execution too), so the narrowing can be A/B'd on one
    // binary. Only a gil-free worker ever holds this guard, so both arms are
    // byte-identical on the serial path.
    #[cfg(feature = "next-gen-gc")]
    fn wide_envelope() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_WIDE_ENVELOPE").is_some())
    }
    if vm.jit_pending_is_set() {
        return 0;
    }
    // STEADY-STATE devirt probe (read-only, gated): this callback fires precisely when the
    // 4-way PIC allocates a way for a not-yet-cached receiver class. Record the distinct
    // classes seen per compiled site (keyed by its baked cache pointer) to profile real
    // steady-state polymorphism — the compiled-code answer to the warmup SITES caveat.
    if crate::devirt::steady_enabled() {
        let v = unbox_ref(vm, recv, "");
        if let crate::Value::Ref(h) = &v {
            if let Some(cn) = vm.heap.class_of(*h) {
                crate::devirt::record_steady(cache_ptr as usize, (caller_ptr as usize, cp as u16), &cn);
            }
        }
        #[cfg(feature = "next-gen-gc")]
        if let crate::Value::Obj(p) = &v {
            let cid = crate::nextgen::arena_class_id(*p);
            let cn = crate::nextgen::class_name(cid).unwrap_or_else(|| std::sync::Arc::from("<arena>"));
            crate::devirt::record_steady(cache_ptr as usize, (caller_ptr as usize, cp as u16), &cn);
        }
    }
    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    // CALLEE-SCOPED ENVELOPE. The guard above used to span `dispatch_miss` WHOLE —
    // resolve AND the execution of the resolved callee — and because the lock is
    // re-entrant, the first PIC miss in a ring task held it for that task's ENTIRE
    // subtree. Measured (RAVA_LANE_OUTER): 126 outer acquisitions here, one per task,
    // 172.8 s of hold, max 2.2 s each — while `jit_call`'s own envelope held 1 ms
    // across 127. So the serialization is this span, not the general-path envelope.
    //
    // Split it: hold across RESOLVE + PUBLISH (which mutates the shared IC cache,
    // freeze table, jit_cache and can run a `<clinit>`), then RELEASE before
    // descending. Nothing resolved is carried across the release — the execute half
    // re-derives from (caller, cp, kind, buf) and re-probes the tables — so there is
    // no resolve-then-stale-use window; and published verdicts are monotonic with
    // immortal fptrs (see `freeze_resolve`), which is what already makes the
    // lock-free freeze-table hit path above sound. Inner frames that genuinely need
    // the lock take it themselves (`jit_call`'s `_worker_exec_guard`, the S09 gates).
    vm.dispatch_miss_resolve(cache_ptr, recv, caller, cp as u16, kind as u8, buf as *const i64, n);
    // TYPED NATIVE ENTRY (`RAVA_NO_PIC_NATIVE=1` disables). The resolve above has just written —
    // or has just confirmed — this (site, class) pair's PERMANENT `-1`, the verdict for a
    // `java/*` receiver with no bytecode: ~19 M of a boot's ~35.1 M PIC misses. That entry is
    // currently inert; the emitted probe skips it on `fp > 0` and the call comes here anyway, to
    // walk the resolve, `jit_call`, `try_invoke_chain` and the owner cascade for an answer the
    // cache already fixed. Replace it with a native descriptor so the NEXT call is served from
    // the same 4-way compare without leaving machine code. Fill-only: this call still completes
    // through the unchanged tail below.
    #[cfg(feature = "next-gen-gc")]
    pic_native::try_fill(cache_ptr, recv, caller_ptr, cp, kind, n);
    // TYPED NATIVE ENTRY, HEAP-REF HALF (`RAVA_NO_EMIT_PICNAT2=1` disables). `dispatch_miss_
    // resolve` has just run `heappic_fill` for a non-arena receiver, so this site's 4-way cache
    // now holds whatever verdict that fill reached for the receiver's CLASS WORD — including the
    // `-1` that the emitted refpic probe finds and throws away on `fp > 0`, sending the call
    // here anyway. Bind that entry the same way the arena half binds its own. Fill-only: this
    // call still completes through the unchanged tail below.
    #[cfg(feature = "next-gen-gc")]
    pic_native::try_fill_ref(vm, cache_ptr, recv, caller_ptr, cp, kind, buf, n);
    // Leave the site for the serve arm three frames down (see `take_dispatch_site`): the worker
    // path publishes its verdicts and the serial path never has, and the serial path is 40s of a
    // 53.5s boot.
    #[cfg(feature = "next-gen-gc")]
    if crate::vm::jit_direct_call_main_dt_learn() {
        dispatch_table::set_dispatch_site(cache_ptr);
    }
    if !wide_envelope() {
        drop(_resolve_guard);
    }
    let raw = unsafe { std::slice::from_raw_parts(buf as *const i64, n.max(0) as usize) };
    // Publish THIS site's 4-way PIC for the duration of the call: the one arm that holds a
    // resolved-and-compiled capturing-lambda entry (`jit_direct_call`'s Hit) can then install a
    // thunk into it. The reader checks `(caller, cp)` against its own, so a nested dispatch can
    // never publish here — see `cap_thunk::site`.
    #[cfg(feature = "next-gen-gc")]
    let _site = crate::jit::emit::cap_thunk::enabled().then(|| {
        crate::jit::emit::cap_thunk::site::publish(cache_ptr, caller_ptr as usize, cp as u16)
    });
    match vm.jit_call(caller, cp as u16, kind as u8, raw) {
        Ok(bits) => bits,
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(e) => {
            if crate::env_flag!("RAVA_TRACE_JITERR") {
                let caller = caller_ptr as *const crate::class_loader::LoadedClass;
                let (cc, mref) = unsafe {
                    let c = &*caller;
                    let m = c.file.pool.member_ref(cp as u16)
                        .map(|m| format!("{}.{}{}", m.owner, m.name, m.descriptor))
                        .unwrap_or_default();
                    (c.file.this_class.to_string(), m)
                };
                eprintln!("[JIT-DISPATCH-ERR] caller={cc} cp={cp} kind={kind} target={mref} recv={recv:#x} err={e:?}");
            }
            if !vm.jit_pending_is_set() {
                let exc = vm.make_exception("java/lang/InternalError", "jit dispatch");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// 23.9 MONOMORPHIC DIRECT-CALL miss (invokestatic/invokespecial). The machine-code direct-mono site
/// found its single-fptr cache empty/negative. Resolve+fill the cache once (idempotent), then RUN the
/// call via the general dispatch (same ABI + error handling as rava_invoke). `buf`/`n` = the packed
/// operand cells. Fills fptr|0 (leaf) / fptr|1 (self_reg deep) / -1 (never).
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_direct_miss(cache_ptr: i64, caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    cb(0);
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): the SAME sanction `rava_invoke`
    // carries, for the same reason — every continuation of this callback is
    // `jit_call`, whose in-body G1.1 worker guard IS the gate (a lane worker routes
    // to the read-only `jit_direct_call_worker`; a callee that arm cannot serve
    // either runs under the re-entrant resolve lock or aborts the region). The
    // pre-call body below only reads the TLS pending slot, and on a gil-free worker
    // it performs NO resolve and NO cache write — see the frozen-site-cache arm.
    //
    // WHY THIS MATTERED: this callback used to take a PLAIN `vm_ref()`, which denies
    // unconditionally on a gil-free worker (the jit.rs vm_ref denial) and aborted the
    // region BEFORE any dispatch. `directmono` is default-ON, so EVERY compiled
    // invokestatic/invokespecial site whose baked single-fptr cache is cold (0) or
    // permanently negative (-1 — a `java/`-owned / native-served / uncompiled callee,
    // which `emit_direct_mono` routes here on every single call) lands in this
    // function. That denial therefore killed every off-GIL ring task within 0-1 ms,
    // long before the frozen dispatch caches could serve anything: the measured
    // "0 of 128 tasks complete, workers take 1-3 dispatch misses in a whole boot".
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    // OBSERVATIONAL CENSUS (`RAVA_DIRECTMONO_CENSUS`, default OFF): one tick per dynamic
    // entry here, classified by invoke kind, entry cache word and worker-thread context.
    // It reads the cache word without writing it; `parallel::gilfree()` separately controls
    // whether this callback may resolve/publish. The fallback tally distinguishes
    // negative and cold-zero arrivals; timing excludes all diagnostic bookkeeping.
    let census_start = if crate::directmono::census_on() {
        let worker = crate::gil::is_worker();
        let word = unsafe { *(cache_ptr as *const i64) };
        crate::directmono::note_miss(kind, word, worker);
        let op = crate::fallback_tally::invoke_kind_op(kind as u8);
        let tally_kind = if word < 0 {
            Some(crate::fallback_tally::Kind::DirectMono(op))
        } else if word == 0 {
            Some(crate::fallback_tally::Kind::DirectMonoCold(op))
        } else {
            None
        };
        if let Some(tally_kind) = tally_kind {
            crate::fallback_tally::note(tally_kind, caller_ptr as usize, cp as u16);
        }
        Some((crate::directmono::start_miss(), word, worker))
    } else {
        None
    };
    // FROZEN SITE CACHE on a gil-free worker — the exact discipline `jit_direct_call`
    // already applies to the 16-way per-site direct cache. Both halves of the fill are
    // shared publication a worker must not perform:
    //   * `direct_mono_resolve` runs `resolve_method` through `&mut Vm` (class load +
    //     resolution-cache publication, and `<clinit>` behind it);
    //   * `*cache_ptr = f` publishes into a leaked, baked global that every thread
    //     executing this compiled body reads on its next call.
    // So a worker does NEITHER: it leaves the cache exactly as main left it and just
    // runs the call below. The site stays cold for the worker (it re-enters here next
    // call and is served by `jit_call`'s worker arms), which is the same
    // read-only-until-main-warms-it contract the frozen direct cache has. Main's
    // behaviour is byte-identical to before.
    if !crate::parallel::gilfree() {
        // Fill the site cache once (idempotent — only when still 0).
        unsafe {
            if *(cache_ptr as *const i64) == 0 {
                // CENSUS ONLY (`RAVA_DIRECTMONO_CENSUS`): timestamp the resolve and
                // classify its actual result. The resolved value is REUSED for the
                // branch and cache write below, so this adds one `Instant` read and
                // keeps routing/semantics identical; the disabled path does nothing.
                if crate::directmono::census_on() {
                    let resolve_start = crate::directmono::start_miss();
                    let res = vm.direct_mono_resolve(caller, cp as u16);
                    crate::directmono::note_fill_resolve(resolve_start, res);
                    if let Some(f) = res {
                        *(cache_ptr as *mut i64) = f;
                        if f > 0 {
                            crate::directmono::note_fill(false);
                        } else {
                            crate::directmono::note_fill(true);
                            // CENSUS ONLY (`RAVA_DIRECTMONO_CENSUS`): the resolve itself
                            // returned a permanent negative. One tick per site — the `== 0`
                            // guard above makes this arm run once per site — so the total
                            // approximates how many site descriptors the fast route's bounded
                            // pool would allocate. No site names/hashes, no routing change.
                            crate::directmono::note_fill_neg_res();
                        }
                    }
                } else if let Some(f) = vm.direct_mono_resolve(caller, cp as u16) {
                    *(cache_ptr as *mut i64) = f;
                    if f > 0 {
                        crate::directmono::note_fill(false);
                    } else {
                        crate::directmono::note_fill(true);

                    }
                }
            }
        }
    }
    let cells = unsafe { std::slice::from_raw_parts(buf, n.max(0) as usize) };
    // Publish THIS site's 4-way PIC for the duration of the call, so the one arm that holds a
    // resolved-and-compiled capturing-lambda entry can install a thunk into it. The reader checks
    // `(caller, cp)` against its own, so a nested dispatch cannot publish here — see
    // `cap_thunk::site`.
    #[cfg(feature = "next-gen-gc")]
    let _site = crate::jit::emit::cap_thunk::enabled()
        .then(|| crate::jit::emit::cap_thunk::site::publish(cache_ptr, caller_ptr as usize, cp as u16));
    let census_result = vm.jit_call(caller, cp as u16, kind as u8, cells);
    // Census duration lands BEFORE any error handling: every counted callback —
    // served, thrown or faulted — contributes its elapsed ns to the same
    // (thread × entry cache word) bucket its count went into. Gate-off leaves
    // `census_start` as `None` and this is one cheap branch.
    if let Some((started, word, worker)) = census_start {
        crate::directmono::note_duration(started, word, worker);
    }
    match census_result {
        Ok(packed) => packed,
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(e) => {
            if !vm.jit_pending_is_set() {
                let (cc, mref) = unsafe {
                    let c = &*caller;
                    let m = c.file.pool.member_ref(cp as u16).map(|m| format!("{}.{}{}", m.owner, m.name, m.descriptor)).unwrap_or_default();
                    (c.file.this_class.to_string(), m)
                };
                let recv0 = if n > 0 { unsafe { *buf } } else { 0 };
                eprintln!("[JIT-ERR] direct_miss caller={cc} cp={cp} kind={kind} target={mref} recv0={recv0:#x}: {e:?}");
                let exc = vm.make_exception("java/lang/InternalError", "direct miss");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// TYPED NATIVE ENTRIES FOR THE MACHINE-CODE PIC.
///
/// ⭐**THE POPULATION.** Of ~35.1 M PIC misses on a product boot, ~19 M have a `java/*`
/// receiver with no bytecode on the classpath: they are calls into this engine's Rust
/// java.util. `dispatch_resolve_cacheable_cls` refuses them on its very first clause, and
/// `dispatch_miss_resolve` records that refusal as a PERMANENT `-1` in the site's 4-way cache
/// — after which the emitted probe's `fp > 0` test skips the entry and the call leaves machine
/// code ANYWAY, every single time, to run `rava_dispatch_miss` → negative-cache probe →
/// `jit_call` → `jit_call_body` (marshal to `Vec<Value>` + a synthetic `Frame`) →
/// `opcodes::invokevirtual` → `try_invoke_chain` → `dispatch_owner` → `owner_bits::plan` →
/// `dispatch_owner_cascade`. Five separate rows of the host profile, entered to re-derive a
/// verdict the cache already holds.
///
/// So the `-1` is not a dead end, it is an UNUSED BINDING. This module turns it into a live
/// one: the miss handler replaces the `-1` with the address of a leaked descriptor whose first
/// word is the address of a Rust thunk, and the emitted hit arm (`emit_pic_native_arms`) calls
/// that thunk directly off the SAME 4-way compare — no `rava_dispatch_miss`, no resolve, no
/// cascade walk.
///
/// **HOW A NATIVE ENTRY IS TOLD FROM A GUEST BODY — a RANGE, not a tag bit.** The obvious
/// encoding is a low tag bit beside the existing bit0=DEEP / bit1=SHIFT markers, but that needs
/// bit 2 of every compiled fptr to be clear, and the only alignment this tree actually claims
/// for JIT code is 8 — i.e. bit 2 is exactly the first bit NOT covered. Instead every descriptor
/// is carved from one leaked, fixed pool, so `desc - POOL_BASE < POOL_SPAN` identifies it with
/// one `isub` plus one unsigned compare, and cannot alias a code address for a structural reason
/// (a Rust heap allocation and the JIT's executable mapping are disjoint).
///
/// **TWO ARMS AT THE CALL SITE, BEHIND `RAVA_NO_LEAF_FASTCALL`.** The unified rooted arm was
/// measured, and rooting a pure leaf thunk is redundant work 8.39 M times a boot; the leaf
/// descriptors now get their own UNPILLED entry. Generated code (default ON) branches on
/// `t < leaf_span()`: a LEAF descriptor is called with the caller's references NOT spilled —
/// safe because a leaf thunk is a pure read: no guest allocation, no collection, no throw, so
/// the unspilled references cannot move under the call — while a ROOTED descriptor keeps
/// the spill → call → reload discipline verbatim, exactly as before. A refusal branches to
/// the site's existing `miss` block BEFORE any mutation, so the miss arm may spill the same
/// references again idempotently. `RAVA_NO_LEAF_FASTCALL=1` (or `RAVA_NO_PIC_NATIVE=1`,
/// `RAVA_NO_EMIT_MORE=1`) restores the ONE rooted arm, byte-identical.
///
/// **REFUSAL IS INVISIBLE.** Every thunk returns 1 when served (`*out` contains
/// the bits) or 0 when refused. Refusal jumps to the site's existing `miss`
/// block. A rooted thunk may return 0 only in its entry prologue before touching
/// the VM, so the miss arm may spill the same references again without storing
/// stale registers; a leaf refusal is trivially mutation-free.
///
/// Kill switch: `RAVA_NO_PIC_NATIVE=1`. Read at EMIT time as well as at fill time, so with it
/// set no descriptor is ever written AND no site emits the range test — the codegen is
/// byte-identical to before this module existed.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod pic_native {
    use std::sync::atomic::{AtomicI64, AtomicU32, AtomicUsize, Ordering::Relaxed};

    /// `RAVA_NO_PIC_NATIVE=1` disables. DEFAULT ON. Memoised: read once per emitted PIC site AND
    /// on the miss path, and `env::var_os` is a linear environment-block walk on Windows.
    #[inline]
    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_PIC_NATIVE").is_none())
    }

    /// ⭐**THE HEAP-REF HALF OF THE TYPED NATIVE ENTRY — `RAVA_NO_EMIT_PICNAT2=1` disables.**
    ///
    /// The arena arm above binds a `-1` written by `dispatch_miss_resolve`. The heap-Ref arm of
    /// the same PIC (`emit_dispatch_pic`'s `refpic`, default-ON since `heappic::live_enabled`)
    /// probes a SECOND set of negative entries that nothing has ever bound: `heappic_fill` writes
    /// `(arc_key, -1)` for a class its own resolve proved NeverCacheable, `emit_refpic_probe`
    /// finds it, discards it on `fp > 0`, and the call leaves machine code for
    /// `rava_dispatch_miss` → `dispatch_miss_resolve` → `heappic_fill`'s negative probe →
    /// `jit_call` → `try_invoke_chain` → `dispatch_owner_cascade` — to re-derive a verdict that
    /// very cache already holds. `heappic_fill`'s own comment prices the population it can see:
    /// **8.39 M of 9.06 M are `isEmpty` on four fastutil collections** (`LongOpenHashSet` 3.42 M,
    /// `LongArrayFIFOQueue` 3.11 M, `ReferenceOpenHashSet` 933 k, `ObjectArrayList` 931 k) — the
    /// cheapest question a collection can be asked, paying a full host excursion every time.
    ///
    /// Everything else is shared with the arena arm: descriptor pool, range
    /// test, unified rooted hit, refusal channel, and thunks. Only the key
    /// differs—a class word instead of a small arena class id—and those domains
    /// cannot collide in the shared four-way cache.
    ///
    /// Consults `enabled()`, so `RAVA_NO_PIC_NATIVE=1` and `RAVA_NO_EMIT_MORE=1` both take it
    /// down with the arm it extends. Read at EMIT time, so with it set the refpic arm emits not
    /// one instruction of the range test and its codegen is byte-identical to before.
    #[inline]
    pub(crate) fn ref_enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| enabled() && std::env::var_os("RAVA_NO_EMIT_PICNAT2").is_none())
    }

    /// ⭐**KILL SWITCH FOR THE LEAF/ROOTED SPLIT AT THE CALL SITE — `RAVA_NO_LEAF_FASTCALL=1`**
    /// (or the block switch `RAVA_NO_EMIT_MORE=1`, or `RAVA_NO_PIC_NATIVE=1` via the
    /// `enabled()` conjunct). Consulted at EMIT time by BOTH PIC arms (`dispatch_pic.rs`)
    /// to decide whether `emit_pic_native_arms` receives the descriptor offset `t`: with it
    /// set, the call site emits EXACTLY today's unified rooted arm — one `f` load, spill,
    /// call, reload — byte-identical, no `leaf_span` compare, no second block. Default ON,
    /// the emitted code branches at `leaf_span()` and a LEAF descriptor is entered WITHOUT
    /// spilling the caller's live references (safe: a leaf thunk is a pure read, no guest
    /// allocation, no collection — the refs cannot move), while a ROOTED descriptor keeps
    /// the spill → call → reload discipline verbatim.
    #[inline]
    pub(crate) fn leaf_fastcall_enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| enabled() && std::env::var_os("RAVA_NO_LEAF_FASTCALL").is_none())
    }

    /// ⭐**KILL SWITCH FOR THE LEAF `isEmpty` OP — `RAVA_NO_FU_ISEMPTY_LEAF=1`** (or the
    /// block switch `RAVA_NO_EMIT_MORE=1`). Withdraws the `("isEmpty","()Z")` arm from
    /// `leaf_op`/`leaf_op_ref`, so no `OP_FU_ISEMPTY` descriptor is ever bound and the
    /// census population keeps today's rooted arm / miss path exactly. Also gated on
    /// `ring_size_enabled()` — the leaf serve replays rows only the ring derive writes, so
    /// the derive must be on for the op to exist at all.
    #[inline]
    pub(crate) fn fu_isempty_leaf() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_NO_FU_ISEMPTY_LEAF").is_none()
                && !crate::flags::emit_more_disabled()
                && crate::vm::fu_fastpath::ring_size_enabled()
        })
    }

    /// Thunk ABI. `desc` is the descriptor address (== the value cached in the PIC way), `buf`
    /// the PACKED operand cells (one per argument — what `jit_call` and the `ws_*` arms expect,
    /// NOT the slot-laid hit buffer), `out` a one-word caller stack slot receiving the result
    /// bits. Returns 1 = served, 0 = refused → the caller's unchanged `miss` arm.
    pub(crate) type ThunkFn = extern "C" fn(desc: i64, buf: *mut i64, out: *mut i64) -> i64;

    const OP_GENERAL: i64 = 0;
    /// Rooted descriptor that deliberately keeps the HR-SYNTH table lookup.
    const OP_TABLE: i64 = -1;
    const OP_LIST_GET: i64 = 1;
    const OP_ATOMIC_LONG_GET: i64 = 2;
    const OP_OPTIONAL: i64 = 3;
    /// `ThreadLocal.get`/`set`. HANDLE-ONLY BY CONSTRUCTION, and that is exactly why it appears
    /// now and could not appear before: `ws_threadlocal` keys its state by HANDLE in a side
    /// table, so the arena arm — whose receiver is a raw arena pointer with no handle — would
    /// have refused it 100% of the time and retired the entry. The heap-Ref arm's receiver IS a
    /// handle, so the one reason the module doc gave for leaving it out does not apply here.
    /// The census `ws_recv_of_class` cites bills it at **832,085 calls** across `RAVA_ARENA_SYNTH`.
    const OP_THREADLOCAL: i64 = 4;
    const OP_BOOLEAN_VALUE: i64 = 5;
    const OP_NUMBER_INT_VALUE: i64 = 6;
    const OP_NUMBER_LONG_VALUE: i64 = 7;
    const OP_NUMBER_FLOAT_VALUE: i64 = 8;
    const OP_NUMBER_DOUBLE_VALUE: i64 = 9;
    /// ⭐**THE CENSUS LEAF: `isEmpty()Z` ON THE FASTUTIL COLLECTIONS.** `heappic_fill`'s own
    /// `[ICMISS]` comment prices this population at **8.39 M of 9.06 M heap-Ref misses per
    /// boot** — `LongOpenHashSet` 3.42 M, `LongArrayFIFOQueue` 3.11 M, `ReferenceOpenHashSet`
    /// 933 k, `ObjectArrayList` 931 k — the cheapest question a collection can be asked,
    /// paying a full `rava_dispatch_miss` → negative-probe → `jit_call` → `try_invoke_chain`
    /// excursion on every call. Served by `fu_isempty_leaf_serve` off a site-row the ring
    /// derive (`ring_size_fastpath`) wrote; see that function for the hit-only/no-resolve
    /// soundness rule.
    const OP_FU_ISEMPTY: i64 = 10;

    /// A refusal may be input-specific. Fall back for that call, then move the binding to its
    /// rooted twin so future calls are re-evaluated without a frequency threshold.

    /// Per-(site, class) binding. Every field is atomic because the fill runs on the main thread
    /// while a pool worker seeded from `IC_GLOBAL_BYTES` may already be reading the descriptor.
    /// `f` MUST stay at offset 0: the emitted arm loads the thunk from `[desc+0]`.
    #[repr(C)]
    pub(crate) struct NativeEntry {
        f: AtomicI64,
        caller: AtomicI64,
        cp: AtomicI64,
        kind: AtomicI64,
        nargs: AtomicI64,
        /// Leaf id, rooted-plan pointer, or `OP_TABLE` for the kill-switch route.
        op: AtomicI64,
        cache: AtomicI64,
        key: AtomicI64,
        retired: AtomicU32,
    }

    impl NativeEntry {
        fn blank() -> Self {
            Self {
                f: AtomicI64::new(0),
                caller: AtomicI64::new(0),
                cp: AtomicI64::new(0),
                kind: AtomicI64::new(0),
                nargs: AtomicI64::new(0),
                op: AtomicI64::new(0),
                cache: AtomicI64::new(0),
                key: AtomicI64::new(0),
                retired: AtomicU32::new(0),
            }
        }
    }

    #[inline]
    fn rooted_op() -> i64 {
        static OP: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
        *OP.get_or_init(|| {
            if std::env::var_os("RAVA_NO_NATIVE_PLAN_PTR").is_some() {
                OP_TABLE
            } else {
                OP_GENERAL
            }
        })
    }

    /// Bind the immutable HR-SYNTH plan once. This is cold by descriptor construction: the
    /// published pointer makes every later hit a single atomic load.
    #[cold]
    #[inline(never)]
    fn bind_plan(e: &NativeEntry, nkey: (usize, u16, usize)) -> i64 {
        let Some(plan) = crate::vm::jit_direct_call::hr_synth::PLANS
            .with(|table| table.borrow().get(&nkey).cloned())
        else {
            return OP_GENERAL;
        };
        let candidate = std::sync::Arc::into_raw(plan) as usize as i64;
        match e.op.compare_exchange(OP_GENERAL, candidate, Relaxed, Relaxed) {
            Ok(_) => candidate,
            Err(current) => {
                unsafe {
                    drop(std::sync::Arc::from_raw(
                        candidate as *const crate::vm::jit_direct_call::hr_synth::NativePlan,
                    ))
                };
                current
            }
        }
    }

    /// Pool halves. Leaf entries occupy `[0, NAT_LEAF_N)`, rooted `[NAT_LEAF_N, ..)`, so the ONE
    /// subtraction the arm already computed classifies both membership and arm with two compares
    /// and no load. Exhaustion is not an error: the fill stops and those sites keep today's
    /// behaviour, which is the whole refusal discipline in one line.
    const NAT_LEAF_N: usize = 4096;
    /// Rooted capacity with the heap-Ref arm OFF — the shipped arena-only number.
    const NAT_ROOT_ARENA_ONLY: usize = 20480;

    /// Rooted capacity. `try_fill_ref` adds a whole SECOND population of (site, class) pairs —
    /// every never-cacheable heap-Ref class at every PIC site — on top of the arena pairs that
    /// already share this half, so the arena-only bound is doubled when that arm is on.
    ///
    /// ⚠**IT IS GATE-DEPENDENT ON PURPOSE.** `pool_span()` is BAKED into every emitted range
    /// test; growing the pool unconditionally would change that immediate even with
    /// `RAVA_NO_EMIT_PICNAT2=1` set, and then "the codegen is byte-identical when off" would be
    /// false. Both gates are process constants read once, so the size is fixed before the first
    /// site compiles and can never change under a live `pool_base()`.
    ///
    /// Exhaustion is not an error — the fill stops and those pairs keep today's behaviour — but
    /// it WAS silent, which is why `NAT_LEAF_FULL`/`NAT_ROOT_FULL` exist below: a saturated pool
    /// and an unbindable population look identical in the fill count alone.
    #[inline]
    fn nat_root_n() -> usize {
        if ref_enabled() {
            2 * NAT_ROOT_ARENA_ONLY
        } else {
            NAT_ROOT_ARENA_ONLY
        }
    }

    static POOL: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static LEAF_NEXT: AtomicUsize = AtomicUsize::new(0);
    static ROOT_NEXT: AtomicUsize = AtomicUsize::new(0);

    #[inline]
    pub(crate) fn entry_size() -> usize {
        std::mem::size_of::<NativeEntry>()
    }

    /// Base address of the descriptor pool, allocated once and never moved — it is BAKED into
    /// every emitted native arm, so it must exist before the first site is compiled and must
    /// stay put for the life of the process.
    pub(crate) fn pool_base() -> usize {
        *POOL.get_or_init(|| {
            let v: Vec<NativeEntry> =
                (0..NAT_LEAF_N + nat_root_n()).map(|_| NativeEntry::blank()).collect();
            Box::leak(v.into_boxed_slice()).as_ptr() as usize
        })
    }

    #[inline]
    pub(crate) fn pool_span() -> usize {
        (NAT_LEAF_N + nat_root_n()) * entry_size()
    }

    pub(crate) fn descriptor_kind(ptr: usize) -> Option<&'static str> {
        let base = *POOL.get()?;
        let offset = ptr.checked_sub(base)?;
        if offset >= pool_span() || offset % entry_size() != 0 {
            return None;
        }
        Some(if offset / entry_size() < NAT_LEAF_N {
            "pic-native-leaf"
        } else {
            "pic-native-rooted"
        })
    }

    #[inline]
    pub(crate) fn leaf_span() -> usize {
        NAT_LEAF_N * entry_size()
    }

    /// Counters, behind the same gate the `ws_*` arms use — an unconditional RMW on a path taken
    /// millions of times biases the very A/B it exists to inform.
    pub(crate) static NAT_FILLED: AtomicI64 = AtomicI64::new(0);
    pub(crate) static NAT_LEAF_SERVE: AtomicI64 = AtomicI64::new(0);
    pub(crate) static NAT_LEAF_REFUSE: AtomicI64 = AtomicI64::new(0);
    pub(crate) static NAT_ROOT_SERVE: AtomicI64 = AtomicI64::new(0);
    pub(crate) static NAT_RETIRED: AtomicI64 = AtomicI64::new(0);
    /// Heap-Ref fills, split out from `NAT_FILLED` so the two arms' coverage is separable — the
    /// arena arm's number says nothing about whether the new one bound anything.
    pub(crate) static NAT_REF_FILLED: AtomicI64 = AtomicI64::new(0);
    /// (site, class) pairs the heap-Ref arm declined because all four ways were live positive
    /// entries. Taking one would evict a real machine-code hit to install a host call.
    pub(crate) static NAT_REF_NOWAY: AtomicI64 = AtomicI64::new(0);
    /// Pool exhaustion — the blind spot that makes a low fill count unreadable, because a
    /// saturated pool and an unbindable population produce the same low number.
    ///
    /// ⚠Behind `stats()` like everything else here, and that is not caution about noise: an
    /// exhausted pool does NOT stop being asked. Every later miss of an unbound pair re-enters
    /// the fill, so an ungated RMW here would be a per-call atomic on exactly the sites the pool
    /// failed to serve — the shape this round is supposed to remove, not add.
    pub(crate) static NAT_LEAF_FULL: AtomicI64 = AtomicI64::new(0);
    pub(crate) static NAT_ROOT_FULL: AtomicI64 = AtomicI64::new(0);

    #[inline]
    fn stats() -> bool {
        crate::jit::freeze_resolve::stats_on()
    }

    /// (site, class) → descriptor index. Consulted only on the miss path, which a filled site
    /// stops taking — so this is warm-up traffic and a `Mutex` costs nothing measurable there.
    /// It exists so an evicted-then-re-missed pair REUSES its descriptor instead of burning a
    /// fresh pool slot, and so a retired pair is never re-filled.
    ///
    /// The key's second component is the PAIR KEY as it is stored in the cache way — a dense
    /// arena `class_id` for the arena arm, a class WORD for the heap-Ref arm. It is `i64`, not
    /// `u32`, because a class word is a pointer; the two arms cannot collide for the same reason
    /// their entries share one 4-way cache (small ids vs large aligned pointers).
    fn seen() -> &'static std::sync::Mutex<std::collections::HashMap<(i64, i64), usize>> {
        static M: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<(i64, i64), usize>>,
        > = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    /// `seen` value meaning "asked and answered: this pair can never bind". It is a SENTINEL and
    /// never an index, so `entry_at` must never be reached with it. It exists so the heap-Ref
    /// verdict below — the one place in this module that runs a resolve — runs at most once per
    /// (site, class); without it a pair the engine refuses would re-derive its refusal on every
    /// miss, which is the exact re-resolve loop the negative cache was built to end.
    const SEEN_UNBINDABLE: usize = usize::MAX;

    #[inline]
    fn entry_at(idx: usize) -> &'static NativeEntry {
        unsafe { &*((pool_base() + idx * entry_size()) as *const NativeEntry) }
    }

    /// Which pure-read arm, if any, implements this (receiver class, name, descriptor).
    ///
    /// This is RESOLUTION, not eligibility routing: it binds a symbolic method reference plus
    /// the receiver's concrete class to the Rust function that implements it, exactly as
    /// `dispatch_owner_cascade` does per call — the difference is that this runs ONCE per
    /// (site, class) instead of ~19 M times. Nothing here decides whether a site may be
    /// compiled, inlined or cached; the `-1` already decided that.
    ///
    /// Every arm named here re-checks the receiver's class AND the site's name/descriptor
    /// itself (`ws_recv_of_class` is exact-class, and each arm reads `member_ref(cp)`), so a
    /// wrong binding can only produce a REFUSAL, never a wrong answer.
    ///
    /// **WHY THESE THREE AND NOT THE OTHER TWO WS ARMS.** Two separate audits gate this list.
    ///
    /// GC-LEAFNESS, because the leaf arm calls with the caller's refs unspilled: every read
    /// below bottoms out in `synth_get_ro` / `Heap::array_len` / `Heap::array_get` and returns
    /// through `box_ref`, none of which births a guest object — `box_ref`'s only non-trivial arm
    /// is `frames::box_push`, a push onto a host-side table. No guest allocation ⇒ no nursery
    /// bump ⇒ no collection ⇒ the unspilled refs cannot move under the call.
    ///
    /// REFUSAL FREQUENCY, because a refusal is not free here: it falls to `miss`, and the
    /// descriptor this fill wrote has displaced the `-1` that `dispatch_miss_resolve` uses to
    /// short-circuit, so the refusing call pays a full re-resolve before `try_fill` puts the
    /// descriptor back. That is fine for an EDGE-case refusal (an out-of-range index, a `get()`
    /// on an empty `Optional`) and it is why `ws_list_contains` is deliberately absent: its
    /// refusal is DATA-dependent — it declines every needle whose element key could escalate
    /// (`@`/`o:`/`m:`/`c:`), i.e. every object needle — so binding it would re-resolve on a
    /// steady fraction of all calls. `ws_threadlocal` is absent for a different reason: its
    /// state is a side table keyed by HANDLE, so an arena receiver — the only kind this arm ever
    /// sees — can never match it, and the entry would refuse 100% of the time.
    fn leaf_op(cls: &str, name: &str, desc: &str) -> Option<i64> {
        let existing = match (cls, name, desc) {
            ("java/util/ArrayList", "get", "(I)Ljava/lang/Object;") => Some(OP_LIST_GET),
            ("java/util/concurrent/atomic/AtomicLong", "get", "()J") => {
                Some(OP_ATOMIC_LONG_GET)
            }
            ("java/util/Optional", "isPresent", "()Z")
            | ("java/util/Optional", "isEmpty", "()Z")
            | ("java/util/Optional", "get", "()Ljava/lang/Object;") => Some(OP_OPTIONAL),
            (_, "isEmpty", "()Z") if fu_isempty_leaf() => Some(OP_FU_ISEMPTY),
            _ => None,
        };
        existing.or_else(|| {
            if !native_program_leaf_enabled() {
                return None;
            }
            match crate::natives::program_for(cls, name, desc)? {
                crate::natives::NativeProgram::BooleanValue => Some(OP_BOOLEAN_VALUE),
                crate::natives::NativeProgram::NumberIntValue => Some(OP_NUMBER_INT_VALUE),
                crate::natives::NativeProgram::NumberLongValue => Some(OP_NUMBER_LONG_VALUE),
                crate::natives::NativeProgram::NumberFloatValue => Some(OP_NUMBER_FLOAT_VALUE),
                crate::natives::NativeProgram::NumberDoubleValue => Some(OP_NUMBER_DOUBLE_VALUE),
                _ => None,
            }
        })
    }

    fn native_program_leaf_enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_NO_NATIVE_PROGRAM_LEAF").is_none())
    }

    fn leaf_op_class(
        cls: crate::nextgen::LeafClass,
        name: &str,
        desc: &str,
    ) -> Option<i64> {
        let existing = match (cls, name, desc) {
            (crate::nextgen::LeafClass::ArrayList, "get", "(I)Ljava/lang/Object;") => {
                Some(OP_LIST_GET)
            }
            (crate::nextgen::LeafClass::AtomicLong, "get", "()J") => {
                Some(OP_ATOMIC_LONG_GET)
            }
            (crate::nextgen::LeafClass::Optional, "isPresent", "()Z")
            | (crate::nextgen::LeafClass::Optional, "isEmpty", "()Z")
            | (crate::nextgen::LeafClass::Optional, "get", "()Ljava/lang/Object;") => {
                Some(OP_OPTIONAL)
            }
            // The receiver class is deliberately irrelevant: the binder validates the site row
            // against the actual class key before serving.
            (_, "isEmpty", "()Z") if fu_isempty_leaf() => Some(OP_FU_ISEMPTY),
            _ => None,
        };
        existing.or_else(|| {
            if !native_program_leaf_enabled() {
                return None;
            }
            let provider = match cls {
                crate::nextgen::LeafClass::Boolean => crate::natives::NativeProgramClass::Boolean,
                crate::nextgen::LeafClass::Number => crate::natives::NativeProgramClass::Number,
                _ => return None,
            };
            match crate::natives::program_for_class(provider, name, desc)? {
                crate::natives::NativeProgram::BooleanValue => Some(OP_BOOLEAN_VALUE),
                crate::natives::NativeProgram::NumberIntValue => Some(OP_NUMBER_INT_VALUE),
                crate::natives::NativeProgram::NumberLongValue => Some(OP_NUMBER_LONG_VALUE),
                crate::natives::NativeProgram::NumberFloatValue => Some(OP_NUMBER_FLOAT_VALUE),
                crate::natives::NativeProgram::NumberDoubleValue => Some(OP_NUMBER_DOUBLE_VALUE),
                _ => None,
            }
        })
    }

    /// `leaf_op` for a HANDLE receiver, plus the one arm that only a handle can satisfy.
    ///
    /// ⭐**WHY THE ARENA LEAF LIST IS NEARLY DEAD, AND WHY THIS ONE IS NOT.** Four of the five
    /// shapes `leaf_op` names never reach `emit_dispatch_pic` at all: `fu_routable` admits the
    /// `java/util/Optional` readers (`get`/`isPresent`/`isEmpty`/`orElse`/`orElseThrow`) and the
    /// `List`/`Collection`/`AbstractList` operations to `env.fu_invoke`, and `invoke_virtual`
    /// routes every `size()I` to `emit_arraylist_size_inline`, all of them BEFORE the PIC is
    /// emitted. A site routed there has no PIC cache, so it never gets a `-1` and can never
    /// reach a fill. Only `AtomicLong.get()J` — which no admission claims — and a `get(I)` site
    /// declared on the concrete `java/util/ArrayList` type survive to the arena arm.
    ///
    /// `ThreadLocal.get`/`set` is claimed by nothing either, and unlike the four above it is a
    /// pure per-thread side-table access with no guest allocation and no throw — a leaf by the
    /// same argument `ws_arraylist_get` uses (`box_ref`'s only non-trivial arm is
    /// `frames::box_push`, a host table). A `get` that MISSES this thread's table returns `None`
    /// and takes the site's unchanged `miss` arm; if `tl_worker` is off it refuses and is
    /// demoted on the first miss rather than using a refusal-count cutoff.
    fn leaf_op_ref(cls: &str, name: &str, desc: &str) -> Option<i64> {
        if let Some(op) = leaf_op(cls, name, desc) {
            return Some(op);
        }
        match (cls, name, desc) {
            ("java/lang/ThreadLocal" | "java/lang/InheritableThreadLocal", "get", "()Ljava/lang/Object;")
            | ("java/lang/ThreadLocal" | "java/lang/InheritableThreadLocal", "set", "(Ljava/lang/Object;)V") => {
                Some(OP_THREADLOCAL)
            }
            _ => None,
        }
    }

    /// A leaf refusal falls back immediately to a rooted twin. This removes the history-based
    /// refusal threshold while retaining a complete fallback for data-dependent misses such as
    /// an empty Optional or an out-of-range index. If the rooted half of the pool is exhausted,
    /// the original `-1` negative-cache entry is restored.
    ///
    /// Only main may rewrite the slot: a worker reads a per-thread slice seeded from the global
    /// box, so its refusal says nothing about main's entry and its write would land in the
    /// wrong table.
    #[inline]
    fn claim_retirement(e: &NativeEntry) -> bool {
        e.retired.swap(1, Relaxed) == 0
    }

    fn note_refusal(e: &NativeEntry) {
        if stats() {
            NAT_LEAF_REFUSE.fetch_add(1, Relaxed);
        }
        if crate::gil::is_worker() || crate::parallel::gilfree() || !claim_retirement(e) {
            return;
        }
        let cache = e.cache.load(Relaxed);
        if cache == 0 {
            return;
        }
        let key = e.key.load(Relaxed);
        let me = (e as *const NativeEntry) as i64;
        let idx = ROOT_NEXT.fetch_add(1, Relaxed);
        let repl = if idx < nat_root_n() {
            let r = entry_at(NAT_LEAF_N + idx);
            r.caller.store(e.caller.load(Relaxed), Relaxed);
            r.cp.store(e.cp.load(Relaxed), Relaxed);
            r.kind.store(e.kind.load(Relaxed), Relaxed);
            r.nargs.store(e.nargs.load(Relaxed), Relaxed);
            r.op.store(rooted_op(), Relaxed);
            r.cache.store(cache, Relaxed);
            r.key.store(key, Relaxed);
            r.retired.store(0, Relaxed);
            r.f.store(nt_rooted as ThunkFn as usize as i64, Relaxed);
            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
            if let Ok(mut m) = seen().lock() {
                m.insert((cache, key), NAT_LEAF_N + idx);
            }
            (r as *const NativeEntry) as i64
        } else {
            ROOT_NEXT.store(nat_root_n(), Relaxed);
            if stats() { NAT_ROOT_FULL.fetch_add(1, Relaxed); }
            -1
        };
        unsafe {
            let c = cache as *mut i64;
            for k in 0..4usize {
                if *c.add(k * 2) == key && *c.add(k * 2 + 1) == me {
                    *c.add(k * 2 + 1) = repl;
                    break;
                }
            }
        }
        if stats() {
            NAT_RETIRED.fetch_add(1, Relaxed);
        }
    }

    /// LEAF thunk: a pure read — no allocation, no GC, no throw — which is why its arm needs no
    /// spill and no root registration. Every arm it calls is an already-shipped, gate-covered
    /// `ws_*` fast path whose `None` means "not a shape I can serve"; that `None` becomes a 0
    /// here and the site falls to its unchanged `miss` arm having touched nothing.
    ///
    /// ⚠ These arms exist today ONLY behind `crate::gil::is_worker()` — `rava_dispatch_miss`'s
    /// step-0.5 and `jit_call_body`'s worker guard — i.e. the SERIAL boot, which is the large
    /// majority of a product boot's wall, never runs a single one of them. That is the coverage
    /// this entry opens.
    pub(crate) extern "C" fn nt_leaf(desc: i64, buf: *mut i64, out: *mut i64) -> i64 {
        let e = unsafe { &*(desc as *const NativeEntry) };
        // A worker's contract is the pre-lock cascade in `rava_dispatch_miss` (freeze table →
        // frozen direct cache → ws natives → resolve lock). Refusing here keeps worker behaviour
        // byte-identical instead of re-implementing that ordering.
        if crate::gil::is_worker() || crate::parallel::gilfree() {
            return 0;
        }
        let Some(vm) = crate::jit::vm_ref_worker_sanctioned() else { return 0 };
        if vm.jit_pending_is_set() {
            return 0;
        }
        let n = e.nargs.load(Relaxed).max(0) as usize;
        let raw = unsafe { std::slice::from_raw_parts(buf as *const i64, n) };
        let caller = e.caller.load(Relaxed) as *const crate::class_loader::LoadedClass;
        let cp = e.cp.load(Relaxed) as u16;
        let op = e.op.load(Relaxed);
        let served = match op {
            OP_LIST_GET => vm.ws_arraylist_get(caller, cp, raw),
            OP_ATOMIC_LONG_GET => vm.ws_atomic_long_get(caller, cp, raw),
            OP_OPTIONAL => vm.ws_optional(caller, cp, raw),
            // Heap-Ref arm only (see `leaf_op_ref`): the arm re-reads `member_ref(cp)` and the
            // receiver's class itself, so a wrong binding can only refuse, never answer wrongly.
            OP_THREADLOCAL => vm.ws_threadlocal(caller, cp, raw),
            OP_BOOLEAN_VALUE => crate::natives::invoke_program_cells(
                crate::natives::NativeProgram::BooleanValue,
                vm,
                raw,
            ),
            OP_NUMBER_INT_VALUE => crate::natives::invoke_program_cells(
                crate::natives::NativeProgram::NumberIntValue,
                vm,
                raw,
            ),
            OP_NUMBER_LONG_VALUE => crate::natives::invoke_program_cells(
                crate::natives::NativeProgram::NumberLongValue,
                vm,
                raw,
            ),
            OP_NUMBER_FLOAT_VALUE => crate::natives::invoke_program_cells(
                crate::natives::NativeProgram::NumberFloatValue,
                vm,
                raw,
            ),
            OP_NUMBER_DOUBLE_VALUE => crate::natives::invoke_program_cells(
                crate::natives::NativeProgram::NumberDoubleValue,
                vm,
                raw,
            ),
            // ⭐THE CENSUS LEAF: 8.39 M of the 9.06 M heap-Ref misses a boot, answered off a
            // site-row the ring derive wrote (see `fu_isempty_leaf_serve` for the hit-only
            // rule). `&self` pure reads only — the arm is entered unspilled by the LEAF
            // fastcall, and every path here either answers or refuses BEFORE any mutation.
            OP_FU_ISEMPTY => vm.fu_isempty_leaf_serve(caller, cp, raw),
            _ => None,
        };
        match served {
            Some(bits) => {
                unsafe { *out = bits };
                if stats() {
                    NAT_LEAF_SERVE.fetch_add(1, Relaxed);
                }
                1
            }
            None => {
                note_refusal(e);
                0
            }
        }
    }

    /// ROOTED thunk: the general native call, entered WITHOUT `rava_dispatch_miss`.
    ///
    /// What it drops relative to that handler, and why each is safe to drop for a pair whose
    /// verdict is the permanent `-1`:
    ///   * `dispatch_miss_resolve` — for a known-negative pair its whole body is the 4-way
    ///     negative probe followed by an immediate return. There is nothing to publish.
    ///   * `lane_probe::note_site`, `pilot_census`, `bootdisp`, `picprobe`, the freeze-coverage
    ///     census, `devirt::record_steady` — diagnostics, inert in production, and each one an
    ///     unconditional call or a gate load on this path.
    ///   * the worker pre-lock cascade and the resolve guard — reached only on a worker, and a
    ///     worker is refused in the prologue below, so that path is untouched.
    /// What it KEEPS, because dropping either changes behaviour rather than cost:
    /// `dispatch_table::set_dispatch_site` (feeds the default-ON dispatch-table learning three
    /// frames down) and the `cap_thunk` site publication (lets a resolved capturing lambda
    /// install its thunk into this very site).
    ///
    /// ⚠ IT MAY ONLY RETURN 0 FROM THE PROLOGUE. The caller's rooted arm has already spilled by
    /// the time this runs; jumping to `miss` afterwards re-spills the same SSA values, which is
    /// idempotent ONLY while nothing has moved. Past the `vm_ref` the answer is always 1.
    pub(crate) extern "C" fn nt_rooted(desc: i64, buf: *mut i64, out: *mut i64) -> i64 {
        let e = unsafe { &*(desc as *const NativeEntry) };
        if crate::gil::is_worker() || crate::parallel::gilfree() {
            return 0; // prologue refusal — nothing touched, so the miss arm is exact
        }
        let Some(vm) = crate::jit::vm_ref_worker_sanctioned() else { return 0 };
        crate::jit::cb(0); // this IS the dispatch fallback: same billing as rava_dispatch_miss
        unsafe { *out = 0 };
        if vm.jit_pending_is_set() {
            return 1;
        }
        let caller_raw = e.caller.load(Relaxed);
        let caller = caller_raw as *const crate::class_loader::LoadedClass;
        let cp = e.cp.load(Relaxed) as u16;
        let kind = e.kind.load(Relaxed) as u8;
        let cache = e.cache.load(Relaxed);
        if crate::vm::jit_direct_call_main_dt_learn() {
            crate::jit::dispatch_table::set_dispatch_site(cache);
        }
        let n = e.nargs.load(Relaxed).max(0) as usize;
        let raw = unsafe { std::slice::from_raw_parts(buf as *const i64, n) };
        let _site = crate::jit::emit::cap_thunk::enabled()
            .then(|| crate::jit::emit::cap_thunk::site::publish(cache, caller_raw as usize, cp));
        // The descriptor and HR-SYNTH describe the same immutable
        // (caller, cp, receiver-class) pair. Cache a strong plan pointer in this descriptor after
        // the first rooted hit: the HR-SYNTH table remains authoritative for construction and
        // invalidation, but repeated PIC hits no longer hash the same three-word key.
        let recv_guard = match raw.first().copied() {
            Some(cell)
                if (cell as u64) >> 32
                    == (crate::value::REF_TAG as u64) >> 32 =>
            {
                vm.heap.class_name_ptr(cell as u32)
            }
            Some(cell) if cell != 0 && (cell as u64) >> 40 == 0 && cell & 7 == 0 => {
                (e.key.load(Relaxed) as usize).wrapping_add(1)
            }
            _ => 0,
        };
        let nkey = (caller_raw as usize, cp, recv_guard);
        let direct = if recv_guard == 0 {
            None
        } else {
            let mut plan_ptr = e.op.load(Relaxed);
            if plan_ptr == OP_GENERAL {
                plan_ptr = bind_plan(e, nkey);
            }
            if plan_ptr == OP_TABLE || plan_ptr == OP_GENERAL {
                vm.hr_synth_serve(nkey, caller, cp, kind, raw)
            } else {
                let plan = unsafe {
                    &*(plan_ptr as usize
                        as *const crate::vm::jit_direct_call::hr_synth::NativePlan)
                };
                if plan.recheck && crate::class_loader::has_bytecode(&plan.chain[0]) {
                    // `hr_synth_finish` would drop the table entry too; clear the descriptor's
                    // strong cached route first so the next call cannot retry a stale plan.
                    e.op.store(OP_GENERAL, Relaxed);
                    vm.hr_synth_serve(nkey, caller, cp, kind, raw)
                } else {
                    vm.hr_synth_finish(nkey, plan, caller, cp, kind, raw)
                }
            }
        };
        let bits = match direct.unwrap_or_else(|| vm.jit_call(caller, cp, kind, raw)) {
            Ok(b) => b,
            Err(crate::VmError::Throw(x)) => {
                if !vm.jit_pending_is_set() {
                    vm.jit_pending_set(x);
                }
                vm.heap.set_jit_fault();
                0
            }
            Err(_) => {
                if !vm.jit_pending_is_set() {
                    let exc = vm.make_exception("java/lang/InternalError", "jit dispatch");
                    vm.jit_pending_set(exc);
                }
                vm.heap.set_jit_fault();
                0
            }
        };
        unsafe { *out = bits };
        if stats() {
            NAT_ROOT_SERVE.fetch_add(1, Relaxed);
        }
        1
    }

    /// Bind this (site, class) to a native entry, replacing the permanent `-1`.
    ///
    /// Preconditions, all of them evidence the rules permit (bytecode / descriptor / resolved
    /// dispatch / runtime shape — never a workload, a timing or a prior abort):
    ///   * the receiver is an ARENA object — the `-1` is written by `dispatch_miss_resolve`'s
    ///     arena arm and nowhere else, and the emitted native arm sits inside the arena hit;
    ///   * the site's 4-way cache ALREADY holds `(class_id, -1)`, i.e. the engine has itself
    ///     proven this pair can never resolve to a compiled guest body, so nothing is displaced
    ///     and no future positive fill is denied;
    ///   * the caller is the main thread — a worker reads a per-thread slice and its verdicts
    ///     belong to the pre-lock cascade.
    pub(crate) fn try_fill(cache_ptr: i64, recv: i64, caller_ptr: i64, cp: i64, kind: i64, n: i64) {
        if !enabled() || cache_ptr == 0 || caller_ptr == 0 {
            return;
        }
        if crate::gil::is_worker() || crate::parallel::gilfree() {
            return;
        }
        // 1 = invokevirtual, 3 = invokeinterface: the only kinds `emit_dispatch_pic` serves.
        if kind != 1 && kind != 3 {
            return;
        }
        if recv == 0 || (recv as u64) >> 40 != 0 {
            return;
        }
        let class_id = unsafe { *(recv as *const u32) };
        // THE PRECONDITION: a proven-permanent refusal is already recorded here.
        let cache = cache_ptr as *mut i64;
        let mut way = None;
        for k in 0..4usize {
            unsafe {
                if *cache.add(k * 2) == class_id as i64 && *cache.add(k * 2 + 1) == -1 {
                    way = Some(k);
                    break;
                }
            }
        }
        let Some(way) = way else { return };

        let Ok(mut map) = seen().lock() else { return };
        if let Some(&idx) = map.get(&(cache_ptr, class_id as i64)) {
            if idx == SEEN_UNBINDABLE {
                return; // heap-Ref verdict sentinel; an arena pair never stores it
            }
            let e = entry_at(idx);
            if e.retired.load(Relaxed) != 0 {
                return; // proven useless once; never pay for it again
            }
            // Re-publish after an eviction — the descriptor is still exactly right.
            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
            unsafe { *cache.add(way * 2 + 1) = (e as *const NativeEntry) as i64 };
            return;
        }

        let caller: &crate::class_loader::LoadedClass =
            unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
        let Some(m) = caller.file.pool.member_ref(cp as u16) else { return };
        // A LEAF binding bypasses `jit_call` entirely, so it must not exist while the pure-call
        // memo is armed: `dispatch_resolve_cacheable_cls` refuses to cache exactly the sites the
        // memo layer must SEE, and serving one from machine code would hide it from the window.
        let leaf = if crate::pure_memo::armed() {
            None
        } else {
            crate::nextgen::leaf_class_of(class_id)
                .and_then(|class| leaf_op_class(class, m.name, m.descriptor))
        };
        let (idx, f): (usize, ThunkFn) = match leaf {
            Some(_) => {
                let i = LEAF_NEXT.fetch_add(1, Relaxed);
                if i >= NAT_LEAF_N {
                    LEAF_NEXT.store(NAT_LEAF_N, Relaxed);
                    if stats() { NAT_LEAF_FULL.fetch_add(1, Relaxed); }
                    return;
                }
                (i, nt_leaf as ThunkFn)
            }
            None => {
                let i = ROOT_NEXT.fetch_add(1, Relaxed);
                if i >= nat_root_n() {
                    ROOT_NEXT.store(nat_root_n(), Relaxed);
                    if stats() { NAT_ROOT_FULL.fetch_add(1, Relaxed); }
                    return;
                }
                (NAT_LEAF_N + i, nt_rooted as ThunkFn)
            }
        };
        let e = entry_at(idx);
        e.caller.store(caller_ptr, Relaxed);
        e.cp.store(cp, Relaxed);
        e.kind.store(kind, Relaxed);
        e.nargs.store(n.max(0), Relaxed);
        e.op.store(leaf.unwrap_or_else(rooted_op), Relaxed);
        e.cache.store(cache_ptr, Relaxed);
        e.key.store(class_id as i64, Relaxed);
        e.retired.store(0, Relaxed);
        e.f.store(f as usize as i64, Relaxed);
        // Publish the descriptor only after every field it exposes is visible: a worker seeded
        // from IC_GLOBAL_BYTES can start executing this entry the instant the slot is written.
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        unsafe { *cache.add(way * 2 + 1) = (e as *const NativeEntry) as i64 };
        map.insert((cache_ptr, class_id as i64), idx);
        if stats() {
            NAT_FILLED.fetch_add(1, Relaxed);
        }
    }

    /// What this (site, class) already is, as far as the descriptor table knows. Computed under
    /// the `seen` lock and then RELEASED, because the verdict step below can run guest code.
    enum Known {
        /// Already has a descriptor — republish it into a way if one is available.
        Bound(usize),
        /// Asked once, cannot bind. Never ask again.
        Unbindable,
        Unknown,
    }

    /// Bind a HEAP-REF (site, class) pair to a native entry — the twin of `try_fill`, for the
    /// half of the same 4-way cache that `heappic_fill` writes.
    ///
    /// **THE KEY IS THE CLASS WORD**, the value `emit_heap_ref_class_key` reads in machine code
    /// and `emit_refpic_probe` compares against, so a descriptor written here is found by the
    /// exact four compares that already run — the range test in `emit_dispatch_pic`'s refpic arm
    /// then routes it to the thunk instead of through `rava_dispatch_miss`.
    ///
    /// **TWO WAYS IN, AND ONLY TWO.**
    ///   * A way already holding `(key, -1)`: `heappic_fill` has itself proven this pair
    ///     NeverCacheable and recorded it. Nothing is displaced, no verdict is re-derived, and
    ///     no future positive fill is denied — the fill that wrote the `-1` is the one that
    ///     would have written a positive entry.
    ///   * A FREE way (`fptr == 0`) plus a fresh `NeverCacheable` from the engine's own
    ///     `dispatch_resolve_cacheable_cls`. This covers the population `heappic_fill` never
    ///     records at all: its FAST NEGATIVE BAIL returns for a `$`-synthetic or bytecode-less
    ///     `java/*` receiver class BEFORE writing anything, and its own comment prices that at
    ///     **19.4 M of the 19,856,228 refusals** a boot. A `NotYet` or `Compiled` verdict binds
    ///     NOTHING, so a callee that compiles later still gets its machine-code entry.
    ///
    /// A site whose four ways are all live positive entries is DECLINED (`NAT_REF_NOWAY`):
    /// evicting a real machine-code hit to install a host call is a straight loss.
    ///
    /// **WHY THE VERDICT RUNS OUTSIDE THE LOCK.** `dispatch_resolve_cacheable_cls` may load a
    /// class and run its `<clinit>` — guest code, which can re-enter compiled code, miss a PIC
    /// and land back in this function. Holding `seen`'s non-reentrant `Mutex` across it would
    /// deadlock, and a collection inside it would move objects the operand buffer points at, so
    /// `buf` is registered as a GC root for the duration exactly as `dispatch_miss_resolve` does
    /// around the identical call. Phase C re-takes the lock and re-checks, so a re-entrant bind
    /// of the same pair is adopted rather than duplicated.
    ///
    /// Cost if removed: those calls keep leaving machine code for `rava_dispatch_miss` →
    /// `dispatch_miss_resolve` → `heappic_fill`'s class-word read and negative probe → `jit_call`
    /// → `try_invoke_chain` → `dispatch_owner_cascade`, per call, forever.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_fill_ref(
        vm: &mut crate::Vm,
        cache_ptr: i64,
        recv: i64,
        caller_ptr: i64,
        cp: i64,
        kind: i64,
        buf: i64,
        n: i64,
    ) {
        if !ref_enabled() || cache_ptr == 0 || caller_ptr == 0 {
            return;
        }
        // Same thread contract as the arena fill: a worker reads a per-thread slice seeded from
        // the global box, so its verdicts belong to the pre-lock cascade and its writes would
        // land in the wrong table.
        if crate::gil::is_worker() || crate::parallel::gilfree() {
            return;
        }
        // 1 = invokevirtual, 3 = invokeinterface: the only kinds `emit_dispatch_pic` serves.
        if kind != 1 && kind != 3 {
            return;
        }
        // REF_TAG = 1<<40 uniquely identifies a heap handle (BOX/ARRAY/ABOX are 2/4/8).
        if recv == 0 || (recv as u64) >> 40 != 1 {
            return;
        }
        let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
        // The authoritative class word — the same one `heappic_fill` keys with and the same one
        // the emitted arm reads. A swept slot has none, and then there is nothing to key on.
        let Some(key) = vm.heap.object_class_word(handle) else { return };
        if key <= 0 {
            return;
        }
        // Way survey: the pair's own negative entry, its own descriptor (already bound), or a
        // free way. `fp > 0` and inside the pool span is the same membership test the machine
        // code applies, so "already bound" is decided exactly as the hit path decides it.
        let cache = cache_ptr as *mut i64;
        let (base, span) = (pool_base() as i64, pool_span() as i64);
        let mut neg_way: Option<usize> = None;
        let mut free_way: Option<usize> = None;
        for k in 0..4usize {
            let (kk, fp) = unsafe { (*cache.add(k * 2), *cache.add(k * 2 + 1)) };
            if kk == key && fp > 0 && (fp.wrapping_sub(base) as u64) < (span as u64) {
                return; // already bound at this site
            }
            if kk == key && fp == -1 {
                neg_way = Some(k);
                break;
            }
            if fp == 0 && free_way.is_none() {
                free_way = Some(k);
            }
        }

        // PHASE A — what the table already knows, under the lock, then release it.
        let known = match seen().lock() {
            Ok(map) => match map.get(&(cache_ptr, key)) {
                Some(&SEEN_UNBINDABLE) => Known::Unbindable,
                Some(&idx) => Known::Bound(idx),
                None => Known::Unknown,
            },
            Err(_) => return,
        };
        // Write the descriptor into a way — and RE-CHECK the way first, because the survey above
        // may be stale: the verdict step below can run a `<clinit>`, and anything that runs can
        // miss this very site and fill this very way. The only words this may overwrite are the
        // ones it surveyed as available — empty (0), this pair's own negative entry (-1), or a
        // descriptor already in the pool. A live compiled fptr is never displaced: a machine-code
        // hit beats a host call, and clobbering one would be a straight loss.
        let publish = |way: usize, e: &'static NativeEntry| {
            let (kk, fp) = unsafe { (*cache.add(way * 2), *cache.add(way * 2 + 1)) };
            let free = fp == 0;
            let ours = kk == key
                && (fp == -1 || (fp > 0 && (fp.wrapping_sub(base) as u64) < (span as u64)));
            if !free && !ours {
                if stats() { NAT_REF_NOWAY.fetch_add(1, Relaxed); }
                return;
            }
            // Publish only after every field the descriptor exposes is visible: a worker seeded
            // from IC_GLOBAL_BYTES can start executing this entry the instant the slot is written.
            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
            unsafe {
                *cache.add(way * 2) = key;
                *cache.add(way * 2 + 1) = (e as *const NativeEntry) as i64;
            }
        };
        match known {
            Known::Unbindable => return,
            Known::Bound(idx) => {
                let e = entry_at(idx);
                if e.retired.load(Relaxed) != 0 {
                    return; // proven useless once; never pay for it again
                }
                // Re-publish after an eviction — the descriptor is still exactly right. Its own
                // negative way first, else a free one; never over a live positive entry.
                if let Some(way) = neg_way.or(free_way) {
                    publish(way, e);
                } else if stats() {
                    NAT_REF_NOWAY.fetch_add(1, Relaxed);
                }
                return;
            }
            Known::Unknown => {}
        }

        // PHASE B — a way, and the verdict that licenses using it. NO LOCK IS HELD HERE.
        let way = match neg_way {
            Some(w) => w,
            None => {
                let Some(w) = free_way else {
                    if stats() { NAT_REF_NOWAY.fetch_add(1, Relaxed); }
                    return;
                };
                // `caller` comes from a raw pointer, so its borrow is independent of `&mut vm`;
                // `cls` must be owned because `class_of` borrows `vm.heap`.
                let caller: &crate::class_loader::LoadedClass =
                    unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
                let Some(m) = caller.file.pool.member_ref(cp as u16) else { return };
                let (name, desc) = (m.name.to_string(), m.descriptor.to_string());
                let Some(cls) = vm.heap.class_of(handle).map(|c| c.to_string()) else { return };
                // ROOT the operand cells across the resolve: it can load a class, run its
                // `<clinit>`, collect and MOVE the objects `buf` points at — and `buf` is the
                // compiled frame's operand buffer, which `rava_dispatch_miss` re-reads and hands
                // to `jit_call` after this returns. Same registration, same reason, as the arena
                // resolve in `dispatch_miss_resolve`.
                crate::frames::cells_push((buf as *const i64, n.max(0) as usize));
                let verdict = vm.dispatch_resolve_cacheable_cls(&cls, &name, &desc);
                crate::frames::cells_pop();
                // A `<clinit>` that threw leaves a pending exception the caller must deliver;
                // publishing into the cache on the way out would be a side effect of a failed
                // call. Not memoised either — the verdict was never reached.
                if vm.jit_pending_is_set() {
                    return;
                }
                if !matches!(verdict, crate::DispatchResolve::NeverCacheable) {
                    // Compiled / NotYet: a machine-code entry is still POSSIBLE for this pair,
                    // and a real compiled fptr beats a descriptor, so leave the way alone and
                    // let `heappic_fill` write it.
                    //
                    // ⚠**AND MEMOISE THE REFUSAL, WHICH IS THE WHOLE COST CONTROL.** `NotYet` is
                    // transient, so the tempting rule is "ask again later" — but `heappic_fill`
                    // has no NotYet memo on this path (its own `FILL_NOTYET` counter exists to
                    // say so), which means a NotYet pair already re-resolves on every call. A
                    // second resolve per call would DOUBLE that, and the pair loses nothing by
                    // never being asked again: if the callee compiles, `heappic_fill` fills a
                    // positive entry and the site is served from machine code without this
                    // module. Asking once is therefore strictly better than asking periodically.
                    if let Ok(mut map) = seen().lock() {
                        map.insert((cache_ptr, key), SEEN_UNBINDABLE);
                    }
                    return;
                }
                w
            }
        };

        // PHASE C — allocate and publish, re-checking what Phase B may have missed.
        let Ok(mut map) = seen().lock() else { return };
        if let Some(&idx) = map.get(&(cache_ptr, key)) {
            // A re-entrant bind (from the `<clinit>` above) got here first — adopt it.
            if idx != SEEN_UNBINDABLE {
                let e = entry_at(idx);
                if e.retired.load(Relaxed) == 0 {
                    publish(way, e);
                }
            }
            return;
        }
        let caller: &crate::class_loader::LoadedClass =
            unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
        let Some(m) = caller.file.pool.member_ref(cp as u16) else { return };
        // Same exclusion the arena fill applies: a LEAF binding bypasses `jit_call` entirely, so
        // it must not exist while the pure-call memo is armed — that layer must SEE these calls.
        let leaf = if crate::pure_memo::armed() {
            None
        } else {
            vm.heap.class_of(handle).and_then(|cls| leaf_op_ref(cls, m.name, m.descriptor))
        };
        let (idx, f): (usize, ThunkFn) = match leaf {
            Some(_) => {
                let i = LEAF_NEXT.fetch_add(1, Relaxed);
                if i >= NAT_LEAF_N {
                    LEAF_NEXT.store(NAT_LEAF_N, Relaxed);
                    if stats() { NAT_LEAF_FULL.fetch_add(1, Relaxed); }
                    return;
                }
                (i, nt_leaf as ThunkFn)
            }
            None => {
                let i = ROOT_NEXT.fetch_add(1, Relaxed);
                if i >= nat_root_n() {
                    ROOT_NEXT.store(nat_root_n(), Relaxed);
                    if stats() { NAT_ROOT_FULL.fetch_add(1, Relaxed); }
                    return;
                }
                (NAT_LEAF_N + i, nt_rooted as ThunkFn)
            }
        };
        let e = entry_at(idx);
        e.caller.store(caller_ptr, Relaxed);
        e.cp.store(cp, Relaxed);
        e.kind.store(kind, Relaxed);
        e.nargs.store(n.max(0), Relaxed);
        e.op.store(leaf.unwrap_or_else(rooted_op), Relaxed);
        e.cache.store(cache_ptr, Relaxed);
        e.key.store(key, Relaxed);
        e.retired.store(0, Relaxed);
        e.f.store(f as usize as i64, Relaxed);
        publish(way, e);
        map.insert((cache_ptr, key), idx);
        if stats() {
            NAT_REF_FILLED.fetch_add(1, Relaxed);
        }
    }

    /// One line naming what the entries actually did, printed by the same reporting the other
    /// dispatch counters use. A fill count says what the cache HOLDS; only the serve counters
    /// say what it SERVED, and a zero there beside a non-zero fill is the failure this module
    /// would otherwise hide.
    #[allow(dead_code)]
    pub(crate) fn report() -> String {
        format!(
            "[PIC-NATIVE] arena_filled={} ref_filled={} leaf_serve={} leaf_refuse={} rooted_serve={} retired={} | ref_no_free_way={} leaf_pool_full={} rooted_pool_full={} (leaf_cap={} rooted_cap={})",
            NAT_FILLED.load(Relaxed),
            NAT_REF_FILLED.load(Relaxed),
            NAT_LEAF_SERVE.load(Relaxed),
            NAT_LEAF_REFUSE.load(Relaxed),
            NAT_ROOT_SERVE.load(Relaxed),
            NAT_RETIRED.load(Relaxed),
            NAT_REF_NOWAY.load(Relaxed),
            NAT_LEAF_FULL.load(Relaxed),
            NAT_ROOT_FULL.load(Relaxed),
            NAT_LEAF_N,
            nat_root_n(),
        )
    }
    #[cfg(test)]
    mod tests {
        use super::{claim_retirement, NativeEntry};

        #[test]
        fn first_leaf_refusal_claims_fallback_once() {
            let entry = NativeEntry::blank();
            assert!(claim_retirement(&entry));
            assert!(!claim_retirement(&entry));
        }
        #[test]
        fn arena_leaf_binding_matches_name_route() {
            use std::sync::Arc;

            let cases = [
                ("java/util/ArrayList", "get", "(I)Ljava/lang/Object;"),
                ("java/util/concurrent/atomic/AtomicLong", "get", "()J"),
                ("java/util/Optional", "isPresent", "()Z"),
                ("java/util/Optional", "isEmpty", "()Z"),
                ("java/util/Optional", "get", "()Ljava/lang/Object;"),
                ("java/lang/Boolean", "booleanValue", "()Z"),
                ("java/lang/Integer", "intValue", "()I"),
                ("java/lang/Long", "longValue", "()J"),
                ("java/lang/Float", "floatValue", "()F"),
                ("java/lang/Double", "doubleValue", "()D"),
                ("java/util/HashMap", "isEmpty", "()Z"),
                ("java/util/Optional", "get", "()Z"),
                ("java/lang/Boolean", "intValue", "()I"),
                ("net/example/ArrayListish", "get", "(I)Ljava/lang/Object;"),
                ("[arena-string]", "get", "(I)Ljava/lang/Object;"),
            ];
            for (name, method, descriptor) in cases {
                let class_id = crate::nextgen::register_class(
                    &Arc::<str>::from(name),
                    &Arc::<[(u32, u8)]>::from([]),
                );
                let by_name = crate::nextgen::class_name(class_id)
                    .and_then(|class| super::leaf_op(&class, method, descriptor));
                let by_contract = crate::nextgen::leaf_class_of(class_id)
                    .and_then(|class| super::leaf_op_class(class, method, descriptor));
                assert_eq!(by_contract, by_name, "class {name} method {method}{descriptor}");
            }
            assert_eq!(crate::nextgen::leaf_class_of(u32::MAX), None);
        }
    }
}
