//! Field access from the interpreter, including static resolution.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

#[allow(invalid_reference_casting)]
pub(crate) fn getstatic_resolve(
    vm: &crate::vm::thread_context::SharedVm,
    owner: &str,
    name: &str,
    _desc: &str,
) -> Result<Value, VmError> {
    match vm.loader.get_or_load(owner) {
        Ok(class) => {
            // ⭐**THIS RESOLVER IS THE PERMANENTLY-REFUSED POPULATION'S ONLY PATH.** A getstatic
            // site the per-site binding can serve never arrives here; what does arrive is the
            // shapes the binding deliberately refuses — an INHERITED static, an interface
            // constant, a natively-served owner — and those take this entire body on EVERY read,
            // forever. So each unconditional re-derivation in it is paid per access, not per site.
            // The first of them: an out-of-line `ensure_init` call plus a SeqCst probe, to re-ask
            // a question that latched permanently the first time. One acquire load answers it.
            unsafe {
                &mut *(vm as *const crate::vm::thread_context::SharedVm
                    as *mut crate::vm::thread_context::SharedVm)
            }
            .ensure_init_handoff(&class)?;
            // 3c-PARITY PROBE (RAVA_PROBE_CLINIT): reading a static while the class is
            // initialized-latched but its <clinit> has NOT completed = the stackful-only
            // null-static window (ensure_init early-returns on the latch without running
            // the deferred trampolined clinit). Log the exact culprit class + field.
            if vm.trampoline
                && class.initialized.load(std::sync::atomic::Ordering::Relaxed)
                && !class.clinit_done.load(std::sync::atomic::Ordering::Relaxed)
                && std::env::var_os("RAVA_PROBE_CLINIT").is_some()
            {
                eprintln!("[CLINIT-WINDOW] getstatic {owner}.{name} read while <clinit> PENDING (initialized-latched, clinit_done=false)");
            }
            // Static fields are inherited: if the named class doesn't hold it,
            // walk the superclass chain to the declaring class (JVMS 5.4.3.2).
            // ONE lookup, not two. This ran `contains_key` and then `get` — the same key hashed
            // and probed twice on the hot getstatic path, and `statics` is keyed by String.
            if let Some(value) = class
                .statics
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&*name)
                .cloned()
            {
                return Ok(field_or_default(value, _desc));
            }
            // If the owner DECLARES this static (JLS 8.3) but it isn't set yet,
            // it's the owner's own distinct field at its default — do NOT inherit
            // a same-named superclass static.
            // THE ONE-BIT TEST FIRST. This scan is O(fields declared by the owner) on every
            // access, and it asked the EXPENSIVE half first: a name compare against every field
            // in the table, instance fields included — and in a Minecraft class those are the
            // large majority, none of which can ever satisfy the ACC_STATIC half. Testing the bit
            // first turns each of them from a string compare into one `and`. Same conjunction,
            // same answer, no reordering of anything observable (both operands are pure reads of
            // immutable classfile data).
            if class
                .file
                .fields
                .iter()
                .any(|f| f.access_flags & 0x0008 != 0 && f.name.as_str() == &*name)
            {
                return Ok(field_or_default(Value::Null, _desc));
            }
            // ONE super-chain lookup, not two. The walk below and the interface-closure seed
            // further down each called `vm.super_chain(owner)` — the same owner-name hash into the
            // loader's cache, twice per access, for an `Arc` the loader hands back by clone anyway.
            // Holding it borrows nothing from `vm`, so the `&mut vm` calls inside are unaffected.
            let chain = vm.super_chain(owner);
            // ⭐**AND ONE LOADER PROBE PER ANCESTOR, NOT TWO.** The interface-closure seed used to
            // re-`get_or_load` EVERY class this walk had just resolved, purely to read
            // `file.interfaces` back off it — a second `RwLock` read, a second hash of a name like
            // `net/minecraft/world/level/block/state/BlockBehaviour$BlockStateBase`, and a second
            // `Arc<LoadedClass>` clone/drop pair, per ancestor, per access. The quantity that
            // grows is superclass-chain depth, and this function's own header comment says the
            // refused population takes this whole body on EVERY read, forever.
            //     was: O(chain depth) loader probes, twice.  is: O(chain depth), once.
            //
            // Collected only AFTER the statics probe has missed on that class, so the commonest
            // refused shape — an inherited static found in a direct super — still allocates
            // nothing at all and returns from exactly where it did before.
            let mut walked: Vec<Arc<crate::class_loader::LoadedClass>> = Vec::new();
            for cn in chain.iter() {
                if cn.as_ref() == &*owner {
                    continue;
                }
                if let Ok(c) = vm.loader.get_or_load(cn) {
                    unsafe {
                        &mut *(vm as *const crate::vm::thread_context::SharedVm
                            as *mut crate::vm::thread_context::SharedVm)
                    }
                    .ensure_init_handoff(&c)?;
                    if let Some(v) = c
                        .statics
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&*name)
                        .cloned()
                    {
                        return Ok(field_or_default(v, _desc));
                    }
                    walked.push(c);
                }
            }
            // Fields can also be declared as interface constants (e.g. an
            // `interface Type { Map TYPES = new HashMap(); }` shared by an enum
            // implementing it). Walk the transitive interface closure, running
            // each interface's <clinit> so its constants are populated.
            //
            // SAME SEED, SAME ORDER. `chain[0]` IS the owner (`Loader::super_chain` seeds its
            // result with `start`) and the owner's `LoadedClass` is the `class` already in hand,
            // so seeding from `class` and then from the ancestors the walk resolved reproduces
            // the old second pass over `chain` element for element.
            let mut seen: Vec<String> = Vec::new();
            let mut queue: Vec<String> = class.file.interfaces.clone();
            for c in &walked {
                queue.extend(c.file.interfaces.iter().cloned());
            }
            while let Some(iname) = queue.pop() {
                if let Ok(ic) = vm.loader.get_or_load(&iname) {
                    unsafe {
                        &mut *(vm as *const crate::vm::thread_context::SharedVm
                            as *mut crate::vm::thread_context::SharedVm)
                    }
                    .ensure_init_handoff(&ic)?;
                    if let Some(v) = ic
                        .statics
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&*name)
                        .cloned()
                    {
                        return Ok(field_or_default(v, _desc));
                    }
                    // FILTERED AT THE PUSH, NOT ONLY AT THE POP. An interface graph is a diamond —
                    // every Mojang interface extends the same two or three bases — so most EDGES
                    // name a node this walk has already visited, and each of them was cloned into
                    // a fresh `String` just to be recognised and dropped at the pop. The scan is
                    // the one the pop performs anyway, moved a step earlier; a name skipped here
                    // would have been skipped there, so the closure visited is unchanged.
                    for i in ic.file.interfaces.iter() {
                        if !seen.iter().any(|s| s == i) {
                            queue.push(i.clone());
                        }
                    }
                }
                // MOVED, NOT CLONED. `iname` is owned here and dead after this push, and the
                // historic line cloned it — one `String` allocation per distinct interface per
                // access, for a name the loop is about to drop anyway.
                seen.push(iname);
            }
            Ok(field_or_default(Value::Null, _desc))
        }
        // A WORKER WAS REFUSED THE LOAD. The class may be perfectly present; this thread is just
        // not allowed to load it, and the region has already been aborted. Unwind as an abort —
        // `RegionAbort` is deliberately not a `Throw`, so no guest `catch` can swallow it and the
        // coordinator retries the region serially. Reading this as "no bytecode in the jar" is
        // what turned a routine worker miss into a fatal UnsupportedNative.
        #[cfg(feature = "next-gen-gc")]
        Err(crate::LoadError::WorkerRefused(_)) => Err(VmError::RegionAbort),
        #[cfg(not(feature = "next-gen-gc"))]
        Err(crate::LoadError::WorkerRefused(n)) => {
            Err(VmError::Load(crate::LoadError::NotFound(n)))
        }
        // JDK class with no bytecode in the jar — try the native static fields.
        Err(crate::LoadError::NotFound(_)) => {
            let vm_mut = unsafe {
                &mut *(vm as *const crate::vm::thread_context::SharedVm
                    as *mut crate::vm::thread_context::SharedVm)
            };
            crate::natives::get_static(vm_mut, owner, name)
                .ok_or_else(|| VmError::UnsupportedNative(format!("getstatic {owner}.{name}")))
        }
        Err(e) => Err(e.into()),
    }
}

