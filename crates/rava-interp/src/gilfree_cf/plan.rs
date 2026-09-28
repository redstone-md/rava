//! Deciding what a worker may be handed, and packing the claim.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

/// Cheap dry probe for the SEAM routing decision: would `prepare` plausibly form
/// a plan? (No claims, no allocation.) Routing an UN-plannable task into the
/// resident runtime is a boot-DEADLOCK hazard: MC awaits unproven-executor
/// futures with a guest spin that holds the GIL, starving the workers — so only
/// tasks the gilfree lane can actually run off-GIL may be diverted.
pub(crate) fn plannable(vm: &mut Vm, task: &Value) -> bool {
    plannable_with_target(vm, task).is_some()
}

/// Resolve and compile a viable task once, returning the identity for diagnostics.
pub(crate) fn plannable_with_target(vm: &mut Vm, task: &Value) -> Option<ImplTarget> {
    let Some(target) = resolve_impl(vm, task) else {
        return None;
    };
    let (owner, name, desc) = (&target.owner, &target.name, &target.desc);
    let Ok(Some(decl)) = vm.resolve_method(owner, name, desc) else {
        reject(owner, name, "resolve_method");
        return None;
    };
    if !decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
        reject(owner, name, "clinit-pending");
        return None;
    }
    let Some(method) = decl.file.method(name, desc) else {
        reject(owner, name, "method-missing");
        return None;
    };
    if !access_ok(method.access_flags, target.receiver.is_some()) {
        reject(owner, name, "access-flags");
        return None;
    }
    let Some(code) = method.code.as_ref() else {
        reject(owner, name, "no-code");
        return None;
    };
    if !code.exceptions.is_empty() {
        reject(owner, name, "exception-handlers");
        return None;
    }
    // SOUNDNESS GATE (S4 optimistic-admit REVERTED). The S2 callback write-barrier
    // (rava_putfield/aastore) is NOT a complete barrier: compiled INLINE arena
    // stores are emitted as direct machine code with NO callback, so a worker body
    // that writes shared state via an inline store (e.g. MappedRegistry.freeze)
    // bypasses the barrier → SIGSEGV. A complete barrier needs JIT-EMITTED
    // inline-store worker-checks (a codegen change — the real wall). Until then,
    // keep the conservative static gate so flag-ON stays SAFE (admits nothing that
    // could reach an ungated inline store); the S2 barrier is retained
    // infrastructure for when the codegen barrier lands.
    // RAVA_GILFREE_OPTIMISTIC (test gate): skip the conservative static gate and
    // admit optimistically, relying on the S2 write-barrier to deopt on shared
    // writes. Used to validate the worker-safe-compile hypothesis before building
    // the dual-compilation cache. Default OFF → sound static gate (safe).
    //
    // GILFREE-RINGS stage 4: the codegen barrier now EXISTS (the `cow_armed`
    // putfield arm plus the new array-store arm), so the rings lane admits
    // optimistically and relies on it — the static gate rejects the ring lambda
    // (it calls `findBiomeHorizontal`, a deep virtual tree the devirt analysis
    // cannot prove) and would leave the phase serial forever.
    if !rings()
        && !{
            static OPT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            crate::jit::env_gate("RAVA_GILFREE_OPTIMISTIC", &OPT)
        }
        && !vm.is_offloadable_devirt(owner, name, desc, false, true)
    {
        reject(owner, name, "is_offloadable_devirt");
        return None;
    }
    let key = code as *const rava_classfile::Code as usize;
    if matches!(vm.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_))) {
        return Some(target);
    }
    // item-1: the shared static impl is still cold at supplyAsync enqueue time
    // (its SAM-apply dispatch never ticks the compile threshold). Force-compile
    // it once under the GIL so the task can route off-GIL; cheap after the first
    // of the N tasks that share this impl. (owner/name/desc are owned Strings;
    // the borrows above are into the owned `decl`, not `vm`.)
    if vm.force_compile(owner, name, desc) {
        Some(target)
    } else {
        reject(owner, name, "force_compile");
        None
    }
}

