//! Hoisted field access: the base a loop computes once and the flat arena reads it feeds.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::{ARRAY_TAG, REF_TAG};
use crate::jit::*;

/// TRACER (next-gen-gc): dual-path constant-slot getfield for a PRIMITIVE int
/// The arena-receiver discriminator, shared by every inline arena field op:
/// `obj != 0 && (obj as u64) >> 40 == 0`. Sound because the arena is low-address
/// (every `ArenaPtr` < 2^40, see `nextgen::Region`) while a high-word handle
/// carries a tag in bits 40..43 (`hi >= 256`). NOT `obj & 3` — handles keep the
/// high-word ABI during coexistence, so their low 2 bits are arbitrary.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_is_arena(b: &mut FunctionBuilder, obj: Value) -> Value {
    let zero = b.ins().iconst(I64, 0);
    let obj_nz = b.ins().icmp(IntCC::NotEqual, obj, zero);
    let hi = b.ins().ushr_imm(obj, 40);
    let hi_z = b.ins().icmp_imm(IntCC::Equal, hi, 0);
    b.ins().band(obj_nz, hi_z)
}

/// FLAT-GETFIELD (next-gen-gc): the ARENA arm of
/// `native_getfield_const_prim_arena_dual` with the `is_arena` diamond REMOVED —
/// a bare straight-line load of the SAME cell `[obj + HDR + slot*8]` the diamond's
/// `arena_blk` reads. Emitted ONLY for a getfield whose receiver is the slot the
/// method-entry unswitch branched on, inside the arena-speculated body, where that one
/// guard already settled the receiver's kind (invariant across the object's lifetime,
/// so ONE entry check covers every site). GC-safe: the
/// receiver `obj` is still the CURRENT (reloaded/forwarded) receiver value at this
/// site — only the KIND test was hoisted, not the pointer. Straight-line so the
/// egraph can CSE/hoist the ~redundant field loads the per-site diamond used to wall.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_arena_getfield_prim_flat(
    b: &mut FunctionBuilder,
    env: &Env,
    obj: Value,
    slot: u32,
    jt: JT,
) -> Value {
    let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
    let mf = if env.invariant_fields {
        MemFlags::trusted().with_readonly()
    } else {
        MemFlags::trusted()
    };
    let cell = b.ins().load(I64, mf, obj, Offset32::new(off));
    cell_to_sv(b, cell, jt).v
}