/// ⭐**ONE ACQUIRE LOAD FOR EVERY PROCESS-CONSTANT DIAGNOSTIC SWITCH ON THE FIELD OPCODES**
/// (`RAVA_NO_FAST_FIELDOPS=1` restores the historic per-switch reads; also stands down with the
/// block switch).
///
/// `getfield` and `putfield` are the two highest-traffic opcodes in the engine — row 349 puts
/// `getfield` alone at 23.8% of 1.29 billion hot-run exits — and each of them opened with a RUN of
/// independent `OnceLock<bool>` probes for shape/biome/stale-pointer diagnostics that are off in
/// every shipped configuration: `m3shape::enabled()` in both, `m3biome::enabled()` again in
/// `putfield`, and `trace_stalefield()` a third time inside the arena arm of each read. A
/// `OnceLock` read is an ACQUIRE load, and the cost is not the `mov` — it is that LLVM may neither
/// hoist it out of the loop nor fold two of them together, which is the shape this tree already
/// measured at -15% when one relaxed atomic came off the interpreter's push path, and the shape
/// `tlcache::cflags` records for the dispatch probes ("four switches, one plain load").
///
/// **Previously:** one acquire-ordered switch read per diagnostic family per field access (two in
/// an arena `getfield`, two in `putfield`, two in `object_field_by_id`). **Now:** one read, hoisted
/// into a local and shared by all of them.
///
/// What grows is field accesses per boot. No table — this is a single `bool`.
///
/// THE KILL SWITCH LIVES INSIDE THE ANSWER, not beside it, so restoring the historic behaviour
/// costs the fast path nothing: with `RAVA_NO_FAST_FIELDOPS` set (or `emit_more_disabled`) this
/// reports ARMED, and every site below then consults exactly the individual switches it always
/// did, in the order it always did.
#[allow(dead_code)] // every consumer is `next-gen-gc`-gated
fn field_diag_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        #[cfg(feature = "next-gen-gc")]
        let shape = crate::m3shape::enabled() || crate::m3biome::enabled();
        #[cfg(not(feature = "next-gen-gc"))]
        let shape = false;
        shape
            || trace_stalefield()
            || std::env::var_os("RAVA_NO_FAST_FIELDOPS").is_some()
            || crate::flags::emit_more_disabled()
    })
}

