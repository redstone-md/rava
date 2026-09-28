//! Making compiled-frame references visible to the collector across a re-entrant callback.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

#[cfg(feature = "next-gen-gc")]
pub(crate) fn stale_store_trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_STALESTORE").is_some())
}

/// Default-build no-op: the stale-store forensic reads arena headers, which don't
/// exist without `next-gen-gc` — the diagnostic is permanently off.
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn stale_store_trace() -> bool {
    false
}

/// Default-build no-op twin of the arena stale-young probe: no arena → no young
/// window → no pointer is ever stale.
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn stale_young_ptr(_val: i64) -> Option<u64> {
    None
}

/// True if `val` (a raw arena pointer, tag 0b00) is a STALE young pointer: either its object
/// has a forward word set (moved young→old, header still intact — window A) OR its header word
/// is implausible (`nfields` field huge — its young slot was reset + re-bumped into a string /
/// other object — window B). Catches a compiled register that survived a collection un-spilled
/// and is now being stored. Returns the forward target (or 0) for logging.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn stale_young_ptr(val: i64) -> Option<u64> {
    if val == 0 || (val & 0b11) != 0 {
        return None;
    }
    // Use the FULL nursery address range [ylo,yhi) (cap-bounded), not tlab_in_nursery (which
    // gates on young.top): a window-B stale ptr sits ABOVE the current top (its slot freed by a
    // reset that dropped top below it) yet still inside the mapped nursery — the tlab_in_nursery
    // gate silently dropped exactly those (the false STALE-READ=0), missing the stale
    // Holder$Direct.value reads. The whole range is mapped, so reading the header is safe.
    let (ylo, yhi) = crate::nextgen::tlab_young_window();
    let uval = val as u64;
    if uval < ylo || uval >= yhi {
        return None;
    }
    let hdr = unsafe { (val as u64 as *const u64).read_unaligned() };
    // Decode the forwarding word through the layout accessor rather than a raw +8
    // u64 load: under `shrink-header` that word is a u32 offset packed beside the u32
    // identity hash, so a raw 64-bit read would mistake a hashed-but-live object for a
    // forwarded one. Only 8-aligned addresses can be real headers.
    let fwd = if uval & 7 == 0 {
        unsafe { crate::nextgen::obj_fwd(uval as *mut u8) as u64 }
    } else {
        0
    };
    let fwd_set = fwd != 0 && (fwd & 0b111) == 0 && fwd >= 0x2_0000_0000;
    let hdr_bad = (hdr >> 32) > 0xFFFF; // nfields field implausibly large ⇒ slot reused
    if fwd_set || hdr_bad {
        Some(if fwd_set { fwd } else { 0 })
    } else {
        None
    }
}

/// Local slots whose value is PINNED for the whole call: an `aload k` exists and
/// NO store ever targets k. Valid bytecode cannot read an uninitialised non-arg
/// local, so "loaded as a ref + never stored" ⟹ k is a REF ARG that keeps its
/// entry value — a field-cell pointer resolved from it at entry stays correct
/// (the receiver can't change; under no_alloc the object can't move either).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn pinned_ref_slots(bc: &[u8], max_locals: usize) -> Vec<bool> {
    let mut loaded = vec![false; max_locals];
    let mut stored = vec![false; max_locals];
    let mut kill = |v: &mut Vec<bool>, i: usize, wide: bool| {
        if i < v.len() {
            v[i] = true;
        }
        if wide && i + 1 < v.len() {
            v[i + 1] = true;
        }
    };
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return vec![false; max_locals] };
        match op {
            0x19 => kill(&mut loaded, bc[pc + 1] as usize, false), // aload
            0x2a..=0x2d => kill(&mut loaded, (op - 0x2a) as usize, false),
            0x36..=0x3a => kill(&mut stored, bc[pc + 1] as usize, matches!(op, 0x37 | 0x39)), // istore..astore
            0x3b..=0x4e => kill(
                &mut stored,
                ((op - 0x3b) & 3) as usize,
                matches!(op, 0x3f..=0x42 | 0x47..=0x4a), // lstore_n / dstore_n
            ),
            0x84 => kill(&mut stored, bc[pc + 1] as usize, false), // iinc
            0xc4 => {
                // wide store/iinc forms; kill idx (+1 conservatively for l/d).
                let wop = bc.get(pc + 1).copied().unwrap_or(0);
                if matches!(wop, 0x36..=0x3a | 0x84) {
                    let idx = ((bc[pc + 2] as usize) << 8) | bc[pc + 3] as usize;
                    kill(&mut stored, idx, true);
                }
            }
            _ => {}
        }
        pc += len;
    }
    loaded.iter().zip(stored.iter()).map(|(&l, &s)| l && !s).collect()
}

