//! Lambda and heap-reference key selection feeding the PIC probe.
//!
//! Re-exported from the parent module, so no call site outside it changed.


use crate::jit::*;

/// Load the interned class key from a heap-reference slot. Nullable tombstones
/// retain their handle index but have no `Object`; route them to the caller's
/// existing semantic fallback before attempting the class-word load.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_heap_ref_class_key(
    b: &mut FunctionBuilder,
    array_desc_cell: Value,
    recv: Value,
    null_target: Block,
) -> Value {
    let class_load = b.create_block();
    let handle = b.ins().band_imm(recv, crate::value::REF_TAG - 1);
    // R2 (Flat-Floor): objects base is cached in JitCtx+48 and republished
    // after every table relocation. ObjectSlot is pointer-sized; a swept
    // nullable slot contains a null pointer and must take the caller's
    // existing slow/miss arm instead of dereferencing address zero.
    let base = b.ins().load(I64, MemFlags::trusted(), array_desc_cell, Offset32::new(48));
    let hoff = b.ins().imul_imm(
        handle,
        std::mem::size_of::<crate::heap::ObjectSlot>() as i64,
    );
    let slot = b.ins().iadd(base, hoff);
    let box_ptr = b.ins().load(I64, MemFlags::trusted(), slot, Offset32::new(0));
    let live = b.ins().icmp_imm(IntCC::NotEqual, box_ptr, 0);
    b.ins().brif(live, class_load, &[], null_target, &[]);

    b.switch_to_block(class_load);
    b.seal_block(class_load);
    let class_offset = crate::heap::Heap::object_class_offset() as i32;
    b.ins().load(
        I64,
        MemFlags::trusted(),
        box_ptr,
        Offset32::new(class_offset),
    )
}

/// λ-PIC key select (emitted only under `lambda_pic_enabled`): remap a "$Lambda"
/// class word to the receiver's IDENTITY key so directable lambda singletons probe
/// the site cache per-receiver instead of sharing one (never-cacheable) class key.
///
///   key' = select(key == LAMBDA_CLASS_WORD, (recv << 1) | 1, key)
///
/// • `LAMBDA_CLASS_WORD` is loaded from its static address per dispatch (never
///   baked): 0 before the first fill ⇒ the equality can't fire (a live class word
///   is a non-null pointer) ⇒ pre-populate behaviour is exactly the class-word probe.
/// • `(recv << 1) | 1`: class words are 8-aligned (low bits 0) so an ODD key can
///   never collide with one; the SHIFT keeps two distinct handles from colliding
///   (`recv | 1` would alias handles 2k and 2k+1). recv carries REF_TAG (bit 40),
///   so the shifted value stays far below any sign issue. The arena arm probes a
///   separate branch, so small arena class ids are out of scope entirely.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_lambda_key_select(b: &mut FunctionBuilder, recv: Value, key: Value) -> Value {
    let wa = b.ins().iconst(
        I64,
        (&LAMBDA_CLASS_WORD as *const std::sync::atomic::AtomicI64) as i64,
    );
    let lam_word = b.ins().load(I64, MemFlags::trusted(), wa, Offset32::new(0));
    let is_lam = b.ins().icmp(IntCC::Equal, key, lam_word);
    let id = b.ins().ishl_imm(recv, 1);
    let id_key = b.ins().bor_imm(id, 1);
    b.ins().select(is_lam, id_key, key)
}

