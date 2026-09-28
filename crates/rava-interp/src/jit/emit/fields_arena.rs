//! Dual-path field access: one shape that serves an arena object and a handle object.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::{ARRAY_TAG, REF_TAG};
use crate::jit::*;

/// May this site emit the inline arena slot probe at all?
///
/// Split from the emitter because the caller must decide BEFORE it creates the outer
/// arena/handle diamond — a refusal has to leave the emitted code exactly as it was.
///
/// Refuses under the pinned-mirror flags for the same reason every other arena emitter
/// does: there a raw pointer receiver can be a HANDLE object's mirror, whose
/// authoritative cells live in the FieldBlob, so `[obj + HDR + slot*8]` names the wrong
/// storage. Refuses under RAVA_SHADOW_GC because an inline store skips the card try_lock
/// that path expects.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_slot_probe_ok(fid: i64) -> bool {
    inline_fields_enabled()
        && !crate::nextgen::pinned_ptrs_enabled()
        && !crate::nextgen::pinned_read_enabled()
        && !crate::nextgen::shadow_enabled()
        && u32::try_from(fid).is_ok_and(|f| f < (1 << 24))
}

/// Emit the inline `(class id, field id) → slot` lookup that replaces the
/// `rava_getfield → arena_getfield → slot_of` cascade at a site whose slot the compiler
/// could not resolve. Returns `(ok, slot_plus_1)`; the caller branches on `ok` and must
/// treat the false arm as a plain callback.
///
/// Without `RAVA_FIELD_PROBE_CENSUS`, this is ~11 instructions, two loads, no call,
/// and no store. The receiver's class-id word is the guard — stricter than the
/// const-slot arena emitters (which rely only on an `is_arena` test), so admission
/// never weakens what the engine already trusts.
///
/// The entry load is unconditional and always safe: `ARENA_SLOT_CACHE` is a process
/// static. A normal miss reads a word and branches without mutation. The opt-in census
/// adds one atomic counter increment per probe and is diagnostic-only.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_arena_slot_probe(b: &mut FunctionBuilder, obj: Value, fid: i64) -> (Value, Value) {
    let fid_u = fid as u32;
    // class id = low half of the header word at [obj + 0].
    let cid32 = b.ins().load(I32, MemFlags::trusted(), obj, Offset32::new(0));
    let cid = b.ins().uextend(I64, cid32);
    // Bucket. `arena_slot_mix(fid)` is a compile-time constant, so this is xor + and.
    let mixed = b.ins().bxor_imm(cid, arena_slot_mix(fid_u) as i64);
    let idx = b.ins().band_imm(mixed, ARENA_SLOT_MASK as i64);
    let byte = b.ins().ishl_imm(idx, 3);
    let base = b.ins().iconst(I64, arena_slot_base());
    let addr = b.ins().iadd(base, byte);
    let e = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    // Key check on the SAME word the slot came out of — see the tearing note on
    // ARENA_SLOT_CACHE for why the pair may not be split across two loads.
    //
    // Compared as `e >> 16` against `(class_id << 24) | fid`, NOT as `class_id << 40`
    // against a masked entry: a class id at or above 2^24 would have its top bits shifted
    // clean out of a 64-bit word and could then alias a genuinely published entry for a
    // DIFFERENT class. Shifted by 24 instead, an oversized id lands above 2^48, which no
    // entry's `e >> 16` can ever reach — it simply never matches. (`fid < 2^24` is a
    // precondition of `arena_slot_probe_ok`.)
    let want_hi = b.ins().ishl_imm(cid, 24);
    let want = b.ins().bor_imm(want_hi, fid_u as i64);
    let ehi = b.ins().ushr_imm(e, 16);
    let key_ok = b.ins().icmp(IntCC::Equal, ehi, want);
    let slot_p1 = b.ins().band_imm(e, 0xFFFF);
    // Zero = never published. This is the bootstrap trap's whole answer: the address is
    // baked, the VALUE is read, and the arm refuses until the callbacks it serves have
    // filled it in.
    let ready = b.ins().icmp_imm(IntCC::NotEqual, slot_p1, 0);
    let ok = b.ins().band(key_ok, ready);
    if field_probe_census_enabled() {
        crate::jit::field_probe_note_site();
        let hits = b
            .ins()
            .iconst(I64, crate::jit::field_probe_counter_ptr(true));
        let misses = b
            .ins()
            .iconst(I64, crate::jit::field_probe_counter_ptr(false));
        let counter = b.ins().select(ok, hits, misses);
        let one = b.ins().iconst(I64, 1);
        b.ins().atomic_rmw(
            I64,
            MemFlags::trusted(),
            cranelift::codegen::ir::AtomicRmwOp::Add,
            counter,
            one,
        );
    }
    (ok, slot_p1)
}

