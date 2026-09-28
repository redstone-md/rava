//! Invocation bridges: the synthetic-frame call, the indy bootstrap, and the two calls served without one.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


/// invokestatic/special/virtual/interface callback. Reads `n` packed-i64 args
/// from `buf`, resolves+invokes via the VM, returns the packed result (0=void).
/// On a thrown exception sets jit_pending (first wins) and returns 0; if a fault
/// is already pending it short-circuits (no further calls = no post-fault side
/// effects, matching the interpreter).
pub(crate) extern "C" fn rava_invoke(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    crate::vm::invoke_census::note(caller_ptr, cp as u16);
    cb(0);
    let _route = crate::vm::aot_route_diag::InvokeScope::enter();
    // 23.2 PIC probe: a call from a site with NO machine-code PIC (static/special always; a
    // virtual/interface that bailed the PIC gate) landed here. Split by kind (inert, gated).
    if crate::picprobe::enabled() {
        crate::picprobe::plain_invoke(kind as u8);
    }
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): on a gil-free worker every
    // continuation of this callback is `jit_call`, whose in-body worker guard
    // (lib.rs, "G1.1") aborts BEFORE any resolve/compile/interpret unless the
    // locked-exec envelope runs it under the resolve lock; the pre-guard body
    // here only reads the TLS pending slot and inert probes.
    let Some(vm) = vm_ref_worker_sanctioned() else {
        crate::vm::aot_route_diag::callback_early();
        return 0;
    };
    if vm.jit_pending_is_set() {
        crate::vm::aot_route_diag::callback_early();
        return 0;
    }
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    // 23.5 Lever-B sizing (RAVA_JIT_DIRECT_PROBE, inert): of the static/special plain-invokes reaching
    // this host callback, how many are leaf+compiled (a machine-code direct call could serve them)?
    #[cfg(feature = "next-gen-gc")]
    if crate::directprobe::enabled() && (kind == 0 || kind == 2) {
        vm.direct_leaf_probe(caller_ptr, cp as u16, kind as u8);
    }
    // 23.7 self_reg coverage of the static/special pool (Option A sizing).
    #[cfg(feature = "next-gen-gc")]
    if crate::selfreg::enabled() && (kind == 0 || kind == 2) {
        vm.selfreg_probe_ss(caller_ptr, cp as u16);
    }
    let cells = unsafe { std::slice::from_raw_parts(buf, n.max(0) as usize) };
    // AOT invoke-args forensics (RAVA_AOT_TRACE_INVOKE): dump the caller + resolved
    // callee + the args buffer for a call whose receiver (buf[0], non-static) is null —
    // the args-marshalling-bug signature (a null/duplicated receiver reaching a callee).
    static G_TRACE_INVOKE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if env_gate("RAVA_AOT_TRACE_INVOKE", &G_TRACE_INVOKE) && kind != 0 && cells.first() == Some(&0) {
        let caller: &crate::class_loader::LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp as u16);
        eprintln!(
            "[AOT-INV-NULLRECV] caller={} kind={kind} cp={cp} callee={}.{} n={n} buf={:?}",
            caller.file.this_class,
            m.map(|m| m.owner).unwrap_or("?"),
            m.map(|m| m.name).unwrap_or("?"),
            cells,
        );
    }
    static G_TRACE_GRAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let tgras = env_gate("RAVA_AOT_TRACE_GRAS", &G_TRACE_GRAS) && {
        let c: &crate::class_loader::LoadedClass = unsafe { &*caller_ptr };
        c.file.pool.member_ref(cp as u16).map(|m| m.name == "getResourceAsStream" || m.name == "getClass").unwrap_or(false)
    };
    if tgras {
        let c: &crate::class_loader::LoadedClass = unsafe { &*caller_ptr };
        let nm = c.file.pool.member_ref(cp as u16).map(|m| m.name).unwrap_or("?");
        let recv = format!("{:?}", unbox_ref(vm, cells.first().copied().unwrap_or(0), ""));
        let arg = if cells.len() > 1 { format!("{:?}", unbox_ref(vm, cells[1], "")) } else { "-".into() };
        eprintln!("[GRAS] {nm} recv={recv} arg={arg}");
    }
    match vm.jit_call(caller_ptr, cp as u16, kind as u8, cells) {
        Ok(packed) => {
            if tgras {
                eprintln!("[GRAS]   -> {:?}", unbox_ref(vm, packed, ""));
            }
            packed
        }
        Err(crate::VmError::Throw(e)) => {
            crate::vm::aot_route_diag::callback_early();
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(e) => {
            crate::vm::aot_route_diag::callback_early();
            if !vm.jit_pending_is_set() {
                // This becomes an InternalError the program likely treats as fatal —
                // always surface the underlying engine error, or the root cause is
                // invisible (MC catches Throwable at top level; its logger is a no-op).
                eprintln!("[JIT-ERR] invoke cp={cp} kind={kind}: {e:?}");
                let exc = vm.make_exception("java/lang/InternalError", "jit call");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// JIT `athrow`: the compiled code popped the exception object into `exc_cell`. Stash
/// it as the pending exception (jit_run turns a pending exception into a real throw
/// when the compiled function returns) and latch the fault so the compiled body's
/// subsequent callbacks short-circuit. Returns a dummy — the compiled function's
/// return value is ignored once a fault is pending. This lets methods with a
/// bounds-check throw (e.g. fastutil `getDouble`, VoxelShape accessors — millions of
/// boot calls) COMPILE instead of bailing to the interpreter on the `athrow` opcode.
pub(crate) extern "C" fn rava_throw(exc_cell: i64) -> i64 {
    // The null arm creates an exception; the emitter roots its live references.
    // GAP-S09 stage 4 SANCTIONED: the pending slot is
    // execution_state TLS, `unbox_ref` takes `&Vm`, and `set_jit_fault` latches
    // the ACTIVE JitCtx — the worker-PRIVATE block under `WorkerJitCtxGuard`
    // (stage-2 hole-2 closure). Null construction additionally needs a worker gate.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    if !vm.jit_pending_is_set() {
        let e = unbox_ref(vm, exc_cell, "");
        if matches!(e, crate::Value::Null) {
            #[cfg(feature = "next-gen-gc")]
            let _locked = match worker_illegal_gate(s09census::IDX_THROW_NULL, "athrow(null)") {
                WorkerGate::Serial => None,
                WorkerGate::Locked(guard) => Some(guard),
                WorkerGate::Abort => return 0,
            };
            set_pending(vm, "java/lang/NullPointerException", &crate::npe::throw_null());
        } else {
            vm.jit_pending_set(e);
        }
    }
    vm.heap.set_jit_fault();
    0
}

/// FASTUTIL SCALAR FAST-PATH (next-gen-gc): same `(caller, cp, kind, buf, n)` ABI as
/// `rava_invoke`, but routes through `jit_fu_invoke` — which serves an
/// `IntImmutableList`/`IntArrayList` `getInt`/`size`/`isEmpty`/`iterator` straight off
/// its backing `int[]` (no fastutil-bytecode synthetic frame) and otherwise falls
/// back to the identical general dispatch. The JIT emits this in place of `rava_invoke`
/// only at call sites whose static member matches `fu_routable`.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_fu_invoke(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    cb(0);
    // GAP-S09 census: fu_invoke can mutate fastutil backing state — split out of
    // the base `invoke` kind (stage-3 abort-stub candidate).
    s09census::note(s09census::IDX_FU_INVOKE);
    // GAP-S09 stage 3: fu_invoke may mutate the shared `$elems` backing (add/set/
    // remove) — worker-illegal. Abort BEFORE dispatch, or run locked under
    // RAVA_M3_WORKER_LOCKED_EXEC (guard held across the whole body).
    // WHICH MEMBER takes the lock. `RAVA_LANE_OUTER` put 82,382,752 of a boot's 86,302,646
    // resolve-lock acquisitions on the `WorkerGate::Locked` arm, and a per-kind tally showed
    // 100% of them are THIS callback. But the gate is entered before anything knows whether
    // the call is a READ (`getInt`, `size`, `isEmpty`) or a MUTATION (`add`, `set`, `remove`)
    // — and the exclusive lock only exists for the mutations. The member is already in hand
    // here: `caller_ptr` + `cp` name it without a resolve. Diagnostic only, behind the probe.
    // THE READ TAKES NO LOCK. A gil-free worker asking a `DoubleImmutableList` for an
    // element is 100% of this gate's traffic (see `Vm::fu_read_shared` for the census), and
    // a read needs `&Vm`, not the exclusive `&mut Vm` the gate exists to serialise. A miss
    // here changes nothing: it falls into the gate below exactly as before.
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel::gilfree() {
        // The SHARED guard is held for the whole read and released before the gate below.
        // `None` means this thread already owns the exclusive side, where a `.read()` would
        // self-deadlock — fall through, and the gate passes it through as `Nested`.
        if let (Some(_read), Some(vm)) =
            (crate::jit::resolve_lock_read(), crate::jit::vm_shared())
        {
            let class: &crate::class_loader::LoadedClass =
                unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
            if let Some(m) = class.file.pool.member_ref(cp as u16) {
                if let Some(r) = vm.fu_read_shared(m.name, m.descriptor, unsafe {
                    std::slice::from_raw_parts(buf, n.max(0) as usize)
                }) {
                    return r;
                }
            }
        }
    }
    let _locked_exec = match worker_illegal_gate(s09census::IDX_FU_INVOKE, "fu_invoke") {
        WorkerGate::Serial => None,
        WorkerGate::Locked(g) => Some(g),
        WorkerGate::Abort => return 0,
    };
    let Some(vm) = vm_ref() else { return 0 };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let caller_ptr = caller_ptr as *const crate::class_loader::LoadedClass;
    let cells = unsafe { std::slice::from_raw_parts(buf, n.max(0) as usize) };
    match vm.jit_fu_invoke(caller_ptr, cp as u16, kind as u8, cells) {
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
                eprintln!("[JIT-ERR] fu_invoke cp={cp} kind={kind}: {e:?}");
                let exc = vm.make_exception("java/lang/InternalError", "jit fu call");
                vm.jit_pending_set(exc);
            }
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// ENUM ordinal FAST-PATH (next-gen-gc): same ABI as `rava_invoke`. `ordinal()I`
/// resolves to `java/lang/Enum.ordinal` — FINAL, native-served (jar enums carry no
/// bytecode for it), so neither the direct-call cache nor the dispatch PIC could
/// ever serve it and every call paid the full invoke gauntlet (DensityFunctions
/// `$Type.ordinal()` switches in worldgen, `Direction.ordinal()` in shapes). Read
/// the `$enumOrd` synthetic field (written by the `Enum.<init>` native) straight
/// off the heap receiver; anything else — arena receiver, a non-enum class with
/// its own `ordinal()I`, an unset field — falls back to the identical general
/// dispatch, which is always correct.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_enum_ordinal(caller_ptr: i64, cp: i64, kind: i64, buf: *const i64, n: i64) -> i64 {
    // GAP-S09 stage 4 SANCTIONED (read-only probe): the fast arm reads the
    // interned `$enumOrd` field id (`field_id_opt`, `&self`) + `get_field_hint`
    // (`&self`) off the frozen heap receiver — no publication; the miss arm is
    // `rava_invoke`, itself abort-guarded on workers inside `jit_call`.
    if let Some(vm) = vm_ref_worker_sanctioned() {
        if !vm.jit_pending_is_set() && n >= 1 {
            let cell = crate::nextgen::forward_if_moved(unsafe { *buf });
            if (cell as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
                // Interned once; absent until the first enum <init> runs (then the
                // fallback path serves — and no enum exists to call ordinal() on).
                static ORD_ID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
                let id = *ORD_ID.get_or_init(|| vm.heap.field_id_opt("$enumOrd"));
                if let Some(id) = id {
                    let (v, _) = vm.heap.get_field_hint(cell as u32, id, 0);
                    if let crate::value::Value::Int(ord) = v {
                        return pack_local(&crate::value::Value::Int(ord));
                    }
                }
            } else if crate::nextgen::arena_enum_enabled()
                && crate::nextgen::is_arena_obj_addr(cell as u64)
            {
                // ENUM MIGRATION (RAVA_ARENA_ENUM): an arena-born enum receiver — the
                // cell is a raw 0b00 arena pointer (fails the REF_TAG test above), so
                // read `$enumOrd` straight off its injected layout cell. Read-only
                // arena getfield (same sanctioned class as the handle arm); any miss
                // (non-enum arena obj / absent slot) falls to the general dispatch.
                static ORD_ID_A: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
                let id = *ORD_ID_A.get_or_init(|| vm.heap.field_id_opt("$enumOrd"));
                if let Some(id) = id {
                    let p = unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) };
                    let cid = crate::nextgen::arena_class_id(p);
                    if crate::nextgen::slot_of(cid, id).is_some() {
                        if let crate::value::Value::Int(ord) = crate::opcodes::arena_getfield(p, id) {
                            return pack_local(&crate::value::Value::Int(ord));
                        }
                    }
                }
            }
        }
    }
    rava_invoke(caller_ptr, cp, kind, buf, n)
}

