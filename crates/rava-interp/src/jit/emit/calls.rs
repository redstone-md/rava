//! Call sites: the register-ABI static call, the monomorphic direct call, and the generic paths.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Kill switch for the wide-argument widening of `emit_direct_mono` (see its doc comment).
/// DEFAULT ON. `RAVA_NO_EMIT_STATIC=1` turns it off on its own; `RAVA_NO_EMIT_MORE=1` turns off
/// this whole round of emission work. Read ONCE, at EMIT time, so an off build emits the exact
/// instruction sequence the previous build did — the refusal happens before any IR is created,
/// so there is no dead block, no extra stack slot and no changed block numbering to perturb a
/// bit-exactness gate.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_static_wide_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_EMIT_STATIC").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE CALL BUFFER IS THE FRAME. POOL IT.** (`RAVA_NO_SLOTPOOL=1` restores the per-site slot;
/// `RAVA_NO_EMIT_MORE=1` disables the whole round.) DEFAULT ON, read ONCE through a `OnceLock` at
/// EMIT time — it decides codegen, and nothing in compiled code ever reads it.
///
/// **THE NUMBER THAT AIMED THIS.** A profile of a real boot puts 21.1% of ALL machine-code time in
/// one guest method, `BlockBehaviour$BlockStateBase.initCache`. Its CLIF dump is 2,068 real
/// instructions and **101 `explicit_slot`s totalling 18,976 bytes of stack frame, every slot
/// referenced EXACTLY ONCE**, with a load histogram of +0/+8/+16/…/+56 decaying by offset — that is
/// not field access, that is argument marshalling through memory, ~6 loads per call across 111
/// calls. Cranelift does NOT colour explicit slots, so a method's frame is the SUM of every call
/// site's buffer: `dispatch_callbuf_slots() * 8` bytes per site, multiplied by the site count.
///
/// **WHY ONE BUFFER SERVES THEM ALL, argued for THIS emitter and not assumed.** A call buffer is
/// live from its first argument store to the call that consumes it, and dead the instant that call
/// returns. In `emit_direct_mono` that window contains no nested emission at all:
///   * the arguments are POPPED OFF `stack` INTO SSA VALUES (`cells`) before a single store lands.
///     Everything that produced them — including nested calls, which are their own complete
///     emissions with their own buffer windows — has already finished. `f(g(x))` evaluates `g`
///     to an SSA value first; there is no shape in which `g`'s emission runs between `f`'s stores
///     and `f`'s call, because `f` has not been reached yet when `g` is emitted;
///   * between the store loop and the two consuming calls the emitter runs only `stack_addr`,
///     `iconst`, block creation/sealing, the `picprobe` counter (a load/add/store against a
///     GLOBAL address, never the buffer), `import_signature`, and `spill_for_gc` /
///     `reload_locals_after_gc` — both of which touch `locals_ptr` ONLY (see `gcroots.rs`: they
///     emit stores and loads at `locals_ptr` offsets and no call whatsoever);
///   * nothing reads the buffer AFTER its call: the result comes from `inst_results`, the surviving
///     ref operands come back through `locals_ptr` and the merge block's parameters;
///   * the callee runs in ITS OWN frame with ITS OWN slots, so recursion and re-entrancy cannot
///     alias this one, and `rava_direct_miss` reads exactly `n` cells
///     (`slice::from_raw_parts(buf, n)`) for the duration of one call and keeps no pointer;
///   * an INLINED callee body inherits this `Env` through `..*env` (see `inline.rs`), which is
///     the RIGHT answer and not an accident — it is the same Cranelift function, hence the same
///     frame, hence the same buffer with the same sequential discipline.
/// The precedent is already in the tree and years old: `env.args_slot` is one function-wide
/// marshalling buffer shared by `emit_invoke`, the collection intrinsics, `lambda_mint` and
/// `invoke_dynamic` under exactly this argument.
///
/// ⚠**AND WHY A POOLED BUFFER IS NOT A GC HAZARD, which is the half that is not obvious.** A
/// `deep` (self-registering) callee pushes `(buffer, frame_len)` onto the thread's cell-root stack
/// and the collector walks that range BY RANGE, with no liveness map — so a pooled slot, which
/// holds the PREVIOUS call site's bytes rather than fresh stack garbage, would be a live
/// resurrect-or-forward hazard if any of the scanned range were left unwritten. It is not, and the
/// cover is a property of the CALLEE'S OWN FRAME rather than of who used the memory last:
///   * this emitter writes `[0, slot_total)` IN FULL — one store per argument plus the explicit
///     zero store into every wide argument's dead pair slot (see the store loop below);
///   * `slot_total` IS the callee's `args_slots` (`vm/jit_run.rs` and `vm/selfreg.rs` both build
///     `DeepShape { args_slots: desc_args_slots(descriptor, is_static) }` from the same
///     descriptor this site walked), and the v3 prologue zeroes `[args_slots, frame_len)`
///     unconditionally on entry (`prologue.rs`);
///   * the two ranges abut and together cover `[0, frame_len)`, the whole scanned span.
/// `prologue.rs` states the same invariant from the other end, in as many words: dropping that
/// zero loop is wrong precisely because "the dispatch IC calls a deep body DIRECTLY on a REUSED
/// call buffer". Reuse was always the contract; this gate only makes it happen more often.
/// A GC-leaf callee registers no range at all, and every return path — ordinary return, the
/// latched-fault refusal — runs `emit_deep_epilogue`, so a registration cannot outlive its call.
///
/// **WHAT IS DELIBERATELY NOT POOLED.** Any slot whose ADDRESS outlives its call: the capture
/// buffers in `cap_thunk.rs` are a lambda's environment, handed to a thunk that keeps it. Those
/// are not created here and are not touched.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn slotpool_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_SLOTPOOL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Kill switch for THIS ROUND's emit-time fact threading (`RAVA_NO_FAST_EMITDERIVE=1` restores the
/// old shape; `RAVA_NO_EMIT_MORE=1` disables the whole round of emission work). DEFAULT ON.
///
/// ⚠**NOTHING ABOUT THE EMITTED MACHINE CODE DEPENDS ON IT.** Every gate `SiteGates` carries is a
/// PROCESS CONSTANT — `dispatch_callbuf_slots`, `ic_deep_enabled`, `lambda_pic_enabled` are bits of
/// the one `ic_gates()` word latched before the first site compiles, and `picprobe::enabled()` is a
/// `OnceLock<bool>` — so a memo and a re-read return the same answer, in the same order, producing
/// the same IR. What the switch changes is how many times ONE call-site emission ASKS. Off ⇒ every
/// field is `None` ⇒ each accessor falls through to the original expression at the original place,
/// which is exactly what an A/B of the SHAPE (and nothing else) needs.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fast_emitderive_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // MEASURED OPT-IN (round 31) -- see `heap::fields` HG_NODERIVE for the numbers.
        std::env::var_os("RAVA_FAST_EMITDERIVE").is_some() && !crate::flags::emit_more_disabled()
    })
}

