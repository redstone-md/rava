//! Primitive field access emitted inline: the checked variants, constant offset and not.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Master switch for everything this family added on top of the const-slot emitters:
/// the inline arena slot probe at slot-unresolved sites, and the FieldBlob KIND-byte
/// service that keeps null / raw-arena reference cells inside machine code.
/// DEFAULT ON; `RAVA_NO_INLINE_FIELDS=1` restores the callback-only routing so a broken
/// boot can be bisected in one run.
pub(crate) fn inline_fields_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_INLINE_FIELDS").is_none())
}

/// Native getfield for a PRIMITIVE field at a site whose slot the compiler could not
/// resolve.
///
/// THE HANDLE DECODE BELOW IS NOT VALID FOR AN ARENA RECEIVER — it masks the low 32
/// bits of the operand cell and indexes `obj_desc_base` with them, which for a raw
/// arena pointer is an arbitrary index into the descriptor table. So split first: an
/// arena receiver takes the inline slot probe (and its callback on a miss), a handle
/// takes the unchanged scan. Before this split the only thing keeping that read in
/// bounds was that such sites were assumed never to see an arena object.
pub(crate) fn native_getfield(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    jt: JT,
) -> Value {
    #[cfg(feature = "next-gen-gc")]
    if arena_slot_probe_ok(fid) {
        let res_ty = jt_ty(jt);
        let arena_blk = b.create_block();
        let fast = b.create_block();
        let cold = b.create_block();
        let handle_blk = b.create_block();
        let out = b.create_block();
        b.append_block_param(out, res_ty);

        let is_arena = emit_is_arena(b, obj);
        b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

        b.switch_to_block(arena_blk);
        b.seal_block(arena_blk);
        let (ok, slot_p1) = emit_arena_slot_probe(b, obj, fid);
        b.ins().brif(ok, fast, &[], cold, &[]);

        b.switch_to_block(fast);
        b.seal_block(fast);
        let addr = emit_arena_cell_addr(b, obj, slot_p1);
        // Raw bits are identical in both cell schemes for a primitive, so one load
        // serves int/long/float/double. Readonly under invariant_fields, matching the
        // const-slot arena arm, so the egraph can still hoist it out of a loop.
        let mf = if env.invariant_fields {
            MemFlags::trusted().with_readonly()
        } else {
            MemFlags::trusted()
        };
        let cell = b.ins().load(I64, mf, addr, Offset32::new(0));
        let sv = cell_to_sv(b, cell, jt);
        b.ins().jump(out, &[sv.v.into()]);

        b.switch_to_block(cold);
        b.seal_block(cold);
        let fidc = b.ins().iconst(I64, fid);
        let isr = b.ins().iconst(I64, 0);
        let call = b.ins().call(env.getfield, &[obj, fidc, isr]);
        let res = b.inst_results(call)[0];
        let svc = cell_to_sv(b, res, jt);
        b.ins().jump(out, &[svc.v.into()]);

        b.switch_to_block(handle_blk);
        b.seal_block(handle_blk);
        let v = native_getfield_handle(b, env, ctx_cell, obj, fid, jt);
        b.ins().jump(out, &[v.into()]);

        b.switch_to_block(out);
        b.seal_block(out);
        return b.block_params(out)[0];
    }
    native_getfield_handle(b, env, ctx_cell, obj, fid, jt)
}

