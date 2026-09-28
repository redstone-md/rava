//! Field access: getfield, putfield, getstatic, putstatic, each with its inline fast paths and the callback it falls back to.
//!
//! Arm bodies are unchanged; the preamble restores every name they were written against.

use super::Ops;
use crate::jit::*;

pub mod site_census {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static CB: AtomicU64 = AtomicU64::new(0);
    pub static PRIM_RESOLVED: AtomicU64 = AtomicU64::new(0);
    pub static PRIM_UNRESOLVED: AtomicU64 = AtomicU64::new(0);
    pub static REF_RESOLVED: AtomicU64 = AtomicU64::new(0);
    pub static REF_UNRESOLVED: AtomicU64 = AtomicU64::new(0);
    pub static PUT_RESOLVED: AtomicU64 = AtomicU64::new(0);
    pub static PUT_UNRESOLVED: AtomicU64 = AtomicU64::new(0);
    pub fn bump(c: &AtomicU64) { ensure_printer(); c.fetch_add(1, Ordering::Relaxed); }
    fn ensure_printer() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            if std::env::var_os("RAVA_FIELD_SITE_CENSUS").is_none() { return; }
            std::thread::Builder::new().name("field-sites-printer".into()).spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let s = report_inner();
                if !s.is_empty() { eprintln!("{s}"); }
            }).ok();
        });
    }
    fn report_inner() -> String {
        use std::sync::atomic::Ordering::Relaxed;
        let (cb, pr, pu, rr, ru, ptr, ptu) = (
            CB.load(Relaxed), PRIM_RESOLVED.load(Relaxed), PRIM_UNRESOLVED.load(Relaxed),
            REF_RESOLVED.load(Relaxed), REF_UNRESOLVED.load(Relaxed),
            PUT_RESOLVED.load(Relaxed), PUT_UNRESOLVED.load(Relaxed),
        );
        let tot = cb+pr+pu+rr+ru+ptr+ptu;
        if tot==0 { return String::new(); }
        let p = |v: u64| format!("{:.1}%", 100.0*v as f64/tot as f64);
        format!("[FIELD-SITES] {tot} sites | prim resolved {pr} {} unresolved {pu} {} | ref resolved {rr} {} unresolved {ru} {} | put resolved {ptr} {} unresolved {ptu} {} | callback {cb} {}
",
            p(pr), p(pu), p(rr), p(ru), p(ptr), p(ptu), p(cb))
    }
    pub fn report() -> String {
        if std::env::var_os("RAVA_FIELD_SITE_CENSUS").is_none() { return String::new(); }
        report_inner()
    }
}

