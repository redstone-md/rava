//! Array element access and length, with the bounds check emitted in line.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::{ABOX_TAG, ARRAY_TAG, BOX_TAG};
use crate::jit::*;

/// ⭐**RAVA_NO_EMIT_ARRAY=1 emits the pre-round array access verbatim.**
///
/// What this round adds to every array op is the SHAPE GATE that `native_primitive_load`
/// already carried alone: a compare of the operand cell's high word against `ARRAY_TAG_HI`
/// BEFORE its low 32 bits are used as an index into the descriptor spine. Read at EMIT time
/// only — with the switch set, `build_body` lays down byte-identical machine code, so the
/// flag bisects a regression without changing register allocation or block counts.
///
/// Also consults `emit_more_disabled()` so this falls together with the rest of the
/// "emit more, cross less" block when that block is priced as a whole.
pub(crate) fn emit_array() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_EMIT_ARRAY").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The SHAPE GATE, one compare: is `arr` a real `ARRAY_TAG` cell?
///
/// ⚠**THIS IS THE TEST WHOSE ABSENCE COST THIS TREE THE DATAPACK-CODEC CORRUPTION.** Every
/// array op reaches its element buffer through `array_descs[arr & 0xFFFF_FFFF]`. That mask is
/// only meaningful for an `ARRAY_TAG` cell; fed a boxed value, a `REF_TAG` handle, an
/// `ABOX_TAG` cell or a raw arena pointer it indexes a WRONG (or out-of-spine) descriptor and
/// the op then reads — or, for a store, WRITES — through a data pointer that belongs to a
/// different object. `native_primitive_load` has gated on this since the corruption was found;
/// `arraylength`, `aaload`, `aastore` and the primitive store never did, and they tested only
/// `arr == 0`.
///
/// The gate REPLACES that null test rather than joining it: null's high word is 0 and
/// `ARRAY_TAG_HI` is not, so null already fails this compare and lands on the same cold arm.
/// Net branch count per op is therefore UNCHANGED — one compare in, one compare out — while
/// every non-null non-array encoding stops reading a foreign descriptor. Removing it restores
/// a wild read on `arraylength`/`aaload` and a wild WRITE on `aastore`/`*astore`.
fn emit_is_array_cell(b: &mut FunctionBuilder, arr: Value) -> Value {
    let hi = b.ins().ushr_imm(arr, 32);
    let want = b.ins().iconst(I64, (ARRAY_TAG as u64 >> 32) as i64);
    b.ins().icmp(IntCC::Equal, hi, want)
}

/// The ELEMENT-LAYOUT GATE, one compare in the common case: is this descriptor's
/// `element_id` one of the 8 primitive element names?
///
/// `Heap::empty_array` picks the backing representation from the element NAME alone
/// (`byte|boolean`→`u8`, `char|short|int`→`i32`, `long`→`i64`, `float`→`f32`, `double`→`f64`,
/// anything else→`ArrayData::Ref`, a vector of 8-byte tagged cells). So "element_id is a
/// primitive id" is an exact proof of "the buffer is the flat packed vector this op's
/// `shift` addresses", which is what a raw indexed load/store needs and nothing weaker
/// supplies.
///
/// The 8 ids are interned back-to-back by `Heap::ensure_prim_element_ids` at VM construction,
/// so membership is ONE unsigned range check instead of 8 compares + 7 ors + 8 constants.
/// Contiguity is verified here at COMPILE time rather than assumed — if anything ever interns
/// an array element name before the VM does, the general chain is emitted and the gate stays
/// correct, only slower.
fn emit_prim_element_test(b: &mut FunctionBuilder, env: &Env, elem_id: Value) -> Value {
    let mut sorted = env.prim_element_ids;
    sorted.sort_unstable();
    let contiguous = sorted[0] >= 0
        && sorted.windows(2).all(|w| w[1] == w[0] + 1)
        && sorted[7] < i64::from(u32::MAX);
    if contiguous {
        // (elem_id - lo) <u 8  ⇒ elem_id ∈ [lo, lo+7]. The wrapping subtract sends
        // every id below `lo` to a huge unsigned value, so one compare covers both ends.
        let lo = b.ins().iconst(I32, sorted[0]);
        let rel = b.ins().isub(elem_id, lo);
        b.ins().icmp_imm(IntCC::UnsignedLessThan, rel, 8)
    } else {
        let mut eqs = [b.ins().iconst(I64, 0); 8];
        for (i, id) in env.prim_element_ids.iter().enumerate() {
            let c = b.ins().iconst(I32, *id);
            eqs[i] = b.ins().icmp(IntCC::Equal, elem_id, c);
        }
        let a01 = b.ins().bor(eqs[0], eqs[1]);
        let a23 = b.ins().bor(eqs[2], eqs[3]);
        let a45 = b.ins().bor(eqs[4], eqs[5]);
        let a67 = b.ins().bor(eqs[6], eqs[7]);
        let a03 = b.ins().bor(a01, a23);
        let a47 = b.ins().bor(a45, a67);
        b.ins().bor(a03, a47)
    }
}

