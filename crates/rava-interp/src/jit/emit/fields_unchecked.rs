//! Field access with the null and shape checks already discharged by the caller.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Straight-line const-slot putfield (no id verify; universal pre-population). A
/// prior latched fault or a null receiver routes to the callback (which raises
/// the NPE); else a direct `cells[slot]` store. No scan/verify branches.
pub(crate) fn native_putfield_const_unchecked(
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
    let store = b.create_block();
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
    b.ins().brif(isnull, cb, &[], store, &[]);

    b.switch_to_block(store);
    b.seal_block(store);
    // The descriptor structure (base + cells ptr) is readonly under no_alloc, so it CSEs with
    // the getfields' decode of the same object; the STORE below is a normal mutable write.
    let mf_struct = if env.no_alloc { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let base = b.ins().load(I64, mf_struct, ctx_cell, Offset32::new(16));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(obj, mask);
    let stride = b.ins().iconst(I64, 16);
    let off = b.ins().imul(h, stride);
    let desc = b.ins().iadd(base, off);
    let cells = b.ins().load(I64, mf_struct, desc, Offset32::new(8));
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

/// Straight-line const-slot getfield (no id verify): null → NPE-via-callback +
/// return; else readonly `cells[slot]` load with no merge → hoistable.
pub(crate) fn native_getfield_const_unchecked(
    b: &mut FunctionBuilder, env: &Env, ctx_cell: Value, obj: Value, fid: i64, slot: u32, jt: JT,
) -> Value {
    native_getfield_const_unchecked_proven(b, env, ctx_cell, obj, fid, slot, jt, false)
}

pub(crate) fn native_getfield_const_unchecked_proven(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    obj: Value,
    fid: i64,
    slot: u32,
    jt: JT,
    proven_nonnull: bool,
) -> Value {
    // Descriptor structure readonly under no_alloc (CSE the decode); cell value readonly only
    // under invariant_fields (a putfield must be observed). See method_no_alloc.
    let mf_struct = if env.no_alloc { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let mf_cell = if env.invariant_fields { MemFlags::trusted().with_readonly() } else { MemFlags::trusted() };
    let load = b.create_block();
    let npe = if proven_nonnull { None } else { Some(b.create_block()) };
    if let Some(npe) = npe {
        let zero = b.ins().iconst(I64, 0);
        let isnull = b.ins().icmp(IntCC::Equal, obj, zero);
        b.ins().brif(isnull, npe, &[], load, &[]);
        b.switch_to_block(npe);
        b.seal_block(npe);
        let fidc = b.ins().iconst(I64, fid);
        let is_ref = b.ins().iconst(I64, 0);
        b.ins().call(env.getfield, &[obj, fidc, is_ref]);
        let z = b.ins().iconst(I64, 0);
        b.ins().return_(&[z]);
    } else {
        b.ins().jump(load, &[]);
    }

    // Non-null: handle → obj_desc → cells → cells[slot], all readonly-hoistable.
    b.switch_to_block(load);
    b.seal_block(load);
    let base = b.ins().load(I64, mf_struct, ctx_cell, Offset32::new(16));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(obj, mask);
    let stride = b.ins().iconst(I64, 16);
    let off = b.ins().imul(h, stride);
    let desc = b.ins().iadd(base, off);
    let cells = b.ins().load(I64, mf_struct, desc, Offset32::new(8));
    let cell = b.ins().load(I64, mf_cell, cells, Offset32::new(slot as i32 * 8));
    let sv = cell_to_sv(b, cell, jt);
    sv.v
}