pub(crate) fn resolve_impl(vm: &mut Vm, task: &Value) -> Option<ImplTarget> {
    if !enabled() || vm.trampoline {
        return None;
    }
    // ARENA TWIN. Under `RAVA_ARENA_LAMBDA` a capturing closure is a `Value::Obj`, and
    // this function refused it on its first line — so the lane admitted NOTHING and the
    // boot ran serial (`batch=0 admit=0` against the control's `batch=3 admit=8`, ledger
    // row 46). The three facts the predicate needs all have arena accessors; the arms
    // below mirror the handle arms exactly rather than inventing new admission rules.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = task {
        if crate::nextgen::is_arena_lambda(*p) {
            let site = crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(*p))?;
            let (owner, name, desc) = (
                site.owner.to_string(),
                site.name.to_string(),
                site.descriptor.to_string(),
            );
            return match crate::nextgen::arena_lambda_kind(*p) {
                6 => Some(ImplTarget {
                    owner,
                    name,
                    desc,
                    receiver: None,
                }),
                5 | 7 if rings() => {
                    let (c, k) =
                        crate::nextgen::arena_lambda_cell(*p, crate::nextgen::AL_HDR_CELLS);
                    let receiver = crate::opcodes::decode_arena_cell(c, k);
                    if !matches!(receiver, Value::Ref(_) | Value::Obj(_)) {
                        return reject_none(&owner, &name, "receiver-not-object");
                    }
                    let rc = vm.value_class_name(&receiver);
                    if rc != owner && !impl_is_monomorphic(vm, &owner, &name, &desc, &rc) {
                        return reject_none(&owner, &name, "receiver-class-inexact");
                    }
                    Some(ImplTarget {
                        owner,
                        name,
                        desc,
                        receiver: Some(receiver),
                    })
                }
                k => {
                    if rings() {
                        note_reject(
                            &owner,
                            &name,
                            if k == 9 { "kind-9-interface" } else { "kind" },
                        );
                    }
                    None
                }
            };
        }
    }
    let Value::Ref(handle) = task else {
        return None;
    };
    let lambda = vm.heap.lambda_of(*handle)?;
    let (owner, name, desc) = (
        lambda.site.owner.to_string(),
        lambda.site.name.to_string(),
        lambda.site.descriptor.to_string(),
    );
    match lambda.kind {
        6 => Some(ImplTarget {
            owner,
            name,
            desc,
            receiver: None,
        }),
        // kind 5 = REF_invokeVirtual, 7 = REF_invokeSpecial. javac emits kind 5
        // for a PRIVATE instance lambda body (nestmate invocation), so virtual
        // dispatch is exact in practice — but prove it rather than assume:
        // require capture 0's CONCRETE class to be the impl's declaring class,
        // which makes the target monomorphic by construction.
        5 | 7 if rings() => {
            let receiver = lambda.captured.first()?.clone();
            if !matches!(receiver, Value::Ref(_) | Value::Obj(_)) {
                return reject_none(&owner, &name, "receiver-not-object");
            }
            let rc = vm.value_class_name(&receiver);
            if rc != owner && !impl_is_monomorphic(vm, &owner, &name, &desc, &rc) {
                return reject_none(&owner, &name, "receiver-class-inexact");
            }
            Some(ImplTarget {
                owner,
                name,
                desc,
                receiver: Some(receiver),
            })
        }
        k => {
            if rings() {
                note_reject(
                    &owner,
                    &name,
                    if k == 9 { "kind-9-interface" } else { "kind" },
                );
            }
            None
        }
    }
}

