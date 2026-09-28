//! Reference field access emitted inline, including the write barrier it implies.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::{ARRAY_TAG, REF_TAG};
use crate::jit::*;

/// FK_OBJ as an immediate. The non-arena build never selects it (its `objraw` guard is
/// `None`), but the emitter still has to typecheck there.
#[cfg(feature = "next-gen-gc")]
const KIND_OBJ: i64 = crate::heap::FK_OBJ as i64;
#[cfg(not(feature = "next-gen-gc"))]
const KIND_OBJ: i64 = 8;

/// May a handle reference READ be finished from the FieldBlob kind byte instead of the
/// callback? Refused under RAVA_PINNED_READ, where `get_field_hint` serves the pinned
/// mirror and the blob is no longer the authority.
fn ref_kind_service_ok() -> bool {
    inline_fields_enabled() && !crate::nextgen::pinned_read_enabled()
}

/// FIELD2 — master switch for the two widenings below, read ONCE at EMIT time so the
/// machine code is byte-identical when it is off (`RAVA_NO_EMIT_FIELD2=1`, or the
/// blanket `RAVA_NO_EMIT_MORE`). DEFAULT ON.
///
/// What it gates:
///   1. the ABSENT-FIELD service (`emit_ref_absent_tail`) — the last arm of a handle
///      reference READ that still crossed to the host;
///   2. dropping the redundant hint WRITE-BACK on a hint HIT (see `scan_found` below).
/// If it is off, both emitters keep their previous block graph exactly.
fn field2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_EMIT_FIELD2").is_none()
    })
}

/// ABSENT-FIELD SERVICE — finish a handle reference read whose field is NOT IN THE BLOB
/// AT ALL, without leaving machine code.
///
/// This is the arm the id scan falls out of, and until now it was an unconditional
/// `rava_getfield`. It is not an error path: two inlined intrinsics reach it BY DESIGN
/// on a shape they then handle by value — `BitSet.get` reads `$bits` and treats a
/// missing array as the empty set, and the Map.Entry `$key`/`$val` inline reads treat an
/// absent field exactly as the native's `Ok(None)`. Both therefore paid a full host
/// transition per execution to be told "no such field".
///
/// WHAT THE GUARD PROVES, and it has to prove the callback's ANSWER, not just its shape:
///   * `[ctx+12] == 0` — no COW region is armed, so `rava_getfield` cannot serve this
///     read out of a worker's TLS shadow (a shadow write can name a field the shared
///     blob has no slot for, and only the callback can see it). Armed → the callback,
///     byte-for-byte the old behaviour.
///   * `obj >> 32 == REF_TAG_HI` — the receiver is a plain heap handle, so the callback
///     would take its `untag_ref` fast path. This is load-bearing: an ARENA receiver
///     that mis-decodes through the handle scan ALSO falls out of the loop here, and its
///     correct answer only exists on the far side of the call.
/// Given both, `rava_getfield` runs `get_field_hint(h, fid, 0)` over the very blob this
/// scan just walked, `find_id` misses (the emitted loop is `scan_id` instruction for
/// instruction), and it returns `(Value::Null, _)` → `box_ref(Null)` → cell `0`.
///
/// COST IF REMOVED: one host call, on every execution of every inlined BitSet/Entry
/// probe whose field is not present. Adds two loads and two compares to an arm that
/// was a `call` — nothing on any path that hits.
fn emit_ref_absent_tail(
    b: &mut FunctionBuilder,
    ctx_cell: Value,
    obj: Value,
    merge: cranelift::codegen::ir::Block,
    cb: cranelift::codegen::ir::Block,
) {
    let prove = b.create_block();
    let armed = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(12));
    let is_armed = b.ins().icmp_imm(IntCC::NotEqual, armed, 0);
    b.ins().brif(is_armed, cb, &[], prove, &[]);

    b.switch_to_block(prove);
    b.seal_block(prove);
    let hi = b.ins().ushr_imm(obj, 32);
    let is_handle = b.ins().icmp_imm(IntCC::Equal, hi, REF_TAG_HI as i64);
    let z = b.ins().iconst(I64, 0);
    let null_cell: [cranelift::codegen::ir::BlockArg; 1] = [z.into()];
    b.ins().brif(is_handle, merge, &null_cell, cb, &[]);
}