/// Native getfield for a PRIMITIVE field. Loads the object's field descriptor
/// (ids/cells/count at obj_desc_base, read from ctx_cell+16), scans for `fid`,
/// and reads the matching cell as a native value. Null receiver OR field-not-found
/// falls back to the read callback (which handles NPE / inherited-field / default
/// exactly like the interpreter). Returns the typed value (jt_ty).
fn native_getfield_handle(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    jt: JT,
) -> Value {
    let res_ty = jt_ty(jt);
    let scan = b.create_block();
    let hint_try = b.create_block();
    let hint_hit = b.create_block();
    let loop_h = b.create_block();
    b.append_block_param(loop_h, I64); // induction i
    let body = b.create_block();
    let nxt = b.create_block();
    let found = b.create_block();
    b.append_block_param(found, I64); // matched slot
    let cb = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, res_ty);

    let zero = b.ins().iconst(I64, 0);
    let isnull = b.ins().icmp(IntCC::Equal, obj, zero);
    b.ins().brif(isnull, cb, &[], scan, &[]);

    b.switch_to_block(scan);
    b.seal_block(scan);
    let base = b.ins().load(I64, MemFlags::trusted(), ctx_cell, Offset32::new(16));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(obj, mask);
    let stride = b.ins().iconst(I64, 16); // size_of ObjDesc
    let off = b.ins().imul(h, stride);
    let desc = b.ins().iadd(base, off);
    let ids = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
    let cells = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(8));
    let count = b.ins().load(I32, MemFlags::trusted(), cells, Offset32::new(-8));
    let count64 = b.ins().uextend(I64, count);
    // Field-hint fast path: hint_base (ctx+24)[fid & (N-1)] is the slot this field
    // was last found at. `bucket` is a compile-time constant (fid is constant).
    let hint_base = b.ins().load(I64, MemFlags::trusted(), ctx_cell, Offset32::new(24));
    let bucket_off = ((fid as usize & (crate::heap::JIT_FIELD_HINT_N - 1)) * 4) as i32;
    let hint = b.ins().load(I32, MemFlags::trusted(), hint_base, Offset32::new(bucket_off));
    let hint64 = b.ins().uextend(I64, hint);
    let hint_ok = b.ins().icmp(IntCC::UnsignedLessThan, hint64, count64);
    b.ins().brif(hint_ok, hint_try, &[], loop_h, &[zero.into()]);

    b.switch_to_block(hint_try);
    b.seal_block(hint_try);
    let hid_off = b.ins().ishl_imm(hint64, 2);
    let hid_addr = b.ins().iadd(ids, hid_off);
    let hid = b.ins().load(I32, MemFlags::trusted(), hid_addr, Offset32::new(0));
    let fidc0 = b.ins().iconst(I32, fid);
    let hmatch = b.ins().icmp(IntCC::Equal, hid, fidc0);
    b.ins().brif(hmatch, hint_hit, &[], loop_h, &[zero.into()]);

    b.switch_to_block(hint_hit);
    b.seal_block(hint_hit);
    let hcell_off = b.ins().ishl_imm(hint64, 3);
    let hcell_addr = b.ins().iadd(cells, hcell_off);
    let hcell = b.ins().load(I64, MemFlags::trusted(), hcell_addr, Offset32::new(0));
    let hsv = cell_to_sv(b, hcell, jt);
    b.ins().jump(merge, &[hsv.v.into()]);

    b.switch_to_block(loop_h);
    let i = b.block_params(loop_h)[0];
    let done = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, count64);
    b.ins().brif(done, cb, &[], body, &[]);

    b.switch_to_block(body);
    b.seal_block(body);
    let id_off = b.ins().ishl_imm(i, 2);
    let id_addr = b.ins().iadd(ids, id_off);
    let id_i = b.ins().load(I32, MemFlags::trusted(), id_addr, Offset32::new(0));
    let fidc = b.ins().iconst(I32, fid);
    let matched = b.ins().icmp(IntCC::Equal, id_i, fidc);
    b.ins().brif(matched, found, &[i.into()], nxt, &[]);

    b.switch_to_block(nxt);
    b.seal_block(nxt);
    let i1 = b.ins().iadd_imm(i, 1);
    b.ins().jump(loop_h, &[i1.into()]);
    b.seal_block(loop_h); // preds (scan, hint_try, nxt) now emitted

    b.switch_to_block(found);
    b.seal_block(found);
    let fi = b.block_params(found)[0];
    // Update the hint for next time (scan path only — the hit path skips it).
    let fi32 = b.ins().ireduce(I32, fi);
    b.ins().store(MemFlags::trusted(), fi32, hint_base, Offset32::new(bucket_off));
    let cell_off = b.ins().ishl_imm(fi, 3);
    let cell_addr = b.ins().iadd(cells, cell_off);
    let cell = b.ins().load(I64, MemFlags::trusted(), cell_addr, Offset32::new(0));
    let sv = cell_to_sv(b, cell, jt);
    b.ins().jump(merge, &[sv.v.into()]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let is_ref = b.ins().iconst(I64, 0);
    let call = b.ins().call(env.getfield, &[obj, fidc2, is_ref]);
    let res = b.inst_results(call)[0];
    let svc = cell_to_sv(b, res, jt);
    b.ins().jump(merge, &[svc.v.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Native putfield for a PRIMITIVE field at a site whose slot the compiler could not
/// resolve. Same split, and the same reason, as `native_getfield`: the handle decode
/// would index `obj_desc_base` with the low half of a raw arena pointer — and here it
/// would STORE through the result.
pub(crate) fn native_putfield(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
    kind: i64,
) {
    #[cfg(feature = "next-gen-gc")]
    if arena_slot_probe_ok(fid) {
        let arena_blk = b.create_block();
        let probe = b.create_block();
        let fast = b.create_block();
        let cold = b.create_block();
        let handle_blk = b.create_block();
        let out = b.create_block();

        let is_arena = emit_is_arena(b, obj);
        b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

        // A store must not run while this frame is already unwinding — the same
        // short-circuit the handle body opens with, kept on the arena arm only so the
        // handle path stays byte-identical.
        b.switch_to_block(arena_blk);
        b.seal_block(arena_blk);
        let ff = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
        let faulted = b.ins().icmp_imm(IntCC::NotEqual, ff, 0);
        b.ins().brif(faulted, out, &[], probe, &[]);

        b.switch_to_block(probe);
        b.seal_block(probe);
        let (ok, slot_p1) = emit_arena_slot_probe(b, obj, fid);
        b.ins().brif(ok, fast, &[], cold, &[]);

        b.switch_to_block(fast);
        b.seal_block(fast);
        let addr = emit_arena_cell_addr(b, obj, slot_p1);
        // Primitives need no encode: the raw bits ARE the arena cell.
        b.ins().store(MemFlags::trusted(), val_cell, addr, Offset32::new(0));
        b.ins().jump(out, &[]);

        b.switch_to_block(cold);
        b.seal_block(cold);
        let fidc = b.ins().iconst(I64, fid);
        let kindc = b.ins().iconst(I64, kind);
        b.ins().call(env.putfield, &[obj, fidc, val_cell, kindc]);
        b.ins().jump(out, &[]);

        b.switch_to_block(handle_blk);
        b.seal_block(handle_blk);
        native_putfield_handle(b, env, ctx_cell, obj, val_cell, fid, kind);
        b.ins().jump(out, &[]);

        b.switch_to_block(out);
        b.seal_block(out);
        return;
    }
    native_putfield_handle(b, env, ctx_cell, obj, val_cell, fid, kind);
}

/// Native putfield for a PRIMITIVE field whose slot already exists (the common
/// post-<init> case). Scans the object descriptor for `fid`; on a hit writes the
/// already-encoded `val_cell` to the cell slot natively (the kind is unchanged —
/// a primitive field keeps its kind). Null receiver, a not-yet-present field
/// (needs an append + descriptor resync), or a pending fault fall to the
/// rava_putfield callback, which matches the interpreter exactly. `kind` is the
/// FK_* tag passed to the callback for the fallback path.
fn native_putfield_handle(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
    kind: i64,
) {
    let check_null = b.create_block();
    let scan = b.create_block();
    let hint_try = b.create_block();
    let hint_hit = b.create_block();
    let loop_h = b.create_block();
    b.append_block_param(loop_h, I64);
    let body = b.create_block();
    let nxt = b.create_block();
    let found = b.create_block();
    b.append_block_param(found, I64);
    let cb = b.create_block();
    let done = b.create_block();

    // Short-circuit if a fault is already pending in this frame.
    let ff = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
    let zero32 = b.ins().iconst(I32, 0);
    let faulted = b.ins().icmp(IntCC::NotEqual, ff, zero32);
    b.ins().brif(faulted, done, &[], check_null, &[]);

    b.switch_to_block(check_null);
    b.seal_block(check_null);
    let zero = b.ins().iconst(I64, 0);
    let isnull = b.ins().icmp(IntCC::Equal, obj, zero);
    // B4/B6a: putfield WRITES must keep the pinned mirror in sync (dual-store via
    // rava_putfield → put_field_hint) — force the callback under the flag. Reads
    // no longer force (pointer receivers take the inline arena arm; tagged reads
    // hit the authoritative FieldBlob directly).
    if crate::nextgen::pinned_read_enabled() {
        b.ins().jump(cb, &[]);
    } else {
        b.ins().brif(isnull, cb, &[], scan, &[]);
    }

    b.switch_to_block(scan);
    b.seal_block(scan);
    let base = b.ins().load(I64, MemFlags::trusted(), ctx_cell, Offset32::new(16));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(obj, mask);
    let stride = b.ins().iconst(I64, 16); // size_of ObjDesc
    let off = b.ins().imul(h, stride);
    let desc = b.ins().iadd(base, off);
    let ids = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
    let cells = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(8));
    let count = b.ins().load(I32, MemFlags::trusted(), cells, Offset32::new(-8));
    let count64 = b.ins().uextend(I64, count);
    // Field-hint fast path (see native_getfield): try the cached slot first.
    let hint_base = b.ins().load(I64, MemFlags::trusted(), ctx_cell, Offset32::new(24));
    let bucket_off = ((fid as usize & (crate::heap::JIT_FIELD_HINT_N - 1)) * 4) as i32;
    let hint = b.ins().load(I32, MemFlags::trusted(), hint_base, Offset32::new(bucket_off));
    let hint64 = b.ins().uextend(I64, hint);
    let hint_ok = b.ins().icmp(IntCC::UnsignedLessThan, hint64, count64);
    b.ins().brif(hint_ok, hint_try, &[], loop_h, &[zero.into()]);

    b.switch_to_block(hint_try);
    b.seal_block(hint_try);
    let hid_off = b.ins().ishl_imm(hint64, 2);
    let hid_addr = b.ins().iadd(ids, hid_off);
    let hid = b.ins().load(I32, MemFlags::trusted(), hid_addr, Offset32::new(0));
    let fidc0 = b.ins().iconst(I32, fid);
    let hmatch = b.ins().icmp(IntCC::Equal, hid, fidc0);
    b.ins().brif(hmatch, hint_hit, &[], loop_h, &[zero.into()]);

    b.switch_to_block(hint_hit);
    b.seal_block(hint_hit);
    let hcell_off = b.ins().ishl_imm(hint64, 3);
    let hcell_addr = b.ins().iadd(cells, hcell_off);
    b.ins().store(MemFlags::trusted(), val_cell, hcell_addr, Offset32::new(0));
    b.ins().jump(done, &[]);

    b.switch_to_block(loop_h);
    let i = b.block_params(loop_h)[0];
    let done_scan = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, count64);
    b.ins().brif(done_scan, cb, &[], body, &[]);

    b.switch_to_block(body);
    b.seal_block(body);
    let id_off = b.ins().ishl_imm(i, 2);
    let id_addr = b.ins().iadd(ids, id_off);
    let id_i = b.ins().load(I32, MemFlags::trusted(), id_addr, Offset32::new(0));
    let fidc = b.ins().iconst(I32, fid);
    let matched = b.ins().icmp(IntCC::Equal, id_i, fidc);
    b.ins().brif(matched, found, &[i.into()], nxt, &[]);

    b.switch_to_block(nxt);
    b.seal_block(nxt);
    let i1 = b.ins().iadd_imm(i, 1);
    b.ins().jump(loop_h, &[i1.into()]);
    b.seal_block(loop_h);

    b.switch_to_block(found);
    b.seal_block(found);
    let fi = b.block_params(found)[0];
    let fi32 = b.ins().ireduce(I32, fi);
    b.ins().store(MemFlags::trusted(), fi32, hint_base, Offset32::new(bucket_off));
    let cell_off = b.ins().ishl_imm(fi, 3);
    let cell_addr = b.ins().iadd(cells, cell_off);
    b.ins().store(MemFlags::trusted(), val_cell, cell_addr, Offset32::new(0));
    b.ins().jump(done, &[]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let kindc = b.ins().iconst(I64, kind);
    b.ins().call(env.putfield, &[obj, fidc2, val_cell, kindc]);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// Constant-slot native putfield (per-class layout active): write a PRIMITIVE
/// field at its class-constant `slot` with one verified store instead of the
/// id-scan/hint. Verify `slot < count && ids[slot] == fid` (guards a non-pre-
/// populated object / wrong slot → callback, exactly the interpreter's effect).
/// Pending-fault and null receiver short-circuit to the callback like
/// `native_putfield`. The kind is unchanged (a primitive field keeps its kind).
pub(crate) fn native_putfield_const(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
    slot: u32,
    kind: i64,
) {
    let check_null = b.create_block();
    let scan = b.create_block();
    let verify = b.create_block();
    let hit = b.create_block();
    let cb = b.create_block();
    let done = b.create_block();

    let ff = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
    let zero32 = b.ins().iconst(I32, 0);
    let faulted = b.ins().icmp(IntCC::NotEqual, ff, zero32);
    b.ins().brif(faulted, done, &[], check_null, &[]);

    b.switch_to_block(check_null);
    b.seal_block(check_null);
    let zero = b.ins().iconst(I64, 0);
    let isnull = b.ins().icmp(IntCC::Equal, obj, zero);
    // B4 read-flip support (RAVA_PINNED_READ, emit-time, default OFF): the inline
    // cells[slot] store would bypass the pinned-mirror dual-store and leave the
    // mirror stale for compiled writes — route EVERY handle putfield through the
    // callback (rava_putfield → put_field_hint → mirror). Experiment-only codegen.
    if crate::nextgen::pinned_read_enabled() {
        b.ins().jump(cb, &[]);
    } else {
        b.ins().brif(isnull, cb, &[], scan, &[]);
    }

    b.switch_to_block(scan);
    b.seal_block(scan);
    // Descriptor structure readonly under no_alloc → CSEs with the getfields' decode; the store
    // stays a normal mutable write.
    let mf_struct = if env.no_alloc { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let base = b.ins().load(I64, mf_struct, ctx_cell, Offset32::new(16));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(obj, mask);
    let stride = b.ins().iconst(I64, 16);
    let off = b.ins().imul(h, stride);
    let desc = b.ins().iadd(base, off);
    let ids = b.ins().load(I64, mf_struct, desc, Offset32::new(0));
    let cells = b.ins().load(I64, mf_struct, desc, Offset32::new(8));
    let count = b.ins().load(I32, mf_struct, cells, Offset32::new(-8));
    let slotc = b.ins().iconst(I32, slot as i64);
    let in_range = b.ins().icmp(IntCC::UnsignedLessThan, slotc, count);
    b.ins().brif(in_range, verify, &[], cb, &[]);

    b.switch_to_block(verify);
    b.seal_block(verify);
    let id_i = b.ins().load(I32, mf_struct, ids, Offset32::new(slot as i32 * 4));
    let fidc = b.ins().iconst(I32, fid);
    let ok = b.ins().icmp(IntCC::Equal, id_i, fidc);
    b.ins().brif(ok, hit, &[], cb, &[]);

    b.switch_to_block(hit);
    b.seal_block(hit);
    b.ins().store(MemFlags::trusted(), val_cell, cells, Offset32::new(slot as i32 * 8));
    b.ins().jump(done, &[]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let kindc = b.ins().iconst(I64, kind);
    b.ins().call(env.putfield, &[obj, fidc2, val_cell, kindc]);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// Constant-slot native getfield (per-class layout pre-population active): the
/// field lives at the class-constant `slot`, so read it with one verified load
/// instead of the id-scan / hint table. Verify `slot < count && ids[slot]==fid`
/// — a guard that keeps it CORRECT for a non-pre-populated object (wrong slot →
/// callback, exactly the interpreter's result); on hit load `cells[slot]`. In an
/// invariant method (no writes/allocs/real-calls) the loads are `readonly` and
/// at constant offsets, so the egraph hoists loop-invariant field reads out of
/// loops (field-LICM, matching the JVM). Null receiver → callback.
pub(crate) fn native_getfield_const(
    b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64, slot: u32, jt: JT,
) -> Value {
    native_getfield_const_proven(b, env, ctx_cell, obj, fid, slot, jt, false)
}

pub(crate) fn native_getfield_const_proven(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    slot: u32,
    jt: JT,
    proven_nonnull: bool,
) -> Value {
    let res_ty = jt_ty(jt);
    // Descriptor STRUCTURE (base/ids/cells ptrs + count + id) is immutable when no GC/move can
    // occur (no_alloc) — readonly so the egraph CSEs the per-field re-decode even with putfields.
    // The field CELL value is readonly only when NO putfield exists (invariant_fields).
    let mf_struct = if env.no_alloc { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let mf_cell = if env.invariant_fields { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let scan = b.create_block();
    let verify = b.create_block();
    let hit = b.create_block();
    let cb = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, res_ty);

    if proven_nonnull {
        b.ins().jump(scan, &[]);
    } else {
        let zero = b.ins().iconst(I64, 0);
        let isnull = b.ins().icmp(IntCC::Equal, obj, zero);
        b.ins().brif(isnull, cb, &[], scan, &[]);
    }

    b.switch_to_block(scan);
    b.seal_block(scan);
    let base = b.ins().load(I64, mf_struct, ctx_cell, Offset32::new(16));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(obj, mask);
    let stride = b.ins().iconst(I64, 16); // size_of ObjDesc
    let off = b.ins().imul(h, stride);
    let desc = b.ins().iadd(base, off);
    let ids = b.ins().load(I64, mf_struct, desc, Offset32::new(0));
    let cells = b.ins().load(I64, mf_struct, desc, Offset32::new(8));
    let count = b.ins().load(I32, mf_struct, cells, Offset32::new(-8));
    let slotc = b.ins().iconst(I32, slot as i64);
    let in_range = b.ins().icmp(IntCC::UnsignedLessThan, slotc, count);
    b.ins().brif(in_range, verify, &[], cb, &[]);

    b.switch_to_block(verify);
    b.seal_block(verify);
    let id_i = b.ins().load(I32, mf_struct, ids, Offset32::new(slot as i32 * 4));
    let fidc = b.ins().iconst(I32, fid);
    let ok = b.ins().icmp(IntCC::Equal, id_i, fidc);
    b.ins().brif(ok, hit, &[], cb, &[]);

    b.switch_to_block(hit);
    b.seal_block(hit);
    let cell = b.ins().load(I64, mf_cell, cells, Offset32::new(slot as i32 * 8));
    let sv = cell_to_sv(b, cell, jt);
    b.ins().jump(merge, &[sv.v.into()]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let is_ref = b.ins().iconst(I64, 0);
    let call = b.ins().call(env.getfield, &[obj, fidc2, is_ref]);
    let res = b.inst_results(call)[0];
    let svc = cell_to_sv(b, res, jt);
    b.ins().jump(merge, &[svc.v.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}