/// Is this thread redirecting field writes into the COW shadow right now?
///
/// `parallel::cow_active()` is `cow_gate() && gilfree()`, and `cow_gate()` is DEFAULT-ON — so the
/// acquire-ordered `OnceLock` read was evaluated on every field access on every thread, only to be
/// ANDed with a per-thread flag that is `false` for the entire serial boot. The gate is a process
/// constant, so `A && B` here commutes exactly (`gilfree() && CONST == CONST && gilfree()`);
/// asking the plain thread-local `Cell` first means the main thread never touches the barrier.
/// Not a behaviour change and deliberately not switched: it is the same conjunction.
#[inline(always)]
fn cow_redirecting() -> bool {
    crate::parallel::gilfree() && crate::parallel::cow_gate()
}

pub(crate) fn getfield(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    index: u16,
) -> Result<(), VmError> {
    let recv = frame.pop()?;
    // SAFETY: `frame.code` points inside the class this frame holds an `Arc` to (see `Frame`).
    let bc: &[u8] = unsafe { &(*frame.code).bytecode };
    let v = getfield_value(vm, &frame.class, index, recv, bc, frame.pc)?;
    frame.push(v);
    Ok(())
}

/// `getfield` WITHOUT a `Frame` — the opcode's body, taking the receiver and returning the value.
///
/// ⭐**Ledger row 349.** `run_hot` returns to `run` for every opcode it does not own, and
/// `getfield` ends **306,233,425 of 1,286,515,599 hot-run exits — 23.8%**, the single largest.
/// It cannot be served inside the loop while its body needs `&mut Frame`, because `run_hot` holds
/// the frame destructured into disjoint borrows (`pc`, `locals`, `stack`, `class`) and cannot
/// produce one. Everything the body actually used from the frame is passed instead: the class (for
/// the site key and the constant pool), the bytecode and the pc (for the NPE source), and the
/// receiver. **Three separate levers block on this decoupling** — see `docs/interpreter-program.md`.
#[allow(invalid_reference_casting)]
pub(crate) fn getfield_value(
    vm: &crate::vm::thread_context::SharedVm,
    frame_class: &Arc<crate::class_loader::LoadedClass>,
    index: u16,
    recv: Value,
    bc: &[u8],
    pc: usize,
) -> Result<Value, VmError> {
    // B5: a PINNED pointer receiver is a handle object — normalize to Ref(h) so
    // the whole handle path (mirror-served reads, synthetic fields) applies.
    #[cfg(feature = "next-gen-gc")]
    let recv = match recv {
        Value::Obj(p) => match crate::nextgen::pinned_handle_of(p.addr() as u64) {
            Some(h) => Value::Ref(h),
            None => Value::Obj(p),
        },
        other => other,
    };
    // Every process-constant diagnostic switch this body consults, read ONCE (see
    // `field_diag_armed`) — the individual switches are still what decides each probe, they are
    // just not re-read through an acquire load apiece on the engine's hottest opcode.
    #[cfg(feature = "next-gen-gc")]
    let diag = field_diag_armed();
    // M3 9.2a: classify this read against the initCache-in-progress state (read-only probe).
    #[cfg(feature = "next-gen-gc")]
    if diag && crate::m3shape::enabled() {
        if let Value::Obj(p) = &recv {
            crate::m3shape::note_read(p.addr(), crate::nextgen::arena_class_id(*p));
        }
    }
    // Resolve (field id, slot hint) from the call-site cache; on a miss intern
    // the name once. Skips the field-name hash on every repeat access.
    let site = (Arc::as_ptr(frame_class) as usize, index);
    let (id, hint) = match crate::tlcache::field_get(site) {
        Some(pair) => pair,
        None => {
            let (owner, name, _desc) = ref_parts_of(frame_class, index)?;
            let id = vm.resolve_field_id(&owner, &name);
            crate::tlcache::field_put(site, id, 0);
            (id, 0)
        }
    };
    // M3 COW ISOLATION: a gil-free worker reads its own shadowed write first; a miss falls
    // through to the shared (read-only during phase B) cell below. Default-off.
    if cow_redirecting()
        && !((crate::parallel::cow_owned_bypass() || crate::parallel::cow_owned_undo())
            && crate::parallel::cow_owned(&recv))
    {
        if let Some(key) = crate::parallel::cow_obj_key(&recv) {
            if let Some(sv) = crate::parallel::shadow_get_field(key, id) {
                return Ok(sv); // caller (0xb4 dispatch) advances the pc
            }
        }
    }
    let v = match recv {
        Value::Ref(h) => {
            let (v, slot) = vm.heap.get_field_hint(h, id, hint);
            if slot != hint {
                crate::tlcache::field_put(site, id, slot);
            }
            // Default-by-type only matters for an unset (Null) field.
            if matches!(v, Value::Null) {
                // BORROWED, NOT CLONED. `ref_parts_of` clones all THREE `Arc<str>` of the site —
                // six atomic refcount round-trips on one shared cache line — and this arm reads
                // exactly one of them. `ref_site_ref` is the same two loads and a null test that
                // built them, minus the counting; the borrow lives on `frame_class`, which is
                // already shared here. Same argument `getstatic` records for the identical swap.
                field_or_default(Value::Null, &ref_site_ref(frame_class, index)?.descriptor)
            } else {
                v
            }
        }
        // DUAL DISPATCH (next-gen-gc): an arena object derefs straight to its
        // inline cells under the GIL safepoint.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            // RAVA_TRACE_STALEFIELD — a stale/un-forwarded arena receiver decodes to
            // an invalid class id (PHASE5 panic). Surface the constant-pool field
            // name + receiver ptr + backtrace BEFORE the panic, so the stale-receiver
            // provenance is identifiable (id alone is opaque).
            if diag
                && trace_stalefield()
                && crate::nextgen::class_name(crate::nextgen::arena_class_id(p)).is_none()
            {
                let (owner, name, _d) = ref_parts_of(frame_class, index)?;
                eprintln!(
                    "[STALEFIELD] getfield {owner}.{name} (id={id}) on stale arena obj {:#x}\n{}",
                    p.addr(),
                    vm.frame_trace()
                );
                // Forensic: who STILL holds this stale pointer (the reference the
                // collector never forwarded)? Scan heap structures + every registered
                // interpreter frame's locals/stack.
                let addr = p.addr();
                let mut holders = vm.heap.find_arena_holders(addr);
                for &fp in &crate::frames::all() {
                    let f = unsafe { &*(fp as *const Frame) };
                    for (i, v) in f.locals().iter().enumerate() {
                        if matches!(v, Value::Obj(q) if q.addr() == addr) {
                            holders.push(format!("frame {} local[{i}]", f.class.file.this_class));
                        }
                    }
                    for (i, v) in f.stack().iter().enumerate() {
                        if matches!(v, Value::Obj(q) if q.addr() == addr) {
                            holders.push(format!("frame {} stack[{i}]", f.class.file.this_class));
                        }
                    }
                }
                eprintln!("[STALE-HOLDERS] {} holder(s) of {addr:#x}:", holders.len());
                for h in &holders {
                    eprintln!("    {h}");
                }
            }
            {
                arena_cells::note_gf_interp();
                arena_getfield(p, id)
            }
        }
        // A getfield on null is a catchable NullPointerException on every JVM, and it already is
        // one both in compiled code (jit/callbacks/fields.rs sets a pending NPE) and in putfield
        // twenty lines below. Only the interpreted getfield killed the process — the highest-
        Value::Null => {
            let (_o, name, _d) = ref_parts_of(frame_class, index)?;
            let src = crate::npe::source_at(bc, pc, 0, &frame_class.file.pool);
            let npe = vm.make_exception_shared(
                "java/lang/NullPointerException",
                &crate::npe::read_field(&name, src),
            );
            return Err(VmError::Throw(npe));
        }
        _ => {
            let (_o, name, _d) = ref_parts_of(frame_class, index)?;
            let npe = vm.make_exception_shared("java/lang/NullPointerException", &name);
            return Err(VmError::Throw(npe));
        }
    };
    Ok(v)
}

