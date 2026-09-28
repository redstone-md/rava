//! Everything a compile needs resolved before it starts, gathered in one pass.
//!
//! Inline targets, static intrinsics, the getstatic value cells, and the arena/ctor/getter/
//! concat/HOF plans. Reservations made here join the compile's rollback, so a refused compile
//! leaks no cells.

use crate::*;
use crate::class_loader::LoadedClass;
use rava_classfile::Code;
use std::sync::Arc;

pub(crate) struct CompilePlans {
    pub(crate) static_intrinsics: std::collections::HashMap<u16, jit::StaticIntrinsic>,
    pub(crate) inline_targets: std::collections::HashMap<u16, jit::InlineCallee>,
    pub(crate) native_calls: std::collections::HashSet<u16>,
    pub(crate) statics: std::collections::HashMap<u16, (u32, jit::JT)>,
    pub(crate) slot_mark: u32,
    pub(crate) inline_reserved: u32,
    pub(crate) reserved: u32,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) new_arena: std::collections::HashMap<u16, (u32, u32)>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ctor_inlines: std::collections::HashMap<u16, jit::CtorInline>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) getter_inlines: std::collections::HashMap<u16, (u32, jit::JT)>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) concat_shapes: std::collections::HashMap<u16, jit::ConcatShape>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) hof_fusions: std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) prim_arrays: jit::ArenaPrimArrayPlan,
}

