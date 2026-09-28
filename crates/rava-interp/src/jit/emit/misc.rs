//! Small emitters with no larger family: ldc, the ring store guard, the deep epilogue, blocks.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// `RAVA_NO_FAST_EMIT=1` restores the emitter's original per-opcode bookkeeping.
///
/// EVERY BYTE OF EMITTED IR IS IDENTICAL EITHER WAY — this gate only changes how the
/// emitter spends its OWN time, and Cranelift compilation sits on the boot's SERIAL
/// critical path, so emitter time is boot time that no amount of good machine code
/// pays back. What it turns off: the leader CURSOR (below), the reused operand-stack
/// buffer, and the one-pass FIELD-LICM grouping in `build_body`.
pub(crate) fn fast_emit() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::round7() && std::env::var_os("RAVA_NO_FAST_EMIT").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Translate the whole bytecode body into a fresh universe of basic blocks
/// (per-leader block map, operand-stack shapes, back-edge seals). Factored out
/// of `build_body` so the PROMO unswitch can emit the body TWICE — once with
/// register-promoted fields, once on the plain base-pointer path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_blocks(
    b: &mut FunctionBuilder,
    vars: &[Variable],
    bc: &[u8],
    leaders: &[usize],
    pool: &ConstantPool,
    env: &Env,
    array_desc_cell: Value,
    locals_ptr: Value,
    // What this body knows about the M3 COW arm flag for the whole invocation
    // (see the method-entry COW UNSWITCH in `build_body`).
    cow: CowArm,
    // Which of THIS body's local slots the method-entry unswitch already proved to
    // hold an arena pointer (see `flat_unswitch_slot`). Empty / all-false = nothing
    // proven, so every field site derives its own guard exactly as before.
    #[cfg(feature = "next-gen-gc")] arena_proof: &[bool],
    #[cfg(feature = "next-gen-gc")] cursor_var: Option<Variable>,
    #[cfg(feature = "next-gen-gc")] licm: Option<Licm>,
    #[cfg(feature = "next-gen-gc")] dv: Option<DeepVars>,
) -> Option<()> {
    let fast = fast_emit();
    // Both maps are sized ONCE from the leader count instead of growing (and rehashing
    // every live key) as the body is walked.
    let mut blocks = std::collections::HashMap::with_capacity(leaders.len());
    // Parallel to `leaders`, in the same order: the block handle without a hash lookup.
    let mut leader_blocks: Vec<Block> = Vec::with_capacity(leaders.len());
    for &l in leaders {
        let blk = b.create_block();
        blocks.insert(l, blk);
        leader_blocks.push(blk);
    }
    let mut block_shapes: std::collections::HashMap<usize, Vec<JT>> =
        std::collections::HashMap::with_capacity(leaders.len() + 1);
    block_shapes.insert(0, Vec::new());
    b.ins().jump(blocks[&0], &[]);

    let mut stack: Vec<SV> = Vec::new();
    let mut pc = 0usize;
    // Cursor into `leaders`, monotone with `pc` (see the loop head).
    let mut li = 0usize;
    let mut cur_open = false;
    // Only compute this on an otherwise-refused path. Straight-line methods
    // and bodies whose block shapes are already known pay no extra CFG walk.
    let mut normal_flow = None;

    while pc < bc.len() {
        if !cur_open && pc != 0 && !block_shapes.contains_key(&pc) {
            if normal_flow.is_none() {
                normal_flow = Some(crate::jit::analysis::normal_flow::NormalFlow::compute(bc)?);
            }
            if !normal_flow.as_ref()?.contains(pc) {
                pc += insn_len_at(bc, pc)?;
                continue;
            }
        }
        // LEADER TEST — WAS a SipHash probe of `blocks` for EVERY OPCODE of every body
        // (`blocks.get(&pc)`), i.e. O(opcodes) hashes of a usize asked to answer a
        // question about the small minority of PCs that actually start a block; and the
        // body is emitted up to THREE times per method (COW / PROMO / FLAT unswitch), so
        // it is O(3 × opcodes) hashes per compiled method. IS an integer compare against
        // a cursor: `find_leaders` collects a `BTreeSet`, so `leaders` is ascending and
        // unique, and `pc` walks instruction starts monotonically — one forward cursor
        // answers the same question in O(opcodes) compares + O(leaders) cursor steps.
        // The map is still BUILT (jump_to/brif_to/emit_op are handed it by reference) but
        // is no longer consulted on the straight-line path. The `< pc` skip loop keeps
        // this correct even for a leader that is not an instruction start — such a body
        // is rejected by `find_leaders` before it ever reaches here.
        let here: Option<Block> = if fast {
            while li < leaders.len() && leaders[li] < pc {
                li += 1;
            }
            if li < leaders.len() && leaders[li] == pc {
                li += 1;
                Some(leader_blocks[li - 1])
            } else {
                None
            }
        } else {
            blocks.get(&pc).copied()
        };
        if let Some(blk) = here {
            if cur_open {
                jump_to(b, &blocks, &mut block_shapes, pc, &stack)?;
            } else if !block_shapes.contains_key(&pc) {
                return None;
            }
            b.switch_to_block(blk);
            // BLOCK ENTRY STACK — WAS two heap allocations per BASIC BLOCK: a `clone()` of
            // the recorded shape `Vec<JT>` (never mutated, and `block_shapes`/`b` are
            // distinct bindings so the clone bought no borrow) plus a fresh `Vec<SV>`
            // collected for the operand stack. O(blocks) allocations per body × up to
            // three bodies. IS: the shape is read in place and the ONE operand-stack
            // buffer is refilled, so the whole method costs at most the deepest stack.
            let shape = block_shapes.get(&pc)?;
            let params = b.block_params(blk);
            if params.len() != shape.len() {
                return None;
            }
            if fast {
                stack.clear();
            } else {
                stack = Vec::with_capacity(shape.len());
            }
            stack.extend(shape.iter().zip(params.iter()).map(|(&t, &v)| SV {
                v,
                t,
                src: None,
                #[cfg(feature = "next-gen-gc")]
                arena: false,
            }));
            cur_open = true;
        } else if !cur_open {
            return None; // unreachable code after a terminator and not a leader
        }
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        if emit_op(
            b,
            vars,
            &mut stack,
            bc,
            pc,
            op,
            &blocks,
            &mut block_shapes,
            &mut cur_open,
            pool,
            env,
            array_desc_cell,
            locals_ptr,
            cow,
            #[cfg(feature = "next-gen-gc")]
            arena_proof,
            #[cfg(feature = "next-gen-gc")]
            cursor_var,
            #[cfg(feature = "next-gen-gc")]
            licm,
            #[cfg(feature = "next-gen-gc")]
            dv,
        )
        .is_none()
        {
            // Gated: which opcode made emit bail (RAVA_DBG_EMITOP) — drives the safe
            // "implement the missing opcode" coverage grind (as athrow/insn_len did).
            if std::env::var("RAVA_DBG_EMITOP").is_ok() {
                eprintln!("[EMITOP-BAIL] op=0x{:02x} pc={} len_bc={}", op, pc, bc.len());
            }
            return None;
        }
        pc += len;
    }

    // Same order as `leaders` by construction — one hash lookup per leader removed.
    for &blk in &leader_blocks {
        b.seal_block(blk);
    }
    Some(())
}