/// `putfield`, with the receiver and value already popped.
///
/// ⭐**EXTRACTED FOR THE SAME REASON `getfield_value` WAS** (row 350): the hot interpreter loop
/// cannot run an opcode whose implementation needs a `&mut Frame`, so every `putfield` EXITED the
/// loop to the cold path. Row 378 measures what that costs on `bench/Allocs.java`: **62,000,000 of
/// 124,000,057 hot-run exits — exactly half — are `0xb5`**, and the loop survives only **6.63
/// opcodes per entry**.
///
/// The caller pops in JVMS order (value on top, then objectref) and passes both; `bc`/`pc` are the
/// body and offset of THIS instruction, used only to name the source expression in a
/// NullPointerException message.
pub(crate) fn putfield_value(
    vm: &crate::vm::thread_context::SharedVm,
    frame_class: &Arc<crate::class_loader::LoadedClass>,
    index: u16,
    recv: Value,
    value: Value,
    bc: &[u8],
    pc: usize,
) -> Result<(), VmError> {
    // Diagnostic: a double field receiving an abnormally large value (coordinate /
    // velocity blow-up). Print the field + the call stack that produced it, so the
    // generic compute bug behind the player-tick X,Z explosion is visible.
    if vm.trace.big {
        if let Value::Double(d) = value {
            if d.abs() > 1.0e7 {
                let (owner, name, _) = ref_parts_of(frame_class, index)?;
                eprintln!("[BIG] {owner}.{name} = {d}\n{}", vm.backtrace());
            }
        }
    }
    // B5: a PINNED pointer receiver is a handle object — normalize to Ref(h) so
    // the write goes through put_field* (FieldBlob authoritative + mirror sync).
    #[cfg(feature = "next-gen-gc")]
    let recv = match recv {
        Value::Obj(p) => match crate::nextgen::pinned_handle_of(p.addr() as u64) {
            Some(h) => Value::Ref(h),
            None => Value::Obj(p),
        },
        other => other,
    };
    // BOTH SHAPE PROBES BEHIND ONE SWITCH READ (see `field_diag_armed`): the two `enabled()` calls
    // are two acquire loads on the second-hottest opcode in the engine, for two diagnostics that
    // are off together in every shipped configuration. Each still decides its own probe.
    #[cfg(feature = "next-gen-gc")]
    if field_diag_armed() {
        // M3 9.2a: classify this write against the initCache-in-progress state (read-only probe).
        if crate::m3shape::enabled() {
            if let Value::Obj(p) = &recv {
                crate::m3shape::note_write(p.addr(), crate::nextgen::arena_class_id(*p));
            }
        }
        // M3 9.2d.1/.2: classify this write vs the pre-built shared RTree/Climate state + record its type.
        if crate::m3biome::enabled() {
            match &recv {
                Value::Obj(p) => {
                    let cn = crate::nextgen::class_name(crate::nextgen::arena_class_id(*p));
                    crate::m3biome::note_write(p.addr(), false, cn.as_deref().unwrap_or("?"));
                }
                Value::Ref(h) => {
                    let ty = vm.heap.class_of(*h).unwrap_or("?");
                    crate::m3biome::note_write(crate::heap::obj_gen(*h), true, ty);
                }
                _ => {}
            }
        }
    }
    let site = (Arc::as_ptr(frame_class) as usize, index);
    let cached = crate::tlcache::field_get(site);
    #[cfg(feature = "next-gen-gc")]
    if cached.is_none() && crate::gilfree_cf::field_txn::active() {
        // Interpreter-side field resolution can load classes and mutate shared
        // loader metadata. A cold worker site is not transaction-safe.
        crate::parallel::request_abort();
        return Err(VmError::RegionAbort);
    }
    let (id, hint) = match cached {
        Some(pair) => pair,
        None => {
            let (owner, name, _desc) = ref_parts_of(frame_class, index)?;
            let id = vm.resolve_field_id(&owner, &name);
            crate::tlcache::field_put(site, id, 0);
            (id, 0)
        }
    };
    // A worker-owned receiver is private to this task and may be stored directly.
    // Shared writes are journalled only in the explicitly enabled write-only
    // transaction; otherwise they abort so serial fallback preserves the write.
    // Cold resolution above also aborts before touching shared loader metadata.
    #[cfg(feature = "next-gen-gc")]
    if crate::gilfree_cf::in_lane() && !crate::parallel::cow_owned(&recv) {
        let recorded = if crate::gilfree_cf::field_txn::active() {
            match &recv {
                Value::Obj(p) => {
                    crate::nextgen::slot_kind_of(crate::nextgen::arena_class_id(*p), id)
                        .is_some_and(|(_, kind)| {
                            crate::gilfree_cf::field_txn::FieldLayout::from_kind(kind).is_some_and(
                                |layout| {
                                    crate::gilfree_cf::field_txn::record(
                                        *p,
                                        id,
                                        layout,
                                        value.clone(),
                                    )
                                },
                            )
                        })
                }
                _ => false,
            }
        } else {
            false
        };
        if recorded {
            return Ok(());
        }
        crate::gilfree_cf::abort_census::note(vm, &recv);
        crate::gilfree_cf::ABORT_SHARED_FIELD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::parallel::request_abort();
        return Err(VmError::RegionAbort);
    }
    // M3 COW ISOLATION: a gil-free worker's putfield goes to its thread-local shadow, NOT
    // the shared object — so an impure fork body doesn't race/publish. (Null still NPEs
    // below; non-object receivers fall through.) Default-off (byte-identical).
    if cow_redirecting() {
        // OWNED-UNDO: direct write + old-value log (rolled back per element).
        if crate::parallel::cow_owned_undo() && crate::parallel::cow_owned(&recv) {
            if let Value::Obj(p) = recv {
                let old = arena_getfield(p, id);
                crate::parallel::undo_note(p.addr(), id, old);
                arena_putfield(p, id, &value);
                return Ok(());
            }
        }
        if !(crate::parallel::cow_owned_bypass() && crate::parallel::cow_owned(&recv)) {
            if let Some(key) = crate::parallel::cow_obj_key(&recv) {
                crate::parallel::shadow_put_field(key, id, value);
                return Ok(()); // caller (0xb5 dispatch) advances the pc
            }
        }
    }
    match recv {
        Value::Ref(h) => {
            let slot = vm.heap.put_field_hint(h, id, value, hint);
            if slot != hint {
                crate::tlcache::field_put(site, id, slot);
            }
        }
        // DUAL DISPATCH (next-gen-gc): write straight to the arena object's cell.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => arena_putfield(p, id, &value),
        Value::Null => {
            let (_o, name, _d) = ref_parts_of(frame_class, index)?;
            // putfield's stack is [objectref, value], so the value sits above the null.
            let src = crate::npe::source_at(bc, pc, 1, &frame_class.file.pool);
            let npe = vm.make_exception_shared(
                "java/lang/NullPointerException",
                &crate::npe::assign_field(&name, src),
            );
            return Err(VmError::Throw(npe));
        }
        other => {
            let (_o, name, _desc) = ref_parts_of(frame_class, index)?;
            if crate::env_flag!("RAVA_TRACE_THROW") {
                eprintln!(
                    "[BADPUT] putfield {name} desc={_desc} recv={}\n{}",
                    other.display(),
                    vm.backtrace()
                );
            }
            return Err(VmError::UnsupportedNative(format!(
                "putfield {name} on non-object"
            )));
        }
    }
    Ok(())
}
/// The framed entry point: pop value then objectref (JVMS order) and delegate.
pub(crate) fn putfield(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    index: u16,
) -> Result<(), VmError> {
    let value = frame.pop()?;
    let recv = frame.pop()?;
    let class = Arc::clone(&frame.class);
    // SAFETY: the `Code` lives in the class the frame holds an `Arc` to, and the borrow ends
    // before `frame` is touched again — the same argument `run` makes for its own `code` borrow.
    let (bc, pc) = (unsafe { &(*frame.code).bytecode }, frame.pc);
    putfield_value(vm, &class, index, recv, value, bc, pc)
}