impl Vm {
    pub(crate) fn jit_plans(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
        name: &str,
        descriptor: &str,
        is_static: bool,
        #[cfg(feature = "next-gen-gc")]
        prebuilt_hof_fusions: Option<
            std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
        >,
    ) -> Option<CompilePlans> {
        let _phase = crate::jit_phase_trace::enter("plans", code as *const Code as usize);
        // Nested compilation must finish before `static_slot_mark()`: rollback relies on this
        // plan's reservation block remaining contiguous at the table top.
        let static_intrinsics = self.collect_static_intrinsics(code, class);
        #[allow(unused_mut)]
        let mut inline_targets = self.collect_inline_targets(code, class);
        // RAVA_REF_STATIC_CELLS: assign getstatic value cells to the INLINED
        // callees' own sites (keyed by each callee's cp indices — emit_inline
        // runs the body under `callee.statics`). Without this every inlined
        // getstatic pays the full callback forever: the fallback tally billed
        // SimplexNoise.GRADIENT (read from the inlined noise leaf) at 101M
        // callbacks in one ChunkFill. Reservations join this compile's
        // rollback below, so a refused compile leaks no cells.
        // High-water mark BEFORE this compile reserves anything, so a rollback can
        // prove the block is still on top (see `Heap::free_static_slots_from`).
        let slot_mark = self.heap.static_slot_mark();
        let inline_reserved = if jit::ref_static_cells_enabled() {
            fn assign(heap: &mut Heap, c: &mut jit::InlineCallee) -> u32 {
                let sites =
                    jit::primitive_getstatic_cps(&c.bytecode, &c.class.file.pool);
                let n = sites.len() as u32;
                let mut total = 0;
                if n > 0 {
                    if let Some(base) = heap.reserve_static_slots(n) {
                        c.statics = sites
                            .iter()
                            .enumerate()
                            .map(|(i, (cp, jt))| (*cp, (base + i as u32, *jt)))
                            .collect();
                        total += n;
                    }
                }
                for nc in c.nested.values_mut() {
                    total += assign(heap, nc);
                }
                total
            }
            let mut total = 0u32;
            for c in inline_targets.values_mut() {
                total += assign(&mut self.heap, c);
            }
            total
        } else {
            0
        };
        #[allow(unused_mut)]
        let mut native_calls = self.collect_compiled_callee_sites(code, class);
        let pool = &class.file.pool;
        // Assign a getstatic value cell to each PRIMITIVE getstatic site (in
        // bytecode order) so compiled code can read them natively. Reserve the
        // block up front (sequential — released before the intern closure
        // borrows self); roll back if the method ultimately doesn't compile so
        // rejected methods don't leak the fixed cell table.
        let mut gs_sites = jit::primitive_getstatic_cps(&code.bytecode, pool);
        let mut gs_n = gs_sites.len() as u32;
        let mut gs_base = if gs_n > 0 { self.heap.reserve_static_slots(gs_n) } else { None };
        // RAVA_REF_STATIC_CELLS: if the fixed cell table cannot fit the
        // ref-extended set, degrade to the primitives-only set rather than
        // letting the flag starve primitive cells for this method.
        if gs_base.is_none() && gs_sites.iter().any(|&(_, jt)| jt == jit::JT::Ref) {
            gs_sites.retain(|&(_, jt)| jt != jit::JT::Ref);
            gs_n = gs_sites.len() as u32;
            gs_base = if gs_n > 0 { self.heap.reserve_static_slots(gs_n) } else { None };
        }
        let statics: std::collections::HashMap<u16, (u32, jit::JT)> =
            match gs_base {
                Some(base) => gs_sites
                    .iter()
                    .enumerate()
                    .map(|(i, (cp, jt))| (*cp, (base + i as u32, *jt)))
                    .collect(),
                None => std::collections::HashMap::new(),
            };
        let reserved = if statics.is_empty() { 0 } else { gs_n };
        // INLINE TLAB BUMP: resolve which `new` sites allocate arena-eligible,
        // already-initialised classes so compiled code bump-allocates them
        // inline (no callback). Built before the compile closure borrows self.
        #[cfg(feature = "next-gen-gc")]
        let new_arena = self.build_new_arena(code, class);
        // CONSTRUCTOR INLINER: resolve trivial arena `<init>` invokespecial
        // sites to inline store plans (no rava_invoke). Credit them as native
        // in the callback_heavy gate (they fold to direct stores).
        #[cfg(feature = "next-gen-gc")]
        let ctor_inlines = self.build_ctor_inlines(code, class);
        #[cfg(feature = "next-gen-gc")]
        native_calls.extend(ctor_inlines.keys().copied());
        // SYNERGY LAYER 1: trivial-getter inline plan for the data-parallel body.
        #[cfg(feature = "next-gen-gc")]
        let getter_inlines = self.build_getter_inlines(code, class);
        // INLINE CONCAT: resolve all-int/ASCII indy concat sites to an
        // inline itoa plan (no rava_indy callback). Credit them as native.
        #[cfg(feature = "next-gen-gc")]
        let concat_shapes = self.build_concat_shapes(code, class);
        #[cfg(feature = "next-gen-gc")]
        native_calls.extend(concat_shapes.keys().copied());
        // Default-off structural HOF fusion. Eligibility comes only from
        // linked lambda capability + bytecode/CFG/descriptor proofs.
        #[cfg(feature = "next-gen-gc")]
        let hof_fusions = prebuilt_hof_fusions
            .unwrap_or_else(|| self.build_hof_fusions(code, class, descriptor, is_static));
        // Opt-in tracer/codegen plan. The UNSET path performs no bytecode analysis.
        #[cfg(feature = "next-gen-gc")]
        let prim_arrays = if jit::arena_primitive_arrays_enabled() {
            jit::analyze_arena_primitive_arrays(code)
        } else {
            jit::ArenaPrimArrayPlan::default()
        };
        // A `fu_routable` invoke lowers to `rava_fu_invoke` (a native collection
        // serve off the backing array — cheap), NOT a weight-3 method callback.
        // Count those sites as native so a thin delegating wrapper (e.g.
        // `VoxelShape.get` = getCoords().getDouble(), `EntrySet$1.next`) is not
        // wrongly judged callback-heavy and left interpreted forever. Likewise
        // `Object.<init>()V`: the JIT emits NOTHING for it (empty root ctor), so a
        // trivial super-ctor (`aload_0; invokespecial Object.<init>; return` —
        // fastutil AbstractDouble* chains, millions of boot calls) must not be
        // judged callback-heavy by its one zero-cost invoke.
        #[cfg(feature = "next-gen-gc")]
        for (cp, _op) in jit::invoke_indices(code) {
            if let Some(m) = pool.member_ref(cp) {
                if jit::fu_routable(m.owner, m.name, m.descriptor)
                    || (m.owner == "java/lang/Object"
                        && m.name == "<init>"
                        && m.descriptor == "()V")
                {
                    native_calls.insert(cp);
                }
            }
        }
        // KIT 3 (recognition seam): when RAVA_JIT_PARALLEL is set, detect a
        // counted associative-accumulator data-parallel loop. Today this REPORTS
        // eligibility (the analyzer running on real method bytecode) — the fork
        // codegen plugs in HERE, emitting a `RavaPool::parallel_reduce` call over
        // a specialised `ReduceBody` for the loop, with a serial GIL fallback on
        // RegionAbort. Gated + diagnostic-only → the compiled output is unchanged.
        #[cfg(feature = "next-gen-gc")]
        if std::env::var("RAVA_JIT_PARALLEL").is_ok() {
            if let Some(dp) = jit::analyze_data_parallel_loop(&code.bytecode, None) {
                let trip = match dp.limit_local {
                    Some(l) => format!("runtime(L{l}, guard>={})", jit::MIN_PARALLEL_TRIP),
                    None => format!("const {}", dp.lim_const),
                };
                eprintln!(
                    "[JIT-PARALLEL] {}.{}{} → data-parallel: op={:?} iv=L{} acc=L{}{} c0={} step={} trip={trip} body=[{}..{}] — RavaPool::parallel_reduce ready",
                    class.file.this_class, name, descriptor, dp.op, dp.iv_local, dp.acc_local,
                    if dp.acc_is_long { "(long)" } else { "(int)" },
                    dp.c0, dp.step, dp.body_range.0, dp.body_range.1,
                );
            }
        }
        // ESCAPE ANALYSIS (recognition seam): when RAVA_JIT_SROA is set, run
        // the escape-analysis pass and report which `new` sites are confined
        // (Scalar-Replacement candidates — their allocation can be erased) vs
        // escaping. Diagnostic-only today; the SROA materialization codegen
        // (fields → SSA / stack, no arena alloc) plugs in HERE.
        // V1-Tracer (Escape-VOA): INERT logging of scalar-replacement candidates.
        // For each `new` site, classify CONFINED (alloc erasable now) / CF-BLOCKED
        // (would be confined under a CF-aware detector — the addressable frontier)
        // / ESCAPES (genuine sink). Aggregate the [VOA] lines by class to rank
        // candidates. Read-only ⇒ bit-exact. RAVA_JIT_SROA kept as an alias.
        // V2 CF-aware tracer: run BOTH the straight-line and the CFG-dataflow
        // escape analyses; log the CFG verdict and flag CF-BLOCKED→CONFINED
        // promotions (the objects the flow-sensitive detector newly proves
        // confined). Read-only ⇒ bit-exact. RAVA_JIT_SROA kept as an alias.
        #[cfg(feature = "next-gen-gc")]
        if std::env::var_os("RAVA_DBG_VOA").is_some() || std::env::var_os("RAVA_JIT_SROA").is_some() {
            let sl = jit::analyze_escape(&code.bytecode, pool);
            let cf = jit::analyze_escape_cfg(&code.bytecode, pool, !code.exceptions.is_empty());
            for ((pc, sl_r), (_, cf_r)) in sl.iter().zip(cf.iter()) {
                let cpi = ((code.bytecode[*pc + 1] as u16) << 8) | code.bytecode[*pc + 2] as u16;
                let cls = pool.class_name(cpi).unwrap_or("?");
                let verdict = match cf_r {
                    jit::EscapeReason::Confined => "CONFINED",
                    jit::EscapeReason::ControlFlow => "CF-BLOCKED",
                    jit::EscapeReason::Sink => "ESCAPES",
                };
                let promo = if *sl_r == jit::EscapeReason::ControlFlow
                    && *cf_r == jit::EscapeReason::Confined
                {
                    " *CF-PROMOTED"
                } else {
                    ""
                };
                eprintln!("[VOA] {verdict} new={cls} @pc{pc} in {}.{}{promo}", class.file.this_class, name);
            }
        }
        Some(CompilePlans {
            static_intrinsics,
            inline_targets,
            native_calls,
            statics,
            slot_mark,
            inline_reserved,
            reserved,
            #[cfg(feature = "next-gen-gc")]
            new_arena,
            #[cfg(feature = "next-gen-gc")]
            ctor_inlines,
            #[cfg(feature = "next-gen-gc")]
            getter_inlines,
            #[cfg(feature = "next-gen-gc")]
            concat_shapes,
            #[cfg(feature = "next-gen-gc")]
            hof_fusions,
            #[cfg(feature = "next-gen-gc")]
            prim_arrays,
        })
    }
}
