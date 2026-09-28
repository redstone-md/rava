//! Default-off producer for method-confined reference-array sites.
//!
//! A raw arena reference array may cross only the local structural consumers
//! proved by the method-local analysis (`aaload`, `aastore`, and `arraylength`).
//! Calls, fields, callbacks, returns, and other escape or safepoint-capable
//! operations reject the site.  The plan is keyed by exact bytecode PC and
//! retains the CP descriptor used by the proof.  The gate is process-constant
//! and default off; no class, method, workload, history, or timing information
//! is consulted.

use crate::jit::*;
use rava_classfile::{Code, ConstantPool};
use std::collections::HashMap;

/// One accepted reference-array producer.  The exact CP index and descriptor are retained with
/// the site even though emission only needs the resolved CID; this keeps the proof tied to the
/// bytecode site and prevents a descriptor from being inferred from a name or another site.
#[derive(Clone, Debug)]
struct ArenaRefArraySite {
    cp: u16,
    component: Box<str>,
    class_id: u32,
}

/// Reference-array sites keyed by exact bytecode PC.  `get` preserves the existing emitter
/// contract (`Option<&u32>`) while the plan retains the CP descriptor used to admit the site.
#[derive(Clone, Debug, Default)]
pub(crate) struct ArenaRefArrayPlan {
    sites: HashMap<usize, ArenaRefArraySite>,
}

impl ArenaRefArrayPlan {
    fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    pub(crate) fn get(&self, pc: &usize) -> Option<&u32> {
        self.sites.get(pc).map(|site| &site.class_id)
    }

    #[allow(dead_code)]
    pub(crate) fn descriptor(&self, pc: usize) -> Option<(u16, &str)> {
        self.sites
            .get(&pc)
            .map(|site| (site.cp, site.component.as_ref()))
    }
}

/// The producer is an explicit opt-in tracer bullet.  It is deliberately
/// separate from the existing primitive-array gate because ref-array consumers
/// have a different representation contract.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_reference_arrays_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("RAVA_ARENA_REF_ARRAYS").ok().as_deref() == Some("1")
            && !arena_inline_disabled()
    })
}

#[cfg(feature = "next-gen-gc")]
fn reference_array_stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARENA_REF_ARRAY_STATS").is_some())
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn arena_reference_arrays_enabled() -> bool {
    false
}

/// Convert the `anewarray` CONSTANT_Class spelling into the exact component
/// descriptor retained by the arena class registry.  JVM array class names are
/// already descriptors (`[Ljava/lang/String;`, `[I`, ...); an ordinary class
/// name denotes one reference component and receives the `L...;` wrapper.
pub(crate) fn reference_array_component(pool: &ConstantPool, cp: u16) -> Option<String> {
    let element = pool.class_name(cp)?;
    if element.starts_with('[') {
        Some(element.to_owned())
    } else {
        Some(format!("L{element};"))
    }
}

