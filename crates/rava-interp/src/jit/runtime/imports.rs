//! Every host callback the compiled code can reach, declared once per compile.
//!
//! Emission asks the module for a FuncRef per site; this is the one place that says which
//! symbols exist at all, so the callback surface is readable as a list instead of as three
//! hundred lines wedged inside the compile driver.

use cranelift_module::FuncId;
use crate::jit::*;

pub(crate) struct Imports {
    pub(crate) gf_id: FuncId,
    pub(crate) pf_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) rsg_id: FuncId,
    pub(crate) al_id: FuncId,
    pub(crate) ld_id: FuncId,
    pub(crate) gs_id: FuncId,
    pub(crate) gsc_id: FuncId,
    pub(crate) ps_id: FuncId,
    pub(crate) new_id: FuncId,
    pub(crate) iof_id: FuncId,
    pub(crate) cast_id: FuncId,
    pub(crate) npe_site_id: FuncId,
    pub(crate) na_id: FuncId,
    pub(crate) ana_id: FuncId,
    pub(crate) dr_id: FuncId,
    pub(crate) ldr_id: FuncId,
    pub(crate) frem_id: FuncId,
    pub(crate) drem_id: FuncId,
    pub(crate) ias_id: FuncId,
    pub(crate) das_id: FuncId,
    pub(crate) safepoint_id: Option<FuncId>,
    pub(crate) ac_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) cm_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) cur_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) jca_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) asp_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) dpa_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) dm_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ic_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) hob_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) hps_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) dmm_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ce_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) cx_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) mp_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) mg_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) mr_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) mc_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amg_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amc_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amp_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amr_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amb_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amd_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bi_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ui_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) indy_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ldc_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) sl_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) sh_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ssw_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) sca_id: FuncId,
    pub(crate) af_id: FuncId,
    pub(crate) alv_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bv_id: FuncId,
    pub(crate) aas_id: FuncId,
    pub(crate) inv_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) resolve_reg_id: FuncId,
    pub(crate) throw_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fu_inv_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) deopt_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) enum_ord_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bs_get_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bs_set_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bs_clear_id: FuncId,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bs_isempty_id: FuncId,
}