/// Is `owner.name desc` the target for EVERY receiver of class `recv`, i.e. is the
/// lambda's implementation method monomorphic by construction?
///
/// THE EXACT-CLASS TEST WAS TOO STRONG, and it is what kept the datapack phase
/// serial. `plan` required `value_class_name(receiver) == owner`, so a kind-5
/// (REF_invokeVirtual) lambda body declared in a BASE class was refused whenever
/// the receiver was a subclass — which is every one of MC's reload listeners:
/// the whole census of gil-free admission rejections in a boot is
/// `SimplePreparableReloadListener::lambda$reload$0 rejected-by=receiver-class-inexact`,
/// and that lambda is the datapack reload itself.
///
/// What the lane actually needs is that the call cannot dispatch anywhere else.
/// That is decided by ACCESS FLAGS, not by class identity: a `private`, `final`
/// or `static` method — or any method of a `final` class — has exactly one
/// implementation, so a receiver of any subclass resolves to this body. javac
/// emits kind 5 for a PRIVATE instance lambda body (nestmate invocation), which
/// is precisely this case.
///
/// Admission by access flags and the loaded hierarchy only: no class NAME is
/// matched, nothing about the workload is consulted, and a class this engine
/// cannot load answers `false`.
fn impl_is_monomorphic(vm: &mut Vm, owner: &str, name: &str, desc: &str, recv: &str) -> bool {
    /// JVMS 4.6 / 4.1 access flags.
    const ACC_PRIVATE: u16 = 0x0002;
    const ACC_STATIC: u16 = 0x0008;
    const ACC_FINAL: u16 = 0x0010;
    // The receiver must BE an `owner` at all — otherwise the body would be reached
    // through a type it was never declared on.
    if recv != owner && !vm.super_chain(recv).iter().any(|a| &**a == owner) {
        return false;
    }
    let Ok(c) = vm.loader.get_or_load(owner) else {
        return false;
    };
    if c.file.access_flags & ACC_FINAL != 0 {
        return true;
    }
    match c.file.method(name, desc) {
        Some(m) => m.access_flags & (ACC_PRIVATE | ACC_STATIC | ACC_FINAL) != 0,
        None => false,
    }
}

/// WHICH GATE REFUSES A LANE ADMISSION (`RAVA_PLAN_CENSUS`, default OFF, inert when off).
///
/// ⭐`plan_meta` has ELEVEN `return None` gates ahead of `trace_admit`, and a product boot admits
/// a handful of tasks against a JVM that runs the same reload on three cores. The cores are the
/// largest number left in the engine, so which of those eleven binds is the question the whole
/// parallel front rests on — and nothing could answer it: the trace prints ADMISSIONS, and a
/// refusal has never been counted at all.
pub(crate) mod plan_census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) const NAMES: [&str; 13] = [
        "off/trampoline",
        "not-a-lambda value",
        "captures unreadable",
        "resolve_impl failed",
        "resolve_method failed",
        "clinit NOT done",
        "access flags (static/sync/native/abstract)",
        "no Code attribute",
        "has exception handlers",
        "not compiled after force_compile",
        "descriptor unparseable",
        "arity: captures + args != params",
        "pure-admit: body writes shared state",
    ];
    /// Slot for the structural purity gate (`RAVA_LANE_PURE_ADMIT`), so a refusal it makes is
    /// distinguishable from the eleven pre-existing ones rather than folded into the last.
    pub(crate) const PURE_ADMIT: usize = 12;
    pub(crate) static N: [AtomicU64; 13] = [const { AtomicU64::new(0) }; 13];
    pub(crate) static ADMIT: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_PLAN_CENSUS").is_some())
    }
    #[inline]
    pub(crate) fn bail(i: usize) {
        if on() {
            N[i.min(12)].fetch_add(1, Relaxed);
        }
    }
    #[inline]
    pub(crate) fn admit() {
        if on() {
            ADMIT.fetch_add(1, Relaxed);
        }
    }
    /// Hooked to the periodic sampler dump, never to a teardown a server boot cannot reach.
    pub fn dump() {
        if !on() {
            return;
        }
        let a = ADMIT.load(Relaxed);
        let tot: u64 = N.iter().map(|c| c.load(Relaxed)).sum::<u64>() + a;
        eprintln!(
            "[PLAN-CENSUS] {tot} plan_meta calls, {a} ADMITTED ({:.2}%)",
            a as f64 * 100.0 / tot.max(1) as f64
        );
        let mut rows: Vec<(u64, &str)> = N
            .iter()
            .zip(NAMES)
            .map(|(c, n)| (c.load(Relaxed), n))
            .filter(|r| r.0 > 0)
            .collect();
        rows.sort_by(|x, y| y.0.cmp(&x.0));
        for (n, name) in rows {
            eprintln!(
                "[PLAN-CENSUS]   {:>6.2}%  {n:>9}  {name}",
                n as f64 * 100.0 / tot.max(1) as f64
            );
        }
    }
}