/// Kill switch for THIS ROUND's call-sequence collapse. DEFAULT ON; `RAVA_NO_CALLSEQ=1` restores
/// the historic emission on its own, and `RAVA_NO_EMIT_MORE=1` turns off the whole round. Read
/// ONCE through a `OnceLock` — `emit_dispatch_pic` alone runs ~9,500 times a boot on the serial
/// critical path and `env::var_os` is a linear environment-block walk on Windows — and consulted
/// only at EMIT time. No compiled instruction reads it; it does not even decide codegen, only how
/// many times the emitter re-derives a constant it already has.
///
/// Deliberately NOT `crate::jit::callseq::enabled()`: that one is the CENSUS gate and is default
/// OFF (it additionally wants `RAVA_JIT_CALLSEQ` or an armed `picprobe`). The two share the same
/// `RAVA_NO_CALLSEQ` off-switch so one variable disarms the measurement and the change together.
#[cfg(feature = "next-gen-gc")]
fn callseq_emit_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // Per-area switch -- see the note in `jit::emit::body`. `RAVA_NO_CALLSEQ` disables everything.
        // MEASURED OPT-IN -- see the note in `jit::emit::body` for the numbers. Alone it read
        // +1.81% / +3.15%; the 7-round pair ABBA says -1.84% / -1.21%. `RAVA_FAST_CALLSEQ_EMIT=1`.
        std::env::var_os("RAVA_NO_CALLSEQ").is_none()
            && std::env::var_os("RAVA_FAST_CALLSEQ_EMIT").is_some()
            && !crate::flags::emit_more_disabled()
    })
}

/// The current function's shared `(i64, i64) -> i64` call-buffer ABI `SigRef`, or `None` outside a
/// body this emitter has already imported it for.
///
/// ⚠**A `SigRef` IS AN INDEX INTO ONE FUNCTION'S SIGNATURE TABLE**, exactly like the `StackSlot`
/// that `body::scratch_out_slot` pools, so a value minted while emitting function A is meaningless
/// in function B. `scratch_out_slot` closes that by clearing its cell at both ends of `build_body`;
/// this one cannot borrow that hook (it lives in a file this round does not own), so it VALIDATES
/// instead of trusting — see `callbuf_abi_sigref`. Validation is strictly stronger than a reset:
/// it holds even if a future emitter calls this from somewhere `build_body` never wrapped.
#[cfg(feature = "next-gen-gc")]
thread_local! {
    static CALLBUF_ABI_SIG: std::cell::Cell<Option<cranelift::codegen::ir::SigRef>> =
        std::cell::Cell::new(None);
}

