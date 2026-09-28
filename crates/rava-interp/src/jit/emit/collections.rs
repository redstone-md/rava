//! Map and BitSet probes emitted inline.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::{ARRAY_TAG, REF_TAG};
use crate::jit::*;

/// STEP 3 — the pure-register open-addressing probe loop for an INT-keyed
/// `HashMap.get`/`containsKey` on an arena ArenaMap (the `Integer.valueOf+get`
/// fusion supplies the raw key int). ZERO FFI on the hot path: the query hash is
/// computed in IR from the int, the table is walked bucket-by-bucket in registers,
/// and an int bucket is matched with a single `icmp` on its raw `key_int` cell.
/// No allocation happens in the loop, so the raw arena pointers held in registers
/// stay valid (no spill/reload). A `get` HIT whose value is an ABOX-boxed primitive
/// is the one place a thin (GC-free) helper converts the cell; handle/obj/null
/// values convert in IR. A non-arena / non-map receiver boxes the int and takes the
/// general helper (spill/reload — it may collect). Leaves the result on `stack`.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_amap_int_probe(
    b: &mut FunctionBuilder,
    env: &Env,
    recv: Value,
    key_i32: Value,
    is_contains: bool,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
) -> Option<()> {
    // Compile-time census: how many SITES this probe is emitted at. Zero here and zero at
    // runtime are different diagnoses -- never emitted, versus emitted and never taken.
    AM_SITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hdr = crate::nextgen::HDR as i32; // 24
    let map_cid = crate::arena_map::class_id() as i64;
    let guard = b.create_block();
    let fast = b.create_block();
    let fallback = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);

    // Guard 1: a low arena pointer. (A $map handle has its tag in the high bits, so
    // dereferencing it would fault — check before any load.)
    let is_a = emit_is_arena(b, recv);
    b.ins().brif(is_a, guard, &[], fallback, &[]);

    // Guard 2: the header's class id is exactly ArenaMap.
    b.switch_to_block(guard);
    b.seal_block(guard);
    bump(b, &AM_ARENA);
    let hdrword = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(0));
    let cid = b.ins().band_imm(hdrword, 0xFFFF_FFFF);
    let is_map = b.ins().icmp_imm(IntCC::Equal, cid, map_cid);
    b.ins().brif(is_map, fast, &[], fallback, &[]);

    // ── fast: hash the int in IR, load cap+table, seed the probe.
    b.switch_to_block(fast);
    b.seal_block(fast);
    bump(b, &AM_FAST);
    let n64 = b.ins().sextend(I64, key_i32); // MapKey::Int(n as i64)
    // hash_of: h = (n ^ (n>>32)) as i32 ; h ^= (h as u32) >> 16
    let t = b.ins().sshr_imm(n64, 32);
    let xa = b.ins().bxor(n64, t);
    let h0 = b.ins().ireduce(I32, xa);
    let s16 = b.ins().ushr_imm(h0, 16);
    let hash_i32 = b.ins().bxor(h0, s16);
    let hash_u = b.ins().uextend(I64, hash_i32); // (hash as u32) for the mask
    let cap = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(hdr));
    let table = b.ins().load(I64, MemFlags::trusted(), recv, Offset32::new(hdr + 16));
    let mask = b.ins().iadd_imm(cap, -1);
    let idx0 = b.ins().band(hash_u, mask);
    let loop_hdr = b.create_block();
    b.append_block_param(loop_hdr, I64); // idx
    b.append_block_param(loop_hdr, I64); // trip count (defensive bound)
    let z = b.ins().iconst(I64, 0);
    b.ins().jump(loop_hdr, &[idx0.into(), z.into()]);

    // ── loop header (NOT sealed: the advance edge loops back).
    b.switch_to_block(loop_hdr);
    let idx = b.block_params(loop_hdr)[0];
    let trip = b.block_params(loop_hdr)[1];
    let body = b.create_block();
    // Shared MISS block (EMPTY bucket or probe overflow): containsKey → 0 (false); get →
    // the map's default-return (Null normally, the fastutil defaultReturnValue if set),
    // via a cold callback — the rare miss path keeps the hot HIT path zero-FFI.
    let miss_blk = b.create_block();
    let over = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, trip, cap);
    b.ins().brif(over, miss_blk, &[], body, &[]);

    // ── body: load head, branch on state.
    b.switch_to_block(body);
    b.seal_block(body);
    let boff = b.ins().imul_imm(idx, (BW_BYTES) as i64);
    let bucket = b.ins().iadd(table, boff);
    let head = b.ins().load(I64, MemFlags::trusted(), bucket, Offset32::new(hdr));
    let state = b.ins().band_imm(head, 3);
    let occ = b.create_block();
    let is_empty = b.ins().icmp_imm(IntCC::Equal, state, 0); // EMPTY
    b.ins().brif(is_empty, miss_blk, &[], occ, &[]);

    // ── occ: a hit needs OCC + INT_KEY + hash match + key_int == n.
    b.switch_to_block(occ);
    b.seal_block(occ);
    let is_occ = b.ins().icmp_imm(IntCC::Equal, state, 1); // OCC
    let int_sh = b.ins().ushr_imm(head, 2);
    let int_bit = b.ins().band_imm(int_sh, 1);
    let is_int = b.ins().icmp_imm(IntCC::Equal, int_bit, 1);
    let hash_sh = b.ins().sshr_imm(head, 3);
    let bhash = b.ins().ireduce(I32, hash_sh); // (head>>3) as i32
    let hash_eq = b.ins().icmp(IntCC::Equal, bhash, hash_i32);
    let key_int = b.ins().load(I64, MemFlags::trusted(), bucket, Offset32::new(hdr + 8));
    let key_eq = b.ins().icmp(IntCC::Equal, key_int, n64);
    let m1 = b.ins().band(is_occ, is_int);
    let m2 = b.ins().band(m1, hash_eq);
    let matched = b.ins().band(m2, key_eq);
    let found = b.create_block();
    let advance = b.create_block();
    b.ins().brif(matched, found, &[], advance, &[]);

    // ── found: containsKey → 1; get → convert the value cell to a JIT operand.
    b.switch_to_block(found);
    b.seal_block(found);
    if is_contains {
        let one = b.ins().iconst(I64, 1);
        b.ins().jump(merge, &[one.into()]);
    } else {
        let vcell = b.ins().load(I64, MemFlags::trusted(), bucket, Offset32::new(hdr + 24));
        // null (0) → 0; tag 0b01 Ref → h|REF_TAG; 0b10 Array → h|ARRAY_TAG;
        // 0b00 (non-null) raw Obj → unchanged; 0b11 ABOX → thin helper.
        let tag = b.ins().band_imm(vcell, 3);
        let h = b.ins().ushr_imm(vcell, 2);
        let ref_form = b.ins().bor_imm(h, REF_TAG);
        let arr_form = b.ins().bor_imm(h, ARRAY_TAG);
        let is_arr = b.ins().icmp_imm(IntCC::Equal, tag, 2);
        let handle = b.ins().select(is_arr, arr_form, ref_form);
        let is_raw = b.ins().icmp_imm(IntCC::Equal, tag, 0); // Obj or null
        let nonbox = b.ins().select(is_raw, vcell, handle); // raw/obj/null vs Ref/Array
        let is_box = b.ins().icmp_imm(IntCC::Equal, tag, 3);
        let boxblk = b.create_block();
        let donev = b.create_block();
        b.append_block_param(donev, I64);
        b.ins().brif(is_box, boxblk, &[], donev, &[nonbox.into()]);
        b.switch_to_block(boxblk);
        b.seal_block(boxblk);
        let call = b.ins().call(env.amap_box_value_fn, &[vcell]);
        let boxed = b.inst_results(call)[0];
        b.ins().jump(donev, &[boxed.into()]);
        b.switch_to_block(donev);
        b.seal_block(donev);
        let result = b.block_params(donev)[0];
        b.ins().jump(merge, &[result.into()]);
    }

    // ── advance: idx = (idx+1) & mask, trip+1, loop.
    b.switch_to_block(advance);
    b.seal_block(advance);
    let idx_inc = b.ins().iadd_imm(idx, 1);
    let nidx = b.ins().band(idx_inc, mask);
    let ntrip = b.ins().iadd_imm(trip, 1);
    b.ins().jump(loop_hdr, &[nidx.into(), ntrip.into()]);
    b.seal_block(loop_hdr);

    // ── miss: key absent. containsKey → 0; get → the configured default-return cell
    //    (cold callback, hot HIT path stays zero-FFI). `recv` is the arena map pointer.
    b.switch_to_block(miss_blk);
    b.seal_block(miss_blk);
    let miss_val = if is_contains {
        b.ins().iconst(I64, 0)
    } else {
        let dcall = b.ins().call(env.amap_default_fn, &[recv]);
        b.inst_results(dcall)[0]
    };
    b.ins().jump(merge, &[miss_val.into()]);

    // ── fallback: a non-arena / non-map receiver. Box the int and take the general
    //    helper (which can collect → spill/reload around the call).
    b.switch_to_block(fallback);
    b.seal_block(fallback);
    let ni = b.ins().sextend(I64, key_i32);
    let bc = b.ins().call(env.box_int_fn, &[ni]);
    let boxed_key = b.inst_results(bc)[0];
    spill_for_gc(b, vars, stack, locals_ptr);
    let f = if is_contains { env.amap_contains_fn } else { env.amap_get_fn };
    let call = b.ins().call(f, &[recv, boxed_key]);
    let fres = b.inst_results(call)[0];
    reload_after_gc(b, vars, stack, locals_ptr);
    b.ins().jump(merge, &[fres.into()]);

    // ── merge.
    b.switch_to_block(merge);
    b.seal_block(merge);
    let result = b.block_params(merge)[0];
    if is_contains {
        let r = b.ins().ireduce(I32, result);
        push(stack, r, JT::Int);
    } else {
        push(stack, result, JT::Ref);
    }
    Some(())
}