/// FIELD-LICM prologue resolve: ONE cell-array BASE pointer per pinned receiver
/// local, such that every hoisted field of that receiver reads `[base + cslot*8]`
/// (a constant offset the load folds into its addressing mode — a single live
/// register per RECEIVER, not per field, which keeps the loop's register
/// pressure flat). Arena: `obj + HDR` (fixed layout, trusted like the site's
/// arena arm). Handle: the checked ObjDesc decode, verifying `cslot < count &&
/// ids[cslot]==fid` for EVERY pair up front — any miss → 0 → all sites take
/// their normal guarded path. Runs at METHOD ENTRY even when the guarded
/// getfield never would, so it must be total: the handle arm indexes the ObjDesc
/// table only behind an explicit REF_TAG check (a boxed/array cell in a
/// dead-path receiver local must not fault). Sound under `no_alloc` (no GC ⇒
/// nothing moves) + a pinned receiver (never stored); the handle arm
/// additionally requires `invariant_fields` (`hoist_handle`) — a putfield could
/// APPEND a field and realloc the cells Vec, dangling the base. Arena layouts
/// can't grow, so the arena arm is safe even with putfields.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_licm_base(
    b: &mut FunctionBuilder,
    ctx_cell: Value,
    obj: Value,
    pairs: &[(u32, u32)], // (fid, cslot) to verify on the handle arm
    hoist_handle: bool,
) -> Value {
    let arena_blk = b.create_block();
    let other_blk = b.create_block();
    let zero_blk = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);

    let is_arena = emit_is_arena(b, obj);
    b.ins().brif(is_arena, arena_blk, &[], other_blk, &[]);

    b.switch_to_block(arena_blk);
    b.seal_block(arena_blk);
    let addr = b.ins().iadd_imm(obj, crate::nextgen::HDR as i64);
    b.ins().jump(merge, &[addr.into()]);

    b.switch_to_block(other_blk);
    b.seal_block(other_blk);
    if hoist_handle {
        let scan = b.create_block();
        let hi = b.ins().ushr_imm(obj, 32);
        let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, REF_TAG_HI as i64);
        b.ins().brif(is_ref, scan, &[], zero_blk, &[]);

        // The checked ObjDesc decode of native_getfield_const, verifying every
        // hoisted (fid, cslot) once. readonly: gated on no_alloc via the caller.
        b.switch_to_block(scan);
        b.seal_block(scan);
        let mf = MemFlags::trusted().with_readonly();
        let base = b.ins().load(I64, mf, ctx_cell, Offset32::new(16));
        let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
        let h = b.ins().band(obj, mask);
        let stride = b.ins().iconst(I64, 16);
        let doff = b.ins().imul(h, stride);
        let desc = b.ins().iadd(base, doff);
        let ids = b.ins().load(I64, mf, desc, Offset32::new(0));
        let cells = b.ins().load(I64, mf, desc, Offset32::new(8));
        let count = b.ins().load(I32, mf, cells, Offset32::new(-8));
        // ⭐**ONE BOUNDS CHECK THAT DOMINATES EVERY ID LOAD** (`RAVA_NO_EMIT_MISC2=1`
        // restores the per-pair chain below, byte-identically).
        //
        // THE REFUSAL THAT WAS: the loop below range-checks EACH pair separately —
        // `cslot_i <u count` gets its own compare, its own `brif` and its own two
        // blocks — because a load of `ids[cslot]` past the object's populated prefix
        // reads out of bounds. That precondition is real. What was never true is that
        // it needs N checks: `count` is loaded ONCE and every `cslot` is an EMIT-TIME
        // CONSTANT, so `max(cslot) <u count` implies `cslot_i <u count` for all i
        // (unsigned, and `max` is one of the constants). One check therefore dominates
        // every load, and the accepted predicate is unchanged:
        //     was  ∀i. cslot_i <u count ∧ ids[cslot_i]==fid_i
        //     is   max <u count ∧ ∀i. ids[cslot_i]==fid_i
        // and `max <u count ⟺ ∀i. cslot_i <u count`. Same base, same zero-arm, same
        // fallbacks — only the control flow collapses.
        //
        // With the loads now all dominated by that one check they need no branch
        // between them either: the id compares fold into an AND chain and ONE `brif`
        // decides. So a receiver with N hoisted fields costs 2 blocks and 2 branches
        // instead of 2N and 2N, in the ENTRY block of exactly the no-alloc loop
        // methods FIELD-LICM exists for — and the entry block is also where the egraph
        // has to keep the base and the descriptor decode alive, so the removed merges
        // are removed optimization barriers as much as removed instructions.
        //
        // Speculating the loads past a FAILED id compare is sound and not a widening:
        // every one is in bounds by the dominating check, `mf` is readonly/trusted
        // (no trap, no side effect), and a mismatch still yields base 0 → every site
        // takes its normal guarded path.
        if emit_misc2_enabled() && !pairs.is_empty() {
            let verify = b.create_block();
            let maxslot = pairs.iter().map(|&(_, c)| c).max().unwrap_or(0);
            let maxc = b.ins().iconst(I32, maxslot as i64);
            let in_range = b.ins().icmp(IntCC::UnsignedLessThan, maxc, count);
            b.ins().brif(in_range, verify, &[], zero_blk, &[]);
            b.switch_to_block(verify);
            b.seal_block(verify);
            let mut all_ok: Option<Value> = None;
            for &(fid, cslot) in pairs {
                let id_i = b.ins().load(I32, mf, ids, Offset32::new(cslot as i32 * 4));
                let id_ok = b.ins().icmp_imm(IntCC::Equal, id_i, fid as i64);
                all_ok = Some(match all_ok {
                    Some(prev) => b.ins().band(prev, id_ok),
                    None => id_ok,
                });
            }
            // `pairs` is non-empty (tested above), so the chain has a value.
            let ok = all_ok.expect("non-empty pairs always produce an id-compare chain");
            let cells_args: [cranelift::codegen::ir::BlockArg; 1] = [cells.into()];
            b.ins().brif(ok, merge, &cells_args, zero_blk, &[]);
        } else {
            // Per pair: range-check BEFORE the ids[cslot] load (the ids array is only
            // `count` long — the object's populated prefix — so an unguarded load for
            // a high slot on an under-populated object would read out of bounds).
            for &(fid, cslot) in pairs {
                let verify = b.create_block();
                let next = b.create_block();
                let slotc = b.ins().iconst(I32, cslot as i64);
                let in_range = b.ins().icmp(IntCC::UnsignedLessThan, slotc, count);
                b.ins().brif(in_range, verify, &[], zero_blk, &[]);
                b.switch_to_block(verify);
                b.seal_block(verify);
                let id_i = b.ins().load(I32, mf, ids, Offset32::new(cslot as i32 * 4));
                let id_ok = b.ins().icmp_imm(IntCC::Equal, id_i, fid as i64);
                b.ins().brif(id_ok, next, &[], zero_blk, &[]);
                b.switch_to_block(next);
                b.seal_block(next);
            }
            b.ins().jump(merge, &[cells.into()]);
        }
    } else {
        b.ins().jump(zero_blk, &[]);
    }

    b.switch_to_block(zero_blk);
    b.seal_block(zero_blk);
    let z = b.ins().iconst(I64, 0);
    b.ins().jump(merge, &[z.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// FIELD-LICM site: `base != 0 ? load [base + cslot*8] : <normal guarded
/// getfield>`. The load is the value read (mutable unless `invariant_fields` —
/// a same-method putfield through the SAME address must be observed; the
/// ADDRESS is what the prologue pinned). The zero arm is the untouched
/// dual-path chain, so a miss (null receiver → NPE callback, un-populated
/// handle, …) behaves exactly as before.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_licm_getfield(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    slot: u32,
    jt: JT,
    base_var: Variable,
) -> Value {
    let fast = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, jt_ty(jt));

    let p = b.use_var(base_var);
    let z = b.ins().iconst(I64, 0);
    let nz = b.ins().icmp(IntCC::NotEqual, p, z);
    b.ins().brif(nz, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    b.seal_block(fast);
    let mf = if env.invariant_fields {
        MemFlags::trusted().with_readonly()
    } else {
        MemFlags::trusted()
    };
    let cell = b.ins().load(I64, mf, p, Offset32::new(slot as i32 * 8));
    let sv = cell_to_sv(b, cell, jt);
    b.ins().jump(merge, &[sv.v.into()]);

    b.switch_to_block(slow);
    b.seal_block(slow);
    let v = native_getfield_const_prim_arena_dual(b, env, ctx_cell, obj, fid, slot, jt);
    b.ins().jump(merge, &[v.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// FLAT-GETFIELD (next-gen-gc): the ARENA arm of
/// `native_getfield_ref_const_arena_dual` with the outer `is_arena` diamond REMOVED
/// (the `this`-is-arena entry guard replaces it). The cell LOAD is now straight-line
/// so the egraph can CSE it; the remaining raw/ref/array/box classify is a pure tag
/// dispatch on the loaded value (identical to the dual's arena arm), reading the SAME
/// cell `[obj + HDR + slot*8]`. Emitted only for a receiver the method-entry
/// unswitch proved arena (`SV::arena`).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_arena_getfield_ref_flat(
    b: &mut FunctionBuilder,
    env: &Env,
    obj: Value,
    fid: i64,
    slot: u32,
    // May the bare-deref flip collapse the tag classify here? False for a field DECLARED as an array (descriptor split at the call site).
    bare_ok: bool,
) -> Value {
    block_census::note(&block_census::REF_FIELD);
    let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
    let mf = if env.invariant_fields {
        MemFlags::trusted().with_readonly()
    } else {
        MemFlags::trusted()
    };
    // STEP 6 PILOT (RAVA_JIT_BARE_REF_LOAD): bare dereference — the whole
    // raw/ref/array/box classify below collapses to this ONE load, which is what
    // the value-model rewrite's final flip emits. Measurement only; unsound unless
    // every ref cell in the running program is 0b00 (see nextgen gate doc).
    // SHADOW VERIFY (`RAVA_BARE_VERIFY`, flip OFF) — the LICM-hoisted twin of the arm in
    // `fields_arena`. Instrumenting only one of the two load emitters is how the first run of this
    // probe reported nothing at all.
    // SAMPLED 1-IN-32 BY FIELD ID, decided at EMIT time so unsampled sites stay clean.
    // A call in the middle of every arena ref-field read is a ~30x boot slowdown (a call
    // clobbers caller-saved registers, so every live value spills around it — ledger row 307's
    // shape). Sampling keeps the population representative and the workload runnable.
    if bare_ok && crate::nextgen::bare_verify_enabled() && (fid & 31) == 0 {
        let c = b.ins().load(I64, mf, obj, Offset32::new(off));
        let f = b.ins().iconst(I64, fid);
        let sl = b.ins().iconst(I64, slot as i64);
        b.ins().call(env.bare_verify, &[c, f, sl]);
    }
    if bare_ok && crate::nextgen::bare_ref_load_enabled() {
        return b.ins().load(I64, mf, obj, Offset32::new(off));
    }

    // BRANCHLESS TAG SELECT (`RAVA_JIT_FLAT_TAGSELECT`) - see the same fold in
    // `native_getfield_ref_const_arena_dual`. THIS is the emitter the ring's hot method actually
    // uses: folding only the dual moved DensityFunctions$ShiftedNoise.compute from 852 blocks to
    // 836, i.e. 16 of its 435 merge blocks, because the flat variant was left untouched. Unlike
    // the dual, here the blocks are created AFTER the early return, so the unused ones are simply
    // never created rather than emitted dead.
    if flat_tagselect_enabled() {
        let cb = b.create_block();
        let merge = b.create_block();
        b.append_block_param(merge, I64);
        let cell = b.ins().load(I64, mf, obj, Offset32::new(off));
        let t = b.ins().band_imm(cell, 0b11);
        let h = b.ins().ushr_imm(cell, 2);
        let ref_form = b.ins().bor_imm(h, REF_TAG);
        let arr_form = b.ins().bor_imm(h, ARRAY_TAG);
        let is_arr = b.ins().icmp_imm(IntCC::Equal, t, 0b10);
        let tagged = b.ins().select(is_arr, arr_form, ref_form);
        let is_raw0 = b.ins().icmp_imm(IntCC::Equal, t, 0b00);
        let non_box = b.ins().select(is_raw0, cell, tagged);
        let is_box0 = b.ins().icmp_imm(IntCC::Equal, t, 0b11);
        let nb: [cranelift::codegen::ir::BlockArg; 1] = [non_box.into()];
        b.ins().brif(is_box0, cb, &[], merge, &nb);
        b.switch_to_block(cb);
        b.seal_block(cb);
        let fidc = b.ins().iconst(I64, fid);
        let is_ref = b.ins().iconst(I64, 1);
        let call = b.ins().call(env.getfield, &[obj, fidc, is_ref]);
        let res = b.inst_results(call)[0];
        b.ins().jump(merge, &[res.into()]);
        b.switch_to_block(merge);
        b.seal_block(merge);
        return b.block_params(merge)[0];
    }

    let raw = b.create_block();
    let classify = b.create_block();
    let retag = b.create_block();
    let cb = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);

    let cell = b.ins().load(I64, mf, obj, Offset32::new(off));
    let t = b.ins().band_imm(cell, 0b11);
    let is_raw = b.ins().icmp_imm(IntCC::Equal, t, 0b00);
    b.ins().brif(is_raw, raw, &[], classify, &[]);

    // Raw ArenaPtr / null already use the JIT operand-cell representation.
    b.switch_to_block(raw);
    b.seal_block(raw);
    b.ins().jump(merge, &[cell.into()]);

    b.switch_to_block(classify);
    b.seal_block(classify);
    let is_box = b.ins().icmp_imm(IntCC::Equal, t, 0b11);
    b.ins().brif(is_box, cb, &[], retag, &[]);

    // Re-tag 0b01 (Ref) / 0b10 (Array) into a JIT operand cell.
    b.switch_to_block(retag);
    b.seal_block(retag);
    let h = b.ins().ushr_imm(cell, 2);
    let ref_form = b.ins().bor_imm(h, REF_TAG);
    let arr_form = b.ins().bor_imm(h, ARRAY_TAG);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, t, 0b10);
    let result = b.ins().select(is_arr, arr_form, ref_form);
    b.ins().jump(merge, &[result.into()]);

    // Boxed Str/sentinel: the generic callback boxes it into the jit_box_table.
    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc = b.ins().iconst(I64, fid);
    let is_ref = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.getfield, &[obj, fidc, is_ref]);
    let res = b.inst_results(call)[0];
    b.ins().jump(merge, &[res.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// FIELD-LICM putfield site: `base != 0 ? store [base + cslot*8] : <normal dual
/// chain>`. Only reachable when the method HAS putfields ⇒ invariant_fields is
/// false ⇒ the prologue resolved base on the ARENA arm only, whose fixed layout
/// makes the trusted store exactly `native_putfield_const_prim_arena_dual`'s
/// arena arm. The zero arm (handle / null receiver) is the untouched chain.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_licm_putfield(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
    slot: u32,
    kind: i64,
    base_var: Variable,
) {
    // PINNED-MIRROR COHERENCE (RAVA_PINNED_READ, default OFF): the fast arm's raw
    // `[base + cslot*8]` store would strand the FieldBlob when `base` is a pinned
    // mirror. The read-flip flag already disables FIELD-LICM base hoisting (licm_pairs
    // gate) so this emitter is unreachable then, but keep the arm coherent regardless
    // by routing through the callback. Byte-identical when the flag is off.
    if crate::nextgen::pinned_read_enabled() {
        let fidc = b.ins().iconst(I64, fid);
        let kindc = b.ins().iconst(I64, kind);
        b.ins().call(env.putfield, &[obj, fidc, val_cell, kindc]);
        return;
    }
    let fast = b.create_block();
    let slow = b.create_block();
    let done = b.create_block();

    let p = b.use_var(base_var);
    let z = b.ins().iconst(I64, 0);
    let nz = b.ins().icmp(IntCC::NotEqual, p, z);
    b.ins().brif(nz, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    b.seal_block(fast);
    b.ins().store(MemFlags::trusted(), val_cell, p, Offset32::new(slot as i32 * 8));
    b.ins().jump(done, &[]);

    b.switch_to_block(slow);
    b.seal_block(slow);
    native_putfield_const_prim_arena_dual(b, env, ctx_cell, obj, val_cell, fid, slot, kind);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// PROMO write-back: store every WRITTEN promoted register to its cell.
/// Emitted before every outer-body return; only ever reached inside the
/// promoted body, whose bases the unswitch proved non-zero — the stores are
/// unconditional. Deferring the stores to here is C2's scalar promotion under
/// the single-thread JMM (racy readers may see stale — allowed).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_promo_flush(b: &mut FunctionBuilder, licm: &Licm) {
    for &(base, val, cslot) in licm.flush {
        let p = b.use_var(base);
        let v = b.use_var(val);
        b.ins().store(MemFlags::trusted(), v, p, Offset32::new(cslot as i32 * 8));
    }
}