pub(crate) fn declare_imports(module: &mut JITModule, poll_backedges: bool) -> Option<Imports> {
    // Import signatures: (i64,i64,i64)->i64 for getfield/aload (3rd = is_ref),
    // (i64)->i64 for arrlen.
    let mut sig3 = module.make_signature();
    for _ in 0..3 {
        sig3.params.push(AbiParam::new(I64));
    }
    sig3.returns.push(AbiParam::new(I64));
    let mut sig1 = module.make_signature();
    sig1.params.push(AbiParam::new(I64));
    sig1.returns.push(AbiParam::new(I64));
    // (x, y) -> i64 for rava_acmp.
    let mut sig2 = module.make_signature();
    sig2.params.push(AbiParam::new(I64));
    sig2.params.push(AbiParam::new(I64));
    sig2.returns.push(AbiParam::new(I64));
    let safepoint_id = if poll_backedges {
        let mut safepoint_sig = module.make_signature();
        safepoint_sig.returns.push(AbiParam::new(I64));
        Some(
            module
                .declare_function("rava_jit_safepoint", Linkage::Import, &safepoint_sig)
                .ok()?,
        )
    } else {
        None
    };
    // (obj, fid, val, kind) -> i64 for rava_putfield.
    let mut sig4 = module.make_signature();
    for _ in 0..4 {
        sig4.params.push(AbiParam::new(I64));
    }
    sig4.returns.push(AbiParam::new(I64));
    let gf_id = module
        .declare_function("rava_getfield", Linkage::Import, &sig3)
        .ok()?;
    let pf_id = module
        .declare_function("rava_putfield", Linkage::Import, &sig4)
        .ok()?;
    // rava_ring_store_guard: (arr) -> i64 (1 = skip the store, 0 = store inline).
    #[cfg(feature = "next-gen-gc")]
    let rsg_id = module
        .declare_function("rava_ring_store_guard", Linkage::Import, &sig1)
        .ok()?;
    let al_id = module
        .declare_function("rava_arraylength", Linkage::Import, &sig1)
        .ok()?;
    let ld_id = module
        .declare_function("rava_aload", Linkage::Import, &sig3)
        .ok()?;
    // rava_getstatic: (caller_ptr, cp, is_ref) -> i64.
    let gs_id = module
        .declare_function("rava_getstatic", Linkage::Import, &sig3)
        .ok()?;
    // rava_getstatic_cached: (caller_ptr, cp, is_ref, slot) -> i64 — same as
    // getstatic, but also writes the value cell `slot` (gen + packed) so the
    // native fast path can skip the callback next time.
    let gsc_id = module
        .declare_function("rava_getstatic_cached", Linkage::Import, &sig4)
        .ok()?;
    let ps_id = module
        .declare_function("rava_putstatic", Linkage::Import, &sig4)
        .ok()?;
    let new_id = module
        .declare_function("rava_new", Linkage::Import, &sig2)
        .ok()?;
    let iof_id = module
        .declare_function("rava_instanceof", Linkage::Import, &sig3)
        .ok()?;
    let cast_id = module
        .declare_function("rava_checkcast", Linkage::Import, &sig3)
        .ok()?;
    let npe_site_id = module
        .declare_function("rava_npe_invoke_site", Linkage::Import, &sig3)
        .ok()?;
    let na_id = module
        .declare_function("rava_newarray", Linkage::Import, &sig2)
        .ok()?;
    let ana_id = module
        .declare_function("rava_anewarray", Linkage::Import, &sig3)
        .ok()?;
    // rava_i32_divrem: (lhs, rhs, is_rem) -> i64.
    let dr_id = module
        .declare_function("rava_i32_divrem", Linkage::Import, &sig3)
        .ok()?;
    // rava_i64_divrem: (lhs, rhs, is_rem) -> i64 (same shape as the i32 form).
    let ldr_id = module
        .declare_function("rava_i64_divrem", Linkage::Import, &sig3)
        .ok()?;
    // rava_f32_rem / rava_f64_rem: floating remainder libcalls.
    let mut sigf32 = module.make_signature();
    sigf32.params.push(AbiParam::new(F32));
    sigf32.params.push(AbiParam::new(F32));
    sigf32.returns.push(AbiParam::new(F32));
    let frem_id = module
        .declare_function("rava_f32_rem", Linkage::Import, &sigf32)
        .ok()?;
    let mut sigf64 = module.make_signature();
    sigf64.params.push(AbiParam::new(F64));
    sigf64.params.push(AbiParam::new(F64));
    sigf64.returns.push(AbiParam::new(F64));
    let drem_id = module
        .declare_function("rava_f64_rem", Linkage::Import, &sigf64)
        .ok()?;
    // rava_iastore: (arr, idx, value) -> ignored i64.
    let ias_id = module
        .declare_function("rava_iastore", Linkage::Import, &sig3)
        .ok()?;
    // rava_dastore: (arr, idx, double_bits) -> ignored i64.
    let das_id = module
        .declare_function("rava_dastore", Linkage::Import, &sig3)
        .ok()?;
    // rava_acmp: (x, y) -> i64 (1/0).
    let ac_id = module
        .declare_function("rava_acmp", Linkage::Import, &sig2)
        .ok()?;
    // rava_card_mark: (obj, val) -> i64 (ignored). Inline arena putfield-ref barrier.
    #[cfg(feature = "next-gen-gc")]
    let cm_id = module
        .declare_function("rava_card_mark", Linkage::Import, &sig2)
        .ok()?;
    // rava_jit_cursor_addr / rava_arena_safepoint: () -> i64. Inline TLAB bump
    // cursor fetch + nursery-full safepoint.
    #[cfg(feature = "next-gen-gc")]
    let mut sig0 = module.make_signature();
    #[cfg(feature = "next-gen-gc")]
    sig0.returns.push(AbiParam::new(I64));
    #[cfg(feature = "next-gen-gc")]
    let cur_id = module
        .declare_function("rava_jit_cursor_addr", Linkage::Import, &sig0)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let jca_id = module
        .declare_function("rava_jit_ctx_addr", Linkage::Import, &sig0)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let asp_id = module
        .declare_function("rava_arena_safepoint", Linkage::Import, &sig0)
        .ok()?;
    // rava_arena_deopt_alloc: (size) -> i64. The BOUNDED EXIT from the inline-alloc
    // retry loop — see `emit_inline_arena_new`'s `bail` block.
    #[cfg(feature = "next-gen-gc")]
    let dpa_id = module
        .declare_function("rava_arena_deopt_alloc", Linkage::Import, &sig1)
        .ok()?;
    // rava_dispatch_miss: (cache, recv, caller, cp, kind, buf, n) -> i64.
    #[cfg(feature = "next-gen-gc")]
    let dm_id = {
        let mut sig8 = module.make_signature();
        for _ in 0..8 {
            sig8.params.push(AbiParam::new(I64));
        }
        sig8.returns.push(AbiParam::new(I64));
        module
            .declare_function("rava_dispatch_miss", Linkage::Import, &sig8)
            .ok()?
    };
    // rava_ic_base: () -> i64 (11.3). This thread's per-thread IC table base (0 = main/none).
    #[cfg(feature = "next-gen-gc")]
    let ic_id = module
        .declare_function("rava_ic_base", Linkage::Import, &sig0)
        .ok()?;
    // 23.4 heap-Ref PIC: rava_heap_objects_base () -> i64, rava_heap_pic_shadow (recv, key) -> i64.
    #[cfg(feature = "next-gen-gc")]
    let hob_id = module
        .declare_function("rava_heap_objects_base", Linkage::Import, &sig0)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let hps_id = module
        .declare_function("rava_heap_pic_shadow", Linkage::Import, &sig2)
        .ok()?;
    // 23.9 rava_direct_miss: (cache, caller, cp, kind, buf, n) -> i64.
    #[cfg(feature = "next-gen-gc")]
    let dmm_id = {
        let mut sig6 = module.make_signature();
        for _ in 0..6 {
            sig6.params.push(AbiParam::new(I64));
        }
        sig6.returns.push(AbiParam::new(I64));
        module.declare_function("rava_direct_miss", Linkage::Import, &sig6).ok()?
    };
    #[cfg(feature = "next-gen-gc")]
    let ce_id = module
        .declare_function("rava_cells_push_slow", Linkage::Import, &sig2)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let cx_id = module
        .declare_function("rava_box_exit", Linkage::Import, &sig2)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let mp_id = module
        .declare_function("rava_map_put", Linkage::Import, &sig3)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let mg_id = module
        .declare_function("rava_map_get", Linkage::Import, &sig2)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let mr_id = module
        .declare_function("rava_map_remove", Linkage::Import, &sig2)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let mc_id = module
        .declare_function("rava_map_contains", Linkage::Import, &sig2)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let amg_id = module.declare_function("rava_amap_get", Linkage::Import, &sig2).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let amc_id = module.declare_function("rava_amap_contains", Linkage::Import, &sig2).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let amp_id = module.declare_function("rava_amap_put", Linkage::Import, &sig3).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let amr_id = module.declare_function("rava_amap_remove", Linkage::Import, &sig2).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let amb_id = module.declare_function("rava_amap_box_value", Linkage::Import, &sig1).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let amd_id = module.declare_function("rava_amap_default", Linkage::Import, &sig1).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let bi_id = module.declare_function("rava_box_int", Linkage::Import, &sig1).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let ui_id = module.declare_function("rava_unbox_int", Linkage::Import, &sig1).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let indy_id = module.declare_function("rava_indy", Linkage::Import, &sig4).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let ldc_id = module.declare_function("rava_ldc", Linkage::Import, &sig2).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let sl_id = module.declare_function("rava_str_length", Linkage::Import, &sig1).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let sh_id = module.declare_function("rava_str_hash", Linkage::Import, &sig1).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let ssw_id = module.declare_function("rava_str_startswith", Linkage::Import, &sig2).ok()?;
    #[cfg(feature = "next-gen-gc")]
    let sca_id = module.declare_function("rava_str_charat", Linkage::Import, &sig2).ok()?;
    // rava_array_fault: (arr, idx) -> i64 (ignored).
    let af_id = module
        .declare_function("rava_array_fault", Linkage::Import, &sig2)
        .ok()?;
    // rava_aload_verify: (arr, idx, native_bits, site) -> i64 (ignored). Diagnostic.
    let mut sig4v = module.make_signature();
    for _ in 0..4 {
        sig4v.params.push(AbiParam::new(I64));
    }
    sig4v.returns.push(AbiParam::new(I64));
    let alv_id = module
        .declare_function("rava_aload_verify", Linkage::Import, &sig4v)
        .ok()?;
    // rava_bare_verify: (cell, fid, slot) -> i64 (ignored). Diagnostic, see `rava_bare_verify`.
    #[cfg(feature = "next-gen-gc")]
    let bv_id = module
        .declare_function("rava_bare_verify", Linkage::Import, &sig3)
        .ok()?;
    // rava_aastore: (arr, idx, val) -> i64 (ignored).
    let aas_id = module
        .declare_function("rava_aastore", Linkage::Import, &sig3)
        .ok()?;
    // rava_invoke: (caller_ptr, cp, kind, buf_ptr, n) -> i64.
    let mut sig5 = module.make_signature();
    for _ in 0..5 {
        sig5.params.push(AbiParam::new(I64));
    }
    sig5.returns.push(AbiParam::new(I64));
    let inv_id = module
        .declare_function("rava_invoke", Linkage::Import, &sig5)
        .ok()?;
    // Register-ABI arc (I1b): rava_resolve_reg(caller_ptr, cp) -> i64 (sig2).
    #[cfg(feature = "next-gen-gc")]
    let resolve_reg_id = module
        .declare_function("rava_resolve_reg", Linkage::Import, &sig2)
        .ok()?;
    let throw_id = module
        .declare_function("rava_throw", Linkage::Import, &sig1)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let fu_inv_id = module
        .declare_function("rava_fu_invoke", Linkage::Import, &sig5)
        .ok()?;
    // (class, code, site, buf) -> packed result. The uncommon-trap exit: see
    // `jit::callbacks::deopt`.
    #[cfg(feature = "next-gen-gc")]
    let deopt_id = module
        .declare_function("rava_deopt", Linkage::Import, &sig4)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let enum_ord_id = module
        .declare_function("rava_enum_ordinal", Linkage::Import, &sig5)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let bs_get_id = module
        .declare_function("rava_bitset_get", Linkage::Import, &sig5)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let bs_set_id = module
        .declare_function("rava_bitset_set", Linkage::Import, &sig5)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let bs_clear_id = module
        .declare_function("rava_bitset_clear", Linkage::Import, &sig5)
        .ok()?;
    #[cfg(feature = "next-gen-gc")]
    let bs_isempty_id = module
        .declare_function("rava_bitset_isempty", Linkage::Import, &sig5)
        .ok()?;

    Some(Imports {
        #[cfg(feature = "next-gen-gc")]
        deopt_id,
        gf_id,
        pf_id,
        #[cfg(feature = "next-gen-gc")]
        rsg_id,
        al_id,
        ld_id,
        gs_id,
        gsc_id,
        ps_id,
        new_id,
        iof_id,
        cast_id,
        npe_site_id,
        na_id,
        ana_id,
        dr_id,
        ldr_id,
        frem_id,
        drem_id,
        ias_id,
        das_id,
        safepoint_id,
        ac_id,
        #[cfg(feature = "next-gen-gc")]
        cm_id,
        #[cfg(feature = "next-gen-gc")]
        cur_id,
        #[cfg(feature = "next-gen-gc")]
        jca_id,
        #[cfg(feature = "next-gen-gc")]
        asp_id,
        #[cfg(feature = "next-gen-gc")]
        dpa_id,
        #[cfg(feature = "next-gen-gc")]
        dm_id,
        #[cfg(feature = "next-gen-gc")]
        ic_id,
        #[cfg(feature = "next-gen-gc")]
        hob_id,
        #[cfg(feature = "next-gen-gc")]
        hps_id,
        #[cfg(feature = "next-gen-gc")]
        dmm_id,
        #[cfg(feature = "next-gen-gc")]
        ce_id,
        #[cfg(feature = "next-gen-gc")]
        cx_id,
        #[cfg(feature = "next-gen-gc")]
        mp_id,
        #[cfg(feature = "next-gen-gc")]
        mg_id,
        #[cfg(feature = "next-gen-gc")]
        mr_id,
        #[cfg(feature = "next-gen-gc")]
        mc_id,
        #[cfg(feature = "next-gen-gc")]
        amg_id,
        #[cfg(feature = "next-gen-gc")]
        amc_id,
        #[cfg(feature = "next-gen-gc")]
        amp_id,
        #[cfg(feature = "next-gen-gc")]
        amr_id,
        #[cfg(feature = "next-gen-gc")]
        amb_id,
        #[cfg(feature = "next-gen-gc")]
        amd_id,
        #[cfg(feature = "next-gen-gc")]
        bi_id,
        #[cfg(feature = "next-gen-gc")]
        ui_id,
        #[cfg(feature = "next-gen-gc")]
        indy_id,
        #[cfg(feature = "next-gen-gc")]
        ldc_id,
        #[cfg(feature = "next-gen-gc")]
        sl_id,
        #[cfg(feature = "next-gen-gc")]
        sh_id,
        #[cfg(feature = "next-gen-gc")]
        ssw_id,
        #[cfg(feature = "next-gen-gc")]
        sca_id,
        af_id,
        alv_id,
        #[cfg(feature = "next-gen-gc")]
        bv_id,
        aas_id,
        inv_id,
        #[cfg(feature = "next-gen-gc")]
        resolve_reg_id,
        throw_id,
        #[cfg(feature = "next-gen-gc")]
        fu_inv_id,
        #[cfg(feature = "next-gen-gc")]
        enum_ord_id,
        #[cfg(feature = "next-gen-gc")]
        bs_get_id,
        #[cfg(feature = "next-gen-gc")]
        bs_set_id,
        #[cfg(feature = "next-gen-gc")]
        bs_clear_id,
        #[cfg(feature = "next-gen-gc")]
        bs_isempty_id,
    })
}