/// Inline `java.util.BitSet.get(I)Z` (next-gen-gc): read the packed `$bits` word
/// straight off the heap, matching `Vm::jit_bitset_get` bit-for-bit, so the hot
/// voxel `isFull`/`isFullWide` path stops paying the `rava_bitset_get` FFI (buffer
/// marshal + call + spill/reload) on every bit test. Structure — a runtime diamond:
///
///   FAST arm (all guards pass): `recv` is a REF_TAG heap ref, its class word ==
///   the interned `java.util.BitSet` key, and `idx >= 0`. Read `$bits` (a `long[]`)
///   by id; if absent / not an array → 0 (empty set → false, JVM semantics). Else
///   `wordIndex = idx >>> 6`; if `wordIndex >= words.length` → 0; else
///   `(words[wordIndex] >>> (idx & 63)) & 1`.
///
///   SLOW arm (any guard fails — subclass, non-arena-heap receiver, `idx < 0`): the
///   UNCHANGED `env.bitset_get` FFI with the real call cp, which is the exact oracle
///   (its own fallback to `rava_invoke` throws the `IndexOutOfBoundsException` for
///   `idx < 0` and dispatches a subclass override). Bit-exact by construction: the
///   fast arm reproduces `jit_bitset_get`, the slow arm IS the original call.
///
/// ⛔**THE CALLER'S "NO LIVE REFERENCE OPERAND" PRECONDITION IS NO LONGER THE EMITTER'S.** It read
/// "restricted to sites with no live reference operand BELOW recv+idx, so the slow arm's
/// locals-only reload after a (possibly throwing → allocating → GC-moving) callback is SSA-sound
/// on the diamond arm". True of the OLD graph, and it is a permanent emit-time refusal: a site
/// with a reference on the stack never gets the arm, with no cache to warm and no second chance.
/// The identical wording on `emit_arraylist_get_inline` was priced at **one percent of its
/// population** before the merge-parameter cure lifted it. The cure is applied here now (see the
/// `ref_slots` note in the body), so this emitter is sound at ANY operand stack and the caller's
/// `stack[..len-2].iter().all(|sv| sv.t != JT::Ref)` term in `invoke_virtual` is dead weight that
/// can be dropped — it costs the arm every `BitSet.get` in a shape loop that holds a shape ref.
#[cfg(feature = "next-gen-gc")]
/// Inline `java.util.ArrayList.get(I)Ljava/lang/Object;` — read the element straight out of the
/// `$elems` backing in emitted IR, with no FFI at all.
///
/// ⭐**A CHEAPER HOST CALL IS STILL A HOST CALL.** `ArrayList.get` is already served by
/// `fu_fastpath` and admitted by `fu_routable`, and it still costs **187 ns per element**
/// (`bench/OpCost2.java`: a 16-element index loop is 3000 ns against the JVM's 13 for the whole
/// loop) because reaching `fu_fastpath` means leaving machine code. `bitset_get_inline` is the
/// one shape in this tree that does not, and this is its twin.
///
/// Guards, in order, each falling to the unchanged FFI so a miss is only slower and never wrong:
/// a REF_TAG receiver and a non-negative index; the receiver's EXACT class word (a subclass, or
/// a not-yet-published word, goes slow); an `$elems` cell that really is an array; and the index
/// in bounds — an out-of-bounds index takes the slow arm ON PURPOSE, so the real method throws
/// the `IndexOutOfBoundsException` its contract specifies rather than the array callback's
/// `ArrayIndexOutOfBounds`.
///
/// The element load itself is `native_aaload`, which already returns REF/ARRAY/0 cells as-is and
/// routes an ABOX cell through the read callback to box it — so the tagged-cell contract is one
/// implementation, not two.
#[cfg(feature = "next-gen-gc")]
/// `kind` is the interpreter's invoke kind for the SLOW arm: 1 for `invokevirtual`, 3 for
/// `invokeinterface`. It must match the opcode the arm replaced — the fallback re-enters the
/// same resolution the bytecode asked for, and MC reaches `get` through `java/util/List`
/// (interface) for **10.37% of all 13.5M seam callbacks**, the single largest callee there.
pub(crate) fn emit_arraylist_get_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
) -> Option<()> {
    let idx_sv = stack.pop()?;
    let recv_sv = stack.pop()?;
    let idx = idx_sv.v; // I32
    let recv = recv_sv.v; // I64 reference cell

    // ⭐**CARRY LIVE REFERENCES THROUGH THE MERGE.** The call site used to refuse any operand
    // stack holding a `JT::Ref`, because the slow arm's callback can collect and MOVE objects and
    // this merge could not re-materialise them. Counters priced that refusal: the arm fired
    // **15,688 times a boot against a population of 1,537,536 `ArrayList#get` calls** — about one
    // percent — and the restriction was the reason. Same fix as `emit_dispatch_pic` and
    // `guarded_inline`: one merge parameter per live reference, with the fast arm passing its
    // unchanged values through the same ones.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let read_blk = b.create_block();
    let bounds_blk = b.create_block();
    let load_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    // Guard 1: REF_TAG receiver and idx >= 0.
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    let idx_ok = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, idx, 0);
    let g1 = b.ins().band(is_ref, idx_ok);
    b.ins().brif(g1, cls_blk, &[], slow, &[]);

    // Guard 2: EXACT class — the receiver's class key against the published word.
    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    bump(b, &AG_CLS);
    let cw_addr = b.ins().iconst(
        I64,
        (&crate::jit::ARRAYLIST_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g2 = b.ins().band(cw_match, baked_nz);
    b.ins().brif(g2, read_blk, &[], slow, &[]);

    // Guard 3: `$elems` by id, and it must be an array cell.
    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    bump(b, &AG_ELEM);
    let elems = native_getfield_ref(b, env, array_desc_cell, recv, env.elems_fid - 1);
    let ehi = b.ins().ushr_imm(elems, 32);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, ehi, ARRAY_TAG_HI as i64);
    b.ins().brif(is_arr, bounds_blk, &[], slow, &[]);

    // Guard 4: in bounds. Out of bounds goes SLOW so the real method throws.
    b.switch_to_block(bounds_blk);
    b.seal_block(bounds_blk);
    bump(b, &AG_BND);
    let (_data, len) = load_array_desc(b, array_desc_cell, elems);
    let inb = b.ins().icmp(IntCC::UnsignedLessThan, idx, len);
    b.ins().brif(inb, load_blk, &[], slow, &[]);

    // The load. Bounds already proven, so `native_aaload` skips its own check.
    b.switch_to_block(load_blk);
    b.seal_block(load_blk);
    bump(b, &AG_FAST);
    let cell = crate::jit::emit::arrays::native_aaload(b, env, array_desc_cell, elems, idx, true);
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![cell.into()];
    for &i in &ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);

    // SLOW arm: the unchanged fastutil/collection dispatch callback — the exact oracle for a
    // subclass, an absent backing, a negative or out-of-range index, and the first calls before
    // the class word is published.
    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    let idx_cell = to_cell(b, idx_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    b.ins().stack_store(idx_cell, env.args_slot, 8);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 2);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    stack.push(SV {
        v: mparams[0],
        t: JT::Ref,
        src: None,
        arena: false,
    });
    Some(())
}