/// `obj + HDR + (slot_plus_1 - 1) * 8` — emitted inside the guarded arm only, so the
/// miss path pays none of it.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_arena_cell_addr(b: &mut FunctionBuilder, obj: Value, slot_p1: Value) -> Value {
    let s = b.ins().iadd_imm(slot_p1, -1);
    let byte = b.ins().ishl_imm(s, 3);
    let off = b.ins().iadd_imm(byte, crate::nextgen::HDR as i64);
    b.ins().iadd(obj, off)
}

/// Arena reference CELL → JIT operand cell. The 2-bit tag scheme, identical to the arm
/// in `native_getfield_ref_const_arena_dual`: 0b00 raw pointer / null passes through,
/// 0b01 / 0b10 re-tag to REF / ARRAY, 0b11 (boxed Str or sentinel) is the one form that
/// needs the host — it has to land in the per-call box table, which no store-free
/// sequence can do. Jumps to `merge` (one I64 param) or `cb`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_arena_ref_classify(
    b: &mut FunctionBuilder,
    cell: Value,
    merge: cranelift::codegen::ir::Block,
    cb: cranelift::codegen::ir::Block,
) {
    let t = b.ins().band_imm(cell, 0b11);
    let h = b.ins().ushr_imm(cell, 2);
    let ref_form = b.ins().bor_imm(h, REF_TAG);
    let arr_form = b.ins().bor_imm(h, ARRAY_TAG);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, t, 0b10);
    let tagged = b.ins().select(is_arr, arr_form, ref_form);
    let is_raw = b.ins().icmp_imm(IntCC::Equal, t, 0b00);
    let non_box = b.ins().select(is_raw, cell, tagged);
    let is_box = b.ins().icmp_imm(IntCC::Equal, t, 0b11);
    let nb: [cranelift::codegen::ir::BlockArg; 1] = [non_box.into()];
    b.ins().brif(is_box, cb, &[], merge, &nb);
}

/// JIT operand cell → arena reference CELL, for the inline-encodable forms
/// (null / raw arena pointer / REF handle / ARRAY handle). Returns
/// `(inlineable, encoded)`; the caller must route `!inlineable` (a BOX_TAG value, which
/// needs the `arena_box` side table) to the callback. Mirror of the `enc` block in
/// `native_putfield_ref_arena_dual` — the two MUST agree or a cell is written one way
/// and read the other.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_arena_ref_encode(b: &mut FunctionBuilder, val_cell: Value) -> (Value, Value) {
    let vhi = b.ins().ushr_imm(val_cell, 32);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, vhi, REF_TAG_HI as i64);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, vhi, ARRAY_TAG_HI as i64);
    let is_null = b.ins().icmp_imm(IntCC::Equal, val_cell, 0);
    let vhi40 = b.ins().ushr_imm(val_cell, 40);
    let vhi40_z = b.ins().icmp_imm(IntCC::Equal, vhi40, 0);
    let vnz = b.ins().icmp_imm(IntCC::NotEqual, val_cell, 0);
    let is_objraw = b.ins().band(vnz, vhi40_z); // non-null, < 2^40 → raw ArenaPtr
    let ok0 = b.ins().bor(is_ref, is_arr);
    let ok1 = b.ins().bor(ok0, is_null);
    let inlineable = b.ins().bor(ok1, is_objraw);
    let h = b.ins().band_imm(val_cell, 0xFFFF_FFFF);
    let h2 = b.ins().ishl_imm(h, 2);
    let ref_enc = b.ins().bor_imm(h2, 0b01);
    let arr_enc = b.ins().bor_imm(h2, 0b10);
    let tagged = b.ins().select(is_arr, arr_enc, ref_enc);
    let is_handle = b.ins().bor(is_ref, is_arr);
    let encoded = b.ins().select(is_handle, tagged, val_cell); // raw Obj / null stay as-is
    (inlineable, encoded)
}