/// ⭐**THE CALL-BUFFER ABI SIGNATURE IS A CONSTANT OF THE FUNCTION, AND EVERY CALL SITE MINTED ITS
/// OWN.** (`RAVA_NO_CALLSEQ=1`, or the block switch `RAVA_NO_EMIT_MORE=1`, restores the historic
/// five lines verbatim.)
///
/// Both compiled-callee call sites in this module hand the callee `(bufaddr, array_desc_cell)` and
/// take back one cell, i.e. `(i64, i64) -> i64` in the CALLER'S OWN calling convention. Nothing in
/// that is a property of the site: `b.func.signature.call_conv` is fixed for the whole function,
/// and the three `AbiParam`s are literals. `emit_direct_mono` built and imported it once per
/// static/special site and `emit_dispatch_pic` once per PIC site — and `Function::import_signature`
/// is a bare `self.dfg.signatures.push(signature)` (cranelift-codegen 0.132.1,
/// `ir/function.rs:243-245`), with NO deduplication anywhere behind it. So a body with N dispatch
/// sites carried N identical `Signature`s, each two `Vec<AbiParam>` heap allocations, in a table the
/// whole backend then walks. `BlockBehaviour$BlockStateBase.initCache` — 21.1% of all machine-code
/// time in a boot — has 111 calls in one body, and its dump numbers the last of them `sig63`.
///
/// **THE EMITTED MACHINE CODE CANNOT MOVE.** A `call_indirect`'s lowering reads the signature's
/// CONTENTS (call conv, parameter and return types) to place arguments; which slot of
/// `dfg.signatures` holds those contents is not observable in the output. Sharing one `SigRef`
/// between two `call_indirect`s is the ordinary case, not a special one, and nothing in this tree
/// mutates a signature after import (`dfg.signatures` is written in exactly one place — the
/// `import_signature` calls, grepped).
///
/// ⚠**THE GUARD MAY ONLY REJECT, AND IT PROVES ITS ANSWER RATHER THAN ASSUMING IT.** The memo is a
/// thread-local index with no session bracket, so before it is handed back it is checked against
/// THIS function's table:
///   * `dfg.signatures.get(s)` is bounds-checked (`cranelift-entity`'s `PrimaryMap::get`), so an
///     index minted for a longer function simply returns `None` here → mint fresh;
///   * the entry that comes back is compared field by field against the signature this site wants
///     — call conv, both parameter types, the return type, and both LENGTHS, so a signature that
///     merely starts the same cannot pass. A `SigRef`'s entire meaning is "this index of this
///     function's table"; an index that is in range AND holds exactly the wanted signature is
///     therefore correct BY DEFINITION, whichever emitter first imported it.
/// Every other outcome mints a fresh one, which is the historic behaviour exactly. There is no
/// path on which a call is emitted against a signature the site did not ask for.
///
/// The check itself allocates nothing: it reads the stored `Signature` in place and compares
/// `AbiParam`s (a `Copy`, `PartialEq` struct). Only a MISS builds a `Signature`, i.e. once per body
/// instead of once per site.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn callbuf_abi_sigref(b: &mut FunctionBuilder) -> cranelift::codegen::ir::SigRef {
    let conv = b.func.signature.call_conv;
    // COLD ARM: the historic four lines, verbatim, so an off build imports exactly what it did
    // before — same count, same order, same `SigRef` numbering.
    if !callseq_emit_enabled() {
        let mut cs = cranelift::codegen::ir::Signature::new(conv);
        cs.params.push(AbiParam::new(I64));
        cs.params.push(AbiParam::new(I64));
        cs.returns.push(AbiParam::new(I64));
        return b.import_signature(cs);
    }
    if let Some(s) = CALLBUF_ABI_SIG.with(|c| c.get()) {
        if let Some(sig) = b.func.dfg.signatures.get(s) {
            let want = AbiParam::new(I64);
            if sig.call_conv == conv
                && sig.params.len() == 2
                && sig.returns.len() == 1
                && sig.params[0] == want
                && sig.params[1] == want
                && sig.returns[0] == want
            {
                return s;
            }
        }
    }
    let mut cs = cranelift::codegen::ir::Signature::new(conv);
    cs.params.push(AbiParam::new(I64));
    cs.params.push(AbiParam::new(I64));
    cs.returns.push(AbiParam::new(I64));
    let s = b.import_signature(cs);
    CALLBUF_ABI_SIG.with(|c| c.set(Some(s)));
    s
}

/// ⭐**THE GATE ANSWERS ONE CALL-SITE EMISSION DERIVES — DERIVED ONCE.**
///
/// A compiled boot emits ~9,500 PIC sites plus the static/special direct-mono sites, all on the
/// SERIAL critical path, and each emission re-asked the same process constants over and over:
///   * `dispatch_callbuf_slots()` — **5 reads** inside one `emit_dispatch_pic` (the budget bail,
///     the hit buffer's size, and the three packed-copy slots in the arena-native, heap-Ref-native
///     and miss arms) and **2** inside one `emit_direct_mono` (bail + buffer size);
///   * `lambda_pic_enabled()` — **2** per heap-Ref PIC site (key select and hit decoding);
///   * `picprobe::enabled()` — **2** per heap-Ref PIC site (arena and heap hits).
/// Every one of them is a `OnceLock` read: an acquire load plus an initialised-branch that LLVM is
/// not allowed to hoist out from between the `FunctionBuilder` calls around it, so the count is
/// paid, not folded.
///
/// Each field is `Option`: `Some` = pre-read by the constructor and reused; `None` = re-derive at
/// the point of use, which is both the `RAVA_NO_FAST_EMITDERIVE=1` shape and the honest answer for
/// a gate this particular site was never going to consult. A `Some` is checked against a fresh read
/// of the original predicate by a `debug_assert` on EVERY use in a debug build.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy, Default)]
pub(crate) struct SiteGates {
    callbuf_slots: Option<usize>,
    probe: Option<bool>,
    lambda_pic: Option<bool>,
}

#[cfg(feature = "next-gen-gc")]
impl SiteGates {
    /// The gates `emit_direct_mono` consults. `callbuf_slots` is passed IN, not read here, because
    /// the site's budget bail needs it before the rest are worth reading at all — a site over the
    /// budget returns `None` having emitted no IR, and must not pay for gates it never reaches.
    #[inline]
    pub(crate) fn direct(callbuf_slots: usize) -> Self {
        if !fast_emitderive_enabled() {
            return Self::default();
        }
        Self {
            callbuf_slots: Some(callbuf_slots),
            probe: Some(crate::picprobe::enabled()),
            lambda_pic: None, // never consulted on the static/special side
        }
    }

    /// The gates `emit_dispatch_pic` consults. `lambda_pic` is pre-read only for `heap_mode == 2`
    /// — its three uses all live inside that arm, so pre-reading it for a mode-0/1 site would ADD
    /// a read to a site that asks zero times today. Left `None`, the accessor re-derives, and no
    /// mode-0/1 site can reach it.
    #[inline]
    pub(crate) fn pic(callbuf_slots: usize, heap_mode: u8) -> Self {
        if !fast_emitderive_enabled() {
            return Self::default();
        }
        Self {
            callbuf_slots: Some(callbuf_slots),
            probe: Some(crate::picprobe::enabled()),
            lambda_pic: if heap_mode == 2 { Some(lambda_pic_enabled()) } else { None },
        }
    }

    /// Call-buffer budget, in JVM SLOTS.
    #[inline]
    pub(crate) fn callbuf_slots(&self) -> usize {
        match self.callbuf_slots {
            Some(v) => {
                debug_assert_eq!(v, dispatch_callbuf_slots());
                v
            }
            None => dispatch_callbuf_slots(),
        }
    }

    /// The same budget as the stack-slot byte size every call buffer is created with.
    #[inline]
    pub(crate) fn callbuf_bytes(&self) -> u32 {
        (self.callbuf_slots() as u32) * 8
    }