/// Native `arraylength`: an `ARRAY_TAG` operand yields the descriptor's `len` with no
/// callback; anything else (null included) faults with `NullPointerException` and yields 0.
///
/// GUARD: `emit_is_array_cell` — the low 32 bits of the operand really are a descriptor index.
/// COLD ARM: `rava_array_fault`, which raises NPE("array") for exactly the cells this gate
/// rejects (`untag_array` is `None` for all of them) — the SAME exception and the SAME message
/// the removed `arr == 0` branch produced, so no guest-observable behaviour moves.
///
/// ⚠**`env.arrlen` (`rava_arraylength`) is deliberately NOT the cold arm** even though it is
/// imported and otherwise unused: it decodes no encoding this gate rejects (it calls the same
/// `untag_array`), so it would buy nothing, and its NPE message is "arraylength" where this
/// site has always raised "array". Message drift is guest-visible through `getMessage()`.
///
/// COST IF REMOVED: a `REF_TAG`/`ABOX`/boxed/arena cell indexes a foreign `ArrayDesc` and
/// `arraylength` returns another object's length as a plausible-looking int.
pub(crate) fn native_arraylength(b: &mut FunctionBuilder, env: &Env, desc_cell: Value, arr: Value) -> Value {
    let fault = b.create_block();
    let ok = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I32);
    let zero = b.ins().iconst(I64, 0);
    if emit_array() {
        let is_arr = emit_is_array_cell(b, arr);
        b.ins().brif(is_arr, ok, &[], fault, &[]);
    } else {
        let isnull = b.ins().icmp(IntCC::Equal, arr, zero);
        b.ins().brif(isnull, fault, &[], ok, &[]);
    }

    b.switch_to_block(ok);
    b.seal_block(ok);
    let (_data, len) = load_array_desc(b, desc_cell, arr);
    b.ins().jump(merge, &[len.into()]);

    b.switch_to_block(fault);
    b.seal_block(fault);
    b.ins().call(env.array_fault, &[arr, zero]);
    let z = b.ins().iconst(I32, 0);
    b.ins().jump(merge, &[z.into()]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Native aaload (reference array). The element is a flat tagged ref cell:
/// REF_TAG/ARRAY_TAG/0 cells are valid JIT operand cells and are returned as-is;
/// an ABOX_TAG cell (Str/boxed in the heap array_box) routes to the read callback
/// to box it into the transient table. OOB faults to `rava_array_fault`.
///
/// GUARD (this round): `emit_is_array_cell` on the receiver, REPLACING the `arr == 0` test.
/// COLD ARM: the ABOX block that already exists — `rava_aload(arr, idx, is_ref=1)`, whose
/// `untag_array` rejects every cell this gate rejects and raises NPE("array"): bit-identical
/// to the `rava_array_fault` NPE the deleted null branch produced. So the null case keeps its
/// exception AND its message while costing one fewer block; the branch budget is unchanged
/// (one compare in, one compare out) and a `REF_TAG`/boxed/arena cell stops reading a foreign
/// descriptor and handing the result back as a REFERENCE the caller will dereference.
///
/// ⚠`skip_bounds` remains purely the loop-BCE / caller-proven-index contract; it does NOT
/// imply the shape is proven, because `ops::arrays` feeds it from `env.bce_safe`, which
/// reasons about the INDEX only. The gate is therefore emitted unconditionally.
pub(crate) fn native_aaload(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    arr: Value,
    idx: Value,
    skip_bounds: bool,
) -> Value {
    let gated = emit_array();
    // The AIOOBE arm exists only where something can still branch to it: with the shape gate on
    // AND the index already proven by the loop-BCE analysis, nothing can, and emitting a block
    // whose instructions use values no longer dominating it is not merely dead — it is invalid
    // IR. So it is created only when reachable.
    let fault_reachable = !gated || !skip_bounds;
    let check = b.create_block();
    let ok = b.create_block();
    let plain = b.create_block();
    let abox = b.create_block();
    let fault = fault_reachable.then(|| b.create_block());
    let merge = b.create_block();
    b.append_block_param(merge, I64);

    let zero = b.ins().iconst(I64, 0);
    if gated {
        // Shape gate. The false edge is the ABOX callback block, not a separate fault block:
        // `rava_aload` raises the identical NPE("array") for a null / non-array cell.
        let is_arr = emit_is_array_cell(b, arr);
        b.ins().brif(is_arr, check, &[], abox, &[]);
    } else {
        let isnull = b.ins().icmp(IntCC::Equal, arr, zero);
        b.ins().brif(isnull, fault.unwrap(), &[], check, &[]);
    }

    b.switch_to_block(check);
    b.seal_block(check);
    let (data, len) = load_array_desc(b, ctx_cell, arr);
    if skip_bounds {
        // V4.1 generic loop-BCE: `0 <= idx < len` proven by the counted-loop analysis
        // (safe_bce_loads) → skip the bounds branch. `len` DCE's out.
        let _ = len;
        b.ins().jump(ok, &[]);
    } else {
        let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        b.ins().brif(oob, fault.unwrap(), &[], ok, &[]);
    }

    b.switch_to_block(ok);
    b.seal_block(ok);
    let idx64 = b.ins().uextend(I64, idx);
    let eoff = b.ins().ishl_imm(idx64, 3); // i64 cells
    let addr = b.ins().iadd(data, eoff);
    let cell = b.ins().load(I64, MemFlags::trusted(), addr, Offset32::new(0));
    let hi = b.ins().ushr_imm(cell, 32);
    let abox_hi = b.ins().iconst(I64, (ABOX_TAG as u64 >> 32) as i64);
    let is_abox = b.ins().icmp(IntCC::Equal, hi, abox_hi);
    b.ins().brif(is_abox, abox, &[], plain, &[]);

    b.switch_to_block(plain);
    b.seal_block(plain);
    b.ins().jump(merge, &[cell.into()]);

    b.switch_to_block(abox);
    b.seal_block(abox);
    let idx_s = b.ins().sextend(I64, idx);
    let is_ref = b.ins().iconst(I64, 1);
    let call = b.ins().call(env.aload, &[arr, idx_s, is_ref]);
    let boxed = b.inst_results(call)[0];
    b.ins().jump(merge, &[boxed.into()]);

    if let Some(fault) = fault {
        b.switch_to_block(fault);
        b.seal_block(fault);
        let idxf = b.ins().sextend(I64, idx);
        b.ins().call(env.array_fault, &[arr, idxf]);
        b.ins().jump(merge, &[zero.into()]);
    }

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Native aastore. Bare handle/null elements (REF_TAG/ARRAY_TAG/0) are stored
/// directly into the flat ref-cell buffer; a transient BOX_TAG value (Str/boxed)
/// or a rejected array shape / OOB index routes to rava_aastore (which persists into
/// array_box / faults). No-ops after a pending fault.
///
/// GUARD (this round): `emit_is_array_cell` on the receiver, REPLACING the `arr == 0` test —
/// same block, same edge, same branch count. COLD ARM: the `rava_aastore` block that already
/// existed, whose `untag_array` rejects exactly what the gate rejects and raises the identical
/// NPE("aastore"). COST IF REMOVED: this is the WILD WRITE — a boxed/`REF_TAG`/arena cell
/// arriving here indexed a foreign `ArrayDesc` and stored 8 bytes through another object's data
/// pointer, which the collector then follows.
///
/// The inline store proves array layout, but still does not prove runtime
/// component-type compatibility. The fallback callback now performs the shared
/// `astore_reject` check; the direct store needs a separate type proof or a
/// checked fallback before it can claim full covariance semantics.
pub(crate) fn native_aastore(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    arr: Value,
    idx: Value,
    val: Value,
) {
    let proceed = b.create_block();
    let check_null = b.create_block();
    let check_bounds = b.create_block();
    let store_ok = b.create_block();
    let cb = b.create_block();
    let done = b.create_block();

    let ff = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
    let zero32 = b.ins().iconst(I32, 0);
    let faulted = b.ins().icmp(IntCC::NotEqual, ff, zero32);
    b.ins().brif(faulted, done, &[], proceed, &[]);

    b.switch_to_block(proceed);
    b.seal_block(proceed);
    let hi = b.ins().ushr_imm(val, 32);
    let box_hi = b.ins().iconst(I64, (BOX_TAG as u64 >> 32) as i64);
    let is_box = b.ins().icmp(IntCC::Equal, hi, box_hi);
    b.ins().brif(is_box, cb, &[], check_null, &[]);

    b.switch_to_block(check_null);
    b.seal_block(check_null);
    if emit_array() {
        let is_arr = emit_is_array_cell(b, arr);
        b.ins().brif(is_arr, check_bounds, &[], cb, &[]);
    } else {
        let zero = b.ins().iconst(I64, 0);
        let isnull = b.ins().icmp(IntCC::Equal, arr, zero);
        b.ins().brif(isnull, cb, &[], check_bounds, &[]);
    }

    b.switch_to_block(check_bounds);
    b.seal_block(check_bounds);
    let (data, len) = load_array_desc(b, ctx_cell, arr);
    let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
    b.ins().brif(oob, cb, &[], store_ok, &[]);

    b.switch_to_block(store_ok);
    b.seal_block(store_ok);
    // NOTE: a COW arm was tried here (route armed ref-stores to rava_aastore) — MEASURED
    // array_skip=0, i.e. the S2 compiled compute does NO ref-array store, so aastore is NOT
    // the shared-memo write. Reverted. The remaining suspect is a PRIMITIVE array store
    // (dastore/lastore into a double[]/long[] memo) — but even that contradicts (cow-on does
    // not isolate array-index writes yet is bit-exact), so the mechanism is deeper.
    let idx64 = b.ins().uextend(I64, idx);
    let eoff = b.ins().ishl_imm(idx64, 3);
    let addr = b.ins().iadd(data, eoff);
    b.ins().store(MemFlags::trusted(), val, addr, Offset32::new(0));
    b.ins().jump(done, &[]);

    b.switch_to_block(cb);
    b.seal_block(cb);
    let idx_s = b.ins().sextend(I64, idx);
    b.ins().call(env.aastore, &[arr, idx_s, val]);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}

/// Native primitive array element load. `load_ty` is the cranelift element type
/// (I8 for byte, I32 for char/short/int, I64 long, F32 float, F64 double); `shift`
/// is log2(element bytes); `signext_byte` sign-extends an I8 result to I32 (baload).
/// Inline null + unsigned bounds check; faults (NPE/AIOOBE) branch to the cold
/// callback and yield 0. Returns the result value (I32/I64/F32/F64).
pub(crate) fn native_primitive_load(
    b: &mut FunctionBuilder,
    env: &Env,
    desc_cell: Value,
    arr: Value,
    idx: Value,
    load_ty: Type,
    shift: i64,
    signext_byte: bool,
    skip_bounds: bool,
    site: i64,
) -> Value {
    let res_ty = if signext_byte { I32 } else { load_ty };
    let fault = b.create_block();
    let cb = b.create_block();
    let check = b.create_block();
    let bnds = b.create_block();
    let ok = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, res_ty);

    // THE DIVERGENCE THAT KEPT THIS PATH DISABLED: the desc-table index is
    // `arr & 0xFFFF_FFFF`, so a NON-ARRAY_TAG cell (boxed value, ref-tagged
    // handle, arena pointer) silently indexed a WRONG descriptor and read
    // valid-looking garbage (the datapack-codec corruption; RegionRead5 found
    // 253/529 chunks). Only a real ARRAY_TAG cell may take the native load —
    // anything else routes to the callback, which decodes every encoding.
    //
    // NULL NEEDS NO BRANCH OF ITS OWN. A separate `arr == 0 → fault` test used to
    // precede this one, and it was pure duplication: null's high word is 0 and
    // `ARRAY_TAG`'s is not, so null already fails the tag test and lands in the
    // callback — where `rava_aload`'s `untag_array` raises the SAME
    // `NullPointerException` through `set_pending`, which also latches
    // `heap.set_jit_fault()` exactly as `rava_array_fault` does. Deleting the test
    // removes ONE BRANCH AND ONE BLOCK PER ARRAY READ, and the branches are what
    // split blocks: `ImprovedNoise.sampleAndLerp` performs 92 reads inside 991
    // blocks, and it is the block count, not the compare count, that wrecks
    // register allocation there (ledger row 29).
    let hi = b.ins().ushr_imm(arr, 32);
    let want = b.ins().iconst(I64, (ARRAY_TAG as u64 >> 32) as i64);
    let is_arr = b.ins().icmp(IntCC::Equal, hi, want);
    b.ins().brif(is_arr, check, &[], cb, &[]);

    b.switch_to_block(cb);
    let idx_s = b.ins().sextend(I64, idx);
    let is_ref0 = b.ins().iconst(I64, 0);
    let call = b.ins().call(env.aload, &[arr, idx_s, is_ref0]);
    let packed = b.inst_results(call)[0];
    let cbv = if res_ty == I32 {
        b.ins().ireduce(I32, packed)
    } else if res_ty == F32 {
        let bits = b.ins().ireduce(I32, packed);
        b.ins().bitcast(F32, MemFlags::new(), bits)
    } else if res_ty == F64 {
        b.ins().bitcast(F64, MemFlags::new(), packed)
    } else {
        packed
    };
    b.ins().jump(merge, &[cbv.into()]);

    b.switch_to_block(check);
    b.seal_block(check);
    // One descriptor address for all three reads (data, len, element_id). The gate
    // below used to recompute it from scratch.
    let desc = array_desc_addr(b, desc_cell, arr);
    let data = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
    // `len` and `element_id` are IMMUTABLE for the life of an array handle — only
    // `data` moves (evacuation). Marking them readonly lets Cranelift redundant-load-
    // eliminate them across the 92 loads a body like `ImprovedNoise.sampleAndLerp`
    // performs on ONE array; `data` deliberately stays a plain trusted load so a
    // collection between two accesses always re-reads the moved pointer.
    //
    // Soundness of the CSE: the readonly load hangs off `desc`, which is derived from
    // a NON-readonly load of the descriptor-table base. If anything could have moved
    // the table, that base load is re-emitted and yields a different SSA value, so the
    // readonly loads cannot be forwarded across it either.
    let ro = MemFlags::trusted().with_readonly();
    let len = b.ins().load(I32, ro, desc, Offset32::new(8));
    // ELEMENT-TYPE GATE (the Vec3i.lambda$static$0 boot crash): some arrays a
    // primitive load reaches are REF-layout (`ArrayData::Ref` cells, element_id
    // NOT one of the 8 primitives — e.g. an int[] materialized as Object[] with
    // ABOX-boxed cells). A raw load reads boxed-cell low words (0x3612 vs the
    // decoded 1) → garbage coordinates → "Position out of range". The gate
    // routes non-primitive-element arrays to the callback, which decodes every
    // encoding; genuine primitive arrays keep the native flat load.
    let elem_id = b.ins().load(I32, ro, desc, Offset32::new(12));
    // Membership in the 8 primitive element ids — see `emit_prim_element_test`, which the
    // primitive STORE now shares so the two directions of the same access cannot drift.
    let any_prim = emit_prim_element_test(b, env, elem_id);
    b.ins().brif(any_prim, bnds, &[], cb, &[]);

    b.switch_to_block(bnds);
    b.seal_block(bnds);
    if skip_bounds {
        // V4.1 generic loop-BCE: `0 <= idx < len` proven by the counted-loop analysis
        // (safe_bce_loads) → skip the trapping bounds branch. `len` DCE's out.
        let _ = len;
        b.ins().jump(ok, &[]);
    } else {
        // Unsigned compare folds the idx<0 case (negative → huge unsigned ≥ len).
        let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        b.ins().brif(oob, fault, &[], ok, &[]);
    }
    // The element-gate's `cb` predecessor (in `check`) is now in place.
    b.seal_block(cb);

    b.switch_to_block(ok);
    b.seal_block(ok);
    let idx64 = b.ins().uextend(I64, idx);
    let sh = b.ins().iconst(I64, shift);
    let eoff = b.ins().ishl(idx64, sh);
    let addr = b.ins().iadd(data, eoff);
    let raw = b.ins().load(load_ty, MemFlags::trusted(), addr, Offset32::new(0));
    let elem = if signext_byte { b.ins().sextend(I32, raw) } else { raw };
    // DIAGNOSTIC (RAVA_VERIFY_LOADS, compile-time gate): cross-check every native
    // load against the interpreter and name each divergence ([LOADDIV]).
    {
        static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *VERIFY.get_or_init(|| std::env::var_os("RAVA_VERIFY_LOADS").is_some()) {
            let jt = if res_ty == I64 {
                JT::Long
            } else if res_ty == F32 {
                JT::Float
            } else if res_ty == F64 {
                JT::Double
            } else {
                JT::Int
            };
            let packed = to_cell(
                b,
                SV {
                    v: elem,
                    t: jt,
                    src: None,
                    #[cfg(feature = "next-gen-gc")]
                    arena: false,
                },
            );
            let idx_sv = b.ins().sextend(I64, idx);
            let site_c = b.ins().iconst(I64, site);
            b.ins().call(env.aload_verify, &[arr, idx_sv, packed, site_c]);
        }
    }
    b.ins().jump(merge, &[elem.into()]);

    b.switch_to_block(fault);
    b.seal_block(fault);
    let idx64f = b.ins().sextend(I64, idx);
    b.ins().call(env.array_fault, &[arr, idx64f]);
    // A latched Java exception must complete the whole compiled method
    // abruptly. Returning directly prevents an inlined consumer/caller from
    // executing any bytecode after the fault; jit_run observes jit_pending and
    // discards these dummy result bits.
    let z = b.ins().iconst(I64, 0);
    b.ins().return_(&[z]);

    b.switch_to_block(merge);
    b.seal_block(merge);
    b.block_params(merge)[0]
}

/// Native primitive array element store. `val` is the already-typed value to
/// store (I8 byte, I32 char/short/int, I64 long, F32 float, F64 double); `shift`
/// is log2(element bytes). Short-circuits if a prior fault is latched
/// (ctx.fault@8), else inline shape + unsigned bounds check then a native indexed
/// store; on fault calls the cold callback (NPE/AIOOBE) and writes nothing.
///
/// ⭐**THIS IS THE MIRROR OF THE GATE THAT `native_primitive_load` HAS CARRIED SINCE THE
/// DATAPACK-CODEC CORRUPTION, AND THE STORE NEVER GOT IT.** The read side proves two things
/// before it touches element memory — the operand is an `ARRAY_TAG` cell, and the descriptor's
/// `element_id` is one of the 8 primitive names so the buffer really is the flat packed vector
/// `shift` addresses. The write side proved neither and tested only `arr == 0`, so:
///   * a `REF_TAG`/`ABOX`/boxed/arena cell indexed a foreign `ArrayDesc` and stored through
///     ANOTHER OBJECT'S data pointer — the read-side version of this bug only returned garbage;
///   * an array whose element name is not primitive is `ArrayData::Ref`, a vector of 8-byte
///     TAGGED CELLS, and a `shift`-addressed 4-byte store there half-overwrites a live cell,
///     manufacturing a pointer the collector then follows. This is exactly the population the
///     read side's element gate was added to catch (`Vec3i.lambda$static$0`).
///
/// GUARDS: `emit_is_array_cell` (replaces the null test — same branch count) and, where a
/// correct cold arm exists, `emit_prim_element_test`.
/// COLD ARMS: shape → `rava_array_fault`, which raises NPE("array") for precisely the rejected
/// cells — the identical exception and message the deleted null branch produced. Element →
/// `rava_iastore` for an I8/I32 element slot and `rava_dastore` for F64, which perform the store
/// correctly through `Heap::array_set` whatever the backing representation is.
///
/// ⛔**`lastore` AND `fastore` KEEP THE SHAPE GATE BUT GET NO ELEMENT GATE, AND THAT IS A
/// DELIBERATE REFUSAL.** `Env` imports exactly two primitive-store callbacks; there is no
/// `rava_lastore`/`rava_fastore` FuncRef to branch to, and a gate whose cold arm is wrong is
/// worse than no gate — routing a legal `long[]` store to `rava_array_fault` would raise a
/// bogus `NullPointerException`. Widening those two is a one-line `Env`/`imports` addition
/// (`rava_lastore` already exists and is already a linked symbol) and belongs to whoever owns
/// those files.
///
/// ⚠The element cold arm is reached BEFORE the bounds branch, mirroring the load. An
/// out-of-range index into a non-primitive-layout array therefore reports AIOOBE with the
/// callback's message rather than `rava_array_fault`'s; both pathologies at once, and the
/// exception TYPE is unchanged.
pub(crate) fn native_primitive_store(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    arr: Value,
    idx: Value,
    val: Value,
    shift: i64,
) {
    let gated = emit_array();
    // The element-layout gate is emitted only for the element kinds `Env` can serve correctly
    // if it fails. `val`'s cranelift type IS the element kind: the caller has already narrowed
    // bastore to I8 and left the others at their storage width.
    let elem_cb = if gated {
        match b.func.dfg.value_type(val) {
            // bastore (I8) and iastore/castore/sastore (I32) → `rava_iastore`, which stores a
            // `Value::Int`. The byte case sign-extends the already-narrowed value, reproducing
            // exactly the int the interpreter's `bastore` would have stored.
            t if t == I8 || t == I32 => Some((env.iastore, false)),
            // dastore → `rava_dastore`, which takes the double's BITS.
            t if t == F64 => Some((env.dastore, true)),
            // lastore / fastore: no callback — see the refusal note above.
            _ => None,
        }
    } else {
        None
    };

    let proceed = b.create_block();
    let check = b.create_block();
    // Only the gated shape splits the descriptor read from the bounds branch; with the switch
    // set, `check` still performs the bounds test itself so the emitted block graph — and hence
    // the register allocation and the machine code — is the pre-round one exactly.
    let bnds = gated.then(|| b.create_block());
    let store_ok = b.create_block();
    let cb = elem_cb.map(|_| b.create_block());
    let faultblk = b.create_block();
    let done = b.create_block();

    // Skip the whole store if a fault is already pending in this frame.
    let ff = b.ins().load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(8));
    let zero32 = b.ins().iconst(I32, 0);
    let already = b.ins().icmp(IntCC::NotEqual, ff, zero32);
    b.ins().brif(already, done, &[], proceed, &[]);

    b.switch_to_block(proceed);
    b.seal_block(proceed);
    if gated {
        let is_arr = emit_is_array_cell(b, arr);
        b.ins().brif(is_arr, check, &[], faultblk, &[]);
    } else {
        let zero = b.ins().iconst(I64, 0);
        let isnull = b.ins().icmp(IntCC::Equal, arr, zero);
        b.ins().brif(isnull, faultblk, &[], check, &[]);
    }

    b.switch_to_block(check);
    b.seal_block(check);
    let data;
    let len;
    if gated {
        // ONE descriptor address for all three reads, as on the read side. `len` and
        // `element_id` are IMMUTABLE for the life of an array handle so they are `readonly`
        // and the egraph CSEs them across repeated accesses to the same array; `data`
        // deliberately stays a plain trusted load so a collection between two accesses
        // re-reads the moved pointer.
        let desc = array_desc_addr(b, ctx_cell, arr);
        let ro = MemFlags::trusted().with_readonly();
        data = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
        len = b.ins().load(I32, ro, desc, Offset32::new(8));
        let bnds = bnds.expect("gated store always splits the bounds branch");
        match cb {
            Some(cb) => {
                let elem_id = b.ins().load(I32, ro, desc, Offset32::new(12));
                let any_prim = emit_prim_element_test(b, env, elem_id);
                b.ins().brif(any_prim, bnds, &[], cb, &[]);
            }
            // lastore/fastore: no correct cold arm, so no gate — fall straight through.
            None => {
                b.ins().jump(bnds, &[]);
            }
        }
    } else {
        let (d, l) = load_array_desc(b, ctx_cell, arr);
        data = d;
        len = l;
        let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        b.ins().brif(oob, faultblk, &[], store_ok, &[]);
    }

    if let Some(bnds) = bnds {
        b.switch_to_block(bnds);
        b.seal_block(bnds);
        // Unsigned compare folds the idx<0 case (negative → huge unsigned ≥ len), so a signed
        // negative index raises AIOOBE through the same branch, before any side effect.
        let oob = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        b.ins().brif(oob, faultblk, &[], store_ok, &[]);
    }

    b.switch_to_block(store_ok);
    b.seal_block(store_ok);
    // NOTE: an inline primitive-store counter here MEASURED prim_stores=0 under bypass+S2+4W —
    // the compiled compute performs NO primitive array store. Combined with field/ref-array/
    // putstatic/id-hash all 0, the compute writes NOTHING to guest state (read-only). Reverted.
    let idx64 = b.ins().uextend(I64, idx);
    let sh = b.ins().iconst(I64, shift);
    let eoff = b.ins().ishl(idx64, sh);
    let addr = b.ins().iadd(data, eoff);
    b.ins().store(MemFlags::trusted(), val, addr, Offset32::new(0));
    b.ins().jump(done, &[]);

    // ELEMENT COLD ARM: the array is real but its backing is not the flat primitive vector
    // this opcode addresses. Hand the store to the callback, which goes through
    // `Heap::array_set` and therefore writes the correct representation (including boxing into
    // `array_box` for a `ArrayData::Ref` backing) and raises the correct exceptions.
    if let (Some(cb), Some((callee, is_bits))) = (cb, elem_cb) {
        b.switch_to_block(cb);
        b.seal_block(cb);
        let idx_s = b.ins().sextend(I64, idx);
        let wide = if is_bits {
            b.ins().bitcast(I64, MemFlags::new(), val)
        } else {
            let t = b.func.dfg.value_type(val);
            if t == I64 { val } else { b.ins().sextend(I64, val) }
        };
        b.ins().call(callee, &[arr, idx_s, wide]);
        b.ins().jump(done, &[]);
    }

    b.switch_to_block(faultblk);
    b.seal_block(faultblk);
    let idx64f = b.ins().sextend(I64, idx);
    b.ins().call(env.array_fault, &[arr, idx64f]);
    b.ins().jump(done, &[]);

    b.switch_to_block(done);
    b.seal_block(done);
}