/// invokedynamic from compiled code (next-gen-gc): concat → ArenaString / lambda /
/// etc. `buf` = n packed arg cells.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_indy(caller_ptr: i64, cp: i64, buf: *const i64, n: i64) -> i64 {
    cb(0);
    // GAP-S09 census: indy is a stage-3 worker-abort candidate — split it out of
    // the base `invoke` kind so the acceptance census can prove zero gil-free hits.
    s09census::note(s09census::IDX_INDY);
    // GAP-S09 stage 4 SANCTIONED (abort-guarded): the gil-free triage below either
    // aborts the COLD arm BEFORE any resolution, takes the locked-exec envelope, or
    // serves a WARM site whose whole service is thread-private — the `&mut` never
    // escapes those three arms.
    let Some(vm) = vm_ref_worker_sanctioned() else { return 0 };
    // GILFREE-RINGS: only COLD indy is worker-illegal. Resolution walks the
    // bootstrap methods and interns the impl identity into the shared class pool,
    // and a non-capturing site publishes into `indy_singletons` +
    // `indy_singleton_handles` — those stay denied. A WARM capturing lambda mutates
    // nothing shared (see `worker_indy_is_mutation_free`), and denying it kept the
    // ring phase — 120 tasks, the single biggest item in the boot — fully serial:
    // every admitted task aborted here at the first indy in its body.
    //
    // The lane self-warms: the first task on each worker still aborts, and its
    // resident (GIL-bound) re-run happens ON THIS SAME THREAD, which populates this
    // thread's plan cache — so every later task on that worker takes the warm arm.
    let _locked_exec = if crate::parallel::gilfree() {
        let caller = unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() };
        let refusal = match caller {
            Some(c) => crate::opcodes::worker_indy_refusal(vm, c, cp as u16),
            None => Some("indy(caller undecodable)"),
        };
        if crate::worker_locked_exec_enabled() {
            Some(resolve_lock_acquire())
        } else if let Some(why) = refusal {
            worker_abort_stub(s09census::IDX_INDY, why);
            return 0;
        } else {
            None // servable: closure + `$fi` box come from this worker's own slices
        }
    } else {
        None
    };
    if vm.jit_pending_is_set() {
        return 0;
    }
    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    let cells = unsafe { std::slice::from_raw_parts(buf, n.max(0) as usize) };
    // HR-INDY census (`RAVA_JIT_PIC_PROBE` only — one relaxed add per callback otherwise):
    // what still reaches this callback, classified by the shape the inline mint would have
    // seen, so `[PC-INDY]` names the remaining prize rather than a total. `RAVA_NO_HR_INDY=1`
    // silences it with the rest of the area.
    if crate::picprobe::enabled() && crate::jit::lambda_shape::hr_enabled() {
        if let Some(c) = unsafe { caller.as_ref() } {
            crate::jit::lambda_shape::note_callback(c, cp as u16, cells);
        }
    }
    // HR-UNDESC census (`RAVA_NO_HR_UNDESC=1` makes both halves inert): is this callback at an
    // UNDESCRIBED capturing `LambdaMetafactory` site — the mint's own population — and does
    // the site it is about to run leave it described (first visit) or not (a repeat, by
    // cause)? The recorder itself is inside `jit_indy` (`opcodes::invokedynamic` → the Lambda
    // arm), so the callback needs nothing more than to look before and after. Two loads on the
    // warm path; an atomic add only on the undescribed arm, which is the population being
    // removed.
    let undesc_site = match unsafe { caller.as_ref() } {
        Some(c) => crate::jit::lambda_shape::undesc_before(c, cp as u16, cells.len()),
        None => None,
    };
    static G_TRACE_INDY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let trace = env_gate("RAVA_AOT_TRACE_INDY", &G_TRACE_INDY);
    let show = |vm: &mut crate::Vm, cell: i64| -> String {
        match unbox_ref(vm, cell, "") {
            crate::Value::Str(s) => format!("Str({s:?})"),
            crate::Value::Obj(p) if crate::nextgen::is_arena_string(p) => {
                format!("AStr({:?})", String::from_utf8_lossy(crate::nextgen::arena_str_bytes(p)))
            }
            v => format!("{v:?}"),
        }
    };
    if trace {
        let a0 = if !cells.is_empty() { show(vm, cells[0]) } else { "-".into() };
        eprintln!("[INDY] cp={cp} n={n} arg0={a0}");
    }
    match vm.jit_indy(caller, cp as u16, cells) {
        Ok(bits) => {
            if trace {
                let s = show(vm, bits);
                eprintln!("[INDY]   -> {s}");
            }
            if undesc_site.is_some() {
                if let Some(c) = unsafe { caller.as_ref() } {
                    crate::jit::lambda_shape::undesc_after(c, cp as u16, cells.len());
                }
            }
            return bits;
        }
        Err(crate::VmError::Throw(e)) => {
            if !vm.jit_pending_is_set() {
                vm.jit_pending_set(e);
            }
            vm.heap.set_jit_fault();
            0
        }
        Err(_) => {
            vm.heap.set_jit_fault();
            0
        }
    }
}