    /// PIC-coverage probe: emit the inline load/add/store into every hit arm?
    #[inline]
    pub(crate) fn probe(&self) -> bool {
        match self.probe {
            Some(v) => {
                debug_assert_eq!(v, crate::picprobe::enabled());
                v
            }
            None => crate::picprobe::enabled(),
        }
    }

    /// λ-PIC: probe the heap-Ref cache by receiver IDENTITY for a `$Lambda` class word.
    #[inline]
    pub(crate) fn lambda_pic(&self) -> bool {
        match self.lambda_pic {
            Some(v) => {
                debug_assert_eq!(v, lambda_pic_enabled());
                v
            }
            None => lambda_pic_enabled(),
        }
    }
}

/// WIDE-ARGUMENT COVERAGE FOR THE MONOMORPHIC DIRECT CALL (`RAVA_NO_EMIT_STATIC=1` restores the
/// old refusal; `RAVA_NO_EMIT_MORE=1` disables the whole round). Read once, at EMIT time, so an
/// unflagged build's codegen is byte-identical.
///
/// ⭐**WHAT KEPT 3,414 STATIC/SPECIAL SITES OUT OF MACHINE CODE WAS ONE `any(|w| w != 1)`.** A
/// static or special callee has no receiver polymorphism, so `emit_direct_mono` needs no class
/// key — but it refused OUTRIGHT for any site carrying a `long` or a `double` argument, and every
/// one of those falls to `emit_invoke` → `rava_invoke` → `jit_call` → marshal-to-`Vec<Value>` +
/// synthetic frame, FOREVER: there is no cache to warm, no second chance, the refusal is baked
/// into the codegen. `Vec3.<init>(DDD)V`, `Mth.lerp(DDD)D`, `BlockPos.asLong`, every
/// `<init>(J)V` — the whole wide half of the static/special pool.
///
/// The refusal was not a correctness requirement. It was a LAYOUT shortcut, stated in the old
/// comment as "so one slot-laid buffer serves both the direct call and the packed miss ABI":
///   * the DIRECT arm calls a compiled body, which indexes its locals by JVM SLOT — a `long`
///     occupies slot k and leaves k+1 unused (see `body.rs`'s per-slot `load(locals_ptr, i*8)`);
///   * the MISS arm calls `rava_direct_miss` → `jit_call`, whose ABI is PACKED — one cell per
///     argument, the descriptor walked type by type (`jit_call_body`'s `raw[ai]`, `ai += 1`).
/// The two layouts coincide exactly when every argument is one slot wide. So the fix is the one
/// `emit_dispatch_pic` already uses for the same reason on the virtual/interface side: lay the
/// buffer out in SLOTS for the direct arm, and build the packed copy INSIDE THE COLD MISS ARM,
/// where it costs `nargs` stores on a path that already enters the Rust host.
///
/// ⚠**AND ONE THING THE PIC ANALOGY DOES NOT GIVE YOU: THE DEAD PAIR SLOT IS A GC ROOT.** A
/// `deep` (self-registering) callee pushes `(buffer, frame_len)` onto the thread's cell-root
/// stack and the collector scans that range BY RANGE, not by liveness. The v3 prologue zeroes
/// only `[args_slots, frame_len)`, and a wide argument's pair slot sits BELOW `args_slots` — so
/// it would be scanned holding whatever the previous user of this stack slot left there. An
/// arena cell with low bits 00 reads as a raw arena POINTER, i.e. the collector would follow
/// garbage. Hence the explicit zero store per wide argument below; it is one `mov` per wide arg
/// per call, and without it the deep arm is a live memory-corruption hazard.
///
/// GUARD / COLD ARM: unchanged — `fptr > 0` (the site's baked single-fptr cache is filled) proves
/// the callee is resolved, compiled and cacheable, and `<clinit>` has therefore already run,
/// because only a completed `rava_direct_miss` ever writes the cell. Everything else — cold cache
/// (0), permanently-never (-1), a gil-free worker that must not publish — takes the miss arm.
///
/// 23.9 MONOMORPHIC DIRECT-CALL codegen for invokestatic (kind 0) / invokespecial (kind 2). No class
/// guard — the target is monomorphic. A per-site single-fptr cache: HIT (fptr>0) → leaf (plain call)
/// or self_reg deep (spill→strip|1→call→reload) by the low bit; MISS (0/-1) → rava_direct_miss
/// (resolve+fill+run). Returns None (fall through to emit_invoke) only for sites whose SLOT total
/// exceeds the call-buffer budget — which is also the bound `dispatch_resolve_cacheable_cls_id`
/// checks on the callee, so a site over it could never have been served anyway.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_direct_mono(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    env: &Env,
    cp: u16,
    kind: u8,
    desc: &str,
    vars: &[Variable],
    locals_ptr: Value,
    array_desc_cell: Value,
) -> Option<()> {
    // arg_slot_widths includes the receiver at [0]; static (kind 0) drops it.
    let all = arg_slot_widths(desc);
    let widths: &[usize] = if kind == 0 { all.get(1..)? } else { &all[..] };
    let nargs = widths.len();
    // SLOT total, not argument count: a long/double argument occupies two of the callee's local
    // slots (the second unused). The budget the callee was admitted against
    // (`dispatch_resolve_cacheable_cls_id`: `max_locals <= dispatch_callbuf_slots()`) is measured
    // in the same slots, so this is the same bound expressed on the caller side.
    let slot_total: usize = widths.iter().sum();
    // ONE read of the callbuf budget for the whole emission: the bail below and the buffer's
    // stack-slot size both want it, and it is a process constant (a field of the latched
    // `ic_gates()` word) so the two reads could never disagree — they were simply two reads.
    let callbuf_slots = dispatch_callbuf_slots();
    if slot_total > callbuf_slots {
        return None; // over the callbuf budget → emit_invoke
    }
    // A site whose slot total differs from its argument count carries at least one wide argument,
    // which is exactly the population the two layouts disagree on. Gated so the widening can be
    // switched off without touching the codegen of every narrow site.
    let wide = slot_total != nargs;
    if wide && !emit_static_wide_enabled() {
        return None; // widening off → the pre-round behaviour: fall through to emit_invoke
    }
    // Gates are read once past both bails, so a refused site emits no IR and
    // pays for no probes. `RAVA_NO_FAST_EMITDERIVE=1` makes each accessor
    // re-derive at its point of use.
    let gates = SiteGates::direct(callbuf_slots);
    // Pop args deepest-first: cells[0] = receiver (special) / param0 (static).
    let mut cells: Vec<SV> = Vec::with_capacity(nargs);
    for _ in 0..nargs {
        cells.push(stack.pop()?);
    }
    cells.reverse();

    // Per-site single-fptr cache (leaked, baked). Monomorphic ⇒ no class key / per-thread IC; the fptr
    // is deterministic for the site so a benign worker race writes the same value.
    let cache_ptr = Box::into_raw(Box::new(0i64)) as i64;

    // SLOT-LAID buffer — the direct arm's callee reads it AS ITS LOCALS. For a narrow site this is
    // byte-for-byte the packed layout (`off` runs 0,1,2,… exactly as the old `i` did), so nothing
    // about a narrow site's emission changes; only a wide site advances by 2 and leaves a hole.
    //
    // POOLED (`slotpool_enabled`): the FUNCTION's one slot-laid buffer, not this site's own. See
    // the gate's doc comment for the liveness and GC-cover arguments, and `CallBufs` in `env.rs`
    // for the lazy creation — a body with no dispatch site still grows no buffer. `env.call_buf`
    // hands back a slot of `shared_callbuf_bytes()`, which is `gates.callbuf_bytes()` PLUS the one
    // guard slot the λ-shift arm needs on the PIC side; every store below is at
    // `off < slot_total <= callbuf_slots`, so the wider region changes nothing here.
    //
    // COLD ARM: the historic expression, verbatim, so `RAVA_NO_SLOTPOOL=1` (and
    // `RAVA_NO_EMIT_MORE=1` through the gate) prices as ONE arm and emits the pre-round IR — same
    // instruction sequence, same slot size, same block numbering. `RAVA_NO_SHARED_CALLBUF=1`
    // reaches the same place from inside `env.call_buf`, which then mints a private
    // `dispatch_callbuf_slots() * 8` slot of its own.
    let callbuf = if slotpool_enabled() {
        env.call_buf(b)
    } else {
        b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            gates.callbuf_bytes(),
            3,
        ))
    };
    {
        // `zero` is materialised only when a hole actually exists, so a narrow site emits no
        // extra instruction at all.
        let zero = if wide { Some(b.ins().iconst(I64, 0)) } else { None };
        let mut off = 0i32;
        for (i, sv) in cells.iter().enumerate() {
            let c = to_cell(b, *sv);
            b.ins().stack_store(c, callbuf, off * 8);
            // THE DEAD PAIR SLOT IS SCANNED. A `deep` callee registers this buffer as a cell-root
            // range and the collector walks it by RANGE; the v3 prologue only zeroes above
            // `args_slots`, and this hole is below it. Left stale it holds whatever the previous
            // occupant of this stack slot wrote — and a cell with low bits 00 reads as a raw arena
            // pointer, so the collector would follow it. One store per wide argument buys that off.
            if widths[i] == 2 {
                b.ins().stack_store(zero.unwrap(), callbuf, (off + 1) * 8);
            }
            off += widths[i] as i32;
        }
    }
    let bufaddr = b.ins().stack_addr(I64, callbuf, 0);

    let ref_slots: Vec<usize> =
        stack.iter().enumerate().filter(|(_, sv)| sv.t == JT::Ref).map(|(i, _)| i).collect();
    let nref = ref_slots.len();

    let direct = b.create_block();
    let miss = b.create_block();
    let merge = b.create_block();
    b.append_block_param(merge, I64);
    for _ in 0..nref {
        b.append_block_param(merge, I64);
    }

    let cachev = b.ins().iconst(I64, cache_ptr);
    let fptr = b.ins().load(I64, MemFlags::trusted(), cachev, Offset32::new(0));
    let have = b.ins().icmp_imm(IntCC::SignedGreaterThan, fptr, 0);
    b.ins().brif(have, direct, &[], miss, &[]);

    // DIRECT arm: leaf (plain) or self_reg deep (rooted), by the fptr's low bit.
    b.switch_to_block(direct);
    b.seal_block(direct);
    if gates.probe() {
        let a = b.ins().iconst(I64, crate::picprobe::pic_hit_addr());
        let cur = b.ins().load(I64, MemFlags::trusted(), a, Offset32::new(0));
        let inc = b.ins().iadd_imm(cur, 1);
        b.ins().store(MemFlags::trusted(), inc, a, Offset32::new(0));
    }
    // ONE import of the call-buffer ABI for the whole FUNCTION, not one per site: this signature
    // has no site-dependent part at all (see `callbuf_abi_sigref`). The `SigRef` it returns names
    // the identical `(i64, i64) -> i64` the historic four lines built here, so the `call_indirect`s
    // below lower byte for byte the way they did.
    let sigref = callbuf_abi_sigref(b);
    // Every cached target uses the rooted ABI. Rooting a GC-leaf call is
    // redundant but cheaper overall than carrying a leaf/deep diamond at every
    // caller; non-leaf callees retain their self-registering prologue.
    let fclean = b.ins().band_imm(fptr, -2i64);
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

    // MISS arm: rava_direct_miss resolves+fills+runs (may GC → spill/reload the caller frame).
    b.switch_to_block(miss);
    b.seal_block(miss);
    // PACKED copy, built HERE and only here. `rava_direct_miss` hands the cells to `jit_call`,
    // whose ABI walks the descriptor one type at a time and consumes one cell per parameter — so
    // a wide site's slot-laid buffer (with its hole) would shift every argument after the first
    // long/double by one and silently pass garbage. The two layouts coincide for a narrow site,
    // which therefore reuses `bufaddr` and emits nothing extra. The copy is `nargs` stores on the
    // COLD arm: this path already calls into the Rust host, resolves a method and may compile it.
    // The `cells` SSA values dominate this block (they were defined before the cache branch), so
    // re-running `to_cell` here is a re-encode of live values, not a reload.
    let missbuf = if !wide {
        bufaddr
    } else {
        // POOLED, and a SECOND region rather than a second use of `call_buf`. The two layouts
        // disagree for a wide site and both addresses are reachable from one branch, so the cold
        // re-marshal must never be able to overwrite the hot slot-laid one — that is the whole
        // reason `CallBufs` carries two cells. Liveness here is the narrowest window in the
        // function: the stores, a `stack_addr`, `spill_for_gc` (locals only) and the call.
        //
        // The scanned range is not a question on this path: `rava_direct_miss` reads exactly `n`
        // cells (`slice::from_raw_parts(buf, n)` with `n = nargs`) and hands them to `jit_call`,
        // which marshals into its own frame — nothing registers this region as a cell-root range,
        // and nothing looks above `nargs`. The `nargs` stores below therefore cover every byte
        // any consumer reads, so a previous site's residue above them is unreachable.
        //
        // COLD ARM: the historic expression verbatim, including its `nargs.max(1)` size.
        let packed = if slotpool_enabled() {
            env.packed_buf(b)
        } else {
            b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                (nargs.max(1) as u32) * 8,
                3,
            ))
        };
        for (i, sv) in cells.iter().enumerate() {
            let c = to_cell(b, *sv);
            b.ins().stack_store(c, packed, (i as i32) * 8);
        }
        b.ins().stack_addr(I64, packed, 0)
    };
    spill_for_gc(b, vars, stack, locals_ptr);
    let callerv = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind as i64);
    let nv = b.ins().iconst(I64, nargs as i64);
    let mcall = b.ins().call(env.direct_miss_fn, &[cachev, callerv, cpv, kindv, missbuf, nv]);
    let miss_bits = b.inst_results(mcall)[0];
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut miss_args: Vec<cranelift::codegen::ir::BlockArg> = vec![miss_bits.into()];
    for &i in &ref_slots {
        let rv = reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr);
        miss_args.push(rv.into());
    }
    b.ins().jump(merge, &miss_args);

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
    Some(())
}