/// 4-way (key, fptr) probe over a heap-Ref site cache — the refpic arm's compare
/// chain, factored so the λ-PIC tests exercise the EXACT production instruction
/// sequence. `fp > 0` skips both empty (0) and negative (-1) entries.
///
/// ⚠**FOUR WAYS, MAIN AND WORKER ALIKE — AND THE ARENA PROBE NEXT DOOR IS NOT.**
/// `emit_dispatch_pic`'s arena arm branches on `rava_ic_base() != 0` into a 16-way probe over the
/// worker's per-thread slice; this one reads `cache_eff` with a fixed four. Under IC_PARALLEL
/// (`RAVA_M3_IC_PARALLEL=1`, opt-in, OFF by default) `cache_eff` on a worker IS that 256-byte
/// 16-way slice, and `rava_dispatch_miss`'s freeze backfill publishes heap CLASS-WORD keys into
/// all sixteen of its ways with a `key & 15` victim — so a heap-Ref entry that lands in way 4..15
/// is invisible here, and that backfill's victim can clobber the four ways this probe does read.
/// That is the same shape as the arena defect where the probe read 4 of the 16 ways its own fill
/// wrote. It is INERT on the default path (`cache_eff` is then always the 4-way global box, and
/// the box is a `[i64; 10]` — reading sixteen ways out of it would run off the allocation), and
/// closing it means changing this probe, `Vm::heappic_fill`'s install and the backfill TOGETHER,
/// which is a change to `emit_dispatch_pic` and out of scope here. Recorded so the next widening
/// starts from the whole list.
///
/// **THE COMPARE PER WAY IS ONE, NOT TWO** (`RAVA_NO_FAST_PICFILL2=1` restores the pair). The old
/// chain asked each way both "is this my key" and "is this entry servable", `band`ed them and
/// selected — six instructions per way on the hottest arm in the engine, to distinguish two cases
/// that cannot both apply. A way is servable unless it holds a `-1`, and a `-1` is always stored
/// under a real key, so selecting on the KEY alone and clamping the single surviving value once at
/// the end reaches the identical answer in 4 instructions per way plus 2: `smax(x, 0)` maps the
/// `-1` to the same 0 the `band` produced, and every other reachable value is already `>= 0`.
///
/// The two forms differ only if one key could occupy TWO ways at once, because the select chain is
/// last-wins: old would keep the positive whichever way held it, new keeps whichever way came
/// last. `Vm::pic_install` is what makes that unreachable — its first rule is "refresh the way
/// that already holds this key", ahead of claiming a free one — and `pic_native::try_fill_ref`
/// reuses this pair's own way for the same reason. Even if a duplicate were somehow published,
/// the worst this returns is 0, i.e. the site's existing `miss` arm: it can lose a hit, never
/// invent one, so no wrong body can be entered.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_refpic_probe(b: &mut FunctionBuilder, cache: Value, key: Value) -> Value {
    if !crate::vm::pic_fill::fast_picfill2_enabled() {
        // LEGACY SHAPE, instruction for instruction.
        let mut fptr_r = b.ins().iconst(I64, 0);
        for k in 0..4i32 {
            let id_k = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(k * 16));
            let fp_k = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(k * 16 + 8));
            let id_eq = b.ins().icmp(IntCC::Equal, id_k, key);
            let fp_ok = b.ins().icmp_imm(IntCC::SignedGreaterThan, fp_k, 0);
            let hit_k = b.ins().band(id_eq, fp_ok);
            fptr_r = b.ins().select(hit_k, fp_k, fptr_r);
        }
        return fptr_r;
    }
    // A free way holds (0, 0) and no live key is ever 0 — a class word is a non-null pointer to an
    // `ArcInner` and a λ identity key is `(recv << 1) | 1`, hence odd — so the key compare alone
    // already rejects empty ways. Only the `-1` verdict needs the clamp.
    let mut fptr_r = b.ins().iconst(I64, 0);
    for k in 0..4i32 {
        let id_k = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(k * 16));
        let fp_k = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(k * 16 + 8));
        let id_eq = b.ins().icmp(IntCC::Equal, id_k, key);
        fptr_r = b.ins().select(id_eq, fp_k, fptr_r);
    }
    let zero = b.ins().iconst(I64, 0);
    b.ins().smax(fptr_r, zero)
}

/// λ-PIC hit-arm adjustment (emitted only under `lambda_pic_enabled`): decode the
/// cached fptr's marker bits. bit0 = DEEP (existing), bit1 = SHIFT — a directable
/// lambda entry's callee (the SAM implementation) does NOT take the lambda itself:
/// its locals are the call's args WITHOUT the receiver slot, so the call passes
/// `bufaddr + 8` (slots are 8 bytes). Returns (fptr & !0b11, effective bufaddr).
/// Class-word entries never carry bit1, so they keep `bufaddr` and the wider mask
/// is a no-op on their 8-aligned fptrs.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_lambda_shift_adjust(b: &mut FunctionBuilder, fptr: Value, bufaddr: Value) -> (Value, Value) {
    let shift_bit = b.ins().band_imm(fptr, 2);
    let shifted = b.ins().icmp_imm(IntCC::NotEqual, shift_bit, 0);
    let buf8 = b.ins().iadd_imm(bufaddr, 8);
    let effbuf = b.ins().select(shifted, buf8, bufaddr);
    let fclean = b.ins().band_imm(fptr, -4i64);
    (fclean, effbuf)
}

