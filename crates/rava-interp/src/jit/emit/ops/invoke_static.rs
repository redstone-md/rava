//! Static and special invocation, plus athrow.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub(crate) fn invoke_static(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
    let b = &mut *o.b;
    let stack = &mut *o.stack;
    let cur_open = &mut *o.cur_open;
    let vars = o.vars;
    let bc = o.bc;
    let blocks = o.blocks;
    let pool = o.pool;
    let env = o.env;
    let array_desc_cell = o.array_desc_cell;
    let locals_ptr = o.locals_ptr;
    let cow = o.cow;
    #[cfg(feature = "next-gen-gc")]
    let (licm, dv) = (o.licm, o.dv);
    match op {
        0xbf => {
            // athrow: hand the popped exception object to rava_throw (stashes it as
            // the pending exception + latches the fault), then terminate the block
            // with a dummy return — jit_run raises the pending exception when the
            // compiled function returns. A null operand allocates an NPE, so root
            // every live reference, including the operand, across the callback.
            spill_for_gc(b, vars, stack, locals_ptr);
            let exc = stack.pop()?;
            let cell = to_cell(b, exc);
            b.ins().call(env.throw, &[cell]);
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
            let zero = b.ins().iconst(I64, 0);
            #[cfg(feature = "next-gen-gc")]
            let zero = emit_deep_epilogue(b, env, dv, zero);
            let zero = abi_ret_from_cell(b, zero);
            b.ins().return_(&[zero]);
            *cur_open = false;
        }
        0xb8 => {
            // invokestatic — intrinsic fast-path, else a real call
            #[cfg(feature = "next-gen-gc")]
            if env.hof_fusions.values().any(|plan| plan.consumer_pc == pc) {
                // The adjacent indy emitted this exact consumer body and left its
                // result on the stack. The map is Code-scoped, so a nested body's
                // numerically equal PC can never be skipped.
                return Some(());
            }
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let m = pool.member_ref(idx)?;
            let desc = m.descriptor.to_string();
            // COLLECTION INTRINSIC: Integer.valueOf(int) — box the int directly.
            #[cfg(feature = "next-gen-gc")]
            let is_box_int = !arena_inline_disabled()
                && m.owner == "java/lang/Integer"
                && m.name == "valueOf"
                && desc == "(I)Ljava/lang/Integer;";
            #[cfg(not(feature = "next-gen-gc"))]
            let is_box_int = false;
            if is_box_int {
                #[cfg(feature = "next-gen-gc")]
                {
                    let x = stack.pop()?;
                    // FUSION: if this boxed int is immediately consumed by an arena-map
                    // get/containsKey (next op, same block — not a branch target), leave
                    // the raw int on the stack (JT::Int) so the probe loop computes the
                    // hash + compares the key in registers, with no box→unbox round trip.
                    let fused = crate::arena_map::enabled() && {
                        let np = pc + 3;
                        !blocks.contains_key(&np)
                            && bc.get(np) == Some(&0xb6)
                            && np + 2 < bc.len()
                            && {
                                let nidx = ((bc[np + 1] as u16) << 8) | bc[np + 2] as u16;
                                pool.member_ref(nidx).is_some_and(|nm| {
                                    nm.owner == "java/util/HashMap"
                                        && matches!(&*nm.name, "get" | "containsKey")
                                })
                            }
                    };
                    if fused {
                        push(stack, x.v, JT::Int); // raw int, consumed by emit_amap_int_probe
                    } else {
                        let xi = to_cell(b, x); // sign-extend the int
                        let call = b.ins().call(env.box_int_fn, &[xi]);
                        let res = b.inst_results(call)[0];
                        push(stack, res, JT::Ref);
                    }
                }
            } else if let Some(&intrinsic) = env.static_intrinsics.get(&idx) {
                emit_static_intrinsic(b, stack, intrinsic)?;
            } else if m.owner == "java/lang/Math"
                && math_intrinsic(b, stack, m.name, &desc).is_some()
            {
                // inlined
            } else if matches!(&*m.owner, "java/lang/Integer" | "java/lang/Long")
                && intbits_intrinsic(b, stack, &m.owner, m.name, &desc).is_some()
            {
                // inlined (ctz/clz/popcnt)
            } else if let Some(callee) = env.inline_targets.get(&idx) {
                if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                emit_inline(b, stack, env, array_desc_cell, callee, locals_ptr, cow, #[cfg(feature = "next-gen-gc")] licm, #[cfg(feature = "next-gen-gc")] dv, #[cfg(feature = "next-gen-gc")] None, #[cfg(feature = "next-gen-gc")] vars)?;
            } else {
                // ⚠**WHAT THIS COUNTER ACTUALLY COUNTS, because the readout's label is wrong and
                // a whole round was aimed at the number it prints.** `[PIC-COV]` renders
                // `SITE_INVOKE[0]`/`[2]` as "invoke-emitted ... static=N special=M (static+special
                // = structurally non-PIC-able)", which reads as "N+M sites emit a `rava_invoke`
                // callback". It is bumped HERE — before `emit_static_reg_call`, before the
                // `fu_static` route and before `emit_direct_mono`, all of which may claim the site
                // and emit a machine-code call instead. So it is a tally of static call SITES
                // REACHED, i.e. the denominator; it is NOT the population that leaves machine code.
                // The honest measurement of that population is the DYNAMIC counter
                // `PLAIN_INVOKE[0]`/`[2]`, bumped inside `rava_invoke` itself by calls that really
                // did take the generic path. Moving this bump down to the real `emit_invoke` below
                // would fix the label and break the denominator `sites = SITE_PIC + SITE_INVOKE`
                // that `picprobe` computes, so it stays put and this comment carries the caveat.
                if crate::picprobe::enabled() { crate::picprobe::site_invoke(0); }
                // REFERENCE register ABI (RAVA_REF_ABI). Tried BEFORE direct-mono, because
                // direct-mono is default-on and would otherwise claim the site and emit the
                // call-buffer marshalling this whole path exists to avoid. Deliberately
                // narrow: it only engages for a descriptor that actually carries a
                // reference, so every all-primitive site keeps its existing codegen and an
                // unflagged build is byte-identical.
                #[cfg(feature = "next-gen-gc")]
                if ref_abi_enabled()
                    && reg_abi_desc(&desc, true).is_some_and(|a| a.carries_ref())
                    && emit_static_reg_call(b, stack, env, idx, &desc, vars, locals_ptr, array_desc_cell).is_some()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_direct(); }
                    return Some(());
                }
                // ⭐**STATICS NEVER ASKED `fu_routable`.** The predicate was consulted only by
                // the `invokevirtual` and `invokedynamic` emitters, so a static admitted to
                // `fu_fastpath` was dead code. ⚠And placed BELOW direct-mono it STILL measured nothing, for the reason the
                // `ref_abi` arc two branches up already documents: **direct-mono is default-on and
                // claims the site first.** Same fix, same place — above it.
                // (An arm for `Objects.equals` measured exactly no
                // change until this line existed. `fu_fastpath` re-checks the member and returns
                // None for anything it does not serve, so routing here cannot change what runs.
                #[cfg(feature = "next-gen-gc")]
                if let Some(m) = pool.member_ref(idx) {
                    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    if !*OFF.get_or_init(|| std::env::var_os("RAVA_NO_FU_STATIC").is_some())
                        && crate::jit::fu_routable(m.owner, m.name, m.descriptor)
                    {
                        if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                        emit_invoke_via(b, stack, env, idx, 0, &desc, vars, locals_ptr, env.fu_invoke)?;
                        return Some(());
                    }
                }
                // Try the opt-in primitive register ABI before the buffer ABI, just
                // like the reference register ABI above. Direct-mono accepts these
                // descriptors too, so testing this after it makes the flag unreachable
                // for ordinary static sites. Only the descriptor controls eligibility.
                #[cfg(feature = "next-gen-gc")]
                if prim_abi_enabled()
                    && all_primitive_desc(&desc).is_some()
                    && emit_static_reg_call(b, stack, env, idx, &desc, vars, locals_ptr, array_desc_cell).is_some()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_direct(); }
                    return Some(());
                }
                // 23.9 monomorphic direct-call for static. A static callee has NO receiver
                // polymorphism, so its target is fixed once resolved and it needs a DIRECT CALL,
                // not a polymorphic cache — the per-site single-fptr cache + `call_indirect` this
                // emits, with `rava_direct_miss` as the cold arm that resolves, fills and runs.
                // It now also serves WIDE-ARGUMENT sites (`long`/`double` params), which it used
                // to refuse outright and hand to `emit_invoke` permanently; kill switch
                // `RAVA_NO_EMIT_STATIC=1` restores that refusal — see `emit_direct_mono`.
                #[cfg(feature = "next-gen-gc")]
                if crate::directmono::enabled()
                    && emit_direct_mono(b, stack, env, idx, 0, &desc, vars, locals_ptr, array_desc_cell).is_some()
                {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_direct(); }
                    return Some(());
                }
                if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_cb(); }
                emit_invoke(b, stack, env, idx, 0, &desc, vars, locals_ptr)?; // kind 0 = static
            }
        }
        0xb7 => {
            // invokespecial — statically bound, so a small instance callee inlines
            // directly (no receiver-class guard needed). Else a real call.
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            // `Object.<init>()V` is the empty root constructor (JVM spec: Object has no
            // fields; the interpreter no-ops it too) — emit NOTHING but the receiver pop.
            // This is what lets a trivial super-ctor chain (AbstractDoubleCollection etc.,
            // millions of boot calls) compile to straight-line code with zero callbacks.
            {
                let m = pool.member_ref(idx)?;
                if m.owner == "java/lang/Object" && m.name == "<init>" && m.descriptor == "()V" {
                    stack.pop()?;
                    return Some(());
                }
            }
            // TRIVIAL ARENA CONSTRUCTOR: store the args straight into the freshly
            // bump-allocated object's cells, NO rava_invoke callback.
            #[cfg(feature = "next-gen-gc")]
            if let Some(ci) = env.ctor_inlines.get(&idx) {
                if field_verify_disabled() && !arena_inline_disabled() {
                    emit_inline_ctor(b, env, stack, array_desc_cell, ci)?;
                    return Some(());
                }
            }
            if let Some(callee) = env.inline_targets.get(&idx) {
                if !callee.is_static {
                    if std::env::var_os("RAVA_INVOKE_CENSUS").is_some() { crate::jit::emit::invoke_site_census::bump_inline(); }
                emit_inline(b, stack, env, array_desc_cell, callee, locals_ptr, cow, #[cfg(feature = "next-gen-gc")] licm, #[cfg(feature = "next-gen-gc")] dv, #[cfg(feature = "next-gen-gc")] None, #[cfg(feature = "next-gen-gc")] vars)?;
                    return Some(());
                }
            }
            let desc = pool.member_ref(idx)?.descriptor.to_string();
            // Same caveat as the invokestatic bump above: this is static/special sites REACHED,
            // not sites that emit a `rava_invoke` callback. `PLAIN_INVOKE[2]` is the dynamic truth.
            if crate::picprobe::enabled() { crate::picprobe::site_invoke(2); }
            // 23.9 monomorphic direct-call for special. `invokespecial` is statically bound, so
            // like a static it wants a direct call rather than a cache keyed on the receiver's
            // class: single-fptr cache + `call_indirect`, `rava_direct_miss` as the cold arm.
            // WIDE-ARGUMENT special sites (`<init>(J)V`, `<init>(DDD)V`, …) now reach the direct
            // arm too instead of being refused at emit time; `RAVA_NO_EMIT_STATIC=1` reverts.
            #[cfg(feature = "next-gen-gc")]
            if crate::directmono::enabled()
                && emit_direct_mono(b, stack, env, idx, 2, &desc, vars, locals_ptr, array_desc_cell).is_some()
            {
                return Some(());
            }
            emit_invoke(b, stack, env, idx, 2, &desc, vars, locals_ptr)?; // kind 2 = special
        }
        _ => return None,
    }
    Some(())
}