/// TRACER (next-gen-gc): dual-path constant-slot getfield for a PRIMITIVE field
/// (int/long/float/double — raw bits are identical in both cell schemes, so a
/// single inline load works for all). Branches on the receiver:
///   - ArenaPtr → INLINE native `load.i64` at `[obj + HDR + slot*8]`, ZERO callbacks.
///   - heap handle / null → the existing high-word handle path, unchanged.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_getfield_const_prim_arena_dual(
    b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64, slot: u32, jt: JT,
) -> Value {
    native_getfield_const_prim_arena_dual_proven(b, env, ctx_cell, obj, fid, slot, jt, false)
}

pub(crate) fn native_getfield_const_prim_arena_dual_proven(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    slot: u32,
    jt: JT,
    proven_nonnull: bool,
) -> Value {
    let arena_blk = b.create_block();
    let handle_blk = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, jt_ty(jt));

    let is_arena = emit_is_arena(b, obj);
    b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

    // ArenaPtr: `obj` IS the raw header pointer; read the inline cell directly.
    b.switch_to_block(arena_blk);
    b.seal_block(arena_blk);
    let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
    // FIELD-LICM (arena): mark the inline-cell load `readonly` when the method makes
    // no heap mutation / non-inlined call (invariant_fields) — same as the heap path
    // (native_getfield_const). Sound: invariant_fields ⇒ no putfield/alloc/callback ⇒
    // no GC mid-call, so the cell address + value are immutable for the whole call,
    // letting the egraph hoist loop-invariant arena field reads out (Fields/DispatchMono
    // were re-loading every iteration under next-gen-gc because this load wasn't readonly).
    let mf = if env.invariant_fields {
        MemFlags::trusted().with_readonly()
    } else {
        MemFlags::trusted()
    };
    let cell = b.ins().load(I64, mf, obj, Offset32::new(off));
    let sv = cell_to_sv(b, cell, jt);
    b.ins().jump(merge, &[sv.v.into()]);

    // Heap handle / null: the CHECKED descriptor-indirect path. (Was UNCHECKED — the exact
    // getfield twin of the f8c5aee putfield bug: a fresh object built in a <init> has only
    // the fields appended so far, so a const-slot read to a not-yet-present high slot returns
    // the WRONG field's bits — RAVA_JIT_VERIFY caught Block.hasDynamicShape()Z returning
    // jit=1065353216 (=0x3F800000, the float 1.0f of an adjacent slot) vs interp=0. The
    // CHECKED const read bounds-checks slot<count + verifies ids[slot]==fid, else routes to
    // the by-id callback — correct for under-populated objects.)
    b.switch_to_block(handle_blk);
    b.seal_block(handle_blk);
    let v = native_getfield_const_proven(b, env, ctx_cell, obj, fid, slot, jt, proven_nonnull);
    b.ins().jump(merge, &[v.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_getfield_ref_const_arena_dual(
    b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64, slot: u32, bare_ok: bool,
) -> Value {
    native_getfield_ref_const_arena_dual_proven(b, env, ctx_cell, obj, fid, slot, bare_ok, false)
}

pub(crate) fn native_getfield_ref_const_arena_dual_proven(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    slot: u32,
    bare_ok: bool,
    proven_nonnull: bool,
) -> Value {
    let arena_blk = b.create_block();
    let raw = b.create_block();
    let classify = b.create_block();
    let retag = b.create_block();
    let cb = b.create_block();
    let handle_blk = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);

    let is_arena = emit_is_arena(b, obj);
    b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

    // Arena: load the low-2-bit cell, split by tag.
    b.switch_to_block(arena_blk);
    b.seal_block(arena_blk);
    let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
    // FIELD-LICM (arena ref): readonly when invariant_fields, so the egraph hoists
    // loop-invariant arena reference reads (see native_getfield_const_prim_arena_dual).
    let mf = if env.invariant_fields {
        MemFlags::trusted().with_readonly()
    } else {
        MemFlags::trusted()
    };
    let cell = b.ins().load(I64, mf, obj, Offset32::new(off));
    // STEP 6 PILOT (RAVA_JIT_BARE_REF_LOAD): see `emit_arena_getfield_ref_flat` —
    // the arena arm's tag classify collapses to the load above.
    // SHADOW VERIFY (`RAVA_BARE_VERIFY`, flip OFF): ask what the bare load WOULD have returned,
    // while the tagged diamond below still produces the correct value. See `rava_bare_verify`.
    // SAMPLED 1-IN-32 BY FIELD ID, decided at EMIT time so unsampled sites stay clean.
    // A call in the middle of every arena ref-field read is a ~30x boot slowdown (a call
    // clobbers caller-saved registers, so every live value spills around it — ledger row 307's
    // shape). Sampling keeps the population representative and the workload runnable.
    if bare_ok && crate::nextgen::bare_verify_enabled() && (fid & 31) == 0 {
        let f = b.ins().iconst(I64, fid);
        let sl = b.ins().iconst(I64, slot as i64);
        b.ins().call(env.bare_verify, &[cell, f, sl]);
    }
    if bare_ok && crate::nextgen::bare_ref_load_enabled() {
        b.ins().jump(merge, &[cell.into()]);
        for blk in [raw, classify, retag, cb] {
            b.switch_to_block(blk);
            b.seal_block(blk);
            b.ins().jump(merge, &[cell.into()]);
        }
        b.switch_to_block(handle_blk);
        b.seal_block(handle_blk);
        let v = native_getfield_ref_const_proven(b, env, ctx_cell, obj, fid, slot, proven_nonnull);
        b.ins().jump(merge, &[v.into()]);
        b.switch_to_block(merge);
        b.seal_block(merge);
        return b.block_params(merge)[0];
    }
    let t = b.ins().band_imm(cell, 0b11);
    // BRANCHLESS TAG SELECT (`RAVA_JIT_FLAT_TAGSELECT`, default OFF).
    //
    // Three of this diamond's four arms are PURE: `raw` returns the cell unchanged and `retag`
    // derives the Ref/Array operand form with shifts and ors. Only the boxed arm needs a call.
    // Branching between pure arms costs more than computing both: the measured shape of this
    // engine's compiled code is 852 blocks for four lines of Java, of which 62.7% do no memory
    // work and no call, and 81.5% of THOSE are bare merge blocks carrying block params and dozens
    // of SSA aliases (bench/block_anatomy.ps1, bench/guard_census.ps1). The phi web those merges
    // create is the plausible source of the 2555 movq that dominate the instruction mix, and it is
    // why retiring the tag TEST alone measured only 5.6% - it deletes two compares and leaves the
    // diamond standing.
    //
    // So fold the pure arms into `select`s and keep exactly one branch, for the box case that
    // genuinely needs the callback. Same values on every path (select is total), three extra cheap
    // ALU ops on the common raw path, minus one branch, three blocks and their merge edges.
    if flat_tagselect_enabled() {
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
        // The three pure arms are now unreachable; fill them so the builder is well-formed and
        // Cranelift's DCE drops them (they must still be sealed and terminated).
        for blk in [raw, classify, retag] {
            b.switch_to_block(blk);
            b.seal_block(blk);
            b.ins().jump(merge, &[cell.into()]);
        }
    } else {
    let is_raw = b.ins().icmp_imm(IntCC::Equal, t, 0b00);
    b.ins().brif(is_raw, raw, &[], classify, &[]);

    // Raw ArenaPtr / null already use the JIT operand-cell representation. This
    // is the overwhelmingly common object-graph edge, so return it before
    // constructing the two tagged-handle forms.
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
    }

    // Boxed Str/sentinel: the generic callback boxes it into the jit_box_table.
    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc = b.ins().iconst(I64, fid);
    let is_ref = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.getfield, &[obj, fidc, is_ref]);
    let res = b.inst_results(call)[0];
    b.ins().jump(merge, &[res.into()]);

    // Heap handle / null: unchanged high-word path.
    b.switch_to_block(handle_blk);
    b.seal_block(handle_blk);
    let v = native_getfield_ref_const_proven(b, env, ctx_cell, obj, fid, slot, proven_nonnull);
    b.ins().jump(merge, &[v.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// TRACER (next-gen-gc): dual-path constant-slot putfield for a PRIMITIVE field.
/// Arena receiver → INLINE native `store` of the raw value bits at
/// `[obj + HDR + slot*8]` (no encode needed for primitives); heap handle / null →
/// the unchanged high-word path (which raises the NPE / repopulates).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn native_putfield_const_prim_arena_dual(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
    slot: u32,
    kind: i64,
) {
    // PINNED-MIRROR COHERENCE (RAVA_PINNED_READ, default OFF): under PINNED_PTRS the
    // receiver `obj` can be a raw pinned-mirror ADDRESS — which classifies as arena
    // (`emit_is_arena`), so the arena arm's inline `[obj + HDR + slot*8]` store would
    // update the mirror ONLY, leaving the authoritative FieldBlob stale. Route the
    // whole op through the callback (rava_putfield → put_field_hint, which recovers
    // the handle via pinned_handle_of and dual-stores blob + mirror; a genuine arena
    // receiver still lands on arena_putfield). Byte-identical when the flag is off.
    if crate::nextgen::pinned_read_enabled() {
        let fidc = b.ins().iconst(I64, fid);
        let kindc = b.ins().iconst(I64, kind);
        b.ins().call(env.putfield, &[obj, fidc, val_cell, kindc]);
        return;
    }
    let arena_blk = b.create_block();
    let handle_blk = b.create_block();
    let done = b.create_block();

    let is_arena = emit_is_arena(b, obj);
    b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

    b.switch_to_block(arena_blk);
    b.seal_block(arena_blk);
    let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
    b.ins().store(MemFlags::trusted(), val_cell, obj, Offset32::new(off));
    b.ins().jump(done, &[]);

    b.switch_to_block(handle_blk);
    b.seal_block(handle_blk);
    // BUG FIX (GC-cycle-1 SIGSEGV, RAVA_JIT_SENTINEL-confirmed): the heap-receiver fallback
    // used native_putfield_const_UNCHECKED, which stores at cells[slot] with NO `slot <
    // count` bound. That is sound ONLY for a fully pre-populated object; but a FRESH object
    // being built in a <init> (e.g. BlockBehaviour$BlockStateBase.<init>) has only the fields
    // appended SO FAR, so a const-slot store to a not-yet-present high slot (a float property
    // like friction=0.2f) OVERFLOWS field_cells and clobbers the ADJACENT heap Box's class
    // Arc → the deterministic class-len corruption. The CHECKED const store bounds-checks
    // `slot < count` + verifies ids[slot]==fid and otherwise routes to the append callback —
    // correct for under-populated objects. (Watchpoint proved: `movq val,0x60(%r8)` = slot 12
    // store with no cmp, hitting a lower handle's Box.)
    native_putfield_const(b, env, ctx_cell, obj, val_cell, fid, slot, kind);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// TRACER (next-gen-gc): dual-path putfield for a REFERENCE field. Arena receiver:
/// ENCODE the JIT high-word operand cell into the arena low-2-bit form, INLINE
/// `store` it at `[obj + HDR + slot*8]`, then call the thin `rava_card_mark`
/// barrier (old→young remembered set). Encode inline for the common cases:
///   null(0) → 0   raw ArenaPtr (`val>>40==0`, non-null) → as-is (already 0b00)
///   Ref `h|REF_TAG` → `(h<<2)|1`   Array `h|ARRAY_TAG` → `(h<<2)|2`
/// A boxed (BOX_TAG) value routes to the generic callback (it must hit the
/// arena_box side table). Heap handle / null receivers take the unchanged path.
#[cfg(feature = "next-gen-gc")]
// `bare_ok`: may the bare-deref flip store an untagged pointer here? False for a field
// DECLARED as an array, and false for a ctor-inline splice whose descriptor is not in this
// pool. Must agree with the LOAD side or the cell is written one way and read the other.
pub(crate) fn native_putfield_ref_arena_dual(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
    slot: u32,
    bare_ok: bool,
) {
    // PINNED/SHADOW COHERENCE: a constant-slot caller reaches this dual emitter without
    // the runtime slot-probe wrapper. Share that wrapper's complete emit-time gate here:
    // pinned mirrors make the low-address receiver classify as arena while FieldBlob remains
    // authoritative, and shadow mode requires the callback's barrier/diagnostic path. Route
    // every refused shape through the coherent callback (kind 4 = ref).
    if !arena_slot_probe_ok(fid) {
        let fidc = b.ins().iconst(I64, fid);
        let kindc = b.ins().iconst(I64, 4);
        b.ins().call(env.putfield, &[obj, fidc, val_cell, kindc]);
        return;
    }
    let arena_blk = b.create_block();
    let enc = b.create_block();
    let cb = b.create_block();
    let handle_blk = b.create_block();
    let done = b.create_block();

    let is_arena = emit_is_arena(b, obj);
    b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

    // Arena: classify the value cell. Inline-encodable = null | raw Obj | REF | ARRAY.
    b.switch_to_block(arena_blk);
    b.seal_block(arena_blk);
    // STEP 6 PILOT (RAVA_JIT_BARE_REF_LOAD): the store side of bare dereference —
    // a 0b00 operand cell IS the stored cell, so the classify+encode collapses to a
    // bare store. Measurement only (see the nextgen gate doc).
    if bare_ok && crate::nextgen::bare_ref_load_enabled() {
        let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
        b.ins().store(MemFlags::trusted(), val_cell, obj, Offset32::new(off));
        b.ins().jump(done, &[]);
        for blk in [enc, cb] {
            b.switch_to_block(blk);
            b.seal_block(blk);
            b.ins().jump(done, &[]);
        }
        b.switch_to_block(handle_blk);
        b.seal_block(handle_blk);
        // `_handle` directly, not the public wrapper: the is_arena test that guards this
        // arm ALREADY answered false, and the wrapper would re-emit the whole diamond.
        native_putfield_ref_handle(b, env, ctx_cell, obj, val_cell, fid);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        b.seal_block(done);
        return;
    }
    let vhi = b.ins().ushr_imm(val_cell, 32);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, vhi, REF_TAG_HI as i64);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, vhi, ARRAY_TAG_HI as i64);
    let is_null = b.ins().icmp_imm(IntCC::Equal, val_cell, 0);
    let vhi40 = b.ins().ushr_imm(val_cell, 40);
    let vhi40_z = b.ins().icmp_imm(IntCC::Equal, vhi40, 0);
    let vnz = b.ins().icmp_imm(IntCC::NotEqual, val_cell, 0);
    let is_objraw = b.ins().band(vnz, vhi40_z); // non-null, < 2^40 → raw ArenaPtr
    let ok0 = b.ins().bor(is_ref, is_arr);
    let ok1 = b.ins().bor(ok0, is_null);
    let inlineable = b.ins().bor(ok1, is_objraw);
    b.ins().brif(inlineable, enc, &[], cb, &[]);

    b.switch_to_block(enc);
    b.seal_block(enc);
    // Encode: handle → (h<<2)|tag; raw Obj / null → pass through unchanged.
    let h = b.ins().band_imm(val_cell, 0xFFFF_FFFF);
    let h2 = b.ins().ishl_imm(h, 2);
    let ref_enc = b.ins().bor_imm(h2, 0b01);
    let arr_enc = b.ins().bor_imm(h2, 0b10);
    let tagged = b.ins().select(is_arr, arr_enc, ref_enc);
    let is_handle = b.ins().bor(is_ref, is_arr);
    let encoded = b.ins().select(is_handle, tagged, val_cell); // raw Obj / null stay as-is
    let off = crate::nextgen::HDR as i32 + slot as i32 * 8;
    b.ins().store(MemFlags::trusted(), encoded, obj, Offset32::new(off));
    // Production GC scans tenured directly; only opt-in diagnostics consume this
    // callback. Avoid a JIT→Rust round-trip on every normal reference write.
    if card_mark_callback_enabled() {
        b.ins().call(env.card_mark, &[obj, encoded]);
    }
    b.ins().jump(done, &[]);

    // Boxed Str/sentinel: generic callback (kind 4 = ref) — encodes via arena_box.
    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc = b.ins().iconst(I64, fid);
    let kindc = b.ins().iconst(I64, 4);
    b.ins().call(env.putfield, &[obj, fidc, val_cell, kindc]);
    b.ins().jump(done, &[]);

    b.switch_to_block(handle_blk);
    b.seal_block(handle_blk);
    // See the bare-flip arm above: the receiver is proven non-arena here, so go straight
    // to the handle body instead of re-emitting the wrapper's is_arena diamond.
    native_putfield_ref_handle(b, env, ctx_cell, obj, val_cell, fid);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}