/// GILFREE-RINGS: open the array-store worker barrier around the native store
/// that follows. The lane is DEFAULT ON, so this is emitted in an ordinary build;
/// `RAVA_NO_GILFREE_RINGS` restores the array-store codegen that predates the arm.
///
/// Shape (mirrors the putfield COW arm, minus the store duplication):
/// ```text
///   brif armed -> chk, inline
///   chk:    skip = rava_ring_store_guard(arr)
///           brif skip -> merge, inline
///   inline: <native store>          ; two predecessors, no duplicated emit
///           jump merge
///   merge:
/// ```
/// The returned block is handed to `close_ring_store_guard` after the store.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_ring_store_guard(
    b: &mut FunctionBuilder,
    env: &Env,
    ctx_cell: Value,
    cow: CowArm,
    arr: Value,
) -> Option<Block> {
    // The COW method-entry UNSWITCH already settled the flag for this body: a
    // DISARMED body would only ever fall straight through this diamond's `inline`
    // arm, so emit none of it — same reasoning (and same win) as the field sites.
    if !crate::gilfree_cf::rings() || matches!(cow, CowArm::Disarmed) {
        return None;
    }
    let chk = b.create_block();
    let inline = b.create_block();
    let merge = b.create_block();
    // Same invocation-invariant flag the field sites branch on — reuse the
    // hoisted entry-block test when this body has one.
    let armed = match cow {
        CowArm::Hoisted(v) => v,
        _ => {
            let a = b
                .ins()
                .load(I32, MemFlags::trusted(), ctx_cell, Offset32::new(12));
            b.ins().icmp_imm(IntCC::NotEqual, a, 0)
        }
    };
    b.ins().brif(armed, chk, &[], inline, &[]);
    b.switch_to_block(chk);
    b.seal_block(chk);
    let call = b.ins().call(env.ring_store_guard, &[arr]);
    let skip = b.inst_results(call)[0];
    b.ins().brif(skip, merge, &[], inline, &[]);
    b.switch_to_block(inline);
    b.seal_block(inline); // both predecessors emitted above
    Some(merge)
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn emit_ring_store_guard(
    _b: &mut FunctionBuilder,
    _env: &Env,
    _ctx_cell: Value,
    _cow: CowArm,
    _arr: Value,
) -> Option<Block> {
    None
}

/// Rejoin the guarded store with the deopt path opened by `emit_ring_store_guard`.
pub(crate) fn close_ring_store_guard(b: &mut FunctionBuilder, merge: Option<Block>) {
    if let Some(merge) = merge {
        b.ins().jump(merge, &[]);
        b.switch_to_block(merge);
        b.seal_block(merge);
    }
}

/// Deep-IC v3 EPILOGUE, emitted before every `return_` of a self-registering
/// body: pop this frame's cell-root entry inline (restore the saved top — all
/// nested entries are already popped by their own epilogues/Rust wrappers),
/// then restore the box discipline. The box-unwind callback is COLD: taken
/// only when the callee actually grew the box table (it also re-homes a
/// BOX-tagged return onto the caller's base). Returns the final result cell.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_deep_epilogue(
    b: &mut FunctionBuilder,
    env: &Env,
    dv: Option<DeepVars>,
    cell: Value,
) -> Value {
    let Some(dv) = dv else { return cell };
    let ctx = b.use_var(dv.ctx);
    let top = b.use_var(dv.top);
    let base = b.use_var(dv.base);
    b.ins().store(MemFlags::trusted(), top, ctx, Offset32::new(0));
    let bl = b.ins().load(I64, MemFlags::trusted(), ctx, Offset32::new(24));
    let grew = b.ins().icmp(IntCC::NotEqual, bl, base);
    let cold = b.create_block();
    let done = b.create_block();
    b.append_block_param(done, I64);
    b.ins().brif(grew, cold, &[], done, &[cell.into()]);
    b.switch_to_block(cold);
    b.seal_block(cold);
    let call = b.ins().call(env.box_exit, &[base, cell]);
    let r = b.inst_results(call)[0];
    b.ins().jump(done, &[r.into()]);
    b.switch_to_block(done);
    b.seal_block(done);
    b.block_params(done)[0]
}