/// Read a field (by interned id) from an object value — handle or arena pointer.
pub(crate) fn object_field_by_id(vm: &mut Vm, v: &Value, id: u32) -> Value {
    match v {
        Value::Ref(h) => vm.heap.get_field_hint(*h, id, 0).0,
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            // STALEFIELD forensic (record-accessor path — e.g. ResourceLocation.getPath()
            // is a record component read that routes here, NOT through the getfield op).
            // Name who still holds the un-forwarded stale arena pointer (the WRITEBACK-MISS
            // location) before arena_getfield panics on `class None`.
            // ONE SWITCH, NOT A SECOND COPY OF IT. This read the SAME env var as
            // `opcodes::trace_stalefield` through a private `OnceLock` of its own, and reached it
            // via `jit::env_gate`, which is TWO acquire loads (its own memo switch, then the
            // cell). `object_field_by_id` is the record-component read path — one call per
            // component per synthesised `toString`/`hashCode`/`equals` — so that was two barriers
            // per component to ask a question with one process-wide answer that the sibling
            // opcode bodies already hold. Same variable, same predicate (`var_os(..).is_some()`),
            // one gate.
            if field_diag_armed()
                && trace_stalefield()
                && crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).is_none()
            {
                let addr = p.addr();
                let mut holders = vm.heap.find_arena_holders(addr);
                for &fp in &crate::frames::all() {
                    let f = unsafe { &*(fp as *const Frame) };
                    for (i, lv) in f.locals().iter().enumerate() {
                        if matches!(lv, Value::Obj(q) if q.addr() == addr) {
                            holders.push(format!("frame {} local[{i}]", f.class.file.this_class));
                        }
                    }
                    for (i, sv) in f.stack().iter().enumerate() {
                        if matches!(sv, Value::Obj(q) if q.addr() == addr) {
                            holders.push(format!("frame {} stack[{i}]", f.class.file.this_class));
                        }
                    }
                }
                eprintln!(
                    "[STALEFIELD-REC] object_field_by_id id={id} on stale arena obj {addr:#x}"
                );
                eprintln!("[STALE-HOLDERS] {} holder(s) of {addr:#x}:", holders.len());
                for h in &holders {
                    eprintln!("    {h}");
                }
            }
            {
                arena_cells::note_gf_interp();
                arena_getfield(*p, id)
            }
        }
        _ => Value::Null,
    }
}