/// TYPED NATIVE HIT ARM. Emitted after the arena PIC range test identifies a
/// `pic_native` descriptor rather than compiled guest code. Membership remains
/// range-based because JIT pointers guarantee only 8-byte alignment and cannot
/// donate another tag bit.
///
/// TWO ARMS when `t` is `Some` (`pic_native::leaf_fastcall_enabled()`, the default):
/// the caller branches on `t <u leaf_span()` — the SAME subtraction the range test
/// already produced, so the class test costs one compare and one branch. A LEAF
/// descriptor is entered WITHOUT `spill_for_gc`/`reload_locals_after_gc`/`reload_ref_slot`
/// and its merge args forward the caller's EXISTING SSA reference values: a leaf thunk
/// is a pure read (no guest allocation ⇒ no nursery bump ⇒ no collection ⇒ the
/// unspilled refs cannot move under the call; see each `ws_*` arm's GC-leafness audit),
/// and a refusal happens before any mutation, so the site's `miss` re-spill of the same
/// references is idempotent. A ROOTED descriptor keeps the spill → call → reload
/// discipline verbatim — it may re-enter general dispatch and collect.
///
/// With `t` `None` (`RAVA_NO_LEAF_FASTCALL=1`, or either parent gate off) the emission
/// is EXACTLY the previous unified rooted arm — one `f` load, spill, call, reload —
/// byte-identical, no `leaf_span` compare, no second block.
///
/// A refusal branches to the site's existing `miss` block. Rooted thunks may
/// refuse only from their prologue, before touching the VM, so spilling the same
/// caller references again in `miss` is idempotent. The operand buffer remains
/// untouched on every refusal.
///
/// Costs if removed: every one of these calls goes back to leaving machine code for
/// `rava_dispatch_miss` → negative-cache probe → `jit_call` → `try_invoke_chain` →
/// `dispatch_owner_cascade`, which is the ~19 M-per-boot native-crossing population.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_pic_native_arms(
    b: &mut FunctionBuilder,
    desc: Value,
    natbuf: Value,
    vars: &[Variable],
    stack: &[SV],
    ref_slots: &[usize],
    locals_ptr: Value,
    merge: Block,
    miss: Block,
    t: Option<Value>,
) {
    // Thunk ABI: (desc, packed buf, out) -> served. Kept to three integer arguments so both
    // Windows and SysV pass all of them in registers.
    let conv = b.func.signature.call_conv;
    let mut cs = cranelift::codegen::ir::Signature::new(conv);
    cs.params.push(AbiParam::new(I64));
    cs.params.push(AbiParam::new(I64));
    cs.params.push(AbiParam::new(I64));
    cs.returns.push(AbiParam::new(I64));
    let sigref = b.import_signature(cs);

    // One-word result slot. Returning the bits through memory rather than in the return register
    // is what buys the refusal channel: the register carries served/refused, so a thunk that
    // cannot serve costs one predicted branch and leaves the site's normal miss arm intact.
    //
    // ⭐**ONE WORD PER FUNCTION, NOT PER SITE** (`RAVA_NO_SLOTPOOL=1` restores the private slot).
    // Cranelift never colors `ExplicitSlot`s, so a body's frame is the SUM of every slot it
    // declares — and this one was minted at each PIC site that emits the native arms, in methods
    // that carry a hundred call sites. The three shapes that make sharing sound, all of them
    // properties of THIS slot rather than of the pool:
    //
    //  * **THE LIVE RANGE IS THREE INSTRUCTIONS.** `call_indirect(…, outp)` → `brif` on the
    //    served word → `load [outp]` in `ok`. There is no call between the write and read, so
    //    no other site's use of the shared slot can be interleaved: reaching another
    //    `emit_pic_native_arms` sequence requires leaving this one past its load.
    //  * **THE ADDRESS DOES NOT OUTLIVE THE CALL.** `ThunkFn`'s `out` is a pure out-parameter:
    //    `nt_leaf`/`nt_rooted` (callbacks/dispatch.rs) only ever `*out = bits`, never read it,
    //    never store the pointer, and never forward it — `jit_call` receives the PACKED operand
    //    buffer, not this. Contrast `cap_thunk`'s capture buffers, whose address IS retained as
    //    a lambda environment; those must keep their own slots and do.
    //  * **THE COLLECTOR NEVER SCANS IT.** The GC hazard that governs the CALL buffers is that a
    //    `deep` callee registers `(buffer, frame_len)` as a cell-root RANGE and the collector
    //    walks it blind, so a pooled buffer's stale words would be followed as arena pointers.
    //    This slot is never registered as a root range (only `locals_ptr` buffers are, via the
    //    self-registering prologue), and root scanning in this tree is precise-by-range, not a
    //    conservative machine-stack walk. So the range the collector reads out of this slot is
    //    EMPTY, and "fully initialised over the range the collector scans" is satisfied without
    //    a store. Stale bytes are unreadable for the second reason too: a refusing thunk returns
    //    0 and the arm branches to `miss`, which never loads `outp`, and every path that returns
    //    1 has written the word first (`nt_rooted` writes `*out = 0` before it can even fault).
    //
    // A per-invocation frame is the sharing unit and the only one: recursion into the same
    // method gets a different physical frame, and a re-entrant thunk cannot resume THIS frame.
    // The LEAF arm added below reuses the same slot under the same three shapes: its live
    // range is the same `call` → `brif` → `load` triple, and its use is mutually exclusive
    // with the rooted arm's (one branch, two arms).
    let outslot = crate::jit::emit::scratch_out_slot(b);
    let outp = b.ins().stack_addr(I64, outslot, 0);

    // The descriptor-class diamond, deliberately, behind `leaf_fastcall_enabled()`: the
    // LEAF arm skips the spill entirely and forwards the caller's existing SSA refs; the
    // ROOTED arm is today's body verbatim. Each arm loads `f` itself so the `None` path
    // emits exactly the previous single-arm sequence.
    if let Some(t) = t {
        let leaf_blk = b.create_block();
        let rooted_blk = b.create_block();
        let is_leaf = b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThan, t, crate::jit::pic_native::leaf_span() as i64);
        b.ins().brif(is_leaf, leaf_blk, &[], rooted_blk, &[]);

        // LEAF: entered unspilled. A leaf thunk is a pure read — no guest allocation, no
        // collection, no throw — so the caller's unspilled references cannot move under the
        // call and the merge takes the EXISTING SSA values (the GC-LEAF merge precedent of
        // `calls.rs`). A refusal occurs before any mutation, so the shared `miss` arm may
        // spill these same references again.
        b.switch_to_block(leaf_blk);
        b.seal_block(leaf_blk);
        let lf = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
        let lcall = b.ins().call_indirect(sigref, lf, &[desc, natbuf, outp]);
        let lserved = b.inst_results(lcall)[0];
        let laccepted = b.ins().icmp_imm(IntCC::NotEqual, lserved, 0);
        let leaf_ok = b.create_block();
        b.ins().brif(laccepted, leaf_ok, &[], miss, &[]);
        b.switch_to_block(leaf_ok);
        b.seal_block(leaf_ok);
        let lbits = b.ins().load(I64, MemFlags::trusted(), outp, Offset32::new(0));
        // NO `reload_locals_after_gc` / `reload_ref_slot`: no GC ran. Forward the live SSA
        // values — the same `keep` discipline the GC-LEAF direct-call arm uses.
        let mut largs: Vec<cranelift::codegen::ir::BlockArg> = vec![lbits.into()];
        for &i in ref_slots {
            largs.push(stack[i].v.into());
        }
        b.ins().jump(merge, &largs);

        // ROOTED: today's body, verbatim.
        b.switch_to_block(rooted_blk);
        b.seal_block(rooted_blk);
    }
    let ok = b.create_block();
    let f = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
    spill_for_gc(b, vars, stack, locals_ptr);
    let call = b.ins().call_indirect(sigref, f, &[desc, natbuf, outp]);
    let served = b.inst_results(call)[0];
    let accepted = b.ins().icmp_imm(IntCC::NotEqual, served, 0);
    // A refusal occurs in the thunk prologue before mutation or collection, so
    // the shared miss arm may safely spill these same caller references again.
    b.ins().brif(accepted, ok, &[], miss, &[]);

    b.switch_to_block(ok);
    b.seal_block(ok);
    let bits = b.ins().load(I64, MemFlags::trusted(), outp, Offset32::new(0));
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut args: Vec<cranelift::codegen::ir::BlockArg> = vec![bits.into()];
    for &i in ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        args.push(rv.into());
    }
    b.ins().jump(merge, &args);
}