/// `kinds` base of a FieldBlob, derived from `cells`. CELLS-FIRST layout: `cap` lives at
/// `[cells - 4]` and `kinds = cells + 12*cap`. The 16-byte `ObjDesc` no longer caches
/// the pointer, so `native_putfield_ref` already re-derives it exactly this way.
fn emit_kinds_ptr(b: &mut FunctionBuilder, mf: MemFlags, cells: Value) -> Value {
    let cap = b.ins().load(I32, mf, cells, Offset32::new(-4));
    let cap64 = b.ins().uextend(I64, cap);
    let off = b.ins().imul_imm(cap64, 12);
    b.ins().iadd(cells, off)
}

/// KIND-BYTE SERVICE — finish a handle reference read whose CELL is not a REF/ARRAY
/// tagged handle, without leaving machine code.
///
/// The cell alone is ambiguous and that ambiguity is the whole reason this used to be a
/// callback: `0` is `Value::Null` OR an identity-boxed `Integer(0)` sitting in a
/// reference-typed field, and a raw arena pointer is indistinguishable from primitive
/// bits. The KIND byte is what `decode_field` itself switches on, so it disambiguates
/// exactly and by construction:
///
///   FK_NULL → `box_ref(Null)` is 0, so push 0.
///   FK_OBJ  → `box_ref(Obj)` is the raw address, which IS the cell. Push it unchanged.
///   FK_BOX / anything else → the host, which has to put the value in the per-call box
///           table. Refused here: no store-free sequence can do that.
///
/// Costs two loads and a multiply, and ONLY on cells that were already heading for a
/// host call — the REF/ARRAY tagged path above never reaches this.
fn emit_ref_kind_tail(
    b: &mut FunctionBuilder,
    mf_cell: MemFlags,
    kind_addr: Value,
    cell: Value,
    merge: cranelift::codegen::ir::Block,
    cb: cranelift::codegen::ir::Block,
) {
    let k8 = b.ins().load(I8, mf_cell, kind_addr, Offset32::new(0));
    let k = b.ins().uextend(I32, k8);
    let nullv = b.create_block();
    let objq = b.create_block();
    let is_null = b.ins().icmp_imm(IntCC::Equal, k, crate::heap::FK_NULL as i64);
    b.ins().brif(is_null, nullv, &[], objq, &[]);

    b.switch_to_block(nullv);
    b.seal_block(nullv);
    let z = b.ins().iconst(I64, 0);
    b.ins().jump(merge, &[z.into()]);

    b.switch_to_block(objq);
    b.seal_block(objq);
    #[cfg(feature = "next-gen-gc")]
    {
        // An arena pointer stored RAW in a handle object's field. The moving collector
        // finds it by scanning every live blob for FK_OBJ (arena_root_locations), not by
        // a card set, so reading it inline neither hides nor needs a barrier.
        let is_obj = b.ins().icmp_imm(IntCC::Equal, k, crate::heap::FK_OBJ as i64);
        let c: [cranelift::codegen::ir::BlockArg; 1] = [cell.into()];
        b.ins().brif(is_obj, merge, &c, cb, &[]);
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        let _ = cell;
        b.ins().jump(cb, &[]);
    }
}