/// Resolve exact component descriptors and CIDs during compilation.  The shared CFG proof admits
/// only a candidate whose live value is confined to locals, `dup`/`pop`, matching `aaload` /
/// `aastore`, and `arraylength`; calls, fields, returns of the array, exceptions,
/// checkcast/instanceof, and allocation/deopt-capable boundaries reject it.  A site with malformed
/// bytecode/CP data is simply left on the historical callback path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_arena_reference_arrays(
    code: &Code,
    pool: &ConstantPool,
) -> ArenaRefArrayPlan {
    let mut plan = ArenaRefArrayPlan::new();
    if !arena_reference_arrays_enabled() || !code.exceptions.is_empty() {
        return plan;
    }

    let bc = &code.bytecode;
    let Some(flow) = ConfinedFlow::decode(bc) else {
        return plan;
    };
    for &pc in flow.pcs() {
        let Some(len) = insn_len_at(bc, pc) else {
            return ArenaRefArrayPlan::new();
        };
        if bc[pc] == 0xbd && len == 3 {
            let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            if let Some(component) = reference_array_component(pool, cp) {
                if proves_confined(code, &flow, pc, ConfinedArrayKind::Reference) {
                    let cid = crate::nextgen::register_ref_array_class(&component);
                    plan.sites.insert(
                        pc,
                        ArenaRefArraySite {
                            cp,
                            component: component.into_boxed_str(),
                            class_id: cid,
                        },
                    );
                }
            }
        }
    }
    plan
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn analyze_arena_reference_arrays(
    _code: &Code,
    _pool: &ConstantPool,
) -> ArenaRefArrayPlan {
    ArenaRefArrayPlan::new()
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn empty_ref_arrays() -> &'static ArenaRefArrayPlan {
    static E: std::sync::OnceLock<ArenaRefArrayPlan> = std::sync::OnceLock::new();
    E.get_or_init(ArenaRefArrayPlan::new)
}

/// Emit one structurally confined `anewarray` as a raw arena object.
///
/// The successful path is a TLAB bump followed by exact CID/count header
/// publication and zeroed reference cells.  A full first attempt performs one
/// GC safepoint and retries once; a second full attempt and negative length use
/// the historical callback, preserving exception and tagged-array semantics.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_inline_reference_anewarray(
    b: &mut FunctionBuilder,
    env: &Env,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    cursor_var: Variable,
    locals_ptr: Value,
    cp: u16,
    class_id: u32,
    count: Value,
) -> Option<()> {
    let cell_bytes = crate::nextgen::PRIMITIVE_ARRAY_CELL_BYTES as i64;
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter_map(|(i, sv)| (sv.t == JT::Ref).then_some(i))
        .collect();
    let nref = ref_slots.len();

    let attempt = b.create_block();
    let overflow = b.create_block();
    let retry = b.create_block();
    let fallback = b.create_block();
    let initialized = b.create_block();
    let zero_loop = b.create_block();
    let cont = b.create_block();
    for block in [attempt, overflow, retry, fallback] {
        for _ in 0..nref {
            b.append_block_param(block, I64);
        }
    }
    b.append_block_param(initialized, I64);
    for _ in 0..nref {
        b.append_block_param(initialized, I64);
    }
    b.append_block_param(zero_loop, I64);
    b.append_block_param(cont, I64);
    for _ in 0..nref {
        b.append_block_param(cont, I64);
    }

    let initial_refs: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();
    let negative = b.ins().icmp_imm(IntCC::SignedLessThan, count, 0);
    b.ins()
        .brif(negative, fallback, &initial_refs, attempt, &initial_refs);

    b.switch_to_block(attempt);
    b.seal_block(attempt);
    let attempt_refs: Vec<Value> = b.block_params(attempt).to_vec();
    let cursor = b.use_var(cursor_var);
    let next = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(0));
    let end = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(8));
    let bytes = b.ins().imul_imm(count, cell_bytes);
    let size = b.ins().iadd_imm(bytes, crate::nextgen::HDR as i64);
    let new_top = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, new_top, end);
    let mut success_args: Vec<cranelift::codegen::ir::BlockArg> = vec![next.into()];
    success_args.extend(
        attempt_refs
            .iter()
            .copied()
            .map(|value| -> cranelift::codegen::ir::BlockArg { value.into() }),
    );
    let overflow_args: Vec<cranelift::codegen::ir::BlockArg> =
        attempt_refs.iter().copied().map(Into::into).collect();
    b.ins()
        .brif(fits, initialized, &success_args, overflow, &overflow_args);

    b.switch_to_block(overflow);
    b.seal_block(overflow);
    let overflow_refs: Vec<Value> = b.block_params(overflow).to_vec();
    adopt_ref_params(stack, &ref_slots, &overflow_refs);
    spill_for_gc(b, vars, stack, locals_ptr);
    let _ = b.ins().call(env.arena_safepoint_fn, &[]);
    reload_locals_after_gc(b, vars, locals_ptr);
    let retry_args = reloaded_ref_args(b, vars.len(), stack, &ref_slots, locals_ptr);
    b.ins().jump(retry, &retry_args);

    b.switch_to_block(retry);
    b.seal_block(retry);
    let retry_refs: Vec<Value> = b.block_params(retry).to_vec();
    let cursor = b.use_var(cursor_var);
    let next = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(0));
    let end = b
        .ins()
        .load(I64, MemFlags::trusted(), cursor, Offset32::new(8));
    let new_top = b.ins().iadd(next, size);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, new_top, end);
    let mut retry_success: Vec<cranelift::codegen::ir::BlockArg> = vec![next.into()];
    retry_success.extend(
        retry_refs
            .iter()
            .copied()
            .map(|value| -> cranelift::codegen::ir::BlockArg { value.into() }),
    );
    let retry_fallback: Vec<cranelift::codegen::ir::BlockArg> =
        retry_refs.iter().copied().map(Into::into).collect();
    b.ins()
        .brif(fits, initialized, &retry_success, fallback, &retry_fallback);
    b.seal_block(initialized);
    b.seal_block(fallback);

    b.switch_to_block(fallback);
    let fallback_refs: Vec<Value> = b.block_params(fallback).to_vec();
    adopt_ref_params(stack, &ref_slots, &fallback_refs);
    spill_for_gc(b, vars, stack, locals_ptr);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let call = b.ins().call(env.anewarray, &[cls, cpv, count]);
    let result = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut fallback_cont = vec![result.into()];
    fallback_cont.extend(reloaded_ref_args(
        b,
        vars.len(),
        stack,
        &ref_slots,
        locals_ptr,
    ));
    b.ins().jump(cont, &fallback_cont);

    b.switch_to_block(initialized);
    let initialized_params: Vec<Value> = b.block_params(initialized).to_vec();
    let object = initialized_params[0];
    let cursor = b.use_var(cursor_var);
    let new_top = b.ins().iadd(object, size);
    b.ins()
        .store(MemFlags::trusted(), new_top, cursor, Offset32::new(0));
    let count_word = b.ins().ishl_imm(count, 32);
    let cid = b.ins().iconst(I64, class_id as i64);
    let header = b.ins().bor(count_word, cid);
    b.ins()
        .store(MemFlags::trusted(), header, object, Offset32::new(0));
    let zero = b.ins().iconst(I64, 0);
    b.ins()
        .store(MemFlags::trusted(), zero, object, Offset32::new(8));
    if crate::nextgen::HDR > 16 {
        b.ins()
            .store(MemFlags::trusted(), zero, object, Offset32::new(16));
    }
    let cells = b.ins().iadd_imm(object, crate::nextgen::HDR as i64);
    let empty = b.ins().icmp_imm(IntCC::Equal, count, 0);
    let initialized_args: Vec<cranelift::codegen::ir::BlockArg> =
        initialized_params.iter().copied().map(Into::into).collect();
    b.ins()
        .brif(empty, cont, &initialized_args, zero_loop, &[cells.into()]);

    b.switch_to_block(zero_loop);
    let cell = b.block_params(zero_loop)[0];
    b.ins()
        .store(MemFlags::trusted(), zero, cell, Offset32::new(0));
    let next_cell = b.ins().iadd_imm(cell, cell_bytes);
    let object_end = b.ins().iadd(object, size);
    let more = b.ins().icmp(IntCC::UnsignedLessThan, next_cell, object_end);
    b.ins().brif(
        more,
        zero_loop,
        &[next_cell.into()],
        cont,
        &initialized_args,
    );
    b.seal_block(zero_loop);
    b.seal_block(cont);

    b.switch_to_block(cont);
    let cont_params: Vec<Value> = b.block_params(cont).to_vec();
    // Diagnostic-only census hook; the ordinary feature-on path is unchanged when unset.
    // `rava_arraylength` handles this exact CID-backed arena representation without collecting.
    if reference_array_stats_enabled() {
        let _ = b.ins().call(env.arrlen, &[cont_params[0]]);
    }
    for (k, &slot) in ref_slots.iter().enumerate() {
        stack[slot].v = cont_params[k + 1];
    }
    push(stack, cont_params[0], JT::Ref);
    Some(())
}

#[cfg(feature = "next-gen-gc")]
fn adopt_ref_params(stack: &mut [SV], slots: &[usize], params: &[Value]) {
    for (k, &slot) in slots.iter().enumerate() {
        stack[slot].v = params[k];
    }
}

#[cfg(feature = "next-gen-gc")]
fn reloaded_ref_args(
    b: &mut FunctionBuilder,
    local_count: usize,
    stack: &[SV],
    slots: &[usize],
    locals_ptr: Value,
) -> Vec<cranelift::codegen::ir::BlockArg> {
    slots
        .iter()
        .map(|&slot| reload_ref_slot(b, local_count, slot, stack[slot].v, locals_ptr).into())
        .collect()
}