/// Field REGISTER PROMOTION: DEFAULT ON (opt-out `RAVA_NO_FIELD_PROMO`, the
/// bisect gate). See `promo_scan`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn field_promo_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FIELD_PROMO").is_none())
}

/// PROMO gate mirror of emit_op's 0xb6/0xb7 intrinsic routing: true when the
/// call would be served by a fastpath/intrinsic INSTEAD of `emit_inline` (those
/// spill + call back — unsound while promoted field registers are dirty).
/// MUST be kept in sync with the invoke arms in `emit_op`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn promo_virt_preempted(owner: &str, name: &str, desc: &str) -> bool {
    fu_routable(owner, name, desc)
        || (name == "ordinal" && desc == "()I")
        || name == "<init>" // ctor_inlines / Object.<init> preempts
        || matches!(
            owner,
            "java/util/HashMap"
                | "it/unimi/dsi/fastutil/ints/Int2ObjectOpenHashMap"
                | "java/lang/Integer"
                | "java/lang/String"
                | "java/util/BitSet" // bitset_get/set/clear/isempty fastpaths
        )
}

/// Field REGISTER PROMOTION gate (the JVM-parity lever on read+written fields):
/// a method passes iff EVERY reachable op is provably exception-free and every
/// getfield/putfield resolves to a FIELD-LICM pair — then each pair's VALUE can
/// live in a Cranelift Variable for the whole call (no load/store in the loop),
/// written registers flushed at returns and before any slow-arm callback.
///
/// Returns `Some(written pair set)` when promotable, `None` otherwise. The
/// strictness exists because a guest exception (→ jit_pending early return)
/// would skip the return flush and lose writes: the whitelist excludes int
/// div/rem, ALL array ops, athrow, statics, allocation, checkcast/instanceof,
/// switches and any call that is not a verified-inlined callee or a pure
/// static intrinsic. The remaining early exits (a promoted site's own slow-arm
/// callback) are pre-flushed by `emit_promo_spill`.
///
/// ALIASING: two locals may hold the SAME object. A (fid, cslot) WRITTEN
/// through one slot while promoted under another would split one memory cell
/// across two registers — bail. Read-only multi-slot pairs are safe (nothing
/// mutates the cell during the call).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn promo_scan(
    bc: &[u8],
    fields: &std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    pool: &ConstantPool,
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
    pairs: &[LicmPair],
    max_locals: usize,
) -> Option<std::collections::HashSet<(u16, u32, u32)>> {
    if pairs.is_empty() {
        return None;
    }
    let pinned = pinned_ref_slots(bc, max_locals);
    let prov: Vec<Option<u16>> = (0..max_locals)
        .map(|k| (pinned.get(k).copied().unwrap_or(false)).then_some(k as u16))
        .collect();
    let mut written = std::collections::HashSet::new();
    promo_scan_body(
        bc,
        fields,
        pool,
        &prov,
        pairs,
        Some((inline_targets, static_intrinsics)),
        &mut written,
    )?;
    for w in &written {
        if pairs.iter().any(|p| (p.fid, p.cslot) == (w.1, w.2) && p.slot != w.0) {
            return None; // written cell promoted under a second (maybe aliasing) slot
        }
    }
    Some(written)
}