/// Resolve a constant-pool entry to a numeric immediate; bail if non-numeric.
/// ldc: numeric inline; a reference constant (String/Class) resolves via the
/// `rava_ldc` callback under next-gen-gc (else bail to interp).
#[allow(unused_variables)]
pub(crate) fn emit_ldc_ref(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    pool: &ConstantPool,
    env: &Env,
    vars: &[Variable],
    locals_ptr: Value,
    idx: u16,
) -> Option<()> {
    if ldc_const(b, stack, pool, idx).is_some() {
        return Some(());
    }
    #[cfg(feature = "next-gen-gc")]
    {
        // LICM: a String constant is loop-invariant + immutable. Intern it ONCE
        // into the permanent low-arena pool at COMPILE time and bake its stable
        // address as an `iconst` — no per-execution `rava_ldc` callback, no
        // nursery churn, automatically hoisted out of every loop. The baked ptr
        // is non-nursery so the collector never moves it (copy_obj leaves it).
        if !arena_inline_disabled() {
            if let Some(s) = pool.string_const(idx) {
                let p = crate::nextgen::intern_arena_string(s);
                if p != 0 {
                    let v = b.ins().iconst(I64, p);
                    push(stack, v, JT::Ref);
                    return Some(());
                }
            }
        }
        let cls = b.ins().iconst(I64, env.class_ptr);
        let cpv = b.ins().iconst(I64, idx as i64);
        spill_for_gc(b, vars, stack, locals_ptr);
        let call = b.ins().call(env.ldc, &[cls, cpv]);
        let res = b.inst_results(call)[0];
        reload_after_gc(b, vars, stack, locals_ptr);
        push(stack, res, JT::Ref);
        return Some(());
    }
    #[cfg(not(feature = "next-gen-gc"))]
    None
}
