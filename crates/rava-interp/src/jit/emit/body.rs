//! Emitting an ordinary method body: the entry prologue, then one pass over the bytecode.

use crate::jit::*;

/// FAST CODEGEN (`RAVA_NO_FAST_CODEGEN=1` restores the previous emission).
///
/// ⭐**CODEGEN VOLUME IS PAID TWICE.** Everything this file lays into the entry block is
/// emitted once by us, then walked by the egraph, then handed to `regalloc2::ion` — which is
/// SUPERLINEAR in live values and blocks and which the phase-A profile prices at 2.82% across
/// three rows, on a compile thread already pinned at ~99%. So an instruction the midend is
/// going to fold away is not free: it is emitter time, GVN hash time and one more node the
/// allocator's interference walk has to look at, on the one resource that is saturated.
///
/// This gate covers ONLY changes that leave the EMITTED MACHINE CODE identical — the entry
/// block's shared zero constant (Cranelift GVNs identical `iconst`s and remats them per block,
/// so 1 and N produce the same code), two bytecode decode walks the emitter did not need to
/// make, and one `spill_for_gc` that provably re-stores the words already in the buffer.
/// Nothing here changes what the program computes, which is what the hash gates measure.
///
/// Read ONCE per process (a `OnceLock`, not a per-emit `var_os`) and consulted only at EMIT
/// time; no compiled code ever reads it.
pub(crate) fn fast_codegen() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_CODEGEN").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// FAST EMIT SHAPE (`RAVA_NO_FAST_EMITSHAPE=1` restores the previous emitter bookkeeping).
///
/// ⭐**THE EMITTER PAYS ITS OWN SHAPE ON THE SERIAL CRITICAL PATH.** Compilation is not
/// overlapped with the boot, so a load, an environment-block walk or a heap allocation
/// `build_body` makes is boot wall time no amount of good machine code buys back. This gate
/// covers ONLY how the emitter decides — not one byte of emitted IR changes, which is what the
/// hash gates measure. Two shapes:
///
///  * the `RAVA_DBG_PROMO` diagnostic was a RAW `std::env::var_os` per PROMOTING METHOD. On
///    Windows MSVC that is `RtlQueryEnvironmentVariable`: a UTF-16 encode of the key into a
///    fresh heap buffer, then a linear walk of the WHOLE process environment block under a
///    lock — to answer a question whose value cannot change during a run (the only writer of
///    the environment is the CLI's config apply, which finishes before the VM exists). Now one
///    process-constant read.
///  * the FIELD-LICM alias table was `vec![None; max_locals]` — an allocation and an
///    O(max_locals) fill for EVERY compiled method, including the majority that have no
///    hoisted (receiver, field) pair at all. With `licm_pairs` empty, `licm_vars` is empty,
///    both `plain_licm` and the PROMO arm are `None`/unreachable, and NOTHING can ever read the
///    table: it was built to be dropped. Now built only when a `Licm` can exist.
///
/// Read ONCE per process (a `OnceLock`, not a per-emit `var_os`) and consulted only at EMIT
/// time; no compiled code ever reads it.
pub(crate) fn fast_emitshape() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_EMITSHAPE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_DBG_PROMO` — which methods promote, and how much. A pure diagnostic, so its answer is
/// a constant of the process; with the shape gate off it goes back to the raw per-method
/// `var_os` verbatim, so the A/B measures the SHAPE and nothing else.
#[cfg(feature = "next-gen-gc")]
fn dbg_promo(fastshape: bool) -> bool {
    if !fastshape {
        return std::env::var_os("RAVA_DBG_PROMO").is_some();
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DBG_PROMO").is_some())
}

/// SCRATCH-SLOT POOLING (`RAVA_NO_SLOTPOOL=1`, or the block switch `RAVA_NO_EMIT_MORE`,
/// restores a private slot at every site — the historic codegen, byte for byte).
///
/// ⭐**CRANELIFT DOES NOT COLOR EXPLICIT SLOTS, SO A METHOD'S FRAME IS THE SUM OF ITS SITES.**
/// `BlockStateBase.initCache` — 21.1% of all machine-code time in a product boot — dumps as
/// **101 `explicit_slot`s totalling 18,976 bytes, every one of them referenced EXACTLY ONCE.**
/// Nothing in the midend merges two `ExplicitSlot`s whose live ranges are disjoint: the
/// declaration IS the reservation. So the only way a per-site scratch region stops multiplying
/// is for the emitter to hand out the SAME slot when it can prove the lifetimes never overlap.
///
/// What this pool covers is deliberately the narrowest such shape in the tree: a ONE-WORD
/// OUT-PARAMETER (`emit_pic_native_arms`), whose live range is `call` → `brif` → `load`, with
/// no instruction between the write and the read that could belong to another site. See the
/// argument at that call site for why no other function's slot can ever be handed back here.
///
/// Read ONCE per process (a `OnceLock`, not a per-emit `var_os`) and consulted only at EMIT
/// time; no compiled code ever reads it.
fn scratch_pool_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_SLOTPOOL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// LEAN CALL/BRANCH SEQUENCE (`RAVA_NO_CALLSEQ=1`, or the block switch `RAVA_NO_EMIT_MORE`,
/// restores the historic emission byte for byte).
///
/// ⭐**THE FRONTEND IS BILLED PER SSA VALUE, NOT PER MACHINE INSTRUCTION.**
/// `BlockStateBase.initCache` — 21.1% of all machine-code time in a product boot — leaves
/// `build_body` as **2,068 real instructions in 426 blocks, but with value ids running to 10,880
/// and 1,409 alias lines**: a 5× builder overhead. Every one of those ids is a `ValueDataPool`
/// entry the egraph hashes, a node `regalloc2::ion` walks (superlinear in live values), and an
/// entry in the SSA builder's own side tables — on the compile thread, which the phase-A profile
/// shows pinned at ~99% and which sits on the boot's SERIAL critical path. So an `iconst` the
/// midend is certain to fold into the next instruction's immediate field is not free; it is
/// paid three times before it disappears.
///
/// This gate covers ONLY changes that leave the SEMANTICS identical (the machine code is
/// expected to be the same or smaller — an `icmp_imm` is what x86 lowering wants anyway):
///
///  * **`ops::branch`** — six `if<cond>`, both `if_acmp`, `ifnull`/`ifnonnull` each materialised
///    their comparison constant as a separate `iconst` and fed it to a two-operand `icmp`.
///    Cranelift's `*_imm` forms carry the constant in the instruction, so the value never exists.
///    Folded ONLY where the operand's `JT` proves its Cranelift type (see each site), so a
///    mistyped stack — which the verifier rejects today — keeps taking the historic path and is
///    rejected exactly as before.
///  * **`ops::stackops`** — `iinc`'s increment, same shape, and `iinc` is the loop-counter
///    opcode, so this one is per loop body rather than per method.
///  * **`build_body`'s static-LICM preheader** — see the `populates` proof at that block: when
///    every static this method reads is a REFERENCE the preheader emits NO callback at all, and
///    the `reload_locals_after_gc` that follows it is then a provable IDENTITY that nevertheless
///    re-`def_var`s EVERY local. That is the single worst thing that can be done to the SSA
///    builder at a point that dominates the whole body.
///
/// Read ONCE per process (a `OnceLock`, not a per-emit `var_os`) and consulted only at EMIT
/// time; no compiled code ever reads it.
pub(crate) fn callseq_lean() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // Per-area switch so a BLOCK verdict can be unbundled into MEMBER verdicts: the four areas
        // of this round shared one name, and the block measured -2% while its members were never
        // priced apart. `RAVA_NO_CALLSEQ` still disables everything.
        // MEASURED OPT-IN. Priced alone over 4 rounds this area read as a win (+2.52% min /
        // +4.27% median when disabled), but the deciding 7-round ABBA on the pair that survived
        // unbundling says the opposite: disabling it is -1.84% min / -1.21% median, i.e. FASTER.
        // Two 4-round readings of overlapping sets disagreed in sign, which is this box's floor
        // talking, so the default follows the tightest measurement and the code stays behind
        // `RAVA_FAST_CALLSEQ_BLOCKS=1`.
        std::env::var_os("RAVA_NO_CALLSEQ").is_none()
            && std::env::var_os("RAVA_FAST_CALLSEQ_BLOCKS").is_some()
            && !crate::flags::emit_more_disabled()
    })
}

thread_local! {
    /// The function-wide one-word scratch slot, or `None` outside a `build_body` session.
    ///
    /// ⚠**A `StackSlot` IS AN INDEX INTO ONE FUNCTION'S SLOT TABLE.** Handing an index minted
    /// while emitting function A to a site in function B would either panic (out of range) or,
    /// far worse, silently alias whatever slot B happens to have at that index. The cell is
    /// therefore cleared at BOTH ends of `build_body`, so it is non-empty only while the
    /// emitter is inside the one function `build_body` is emitting — which makes the invariant
    /// hold no matter which emitter is added next, not merely for today's call graph.
    static SCRATCH_OUT: std::cell::Cell<Option<cranelift::codegen::ir::StackSlot>> =
        std::cell::Cell::new(None);
}

/// Close (and open) a pooling session. Idempotent, and cheap enough to run unconditionally:
/// one thread-local store per compiled method.
fn slotpool_reset() {
    SCRATCH_OUT.with(|c| c.set(None));
}

/// The current function's shared ONE-WORD out-parameter slot, created on first use.
///
/// SAFE TO SHARE, and the proof is a lifetime argument, not a convention:
///  * the only writer is the callee thunk, through the pointer we pass it, DURING the call;
///  * the only reader is the `load` two instructions later, on the served arm;
///  * nothing between them is another site's use, because there is no call between them;
///  * a thunk that re-enters guest code cannot reach another site of THIS frame — that would
///    need this frame's code to run, which it cannot while suspended inside the call. Recursion
///    into the same METHOD is a different invocation with a different physical frame.
/// If any of that stops holding, the answer is a private slot for the offending site, not a
/// wider pool: the guard here only ever REJECTS (kill switch off ⇒ everyone gets their own).
#[allow(dead_code)]
pub(crate) fn scratch_out_slot(b: &mut FunctionBuilder) -> cranelift::codegen::ir::StackSlot {
    if !scratch_pool_enabled() {
        return fresh_scratch8(b);
    }
    if let Some(s) = SCRATCH_OUT.with(|c| c.get()) {
        return s;
    }
    let s = fresh_scratch8(b);
    SCRATCH_OUT.with(|c| c.set(Some(s)));
    s
}

/// The historic private slot, verbatim: 8 bytes, `align_shift` 3.
fn fresh_scratch8(b: &mut FunctionBuilder) -> cranelift::codegen::ir::StackSlot {
    b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3))
}

/// ⭐**THE GAUGE THIS ROUND IS GRADED BY: HOW MANY BYTES OF STACK FRAME DOES A COMPILED BODY
/// RESERVE?** A CLIF dump answers it for ONE method by hand; this answers it for the whole
/// population, which is the only form in which "the frames got smaller" is a measurement rather
/// than an anecdote.
///
/// Reported from the periodic sampler dump beside `compile_bail`, NOT from a teardown — the
/// harness kills the boot at `Done (`, and this tree has already found four censuses that only
/// ever printed at an exit a server boot never reaches.
///
/// Always on: four relaxed adds and one walk of a ~100-entry table, once per COMPILED METHOD
/// (~1,500 per boot). Nothing here runs per call, per site, or per opcode.
pub(crate) mod frame_bytes {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static BODIES: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static SLOTS: AtomicU64 = AtomicU64::new(0);
    static WORST: AtomicU64 = AtomicU64::new(0);
    static BIG: AtomicU64 = AtomicU64::new(0);

    /// A body this large is the shape the round exists to remove (`initCache` is 18,976).
    const BIG_BYTES: u64 = 4096;

    /// Called once per body that `build_body` emitted successfully. The walk covers the WHOLE
    /// function — the driver's `args_slot` and any fork/parallel slot included — because the
    /// prologue's `sub rsp` is what the guest actually pays, not any one emitter's share of it.
    pub(crate) fn note(func: &cranelift::codegen::ir::Function) {
        let mut bytes = 0u64;
        let mut slots = 0u64;
        for d in func.sized_stack_slots.values() {
            if d.kind == cranelift::codegen::ir::StackSlotKind::ExplicitSlot {
                bytes += d.size as u64;
                slots += 1;
            }
        }
        BODIES.fetch_add(1, Relaxed);
        BYTES.fetch_add(bytes, Relaxed);
        SLOTS.fetch_add(slots, Relaxed);
        WORST.fetch_max(bytes, Relaxed);
        if bytes >= BIG_BYTES {
            BIG.fetch_add(1, Relaxed);
        }
    }

    pub(crate) fn report() -> String {
        let n = BODIES.load(Relaxed);
        if n == 0 {
            return String::new();
        }
        let bytes = BYTES.load(Relaxed);
        let slots = SLOTS.load(Relaxed);
        format!(
            "[FRAME] {n} compiled bodies: {bytes} explicit-slot BYTES in {slots} slots \
             (avg {:.0} B / {:.1} slots per body) | worst single body {} B | {} bodies ({:.1}%) over {BIG_BYTES} B",
            bytes as f64 / n as f64,
            slots as f64 / n as f64,
            WORST.load(Relaxed),
            BIG.load(Relaxed),
            BIG.load(Relaxed) as f64 * 100.0 / n as f64,
        )
    }
}

/// Emit the function body. `Some(())` on success, `None` to bail.
///
/// A thin wrapper over the emitter proper, for the two things that must bracket the WHOLE
/// method and cannot be attached to any one of its several `return` paths: the scratch-slot
/// pooling session (see `SCRATCH_OUT` — the cell must be empty the instant we are no longer
/// inside this function's emission, on the bail path as much as on the success path) and the
/// frame-byte census, which needs the finished slot table.
pub(crate) fn build_body(
    b: &mut FunctionBuilder,
    bc: &[u8],
    max_locals: usize,
    leaders: &[usize],
    pool: &ConstantPool,
    env: &Env,
) -> Option<()> {
    slotpool_reset();
    let r = build_body_inner(b, bc, max_locals, leaders, pool, env);
    slotpool_reset();
    if r.is_some() {
        frame_bytes::note(&*b.func);
        // LANE STORE-FORM CENSUS (diagnostic only): mark the body as emitted even when it
        // has no store sites, so the `[LANE-PROOF]` join distinguishes compiled/no-store
        // from a Code identity the emitter never saw.
        #[cfg(feature = "next-gen-gc")]
        crate::jit::emit::store_census::record_compiled(env.code_key);
    }
    r
}

fn build_body_inner(
    b: &mut FunctionBuilder,
    bc: &[u8],
    max_locals: usize,
    leaders: &[usize],
    pool: &ConstantPool,
    env: &Env,
) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    // The entry block can never gain a predecessor — seal it up front (the body
    // may be emitted from a non-entry current block after the LICM prologue's
    // diamonds, so a late seal would need threading the handle around).
    b.seal_block(entry);
    // Register-ABI arc (I1a): when REG_ABI is set the entry params are
    // [array_desc_cell, prim_arg0, prim_arg1, …] (no incoming locals pointer). Provide a
    // dummy internal frame buffer as `locals_ptr` (a pure leaf never spills through it)
    // and pre-convert each register value arg into its JVM local slot as an i64 cell — the
    // vars-init loop below reads these instead of loading from a caller buffer.
    let reg_abi = cur_reg_abi();
    // ⭐**ONE PROCESS CONSTANT, READ FIVE TIMES PER COMPILED METHOD.** `fast_prologue()` and
    // `fast_codegen()` are `OnceLock<bool>`s, i.e. ACQUIRE-ordered atomic loads: LLVM may not
    // hoist one out of the surrounding code, may not merge two of them, and must materialise
    // the values live across each — and this function asked `fast_prologue()` at FIVE separate
    // points (the register-ABI zero-fill floor, the `buf_is_vars` proof, the fused `JitCtx`
    // word, the static-LICM preheader spill skip, and again inside `hoist_late_new_cursor`)
    // and `fast_codegen()` at THREE (here, and once inside each of the two private helpers
    // below). None of them can change during a run. Read each ONCE at the natural entry — this
    // function IS the per-method entry — and pass the boolean down. Identical values, identical
    // decisions, identical emitted IR; only the number of atomic loads on the serial compile
    // path changes, over the ~1500 bodies a boot compiles.
    let fastcg = fast_codegen();
    let fastpro = fast_prologue();
    #[cfg(feature = "next-gen-gc")]
    let fastshape = fast_emitshape();
    // 4th element: see `reg_buf_is_vars` at the static-LICM preheader below — true only when
    // this register entry has provably left the frame buffer holding, word for word, the value
    // every `vars[i]` is about to be defined to, so the preheader's `spill_for_gc` would store
    // back exactly the bytes already there.
    let (locals_ptr, array_desc_cell, reg_arg_cells, reg_buf_is_vars): (
        Value,
        Value,
        Option<Vec<Option<Value>>>,
        bool,
    ) = if let Some(r) = &reg_abi {
            // The `to_vec` is needed only here (the loop below interleaves `b.ins()` with
            // param reads); the MEMORY ABI — every method that is not a register entry —
            // wants two params and used to pay a heap Vec per compiled method for them.
            let bps: Vec<Value> = b.block_params(entry).to_vec();
            let adc = bps[0];
            // Real internal frame buffer, sized to the self-reg registered length (frame_len
            // = max_locals + max_stack) when this body is deep, else max_locals. A non-leaf
            // register entry spills/reloads and self-registers this buffer for GC exactly like
            // the memory ABI's caller-passed buffer.
            //
            // ⛔**THIS SLOT DOES NOT JOIN THE SCRATCH POOL, AND IT IS NOT A CLOSE CALL.** It is
            // this invocation's LOCALS. `locals_ptr` is threaded into every emitted site;
            // `spill_for_gc` writes it before a call and `reload_locals_after_gc` reads it back
            // after; the self-registering prologue publishes `[0, frame_len)` to the collector
            // for the duration of the call. Its live range is therefore ENTRY→RETURN, which
            // overlaps every call buffer in the body — the exact opposite of the strictly
            // nested/sequential lifetime that makes a call-site buffer poolable. Sharing it with
            // anything would let a callee's argument marshalling overwrite the caller's locals.
            // And there is no win to weigh against that risk: this is ONE slot per compiled
            // method already (the entry block runs once), so the frame-byte total the `[FRAME]`
            // census reports cannot fall by a single byte here. The multiplication the round is
            // aimed at is per CALL SITE, and this file has none.
            //
            // The zero-fill below is what the pool's GC rule would have demanded of it anyway,
            // and it is already unconditional for the range the collector scans: see the next
            // comment block — every slot below the prologue's zero floor is written here, so no
            // word of this buffer is ever handed to the collector holding the previous frame's
            // bytes. A pooled buffer would have had to prove exactly that; this one does.
            #[cfg(feature = "next-gen-gc")]
            let flen = env.deep.map(|d| d.frame_len as usize).unwrap_or(max_locals).max(max_locals).max(1);
            #[cfg(not(feature = "next-gen-gc"))]
            let flen = max_locals.max(1);
            let slot = b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                (flen as u32) * 8,
                3,
            ));
            let lp = b.ins().stack_addr(I64, slot, 0);
            let mut cells: Vec<Option<Value>> = vec![None; max_locals];
            let mut bp_i = 1usize; // block-param index (0 = array_desc_cell)
            let mut slot_i = 0usize;
            if r.has_recv {
                // `this` is a Ref cell (i64) at JVM local slot 0.
                let this = bps[bp_i];
                bp_i += 1;
                if !cells.is_empty() {
                    cells[0] = Some(this);
                }
                slot_i = 1;
            }
            for kind in &r.params {
                let bp = bps[bp_i];
                bp_i += 1;
                let cell = match kind {
                    PrimKind::F64 => b.ins().bitcast(I64, MemFlags::new(), bp),
                    PrimKind::F32 => {
                        let bits = b.ins().bitcast(I32, MemFlags::new(), bp);
                        b.ins().uextend(I64, bits)
                    }
                    PrimKind::I32 => b.ins().sextend(I64, bp),
                    // A reference arrives as the operand cell verbatim (`to_cell` on a
                    // JT::Ref is the identity), so there is no conversion at all — and
                    // the store into `cells` below is what makes it a GC root once the
                    // self-registering prologue publishes this buffer.
                    PrimKind::I64 | PrimKind::Ref => bp,
                };
                if slot_i < cells.len() {
                    cells[slot_i] = Some(cell);
                }
                slot_i += kind.slots();
            }
            // Write every arg cell into the frame buffer BEFORE the self-reg prologue
            // registers it: a deep body's GC scans the whole registered length, so the arg
            // slots (incl. the `this` Ref) must hold valid cells, not stack garbage. The
            // prologue zeroes the non-arg slots; vars-init defs the vars from `cells` directly.
            //
            // ⭐**THE ARGUMENT REGION HAD HOLES, AND THEY WERE STACK GARBAGE INSIDE A REGISTERED
            // ROOT RANGE.** `cells` is indexed by JVM local slot, but a WIDE parameter
            // (long/double) occupies TWO slots and produces ONE cell — `slot_i += kind.slots()`
            // leaves its pair slot `None`, so the loop below never stored anything there. This
            // buffer is a fresh `ExplicitSlot`, i.e. whatever the last frame left on the machine
            // stack, and the self-registering push hands `[0, frame_len)` to the collector, which
            // has no way to know a slot is a dead pair half: a garbage word that decodes as an
            // arena pointer gets forwarded and written back. The prologue's zero loop starts at
            // `args_slots` and therefore covers only what is ABOVE this region, never inside it.
            // So fill every unwritten slot below the zero loop's floor with 0. Costs a store per
            // wide-parameter pair slot, and only for a register entry that is actually registered
            // (a GC-leaf register entry pushes nothing, so nothing scans its buffer).
            #[cfg(feature = "next-gen-gc")]
            let zfill: usize = if fastpro {
                env.deep
                    .map(|d| (d.args_slots as usize).max(slot_i).min(cells.len()))
                    .unwrap_or(0)
            } else {
                0
            };
            #[cfg(not(feature = "next-gen-gc"))]
            let zfill: usize = 0;
            // ONE zero per entry block instead of one per argument hole. WAS: a fresh
            // `iconst I64 0` inside the loop, so a wide-parameter-heavy entry laid down as
            // many identical constants as it had pair slots — all of which the egraph then
            // hashes and GVNs back into one, and each of which is one more def the value
            // numbering and the allocator's interference walk must look at. IS: materialised
            // on first need and reused. Cranelift rematerialises `iconst` per block during
            // elaboration, so the machine code is the same either way; only the midend's
            // input shrinks. `fast_codegen()` off restores the per-hole constant exactly.
            let mut zero_c: Option<Value> = None;
            for (i, c) in cells.iter().enumerate() {
                if let Some(cv) = c {
                    b.ins().store(MemFlags::trusted(), *cv, lp, Offset32::new((i * 8) as i32));
                } else if i < zfill {
                    let z = if fastcg {
                        *zero_c.get_or_insert_with(|| b.ins().iconst(I64, 0))
                    } else {
                        b.ins().iconst(I64, 0)
                    };
                    b.ins().store(MemFlags::trusted(), z, lp, Offset32::new((i * 8) as i32));
                }
            }
            // THE FRAME BUFFER NOW EQUALS `vars`, AND ONLY UNDER THIS EXACT CONDITION.
            // Walk the four cases the static-LICM preheader's spill would otherwise cover:
            //  * `cells[i] = Some(cv)`, i < slot_i — the loop above stored `cv`; the deep
            //    prologue's zero loop runs `[args_slots, frame_len)` and `slot_i <=
            //    args_slots` keeps it clear of these, so `cv` survives. `vars[i] = cv`.
            //  * a hole below `zfill` — stored 0 here (re-zeroed by the prologue if it is
            //    also above `args_slots`). `vars[i] = iconst 0`.
            //  * a hole at or above `zfill` — `zfill >= args_slots`, so the prologue's zero
            //    loop covers it. `vars[i] = iconst 0`.
            //  * nothing lies above `max_locals` in `vars`, and `frame_len >= max_locals`.
            // `fast_prologue()` is required because `zfill` is 0 without it (holes below
            // `args_slots` would then be raw stack bits), and `env.deep` is required because
            // a GC-leaf register entry gets no zero loop at all.
            #[cfg(feature = "next-gen-gc")]
            let buf_is_vars =
                fastpro && env.deep.map_or(false, |d| slot_i <= d.args_slots as usize);
            #[cfg(not(feature = "next-gen-gc"))]
            let buf_is_vars = false;
            (lp, adc, Some(cells), buf_is_vars)
        } else {
            // Param 1: the array descriptor-base cell pointer (see Heap::array_desc_cell_ptr).
            let p = b.block_params(entry);
            (p[0], p[1], None, false)
        };

    // M3 COW ARM TEST — HOISTED ONCE PER INVOCATION.
    // `[ctx+12]` (`JitCtx::cow_armed`) is INVARIANT for the duration of one compiled
    // call: its only writer is `Heap::set_cow_armed` (heap.rs), called by the fork
    // COORDINATOR strictly around `thread::scope` while the main thread is parked in
    // the join and runs no guest code, and a phase-B worker executes against a FROZEN
    // private `JitCtx` COPY (`WorkerJitCtxGuard::install`) that nothing writes after
    // install. So test it once here and reuse the boolean at every getfield/putfield
    // site instead of re-loading + re-comparing `[ctx+12]` per field access. Emitted
    // only for bodies that can actually carry a COW check (own field sites or an
    // inline target that may) so no other method gains a dead load; `None` keeps the
    // per-site load (byte-identical to before, and to a COW-gate-off build).
    // ⭐**ONE ALIGNED QWORD FOR BOTH ENTRY GATES.** `JitCtx` is `repr(C)` with
    // `desc_base: usize @0, fault: u32 @8, cow_armed: u32 @12`, so the two words the entry
    // sequence tests are the low and high halves of the SAME 8-aligned qword — and a
    // self-registering body read them as TWO separate 32-bit loads on every single call: the
    // COW hoist below at +12, then the prologue's fault gate at +8. Fold them into one load
    // plus one shift. Both reads were already unsynchronised and non-atomic, an 8-byte aligned
    // load cannot tear on x86-64, and no instruction has ever sat between them, so the pair of
    // values observed is unchanged. Built only for a body that HAS the deep prologue — nothing
    // else in an ordinary body reads `+8`, and a leaf would gain a load it does not want.
    #[cfg(feature = "next-gen-gc")]
    let ctx_words: Option<(Value, Value)> = if fastpro && env.deep.is_some() {
        let w = b.ins().load(I64, MemFlags::trusted(), array_desc_cell, Offset32::new(8));
        let fault = b.ins().ireduce(I32, w); // low half  = JitCtx.fault    (@8)
        let hi = b.ins().ushr_imm(w, 32);
        Some((fault, b.ins().ireduce(I32, hi))) // high half = JitCtx.cow_armed (@12)
    } else {
        None
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let ctx_words: Option<(Value, Value)> = None;

    let cow_armed: Option<Value> = if crate::parallel::cow_gate()
        && !(env.fields.is_empty() && env.inline_targets.is_empty())
    {
        let armed = match ctx_words {
            Some((_, a)) => a,
            None => b.ins().load(I32, MemFlags::trusted(), array_desc_cell, Offset32::new(12)),
        };
        Some(b.ins().icmp_imm(IntCC::NotEqual, armed, 0))
    } else {
        None
    };

    #[cfg(feature = "next-gen-gc")]
    let dv: Option<DeepVars> =
        emit_deep_prologue(b, env, max_locals, locals_ptr, array_desc_cell, cow_armed, ctx_words);
    #[cfg(not(feature = "next-gen-gc"))]
    let dv: Option<DeepVars> = None;

    // The v3-prologue zero-fill boundary is a constant of the METHOD (`deep.args_slots`),
    // not of the slot — derive it ONCE instead of re-reading `env.deep` inside the
    // per-local loop below. `MAX` reproduces "no deep info ⇒ never zero, always load".
    #[cfg(feature = "next-gen-gc")]
    let zero_from: usize = env.deep.map(|d| d.args_slots as usize).unwrap_or(usize::MAX);

    // ⭐**THE ZEROED-LOCAL CONSTANT WAS MATERIALISED ONCE PER SLOT.** Every local at or above
    // the v3 prologue's zero-fill floor is defined to the same `iconst I64 0` — and a method
    // with 24 locals and 3 argument slots emitted TWENTY-ONE separate defs of it, in one block,
    // every one of which the egraph then hashes, GVNs into a single value, and remats per block
    // during elaboration. The machine code is identical whether we emit 1 or 21 (that is what
    // GVN + remat mean here); the difference is 20 instructions the emitter, the value
    // numbering and `regalloc2`'s interference walk never have to touch, per compiled body,
    // across the ~1500 bodies a boot compiles. Materialised on first need so a body that never
    // takes the zero arm emits nothing at all. `fast_codegen()` off restores the per-slot
    // constant exactly.
    let mut zero_v: Option<Value> = None;
    let vars: Vec<Variable> = (0..max_locals)
        .map(|i| {
            let var = b.declare_var(I64);
            // PHASE-A STACK MAPS (`RAVA_STACKMAPS=1`, else not one call is made). Declare
            // every JVM local slot as needing a stack map, matching `spill_for_gc`'s own
            // conservatism EXACTLY — it writes all `max_locals` cells back regardless of
            // type, and the collector shape-tests each one. `declare_var_needs_stack_map`
            // "does not affect any pre-existing uses" (cranelift-frontend-0.132.1
            // src/frontend.rs:439), so it has to happen HERE, before the first `use_var`.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::runtime::stackmaps::declaring() {
                b.declare_var_needs_stack_map(var);
            }
            let off = Offset32::new((i * 8) as i32);
            // Register-ABI (I1a): arg slots come from the register block-params (pre-
            // converted to i64 cells above), every other slot is 0. No caller buffer.
            let loaded = if let Some(cells) = &reg_arg_cells {
                match cells.get(i).copied().flatten() {
                    Some(c) => c,
                    None if fastcg => *zero_v.get_or_insert_with(|| b.ins().iconst(I64, 0)),
                    None => b.ins().iconst(I64, 0),
                }
            } else {
                // Under the v3 prologue the non-argument slots were just zeroed —
                // define them as 0 directly instead of re-loading the store.
                #[cfg(feature = "next-gen-gc")]
                {
                    if i >= zero_from {
                        if fastcg {
                            *zero_v.get_or_insert_with(|| b.ins().iconst(I64, 0))
                        } else {
                            b.ins().iconst(I64, 0)
                        }
                    } else {
                        b.ins().load(I64, MemFlags::trusted(), locals_ptr, off)
                    }
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    b.ins().load(I64, MemFlags::trusted(), locals_ptr, off)
                }
            };
            b.def_var(var, loaded);
            var
        })
        .collect();

    // Inline-TLAB-bump cursor (next-gen-gc): if this method has any arena-eligible
    // `new` site, fetch the thread-local cursor's stable address ONCE here and cache
    // it in a Variable; every inline `new` bumps against it. None (and no callback)
    // otherwise — default build is byte-identical.
    #[cfg(feature = "next-gen-gc")]
    // ⛔**AN INDY SITE DELIBERATELY DOES NOT COUNT HERE.** Adding it gave the inline lambda mint
    // a cursor in methods whose only allocation is a closure — and the run then died with a null
    // local, a different fault from the one the arm already had. Widening the arm's reach keeps
    // uncovering conventions it does not reproduce, so the reach stays where it is verified.
    //
    // ⭐**AND THE LATE-BOUND HALF NOW GETS ONE FETCH TOO.** `env.new_arena` holds only the `new`
    // sites `build_new_arena` could prove arena-eligible AT COMPILE TIME, and its predicate has
    // one non-permanent term — `initialized`. Under the product flag set a method compiles on its
    // FIRST ENTRY, routinely before a cold `new` inside it has run its target's `<clinit>`, so
    // `new_arena` is empty for a large population of allocating methods and this fetch was
    // skipped. Those methods still inline-allocate (`emit_inline_arena_new_late` reads the site's
    // published shape at runtime) — but with `cursor_var == None` each SITE calls
    // `rava_jit_cursor_addr` itself, once per EXECUTION, i.e. once per loop iteration. Hoisting
    // it makes that one call per ENTRY.
    //
    // Three conditions bound it, and each is load-bearing:
    //  * **NO `invokedynamic` ANYWHERE IN THIS BODY.** `cursor_var` is what gates the inline
    //    lambda mint (`ops::invoke_dynamic`), and widening the mint's reach is recorded as having
    //    produced a run that died on a null local. Requiring the body to contain no 0xba at all
    //    means the mint and the inline-concat arms are UNREACHABLE here by construction, so this
    //    hoist cannot reach them — the reach of both stays exactly where it was verified.
    //    (Inlined callees are unaffected either way: `emit_inline` passes them `None`.)
    //  * **THE SITE-CURSOR ARM MUST BE ON.** With `RAVA_NO_INLINE_NEW_SITECURSOR=1` a
    //    late-bound site with no `cursor_var` refuses instead of fetching, so handing it one
    //    would ENABLE the arm rather than cheapen it — a reach change, not a hoist.
    //  * **IT MUST BE A STRICT WIN.** The site pays the fetch only on the arm where its shape
    //    guard passes; the prologue pays it unconditionally. With ONE `new` executed once per
    //    entry the two are equal, and if the site is cold or unpublished the prologue loses.
    //    So require ≥2 `new` sites (≥2 executions ⇒ ≥1 call saved) or a back edge (a `new`
    //    reachable from a loop ⇒ the per-site fetch repeats). Both are pure bytecode/CFG facts.
    //
    // ⭐**THE PROCESS-CONSTANT VETO NOW LEADS THE CONJUNCTION.** `!arena_inline_disabled()` was
    // the RIGHT operand, so `hoist_late_new_cursor` — a full length-decode walk of the whole
    // bytecode — was made to run first even in a build where the arena inline arm is off and
    // its answer could not matter. The gate is a memoised process constant and the walk is
    // O(bytecode) per compiled method; short-circuiting can only save, never cost, and the
    // predicate is unchanged because both operands are pure.
    let cursor_var: Option<Variable> = if !arena_inline_disabled()
        && (!env.new_arena.is_empty()
            || !env.prim_arrays.sites().is_empty()
            || !env.ref_arrays.is_empty()
            || !env.concat_shapes.is_empty()
            || hoist_late_new_cursor(bc, fastpro, fastcg))
    {
        let cv = b.declare_var(I64);
        let call = b.ins().call(env.cursor_addr_fn, &[]);
        let addr = b.inst_results(call)[0];
        b.def_var(cv, addr);
        Some(cv)
    } else {
        None
    };

    // Static-LICM prologue: populate this method's static value cells ONCE at entry
    // (each `getstatic_cached` writes its cell from the canonical static). The method
    // is invariant, so no putstatic can change a static during the call → the body
    // reads the cells as `readonly` loads the egraph hoists out of the loop.
    if env.static_licm {
        // ⭐**A PREHEADER THAT POPULATES NOTHING STILL REDEFINED EVERY LOCAL.**
        // `static_licm` is set by the driver from `field_loads_invariant && has_back_edge &&
        // !statics.is_empty()` — it never asks whether any of those statics is one this
        // preheader can populate. The loop below `continue`s on EVERY `JT::Ref` static (ref
        // cells have no readonly arm; `ops::fields`' getstatic arm refuses them for the same
        // reason), so a looping invariant method whose statics are ALL references — a LOGGER, a
        // registry singleton, an `EMPTY`, an enum's `$VALUES` — reached this block and emitted:
        // two dead `iconst`s, ZERO calls, and then `reload_locals_after_gc`, which is
        // `max_locals` loads and, far worse, `max_locals` `def_var`s.
        //
        // Those `def_var`s are the expensive half and they are provably WORTHLESS here. The
        // reload exists because a `getstatic_cached` callback can run a `<clinit>` and hence a
        // moving collection; with no callback emitted there is no collection point between the
        // spill above and the reload, so `def_var(vars[i], load(locals_ptr + i*8))` re-defines
        // `vars[i]` to the very value `use_var(vars[i])` produced a few instructions earlier —
        // an identity. It is an identity on the skipped-spill arms too, and for the same reason
        // the skip itself is licensed: the memory ABI loaded every `vars[i]` FROM this buffer,
        // and `reg_buf_is_vars` carries the four-case proof for the register ABI. The only
        // instruction that can sit in between is the TLAB cursor fetch, and
        // `rava_jit_cursor_addr` is documented and implemented as "no GC, no allocation — pure
        // setup", so it cannot forward a cell and make the reload observable.
        //
        // But an identity is not free to the FRONTEND: a fresh definition of every local, at a
        // program point that DOMINATES the entire body, is a new value the SSA builder must
        // reconcile at every later merge — precisely the "value per local slot per program
        // point" shape this round exists to remove. So answer the question the driver did not
        // ask, and emit the preheader only when it has something to populate.
        //
        // The guard only ever REJECTS: `populates` is forced true with the kill switch off, and
        // when it is true the emission — constants included, in their original position — is the
        // historic sequence verbatim. The spill is deliberately left ALONE in both arms: it only
        // ever makes the collector's view of the frame buffer more correct, and under the
        // register ABI it is what scrubs the argument-region holes, so removing it would be a
        // GC-root change rather than a builder-volume one.
        let populates =
            !callseq_lean() || env.statics.values().any(|&(_, jt)| jt != JT::Ref);
        let cls_isref = if populates {
            let cls = b.ins().iconst(I64, env.class_ptr);
            let isref0 = b.ins().iconst(I64, 0);
            Some((cls, isref0))
        } else {
            None
        };
        // ⭐**THIS SPILL PUTS BACK THE BYTES IT JUST READ — under the MEMORY ABI it is
        // `max_locals` dead stores per entry.** `spill_for_gc` writes `vars[i]` into
        // `locals_ptr[i]`, and at this exact point every `vars[i]` came from that same buffer a
        // few instructions ago: a slot below the v3 zero-fill floor was LOADED from
        // `locals_ptr[i]`, a slot at or above it is `iconst 0` and the prologue's zero loop has
        // just stored 0 there (and with no deep prologue the floor is `usize::MAX`, so every
        // slot was loaded). Nothing writes this frame in between — the only instruction between
        // them is the TLAB cursor fetch, which touches thread-local nursery state. So the buffer
        // is ALREADY the valid root set the `getstatic_cached` callbacks below need, which is
        // the only reason the spill is here, and `reload_locals_after_gc` afterwards reads the
        // same cells back (forwarded, if a callback collected).
        // The REGISTER ABI kept it unconditionally: there the buffer is a fresh stack slot
        // and only the argument region was written, so slots above it hold garbage the reload
        // would adopt.
        //
        // ⭐**BUT A REGISTER ENTRY WITH THE v3 PROLOGUE HAS NO GARBAGE LEFT** — `reg_buf_is_vars`
        // (built where the argument cells are written, with the four-case proof) is true exactly
        // when the argument-hole fill plus the prologue's `[args_slots, frame_len)` zero loop
        // have between them written every slot below `max_locals` with the SAME word `vars[i]`
        // is defined to. Then this spill is `max_locals` dead stores — and, worse for the row
        // this change is aimed at, `max_locals` `use_var`s that force EVERY local simultaneously
        // live in a register at one program point that the allocator would never have chosen to
        // make a pressure peak. Nothing writes the buffer between the prologue and here: the
        // only instruction in between is the TLAB cursor fetch, which touches thread-local
        // nursery state (and if it could collect, today's spill would be storing STALE register
        // copies over the collector's forwarded cells, so removing it cannot make that worse).
        // `fast_codegen()` off restores the unconditional register-ABI spill.
        let skip_preheader_spill =
            fastpro && (reg_arg_cells.is_none() || (fastcg && reg_buf_is_vars));
        if !skip_preheader_spill {
            spill_for_gc(b, &vars, &[], locals_ptr);
        }
        if let Some((cls, isref0)) = cls_isref {
            for (&cp, &(slot, jt)) in env.statics {
                if jt == JT::Ref {
                    continue; // ref cells stay on the gen-guarded diamond (no readonly arm)
                }
                let cpv = b.ins().iconst(I64, cp as i64);
                let slotv = b.ins().iconst(I64, slot as i64);
                b.ins().call(env.getstatic_cached, &[cls, cpv, isref0, slotv]);
            }
            #[cfg(feature = "next-gen-gc")]
            reload_locals_after_gc(b, &vars, locals_ptr);
        }
    }

    // FIELD-LICM prologue: resolve ONE cell-base pointer per pinned receiver
    // local (all its hoisted fields verified together; sites read
    // `[base + cslot*8]`). MUST run after static_licm (whose getstatic_cached
    // callbacks can GC/move objects — the addresses are only pinned from here
    // on, and no later GC point exists in a no_alloc method). The handle arm is
    // hoisted only when the whole method is putfield-free (invariant_fields): a
    // putfield could append a field and realloc the handle's cells Vec; arena
    // layouts are fixed, so the arena arm is always safe.
    #[cfg(feature = "next-gen-gc")]
    let fast = fast_emit();
    #[cfg(feature = "next-gen-gc")]
    let licm_vars: std::collections::HashMap<(u16, u32, u32), Variable> = {
        let mut m = std::collections::HashMap::with_capacity(env.licm_pairs.len());
        // GROUPING PAIRS BY RECEIVER SLOT — WAS a BTreeSet of the distinct slots followed
        // by a full `filter` re-scan of `licm_pairs` FOR EACH ONE: O(slots × pairs), i.e.
        // quadratic in the number of hoisted (receiver, field) pairs a method has, plus a
        // fresh `Vec` per slot either way. IS one pass that pushes each pair into its
        // slot's bucket: O(pairs log slots). Emission order is unchanged — a `BTreeMap`
        // still yields slots ASCENDING and each bucket keeps `licm_pairs` order, which is
        // exactly what the old set-then-filter produced, so `emit_licm_base` sees the same
        // calls in the same sequence.
        let groups: Vec<(u16, Vec<(u32, u32)>)> = if fast {
            let mut g: std::collections::BTreeMap<u16, Vec<(u32, u32)>> =
                std::collections::BTreeMap::new();
            for p in env.licm_pairs {
                g.entry(p.slot).or_default().push((p.fid, p.cslot));
            }
            g.into_iter().collect()
        } else {
            let slots: std::collections::BTreeSet<u16> =
                env.licm_pairs.iter().map(|p| p.slot).collect();
            slots
                .into_iter()
                .map(|k| {
                    let pairs: Vec<(u32, u32)> = env
                        .licm_pairs
                        .iter()
                        .filter(|p| p.slot == k)
                        .map(|p| (p.fid, p.cslot))
                        .collect();
                    (k, pairs)
                })
                .collect()
        };
        for (k, pairs) in groups {
            let var = b.declare_var(I64);
            let obj = b.use_var(vars[k as usize]);
            // The handle arm needs the cells Vec to never realloc during the call.
            // invariant_fields (no putfield at all) guarantees that; so does the
            // PROMO gate — every written pair is VERIFIED PRESENT by this very
            // prologue (cslot < count && ids[cslot]==fid), so no putfield can
            // append, and the promoted body has no GC point to move anything.
            let hoist_handle = env.invariant_fields || env.promo_written.is_some();
            let base = emit_licm_base(b, array_desc_cell, obj, &pairs, hoist_handle);
            b.def_var(var, base);
            for (fid, cslot) in pairs {
                m.insert((k, fid, cslot), var);
            }
        }
        m
    };
    // ALIAS TABLE — WAS a full `any()` scan of `licm_pairs` FOR EVERY LOCAL SLOT of the
    // method: O(max_locals × pairs), the product of two things that both grow with the
    // method. IS one pass that marks each pair's slot: O(max_locals + pairs). Identical
    // contents — entry `i` is `Some(i)` iff some pair pins slot `i`.
    //
    // ⭐**AND IT WAS BUILT FOR A CONSUMER THAT CANNOT EXIST.** Whichever arm ran, this is a heap
    // allocation plus an O(max_locals) fill for EVERY compiled method — and the only way any
    // byte of it is ever read is through a `Licm`, of which there are exactly three
    // constructions below (`plain_licm`, `promo_licm`, and the PROMO seeding loop), all three
    // guarded by `!licm_vars.is_empty()`. `licm_vars` gains an entry only inside the
    // `for (k, pairs) in groups` loop above, so it is non-empty IFF `env.licm_pairs` is
    // non-empty. When there are no hoisted pairs — the majority of bodies, since FIELD-LICM
    // needs a pinned receiver with verifiable fields — the table was allocated, filled with
    // `None`, borrowed by nothing and dropped. Answer the emptiness question first and skip it.
    // The all-`None` contents are unchanged for every method that CAN read them, so no `Licm`
    // ever sees a different alias table; `fast_emitshape()` off restores the unconditional
    // build exactly.
    #[cfg(feature = "next-gen-gc")]
    let licm_alias: Vec<Option<u16>> = if fastshape && env.licm_pairs.is_empty() {
        Vec::new()
    } else if fast {
        let mut a: Vec<Option<u16>> = vec![None; max_locals];
        for p in env.licm_pairs {
            if (p.slot as usize) < max_locals {
                a[p.slot as usize] = Some(p.slot);
            }
        }
        a
    } else {
        (0..max_locals)
            .map(|i| env.licm_pairs.iter().any(|p| p.slot as usize == i).then_some(i as u16))
            .collect()
    };

    // PROMO method-level UNSWITCH: when the whole method passed `promo_scan`,
    // test ONCE at entry that every hoisted base resolved (non-null arena/handle
    // receivers), then run a SECOND copy of the body whose field accesses are
    // PURE register reads/writes — no per-site guard, no load/store in the loop
    // (C2's scalar-promotion shape). Any miss runs the untouched base-pointer
    // body; base!=0 is fixed per call so the two worlds never mix. Version A is
    // callback-free by construction (the promo_scan whitelist), so no GC point,
    // exception or reentry can observe the deferred stores before a flush.
    #[cfg(feature = "next-gen-gc")]
    {
        let empty_vals: std::collections::HashMap<(u16, u32, u32), Variable> =
            std::collections::HashMap::new();
        let empty_flush: Vec<(Variable, Variable, u32)> = Vec::new();
        let plain_licm = (!licm_vars.is_empty()).then_some(Licm {
            vars: &licm_vars,
            alias: &licm_alias,
            vals: &empty_vals,
            flush: &empty_flush,
        });

        // COW METHOD-ENTRY UNSWITCH — the COW machinery ONCE PER METHOD, not once
        // per field site. `[ctx+12]` is invocation-invariant (hoisted above), so
        // branch on it ONCE here:
        //   * the ARMED body keeps today's per-site diamond (bloom probe + shadow
        //     callback). Cold — only a phase-B fork worker ever enters it — so it
        //     is emitted PLAIN: no PROMO sub-unswitch, no FLAT-GETFIELD arena proof
        //     (empty proof table ⇒ every site keeps its own guard). That is what
        //     bounds the combination to at most THREE bodies (1 armed + the
        //     disarmed side's 1-or-2) instead of squaring to four, and it keeps
        //     COW semantics for a worker exactly as they are today.
        //   * the DISARMED body (fall-through, the hot one: the main thread is
        //     disarmed for all of boot) carries ZERO COW instructions — no
        //     `[ctx+12]` test, no bloom probe, no merge phi. Removing the diamond
        //     also removes the egraph OPTIMIZATION BARRIER it imposed: the
        //     FIELD-LICM base and the guard operands stop being respilled and
        //     reloaded between field sites, which is where the per-getfield
        //     instruction count actually collapses.
        // No hoisted flag (COW gate off, or a body that can carry no COW check at
        // all) ⇒ no unswitch and byte-identical emission.
        // WAS: `count_field_sites` walked the WHOLE bytecode of EVERY compiled method,
        // length-decoding every instruction, before the match that in the common case
        // throws the number away (no hoisted COW flag at all, or the unswitch off). IS:
        // short-circuited behind both gates, so the extra O(bytecode) walk happens only
        // for a body that is actually an unswitch candidate. Same predicate, same result.
        let cow = match cow_armed {
            Some(v)
                if cow_unswitch_enabled()
                    && cow_unswitch_reached(bc, env.inline_targets.len(), fastcg) =>
            {
                let armed_body = b.create_block();
                let disarmed_body = b.create_block();
                b.ins().brif(v, armed_body, &[], disarmed_body, &[]);
                b.switch_to_block(armed_body);
                b.seal_block(armed_body);
                emit_blocks(
                    b, &vars, bc, leaders, pool, env, array_desc_cell, locals_ptr,
                    CowArm::Hoisted(v), &[], cursor_var, plain_licm, dv,
                )?;
                b.switch_to_block(disarmed_body);
                b.seal_block(disarmed_body);
                CowArm::Disarmed
            }
            Some(v) => CowArm::Hoisted(v),
            None => CowArm::PerSite,
        };

        if env.promo_written.is_some() && !licm_vars.is_empty() {
            let written = env.promo_written?;
            let mut bases: Vec<Variable> = Vec::new();
            let mut vals: std::collections::HashMap<(u16, u32, u32), Variable> =
                std::collections::HashMap::new();
            let mut flush: Vec<(Variable, Variable, u32)> = Vec::new();
            for p in env.licm_pairs {
                let key = (p.slot, p.fid, p.cslot);
                let base = licm_vars[&key];
                if !bases.contains(&base) {
                    bases.push(base);
                }
                let val = b.declare_var(I64);
                vals.insert(key, val);
                if written.contains(&key) {
                    flush.push((base, val, p.cslot));
                }
            }
            let mut ok: Option<Value> = None;
            for &base in &bases {
                let pv = b.use_var(base);
                let nz = b.ins().icmp_imm(IntCC::NotEqual, pv, 0);
                ok = Some(match ok {
                    Some(o) => b.ins().band(o, nz),
                    None => nz,
                });
            }
            let la = b.create_block();
            let lb = b.create_block();
            b.ins().brif(ok?, la, &[], lb, &[]);

            // Version A preheader: seed every value register from its cell.
            b.switch_to_block(la);
            b.seal_block(la);
            // Gated diagnostic (RAVA_DBG_PROMO): which methods promote, and how much.
            if dbg_promo(fastshape) {
                eprintln!(
                    "[PROMO] unswitch emitted: pairs={} written={} bases={}",
                    env.licm_pairs.len(),
                    flush.len(),
                    bases.len()
                );
            }
            for p in env.licm_pairs {
                let key = (p.slot, p.fid, p.cslot);
                let base = b.use_var(licm_vars[&key]);
                let cell = b.ins().load(
                    I64,
                    MemFlags::trusted(),
                    base,
                    Offset32::new(p.cslot as i32 * 8),
                );
                b.def_var(vals[&key], cell);
            }
            let promo_licm = Some(Licm {
                vars: &licm_vars,
                alias: &licm_alias,
                vals: &vals,
                flush: &flush,
            });
            emit_blocks(
                b, &vars, bc, leaders, pool, env, array_desc_cell, locals_ptr, cow, &[],
                cursor_var, promo_licm, dv,
            )?;

            // Version B: the untouched base-pointer body.
            b.switch_to_block(lb);
            b.seal_block(lb);
            return emit_blocks(
                b, &vars, bc, leaders, pool, env, array_desc_cell, locals_ptr, cow, &[],
                cursor_var, plain_licm, dv,
            );
        }
        // FLAT-GETFIELD method-entry UNSWITCH: prove ONE receiver arena, then stop
        // re-guarding it. Speculate at entry that the pinned local `flat_unswitch_slot`
        // picked holds an arena Obj; the ARENA body then reads every field of THAT
        // receiver as a straight-line `[recv + HDR + slot*8]` load with no per-site
        // arena/handle diamond, and a FALLBACK body keeps the diamonds for a non-arena
        // (heap Ref / null) receiver. Gated to when NO FIELD-LICM base-pinning applies
        // (plain_licm None — that path already de-diamonds under no_alloc).
        //
        // WHY THE ONE ENTRY TEST COVERS THE WHOLE BODY, and where it would stop:
        //  * REASSIGNMENT would end it — a slot that is astore'd could hold a heap
        //    handle later, and reading `[handle + HDR + ..]` is a wild pointer. Excluded
        //    at the source: `pinned_ref_slots` only ever reports NEVER-STORED slots.
        //  * MOVING does NOT end it. A call can collect, and `reload_locals_after_gc`
        //    then re-`def_var`s the slot, so a site's receiver is a different SSA value
        //    than the one tested here — but it is the FORWARDED pointer of the SAME
        //    object, and `Region::copy_obj` evacuates young arena → tenured arena, both
        //    below 2^40 by `Region::new`. Arena-ness is a lifetime property: a heap
        //    handle is never rewritten into a pointer, and null stays null. Only the
        //    KIND branch is hoisted; the pointer itself still reloads at every site.
        // So the proof spans the whole invocation, calls included, and it is threaded
        // by value identity (`SV::arena`) — including one frame down through the inline
        // arg binding — so no OTHER receiver can ever consume it.
        //
        // Emitting the body TWICE costs compile time and code size, so require at least
        // FLAT_GETFIELD_MIN_GETFIELDS reads of the chosen receiver first.
        #[cfg(feature = "next-gen-gc")]
        if let Some(slot) = (flat_getfield_enabled() && plain_licm.is_none() && !env.fields.is_empty())
            .then(|| flat_unswitch_slot(bc, max_locals))
            .flatten()
        {
            let mut arena_proof = vec![false; max_locals.max(1)];
            arena_proof[slot] = true;
            let recv0 = b.use_var(vars[slot]);
            let is_ar = emit_is_arena(b, recv0);
            let arena_body = b.create_block();
            let fallback_body = b.create_block();
            b.ins().brif(is_ar, arena_body, &[], fallback_body, &[]);

            // ARENA body: the chosen receiver is proven → its getfields straight-line.
            b.switch_to_block(arena_body);
            b.seal_block(arena_body);
            emit_blocks(
                b, &vars, bc, leaders, pool, env, array_desc_cell, locals_ptr, cow,
                &arena_proof, cursor_var, None, dv,
            )?;

            // FALLBACK body: non-arena / null receiver → the normal per-site diamond.
            // NOTHING is proven here, so the proof table is empty.
            b.switch_to_block(fallback_body);
            b.seal_block(fallback_body);
            return emit_blocks(
                b, &vars, bc, leaders, pool, env, array_desc_cell, locals_ptr, cow, &[],
                cursor_var, None, dv,
            );
        }
        return emit_blocks(
            b, &vars, bc, leaders, pool, env, array_desc_cell, locals_ptr, cow, &[],
            cursor_var, plain_licm, dv,
        );
    }
    #[cfg(not(feature = "next-gen-gc"))]
    emit_blocks(
        b,
        &vars,
        bc,
        leaders,
        pool,
        env,
        array_desc_cell,
        locals_ptr,
        match cow_armed {
            Some(v) => CowArm::Hoisted(v),
            None => CowArm::PerSite,
        },
    )
}