/// PROMO callee walk: bind the caller's arg provenances to the callee's pinned
/// param slots (exactly `emit_inline`'s mapping) and re-run the whitelist scan
/// with `calls = None`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn promo_scan_callee(
    callee: &InlineCallee,
    args: &[Option<u16>],
    pairs: &[LicmPair],
) -> Option<()> {
    let bcc: &[u8] = &callee.bytecode;
    let nvars = (callee.max_locals as usize).max(args.len());
    let cpin = pinned_ref_slots(bcc, nvars);
    let mut prov: Vec<Option<u16>> = vec![None; nvars];
    for (j, &a) in args.iter().enumerate() {
        if cpin.get(j).copied().unwrap_or(false) {
            prov[j] = a;
        }
    }
    let mut w = std::collections::HashSet::new();
    promo_scan_body(bcc, &callee.fields, &callee.class.file.pool, &prov, pairs, None, &mut w)
}

#[allow(clippy::too_many_arguments)]
/// Make every live reference in this compiled frame visible to the GC before a
/// callback that can re-enter the interpreter (invoke / getstatic / putstatic /
/// new / anewarray / instanceof — any of these can run <clinit> or arbitrary
/// bytecode, hitting a GC checkpoint). Compiled-frame refs otherwise live only
/// in registers (operand-stack SSA values, locals Variables mutated after
/// entry), invisible to the collector: a nested collection would free objects
/// whose only reference is here, leaving a dangling handle — the flaky
/// boot-killer ("invokevirtual .size() on a freed receiver"). Writes all local
/// Variables back to the rooted cells buffer (their own slots; conservative —
/// the GC tag-checks each cell) and ref-typed stack values to the spill area
/// above max_locals (the caller sizes the buffer max_locals + max_stack).
pub(crate) fn spill_for_gc(b: &mut FunctionBuilder, vars: &[Variable], stack: &[SV], locals_ptr: Value) {
    // PHASE-A STACK MAPS (`RAVA_STACKMAPS=1`, else not one call is made, and this whole
    // block compiles to nothing observable). This is the ONE choke point through which
    // every GC-capable call site in the engine roots its operand stack — ~40 emitters call
    // it — so declaring here names exactly the set the shadow frame names, and no other.
    // Locals are declared once at creation instead (`emit::body`), because
    // `declare_var_needs_stack_map` must precede a variable's first use.
    //
    // Aliases are resolved first: `SafepointSpiller` keys its liveness on
    // `dfg.resolve_aliases(val)` (cranelift-frontend-0.132.1 src/frontend/safepoints.rs:441),
    // so declaring an un-resolved alias would silently register a value the pass never
    // matches — a map that misses a root we DO root, i.e. exactly the false finding this
    // census exists to avoid.
    #[cfg(feature = "next-gen-gc")]
    if crate::jit::runtime::stackmaps::declaring() {
        for sv in stack.iter() {
            if sv.t == JT::Ref {
                let v = b.func.dfg.resolve_aliases(sv.v);
                b.declare_value_needs_stack_map(v);
            }
        }
    }
    // PHASE B (`RAVA_STACKMAPS=1`): the maps ARE the rooting protocol. Nothing is written to
    // the shadow frame, because nothing will read it: the declarations above (plus the
    // per-local `declare_var_needs_stack_map` in `emit::body`) hand Cranelift the complete
    // root set, `SafepointSpiller` parks each declared value in a stack slot across the call,
    // and the collector rewrites that slot in place (`stackmaps::for_each_map_slot`). The
    // stores below are what this phase exists to delete — the whole per-call rooting cost.
    //
    // The frame buffer is still PUBLISHED by the self-registering prologue and still scanned,
    // holding what the prologue left in it: the caller's argument cells below `args_slots` and
    // zeros above (`emit::prologue`'s zero loop covers `[args_slots, frame_len)`). That is a
    // valid, fully-initialised cell set at entry and it is forwarded in place at every
    // collection, so it never goes stale — it is a conservative retention of the entry
    // arguments, not a correctness dependency, and it is what pairs a frame with its SP.
    #[cfg(feature = "next-gen-gc")]
    {
        if crate::jit::runtime::stackmaps::enabled() {
            crate::jit::runtime::stackmaps::note_spill(
                vars.len() + stack.iter().filter(|sv| sv.t == JT::Ref).count(),
            );
        }
        if crate::jit::runtime::stackmaps::declaring() {
            return;
        }
    }
    spill_frame_words(b, vars, stack, locals_ptr);
}

