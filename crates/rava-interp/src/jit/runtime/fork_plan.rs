//! Choosing and emitting a parallel fork.
//!
//! Between the analyses that recognise a loop shape and the fork bodies that implement one
//! sits this: try each shape in a fixed order, and if one is admitted, emit its body instead
//! of the ordinary one. The order is the whole policy, so it lives in one readable place.

use crate::jit::*;

/// What the fork attempt decided: a closed-form fold to emit, and whether a parallel body
/// was emitted (`None` = no fork was attempted, `Some(false)` = attempted and failed).
pub(crate) struct ForkOutcome {
    pub(crate) fold: Option<LoopFold>,
    pub(crate) parallel_done: Option<bool>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_and_emit_fork(
    mut b: &mut FunctionBuilder,
    module: &mut JITModule,
    bc: &[u8],
    ret: Option<JT>,
    class_ptr: i64,
    pool: &ConstantPool,
    new_arena: &std::collections::HashMap<u16, (u32, u32)>,
    ctor_inlines: &std::collections::HashMap<u16, CtorInline>,
    fields: &std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    getter_inlines: &std::collections::HashMap<u16, (u32, JT)>,
) -> ForkOutcome {
    // ⭐**TWO PROCESS CONSTANTS, READ TEN TIMES BETWEEN THEM, ONCE PER COMPILED METHOD.**
    //
    // `par` — shape before: `jit_parallel_enabled()` appeared EIGHT times in this function, once
    // as the tail conjunct of each of the seven analyser chains and once inside the `[DBG-PAR]`
    // dump. The chains' leading terms (`fold.is_none() && parallel.is_none() && …`) are true in
    // the ordinary case — nothing has been recognised yet — so every one of the eight was
    // actually evaluated. Each is an ACQUIRE load of the JIT gate word plus a branch, and an
    // acquire load is an optimisation barrier LLVM may not forward across the analyser calls
    // sitting between two of them, so eight statics' worth of barrier survived even after the
    // gates were folded into one word. Shape after: ONE read at the natural entry, eight uses.
    //
    // `dbg_par` — shape before: `std::env::var_os("RAVA_DBG_PAR")` TWICE per call. On Windows
    // that is `RtlQueryEnvironmentVariable`, a linear walk of the whole process environment
    // block, plus a heap allocation for the `OsString` it returns — for a diagnostic dump that is
    // off in every run that is not being debugged. Shape after: the engine's standard memoised
    // diagnostic gate (`jit::gates::env_gate`, which honours `RAVA_NO_ENV_MEMO`).
    //
    // Population for both: one `plan_and_emit_fork` per JIT-compiled body — ~1,500 a product
    // boot — so ~10,500 acquire loads and ~3,000 environment-block walks with their allocations.
    // ⚠Neither is a behaviour change: an env var is immutable for a VM run (the only writer is
    // the CLI's config apply, which completes before the VM exists), so reading either constant
    // once at entry instead of at each site cannot alter a verdict. Hoisting `par` does read it
    // in the folded case, where the old short-circuit skipped it; that read is pure.
    #[cfg(feature = "next-gen-gc")]
    let par = jit_parallel_enabled();
    #[cfg(feature = "next-gen-gc")]
    let dbg_par = {
        static DBG_PAR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        env_gate("RAVA_DBG_PAR", &DBG_PAR)
    };
    // LOOP-FOLDING: a whole-method invariant-accumulation loop folds to its closed
    // form (no loop). Fully isolated — any non-matching method takes the normal
    // build_body path unchanged. Gated so the default JIT stays byte-identical.
    #[cfg(feature = "next-gen-gc")]
    let fold = analyze_trivial_loop(bc, ret);
    // KIT 3 FORK CODEGEN: when RAVA_JIT_PARALLEL is set and a step-1, long-accumulator
    // data-parallel loop is recognised, compile the body as a SECOND function and emit
    // the fork (parallel_reduce + serial fallback). Body-compile / declare failures
    // leave `b` untouched and fall through to the normal serial path; a parallel emit
    // that itself bails fails the compile (the method stays interpreted), exactly like
    // the fold path. Gated → default + non-flagged builds are byte-identical.
    #[cfg(feature = "next-gen-gc")]
    let parallel = if fold.is_none() && par {
        // Kit 4 / int-acc: step-1 loops with an int OR long accumulator (capture +
        // alloc bodies handled by the codegen below).
        analyze_data_parallel_loop(bc, ret).filter(|dp| dp.step == 1).and_then(|mut dp| {
            // `cls` (the pool owner) is ONLY needed to resolve an `invokestatic Math.max/min` fold
            // (the analyzer left a placeholder op — no pool) or a ldc2_w accumulator seed. Deref it
            // LAZILY, guarded by those cps — an eager deref crashed on a caller with class_ptr==0
            // and neither cp (the unit harness). Same eager-deref hazard as the hoisted-length scan.
            if dp.fold_static_cp.is_some() || dp.acc_init_cp.is_some() {
                let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
                if let Some(cp) = dp.fold_static_cp {
                    let m = cls.file.pool.member_ref(cp)?;
                    dp.op = match (m.owner, m.name) {
                        ("java/lang/Math", "max") => crate::parallel_pool::ReduceOp::Max,
                        ("java/lang/Math", "min") => crate::parallel_pool::ReduceOp::Min,
                        _ => return None, // any other static call in the fold slot → serial
                    };
                }
                if let Some(cp) = dp.acc_init_cp {
                    dp.acc_init = match cls.file.pool.get(cp)? {
                        Constant::Long(v) => *v,
                        _ => return None, // a non-long ldc2_w seed isn't our shape
                    };
                }
            }
            Some(dp)
        })
    } else {
        None
    };
    // M2: an element-independent MAP (void method, `out[i] = f(in[i])`) — tried only when
    // it isn't a reduce. Same fork substrate, disjoint stores instead of a fold.
    #[cfg(feature = "next-gen-gc")]
    let parallel_map_plan = if fold.is_none() && parallel.is_none() && ret.is_none() && par {
        analyze_data_parallel_map(bc).filter(|m| m.step == 1).and_then(|mut m| {
            // Resolve any invokestatic in the value — only Math.max/min qualify (→ smax/smin).
            if !m.static_calls.is_empty() {
                let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
                for &(pc, cp) in &m.static_calls {
                    let mr = cls.file.pool.member_ref(cp)?;
                    let is_max = match (mr.owner, mr.name) {
                        ("java/lang/Math", "max") => true,
                        ("java/lang/Math", "min") => false,
                        _ => return None, // any other static call → serial
                    };
                    m.math_ops.push((pc, is_max));
                }
            }
            // Resolve any ldc2_w in the value → a long / double-bits constant.
            if !m.const_calls.is_empty() {
                let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
                for &(pc, cp) in &m.const_calls {
                    let v = match cls.file.pool.get(cp)? {
                        Constant::Long(v) => *v,
                        Constant::Double(d) => d.to_bits() as i64,
                        _ => return None,
                    };
                    m.const_ops.push((pc, v));
                }
            }
            Some(m)
        })
    } else {
        None
    };
    // M2c: a prefix-SCAN (void, `acc=op(acc,in[i]); out[i]=acc`) — tried only when it's not a
    // reduce/map. Forks via the 2-pass parallel_scan engine.
    #[cfg(feature = "next-gen-gc")]
    let parallel_scan_plan = if fold.is_none() && parallel.is_none() && parallel_map_plan.is_none() && ret.is_none() && par {
        analyze_data_parallel_scan(bc)
    } else {
        None
    };
    // M2d: a FILTER/compaction (`int k=0; …; return k`) — tried when it isn't a reduce/map/scan.
    // Resolve the predicate constant (ldc2_w) from the pool.
    #[cfg(feature = "next-gen-gc")]
    let parallel_filter_plan = if fold.is_none() && parallel.is_none() && parallel_map_plan.is_none() && parallel_scan_plan.is_none() && par {
        analyze_data_parallel_filter(bc).and_then(|mut ff| {
            // int filter carries its constant inline (pred_cp == 0); long resolves ldc2_w.
            if ff.pred_cp != 0 {
                let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
                ff.pred_const = match cls.file.pool.get(ff.pred_cp)? {
                    Constant::Long(v) => *v,
                    _ => return None,
                };
            }
            Some(ff)
        })
    } else {
        None
    };
    // M2e: a COUNT-where-predicate (`int c=0; …; return c`) — tried after filter.
    #[cfg(feature = "next-gen-gc")]
    let parallel_count_plan = if fold.is_none() && parallel.is_none() && parallel_map_plan.is_none() && parallel_scan_plan.is_none() && parallel_filter_plan.is_none() && par {
        analyze_data_parallel_count(bc).and_then(|mut cf| {
            if cf.pred_cp != 0 {
                let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
                cf.pred_const = match cls.file.pool.get(cf.pred_cp)? {
                    Constant::Long(v) => *v,
                    _ => return None,
                };
            }
            Some(cf)
        })
    } else {
        None
    };
    // M2f: an ARGMAX (`max=a[0]; …; return best`) — tried after count.
    #[cfg(feature = "next-gen-gc")]
    let parallel_argmax_plan = if fold.is_none() && parallel.is_none() && parallel_map_plan.is_none() && parallel_scan_plan.is_none() && parallel_filter_plan.is_none() && parallel_count_plan.is_none() && par {
        analyze_data_parallel_argmax(bc)
    } else {
        None
    };
    // M2g: a DUAL reduction (`acc1/acc2 folds; return expr(acc1,acc2)`) — tried after argmax.
    #[cfg(feature = "next-gen-gc")]
    let parallel_reduce2_plan = if fold.is_none() && parallel.is_none() && parallel_map_plan.is_none() && parallel_scan_plan.is_none() && parallel_filter_plan.is_none() && parallel_count_plan.is_none() && parallel_argmax_plan.is_none() && par {
        analyze_data_parallel_reduce2(bc)
    } else {
        None
    };
    #[cfg(feature = "next-gen-gc")]
    if dbg_par && parallel_map_plan.is_some() {
        eprintln!("[DBG-PAR] MAP recognised: {:?}", parallel_map_plan.as_ref().map(|m| (m.out_local, m.out_is_long, &m.array_captures)));
    }
    // Regression probe (RAVA_DBG_PAR): why a loop does/doesn't fork — the fold gate,
    // the analyzer verdict, and the step filter, per compiled method.
    #[cfg(feature = "next-gen-gc")]
    if bc.len() > 24 && dbg_par {
        let raw = analyze_data_parallel_loop(bc, ret);
        eprintln!(
            "[DBG-PAR] len={} fold={} par_enabled={} analyzed={:?} step_ok={}",
            bc.len(),
            fold.is_some(),
            par,
            raw.as_ref().map(|d| (d.step, d.iv_local, d.acc_local)),
            parallel.is_some()
        );
    }
    #[cfg(feature = "next-gen-gc")]
    let parallel_done: Option<bool> = if let Some(dp) = &parallel {
        // SROA context: the maps that let the body emitter scalar-replace a confined
        // `new` (erase its arena allocation into field registers).
        let sroa = SroaCtx { new_arena, ctor_inlines, fields, getters: getter_inlines };
        match build_reduce_body_fn(module, dp, bc, &sroa) {
            Some(body_ptr) => {
                let mut prsig = module.make_signature();
                for _ in 0..5 {
                    prsig.params.push(AbiParam::new(I64));
                }
                prsig.returns.push(AbiParam::new(I64));
                match module.declare_function("rava_parallel_reduce", Linkage::Import, &prsig) {
                    Ok(pr_id) => {
                        let pr_ref = module.declare_func_in_func(pr_id, b.func);
                        // Array-reduction imports: base-ptr resolver (fork + worker env)
                        // and the bounds-checked element load (serial fallback). Only
                        // referenced when dp.array_captures is non-empty, but declared
                        // unconditionally — unused FuncRefs cost nothing in the output.
                        let mut abl_sig = module.make_signature();
                        abl_sig.params.push(AbiParam::new(I64));
                        abl_sig.params.push(AbiParam::new(I64));
                        abl_sig.returns.push(AbiParam::new(I64));
                        let mut al_sig = module.make_signature();
                        for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
                        al_sig.returns.push(AbiParam::new(I64));
                        let mut alen_sig = module.make_signature();
                        alen_sig.params.push(AbiParam::new(I64));
                        alen_sig.returns.push(AbiParam::new(I64));
                        let refs = (|| {
                            let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
                            let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
                            let alen = module.declare_function("rava_arraylength", Linkage::Import, &alen_sig).ok()?;
                            Some((
                                module.declare_func_in_func(abl, b.func),
                                module.declare_func_in_func(al, b.func),
                                module.declare_func_in_func(alen, b.func),
                            ))
                        })();
                        match refs {
                            Some((abl_ref, al_ref, alen_ref)) => Some(
                                build_parallel_body(&mut b, body_ptr, pr_ref, abl_ref, al_ref, alen_ref, bc, dp, &sroa).is_some(),
                            ),
                            None => None,
                        }
                    }
                    Err(_) => None,
                }
            }
            None => None, // body not codegen-able → normal serial path (b untouched)
        }
    } else if let Some(mp) = &parallel_map_plan {
        let sroa = SroaCtx { new_arena, ctor_inlines, fields, getters: getter_inlines };
        match build_map_body_fn(module, mp, bc, &sroa) {
            Some(body_ptr) => {
                let mut pmsig = module.make_signature();
                for _ in 0..4 {
                    pmsig.params.push(AbiParam::new(I64)); // body, trip, abort_ptr, env
                }
                let mut abl_sig = module.make_signature();
                abl_sig.params.push(AbiParam::new(I64));
                abl_sig.params.push(AbiParam::new(I64));
                abl_sig.returns.push(AbiParam::new(I64));
                let mut al_sig = module.make_signature();
                for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
                al_sig.returns.push(AbiParam::new(I64));
                let mut alen_sig = module.make_signature();
                alen_sig.params.push(AbiParam::new(I64));
                alen_sig.returns.push(AbiParam::new(I64));
                let mut store_sig = module.make_signature();
                for _ in 0..3 { store_sig.params.push(AbiParam::new(I64)); }
                store_sig.returns.push(AbiParam::new(I64));
                let refs = (|| {
                    let pm = module.declare_function("rava_parallel_map", Linkage::Import, &pmsig).ok()?;
                    let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
                    let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
                    let alen = module.declare_function("rava_arraylength", Linkage::Import, &alen_sig).ok()?;
                    let ias = module.declare_function("rava_iastore", Linkage::Import, &store_sig).ok()?;
                    let las = module.declare_function("rava_lastore", Linkage::Import, &store_sig).ok()?;
                    Some((
                        module.declare_func_in_func(pm, b.func),
                        module.declare_func_in_func(abl, b.func),
                        module.declare_func_in_func(al, b.func),
                        module.declare_func_in_func(alen, b.func),
                        module.declare_func_in_func(ias, b.func),
                        module.declare_func_in_func(las, b.func),
                    ))
                })();
                refs.map(|(pm_ref, abl_ref, al_ref, alen_ref, ias_ref, las_ref)| {
                    build_parallel_map(&mut b, body_ptr, pm_ref, abl_ref, al_ref, alen_ref, ias_ref, las_ref, bc, mp, &sroa).is_some()
                })
            }
            None => None,
        }
    } else if let Some(sp) = &parallel_scan_plan {
        let mut pssig = module.make_signature();
        for _ in 0..6 {
            pssig.params.push(AbiParam::new(I64)); // trip, out_base, in_base, op, elem_bytes, abort_ptr
        }
        let mut abl_sig = module.make_signature();
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.returns.push(AbiParam::new(I64));
        let mut al_sig = module.make_signature();
        for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
        al_sig.returns.push(AbiParam::new(I64));
        let mut store_sig = module.make_signature();
        for _ in 0..3 { store_sig.params.push(AbiParam::new(I64)); }
        store_sig.returns.push(AbiParam::new(I64));
        let refs = (|| {
            let ps = module.declare_function("rava_parallel_scan", Linkage::Import, &pssig).ok()?;
            let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
            let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
            let las = module.declare_function("rava_lastore", Linkage::Import, &store_sig).ok()?;
            let ias = module.declare_function("rava_iastore", Linkage::Import, &store_sig).ok()?;
            Some((
                module.declare_func_in_func(ps, b.func),
                module.declare_func_in_func(abl, b.func),
                module.declare_func_in_func(al, b.func),
                module.declare_func_in_func(las, b.func),
                module.declare_func_in_func(ias, b.func),
            ))
        })();
        refs.map(|(ps_ref, abl_ref, al_ref, las_ref, ias_ref)| {
            build_parallel_scan(&mut b, ps_ref, abl_ref, al_ref, las_ref, ias_ref, sp).is_some()
        })
    } else if let Some(fp) = &parallel_filter_plan {
        let mut pfsig = module.make_signature();
        for _ in 0..7 { pfsig.params.push(AbiParam::new(I64)); } // trip,out,in,cmp,cst,w,abort
        pfsig.returns.push(AbiParam::new(I64)); // surviving count
        let mut abl_sig = module.make_signature();
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.returns.push(AbiParam::new(I64));
        let mut al_sig = module.make_signature();
        for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
        al_sig.returns.push(AbiParam::new(I64));
        let mut store_sig = module.make_signature();
        for _ in 0..3 { store_sig.params.push(AbiParam::new(I64)); }
        store_sig.returns.push(AbiParam::new(I64));
        let refs = (|| {
            let pf = module.declare_function("rava_parallel_filter", Linkage::Import, &pfsig).ok()?;
            let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
            let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
            let las = module.declare_function("rava_lastore", Linkage::Import, &store_sig).ok()?;
            let ias = module.declare_function("rava_iastore", Linkage::Import, &store_sig).ok()?;
            Some((
                module.declare_func_in_func(pf, b.func),
                module.declare_func_in_func(abl, b.func),
                module.declare_func_in_func(al, b.func),
                module.declare_func_in_func(las, b.func),
                module.declare_func_in_func(ias, b.func),
            ))
        })();
        refs.map(|(pf_ref, abl_ref, al_ref, las_ref, ias_ref)| {
            build_parallel_filter(&mut b, pf_ref, abl_ref, al_ref, las_ref, ias_ref, fp).is_some()
        })
    } else if let Some(cf) = &parallel_count_plan {
        let mut pcsig = module.make_signature();
        for _ in 0..6 { pcsig.params.push(AbiParam::new(I64)); } // trip,in,cmp,cst,w,abort
        pcsig.returns.push(AbiParam::new(I64)); // count
        let mut abl_sig = module.make_signature();
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.returns.push(AbiParam::new(I64));
        let mut al_sig = module.make_signature();
        for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
        al_sig.returns.push(AbiParam::new(I64));
        let refs = (|| {
            let pcf = module.declare_function("rava_parallel_count", Linkage::Import, &pcsig).ok()?;
            let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
            let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
            Some((
                module.declare_func_in_func(pcf, b.func),
                module.declare_func_in_func(abl, b.func),
                module.declare_func_in_func(al, b.func),
            ))
        })();
        refs.map(|(pc_ref, abl_ref, al_ref)| {
            build_parallel_count(&mut b, pc_ref, abl_ref, al_ref, cf).is_some()
        })
    } else if let Some(am) = &parallel_argmax_plan {
        let mut pasig = module.make_signature();
        for _ in 0..5 { pasig.params.push(AbiParam::new(I64)); } // trip,in,want_max,w,abort
        pasig.returns.push(AbiParam::new(I64)); // index
        let mut abl_sig = module.make_signature();
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.params.push(AbiParam::new(I64));
        abl_sig.returns.push(AbiParam::new(I64));
        let mut al_sig = module.make_signature();
        for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
        al_sig.returns.push(AbiParam::new(I64));
        let refs = (|| {
            let pa = module.declare_function("rava_parallel_argmax", Linkage::Import, &pasig).ok()?;
            let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
            let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
            Some((
                module.declare_func_in_func(pa, b.func),
                module.declare_func_in_func(abl, b.func),
                module.declare_func_in_func(al, b.func),
            ))
        })();
        refs.map(|(pa_ref, abl_ref, al_ref)| {
            build_parallel_argmax(&mut b, pa_ref, abl_ref, al_ref, am).is_some()
        })
    } else if let Some(rp) = &parallel_reduce2_plan {
        let sroa = SroaCtx { new_arena, ctor_inlines, fields, getters: getter_inlines };
        match build_reduce2_body_fn(module, rp, bc, &sroa) {
            Some(bptr) => {
                let mut pr2sig = module.make_signature();
                for _ in 0..7 { pr2sig.params.push(AbiParam::new(I64)); } // trip,body,env,op1,op2,out,abort
                let mut abl_sig = module.make_signature();
                abl_sig.params.push(AbiParam::new(I64));
                abl_sig.params.push(AbiParam::new(I64));
                abl_sig.returns.push(AbiParam::new(I64));
                let mut al_sig = module.make_signature();
                for _ in 0..3 { al_sig.params.push(AbiParam::new(I64)); }
                al_sig.returns.push(AbiParam::new(I64));
                let refs = (|| {
                    let pr2 = module.declare_function("rava_parallel_reduce2", Linkage::Import, &pr2sig).ok()?;
                    let abl = module.declare_function("rava_array_base_len", Linkage::Import, &abl_sig).ok()?;
                    let al = module.declare_function("rava_aload", Linkage::Import, &al_sig).ok()?;
                    Some((
                        module.declare_func_in_func(pr2, b.func),
                        module.declare_func_in_func(abl, b.func),
                        module.declare_func_in_func(al, b.func),
                    ))
                })();
                refs.map(|(pr2_ref, abl_ref, al_ref)| {
                    build_parallel_reduce2(&mut b, bptr, pr2_ref, abl_ref, al_ref, bc, rp, &sroa).is_some()
                })
            }
            None => None,
        }
    } else {
        None
    };

    ForkOutcome { fold, parallel_done }
}