/// ⭐**ONE KILL SWITCH FOR THE SHAPES CHAINED ONTO THE EXISTING RECOGNISERS**
/// (`RAVA_NO_EMIT_INTRIN2=1`).
///
/// Nothing here is a new arm and nothing here is keyed on a name. Each widening hangs one more
/// guarded shape off the edge of an arm that already exists — the edge that today goes straight
/// to the callback — so switching this off restores byte-for-byte the emission that was there
/// before. `emit_more_disabled()` is consulted as well, so the whole "emit more, cross less"
/// block still prices as a single flag.
pub(crate) fn intrin2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_EMIT_INTRIN2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Reached the `java/util/ArrayList` class compare on a chained `size()` shape.
pub(crate) static LS_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline: the backing array's length, no crossing.
pub(crate) static LS_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Readers for the diag table. Kept beside the counters they read so a boot can answer
/// "emitted and never taken" versus "never emitted" for this shape without a rebuild.
#[allow(dead_code)]
pub(crate) fn ls_cls() -> u64 {
    LS_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
#[allow(dead_code)]
pub(crate) fn ls_fast() -> u64 {
    LS_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// The CORRECTNESS precondition of the `java/util/ArrayList` `size()` shape — checked by the
/// shape itself, so a caller that cannot refuse (every one of them propagates `None` as a compile
/// failure) still gets a graph that is merely slower.
///
/// * `elems_fid` — the id is BAKED, so it has to exist before this method compiles. It does on a
///   default boot: `lifecycle` publishes `$elems` at VM construction whenever any of the three
///   `$elems` arms is armed, and `dalist_get_inline_enabled()` is default-ON.
/// * ⚠**`any_override()` — THE ONE CORRECTNESS CONDITION, AND IT IS NOT OBVIOUS.** The shape
///   answers with the backing array's LENGTH. `Heap::elems_len` returns exactly that, but only
///   while no `java/` class has been overridden with real bytecode; once one has, an overriding
///   class keeps its logical count in `$size` and the physical array is longer. The class word
///   pins the receiver to the engine's own native `ArrayList`, which never plays by that
///   convention — but a bytecode `java/util/ArrayList` (the `jdk/java/util` route) would, and it
///   would carry the same class name and the same interned word. A process with ANY override
///   refuses the shape outright: one relaxed load at emit time, and the site keeps its callback.
pub(crate) fn alist_size_shape_correct(env: &Env) -> bool {
    env.elems_fid > 0 && !crate::class_loader::any_override()
}

/// Emit-time predicate for CHAINING the shape onto an arm that did not have it. Separate from
/// `alist_size_shape_correct` on purpose: `RAVA_NO_EMIT_INTRIN2` gates only what this round added,
/// so the pre-existing `emit_arraylist_size_inline` keeps meaning exactly what it meant.
pub(crate) fn alist_size_shape_ok(env: &Env) -> bool {
    intrin2_enabled() && alist_size_shape_correct(env)
}

/// ⭐**THE `java/util/ArrayList` SIZE SHAPE, AS A CHAINABLE LINK — 4,439,370 calls a boot.**
///
/// ⛔**Why this had to become a shared shape.** `size()I` reaches TWO emitters and only one arm is
/// emitted per site. `emit_cube_size_inline` is tried first at both the `invokevirtual` and the
/// `invokeinterface` site and, once `cube_ready()` holds, claims every `size()` site in the
/// program — while carrying the CubePointRange, DoubleArrayList and arena-trivial-getter shapes
/// and NOT the `java/util/ArrayList` one. `emit_arraylist_size_inline`, the only emitter that has
/// it, sits below and is unreachable at those sites. So the largest single operation in the whole
/// `fu_fastpath` census kept crossing at exactly the sites the chain had grown to cover: the table
/// of handled cases grew three times and the original entry was left in the other function.
///
/// Contract: TERMINATES the current block. On a class-word match with an `$elems` that really is
/// an array it jumps to `merge` with `[len as i64, extra…]`; every other receiver — a subclass, a
/// fabricated collection with no backing, the calls before the word is published — reaches `next`,
/// which is the caller's unchanged callback arm. `cw` is the receiver's class key, already loaded
/// by the caller's own guard, so the shape costs one compare and one field read on the miss path
/// that was about to pay a full crossing anyway.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_alist_size_shape(
    b: &mut FunctionBuilder,
    env: &Env,
    array_desc_cell: Value,
    recv: Value,
    cw: Value,
    merge: Block,
    extra: &[cranelift::codegen::ir::BlockArg],
    next: Block,
) {
    // The shape cannot be served in this process: hand the receiver to the caller's callback arm,
    // which is what it was going to take anyway. Never `None` — a caller propagating that would
    // fail the whole compile.
    if !alist_size_shape_correct(env) {
        b.ins().jump(next, &[]);
        return;
    }
    bump(b, &LS_CLS);
    let cw_addr = b.ins().iconst(
        I64,
        (&crate::jit::ARRAYLIST_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g = b.ins().band(cw_match, baked_nz);
    let read_blk = b.create_block();
    b.ins().brif(g, read_blk, &[], next, &[]);

    // `$elems` by id, and it must really be an array — a native-fabricated collection that has
    // not been given a backing yet takes the callback, as it already did.
    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    let elems = native_getfield_ref(b, env, array_desc_cell, recv, env.elems_fid - 1);
    let ehi = b.ins().ushr_imm(elems, 32);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, ehi, ARRAY_TAG_HI as i64);
    let have = b.create_block();
    b.ins().brif(is_arr, have, &[], next, &[]);

    b.switch_to_block(have);
    b.seal_block(have);
    let (_data, len) = load_array_desc(b, array_desc_cell, elems);
    let len64 = b.ins().uextend(I64, len);
    bump(b, &LS_FAST);
    let mut args: Vec<cranelift::codegen::ir::BlockArg> = vec![len64.into()];
    args.extend_from_slice(extra);
    b.ins().jump(merge, &args);
}

/// ⭐**`List.size()` WITHOUT A CALL — 4.44 M of them a boot.**
///
/// The `fu_fastpath` census names the collection traffic exactly: `size` 4,439,370 calls a boot,
/// the largest single operation of the 38.4 M that reach the engine's cheapest native path at
/// ~65 ns each. A JVM does not call for this; it reads a field. Neither does this.
///
/// Guarded exactly like `emit_arraylist_get_inline` above, minus the index checks: a heap-Ref
/// receiver, the EXACT published `java/util/ArrayList` class word, and an `$elems` that really is
/// an array. Anything else — a subclass, a fabricated collection with no backing, the calls
/// before the class word is published — takes the same `fu_invoke` callback the site used before.
///
/// ⚠**Why the ARRAY LENGTH is the right answer here, which is not obvious.** `coll_op` computes
/// a collection's length with `elems_len`, not `array_len`, because a guest-bytecode collection
/// keeps spare capacity in `$elems` and its real count in `$size`. Reading `elems_len` shows why
/// that does not apply on this path: with no `java/` class overridden — the default — it returns
/// `array_len` unconditionally, and the `$size` convention exists *only* for a class that ships
/// its own bytecode. The class-word guard pins the receiver to the engine's own native
/// `ArrayList`, which is exactly the population for which the two are the same number.
pub(crate) fn emit_arraylist_size_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
) -> Option<()> {
    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;

    // ⚠**LIVE REFERENCES MUST CROSS THE SLOW ARM.** `emit_arraylist_get_inline` refuses any site
    // with a `JT::Ref` still on the operand stack, because its merge cannot re-materialise one
    // after the callback collects and MOVES objects. That refusal is why the first version of
    // this emitter fired **zero** times on a boot while `size` kept arriving 4.46 M times: real
    // code almost always has a reference on the stack. The cure is the one `emit_dispatch_pic`
    // and `guarded_inline` already use — pass each live ref through the merge block's parameters,
    // and have the fast arm hand its unchanged values through the same ones.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }
    // The live references the shape hands straight through the merge, unchanged.
    let extra: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();

    // Guard 1: a heap-Ref receiver.
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls_blk, &[], slow, &[]);

    // Guards 2 and 3 — the class word and the `$elems` array — are ONE SHAPE now, shared with the
    // `size()` chain in `iterator_inline`, which is where this population actually arrives.
    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    emit_alist_size_shape(b, env, array_desc_cell, recv, cw, merge, &extra, slow);

    // SLOW: the callback this site used before, the oracle for everything the guards refuse.
    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Int);
    stack.push(sv);
    crate::diag::alist_size_inline().inc();
    Some(())
}