/// Phase 1 of planning: resolve + admit + force-compile. May run guest code.
/// `n_args` is how many SAM arguments the deferred body will be applied to (see
/// `LaneTask::args`). An impl method's descriptor counts those parameters AFTER the
/// captures, so the arity identity the lane checks is `captures + SAM args == params` — with
/// `n_args` pinned at 0 the lane could only ever admit a `Supplier`, which is why
/// `[PC-DEFER]` read `offered=7` on a boot whose reload is built from one-argument
/// `*Async` bodies.
pub(crate) fn plan_meta(vm: &mut Vm, task: &Value, n_args: usize) -> Option<PlanMeta> {
    if !enabled() || vm.trampoline {
        plan_census::bail(0);
        return None;
    }
    // ARENA TWIN, same reason as `resolve_impl`. The capture COUNT is the only fact this
    // gate takes from the representation, and an arena lambda carries it in its object
    // header: total cells minus the four header slots. `handle` stays 0 for that path;
    // `arena` is what `pack_and_claim` keys on.
    #[cfg(feature = "next-gen-gc")]
    let (handle, arena) = match task {
        Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => (0u32, Some(*p)),
        Value::Ref(h) => (*h, None),
        _ => {
            plan_census::bail(1);
            return None;
        }
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let handle = match task {
        Value::Ref(h) => *h,
        _ => return None,
    };
    #[cfg(feature = "next-gen-gc")]
    let captured = match arena {
        Some(p) => unsafe {
            (crate::nextgen::obj_nfields(p.as_ptr()) as usize)
                .saturating_sub(crate::nextgen::AL_HDR_CELLS)
        },
        None => match vm.heap.lambda_of(handle) {
            Some(l) => l.captured.len(),
            None => {
                plan_census::bail(2);
                return None;
            }
        },
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let captured = vm.heap.lambda_of(handle)?.captured.len();
    let Some(target) = resolve_impl(vm, task) else {
        plan_census::bail(3);
        return None;
    };
    let instance = target.receiver.is_some();
    let (owner, name, desc) = (target.owner, target.name, target.desc);
    let Some(decl) = vm.resolve_method(&owner, &name, &desc).ok().flatten() else {
        plan_census::bail(4);
        return None;
    };
    if !decl.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
        plan_census::bail(5);
        return None;
    }
    let method = decl.file.method(&name, &desc)?;
    if !access_ok(method.access_flags, instance) {
        plan_census::bail(6);
        return None;
    }
    let Some(code) = method.code.as_ref() else {
        plan_census::bail(7);
        return None;
    };
    if !code.exceptions.is_empty() {
        plan_census::bail(8);
        return None; // handler resume mid-unwind is not worker-proven
    }
    let key = code as *const rava_classfile::Code as usize;
    // GILFREE-RINGS: `plannable` — which force-compiles the shared impl — is
    // short-circuited away by the `||` chain in the supplyAsync seam whenever
    // the executor is independently resident-proven, so the impl can still be
    // COLD here. Compile it once under the GIL; without this the lane admits
    // nothing and the phase silently stays serial (a lane that looks armed and
    // moves no time). Cheap after the first of the N tasks sharing the impl.
    if rings() && !matches!(vm.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_))) {
        vm.force_compile(&owner, &name, &desc);
    }
    if rings() {
        prewarm_indy_identities(vm, &decl);
    }
    // A self_reg (deep-IC) body self-registers its cell-root frame into the
    // worker's JitCtx — the same discipline the S2 chunk-fill fork runs on
    // workers (install_worker_jit_ctx supplies that ctx in `run`).
    let Some(st) = vm.jit_cache.get(&key) else {
        plan_census::bail(9);
        return None;
    };
    let (entry, ret) = match st {
        jit::JitStatus::Compiled(function) => (function.raw() as usize, function.ret),
        _ => {
            plan_census::bail(9);
            return None; // not compiled yet → serial tier (later tasks route)
        }
    };
    // An INSTANCE impl's frame opens with `this` in slot 0 (the JVM layout the
    // compiled body was built against: its `aload_0; getfield biomeSource` reads
    // local 0), so prepend a non-wide slot for the captured receiver.
    let Some(mut params) = param_kinds(&desc) else {
        plan_census::bail(10);
        return None;
    };
    if instance {
        params.insert(0, false);
    }
    if params.len() != captured + n_args {
        plan_census::bail(11);
        return None;
    }
    // Structural admission has two deliberately separate modes. The transaction
    // mode is stricter about reads/calls and journals only proven shared field
    // stores; the older purity mode keeps its existing abort-on-shared-store
    // behavior. Both are opt-in measurement gates.
    let field_txn = rings()
        && crate::gilfree_cf::field_txn_enabled()
        && vm.is_field_txn_safe_devirt(&owner, &name, &desc);
    if rings() && crate::gilfree_cf::field_txn_enabled() && !field_txn {
        plan_census::bail(plan_census::PURE_ADMIT);
        return None;
    }
    if rings()
        && crate::gilfree_cf::pure_admit()
        && !field_txn
        && !vm.is_offloadable_devirt(&owner, &name, &desc, false, false)
    {
        plan_census::bail(plan_census::PURE_ADMIT);
        return None;
    }
    plan_census::admit();
    if rings() {
        trace_admit(&owner, &name);
    }
    Some(PlanMeta {
        entry,
        ret,
        ret_desc: desc[desc.find(')').map(|i| i + 1).unwrap_or(0)..].to_string(),
        handle,
        #[cfg(feature = "next-gen-gc")]
        arena,
        params,
        frame_len: usize::from(code.max_locals + code.max_stack).max(1),
        max_locals: usize::from(code.max_locals),
        field_txn,
    })
}