/// Register-ABI arc (I1b): host callback that resolves a static invokestatic call site's
/// target to a register-ABI entry pointer (`compile_register`), or 1 = permanently
/// ineligible (the caller then uses its buffer fallback). The caller caches the result in
/// a baked per-site cell, so this runs at most once per site. Same VM access + worker
/// sanction as `rava_invoke`.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_resolve_reg(caller_ptr: i64, cp: i64) -> i64 {
    // WORKER REFUSAL. This callback COMPILES (`compile_register`) and memoises the result
    // in `Vm::reg_abi_cache` — both need the exclusive `&mut Vm` and the compile module a
    // GIL-free worker does not have. `vm_ref_worker_sanctioned` would hand one over
    // unchecked, so refuse here instead. Returns 0, NOT the ineligible sentinel: 0 leaves
    // the site cache unresolved so the main thread can still resolve it later, whereas 1
    // would permanently poison a perfectly eligible site. The emitted fast-path test is
    // `ptr > 1`, so 0 routes this call through the buffer fallback.
    #[cfg(feature = "next-gen-gc")]
    if crate::parallel::gilfree() {
        return 0;
    }
    let Some(vm) = vm_ref_worker_sanctioned() else { return 1 };
    let caller = caller_ptr as *const crate::class_loader::LoadedClass;
    vm.jit_resolve_reg(caller, cp as u16)
}
