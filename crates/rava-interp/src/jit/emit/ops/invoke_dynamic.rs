//! Invokedynamic and interface invocation.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn invoke_dynamic(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let vars = o.vars;
    let bc = o.bc;
    let pool = o.pool;
    let env = o.env;
    let array_desc_cell = o.array_desc_cell;
    let locals_ptr = o.locals_ptr;
    let cow = o.cow;
    #[cfg(feature = "next-gen-gc")]
    let (cursor_var, licm, dv) = (o.cursor_var, o.licm, o.dv);
    match op {
        0xba => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let desc = pool.invoke_dynamic(idx).map(|(_, _, d)| d.to_string())?;
            let n = crate::count_args(&desc);
            // STRUCTURAL HOF FUSION: erase the non-escaping lambda object and
            // inline its adjacent static consumer. The consumer's sole proven
            // interface site is rebound to the exact static implementation.
            if let Some(plan) = env.hof_fusions.get(&pc) {
                debug_assert_eq!(plan.indy_pc, pc);
                if plan.capture_count != n {
                    return None;
                }
                let mut captures = Vec::with_capacity(n);
                for _ in 0..n {
                    captures.push(stack.pop()?);
                }
                captures.reverse();
                let marker = b.ins().iconst(I64, 0);
                push(stack, marker, JT::Ref);
                let binding = ActiveSamBinding {
                    owner_code_key: plan.consumer.code_key,
                    sam_call_pc: plan.sam_call_pc,
                    captures: &captures,
                    implementation: &plan.implementation,
                };
                emit_inline(
                    b,
                    stack,
                    env,
                    array_desc_cell,
                    &plan.consumer,
                    locals_ptr,
                    cow,
                    licm,
                    dv,
                    Some(binding),
                    vars,
                )?;
                if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                return Some(());
            }
            // INLINE CONCAT: an all-int/ASCII concat shape assembles its ArenaString
            // inline (native itoa + TLAB bump), no `rava_indy` callback.
            if !arena_inline_disabled() {
                if let (Some(cv), Some(shape)) = (cursor_var, env.concat_shapes.get(&idx)) {
                    let mut args: Vec<SV> = Vec::with_capacity(n);
                    for _ in 0..n {
                        args.push(stack.pop()?);
                    }
                    args.reverse(); // push order: args[0] = first pushed
                    emit_inline_concat(b, env, stack, vars, locals_ptr, cv, shape, &args, idx);
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    return Some(());
                }
            }
            // INLINE LAMBDA MINT: a capturing closure is a TLAB bump plus its header and
            // capture cells — the same shape as the concat arm above, for the group the region
            // profile prices at 7.4% of the compiled region (2,537,018 assemblies a run, of which
            // only 6,188 capture nothing). The arm refuses whenever the site has not been
            // described yet, the recorded shape disagrees with the descriptor, or a capture cell
            // carries a tag it cannot re-encode — and takes the identical callback.
            // DEFAULT ON; `RAVA_NO_INLINE_LAMBDA=1` disables the arm AND its recorder together.
            //
            // ⚠**REACH IS BOUNDED BY `cursor_var`, WHICH THIS FILE DOES NOT DECIDE.** `body.rs`
            // creates the TLAB cursor only for methods with an arena `new` or a concat shape, so
            // a method whose ONLY allocation is a closure still takes the callback. That gate
            // lives in `emit::body` and is deliberately left alone here.
            //
            // ⛔**RE-CHECKED AS A PERMANENT EMIT-TIME REFUSAL, AND IT STILL HOLDS.** The obvious
            // cure — call `env.cursor_addr_fn` at THIS site when `cursor_var` is `None`, the way
            // `emit_inline_arena_new_late` already fetches per site — is the one widening the tree
            // has already tried and recorded: `body.rs`'s late-`new` hoist carries the bullet "NO
            // `invokedynamic` ANYWHERE IN THIS BODY … widening the mint's reach is recorded as
            // having produced a run that died on a null local", and it is written that way
            // precisely so the hoist can never hand this arm a cursor by accident. So the refusal
            // is not stale bookkeeping; it is a measured boundary, and moving it is a change to
            // the mint's verified reach, not a gate lift. Left alone deliberately.
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled() && crate::jit::lambda_shape::enabled() {
                if let Some(cv) = cursor_var {
                    if emit_inline_lambda(b, env, stack, vars, locals_ptr, cv, idx, &desc).is_some()
                    {
                        if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                        return Some(());
                    }
                }
                // HR-INDY (added line; `RAVA_NO_HR_INDY=1` makes this a no-op): the site-cursor entry for a method with no hoisted cursor — see `emit_inline_lambda_sitecursor`.
                if cursor_var.is_none() && n > 0 { if crate::jit::lambda_shape::hr_enabled() { crate::jit::lambda_shape::note_emit_refuse(0); } if emit_inline_lambda_sitecursor(b, env, stack, vars, locals_ptr, idx, &desc).is_some() { return Some(()); } }
            }
            // WARM-SITE INDY RESULT CACHE: a non-capturing `LambdaMetafactory` site is a
            // `ConstantCallSite` — the linked object never changes — so a warm site is one load
            // and a push instead of a `rava_indy` crossing into `invokedynamic_assemble`. Needs
            // no TLAB cursor, so unlike the mint above it reaches every compiled method.
            // `RAVA_NO_INDY_SITE_CACHE=1` restores the callback.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::lambda_shape::site_cache_enabled()
                && emit_indy_site_cache(b, env, stack, vars, locals_ptr, idx, &desc).is_some()
            {
                return Some(());
            }
            for i in (0..n).rev() {
                let a = stack.pop()?;
                let cell = to_cell(b, a);
                b.ins().stack_store(cell, env.args_slot, (i as i32) * 8);
            }
            spill_for_gc(b, vars, stack, locals_ptr);
            let buf = b.ins().stack_addr(I64, env.args_slot, 0);
            let cls = b.ins().iconst(I64, env.class_ptr);
            let cpv = b.ins().iconst(I64, idx as i64);
            let nv = b.ins().iconst(I64, n as i64);
            let call = b.ins().call(env.indy, &[cls, cpv, buf, nv]);
            let res = b.inst_results(call)[0];
            reload_after_gc(b, vars, stack, locals_ptr);
            if let Some(jt) = ret_full(&desc) {
                let sv = cell_to_sv(b, res, jt);
                stack.push(sv);
            }
        }
        0xb9 => {
            // invokeinterface — the arena dispatch IC (4-way machine-code PIC), else
            // a real call.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            #[cfg(feature = "next-gen-gc")]
            if let Some(binding) = env.active_sam.filter(|binding| {
                binding.owner_code_key == env.code_key && binding.sam_call_pc == pc
            }) {
                let desc = pool.member_ref(idx)?.descriptor.to_string();
                let nargs = crate::count_args(&desc);
                let mut sam_args = Vec::with_capacity(nargs);
                for _ in 0..nargs {
                    sam_args.push(stack.pop()?);
                }
                sam_args.reverse();
                stack.pop()?; // typed marker receiver; identity is never observed
                stack.extend_from_slice(binding.captures);
                stack.extend(sam_args);
                emit_inline(
                    b,
                    stack,
                    env,
                    array_desc_cell,
                    binding.implementation,
                    locals_ptr,
                    cow,
                    licm,
                    dv,
                    None,
                    vars,
                )?;
                if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                return Some(());
            }
            // FASTUTIL int-list scalar fast-path (the `IntList`-typed call sites).
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled() {
                let m = pool.member_ref(idx)?;
                // `List.get(I)` — the LARGEST single callee crossing the compiled->host seam on
                // a product boot: 10.37% of 13.5M callbacks (`RAVA_JIT_CB_CENSUS`), and it
                // arrives HERE, through the interface, not through the `invokevirtual` twin.
                // Reading the element inline is the same code; the class-word guard sends any
                // non-ArrayList receiver back to the identical interface dispatch.
                if m.owner == "java/util/List"
                    && m.name == "get"
                    && m.descriptor == "(I)Ljava/lang/Object;"
                    && crate::jit::alist_get_inline_enabled()
                    && env.elems_fid > 0
                    && stack.len() >= 2
                {
                    if std::env::var_os("RAVA_ALIST_DBG").is_some() {
                        eprintln!("[ALIST] EMIT-IFACE fid={}", env.elems_fid - 1);
                    }
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::collections::emit_arraylist_get_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3,
                    )?;
                    return Some(());
                }
                // `List.size()` — 4,439,370 calls a boot, the largest single operation in the
                // 38.4 M that reach `fu_fastpath`. Same guard chain as `get(I)` above; the answer
                // is the backing array's length. `RAVA_NO_ALIST_SIZE` restores the callback.
                // ⚠**NOT KEYED ON THE DECLARED OWNER.** The first version required
                // `java/util/List`, and it fired ZERO times on a boot while `size` kept arriving
                // 4.46 M times — because real code calls `size()` through a dozen declared types.
                // The class word is what makes this correct, not the interface named in the
                // constant pool: any receiver that is not the engine's own `ArrayList` takes the
                // same callback it took before. Bind by guard, not by name.
                // `Iterator.hasNext()` — 4,023,040 calls a boot on one fabricated class whose
                // backing IS a guest field, which is the condition that decides whether an
                // inline arm can exist at all.
                if m.name == "size"
                    && m.descriptor == "()I"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::cube_ready()
                    && !stack.is_empty()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::iterator_inline::emit_cube_size_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3, pc,
                    )?;
                    return Some(());
                }
                if m.name == "getDouble"
                    && m.descriptor == "(I)D"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::cube_ready()
                    && stack.len() >= 2
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::iterator_inline::emit_cube_getdouble_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3, pc,
                    )?;
                    return Some(());
                }
                if m.name == "next"
                    && m.descriptor == "()Ljava/lang/Object;"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::ready()
                    && !stack.is_empty()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::iterator_inline::emit_iter_next_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3, pc,
                    )?;
                    return Some(());
                }
                if m.name == "hasNext"
                    && m.descriptor == "()Z"
                    && crate::jit::emit::iterator_inline::enabled()
                    && crate::jit::emit::iterator_inline::ready()
                    && !stack.is_empty()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::iterator_inline::emit_iter_hasnext_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3, pc,
                    )?;
                    return Some(());
                }
                // `Map$Entry.getKey`/`getValue` — one field read on a two-field fabricated
                // object, 464,896 crossings a boot. Bound by the class WORD at runtime, so an
                // unrelated entry shape simply takes the callback.
                if (m.name == "getKey" || m.name == "getValue")
                    && m.descriptor == "()Ljava/lang/Object;"
                    && crate::jit::entry_inline_enabled()
                    && crate::jit::entry_ready()
                    && !stack.is_empty()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::collections::emit_entry_get_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3, m.name == "getKey",
                    )?;
                    return Some(());
                }
                if m.name == "getDouble"
                    && m.descriptor == "(I)D"
                    && crate::jit::dalist_get_inline_enabled()
                    // The arm bakes `a` and `size` by id, so it is emitted only once a live
                    // `DoubleArrayList` has published them (and its class word). `env.elems_fid`
                    // — the iterator's synthetic — was the WRONG precondition and the wrong read.
                    && {
                        // Counted HERE because this WAS the only site that emits: the
                        // invokevirtual twin sat inside a block that also demanded
                        // `alist_size_inline_enabled`, which is off by default, so instrumenting
                        // it read zero and said nothing.
                        // ⭐**THAT IS NO LONGER TRUE, AND THE READOUT HAS TWO WRITERS NOW.** The
                        // group predicate in `ops::invoke_virtual` was one arm's opt-in switch
                        // hoisted over six unrelated default-ON arms; it has been pushed back down
                        // onto the `size()I` arms it belongs to, so the 0xb6 twin of THIS arm
                        // emits on a default boot and bumps the same `DALIST_GATE_*` pair. Read
                        // the totals as 0xb6+0xb9. `RAVA_NO_EMIT_INVOKEOPS=1` restores the old
                        // nesting, and with it the old single-writer reading.
                        let r = crate::jit::dalist_ready();
                        if r { crate::jit::DALIST_GATE_READY.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                        else { crate::jit::DALIST_GATE_COLD.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                        r
                    }
                    && stack.len() >= 2
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::collections::emit_dalist_getdouble_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3,
                    )?;
                    return Some(());
                }
                if m.name == "size"
                    && m.descriptor == "()I"
                    && crate::jit::alist_size_inline_enabled()
                    && env.elems_fid > 0
                    && !stack.is_empty()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    crate::jit::emit::collections::emit_arraylist_size_inline(
                        b, env, idx, vars, stack, locals_ptr, array_desc_cell, 3,
                    )?;
                    return Some(());
                }
                // ⭐**THE SITE-KEYED ARENA GETTER, ahead of the routable callback it removes.**
                // Emitted for every zero-argument routable site, because whether the site can be
                // described is only known at RUNTIME: `trivial_getter`'s entry is written by the
                // first crossing, which happens AFTER this method compiled. So the test is in the
                // emitted code, not here — the same bootstrap trap that kept three earlier arms
                // at zero. The fallback is the identical `fu_invoke` callback.
                if fu_routable(&m.owner, &m.name, &m.descriptor) {
                    let desc = m.descriptor.to_string();
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                    emit_invoke_via(b, stack, env, idx, 3, &desc, vars, locals_ptr, env.fu_invoke)?;
                    return Some(());
                }
            }
            let desc = pool.member_ref(idx)?.descriptor.to_string();
            // ⭐**GUARDED DEVIRTUALISATION AT AN INTERFACE SITE.** This is where the collection
            // API arrives — `List.size()`, `Iterator.hasNext()`, `Map.get()` — and the reason
            // the inliner used to log zero collapses on a collection workload: an interface call
            // cannot bind by type, so it was always a real call. The hand-written `List.get(I)`
            // arm above is this same idea built for exactly one method; this is the general form,
            // driven by the receiver profile instead of by a name.
            #[cfg(feature = "next-gen-gc")]
            if let Some(callee) = env.inline_targets.get(&idx) {
                if !callee.is_static
                    && (callee.guard_class.is_some() || callee.guard_ref_word.is_some())
                    // ⭐**THE SAM CHAIN OPENS ITS OWN DOOR.** `devirt_iface_enabled()` is the
                    // opt-in gate for receiver-profile devirtualisation of ORDINARY interface
                    // calls, and it is default OFF because that population was measured at +2.42%.
                    // A SAM callee is a different population with a different guard: it is admitted
                    // only for an arena closure at an `invokeinterface` site, and its guard
                    // compares the interned SITE index out of `AL_SITE_SLOT`, never the class id —
                    // because a lambda's class IS its capture signature and a class-id compare
                    // would splice another site's body while reporting a hit. Leaving the SAM
                    // chain behind the ordinary-devirt flag would have shipped admission, profile,
                    // guard and splice all live and the emission dead, which is the shape that has
                    // already cost this tree seven inline arms hidden behind one arm's flag.
                    && (crate::jit::devirt_iface_enabled() || callee.sam.is_some())
                {
                    use crate::jit::emit::guarded_inline::Guarded;
                    let r = crate::jit::emit::guarded_inline::emit_guarded_inline(
                        b, stack, env, array_desc_cell, callee, locals_ptr, cow, vars, pc, idx, licm, dv,
                        |b, stack| {
                            // The slow arm must be exactly what this site would have emitted.
                            if !arena_inline_disabled()
                                && wide_site_pic_ok(pool, idx, &desc)
                                && arg_slot_widths(&desc).iter().sum::<usize>() <= dispatch_callbuf_slots()
                            {
                                let heap_mode = crate::heappic::emit_mode();
                                emit_dispatch_pic(b, stack, env, idx, 3, &desc, vars, locals_ptr, array_desc_cell, heap_mode, pc)
                            } else {
                                emit_invoke(b, stack, env, idx, 3, &desc, vars, locals_ptr)
                            }
                        },
                    );
                    match r {
                        Guarded::Emitted => { if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); } return Some(()) },
                        Guarded::Aborted => return None,
                        Guarded::Declined => {}
                    }
                }
            }
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::pic_elig_enabled() {
                if let Some(m) = pool.member_ref(idx) {
                    crate::jit::pic_elig_note(
                        3, &m.owner, &m.name, &m.descriptor,
                        !crate::jit::arena_inline_disabled(),
                        crate::jit::wide_site_pic_ok(pool, idx, &desc),
                        crate::jit::arg_slot_widths(&desc).iter().sum::<usize>() <= crate::jit::dispatch_callbuf_slots(),
                    );
                }
            }
            #[cfg(feature = "next-gen-gc")]
            if !arena_inline_disabled()
                && wide_site_pic_ok(pool, idx, &desc)
                && arg_slot_widths(&desc).iter().sum::<usize>() <= dispatch_callbuf_slots()
            {
                // Wide params supported (slot-laid buffer) — see emit_dispatch_pic.
                if crate::picprobe::enabled() { crate::picprobe::site_pic(); }
                // 23.4b live heap-Ref PIC: emit at every virtual/interface site (leaf-only cacheability in
                // heappic_fill filters WHAT actually caches — non-leaf classes stay -1/miss). DVS-only was
                // a no-op (its getSize/getXSize pool is non-leaf); the win is the general leaf heap-Ref pool.
                let heap_mode = crate::heappic::emit_mode();
                if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_pic(); }
                emit_dispatch_pic(b, stack, env, idx, 3, &desc, vars, locals_ptr, array_desc_cell, heap_mode, pc)?;
                return Some(());
            }
            if crate::picprobe::enabled() { crate::picprobe::site_invoke(3); }
            if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_cb(); }
            emit_invoke(b, stack, env, idx, 3, &desc, vars, locals_ptr)?; // kind 3 = interface
        }
        _ => return None,
    }
    Some(())
}