/// Register-ABI arc (I1b): emit a static (`invokestatic`) call to an all-primitive target
/// via the register ABI when the runtime resolver finds it eligible, else fall back to the
/// normal direct-mono path (or rava_invoke when disabled). A per-site cell caches the pointer (0 =
/// unresolved, 1 = ineligible, else the register entry). Both arms merge to one i64 result
/// cell that becomes the pushed SV, so the rest of the body is oblivious to which ran.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_static_reg_call(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    env: &Env,
    cp: u16,
    desc: &str,
    vars: &[Variable],
    locals_ptr: Value,
    array_desc_cell: Value,
) -> Option<()> {
    let abi = reg_abi_desc(desc, ref_abi_enabled())?;
    let nparams = abi.params.len();
    if stack.len() < nparams {
        return None;
    }

    // Baked per-site pointer cache (leaked → immortal, like the PIC cells).
    //   0        = unresolved
    //   1        = ineligible (permanent — use the normal static-call fallback)
    //   p (8-al) = GC-LEAF register entry: it cannot reach a safepoint, so the caller
    //              keeps its live references in registers across the call
    //   p|1      = DEEP register entry: it self-registers its frame and CAN collect, so
    //              the caller must spill/reload around it (mirrors emit_direct_mono)
    let cell_ptr: *mut i64 = Box::leak(Box::new(0i64));
    let cell_addr = b.ins().iconst(I64, cell_ptr as i64);
    let cur = b.ins().load(I64, MemFlags::trusted(), cell_addr, Offset32::new(0));
    let need = b.ins().icmp_imm(IntCC::Equal, cur, 0);

    // GC HAZARD, and why the args are still on the operand stack here: the one-shot
    // resolve callback below RESOLVES A METHOD AND COMPILES IT — it loads classes and
    // allocates, so it can run a moving collection. With all-primitive args that was
    // inert (primitives are GC-inert), but a REFERENCE argument popped into an SSA value
    // would be invisible to the collector for the duration of that callback. So the args
    // stay on `stack` across the resolve, where `spill_for_gc` roots them like any other
    // ref operand, and they are re-read from the rooted buffer afterwards. Only then are
    // they popped. The resolve arm is cold (once per site), so the spill costs nothing on
    // the hot path — it is threaded through block params exactly like emit_direct_mono's
    // miss arm rather than emitted straight-line.
    let pre_refs: Vec<usize> =
        stack.iter().enumerate().filter(|(_, sv)| sv.t == JT::Ref).map(|(i, _)| i).collect();
    let resolve_bb = b.create_block();
    let post_bb = b.create_block();
    b.append_block_param(post_bb, I64); // the resolved pointer
    for _ in 0..pre_refs.len() {
        b.append_block_param(post_bb, I64);
    }
    let mut fast: Vec<cranelift::codegen::ir::BlockArg> = vec![cur.into()];
    fast.extend(
        pre_refs.iter().map(|&i| -> cranelift::codegen::ir::BlockArg { stack[i].v.into() }),
    );
    b.ins().brif(need, resolve_bb, &[], post_bb, &fast);

    b.switch_to_block(resolve_bb);
    b.seal_block(resolve_bb);
    spill_for_gc(b, vars, stack, locals_ptr);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let rc = b.ins().call(env.resolve_reg, &[cls, cpv]);
    let rptr = b.inst_results(rc)[0];
    b.ins().store(MemFlags::trusted(), rptr, cell_addr, Offset32::new(0));
    reload_locals_after_gc(b, vars, locals_ptr);
    let mut rargs: Vec<cranelift::codegen::ir::BlockArg> = vec![rptr.into()];
    for &i in &pre_refs {
        rargs.push(reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr).into());
    }
    b.ins().jump(post_bb, &rargs);

    b.switch_to_block(post_bb);
    b.seal_block(post_bb);
    let pparams: Vec<Value> = b.block_params(post_bb).to_vec();
    let ptr = pparams[0];
    for (k, &i) in pre_refs.iter().enumerate() {
        stack[i].v = pparams[1 + k];
    }

    // The args are GC-current again — pop them. From here their register copies are dead
    // in the caller (the operand stack no longer names them), so a collection inside the
    // callee may leave them stale with no observable effect.
    let mut args: Vec<SV> = Vec::with_capacity(nparams);
    for _ in 0..nparams {
        args.push(stack.pop()?);
    }
    args.reverse();

    // References the caller still owns and must see forwarded after any arm that collects.
    //
    // ⭐**THIS IS `pre_refs` TRUNCATED, NOT A SECOND SCAN OF THE OPERAND STACK.** The site derives
    // "which operand-stack entries are Refs" TWICE — once above, across the resolve callback, and
    // once here — and the two differ only by the `nparams` entries popped in between. `pop` removes
    // the HIGHEST indices only; every surviving entry keeps its index and its `.t` (the merge above
    // rewrites `.v` alone), so the Ref indices that remain are exactly the `pre_refs` entries below
    // the new depth. `pre_refs` is ascending by construction ⇒ that is a PREFIX, found by one
    // `partition_point` instead of another O(depth) filter+collect.
    //
    // The equality is exact, not a one-sided prefilter, so it is checked against the original
    // expression by a `debug_assert` on every call in a debug build; `RAVA_NO_FAST_EMITDERIVE=1`
    // restores the original scan verbatim.
    let ref_slots: Vec<usize> = if fast_emitderive_enabled() {
        let keep = pre_refs.partition_point(|&i| i < stack.len());
        let v = pre_refs[..keep].to_vec();
        debug_assert_eq!(
            v,
            stack
                .iter()
                .enumerate()
                .filter(|(_, sv)| sv.t == JT::Ref)
                .map(|(i, _)| i)
                .collect::<Vec<usize>>()
        );
        v
    } else {
        stack.iter().enumerate().filter(|(_, sv)| sv.t == JT::Ref).map(|(i, _)| i).collect()
    };
    let nref = ref_slots.len();

    // `ptr > 1` is the eligibility test, not `ptr != 1`: the resolver also returns 0 when
    // it declined to run at all (a GIL-free worker cannot compile — see rava_resolve_reg),
    // and a 0 must take the buffer fallback rather than be called as a function pointer.
    let is_ok = b.ins().icmp_imm(IntCC::SignedGreaterThan, ptr, 1);
    let fb_bb = b.create_block();
    let reg_bb = b.create_block();
    let done_bb = b.create_block();
    b.append_block_param(done_bb, I64); // merged result as an i64 cell
    for _ in 0..nref {
        b.append_block_param(done_bb, I64);
    }
    b.ins().brif(is_ok, reg_bb, &[], fb_bb, &[]);

    // --- register arm: args travel in registers, no call buffer ---
    b.switch_to_block(reg_bb);
    b.seal_block(reg_bb);
    let cc = b.func.signature.call_conv;
    let mut rsig = cranelift::codegen::ir::Signature::new(cc);
    rsig.params.push(AbiParam::new(I64)); // array_desc_cell
    for k in &abi.params {
        rsig.params.push(AbiParam::new(k.cl_type()));
    }
    rsig.returns.push(AbiParam::new(abi.ret.cl_type()));
    let sref = b.import_signature(rsig);
    let mut call_args: Vec<Value> = Vec::with_capacity(nparams + 1);
    call_args.push(array_desc_cell);
    for sv in &args {
        // `to_cell` is the identity for a Ref (the tagged cell already IS the i64) and for
        // a Long; the other kinds ride in their own register class untouched.
        call_args.push(sv.v);
    }
    let to_cell_ret = |b: &mut FunctionBuilder, rres: Value| -> Value {
        match abi.ret {
            PrimKind::F64 => b.ins().bitcast(I64, MemFlags::new(), rres),
            PrimKind::F32 => {
                let bits = b.ins().bitcast(I32, MemFlags::new(), rres);
                b.ins().uextend(I64, bits)
            }
            PrimKind::I32 => b.ins().sextend(I64, rres),
            PrimKind::I64 | PrimKind::Ref => rres,
        }
    };
    let keep = |stack: &[SV], cell: Value| -> Vec<cranelift::codegen::ir::BlockArg> {
        let mut a: Vec<cranelift::codegen::ir::BlockArg> = vec![cell.into()];
        a.extend(ref_slots.iter().map(|&i| -> cranelift::codegen::ir::BlockArg { stack[i].v.into() }));
        a
    };
    let reload_refs = |b: &mut FunctionBuilder,
                       cell: Value|
     -> Vec<cranelift::codegen::ir::BlockArg> {
        let mut a: Vec<cranelift::codegen::ir::BlockArg> = vec![cell.into()];
        for &i in &ref_slots {
            a.push(reload_ref_slot(b, vars.len(), i, stack[i].v, locals_ptr).into());
        }
        a
    };
    // Leaf vs deep, by the pointer's low bit — the same encoding emit_direct_mono uses.
    let leaf_bb = b.create_block();
    let deep_bb = b.create_block();
    let dbit = b.ins().band_imm(ptr, 1);
    let is_deep = b.ins().icmp_imm(IntCC::NotEqual, dbit, 0);
    b.ins().brif(is_deep, deep_bb, &[], leaf_bb, &[]);

    // LEAF: `is_gc_leaf` proved no safepoint exists inside the callee, so nothing here
    // can collect and the caller's live references stay valid in their registers.
    b.switch_to_block(leaf_bb);
    b.seal_block(leaf_bb);
    let lc = b.ins().call_indirect(sref, ptr, &call_args);
    let lres = b.inst_results(lc)[0];
    let lcell = to_cell_ret(b, lres);
    let largs = keep(stack, lcell);
    b.ins().jump(done_bb, &largs);

    // DEEP: the callee self-registers its own frame (so the reference ARGS are rooted the
    // moment its prologue runs) but it can collect, so the caller spills and reloads what
    // IT still owns.
    b.switch_to_block(deep_bb);
    b.seal_block(deep_bb);
    let clean = b.ins().band_imm(ptr, -2i64);
    spill_for_gc(b, vars, stack, locals_ptr);
    let dc = b.ins().call_indirect(sref, clean, &call_args);
    let dres = b.inst_results(dc)[0];
    let dcell = to_cell_ret(b, dres);
    reload_locals_after_gc(b, vars, locals_ptr);
    let dargs = reload_refs(b, dcell);
    b.ins().jump(done_bb, &dargs);

    // Preserve the normal static-call path when the register entry is unavailable.
    // A rejected register signature must not permanently replace a cached machine
    // call with interpreter dispatch. Use a separate stack for this CFG arm so its
    // forwarded references enter the outer merge without changing the leaf arm.
    b.switch_to_block(fb_bb);
    b.seal_block(fb_bb);
    let mut fallback_stack = stack.clone();
    fallback_stack.extend_from_slice(&args);
    if crate::directmono::enabled()
        && emit_direct_mono(
            b, &mut fallback_stack, env, cp, 0, desc, vars, locals_ptr, array_desc_cell,
        )
        .is_some()
    {
        let result = fallback_stack.pop()?;
        let cell = to_cell(b, result);
        let fargs = keep(&fallback_stack, cell);
        b.ins().jump(done_bb, &fargs);
    } else {
        for (i, sv) in args.iter().enumerate() {
            let cell = to_cell(b, *sv);
            b.ins().stack_store(cell, env.args_slot, (i as i32) * 8);
        }
        spill_for_gc(b, vars, stack, locals_ptr);
        let buf = b.ins().stack_addr(I64, env.args_slot, 0);
        let cls2 = b.ins().iconst(I64, env.class_ptr);
        let cpv2 = b.ins().iconst(I64, cp as i64);
        let kindv = b.ins().iconst(I64, 0);
        let nv = b.ins().iconst(I64, nparams as i64);
        let ic = b.ins().call(env.invoke, &[cls2, cpv2, kindv, buf, nv]);
        let ires = b.inst_results(ic)[0];
        reload_locals_after_gc(b, vars, locals_ptr);
        let fargs = reload_refs(b, ires);
        b.ins().jump(done_bb, &fargs);
    }

    // --- merge ---
    b.switch_to_block(done_bb);
    b.seal_block(done_bb);
    let dparams: Vec<Value> = b.block_params(done_bb).to_vec();
    for (k, &i) in ref_slots.iter().enumerate() {
        stack[i].v = dparams[1 + k];
    }
    if let Some(jt) = ret_full(desc) {
        let sv = cell_to_sv(b, dparams[0], jt);
        stack.push(sv);
    }
    Some(())
}