/// Native putfield for a REFERENCE field at a site whose slot the compiler could not
/// resolve. Splits the arena receiver off the handle scan for the same reason as
/// `native_putfield` — with a store on the far side of the bad decode, not just a read.
pub(crate) fn native_putfield_ref(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
) {
    #[cfg(feature = "next-gen-gc")]
    if arena_slot_probe_ok(fid) {
        let arena_blk = b.create_block();
        let probe = b.create_block();
        let enc = b.create_block();
        let cold = b.create_block();
        let handle_blk = b.create_block();
        let out = b.create_block();

        let is_arena = emit_is_arena(b, obj);
        b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

        // Unwinding frame: no store. Arena arm only, so the handle path is unchanged.
        b.switch_to_block(arena_blk);
        b.seal_block(arena_blk);
        let ff = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
        let faulted = b.ins().icmp_imm(IntCC::NotEqual, ff, 0);
        b.ins().brif(faulted, out, &[], probe, &[]);

        b.switch_to_block(probe);
        b.seal_block(probe);
        let (ok, slot_p1) = emit_arena_slot_probe(b, obj, fid);
        b.ins().brif(ok, enc, &[], cold, &[]);

        b.switch_to_block(enc);
        b.seal_block(enc);
        let (inlineable, encoded) = emit_arena_ref_encode(b, val_cell);
        let store = b.create_block();
        b.ins().brif(inlineable, store, &[], cold, &[]);
        b.switch_to_block(store);
        b.seal_block(store);
        let addr = emit_arena_cell_addr(b, obj, slot_p1);
        b.ins().store(MemFlags::trusted(), encoded, addr, Offset32::new(0));
        // Production GC scans tenured directly; the barrier callback is diagnostics
        // only. Matches `native_putfield_ref_arena_dual`, which must stay its twin.
        if card_mark_callback_enabled() {
            b.ins().call(env.card_mark, &[obj, encoded]);
        }
        b.ins().jump(out, &[]);

        b.switch_to_block(cold);
        b.seal_block(cold);
        let fidc = b.ins().iconst(I64, fid);
        let kindc = b.ins().iconst(I64, 4); // reference kind
        b.ins().call(env.putfield, &[obj, fidc, val_cell, kindc]);
        b.ins().jump(out, &[]);

        b.switch_to_block(handle_blk);
        b.seal_block(handle_blk);
        native_putfield_ref_handle(b, env, ctx_cell, obj, val_cell, fid);
        b.ins().jump(out, &[]);

        b.switch_to_block(out);
        b.seal_block(out);
        return;
    }
    native_putfield_ref_handle(b, env, ctx_cell, obj, val_cell, fid);
}