pub(crate) fn fields(o: &mut Ops, pc: usize, op: u8) -> Option<()> {
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
    let licm = o.licm;
    match op {
        0xb4 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let &(fid, jt, slot) = env.fields.get(&idx)?;
            let proven_nonnull = env.nonnull_getfields.contains(&pc);
            let obj = stack.pop()?;
            // NATIVE-BACKED CLASS: ask the host accessor, exactly as the interpreter does.
            if env.callback_fields {
                site_census::bump(&site_census::CB);
                let fidc = b.ins().iconst(I64, fid as i64);
                let isr = b.ins().iconst(I64, if jt == JT::Ref { 1 } else { 0 });
                let call = b.ins().call(env.getfield, &[obj.v, fidc, isr]);
                let res = b.inst_results(call)[0];
                let sv = cell_to_sv(b, res, jt);
                stack.push(sv);
                return Some(());
            }
            // M3 COW inline-branch (RAVA_FORK_COW build): when armed (a phase-B worker),
            // route the READ through the shadow-aware rava_getfield callback; else the inline
            // fast path below. Gate off → pure inline (byte-identical).
            // `CowArm::Disarmed` — the method-entry UNSWITCH already proved this whole
            // invocation disarmed — emits NO diamond here at all: no `[ctx+12]` test, no
            // bloom probe, no merge phi, so the inline load below is straight-line and the
            // egraph can keep bases/guards in registers across sites.
            let cow_merge = if crate::parallel::cow_gate() && !matches!(cow, CowArm::Disarmed) {
                let cow_blk = b.create_block();
                let inl_blk = b.create_block();
                let merge = b.create_block();
                b.append_block_param(merge, jt_ty(jt));
                // The arm flag cannot change inside one invocation, so reuse the
                // entry-block test instead of re-loading + re-comparing it here.
                let is_armed = match cow {
                    CowArm::Hoisted(v) => v,
                    // Disarmed is excluded above; PerSite has no hoisted flag to reuse.
                    _ => {
                        let armed = b.ins().load(
                            I32,
                            MemFlags::trusted(),
                            array_desc_cell,
                            Offset32::new(12),
                        );
                        b.ins().icmp_imm(IntCC::NotEqual, armed, 0)
                    }
                };
                b.ins().brif(is_armed, cow_blk, &[], inl_blk, &[]);
                b.switch_to_block(cow_blk);
                b.seal_block(cow_blk);
                // Counting Bloom over all currently active TLS shadows. Zero is
                // an exact negative across every worker, so the read can stay on
                // the existing inline shared path. Collisions and another
                // worker's matching bit only cause a conservative callback.
                let key = b.ins().band_imm(obj.v, u32::MAX as i64);
                let fid_mix =
                    (fid as u64).wrapping_mul(crate::heap::COW_BLOOM_FID_MUL) as i64;
                let x = b.ins().bxor_imm(key, fid_mix);
                let s3 = b.ins().ushr_imm(x, 3);
                let s17 = b.ins().ushr_imm(x, 17);
                let x3 = b.ins().bxor(x, s3);
                let mixed = b.ins().bxor(x3, s17);
                let index = b.ins().band_imm(mixed, crate::heap::COW_BLOOM_MASK as i64);
                let byte_offset = b.ins().ishl_imm(index, 2);
                let base = b.ins().load(I64, MemFlags::trusted(), array_desc_cell, Offset32::new(56));
                let counter_addr = b.ins().iadd(base, byte_offset);
                let count = b.ins().load(I32, MemFlags::trusted(), counter_addr, Offset32::new(0));
                let present = b.ins().icmp_imm(IntCC::NotEqual, count, 0);
                let lookup = b.create_block();
                b.ins().brif(present, lookup, &[], inl_blk, &[]);
                b.switch_to_block(lookup);
                b.seal_block(lookup);
                let fidc = b.ins().iconst(I64, fid as i64);
                let isr = b.ins().iconst(I64, if jt == JT::Ref { 1 } else { 0 });
                let call = b.ins().call(env.getfield, &[obj.v, fidc, isr]);
                let res = b.inst_results(call)[0];
                let sv = cell_to_sv(b, res, jt);
                b.ins().jump(merge, &[sv.v.into()]);
                b.switch_to_block(inl_blk);
                b.seal_block(inl_blk);
                Some(merge)
            } else {
                None
            };
            let v = if jt == JT::Ref {
                match slot {
                    Some(s) => {
                        site_census::bump(&site_census::REF_RESOLVED);
                        // TRACER: a ref field on an arena receiver reads inline (re-tag).
                        #[cfg(feature = "next-gen-gc")]
                        {
                            if !arena_inline_disabled() {
                                // FLAT-GETFIELD: this receiver was proven arena by the
                                // method-entry unswitch → straight-line arena arm, no
                                // per-site is_arena diamond.
                                // DESCRIPTOR SPLIT: a field declared `[…` keeps the tag
                                // classify even under the bare-deref flip. Its downstream
                                // consumers (`arraylength`, `aaload`) require an
                                // Array-tagged value, and the descriptor already fixes the
                                // representation, so the test is free to keep — and keeping
                                // it is what takes 211,141 cells out of the flip's blocker
                                // set without moving one array.
                                let bare_ok =
                                    env.bare_allowed && !env.array_fields.contains(&idx);
                                // EMIT-TIME census (`RAVA_BARE_AUDIT`): the rule admitting a field
                                // and the emitter reaching it are different events, and the shadow
                                // verify reported nothing while the audit printed 82,104 lines —
                                // so the gap is here, not in the rule.
                                // Hoisted: an `env::var_os` per EMISSION is compile-path cost on
                                // every ref field, audit or not.
                                static AUDIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                                if *AUDIT.get_or_init(|| std::env::var_os("RAVA_BARE_AUDIT").is_some()) {
                                    eprintln!(
                                        "[BARE-EMIT] bare_ok={bare_ok} allowed={} arena_recv={} idx={idx}",
                                        env.bare_allowed, obj.arena
                                    );
                                }
                                if obj.arena {
                                    emit_arena_getfield_ref_flat(b, env, obj.v, fid as i64, s, bare_ok)
                                } else {
                                    native_getfield_ref_const_arena_dual_proven(
                                        b, env, array_desc_cell, obj.v, fid as i64, s, bare_ok, proven_nonnull,
                                    )
                                }
                            } else {
                                native_getfield_ref_const_proven(b, env, array_desc_cell, obj.v, fid as i64, s, proven_nonnull)
                            }
                        }
                        #[cfg(not(feature = "next-gen-gc"))]
                        {
                            native_getfield_ref_const_proven(b, env, array_desc_cell, obj.v, fid as i64, s, proven_nonnull)
                        }
                    }
                    None => { site_census::bump(&site_census::REF_UNRESOLVED); native_getfield_ref(b, env, array_desc_cell, obj.v, fid as i64) }
                }
            } else {
                match slot {
                    Some(s) if field_verify_disabled() => {
                        site_census::bump(&site_census::PRIM_RESOLVED);
                        // TRACER: a primitive field on an arena receiver reads inline.
                        // Default build compiles ONLY the original call (byte-identical).
                        #[cfg(feature = "next-gen-gc")]
                        {
                            // FIELD-LICM: a prologue-pinned (local, field) pair reads
                            // through its resolved cell pointer (one predictable branch
                            // + one load) instead of the per-site guard chain.
                            let pinned = licm.and_then(|l| {
                                obj.src.and_then(|k| {
                                    l.vars.get(&(k, fid, s)).copied().map(|bv| (l, k, bv))
                                })
                            });
                            if let Some((l, k, bv)) = pinned {
                                // PROMO body: the register IS the field (bases proven
                                // non-zero at the unswitch) — zero instructions here.
                                if let Some(&vv) = l.vals.get(&(k, fid, s)) {
                                    let cell = b.use_var(vv);
                                    cell_to_sv(b, cell, jt).v
                                } else {
                                    emit_licm_getfield(b, env, array_desc_cell, obj.v, fid as i64, s, jt, bv)
                                }
                            } else if obj.arena && !arena_inline_disabled() {
                                // FLAT-GETFIELD: this receiver was proven arena at method
                                // entry → bare straight-line load of the SAME cell, no
                                // per-site arena/handle diamond (egraph can now CSE it).
                                emit_arena_getfield_prim_flat(b, env, obj.v, s, jt)
                            } else if !arena_inline_disabled() {
                                native_getfield_const_prim_arena_dual_proven(
                                    b, env, array_desc_cell, obj.v, fid as i64, s, jt, proven_nonnull,
                                )
                            } else {
                                native_getfield_const_unchecked_proven(
                                    b, env, array_desc_cell, obj.v, fid as i64, s, jt, proven_nonnull,
                                )
                            }
                        }
                        #[cfg(not(feature = "next-gen-gc"))]
                        {
                            native_getfield_const_unchecked_proven(
                                b, env, array_desc_cell, obj.v, fid as i64, s, jt, proven_nonnull,
                            )
                        }
                    }
                    Some(s) => { site_census::bump(&site_census::PRIM_RESOLVED); native_getfield_const_proven(b, env, array_desc_cell, obj.v, fid as i64, s, jt, proven_nonnull) }
                    None => { site_census::bump(&site_census::PRIM_UNRESOLVED); native_getfield(b, env, array_desc_cell, obj.v, fid as i64, jt) }
                }
            };
            // COW: merge the callback + inline values (phi); else push the inline value direct.
            match cow_merge {
                Some(merge) => {
                    b.ins().jump(merge, &[v.into()]);
                    b.switch_to_block(merge);
                    b.seal_block(merge);
                    let mv = b.block_params(merge)[0];
                    push(stack, mv, jt);
                }
                None => push(stack, v, jt),
            }
        }

        // putfield — primitive fields write natively into an existing slot
        // (fallback to the callback for null / new field / pending fault);
        // reference fields stay on the callback (box encode).
        0xb5 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let &(fid, jt, slot) = env.fields.get(&idx)?;
            let value = stack.pop()?;
            let obj = stack.pop()?;
            let val_cell = to_cell(b, value);
            let kind = match jt {
                JT::Int => 0,
                JT::Long => 1,
                JT::Float => 2,
                JT::Double => 3,
                JT::Ref => 4,
            };
            // NATIVE-BACKED CLASS: the WRITE goes to the host accessor too. A read routed through
            // the native path and a write done inline would be worse than either — the pair has
            // to agree on where the field lives.
            if env.callback_fields {
                site_census::bump(&site_census::CB);
                let fidc = b.ins().iconst(I64, fid as i64);
                let kindc = b.ins().iconst(I64, kind);
                b.ins().call(env.putfield, &[obj.v, fidc, val_cell, kindc]);
                return Some(());
            }
            // M3 COW inline-branch: when armed (a phase-B worker) the WRITE goes through
            // rava_putfield (→ its shadow); else the inline store below. Gate off → pure inline
            // (byte-identical). `CowArm::Disarmed` (method-entry UNSWITCH) → no diamond at all.
            let cow_merge = if crate::parallel::cow_gate() && !matches!(cow, CowArm::Disarmed) {
                let cow_blk = b.create_block();
                let inl_blk = b.create_block();
                let merge = b.create_block();
                // Invocation-invariant (see getfield above): reuse the entry-block test.
                let is_armed = match cow {
                    CowArm::Hoisted(v) => v,
                    _ => {
                        let armed = b.ins().load(
                            I32,
                            MemFlags::trusted(),
                            array_desc_cell,
                            Offset32::new(12),
                        );
                        b.ins().icmp_imm(IntCC::NotEqual, armed, 0)
                    }
                };
                b.ins().brif(is_armed, cow_blk, &[], inl_blk, &[]);
                b.switch_to_block(cow_blk);
                b.seal_block(cow_blk);
                let fidc = b.ins().iconst(I64, fid as i64);
                let kindc = b.ins().iconst(I64, kind);
                b.ins().call(env.putfield, &[obj.v, fidc, val_cell, kindc]);
                b.ins().jump(merge, &[]);
                b.switch_to_block(inl_blk);
                b.seal_block(inl_blk);
                Some(merge)
            } else {
                None
            };
            if jt == JT::Ref {
                // TRACER: ref store on an arena receiver inlines (encode + store +
                // card barrier); slot present → inline arm, else handle path.
                match slot {
                    #[cfg(feature = "next-gen-gc")]
                    Some(s) if field_verify_disabled()
                        && !arena_inline_disabled()
                        && arena_slot_probe_ok(fid as i64) => {
                        site_census::bump(&site_census::PUT_RESOLVED);
                        // Same descriptor split as the LOAD, and it must agree with it: a
                        // store that wrote a bare pointer into an array-declared field would
                        // leave a cell the tagged load then misreads.
                        native_putfield_ref_arena_dual(b, env, array_desc_cell, obj.v, val_cell, fid as i64, s, env.bare_allowed && crate::nextgen::bare_store_enabled() && !env.array_fields.contains(&idx))
                    }
                    _ => { site_census::bump(&site_census::PUT_UNRESOLVED); native_putfield_ref(b, env, array_desc_cell, obj.v, val_cell, fid as i64) }
                }
            } else {
                match slot {
                    Some(s) if field_verify_disabled() => {
                        site_census::bump(&site_census::PUT_RESOLVED);
                        // TRACER: primitive store on an arena receiver inlines.
                        #[cfg(feature = "next-gen-gc")]
                        {
                            // FIELD-LICM: a putfield can only exist when
                            // invariant_fields is false, so a hoisted base for this
                            // (local, field) is ARENA-ONLY (the handle arm is gated
                            // on invariant_fields) — storing at [base + slot*8]
                            // matches the arena arm's trusted fixed-layout store.
                            let pinned = licm.and_then(|l| {
                                obj.src.and_then(|k| {
                                    l.vars.get(&(k, fid, s)).copied().map(|bv| (l, k, bv))
                                })
                            });
                            if let Some((l, k, bv)) = pinned {
                                // PROMO body: defer the store to the register (flushed
                                // at the outer returns) — zero memory ops here.
                                if let Some(&vv) = l.vals.get(&(k, fid, s)) {
                                    b.def_var(vv, val_cell);
                                } else {
                                    emit_licm_putfield(b, env, array_desc_cell, obj.v, val_cell, fid as i64, s, kind, bv)
                                }
                            } else if !arena_inline_disabled() {
                                native_putfield_const_prim_arena_dual(b, env, array_desc_cell, obj.v, val_cell, fid as i64, s, kind)
                            } else {
                                native_putfield_const_unchecked(b, env, array_desc_cell, obj.v, val_cell, fid as i64, s, kind)
                            }
                        }
                        #[cfg(not(feature = "next-gen-gc"))]
                        {
                            native_putfield_const_unchecked(b, env, array_desc_cell, obj.v, val_cell, fid as i64, s, kind)
                        }
                    }
                    Some(s) => native_putfield_const(b, env, array_desc_cell, obj.v, val_cell, fid as i64, s, kind),
                    None => native_putfield(b, env, array_desc_cell, obj.v, val_cell, fid as i64, kind),
                }
            }
            // COW: rejoin the inline store with the callback path.
            if let Some(merge) = cow_merge {
                b.ins().jump(merge, &[]);
                b.switch_to_block(merge);
                b.seal_block(merge);
            }
            // LANE STORE-FORM CENSUS (diagnostic only, off unless RAVA_LANE_STORE_CENSUS):
            // record which of the four write forms this putfield emission selected. The
            // expression mirrors the arm selection above exactly (same `env.callback_fields`
            // / `cow_gate()` / `cow` inputs, read at the same point in the body).
            #[cfg(feature = "next-gen-gc")]
            crate::jit::emit::store_census::record_putfield(env, pc, cow);
        }

        // getstatic — primitive sites with a value cell take a native gen-guarded
        // load (cell.gen == ctx.static_gen → load cell.value; else callback, which
        // repopulates). Ref sites / overflowed sites fall back to the plain callback.
        0xb2 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let jt = desc_to_jt(pool.member_ref(idx)?.descriptor);
            // Static-LICM: the cell was populated at entry and can't change during
            // this invariant call → a plain READONLY load (no gen guard, no diamond).
            // The egraph CSEs the base + hoists the loop-invariant value load out.
            // REF statics never take this arm: the readonly load skips the gen
            // guard, and a ref cell can be invalidated (GC move / BOX-skip left it
            // sentinel) — they use the gen-guarded diamond below instead.
            if env.static_licm && jt != JT::Ref {
                if let Some(&(slot, _)) = env.statics.get(&idx) {
                    let ro = MemFlags::trusted().with_readonly();
                    let base = b.ins().load(I64, ro, array_desc_cell, Offset32::new(32));
                    let cell_addr = b.ins().iadd_imm(base, (slot as i64) * 16);
                    let cval = b.ins().load(I64, ro, cell_addr, Offset32::new(8));
                    let sv = cell_to_sv(b, cval, jt);
                    stack.push(sv);
                    return Some(());
                }
            }
            if let Some(&(slot, _)) = env.statics.get(&idx) {
                // ctx+32 = static_cells_base, ctx+40 = static_gen mirror.
                let base = b.ins().load(I64, MemFlags::trusted(), array_desc_cell, Offset32::new(32));
                let gen_now = b.ins().load(I64, MemFlags::trusted(), array_desc_cell, Offset32::new(40));
                let cell_addr = b.ins().iadd_imm(base, (slot as i64) * 16);
                let cgen = b.ins().load(I64, MemFlags::trusted(), cell_addr, Offset32::new(0));
                let hit = b.ins().icmp(IntCC::Equal, cgen, gen_now);
                // Ref-typed operand-stack values live across this getstatic: the SLOW arm
                // resolves the static via a callback that can run a class <clinit> → a moving
                // collection. reload_locals_after_gc restores LOCALS, but the ref STACK SVs
                // must ALSO be forwarded — thread them through the merge (fast arm passes the
                // unchanged SSA, slow arm re-reads the forwarded pointers from the spill
                // buffer). Without this a live arena ref on the stack (e.g. a half-built
                // DensityFunction across a static-singleton access) went stale on the slow arm.
                #[cfg(feature = "next-gen-gc")]
                let ref_slots: Vec<usize> = stack
                    .iter()
                    .enumerate()
                    .filter(|(_, sv)| sv.t == JT::Ref)
                    .map(|(i, _)| i)
                    .collect();
                #[cfg(not(feature = "next-gen-gc"))]
                let ref_slots: Vec<usize> = Vec::new();
                let nref = ref_slots.len();
                let fast = b.create_block();
                let slow = b.create_block();
                let merge = b.create_block();
                b.append_block_param(merge, I64);
                for _ in 0..nref {
                    b.append_block_param(merge, I64);
                }
                b.ins().brif(hit, fast, &[], slow, &[]);

                b.switch_to_block(slow);
                b.seal_block(slow);
                let cls = b.ins().iconst(I64, env.class_ptr);
                let cpv = b.ins().iconst(I64, idx as i64);
                // A ref-typed cell's refill packs via box_ref (is_ref=1); the
                // callback then skips the cell write for an unstable BOX pack.
                let isrefv = b.ins().iconst(I64, (jt == JT::Ref) as i64);
                let slotv = b.ins().iconst(I64, slot as i64);
                spill_for_gc(b, vars, stack, locals_ptr);
                let call = b.ins().call(env.getstatic_cached, &[cls, cpv, isrefv, slotv]);
                let r = b.inst_results(call)[0];
                let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![r.into()];
                #[cfg(feature = "next-gen-gc")]
                {
                    reload_locals_after_gc(b, vars, locals_ptr);
                    for &i in &ref_slots {
                        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
                        slow_args.push(rv.into());
                    }
                }
                b.ins().jump(merge, &slow_args);

                b.switch_to_block(fast);
                b.seal_block(fast);
                let cval = b.ins().load(I64, MemFlags::trusted(), cell_addr, Offset32::new(8));
                let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![cval.into()];
                for &i in &ref_slots {
                    fast_args.push(stack[i].v.into());
                }
                b.ins().jump(merge, &fast_args);

                b.switch_to_block(merge);
                b.seal_block(merge);
                let mparams: Vec<Value> = b.block_params(merge).to_vec();
                for (k, &i) in ref_slots.iter().enumerate() {
                    stack[i].v = mparams[1 + k];
                }
                let sv = cell_to_sv(b, mparams[0], jt);
                stack.push(sv);
            } else {
                let cls = b.ins().iconst(I64, env.class_ptr);
                let cpv = b.ins().iconst(I64, idx as i64);
                let is_ref = b.ins().iconst(I64, (jt == JT::Ref) as i64);
                spill_for_gc(b, vars, stack, locals_ptr);
                let call = b.ins().call(env.getstatic, &[cls, cpv, is_ref]);
                let res = b.inst_results(call)[0];
                #[cfg(feature = "next-gen-gc")]
                reload_after_gc(b, vars, stack, locals_ptr);
                let sv = cell_to_sv(b, res, jt);
                stack.push(sv);
            }
        }

        // putstatic — mutating callback (run-to-completion, no-op after a fault).
        0xb3 => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let jt = desc_to_jt(pool.member_ref(idx)?.descriptor);
            let value = stack.pop()?;
            let val_cell = to_cell(b, value);
            let cls = b.ins().iconst(I64, env.class_ptr);
            let cpv = b.ins().iconst(I64, idx as i64);
            let kind = b.ins().iconst(
                I64,
                match jt {
                    JT::Int => 0,
                    JT::Long => 1,
                    JT::Float => 2,
                    JT::Double => 3,
                    JT::Ref => 4,
                },
            );
            spill_for_gc(b, vars, stack, locals_ptr);
            b.ins().call(env.putstatic, &[cls, cpv, val_cell, kind]);
            #[cfg(feature = "next-gen-gc")]
            reload_after_gc(b, vars, stack, locals_ptr);
        }

        _ => return None,
    }
    Some(())
}