/// Phase 2 of planning: pack the captures into the compiled frame and CLAIM the
/// task's isolated windows. Pure heap reads + bump reservations — it runs no guest
/// code and cannot collect, which is exactly why the coordinator may run it for a
/// whole batch and then park: every packed capture and every carved window is
/// still valid at the join.
pub(crate) fn pack_and_claim(
    vm: &mut Vm,
    meta: PlanMeta,
    budget: &mut usize,
    args: &[Value],
) -> Option<GilfreeCfPlan> {
    // ARENA TWIN: an arena lambda has no heap handle, so its captures come out of the
    // object's own cells (`AL_HDR_CELLS + i`, decoded exactly as the interpreter reads
    // them). Materialised into a Vec here so the packing loop below is identical for
    // both representations — this runs once per task on the coordinator, not per call.
    #[cfg(feature = "next-gen-gc")]
    let captured: Vec<Value> = match meta.arena {
        Some(p) => (0..meta.params.len())
            .map(|i| {
                let (c, k) = crate::nextgen::arena_lambda_cell(p, crate::nextgen::AL_HDR_CELLS + i);
                crate::opcodes::decode_arena_cell(c, k)
            })
            .collect(),
        None => vm.heap.lambda_of(meta.handle)?.captured.clone(),
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let captured: Vec<Value> = vm.heap.lambda_of(meta.handle)?.captured.clone();
    if captured.len() + args.len() != meta.params.len() {
        return None;
    }
    // Pack the captured args as the compiled frame's leading local cells, laid
    // out per the impl descriptor (wide args take 2 slots). A capture that has
    // no flat cell form (Str / sentinels — the transient box table is
    // per-THREAD, a worker could not read main's indices) rejects the plan.
    let mut cells = vec![0i64; meta.frame_len];
    let mut slot = 0usize;
    // Captures first, then the SAM arguments — the order `invoke_lambda_parts` uses when it
    // builds the same call vector on the serial path, so a task that runs in the region and
    // the same task re-run serially after an abort see identical locals.
    for (v, wide) in captured.iter().chain(args.iter()).zip(meta.params.iter()) {
        if matches!(v, Value::Str(_) | Value::StdOut | Value::StdErr) {
            return None;
        }
        if slot >= cells.len() {
            return None;
        }
        cells[slot] = jit::pack_local(v);
        slot += if *wide { 2 } else { 1 };
    }
    if slot > meta.max_locals {
        return None;
    }
    let slice = nursery_slice();
    let (ybase, ytop, ycap) = crate::nextgen::young_extent();
    // NURSERY BUDGET, not a soundness refusal: every claimed task carves its own slice
    // out of main's young region, so a batch can only claim `slice_budget / slice`
    // tasks however sound the rest are. `budget` is the coordinator's pre-computed
    // slice allowance (young headroom minus main's reserve minus the shared overflow
    // pool); exceeding it is counted separately because it is indistinguishable in the
    // batch ledger from a real admission refusal, yet the fix is completely different
    // (grow the nursery / resize the slice, versus widen admission).
    if *budget < slice || ycap.saturating_sub(ytop) < slice + MAIN_RESERVE {
        CLAIM_NURSERY_FULL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return None; // keep headroom for the main thread
    }
    crate::nextgen::young_advance_top(slice);
    // Boxed-field slice for the worker's own closures (see FIELD_BOX_SLOTS). A
    // refusal here is not fatal: the range reads as exhausted on the worker, so a
    // body that needs a box aborts to the resident tier instead of racing.
    let (fbbase, fblen) = match vm.heap.reserve_field_box_range(field_box_slots()) {
        Some(base) => (base, field_box_slots() as u32),
        None => (0, 0),
    };
    *budget -= slice;
    Some(GilfreeCfPlan {
        entry: meta.entry,
        cells,
        ret: meta.ret,
        ret_desc: meta.ret_desc,
        nbase: ybase + ytop,
        nlen: slice,
        obase: vm.heap.reserve_obj_range(obj_slots()),
        olen: obj_slots() as u32,
        fbbase,
        fblen,
        field_txn: meta.field_txn,
    })
}

/// Pre-intern, UNDER THE GIL, the impl identity of every LambdaMetafactory
/// bootstrap this class declares.
///
/// A worker may resolve an `invokedynamic` site itself only while every string the
/// resolution needs is already in the shared class pool — an interner MISS is an
/// insert, the exact publication `worker_indy_refusal` denies. Method names
/// (`lambda$foo$3`) and descriptors are interned nowhere else in a boot, so the
/// FIRST execution of a site is always a miss, and without this pre-warm the lane
/// denies at indy forever ("impl name uninterned"). Reading the bootstrap-method
/// table covers every site in the class without a bytecode walk; sites in CALLEE
/// classes stay unwarmed and keep denying (a named, incremental frontier).
/// The class's identity is its bootstrap table, so this is ONCE PER CLASS, not
/// once per task: `prepare` runs for every admitted task, and re-walking the table
/// each time cost +16s of ChunkFill SETUP when it was unmemoised.
pub(crate) fn prewarm_indy_identities(vm: &mut Vm, class: &crate::class_loader::LoadedClass) {
    // `FastSet`, not `HashSet`: the default hasher is SipHash with a random seed, and this
    // memo is consulted for every admitted task. The product boot's profile showed
    // `std::hash::random` + `hash_one<RandomState>` as a 2.3% cluster in front of a set whose
    // keys are POINTERS.
    pub(crate) static SEEN: std::sync::OnceLock<std::sync::Mutex<crate::FastSet<usize>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(crate::FastSet::default()));
    {
        let mut s = seen.lock().unwrap_or_else(|e| e.into_inner());
        if !s.insert(class as *const crate::class_loader::LoadedClass as usize) {
            return;
        }
    }
    // `class` is borrowed from the caller's owned `Arc`, not from `vm`, so the pool
    // reads and the interner writes can interleave without a staging buffer.
    for bm in &class.file.bootstrap_methods {
        let bootstrap_is_metafactory = class
            .file
            .pool
            .method_handle(bm.handle_index)
            .is_some_and(|h| h.owner == "java/lang/invoke/LambdaMetafactory");
        if !bootstrap_is_metafactory {
            continue;
        }
        let Some(&mh) = bm.args.get(1) else { continue };
        let Some((_, m)) = class.file.pool.method_handle_full(mh) else {
            continue;
        };
        vm.heap.intern_class(m.owner);
        vm.heap.intern_class(m.name);
        vm.heap.intern_class(m.descriptor);
    }
}

/// `true` per REFERENCE-or-primitive param, `wide` flag per slot layout; None
/// on a malformed descriptor.
pub(crate) fn param_kinds(desc: &str) -> Option<Vec<bool>> {
    let b = desc.as_bytes();
    let mut i = desc.find('(')? + 1;
    let mut out = Vec::new();
    while i < b.len() && b[i] != b')' {
        while i < b.len() && b[i] == b'[' {
            i += 1;
        }
        match b.get(i)? {
            b'L' => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1;
                out.push(false);
            }
            b'J' | b'D' => {
                i += 1;
                out.push(true);
            }
            _ => {
                i += 1;
                out.push(false);
            }
        }
    }
    Some(out)
}