pub(crate) fn emit_invoke(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    env: &Env,
    cp: u16,
    kind: u8,
    desc: &str,
    vars: &[Variable],
    locals_ptr: Value,
) -> Option<()> {
    let n_params = crate::count_args(desc);
    let total = n_params + if kind != 0 { 1 } else { 0 };
    // Pop args deepest-first so slot[i] holds argument i ([recv?, p0, p1, ...]).
    for i in (0..total).rev() {
        let a = stack.pop()?;
        let cell = to_cell(b, a);
        b.ins().stack_store(cell, env.args_slot, (i as i32) * 8);
    }
    // Args are out of the frame (rooted by the callee); spill what remains.
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind as i64);
    let nv = b.ins().iconst(I64, total as i64);
    let call = b.ins().call(env.invoke, &[cls, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    // The callee may have run a moving GC; reload live arena roots before using
    // them (the returned `res` is fresh, not stale).
    #[cfg(feature = "next-gen-gc")]
    reload_after_gc(b, vars, stack, locals_ptr);
    if let Some(jt) = ret_full(desc) {
        let sv = cell_to_sv(b, res, jt);
        stack.push(sv);
    }
    Some(())
}

/// `emit_invoke` with an explicit dispatch callback so the fastutil fast path can
/// substitute `env.fu_invoke`. next-gen-gc only — keeps the default `emit_invoke`
/// source (and thus its codegen) byte-identical.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_invoke_via(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    env: &Env,
    cp: u16,
    kind: u8,
    desc: &str,
    vars: &[Variable],
    locals_ptr: Value,
    invoke_fn: cranelift::codegen::ir::FuncRef,
) -> Option<()> {
    let n_params = crate::count_args(desc);
    let total = n_params + if kind != 0 { 1 } else { 0 };
    for i in (0..total).rev() {
        let a = stack.pop()?;
        let cell = to_cell(b, a);
        b.ins().stack_store(cell, env.args_slot, (i as i32) * 8);
    }
    spill_for_gc(b, vars, stack, locals_ptr);
    let buf = b.ins().stack_addr(I64, env.args_slot, 0);
    let cls = b.ins().iconst(I64, env.class_ptr);
    let cpv = b.ins().iconst(I64, cp as i64);
    let kindv = b.ins().iconst(I64, kind as i64);
    let nv = b.ins().iconst(I64, total as i64);
    let call = b.ins().call(invoke_fn, &[cls, cpv, kindv, buf, nv]);
    let res = b.inst_results(call)[0];
    reload_after_gc(b, vars, stack, locals_ptr);
    if let Some(jt) = ret_full(desc) {
        let sv = cell_to_sv(b, res, jt);
        stack.push(sv);
    }
    Some(())
}