/// ⭐**`DoubleArrayList.getDouble(int)` WITHOUT A CALL — 10,528,512 of them a boot.**
///
/// The single largest (class, method) pair in the engine, found only once the `fu_fastpath`
/// population was counted by RECEIVER instead of by method name. It is a primitive read out of a
/// backing `double[]`, served today by a host call at ~65-70 ns; a JVM reads the element.
///
/// One concrete receiver class, so the guard is pinned to what the census actually measured —
/// unlike the `size()` arm, whose name-chosen target arrived on a dozen classes and missed.
///
/// Same guard chain as `emit_arraylist_get_inline`, and the same merge discipline: live


/// Arena receivers that reached the chained arena shape on a `size()` site — the edge both
/// heap-Ref shapes above it send every arena object to.
pub(crate) static MG_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline: one arena cell load, no crossing.
pub(crate) static MG_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn mg_cls() -> u64 {
    MG_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn mg_fast() -> u64 {
    MG_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// The chained `size()` shape's inline-answer round (`mg_fast`). Default on; kill switches
/// `RAVA_NO_MG_FAST=1` and the standing `RAVA_NO_EMIT_MORE` (`emit_more_disabled`). With the
/// round off, the value-computer probe accepts only the shipped `field+1` op, so the emitted
/// code and the callback edge are byte-identical to before this round existed.
pub(crate) fn mg_fast_enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("RAVA_NO_MG_FAST").is_none() && !crate::flags::emit_more_disabled()
    });
    *ON
}



