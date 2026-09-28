//! The polymorphic inline cache: probe, hit, fill and the miss call.
//!
//! Re-exported from the parent module, so no call site outside it changed.


use crate::jit::*;

/// Kill switch for the dead-pair-slot zeroing below. DEFAULT ON. `RAVA_NO_PIC_GCFIX=1` turns it
/// off on its own; `RAVA_NO_EMIT_MORE=1` turns off this whole round of emission work. Read ONCE,
/// at EMIT time, and only consulted on a site that actually HAS a wide argument — so with the
/// switch set, and on every narrow site regardless, not one instruction differs from the previous
/// build: no extra value, no changed instruction numbering, nothing for a bit-exactness gate to
/// see.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn pic_gcfix_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_PIC_GCFIX").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Kill switch for the SHARED (pooled) call buffers below. DEFAULT ON. `RAVA_NO_SLOTPOOL=1` turns
/// it off on its own; `RAVA_NO_EMIT_MORE=1` turns off this whole round of emission work. Read
/// ONCE per process — a `OnceLock`, never a `var_os` on the per-emit path, which runs ~9,500
/// times a boot on the serial critical path — and consulted only at EMIT time, where it decides
/// codegen. With it set, every site mints its own slot again and the emission is the historic one
/// instruction for instruction.
///
/// It is deliberately a SECOND switch on top of `env::shared_callbuf_enabled()`
/// (`RAVA_NO_SHARED_CALLBUF`): that one is the whole-engine arm for the shared buffers, this one
/// bisects the PIC's four slots alone, so a regression can be attributed to a file rather than to
/// "the frame got smaller somewhere".
#[cfg(feature = "next-gen-gc")]
fn slotpool_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_SLOTPOOL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// MACHINE-CODE 4-WAY DISPATCH IC (next-gen-gc) for invokevirtual/invokeinterface
/// on an ARENA receiver. Per call site a leaked, zeroed `[(class_id i64, fptr i64);
/// 4]` cache (stable baked address). Machine code: read `class_id` from the arena
/// header `[recv+0]`, 4-way compare against the cached ids, and on a HIT
/// `call_indirect` the cached compiled callee DIRECTLY (no Rust runtime, no synthetic
/// frame). On a miss / non-arena receiver / empty slot → `rava_dispatch_miss`, which
/// resolves + runs the call AND fills a cache slot (so the next call with that class
/// hits). Only GC-LEAF callees are cached (see `dispatch_resolve_cacheable`), so the
/// HIT path never GCs → no spill/reload and the non-zeroed call buffer is never
/// scanned. The miss path threads ref operand-stack values through the merge (the
/// same SSA-diamond handling as `emit_inline_arena_new`). Returns `None` to bail to
/// `emit_invoke` (too many args). Wide (long/double) arguments ARE served: the buffer is laid out
/// in JVM SLOTS, each wide argument's dead pair slot is explicitly zeroed (it is inside a scanned
/// cell-root range — see the marshal loop), and the packed miss/native ABI gets its own copy built
/// in the cold arm. The caller's `wide_site_pic_ok` decides which wide OWNERS reach here at all.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_dispatch_pic(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    env: &Env,
    cp: u16,
    kind: u8,
    desc: &str,
    vars: &[Variable],
    locals_ptr: Value,
    array_desc_cell: Value,
    heap_mode: u8, // 0=none, 1=shadow-verify (23.4a), 2=live heap-Ref PIC (23.4b)
    pc: usize,
) -> Option<()> {
    // Per-arg JVM slot widths: receiver (1) + each param (long/double = 2). The
    // HIT path calls the compiled callee directly, and compiled bodies index
    // locals by JVM slot — so the call buffer is laid out in SLOT positions
    // (wide args leave their pair slot unused — and it is ZEROED below, because a
    // deep callee has that whole range scanned as GC roots). The MISS callback ABI is packed
    // (one cell per arg, see emit_invoke/jit_call), so a wide-arg site builds a
    // packed copy inside the cold miss arm.
    let widths = arg_slot_widths(desc);
    let nargs = widths.len();
    let slot_total: usize = widths.iter().sum();
    // ⭐**ONE READ OF THE CALL-BUFFER BUDGET FOR THE WHOLE SITE.** It was read FIVE times inside a
    // single emission — the bail below, the hit buffer's stack-slot size, and the packed-copy slot
    // in each of the arena-native, heap-Ref-native and miss arms — and it is a process constant (a
    // field of the latched `ic_gates()` word), so the five could never disagree. They were simply
    // five `OnceLock` acquire loads that LLVM may not hoist across the `FunctionBuilder` calls
    // between them, on ~9,500 sites a boot on the SERIAL critical path.
    let callbuf_slots = dispatch_callbuf_slots();
    if slot_total > callbuf_slots {
        return None; // bail → emit_invoke
    }
    // ⭐**"DOES THIS SITE CARRY A WIDE ARGUMENT" IS ONE FACT, DERIVED FOUR TIMES.** As
    // `slot_total != nargs` for the dead-pair-slot zeroing, then as `slot_total == nargs` in the
    // arena-native, heap-Ref-native and miss arms deciding whether a packed copy is needed. Same
    // comparison, same operands, four places — named once, here.
    let wide = slot_total != nargs;
    // The rest of the process-constant gates this emission consults, read ONCE past the bail:
    // `ic_deep_enabled()` (2 reads), `picprobe::enabled()` (3), `lambda_pic_enabled()` (3, and only
    // pre-read for `heap_mode == 2`, the only mode whose arms ask). `RAVA_NO_FAST_EMITDERIVE=1`
    // leaves every field `None` and each accessor re-derives at its original point of use.
    let gates = SiteGates::pic(callbuf_slots, heap_mode);
    // "Is the dead pair slot zeroed for this site" — ONE read of `pic_gcfix_enabled()`, past the
    // bail, because it is now asked TWICE: by the marshal loop that emits the store, and by the
    // pooling guard below, which may not share a buffer whose hole is left stale.
    let gcfix = wide && pic_gcfix_enabled();
    // ⭐**THE FRAME IS THE SUM OF THE CALL SITES, BECAUSE CRANELIFT DOES NOT COLOUR EXPLICIT
    // SLOTS.** This function mints FOUR of them per site — the slot-laid hit buffer plus a packed
    // copy in each of the arena-native, heap-Ref-native and miss arms — and `initCache` (21.1% of
    // all machine-code time in a boot, ~28k executions) dumps 101 explicit slots totalling 18,976
    // bytes with every one referenced EXACTLY ONCE. `Env::call_buf` / `Env::packed_buf` hand the
    // whole body ONE region of each kind instead, created lazily on first ask so a body with no
    // dispatch site still grows nothing.
    //
    // **WHY THE LIFETIMES CANNOT OVERLAP.** A buffer is live only from its argument stores,
    // through the call, to the result read. The emitter pops its arguments as SSA values, so a
    // nested `f(g(x))` has fully evaluated *and returned from* `g` before `f` stores its first
    // cell — buffer lifetimes are strictly sequential, never interleaved. Nothing here reads a
    // buffer after its call returns: the result comes back in a register (`inst_results`) and the
    // forwarded refs through `merge`'s block params / `locals_ptr`. A deep callee registers
    // `(buffer, frame_len)` as a cell-root RANGE and pops it on return, so reuse after the return
    // is exactly as safe as the same site's own second execution — which already reuses this
    // memory today (see `emit_deep_prologue`: "the dispatch IC calls a deep body DIRECTLY on a
    // REUSED call buffer").
    //
    // ⚠**REJECT-ONLY GUARD: A POOLED BUFFER IS ONLY AS SAFE AS ITS INITIALISATION.** Sharing does
    // not create a new class of hazard, it removes the accident that used to soften one. The
    // scanned range is `[0, slot_total)` (the callee's prologue zeroes `[args_slots, frame_len)`
    // itself) and the marshal loop writes every cell of it — EXCEPT a wide argument's dead pair
    // slot, which only `pic_gcfix_enabled()` fills. Left unwritten in a POOLED slot it holds the
    // previous occupant's real cells, i.e. a value with low bits `00` that decodes as a raw arena
    // pointer and that the collector will follow. So a wide site with the zeroing switched off
    // keeps its own private slot: the guard can only REJECT, and rejecting costs exactly the
    // historic emission. The packed copies have no hole at all (one cell per argument, `[0,
    // nargs)`, fully written in each arm before that arm's call, and the consumers read by
    // argument COUNT), so they pool unconditionally.
    let pool_packed = slotpool_enabled();
    let pool_call = pool_packed && (!wide || gcfix);
    // Pop receiver + args: cells[0] = receiver (deepest) … cells[nargs-1] = top.
    let mut cells: Vec<SV> = Vec::with_capacity(nargs);
    for _ in 0..nargs {
        cells.push(stack.pop()?);
    }
    cells.reverse();
    let recv = cells[0].v;
    // Does the METHOD-ENTRY UNSWITCH already prove this receiver is a raw arena pointer? If so the
    // `emit_is_arena` diamond below is statically true and the heap-Ref arm is dead code. Counted
    // BEFORE anything acts on it - coverage decides whether specialising on it is worth emitting.
    #[cfg(feature = "next-gen-gc")]
    if cells[0].arena {
        block_census::note(&block_census::PIC_ARENA_RECV);
    }

    // Per-site IC cache: 4 × (class_id, fptr), leaked + zeroed, address baked, followed by
    // ONE trailing word (`DISPATCH_TABLE_SLOT`) reserved for the direct-mapped dispatch table.
    // The word is allocated and zeroed here and read by nothing yet — see
    // docs/dispatch-direct-mapped-plan.md, increment 1 of 3, which is required to be
    // byte-identical (the gates prove it).
    //
    // It lives at the END of the GLOBAL site box on purpose. `IC_GLOBAL_BYTES` (64 B, the 4
    // ways) is what the per-thread seed copies, so the trailing word is never copied into a
    // worker's 16-way slice — where index 8 is a real PIC way, not a table pointer. Any future
    // read must therefore bake `cache_ptr + DISPATCH_TABLE_SLOT` and must NOT go through
    // `cache_eff`, or a worker would interpret its 5th way as a table pointer.
    block_census::note(&block_census::PIC);
    let blk_n0 = b.func.dfg.num_blocks() as u64;
    let inst_n0 = b.func.dfg.num_insts() as u64;
    let cache_ptr = Box::into_raw(Box::new([0i64; 10])) as i64;
    let dispatch_site =
        crate::jit::dispatch_diag::register_site(env.class_ptr, env.code_key, pc, cp);
    // 11.1: register this site + assign its fixed per-thread-table offset.
    let ic_offset = ic_site_alloc(cache_ptr as usize);

    // 11.3 — EFFECTIVE IC cache pointer (relative addressing). Computed in the ENTRY block so it
    // dominates BOTH the PIC hit-compare and the miss handler, which read/write through it.
    //   • gate OFF (default): the baked global `cache_ptr` — machine code + dispatch byte-identical.
    //   • gate ON: a runtime branch. `rava_ic_base()` returns this thread's per-thread table base,
    //     0 on the main thread / outside a region. A pool worker (base != 0) reads + writes its OWN
    //     slice at `base + ic_offset` (no shared torn-writes); the main thread keeps the global cache.
    // Passing `cache_eff` to `rava_dispatch_miss` means the miss handler fills the SAME slot the hit
    // path reads — so a worker's misses land in its private table, with no change to dispatch_miss.
    // Serial execution never spawns workers ⇒ base is always 0 ⇒ the global arm ⇒ identical hashes.
    // Under IC_PARALLEL we also retain (is_worker, global, pt) so the PIC probe can branch
    // into a 16-way worker arm vs the 4-way global arm (entry block dominates the pic block,
    // so these SSA values are usable there).
    let mut pic_arms: Option<(Value, Value, Value)> = None;
    let cache_eff = if ic_parallel_enabled() {
        let global = b.ins().iconst(I64, cache_ptr);
        let bcall = b.ins().call(env.ic_base_fn, &[]);
        let base = b.inst_results(bcall)[0];
        let is_worker = b.ins().icmp_imm(IntCC::NotEqual, base, 0);
        let pt = b.ins().iadd_imm(base, ic_offset as i64);
        pic_arms = Some((is_worker, global, pt));
        b.ins().select(is_worker, pt, global)
    } else {
        b.ins().iconst(I64, cache_ptr)
    };

    // Slot-laid-out call buffer (the hit path's callee locals; marshal once).
    // Pooled per FUNCTION (see the guard above). `Env::call_buf` is one slot wider than a site's
    // own — `shared_callbuf_bytes()` adds the guard word `emit_lambda_shift_adjust` needs when it
    // hands the callee `bufaddr + 8` — so a shift arm no longer overhangs into whatever sat next
    // in the frame. The cold arm is the historic four lines verbatim, so the block prices as one
    // arm; `Env::call_buf` itself falls back to a private, budget-sized slot under
    // `RAVA_NO_SHARED_CALLBUF`.
    let callbuf = if pool_call {
        env.call_buf(b)
    } else {
        b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            gates.callbuf_bytes(),
            3,
        ))
    };
    {
        // ⚠**THE DEAD PAIR SLOT IS SCANNED — IT IS A GC ROOT, NOT PADDING.** A `deep` (self-
        // registering) callee pushes `(buffer, frame_len)` onto the thread's cell-root stack and
        // the collector walks that range BY RANGE, with no liveness map. The v3 prologue zeroes
        // only `[args_slots, frame_len)`; a wide argument's pair slot sits BELOW `args_slots`, so
        // it is scanned holding whatever the PREVIOUS occupant of this stack slot left there —
        // and an arena cell with low bits `00` reads as a raw arena POINTER, so the collector
        // follows garbage. This is the same hazard `emit_direct_mono` closed on the static/special
        // side; `emit_dispatch_pic` has exactly the same slot-laid buffer and had not.
        //
        // The unified arena and heap hit arms may hand `bufaddr` to a self-registering
        // compiled body. GC-leaf bodies do not scan it, while `miss` and native thunks
        // read a PACKED copy by argument count. `zero` is materialised only when a hole
        // exists, so a narrow site emits no extra instruction.
        //
        // ⚠**AND WITH A POOLED BUFFER THIS STORE IS NOT OPTIONAL — WHICH IS WHY `pool_call`
        // DEPENDS ON IT.** A private slot's hole holds whatever Cranelift's frame layout left
        // there; a SHARED one holds the previous site's live cell at that offset, which is a real
        // arena pointer far more often than frame residue is. Turning the zeroing off therefore
        // costs the site its share of the pooled buffer, not its safety: `gcfix` false ⇒
        // `pool_call` false ⇒ this site is back on its own slot, exactly as before.
        #[cfg(feature = "next-gen-gc")]
        let zero = if gcfix {
            Some(b.ins().iconst(I64, 0))
        } else {
            None
        };
        let mut off = 0i32;
        for (i, sv) in cells.iter().enumerate() {
            let c = to_cell(b, *sv);
            b.ins().stack_store(c, callbuf, off * 8);
            #[cfg(feature = "next-gen-gc")]
            if widths[i] == 2 {
                if let Some(z) = zero {
                    b.ins().stack_store(z, callbuf, (off + 1) * 8);
                }
            }
            off += widths[i] as i32;
        }
    }
    let bufaddr = b.ins().stack_addr(I64, callbuf, 0);
    // The UNADJUSTED call-buffer address. `emit_lambda_shift_adjust` below REBINDS `bufaddr` to a
    // select between it and `bufaddr + 8` when the λ-shift arm is emitted, and the heap-Ref native
    // arm added further down needs the buffer's true base (a thunk takes the PACKED cells starting
    // at the receiver, never a shifted frame). A plain Rust binding — no IR, so nothing moves.
    let bufaddr_raw = bufaddr;

    // Ref operand-stack values remaining below the args thread through the merge.
    let ref_slots: Vec<usize> = stack
        .iter()
        .enumerate()
        .filter(|(_, sv)| sv.t == JT::Ref)
        .map(|(i, _)| i)
        .collect();
    let nref = ref_slots.len();

    let pic = b.create_block();
    let hit = b.create_block();
    let miss = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64); // result bits
    for _ in 0..nref {
        b.append_block_param(merge, I64);
    }
    // 23.4b live heap-Ref PIC blocks (only for mode==2). `refpic` compares the
    // heap class key; `refhit` executes the unified rooted call and rejoins `merge`.
    #[cfg(feature = "next-gen-gc")]
    let (refpic, refhit) = if heap_mode == 2 {
        (Some(b.create_block()), Some(b.create_block()))
    } else {
        (None, None)
    };

    // 23.4a heap-Ref PIC SHADOW (inert, mode==1, DiscreteVoxelShape sites): read the receiver's class
    // key in MACHINE CODE the exact way the live PIC will, and hand it to the Rust shadow to verify vs
    // the authoritative Object.class word. Only fires for a heap Ref (recv>>40 == 1). Dispatch UNCHANGED.
    #[cfg(feature = "next-gen-gc")]
    if heap_mode == 1 {
        let sblk = b.create_block();
        let cont = b.create_block();
        let hi = b.ins().ushr_imm(recv, 40);
        let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1); // REF_TAG=1<<40 uniquely (BOX/ARRAY/ABOX=2/4/8)
        b.ins().brif(is_ref, sblk, &[], cont, &[]);
        b.switch_to_block(sblk);
        b.seal_block(sblk);
        let key = emit_heap_ref_class_key(b, array_desc_cell, recv, cont);
        b.ins().call(env.heap_shadow_fn, &[recv, key]);
        b.ins().jump(cont, &[]);
        b.switch_to_block(cont);
        b.seal_block(cont);
    }

    let is_a = emit_is_arena(b, recv);
    // 23.4b: live mode routes a NON-arena receiver to the heap-Ref PIC (if it is a heap Ref), instead of
    // straight to the miss. Arena receivers keep the existing arena PIC. Modes 0/1 are unchanged.
    #[cfg(feature = "next-gen-gc")]
    if heap_mode == 2 {
        let refcheck = b.create_block();
        b.ins().brif(is_a, pic, &[], refcheck, &[]);
        b.switch_to_block(refcheck);
        b.seal_block(refcheck);
        let hi = b.ins().ushr_imm(recv, 40);
        let is_ref = b.ins().icmp_imm(IntCC::Equal, hi, 1);
        b.ins().brif(is_ref, refpic.unwrap(), &[], miss, &[]);
    } else {
        b.ins().brif(is_a, pic, &[], miss, &[]);
    }
    #[cfg(not(feature = "next-gen-gc"))]
    b.ins().brif(is_a, pic, &[], miss, &[]);

    // PIC: class_id from the arena header, N-way compare → fptr (0 = no hit).
    b.switch_to_block(pic);
    b.seal_block(pic);
    let cid32 = b.ins().load(I32, MemFlags::trusted(), recv, Offset32::new(0));
    let cid = b.ins().uextend(I64, cid32);
    // The lambda table key is computed INSIDE `emit_table_probe`, not here. Computing it before
    // the 4-way PIC probe put a bitmap load, a receiver load and a select on EVERY dispatch, and
    // the table is only consulted when the PIC has already missed: measured at **ChunkFill 4,589
    // ms → 60,282 ms** with the admission half disabled, so the whole cost was this placement.
    // N-way select chain over `cache` — shared by both arms below.
    let probe_ways = |b: &mut FunctionBuilder, cache: Value, ways: i32| -> Value {
        let mut fptr = b.ins().iconst(I64, 0);
        for k in 0..ways {
            let id_k = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(k * 16));
            let fp_k = b.ins().load(I64, MemFlags::trusted(), cache, Offset32::new(k * 16 + 8));
            let id_eq = b.ins().icmp(IntCC::Equal, id_k, cid);
            // fp > 0: a real entry. -1 is the NEGATIVE entry (class proven
            // never-cacheable) — not a hit; dispatch_miss sees it and skips the
            // per-call resolve.
            let fp_ok = b.ins().icmp_imm(IntCC::SignedGreaterThan, fp_k, 0);
            let hit_k = b.ins().band(id_eq, fp_ok);
            fptr = b.ins().select(hit_k, fp_k, fptr);
        }
        fptr
    };
    let fptr = if let Some((is_worker, global, pt)) = pic_arms {
        // TWO-ARM probe (IC_PARALLEL): worker → 16-way over its per-thread slice (megamorphic
        // sites stay machcode-resident); main → the unchanged 4-way global.
        let gprobe = b.create_block();
        let wprobe = b.create_block();
        let pmerge = b.create_block();
        b.append_block_param(pmerge, I64);
        b.ins().brif(is_worker, wprobe, &[], gprobe, &[]);
        b.switch_to_block(gprobe);
        b.seal_block(gprobe);
        let fg = probe_ways(b, global, 4);
        let fg_args: [cranelift::codegen::ir::BlockArg; 1] = [fg.into()];
        b.ins().jump(pmerge, &fg_args);
        b.switch_to_block(wprobe);
        b.seal_block(wprobe);
        let fw = probe_ways(b, pt, IC_WORKER_WAYS as i32);
        let fw_args: [cranelift::codegen::ir::BlockArg; 1] = [fw.into()];
        b.ins().jump(pmerge, &fw_args);
        b.switch_to_block(pmerge);
        b.seal_block(pmerge);
        b.block_params(pmerge)[0]
    } else {
        probe_ways(b, cache_eff, 4)
    };
    #[cfg(feature = "next-gen-gc")]
    let fptr = emit_table_probe(b, fptr, cid, recv, cache_ptr, hit);
    let have = b.ins().icmp_imm(IntCC::NotEqual, fptr, 0);
    b.ins().brif(have, hit, &[], miss, &[]);
    // SHIFT-CAPABLE ARENA HIT (`RAVA_LAMBDA_SITE_CID`, gated at EMIT time so the default path's
    // codegen is byte-identical). Bit 1 of a cached fptr means the callee takes `raw[1..]` — the
    // lambda convention the heap-Ref arms have served since `emit_lambda_shift_adjust`. Without
    // it `dispatch_table::note_serve_direct` must refuse every `shift` entry, and the counter
    // says that is **19,492,939 of 19,492,939 serves** (ledger row 332): the table's entire
    // would-be population.
    #[cfg(feature = "next-gen-gc")]
    let shift_arm = crate::nextgen::lambdas::site_cid_enabled();

    // HIT: an entry exists. Low bit selects the arm: 0 = GC-LEAF callee (direct
    // call, no GC possible, no spill), 1 = DEEP callee (non-leaf compiled body —
    // spill, zero the buffer tail, register the buffer as a GC root, call).
    b.switch_to_block(hit);
    b.seal_block(hit);
    // 23.2 PIC-coverage probe (gated at emit-time, inert): this block is entered ONLY on a machine-code
    // PIC hit (resolved with no Rust callback). Count it inline — load/add/store to a baked global
    // address. Side-effect-only ⇒ hashes bit-exact; probe runs are single-threaded ⇒ no torn write.
    if gates.probe() {
        let a = b.ins().iconst(I64, crate::picprobe::pic_hit_addr());
        let cur = b.ins().load(I64, MemFlags::trusted(), a, Offset32::new(0));
        let inc = b.ins().iadd_imm(cur, 1);
        b.ins().store(MemFlags::trusted(), inc, a, Offset32::new(0));
    }
    // TYPED NATIVE ENTRY SPLIT (`RAVA_NO_PIC_NATIVE=1` disables; the gate is read at EMIT time,
    // so with it set not one instruction below is emitted and the guest arms are byte-identical).
    //
    // ~19 M of a boot's ~35.1 M PIC misses are a `java/*` receiver with no bytecode — a call into
    // this engine's Rust java.util. `dispatch_miss_resolve` already records that verdict as the
    // PERMANENT `-1` in this very cache, and the probe above already found it and already threw
    // it away on `fp > 0`. `pic_native` replaces that `-1` with a descriptor address, so the same
    // four compares that miss today now HIT, and this branch sends the hit to a Rust thunk
    // instead of out through `rava_dispatch_miss` → the negative-cache probe → `jit_call` →
    // `try_invoke_chain` → `dispatch_owner_cascade`.
    //
    // The test is a RANGE, not a tag bit: descriptors are carved from one leaked fixed pool, so
    // `desc - base <u span` identifies them with an `isub` and one unsigned compare and cannot
    // alias a code address (a Rust heap allocation and the JIT's executable mapping are
    // disjoint). A tag bit would need bit 2 of every compiled fptr clear, and 8-byte alignment —
    // all this tree claims — is exactly what leaves bit 2 unowned. `t` is also passed to the
    // optional census, which classifies descriptor regions without adding control flow.
    //
    // Ordering matters: this branch comes BEFORE the λ-shift decode below, which masks with -4
    // and would corrupt a descriptor pointer. The guest arms then continue in `guest_hit`,
    // reading the same dominating `fptr` they read before.
    //
    // ⭐**THE DESCRIPTOR POOL'S BASE AND SPAN ARE TWO FACTS, DERIVED TWICE EACH PER SITE.** The
    // same `(base, span)` pair is baked into the arena arm's range test here and into the heap-Ref
    // arm's identical range test ~150 lines below, and each derivation is a `OnceLock` read
    // (`pool_base`) plus, inside `pool_span`, ANOTHER `OnceLock` read of `ref_enabled()` — so the
    // pair cost 2 + 2 + 2 = six process-constant reads per emitted site. Memoised here in the
    // arm that is emitted FIRST and threaded down; the two `iconst`s stay where they are (they
    // live in different, non-dominating blocks), so the emitted machine code is unchanged.
    let mut nat_pool: Option<(i64, i64)> = None;
    #[cfg(feature = "next-gen-gc")]
    if crate::jit::pic_native::enabled() {
        let nat_blk = b.create_block();
        let guest_hit = b.create_block();
        let pool = (
            crate::jit::pic_native::pool_base() as i64,
            crate::jit::pic_native::pool_span() as i64,
        );
        if fast_emitderive_enabled() {
            nat_pool = Some(pool);
        }
        let lo = b.ins().iconst(I64, pool.0);
        let t = b.ins().isub(fptr, lo);
        let is_nat = b.ins().icmp_imm(IntCC::UnsignedLessThan, t, pool.1);
        // CENSUS ONLY (`[CALLSEQ]`, default OFF). Counts range-test outcomes and
        // descriptor class branchlessly; generated calls use one rooted arm for
        // both classes. With the gate off this emits nothing. See `jit::callseq`.
        crate::jit::callseq::note_site(b, env.class_ptr, cp, kind, heap_mode, t, is_nat);
        b.ins().brif(is_nat, nat_blk, &[], guest_hit, &[]);

        b.switch_to_block(nat_blk);
        b.seal_block(nat_blk);
        // The thunks take the PACKED cells (one per argument) — the ABI `jit_call` and the `ws_*`
        // arms read — while `bufaddr` is laid out in JVM SLOTS for the compiled-callee hit arm.
        // The two coincide unless the site has a wide argument, and only then is a copy built,
        // in this arm alone. `miss` keeps building its own, so its codegen is untouched.
        //
        // Pooled per FUNCTION: this arm, the heap-Ref native arm and `miss` all take the SAME
        // packed region (`Env::packed_buf`), which is a DIFFERENT region from the slot-laid hit
        // buffer — a wide site has both addresses live across the `is_nat` branch. The three arms
        // are mutually exclusive and each writes all `nargs` cells immediately before its own
        // call, so a REFUSAL falling from here into `miss` is safe for the same reason it always
        // was: no GC has happened yet (a leaf thunk cannot collect, a rooted one may refuse only
        // from its prologue), the cells are the same SSA values, and `miss` rewrites the whole
        // range before it calls.
        let natbuf = if !wide {
            bufaddr
        } else if pool_packed {
            let packed = env.packed_buf(b);
            for (i, sv) in cells.iter().enumerate() {
                let c = to_cell(b, *sv);
                b.ins().stack_store(c, packed, (i as i32) * 8);
            }
            b.ins().stack_addr(I64, packed, 0)
        } else {
            let packed = b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                gates.callbuf_bytes(),
                3,
            ));
            for (i, sv) in cells.iter().enumerate() {
                let c = to_cell(b, *sv);
                b.ins().stack_store(c, packed, (i as i32) * 8);
            }
            b.ins().stack_addr(I64, packed, 0)
        };
        crate::jit::emit::pic_helpers::emit_pic_native_arms(
            b,
            fptr,
            natbuf,
            vars,
            stack,
            &ref_slots,
            locals_ptr,
            merge,
            miss,
            // `t` (the same offset the range test's `isub` produced) reaches the arms only
            // when the leaf/rooted split is on; `None` emits the unified rooted arm verbatim.
            crate::jit::pic_native::leaf_fastcall_enabled().then_some(t),
        );

        b.switch_to_block(guest_hit);
        b.seal_block(guest_hit);
    }
    // ONE import of the call-buffer ABI for the whole FUNCTION, not one per PIC site. `(i64, i64)
    // -> i64` in the caller's own call conv has no site-dependent part — `initCache` imported 111
    // identical copies of it — and the `SigRef` handed back is validated against THIS function's
    // signature table before reuse, so it can only ever be the signature these four `call_indirect`
    // arms already asked for. See `calls::callbuf_abi_sigref` for the reject-only argument.
    let sigref = callbuf_abi_sigref(b);
    // Decode bit 1 (and mask both marker bits) only when shift entries can exist.
    #[cfg(feature = "next-gen-gc")]
    let (fptr, bufaddr) = if shift_arm {
        crate::jit::emit::pic_helpers::emit_lambda_shift_adjust(b, fptr, bufaddr)
    } else {
        (fptr, bufaddr)
    };
    // Every cached guest target uses one rooted hit arm. `emit_lambda_shift_adjust`
    // has already removed both lambda marker bits; ordinary entries only carry
    // the low deep bit.
    let fclean = if shift_arm { fptr } else { b.ins().band_imm(fptr, -2i64) };
    spill_for_gc(b, vars, stack, locals_ptr);
    let call = b.ins().call_indirect(sigref, fclean, &[bufaddr, array_desc_cell]);
    let bits = b.inst_results(call)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut args: Vec<cranelift::codegen::ir::BlockArg> = vec![bits.into()];
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        args.push(rv.into());
    }
    b.ins().jump(merge, &args);

    // 23.4b LIVE heap-Ref PIC HIT (mode==2). `refpic` compares the receiver's
    // machine-code class key against the site cache. HIT enters the unified
    // rooted call with the Ref receiver in callbuf[0]; MISS enters the shared
    // resolver below.
    #[cfg(feature = "next-gen-gc")]
    if heap_mode == 2 {
        let refpic = refpic.unwrap();
        let refhit = refhit.unwrap();
        b.switch_to_block(refpic);
        b.seal_block(refpic);
        let key = emit_heap_ref_class_key(b, array_desc_cell, recv, miss);
        // λ-PIC (gated at COMPILE time, default OFF ⇒ byte-identical codegen): a
        // "$Lambda" class word probes by receiver IDENTITY instead (fills come from
        // Vm::lambda_pic_fill — immortal per-indy-site singletons only, see there).
        let key = if gates.lambda_pic() {
            emit_lambda_key_select(b, recv, key)
        } else {
            key
        };
        let cache = cache_eff;
        let fptr_r = emit_refpic_probe(b, cache, key);
        let have_r = b.ins().icmp_imm(IntCC::NotEqual, fptr_r, 0);
        // TYPED NATIVE ENTRY, HEAP-REF HALF (`RAVA_NO_EMIT_PICNAT2=1` disables; the gate is read
        // at EMIT time, so with it set `ref_nat` is false, not one instruction below is emitted,
        // and the guest arms keep their original `brif(have_r, ..)` byte for byte).
        //
        // The arena arm has bound its `-1` entries since `pic_native` landed; the heap-Ref arm
        // probes a SECOND set of negative entries that nothing bound. `heappic_fill` writes
        // `(class_word, -1)` for a class its own resolve proved NeverCacheable, the four compares
        // above find it, `fp > 0` throws it away, and the call leaves machine code for
        // `rava_dispatch_miss` → `dispatch_miss_resolve` → `heappic_fill`'s negative probe →
        // `jit_call` → `try_invoke_chain` → `dispatch_owner_cascade` — to re-derive a verdict this
        // very cache already holds. `heappic_fill`'s own census prices what it can see at
        // **8.39 M of 9.06 M being `isEmpty` on four fastutil collections**, plus the 19.4 M its
        // fast negative bail never records (which `try_fill_ref` binds off a free way instead).
        //
        // The guard is the same range test as the arena arm: the cached word is
        // a descriptor from `pic_native`'s leaked pool, not compiled code.
        // `desc - base <u span` cannot alias a code address because the Rust heap
        // and JIT executable mapping are disjoint.
        //
        // ⚠**ORDER IS LOAD-BEARING.** This sits BEFORE the deep/λ-shift decode below, which masks
        // with -2/-4 and would corrupt a descriptor pointer. The cold arm on a refusal is the
        // site's own `miss` block — byte-identical to the path the site takes today.
        let ref_nat = if crate::jit::pic_native::ref_enabled() {
            let natcheck = b.create_block();
            let natblk = b.create_block();
            let guest = b.create_block();
            b.ins().brif(have_r, natcheck, &[], miss, &[]);

            b.switch_to_block(natcheck);
            b.seal_block(natcheck);
            // Same `(base, span)` the arena arm baked. `ref_enabled()` is `enabled() && …`, so
            // reaching here PROVES `pic_native::enabled()` was true and the arena arm above
            // already derived the pair — the fallback exists only so this block never depends on
            // that implication for correctness, and the `debug_assert` proves it holds. The two
            // remembered `i64`s are process constants; the `iconst` below is emitted exactly where
            // it was, so the range test's machine code is byte-identical.
            debug_assert!(nat_pool.is_some() || !fast_emitderive_enabled());
            let (nat_lo, nat_span) = nat_pool.unwrap_or_else(|| {
                (
                    crate::jit::pic_native::pool_base() as i64,
                    crate::jit::pic_native::pool_span() as i64,
                )
            });
            let lo = b.ins().iconst(I64, nat_lo);
            let t = b.ins().isub(fptr_r, lo);
            let is_nat = b.ins().icmp_imm(IntCC::UnsignedLessThan, t, nat_span);
            b.ins().brif(is_nat, natblk, &[], guest, &[]);

            b.switch_to_block(natblk);
            b.seal_block(natblk);
            // The thunks take the PACKED cells (one per argument) — the ABI `jit_call` and the
            // `ws_*` arms read — while the hit buffer is laid out in JVM SLOTS for the compiled
            // callee. They coincide unless the site has a wide argument, and only then is a copy
            // built, in this arm alone. `miss` and the arena native arm keep building their own,
            // so neither one's codegen moves.
            // Pooled per FUNCTION, the same region the arena native arm and `miss` use — see the
            // arena arm for why three mutually exclusive arms may share one packed buffer.
            let natbuf = if !wide {
                bufaddr_raw
            } else if pool_packed {
                let packed = env.packed_buf(b);
                for (i, sv) in cells.iter().enumerate() {
                    let c = to_cell(b, *sv);
                    b.ins().stack_store(c, packed, (i as i32) * 8);
                }
                b.ins().stack_addr(I64, packed, 0)
            } else {
                let packed = b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    gates.callbuf_bytes(),
                    3,
                ));
                for (i, sv) in cells.iter().enumerate() {
                    let c = to_cell(b, *sv);
                    b.ins().stack_store(c, packed, (i as i32) * 8);
                }
                b.ins().stack_addr(I64, packed, 0)
            };
            crate::jit::emit::pic_helpers::emit_pic_native_arms(
                b,
                fptr_r,
                natbuf,
                vars,
                stack,
                &ref_slots,
                locals_ptr,
                merge,
                miss,
                // Same split as the arena arm; `t` is this arm's own `isub` product.
                crate::jit::pic_native::leaf_fastcall_enabled().then_some(t),
            );

            b.switch_to_block(guest);
            b.seal_block(guest);
            true
        } else {
            false
        };
        // The native probe has already consumed `have_r` when present; otherwise
        // the heap cache miss still branches to the shared resolver.
        if ref_nat {
            b.ins().jump(refhit, &[]);
        } else {
            b.ins().brif(have_r, refhit, &[], miss, &[]);
        }

        b.switch_to_block(refhit);
        b.seal_block(refhit);
        if gates.probe() {
            let a = b.ins().iconst(I64, crate::picprobe::pic_hit_addr());
            let cur = b.ins().load(I64, MemFlags::trusted(), a, Offset32::new(0));
            let inc = b.ins().iadd_imm(cur, 1);
            b.ins().store(MemFlags::trusted(), inc, a, Offset32::new(0));
        }
        let (call_fp, call_buf) = if gates.lambda_pic() {
            emit_lambda_shift_adjust(b, fptr_r, bufaddr)
        } else {
            (b.ins().band_imm(fptr_r, -2i64), bufaddr)
        };
        spill_for_gc(b, vars, stack, locals_ptr);
        let call = b.ins().call_indirect(sigref, call_fp, &[call_buf, array_desc_cell]);
        let bits = b.inst_results(call)[0];
        reload_locals_after_gc(b, vars, locals_ptr);
        let mut args: Vec<cranelift::codegen::ir::BlockArg> = vec![bits.into()];
        for &i in &ref_slots {
            let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
            args.push(rv.into());
        }
        b.ins().jump(merge, &args);
    }

    // MISS: general dispatch via the callback (also fills the cache). The
    // callback ABI is PACKED (one cell per arg) — identical to the slot buffer
    // when no arg is wide; a wide-arg site re-marshals here (cold arm only).
    b.switch_to_block(miss);
    b.seal_block(miss);
    spill_for_gc(b, vars, stack, locals_ptr);
    // Pooled per FUNCTION, the same region the two native arms use. `rava_dispatch_miss` is handed
    // `nv = nargs` and roots exactly `[0, nargs)`, which this loop writes in full — a pooled slot
    // cannot hand it a stale cell inside the scanned range.
    let miss_buf = if !wide {
        bufaddr
    } else if pool_packed {
        let packed = env.packed_buf(b);
        for (i, sv) in cells.iter().enumerate() {
            let c = to_cell(b, *sv);
            b.ins().stack_store(c, packed, (i as i32) * 8);
        }
        b.ins().stack_addr(I64, packed, 0)
    } else {
        let packed = b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            gates.callbuf_bytes(),
            3,
        ));
        for (i, sv) in cells.iter().enumerate() {
            let c = to_cell(b, *sv);
            b.ins().stack_store(c, packed, (i as i32) * 8);
        }
        b.ins().stack_addr(I64, packed, 0)
    };
    let cachev = cache_eff; // 11.3: miss fills the SAME slot the hit path reads (per-thread on a worker)
    let callerv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind as i64);
    let nv = b.ins().iconst(I64, nargs as i64);
    let dispatch_site_v = b.ins().iconst(I64, dispatch_site);
    let mcall = b.ins().call(
        env.dispatch_miss,
        &[cachev, recv, callerv, cpv, kindv, miss_buf, nv, dispatch_site_v],
    );
    let miss_bits = b.inst_results(mcall)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut miss_args: Vec<cranelift::codegen::ir::BlockArg> = vec![miss_bits.into()];
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        miss_args.push(rv.into());
    }
    b.ins().jump(merge, &miss_args);

    // MERGE: adopt forwarded ref-stack values, decode + push the result.
    b.switch_to_block(merge);
    b.seal_block(merge);
    let mparams: Vec<Value> = b.block_params(merge).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = mparams[1 + k];
    }
    if let Some(jt) = ret_full(desc) {
        let sv = cell_to_sv(b, mparams[0], jt);
        stack.push(sv);
    }
    // Blocks this site ACTUALLY emitted in THIS build configuration - the honest number, as
    // opposed to multiplying invocations by the static create_block() count, which counts arms
    // that are behind flags that are off. Only the successful path is counted; an early `?`
    // bail never emitted the diamond.
    block_census::add_blocks(
        &block_census::PIC_BLOCKS,
        (b.func.dfg.num_blocks() as u64).saturating_sub(blk_n0),
    );
    block_census::add_clif_insts(
        (b.func.dfg.num_insts() as u64).saturating_sub(inst_n0),
    );
    Some(())
}