/// ⭐**THE ONE ROOT A SPLICE'S SAFEPOINTS DO NOT ALREADY NAME — DECLARED, NOT STORED.**
///
/// Under `stackmaps::declaring()` a compiled frame is rooted by a precise map per safepoint, and
/// every part of it is already declared somewhere: the caller's locals and an inlined callee's
/// locals at their `Variable`'s creation (`emit::body`, `emit::inline`), and each frame's ref
/// OPERAND-STACK values at [`spill_for_gc`], the choke point every GC-capable emitter shares.
///
/// The gap is a level question, not a kind question. Inside a spliced body, `spill_for_gc` is
/// handed the CALLEE's operand stack — so the values the CALLER left on its own stack at the
/// splice point are named by no declaration at all, and a safepoint inside the body would map
/// every root but those. `emit_inline` closes it by calling this once, after the arguments have
/// been popped, for every splice.
///
/// **It emits nothing.** A declaration is a note to `SafepointSpiller`, which parks a value in a
/// stack-map slot only at the safepoints it is actually live across; a caller stack value that
/// dies before the splice's first GC point — or a splice with no GC point at all — costs exactly
/// zero. That is why this is unconditional over splices rather than gated on a body scan.
///
/// Aliases are resolved exactly as [`spill_for_gc`] resolves them: `SafepointSpiller` keys its
/// liveness on `dfg.resolve_aliases(val)`, so declaring an un-resolved alias would register a
/// value the pass never matches — a map that misses a root we believe we rooted.
///
/// Returns how many values were declared, for the emit-time census. Inert (and returns 0) with
/// the maps off, so `RAVA_NO_STACKMAPS=1` emission is byte-identical.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn declare_stack_roots(b: &mut FunctionBuilder, stack: &[SV]) -> usize {
    if !crate::jit::runtime::stackmaps::declaring() {
        return 0;
    }
    let mut n = 0usize;
    for sv in stack.iter() {
        if sv.t == JT::Ref {
            let v = b.func.dfg.resolve_aliases(sv.v);
            b.declare_value_needs_stack_map(v);
            n += 1;
        }
    }
    n
}

/// The store half of [`spill_for_gc`], UNCONDITIONAL. Materialises the interpreter's frame
/// layout — locals at `[i*8]`, ref operand-stack slots at `[(nlocals+i)*8]` — in the frame
/// buffer.
///
/// Split out because one caller needs it for a reason that is NOT rooting:
/// `emit::guarded_inline::emit_deopt_exit` hands `locals_ptr` to `rava_deopt`, which rebuilds a
/// `Frame` from those exact words and resumes the interpreter in it. That is a SEMANTIC use of
/// the buffer, so it must keep emitting the stores in both arms; the stack maps replace rooting,
/// not the deopt transition.
pub(crate) fn spill_frame_words(
    b: &mut FunctionBuilder,
    vars: &[Variable],
    stack: &[SV],
    locals_ptr: Value,
) {
    for (i, var) in vars.iter().enumerate() {
        let v = b.use_var(*var);
        b.ins().store(MemFlags::trusted(), v, locals_ptr, Offset32::new((i * 8) as i32));
    }
    for (i, sv) in stack.iter().enumerate() {
        if sv.t == JT::Ref {
            let off = Offset32::new(((vars.len() + i) * 8) as i32);
            b.ins().store(MemFlags::trusted(), sv.v, locals_ptr, off);
        }
    }
}