/// `count_field_sites(bc) + inline_targets >= cow_unswitch_min()`, answered without decoding
/// the whole body.
///
/// WAS: a full O(bytecode) length-decode walk that counted EVERY `getfield`/`putfield` in the
/// method and then compared the total against a threshold whose DEFAULT IS 1 — a census taken
/// to answer a yes/no question, on the compile thread the phase-A profile shows pinned at ~99%,
/// for every method that reaches the COW unswitch gate (which is every compiled method with a
/// field access, since `cow_armed` is `Some` exactly when the body has field sites or inline
/// targets).
///
/// IS: the inline-target count is tested first — it can satisfy the threshold with no walk at
/// all — and the walk that remains STOPS at the instant enough sites have been seen, i.e. at
/// the FIRST field opcode under the default threshold.
///
/// The predicate is unchanged, including its two edges: a threshold of 0 is satisfied before
/// any walk (as `count + n >= 0` always was), and an undecodable body stops the count at the
/// same instruction `count_field_sites` stops it at. Identical emitted body either way; this
/// only changes how the emitter spends its own time. `fast_codegen()` off restores the census.
#[cfg(feature = "next-gen-gc")]
fn cow_unswitch_reached(bc: &[u8], inline_targets: usize, fastcg: bool) -> bool {
    let min = cow_unswitch_min();
    if inline_targets >= min {
        return true;
    }
    if !fastcg {
        return count_field_sites(bc) + inline_targets >= min;
    }
    let need = min - inline_targets;
    let mut seen = 0usize;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return seen >= need };
        if op == 0xb4 || op == 0xb5 {
            seen += 1;
            if seen >= need {
                return true;
            }
        }
        pc += len;
    }
    seen >= need
}