/// Reached the class compare on `Map$Entry.getKey`/`getValue`.
pub(crate) static EG_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Answered inline: one field read, no crossing.
pub(crate) static EG_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn eg_cls() -> u64 {
    EG_CLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn eg_fast() -> u64 {
    EG_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// **`Map$Entry.getKey` / `getValue` — 464,896 crossings a boot**, #5 and #7 of the receiver
/// census. The object is fabricated by this engine and holds exactly two synthetic fields, so
/// the native arm is one `get_field_syn`; the entire cost is REACHING it, which means leaving
/// machine code.
///
/// Same discipline as the `getDouble` arm: guard that the receiver is a heap Ref whose class word
/// matches the published one, read the field by its published id, and send everything else —
/// another entry shape, a null field, an unpublished word — to the callback it already took.
/// A null field must go slow because that is what the native does (`Ok(None)` → general path).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_entry_get_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
    want_key: bool,
) -> Option<()> {
    let recv_sv = stack.pop()?;
    let recv = recv_sv.v;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let read_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    b.ins().brif(is_ref, cls_blk, &[], slow, &[]);

    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    bump(b, &EG_CLS);
    let cw_addr = b.ins().iconst(
        I64,
        (&crate::jit::ENTRY_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g2 = b.ins().band(cw_match, baked_nz);
    b.ins().brif(g2, read_blk, &[], slow, &[]);

    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    let fid_atomic = if want_key {
        &crate::jit::ENTRY_KEY_FID as *const std::sync::atomic::AtomicI64
    } else {
        &crate::jit::ENTRY_VAL_FID as *const std::sync::atomic::AtomicI64
    };
    let fid = unsafe { (*fid_atomic).load(std::sync::atomic::Ordering::Relaxed) } - 1;
    let v = native_getfield_ref(b, env, array_desc_cell, recv, fid);
    // A null/absent field takes the callback, exactly as the native's `Ok(None)` does.
    let nonnull = b.ins().icmp_imm(IntCC::NotEqual, v, 0);
    let ok_blk = b.create_block();
    b.ins().brif(nonnull, ok_blk, &[], slow, &[]);
    b.switch_to_block(ok_blk);
    b.seal_block(ok_blk);
    bump(b, &EG_FAST);
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![v.into()];
    for &i in &ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);

    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    stack.push(SV { v: mparams[0], t: JT::Ref, src: None, arena: false });
    Some(())
}


/// references cross the slow arm and come back through the merge block's parameters, because the
/// callback can collect and move them.
pub(crate) fn emit_dalist_getdouble_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
    kind: i64,
) -> Option<()> {
    let idx_sv = stack.pop()?;
    let recv_sv = stack.pop()?;
    let idx = idx_sv.v;
    let recv = recv_sv.v;

    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let read_blk = b.create_block();
    let bounds_blk = b.create_block();
    let load_blk = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }

    // Guard 1: heap-Ref receiver, non-negative index.
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    let idx_ok = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, idx, 0);
    let g1 = b.ins().band(is_ref, idx_ok);
    b.ins().brif(g1, cls_blk, &[], slow, &[]);

    // Guard 2: the EXACT DoubleArrayList class word.
    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    bump(b, &DG_CLS);
    let cw_addr = b.ins().iconst(
        I64,
        (&crate::jit::DALIST_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g2 = b.ins().band(cw_match, baked_nz);
    b.ins().brif(g2, read_blk, &[], slow, &[]);

    // Guard 3: the backing array field `a` holds an array.
    //
    // ⚠**FOLLOW-UP, AND IT CORRECTS THE COMMIT THAT ADDED THIS.** That commit said the arm still
    // does not fire because the id publishers "sit on paths this workload does not take". THAT IS
    // WRONG, and it was wrong because I read a debug print that was capped at its first few
    // firings and took the early sample for the total. Counted properly instead:
    //
    //   getDouble at the double branch: reached 9,000,001+, ALL classified DArrList, DImm 0
    //   at the same moment: class word published, a_fid=624, size_fid=625, dalist_ready()=TRUE
    //
    // So the ids do publish, on the path that is taken, from the first `getDouble`. The arm is
    // not emitted for a different reason: **with `RAVA_JIT_TIER0` a method compiles on its FIRST
    // ENTRY**, before its own `getDouble` has ever run — so at compile time the ids are still 0.
    // The bootstrap this file relies on ("zero means not emitted, so it bootstraps itself") can
    // never converge for an id that is published by the very call the arm wants to serve. The old
    // gate accidentally dodged that by keying on the ITERATOR's `$elems`, which `iterator_of`
    // publishes early — which is also why the arm was emitted 9.8 M times and hit zero.
    //
    // ⇒ Two ways out, neither taken here: publish `a`/`size` when the CLASS is loaded, before any
    // method that calls into it can compile; or read the field id at RUNTIME out of the atomic,
    // exactly as the class word beside it already is (`native_getfield_ref` takes a baked `i64`,
    // so that means a dynamic-fid variant of a shared field emitter).
    //
    // ⭐**THIS READ `$elems` AND THAT IS WHY THE ROUTE WAS WRITTEN OFF.** `$elems` is the
    // synthetic field the NATIVE ITERATOR carries; a `DoubleArrayList` does not have one, so this
    // test was reached on every call and passed on none (`dg_elem` counted, `dg_fast` zero), and
    // the arm was recorded as "guard never hits, route closed". `fu_double_view` had been reading
    // the real field — `a` — all along, and now publishes its id (see `DALIST_A_FID`).
    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    bump(b, &DG_ELEM);
    let a_fid = crate::jit::DALIST_A_FID.load(std::sync::atomic::Ordering::Relaxed) - 1;
    let elems = native_getfield_ref(b, env, array_desc_cell, recv, a_fid);
    let ehi = b.ins().ushr_imm(elems, 32);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, ehi, ARRAY_TAG_HI as i64);
    b.ins().brif(is_arr, bounds_blk, &[], slow, &[]);

    // Guard 4: in bounds — out of range goes slow so the real method throws.
    //
    // ⚠**Bound by `size`, NOT by the array's length.** A `DoubleArrayList` over-allocates its
    // backing, so the physical array outlives the logical list; checking against the array would
    // serve elements past the end. `fu_double_view` uses `min(size, phys)` and so does this:
    // both are tested, which also covers a `size` field that is somehow larger than its array.
    b.switch_to_block(bounds_blk);
    b.seal_block(bounds_blk);
    bump(b, &DG_BND);
    let (_data, len) = load_array_desc(b, array_desc_cell, elems);
    let size_fid = crate::jit::DALIST_SIZE_FID.load(std::sync::atomic::Ordering::Relaxed) - 1;
    let size = crate::jit::emit::fields_prim_getfield(b, env, array_desc_cell, recv, size_fid);
    let in_size = b.ins().icmp(IntCC::SignedLessThan, idx, size);
    let in_phys = b.ins().icmp(IntCC::UnsignedLessThan, idx, len);
    let inb = b.ins().band(in_size, in_phys);
    b.ins().brif(inb, load_blk, &[], slow, &[]);

    // The read. Bounds proven, so the primitive load skips its own check.
    b.switch_to_block(load_blk);
    b.seal_block(load_blk);
    bump(b, &DG_FAST);
    let val = crate::jit::emit::arrays::native_primitive_load(
        b, env, array_desc_cell, elems, idx, F64, 3, false, true, cp as i64,
    );
    let bits = b.ins().bitcast(I64, MemFlags::new(), val);
    let mut fast_args: Vec<cranelift::codegen::ir::BlockArg> = vec![bits.into()];
    for &i in &ref_slots {
        fast_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &fast_args);

    // SLOW: the callback this site used before — the oracle for a subclass, an absent backing,
    // an out-of-range index, and every call before the class word is published.
    b.switch_to_block(slow);
    b.seal_block(slow);
    bump(b, &DG_SLOW);
    let recv_cell = to_cell(b, recv_sv);
    let idx_cell = to_cell(b, idx_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    b.ins().stack_store(idx_cell, env.args_slot, 8);
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind);
    let nv = b.ins().iconst(I64, 2);
    let call = b.ins().call(env.fu_invoke, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let sv = cell_to_sv(b, mparams[0], JT::Double);
    stack.push(sv);
    crate::diag::dalist_get_inline().inc();
    Some(())
}

pub(crate) fn emit_bitset_get_inline(
    b: &mut FunctionBuilder,
    env: &Env,
    cp: u16,
    vars: &[Variable],
    stack: &mut Vec<SV>,
    locals_ptr: Value,
    array_desc_cell: Value,
) -> Option<()> {
    // Args are [.., recv, idx]; pop deepest-first (idx on top).
    let idx_sv = stack.pop()?;
    let recv_sv = stack.pop()?;
    let idx = idx_sv.v; // I32
    let recv = recv_sv.v; // I64 reference cell

    // ⭐**CARRY LIVE REFERENCES THROUGH THE MERGE — the last arm in this family that could not.**
    //
    // The caller has always restricted this arm to sites with NO `JT::Ref` operand below
    // `recv+idx`, with the recorded justification that the slow arm's locals-only reload is only
    // SSA-sound then. That justification is not wrong, it is INCOMPLETE, and the same wording on
    // `emit_arraylist_get_inline` was measured: the restriction held that arm to **15,688 firings
    // against a population of 1,537,536** — one percent — because real code almost always has a
    // reference on the stack, and `BitSet.get` lives in the voxel `isFull`/`isFullWide` shape
    // loops where the shape reference is exactly that operand.
    //
    // The cure is the one `emit_dispatch_pic`, `guarded_inline` and every other arm in this file
    // now use: one merge-block parameter per live reference, the fast arms passing their unchanged
    // values through the same ones, so a callback that collects and MOVES an object is observed
    // through the merge instead of through a stale SSA value. With this in place the emitter no
    // longer needs the caller's precondition — it is sound at any operand stack.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();

    let cls_blk = b.create_block();
    let read_blk = b.create_block();
    let load_blk = b.create_block();
    let compute = b.create_block();
    let slow = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32);
    for _ in &ref_slots {
        b.append_block_param(merge, I64);
    }
    // Every edge into `merge` carries the same live references, unchanged, except the slow arm's,
    // which carries what the callback gave back.
    let pass: Vec<cranelift::codegen::ir::BlockArg> =
        ref_slots.iter().map(|&i| stack[i].v.into()).collect();

    // Guard 1: `recv` is a REF_TAG heap ref (recv>>40 == 1, REF_TAG=1<<40 uniquely)
    // AND idx >= 0. The REF check gates the box dereference below (a non-ref handle
    // would index the objects table out of bounds).
    let hi = b.ins().ushr_imm(recv, 40);
    let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
    let idx_ok = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, idx, 0);
    let g1 = b.ins().band(is_ref, idx_ok);
    b.ins().brif(g1, cls_blk, &[], slow, &[]);

    // Guard 2: EXACT class java.util.BitSet — read the receiver's class word the same
    // way the machine-code heap-Ref PIC does and compare to the baked-atomic key. A
    // subclass (or a not-yet-populated key = 0) takes the slow arm.
    b.switch_to_block(cls_blk);
    b.seal_block(cls_blk);
    let cw_addr = b.ins().iconst(I64, (&BITSET_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64);
    let baked = b.ins().load(I64, MemFlags::trusted(), cw_addr, Offset32::new(0));
    let cw = emit_heap_ref_class_key(b, array_desc_cell, recv, slow);
    let cw_match = b.ins().icmp(IntCC::Equal, cw, baked);
    let baked_nz = b.ins().icmp_imm(IntCC::NotEqual, baked, 0);
    let g2 = b.ins().band(cw_match, baked_nz);
    b.ins().brif(g2, read_blk, &[], slow, &[]);

    // Read `$bits` (long[]) by id from the heap BitSet — the by-id scan returns the
    // ARRAY_TAG cell (plain path, no callback) when present; a missing / non-array
    // `$bits` (no words yet) is treated as the empty set → false.
    b.switch_to_block(read_blk);
    b.seal_block(read_blk);
    let bits_cell = native_getfield_ref(b, env, array_desc_cell, recv, env.bits_fid);
    let bhi = b.ins().ushr_imm(bits_cell, 32);
    let is_arr = b.ins().icmp_imm(IntCC::Equal, bhi, ARRAY_TAG_HI as i64);
    let z0 = b.ins().iconst(I32, 0);
    let mut empty_args: Vec<cranelift::codegen::ir::BlockArg> = vec![z0.into()];
    empty_args.extend_from_slice(&pass);
    b.ins().brif(is_arr, load_blk, &[], merge, &empty_args);

    // wordIndex bounds check — past the words is `false` (NOT an array fault).
    b.switch_to_block(load_blk);
    b.seal_block(load_blk);
    let (data, len) = load_array_desc(b, array_desc_cell, bits_cell);
    let word_index = b.ins().ushr_imm(idx, 6); // idx>=0 ⇒ logical == arithmetic
    let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, word_index, len);
    let z1 = b.ins().iconst(I32, 0);
    let mut oob_args: Vec<cranelift::codegen::ir::BlockArg> = vec![z1.into()];
    oob_args.extend_from_slice(&pass);
    b.ins().brif(oob, merge, &oob_args, compute, &[]);

    b.switch_to_block(compute);
    b.seal_block(compute);
    let wi64 = b.ins().uextend(I64, word_index);
    let eoff = b.ins().ishl_imm(wi64, 3); // long = 8 bytes
    let addr = b.ins().iadd(data, eoff);
    let word = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let shamt = b.ins().band_imm(idx, 63); // `1L << bitIndex` uses only the low 6 bits
    let shamt64 = b.ins().uextend(I64, shamt);
    let shifted = b.ins().ushr(word, shamt64);
    let bit = b.ins().band_imm(shifted, 1);
    let bit32 = b.ins().ireduce(I32, bit);
    let mut hit_args: Vec<cranelift::codegen::ir::BlockArg> = vec![bit32.into()];
    hit_args.extend_from_slice(&pass);
    b.ins().jump(merge, &hit_args);

    // SLOW arm: the unchanged FFI (exact oracle, incl. idx<0 throw + subclass dispatch).
    b.switch_to_block(slow);
    b.seal_block(slow);
    let recv_cell = to_cell(b, recv_sv);
    let idx_cell = to_cell(b, idx_sv);
    b.ins().stack_store(recv_cell, env.args_slot, 0);
    b.ins().stack_store(idx_cell, env.args_slot, 8);
    // `spill_for_gc` roots the locals AND the live stack operands, so the callback's collector
    // sees every reference; the merge parameters below are what makes a MOVED one observable
    // afterwards, which the locals-only reload cannot do on its own.
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let clsv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, 1); // invokevirtual
    let nv = b.ins().iconst(I64, 2);
    let call = b.ins().call(env.bitset_get, &[clsv, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let res32 = b.ins().ireduce(I32, res);
    let mut slow_args: Vec<cranelift::codegen::ir::BlockArg> = vec![res32.into()];
    for &i in &ref_slots {
        slow_args.push(stack[i].v.into());
    }
    b.ins().jump(merge, &slow_args);

    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    let result = mparams[0];
    stack.push(SV {
        v: result,
        t: JT::Int,
        src: None,
        #[cfg(feature = "next-gen-gc")]
        arena: false,
    });
    Some(())
}

/// Which arm of the `getDouble` diamond actually runs. A guard that never hits is invisible in a
/// site count and obvious here, which is the difference between "31 sites emitted" and "31 sites
/// emitted and none of them fire".
pub(crate) static DG_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static DG_SLOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Reached the CLASS compare, i.e. guard 1 (heap-Ref receiver, non-negative index) passed. Zero
/// here means the arm never even looks at the class word, which is a different bug from a word
/// that does not match - and the two are indistinguishable without this counter.
pub(crate) static DG_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Passed the CLASS compare and is about to read `$elems`.
pub(crate) static DG_ELEM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// `$elems` really was an array; only the bounds test is left.
pub(crate) static DG_BND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The same four-step chain pointed at `emit_arraylist_get_inline` — the arm that has shipped as
/// opt-in for months with NOTHING ever showing that its guard hits. It tests the same tag the
/// `getDouble` arm died on, so the question is not rhetorical.
pub(crate) static AG_CLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static AG_ELEM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static AG_BND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static AG_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Emit a non-atomic increment of a counter whose address is baked in. Loses counts under
/// threading, which is fine for an order-of-magnitude question.
fn bump(b: &mut FunctionBuilder, c: &'static std::sync::atomic::AtomicU64) {
    let addr = b.ins().iconst(I64, (c as *const _) as i64);
    let cur = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let inc = b.ins().iadd_imm(cur, 1);
    b.ins().store(MemFlags::trusted(), inc, addr, Offset32::new(0));
}
pub(crate) fn AG_CLS_READ() -> u64 { AG_CLS.load(std::sync::atomic::Ordering::Relaxed) }
pub(crate) fn AG_ELEM_READ() -> u64 { AG_ELEM.load(std::sync::atomic::Ordering::Relaxed) }
pub(crate) fn AG_BND_READ() -> u64 { AG_BND.load(std::sync::atomic::Ordering::Relaxed) }
pub(crate) fn AG_FAST_READ() -> u64 { AG_FAST.load(std::sync::atomic::Ordering::Relaxed) }

/// ⭐**DOES THE ARENA+INLINE COMBINATION ACTUALLY RUN?** The map half of the collections
/// migration is complete and measures at PARITY, and the recorded expectation was that the win
/// arrives only with the next step — emitting the probe loop straight into IR against arena
/// memory. That step is this function, it has existed for some time, and nothing has ever shown
/// whether it fires.
///
/// The whole "remove the handle heap" verdict rests on the answer: if the probe runs on real
/// traffic and the boot is still at parity, then arena representation PLUS inlining is proven not
/// to pay, and the list half need not be built at all.
pub(crate) static AM_SITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static AM_ARENA: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static AM_FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn am_arena() -> u64 {
    AM_ARENA.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn am_fast() -> u64 {
    AM_FAST.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn am_sites() -> u64 {
    AM_SITES.load(std::sync::atomic::Ordering::Relaxed)
}