/// PHASE B — read ONE ref operand-stack slot back after a GC-capable call.
///
/// Flag OFF: the historic load from the spill area above `max_locals`, byte-identical.
/// Flag ON: `orig` itself. The value was declared at `spill_for_gc`, so Cranelift parked it in
/// a stack-map slot across the call and the collector forwarded that slot in place; every use
/// of `orig` after the call is already a reload of the forwarded word.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn reload_ref_slot(
    b: &mut FunctionBuilder,
    nlocals: usize,
    i: usize,
    orig: Value,
    locals_ptr: Value,
) -> Value {
    if crate::jit::runtime::stackmaps::declaring() {
        return orig;
    }
    let off = Offset32::new(((nlocals + i) * 8) as i32);
    b.ins().load(I64, MemFlags::trusted(), locals_ptr, off)
}

/// TASK 2 — the symmetric partner of `spill_for_gc`. A moving (Cheney) collector
/// run inside a GC-capable callback RELOCATES arena objects and rewrites every
/// rooted cell IN PLACE (the GC forwards `cells_all()` buffers — see
/// `Vm::arena_collect_jit`). The SSA values / local registers the compiled frame
/// held across the callback are now STALE (they point at the pre-move location).
/// This pass re-reads the forwarded pointers from the rooted buffer back into the
/// JIT's local Variables and ref-typed operand-stack values, immediately after the
/// callback returns. Locals go through `def_var` (SSA-correct even when the reload
/// is on one arm of a diamond — Cranelift inserts the merge phi); ref stack values
/// are reloaded in place (sound on the straight-line callsites it's used at). A
/// primitive cell round-trips unchanged (the GC only forwards tagged/arena cells),
/// so reloading every local is safe.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn reload_after_gc(b: &mut FunctionBuilder, vars: &[Variable], stack: &mut [SV], locals_ptr: Value) {
    // PHASE B: nothing was spilled, so there is nothing to read back. The locals' Variables and
    // the ref stack values keep their pre-call SSA values — which is precisely what makes them
    // LIVE ACROSS the safepoint and therefore recorded in the map (Phase A measured the
    // opposite: this reload is what emptied the maps). Cranelift reloads each from its stack-map
    // slot at the next use, and the collector rewrote that slot.
    if crate::jit::runtime::stackmaps::enabled() {
        crate::jit::runtime::stackmaps::note_reload(
            vars.len() + stack.iter().filter(|sv| sv.t == JT::Ref).count(),
        );
    }
    if crate::jit::runtime::stackmaps::declaring() {
        return;
    }
    for (i, var) in vars.iter().enumerate() {
        let loaded = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((i * 8) as i32));
        b.def_var(*var, loaded);
    }
    for (i, sv) in stack.iter_mut().enumerate() {
        if sv.t == JT::Ref {
            let off = Offset32::new(((vars.len() + i) * 8) as i32);
            sv.v = b.ins().load(I64, MemFlags::trusted(), locals_ptr, off);
        }
    }
}

/// Locals-only reload (`def_var`, SSA-safe on a diamond arm). Used where the
/// spill+callback sits on one branch of a control-flow diamond (cached getstatic),
/// so reloading ref stack values in place would break SSA across the merge — those
/// stay handled at the straight-line callsites.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn reload_locals_after_gc(b: &mut FunctionBuilder, vars: &[Variable], locals_ptr: Value) {
    // PHASE B: see `reload_after_gc`. No spill happened, so no reload exists to emit.
    if crate::jit::runtime::stackmaps::enabled() {
        crate::jit::runtime::stackmaps::note_reload(vars.len());
    }
    if crate::jit::runtime::stackmaps::declaring() {
        return;
    }
    for (i, var) in vars.iter().enumerate() {
        let loaded = b.ins().load(I64, MemFlags::trusted(), locals_ptr, Offset32::new((i * 8) as i32));
        b.def_var(*var, loaded);
    }
}