/// Should the entry prologue fetch the TLAB cursor for a body whose `new` sites are ALL
/// late-bound (`env.new_arena` empty)? See the block comment at the `cursor_var` site for why
/// each term is here; this is the predicate, computed from bytecode and CFG only.
///
/// One decode walk answers both counts, and it runs only when the two compile-time-resolved
/// populations (`new_arena`, `concat_shapes`) are already empty — the `||` chain at the call
/// site short-circuits, so a method that already has a cursor never pays for this scan, and the
/// `arena_inline_disabled()` veto now sits to its LEFT so a build with the arm off pays no scan
/// at all. `fastpro`/`fastcg` are `fast_prologue()`/`fast_codegen()` read once by the caller;
/// they are constants of the process, so passing them down is the same predicate with two fewer
/// acquire loads per compiled method.
#[cfg(feature = "next-gen-gc")]
fn hoist_late_new_cursor(bc: &[u8], fastpro: bool, fastcg: bool) -> bool {
    if !fastpro || !inline_new_site_cursor_enabled() {
        return false;
    }
    let mut pc = 0usize;
    let mut news = 0usize;
    // BACK EDGE, COLLECTED IN THE WALK ALREADY BEING MADE. `has_back_edge` is a SECOND full
    // length-decode pass over the same bytes, taken only to answer the `news == 1` arm below —
    // two O(bytecode) walks per compiled method where one carries both answers. The opcode set
    // (`ifeq..if_acmpne`, `goto`, `ifnull`, `ifnonnull`) and the `target <= pc` test are copied
    // from `has_back_edge` verbatim, so the predicate is bit-identical; a body that never
    // reaches the `news == 1` arm simply never reads the flag.
    let mut back_edge = false;
    while pc < bc.len() {
        let op = bc[pc];
        // An undecodable body is not a body this hoist reasons about; the emitter that follows
        // bails on it anyway. Refusing keeps "cannot prove ⇒ do not emit" local to one line.
        // (`has_back_edge` refuses the same way, so folding it in changes nothing here either.)
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            0xbb => news += 1,
            // invokedynamic anywhere ⇒ refuse outright: handing this body a cursor would also
            // hand it to the inline lambda mint / inline concat, whose reach is fixed elsewhere.
            0xba => return false,
            0x99..=0xa7 | 0xc6 | 0xc7 if fastcg => {
                let target = (pc as i32 + s16(bc, pc + 1)) as usize;
                if target <= pc {
                    back_edge = true;
                }
            }
            _ => {}
        }
        pc += len;
    }
    news >= 2 || (news == 1 && (if fastcg { back_edge } else { has_back_edge(bc) }))
}