/// Native putfield for REFERENCE fields on a HANDLE receiver. The value cell is already
/// in the shared tagged form (REF_TAG handle / ARRAY_TAG handle / 0 null / raw arena
/// pointer / BOX index), so everything but a BOX stores natively into an existing slot —
/// writing BOTH the cell and the kind byte (FK_REF/FK_ARRAY/FK_NULL/FK_OBJ by tag) so
/// `decode_field` stays coherent. BOX-tagged values (Str / boxed primitive → field_box
/// append), null receivers, and absent fields fall to the callback.
pub(crate) fn native_putfield_ref_handle(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    val_cell: Value,
    fid: i64,
) {
    block_census::note(&block_census::PUTFIELD);
    let check_null = b.create_block();
    let classify = b.create_block();
    let scan = b.create_block();
    let hint_try = b.create_block();
    let loop_h = b.create_block();
    b.append_block_param(loop_h, I64);
    let body = b.create_block();
    let nxt = b.create_block();
    let found = b.create_block();
    b.append_block_param(found, I64);
    // FIELD2: the slot the LINEAR SCAN found is the only one worth writing back to the
    // hint bucket. Routing the hint HIT through `found` — which is what this emitter and
    // its getfield twin used to do, while both PRIMITIVE twins already split it out —
    // stored the value it had just loaded, on the hot path, into a table other threads
    // read: a pointless dirty line per inline reference store. `scan_found` carries the
    // write-back so `found` is pure. Off ⇒ `None` ⇒ the old graph, store and all.
    let scan_found = if field2_enabled() {
        let blk = b.create_block();
        b.append_block_param(blk, I64);
        Some(blk)
    } else {
        None
    };
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
    // B4 read-flip support: see native_putfield_const — force the callback so the
    // pinned mirror sees every compiled ref putfield (emit-time flag, default OFF).
    if crate::nextgen::pinned_read_enabled() {
        b.ins().jump(cb, &[]);
    } else {
        b.ins().brif(isnull, cb, &[], classify, &[]);
    }

    // Only a plain handle / null stores natively; anything else (BOX) → callback.
    b.switch_to_block(classify);
    b.seal_block(classify);
    let hi = b.ins().ushr_imm(val_cell, 32);
    let ref_hi = b.ins().iconst(I64, REF_TAG >> 32);
    let arr_hi = b.ins().iconst(I64, ARRAY_TAG >> 32);
    let is_ref = b.ins().icmp(IntCC::Equal, hi, ref_hi);
    let is_arr = b.ins().icmp(IntCC::Equal, hi, arr_hi);
    let is_nul = b.ins().icmp(IntCC::Equal, val_cell, zero);
    let ok0 = b.ins().bor(is_ref, is_arr);
    let ok1 = b.ins().bor(ok0, is_nul);
    // FK_OBJ — a RAW ARENA POINTER (non-null, < 2^40, therefore disjoint from every
    // high-word tag) stored into a HANDLE object's reference field. `encode_field`
    // stores exactly these bits with kind FK_OBJ, and the moving collector discovers
    // them by scanning every live FieldBlob for that kind (`arena_root_locations`), NOT
    // through a card set — so an inline store owes no barrier and hides no root. Without
    // this arm an ordinary "arena object into a collection's field" store left machine
    // code for rava_putfield → unbox_ref → put_field_hint, once per write.
    #[cfg(feature = "next-gen-gc")]
    let objraw = if inline_fields_enabled()
        && !crate::nextgen::pinned_read_enabled()
        && !crate::nextgen::pinned_ptrs_enabled()
        && !crate::nextgen::shadow_enabled()
    {
        let vhi40 = b.ins().ushr_imm(val_cell, 40);
        let z = b.ins().icmp_imm(IntCC::Equal, vhi40, 0);
        let nz = b.ins().icmp_imm(IntCC::NotEqual, val_cell, 0);
        Some(b.ins().band(nz, z))
    } else {
        None
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let objraw: Option<Value> = None;
    let ok = match objraw {
        Some(o) => b.ins().bor(ok1, o),
        None => ok1,
    };
    b.ins().brif(ok, scan, &[], cb, &[]);

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
    // kinds_ptr is no longer cached in the (16B) ObjDesc — re-derive from the FieldBlob header
    // (CELLS-FIRST): cells = blob+8, cap = [cells-4], kinds = blob + 8 + 12*cap = cells + 12*cap.
    let cap_k = b.ins().load(I32, MemFlags::trusted(), cells, Offset32::new(-4));
    let cap_k64 = b.ins().uextend(I64, cap_k);
    let kinds_off = b.ins().imul_imm(cap_k64, 12);
    let kinds = b.ins().iadd(cells, kinds_off);
    let count = b.ins().load(I32, MemFlags::trusted(), cells, Offset32::new(-8));
    let count64 = b.ins().uextend(I64, count);
    // Field-hint fast path (see native_getfield): try the cached slot, fall into
    // the shared `found` tail (store cell + kind) on a match. FIELD2: a HIT reaches
    // `found` directly; only the scan detours through `scan_found` to refresh the hint.
    let hint_base = b.ins().load(I64, MemFlags::trusted(), ctx_cell, Offset32::new(24));
    let bucket_off = ((fid as usize & (crate::heap::JIT_FIELD_HINT_N - 1)) * 4) as i32;
    let hintv = b.ins().load(I32, MemFlags::trusted(), hint_base, Offset32::new(bucket_off));
    let hint64 = b.ins().uextend(I64, hintv);
    let hint_ok = b.ins().icmp(IntCC::UnsignedLessThan, hint64, count64);
    b.ins().brif(hint_ok, hint_try, &[], loop_h, &[zero.into()]);

    b.switch_to_block(hint_try);
    b.seal_block(hint_try);
    let hid_off = b.ins().ishl_imm(hint64, 2);
    let hid_addr = b.ins().iadd(ids, hid_off);
    let hid = b.ins().load(I32, MemFlags::trusted(), hid_addr, Offset32::new(0));
    let fidh = b.ins().iconst(I32, fid);
    let hmatch = b.ins().icmp(IntCC::Equal, hid, fidh);
    b.ins().brif(hmatch, found, &[hint64.into()], loop_h, &[zero.into()]);

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
    b.ins().brif(matched, scan_found.unwrap_or(found), &[i.into()], nxt, &[]);

    b.switch_to_block(nxt);
    b.seal_block(nxt);
    let i1 = b.ins().iadd_imm(i, 1);
    b.ins().jump(loop_h, &[i1.into()]);
    b.seal_block(loop_h);

    // FIELD2: scan-only hint write-back (see the block's declaration).
    if let Some(sf) = scan_found {
        b.switch_to_block(sf);
        b.seal_block(sf);
        let si = b.block_params(sf)[0];
        let si32 = b.ins().ireduce(I32, si);
        b.ins().store(MemFlags::trusted(), si32, hint_base, Offset32::new(bucket_off));
        b.ins().jump(found, &[si.into()]);
    }

    b.switch_to_block(found);
    b.seal_block(found);
    let fi = b.block_params(found)[0];
    if scan_found.is_none() {
        let fi32 = b.ins().ireduce(I32, fi);
        b.ins().store(MemFlags::trusted(), fi32, hint_base, Offset32::new(bucket_off));
    }
    let cell_off = b.ins().ishl_imm(fi, 3);
    let cell_addr = b.ins().iadd(cells, cell_off);
    b.ins().store(MemFlags::trusted(), val_cell, cell_addr, Offset32::new(0));
    // Kind byte: FK_REF=4 / FK_ARRAY=5 / FK_NULL=6 / FK_OBJ=8, derived from the tag (the
    // classify block guarantees exactly one of them holds).
    let k4 = b.ins().iconst(I8, 4);
    let k5 = b.ins().iconst(I8, 5);
    let k6 = b.ins().iconst(I8, 6);
    let k_arr = b.ins().select(is_arr, k5, k4);
    let kind_nb = b.ins().select(is_nul, k6, k_arr);
    let kind_b = match objraw {
        Some(o) => {
            let ko = b.ins().iconst(I8, KIND_OBJ);
            b.ins().select(o, ko, kind_nb)
        }
        None => kind_nb,
    };
    let kind_addr = b.ins().iadd(kinds, fi);
    b.ins().store(MemFlags::trusted(), kind_b, kind_addr, Offset32::new(0));
    b.ins().jump(done, &[]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let kindc = b.ins().iconst(I64, 4); // reference kind for the callback
    b.ins().call(env.putfield, &[obj, fidc2, val_cell, kindc]);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// Native getfield for a REFERENCE field at a site whose slot the compiler could not
/// resolve. The handle scan below decodes `obj` as a handle, so an arena receiver has
/// to be split off FIRST (see `native_getfield` for the same hazard); the arena arm
/// resolves its slot from the inline cache and classifies the cell's 2-bit tag.
pub(crate) fn native_getfield_ref(b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64) -> Value {
    #[cfg(feature = "next-gen-gc")]
    if arena_slot_probe_ok(fid) {
        let arena_blk = b.create_block();
        let fast = b.create_block();
        let cold = b.create_block();
        let handle_blk = b.create_block();
        let out = b.create_block();
        b.append_block_param(out, I64);

        let is_arena = emit_is_arena(b, obj);
        b.ins().brif(is_arena, arena_blk, &[], handle_blk, &[]);

        b.switch_to_block(arena_blk);
        b.seal_block(arena_blk);
        let (ok, slot_p1) = emit_arena_slot_probe(b, obj, fid);
        b.ins().brif(ok, fast, &[], cold, &[]);

        b.switch_to_block(fast);
        b.seal_block(fast);
        let addr = emit_arena_cell_addr(b, obj, slot_p1);
        let mf = if env.invariant_fields {
            MemFlags::trusted().with_readonly()
        } else {
            MemFlags::trusted()
        };
        let cell = b.ins().load(I64, mf, addr, Offset32::new(0));
        emit_arena_ref_classify(b, cell, out, cold);

        b.switch_to_block(cold);
        b.seal_block(cold);
        let fidc = b.ins().iconst(I64, fid);
        let isr = b.ins().iconst(I64, 1);
        let call = b.ins().call(env.getfield, &[obj, fidc, isr]);
        let res = b.inst_results(call)[0];
        b.ins().jump(out, &[res.into()]);

        b.switch_to_block(handle_blk);
        b.seal_block(handle_blk);
        let v = native_getfield_ref_handle(b, env, ctx_cell, obj, fid);
        b.ins().jump(out, &[v.into()]);

        b.switch_to_block(out);
        b.seal_block(out);
        return b.block_params(out)[0];
    }
    native_getfield_ref_handle(b, env, ctx_cell, obj, fid)
}

/// Native getfield for a REFERENCE field on a HANDLE receiver. Scans the object
/// descriptor; a found slot holds a flat tagged cell (REF_TAG/ARRAY_TAG → a valid JIT
/// operand cell, pushed as-is; ABOX_TAG → a Str/boxed in field_box, routed to the
/// callback to box it into the transient table; null / raw-arena finish inline off the
/// kind byte). A NOT-FOUND id no longer falls back: the scan running out is a PROOF of
/// absence, and `emit_ref_absent_tail` turns it into the same `0` the callback computes
/// (FIELD2). Only a null receiver, an ABOX cell, and the two arms the absent tail
/// refuses (armed COW region / a receiver that is not a plain heap handle) still cross.
fn native_getfield_ref_handle(b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64) -> Value {
    let scan = b.create_block();
    let hint_try = b.create_block();
    let loop_h = b.create_block();
    b.append_block_param(loop_h, I64);
    let body = b.create_block();
    let nxt = b.create_block();
    let found = b.create_block();
    b.append_block_param(found, I64);
    let plain = b.create_block();
    let cb = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    // FIELD2, both halves. `scan_found`: the hint write-back moves off the hit path (see
    // the putfield twin). `absent`: the id scan running out is served in machine code
    // instead of by `rava_getfield` (see `emit_ref_absent_tail`). Both `None` when the
    // switch is off, and then every `unwrap_or` below restores the original edge, so the
    // emitted graph is unchanged.
    let (scan_found, absent) = if field2_enabled() {
        let sf = b.create_block();
        b.append_block_param(sf, I64);
        // `ref_kind_service_ok` also refuses under RAVA_PINNED_READ, where the blob is
        // no longer the sole authority for a handle's fields — the same reason the kind
        // tail below refuses there.
        (Some(sf), ref_kind_service_ok().then(|| b.create_block()))
    } else {
        (None, None)
    };

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
    // Field-hint fast path (see native_getfield): try the cached slot, then fall
    // into the shared `found` tail (tag check) on a match. FIELD2: a HIT reaches
    // `found` directly; only the scan detours through `scan_found` to refresh the hint.
    let hint_base = b.ins().load(I64, MemFlags::trusted(), ctx_cell, Offset32::new(24));
    let bucket_off = ((fid as usize & (crate::heap::JIT_FIELD_HINT_N - 1)) * 4) as i32;
    let hintv = b.ins().load(I32, MemFlags::trusted(), hint_base, Offset32::new(bucket_off));
    let hint64 = b.ins().uextend(I64, hintv);
    let hint_ok = b.ins().icmp(IntCC::UnsignedLessThan, hint64, count64);
    b.ins().brif(hint_ok, hint_try, &[], loop_h, &[zero.into()]);

    b.switch_to_block(hint_try);
    b.seal_block(hint_try);
    let hid_off = b.ins().ishl_imm(hint64, 2);
    let hid_addr = b.ins().iadd(ids, hid_off);
    let hid = b.ins().load(I32, MemFlags::trusted(), hid_addr, Offset32::new(0));
    let fidh = b.ins().iconst(I32, fid);
    let hmatch = b.ins().icmp(IntCC::Equal, hid, fidh);
    b.ins().brif(hmatch, found, &[hint64.into()], loop_h, &[zero.into()]);

    b.switch_to_block(loop_h);
    let i = b.block_params(loop_h)[0];
    let done = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, count64);
    // FIELD2: scan exhausted = the blob provably has no slot for this id. That is the
    // ABSENT case, not an error — serve it inline when the two preconditions hold.
    b.ins().brif(done, absent.unwrap_or(cb), &[], body, &[]);

    b.switch_to_block(body);
    b.seal_block(body);
    let id_off = b.ins().ishl_imm(i, 2);
    let id_addr = b.ins().iadd(ids, id_off);
    let id_i = b.ins().load(I32, MemFlags::trusted(), id_addr, Offset32::new(0));
    let fidc = b.ins().iconst(I32, fid);
    let matched = b.ins().icmp(IntCC::Equal, id_i, fidc);
    b.ins().brif(matched, scan_found.unwrap_or(found), &[i.into()], nxt, &[]);

    b.switch_to_block(nxt);
    b.seal_block(nxt);
    let i1 = b.ins().iadd_imm(i, 1);
    b.ins().jump(loop_h, &[i1.into()]);
    b.seal_block(loop_h);

    // FIELD2: scan-only hint write-back — a hint HIT re-stored the slot it had just
    // read, on the hot path, into a table shared with every other thread.
    if let Some(sf) = scan_found {
        b.switch_to_block(sf);
        b.seal_block(sf);
        let si = b.block_params(sf)[0];
        let si32 = b.ins().ireduce(I32, si);
        b.ins().store(MemFlags::trusted(), si32, hint_base, Offset32::new(bucket_off));
        b.ins().jump(found, &[si.into()]);
    }

    // FIELD2: absent → `Value::Null` → operand cell 0, or the callback if either
    // precondition fails. Filled here (before `merge`/`cb` are sealed) so both edges
    // are recorded.
    if let Some(ab) = absent {
        b.switch_to_block(ab);
        b.seal_block(ab);
        emit_ref_absent_tail(b, ctx_cell, obj, merge, cb);
    }

    b.switch_to_block(found);
    b.seal_block(found);
    let fi = b.block_params(found)[0];
    if scan_found.is_none() {
        let fi32 = b.ins().ireduce(I32, fi);
        b.ins().store(MemFlags::trusted(), fi32, hint_base, Offset32::new(bucket_off));
    }
    let cell_off = b.ins().ishl_imm(fi, 3);
    let cell_addr = b.ins().iadd(cells, cell_off);
    let cell = b.ins().load(I64, MemFlags::trusted(), cell_addr, Offset32::new(0));
    // Push the cell directly ONLY if it is a real tagged handle (REF/ARRAY).
    // Everything else — null (0), a boxed primitive in a reference field (raw
    // bits, e.g. identity-boxed Integer(0)), or an ABOX Str — goes to the callback
    // which boxes it correctly (the boxed-zero/null ambiguity).
    let hi = b.ins().ushr_imm(cell, 32);
    let ref_hi = b.ins().iconst(I64, REF_TAG_HI as i64);
    let arr_hi = b.ins().iconst(I64, ARRAY_TAG_HI as i64);
    let is_ref_h = b.ins().icmp(IntCC::Equal, hi, ref_hi);
    let is_arr_h = b.ins().icmp(IntCC::Equal, hi, arr_hi);
    let is_handle = b.ins().bor(is_ref_h, is_arr_h);
    // Same kind-byte service as the const-slot twin: null / raw-arena cells finish here
    // instead of crossing to `get_field_hint`.
    if ref_kind_service_ok() {
        let kindq = b.create_block();
        b.ins().brif(is_handle, plain, &[], kindq, &[]);
        b.switch_to_block(kindq);
        b.seal_block(kindq);
        let kinds = emit_kinds_ptr(b, MemFlags::trusted(), cells);
        let ka = b.ins().iadd(kinds, fi);
        emit_ref_kind_tail(b, MemFlags::trusted(), ka, cell, merge, cb);
    } else {
        b.ins().brif(is_handle, plain, &[], cb, &[]);
    }

    b.switch_to_block(plain);
    b.seal_block(plain);
    b.ins().jump(merge, &[cell.into()]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let is_ref = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.getfield, &[obj, fidc2, is_ref]);
    let res = b.inst_results(call)[0];
    b.ins().jump(merge, &[res.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Constant-slot native getfield for a REFERENCE field (per-class layout). Like
/// `native_getfield_const`, but on a slot hit the cell is pushed directly ONLY
/// if it is a real tagged handle (REF/ARRAY); null / boxed-primitive / ABOX-Str
/// route to the callback (same box/null decode as `native_getfield_ref`). Wrong
/// slot / out of range / null receiver also → callback. Readonly in invariant
/// methods → hoistable.
pub(crate) fn native_getfield_ref_const(
    b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64, slot: u32,
) -> Value {
    native_getfield_ref_const_proven(b, env, ctx_cell, obj, fid, slot, false)
}

pub(crate) fn native_getfield_ref_const_proven(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    slot: u32,
    proven_nonnull: bool,
) -> Value {
    // Descriptor structure readonly under no_alloc (CSE the decode); cell value readonly only
    // under invariant_fields (a putfield must be observed). See method_no_alloc.
    let mf_struct = if env.no_alloc { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let mf_cell = if env.invariant_fields { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let scan = b.create_block();
    let verify = b.create_block();
    let hit = b.create_block();
    let plain = b.create_block();
    let cb = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);

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
    let cell = b.ins().load(I64, mf_cell, cells, Offset32::new(slot as i32 * 8));
    let hi = b.ins().ushr_imm(cell, 32);
    let ref_hi = b.ins().iconst(I64, REF_TAG_HI as i64);
    let arr_hi = b.ins().iconst(I64, ARRAY_TAG_HI as i64);
    let is_ref_h = b.ins().icmp(IntCC::Equal, hi, ref_hi);
    let is_arr_h = b.ins().icmp(IntCC::Equal, hi, arr_hi);
    let is_handle = b.ins().bor(is_ref_h, is_arr_h);
    // A null or raw-arena cell used to go straight to the host from here — the two
    // commonest non-handle contents of a reference field. Ask the kind byte instead.
    if ref_kind_service_ok() {
        let kindq = b.create_block();
        b.ins().brif(is_handle, plain, &[], kindq, &[]);
        b.switch_to_block(kindq);
        b.seal_block(kindq);
        let kinds = emit_kinds_ptr(b, mf_struct, cells);
        let ka = b.ins().iadd_imm(kinds, slot as i64);
        emit_ref_kind_tail(b, mf_cell, ka, cell, merge, cb);
    } else {
        b.ins().brif(is_handle, plain, &[], cb, &[]);
    }

    b.switch_to_block(plain);
    b.seal_block(plain);
    b.ins().jump(merge, &[cell.into()]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let fidc2 = b.ins().iconst(I64, fid);
    let is_ref = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.getfield, &[obj, fidc2, is_ref]);
    let res = b.inst_results(call)[0];
    b.ins().jump(merge, &[res.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}
