//! Trivial-shape recognition: closed-form folds, field-setter constructors, callback density.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_trivial_ctor(
    bc: &[u8],
    pool: &ConstantPool,
    init_desc: &str,
    has_exceptions: bool,
) -> Option<Vec<TrivialCtorStore>> {
    if has_exceptions {
        return None;
    }
    // Map each local slot → its argument index (wide args take two slots; the
    // receiver `this` occupies slot 0).
    let mut slot_to_arg: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    {
        let bytes = init_desc.as_bytes();
        let mut i = init_desc.find('(').map(|p| p + 1)?;
        let mut slot = 1usize; // slot 0 = this
        let mut arg = 0usize;
        while i < bytes.len() && bytes[i] != b')' {
            let was_array = bytes[i] == b'[';
            while i < bytes.len() && bytes[i] == b'[' {
                i += 1;
            }
            let kind = *bytes.get(i)?;
            if kind == b'L' {
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
            }
            i += 1;
            slot_to_arg.insert(slot, arg);
            let wide = !was_array && matches!(kind, b'J' | b'D');
            slot += if wide { 2 } else { 1 };
            arg += 1;
        }
    }

    // op0: aload_0
    if bc.first() != Some(&0x2a) {
        return None;
    }
    let mut pc = 1usize;
    // op1: invokespecial Object.<init>()V (the empty super constructor).
    if bc.get(pc) != Some(&0xb7) || pc + 2 >= bc.len() {
        return None;
    }
    let sidx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
    let sm = pool.member_ref(sidx)?;
    if sm.owner != "java/lang/Object" || sm.name != "<init>" || sm.descriptor != "()V" {
        return None;
    }
    pc += 3;

    let mut stores = Vec::new();
    loop {
        match bc.get(pc) {
            Some(&0xb1) => {
                // `return` must be the final op (no trailing dead code).
                return if pc + 1 == bc.len() { Some(stores) } else { None };
            }
            Some(&0x2a) => {} // aload_0 — start of a putfield triple
            _ => return None,
        }
        pc += 1;
        let (lidx, jt, llen) = decode_load(bc, pc)?;
        pc += llen;
        if bc.get(pc) != Some(&0xb5) || pc + 2 >= bc.len() {
            return None; // must be putfield
        }
        let fidx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
        let fm = pool.member_ref(fidx)?;
        let fjt = desc_to_jt(fm.descriptor);
        if fjt != jt {
            return None; // loaded value type must match the field type
        }
        let arg_index = *slot_to_arg.get(&lidx)?;
        stores.push(TrivialCtorStore {
            field_owner: fm.owner.to_string(),
            field_name: fm.name.to_string(),
            jt: fjt,
            arg_index,
        });
        pc += 3;
    }
}

/// Cost guard: a method whose body is dominated by VM callbacks (invokes,
/// allocations, field/static access) runs no faster — often slower — as native
/// code than interpreted, because each callback costs more than the equivalent
/// interpreter step (the decisive A/B in rava-perf.md). Only methods with enough
/// genuinely-native work (arithmetic, branches, native array access) to outweigh
/// their callbacks should compile; the rest stay interpreted. Returns `true` to
/// bail. Invokes/allocations weigh 3 (each runs a whole method / allocates),
/// field & ref-element & instanceof callbacks weigh 1.
///
/// `inline_targets` holds the cp indices the JIT will INLINE (small monomorphic
/// invokestatic/invokespecial callees): those invokes become native code with no
/// callback boundary, so they count as native work — not a weight-3 callback.
/// This is what lets a wrapper dominated by small inlinable helpers compile.
pub(crate) fn callback_heavy(
    bc: &[u8],
    pool: &ConstantPool,
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    native_calls: &std::collections::HashSet<u16>,
    #[cfg(feature = "next-gen-gc")]
    hof_fusions: &std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
) -> bool {
    // TINY-method exemption (bridges, record hashCode/equals, 2-hop delegators, index
    // helpers): the interpreter FRAME cost dominates such a body, and the compiled
    // version makes exactly the same callbacks minus that frame — compiling cannot
    // lose. Default ON (opt-out RAVA_NO_TINY). Enabling this exposed — and led to the
    // fix of — the jit_indy primitive-result bug (record hashCode via ObjectMethods
    // was box_ref'd; the caller read box-tag bits as the hash → datapack tag maps
    // split → ReportedException; pinned by a 662-method Class#method deny bisect).
    // Measured: boot 180s → 166-167s (2 runs) with the hot EntrySet$1.next /
    // getFaceSupportIndex / bridge sinks compiled.
    {
        static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*OFF.get_or_init(|| std::env::var_os("RAVA_NO_TINY").is_some()) && bc.len() <= 16 {
            return false;
        }
    }
    // Basic blocks that TERMINATE in `athrow` are cold exception paths (a bounds-check
    // IndexOutOfBoundsException, a precondition failure): never the hot path, so their
    // invokes/allocs (building the exception message via StringBuilder, etc.) must NOT
    // count as callback weight — otherwise a trivial hot accessor like fastutil
    // `getDouble` (whose hot path is a single native array load) is wrongly judged
    // callback-heavy and stays interpreted for millions of boot calls.
    // Precompute, for each pc, whether it lies in an athrow-terminated basic block.
    let cold: std::collections::HashSet<usize> = {
        let mut set = std::collections::HashSet::new();
        if let Some(mut leaders) = find_leaders(bc) {
            leaders.sort_unstable();
            leaders.dedup();
            leaders.push(bc.len());
            for w in leaders.windows(2) {
                let (start, end) = (w[0], w[1]);
                if start >= bc.len() {
                    break;
                }
                let mut last = start;
                let mut p = start;
                while p < end {
                    last = p;
                    match insn_len_at(bc, p) {
                        Some(l) => p += l,
                        None => break,
                    }
                }
                if bc.get(last) == Some(&0xbf) {
                    let mut p = start;
                    while p < end {
                        set.insert(p);
                        match insn_len_at(bc, p) {
                            Some(l) => p += l,
                            None => break,
                        }
                    }
                }
            }
        }
        set
    };
    let mut native: i32 = 0;
    let mut callback: i32 = 0;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        let is_cold = cold.contains(&pc);
        match op {
            // invokevirtual/special/interface — heavy UNLESS this call site INLINES
            // (folds to native code), its callee is ALREADY COMPILED (takes the
            // guarded jit_direct_call native path), or it is on a cold exception path.
            // new/*newarray always allocate → heavy.
            0xb6 | 0xb7 | 0xb9 => {
                let idx = if pc + 2 < bc.len() {
                    ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16
                } else {
                    0
                };
                if is_cold || inline_targets.contains_key(&idx) || native_calls.contains(&idx) {
                    native += 1;
                } else {
                    // Under next-gen-gc, a virtual/interface site on a USER-BYTECODE
                    // owner compiles to the machine-code dispatch PIC — a hit is a
                    // direct compiled→compiled call, NOT a callback. Refusing to
                    // compile such methods left interface-heavy hot chains (every
                    // DensityFunctions$*.compute default method — the whole worldgen
                    // density graph) interpreting forever: PureTransformer.compute
                    // alone was 90k+ interp calls per 2 ring positions with
                    // compile_fail=1. A JDK/native-served owner (BitSet, DoubleList,
                    // guava) can never be IC-cached (dispatch_resolve_cacheable
                    // excludes it) → every call is the dispatch_miss callback — keep
                    // full callback weight so those methods stay interpreted.
                    // invokespecial (no PIC) keeps the callback weight too.
                    #[cfg(feature = "next-gen-gc")]
                    let pic_ok = op != 0xb7
                        && !arena_inline_disabled()
                        && pic_heur_enabled()
                        && pool.member_ref(idx).is_some_and(|m| {
                            !crate::opcodes::native_served_namespace(m.owner)
                                && !fu_routable(m.owner, m.name, m.descriptor)
                        });
                    #[cfg(not(feature = "next-gen-gc"))]
                    let pic_ok = false;
                    if pic_ok {
                        native += 1;
                    } else {
                        callback += 3;
                    }
                }
            }
            #[cfg(feature = "next-gen-gc")]
            0xba if is_cold => native += 1,
            #[cfg(feature = "next-gen-gc")]
            0xba if hof_fusions.contains_key(&pc) => native += 1,
            #[cfg(feature = "next-gen-gc")]
            0xba => callback += 3,
            // `new` under next-gen-gc is a cheap arena bump (inline / thin arena
            // callback that self-collects), so it no longer blocks compilation of
            // allocating hot loops. newarray/anewarray still allocate via callback.
            0xbb => {
                #[cfg(feature = "next-gen-gc")]
                {
                    native += 1;
                }
                #[cfg(not(feature = "next-gen-gc"))]
                {
                    callback += 3;
                }
            }
            0xbc..=0xbd if is_cold => native += 1,
            0xbc..=0xbd => callback += 3,
            // getstatic / putstatic / instanceof still callback. getfield/putfield
            // (0xb4/0xb5) and aaload/aastore are now NATIVE (heap migration), so
            // they count as native work below — don't penalize field-heavy methods.
            0xb2 | 0xb3 | 0xc0 | 0xc1 if is_cold => native += 1,
            0xb2 | 0xb3 | 0xc0 | 0xc1 => callback += 1,
            0x00 => {} // nop
            _ => native += 1,
        }
        pc += len;
    }
    callback > native
}

/// LOOP-FOLDING analyzer (next-gen-gc). Match a WHOLE method against the foldable
/// template (see `LoopFold`) and extract its plan, or `None`. Deliberately rigid: a
/// match guarantees the method computes exactly `acc_init + INV * trip`, so the
/// closed-form fold is sound. Only `long`-accumulator, int-INV, step-1, `i < LIM`
/// (`if_icmpge`) loops are matched — the shape `javac` emits for a counted
/// accumulation (e.g. `Statics.hot`). `ret` must be a `long` return.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_trivial_loop(bc: &[u8], ret: Option<JT>) -> Option<LoopFold> {
    if ret != Some(JT::Long) {
        return None;
    }
    let mut pc = 0usize;
    // Prologue: acc init (lconst/const → lstore acc) + iv init (iconst → istore iv).
    let (acc_init, l1) = match *bc.first()? {
        0x09 => (0i64, 1usize),                 // lconst_0
        0x0a => (1, 1),                          // lconst_1
        _ => return None,
    };
    pc += l1;
    let (acc_local, l2) = decode_store(bc, pc)?;
    // acc must be a long store (lstore family).
    if !matches!(bc[pc], 0x37 | 0x3f..=0x42) {
        return None;
    }
    pc += l2;
    let (c0, l3) = decode_iconst(bc, pc)?;
    pc += l3;
    let (iv_local, l4) = decode_store(bc, pc)?;
    // iv must be an int store.
    if !matches!(bc[pc], 0x36 | 0x3b..=0x3e) {
        return None;
    }
    pc += l4;

    // Header (the back-edge target): iload iv ; (iload L | iconst L) ; if_icmpge exit.
    let header = pc;
    let (h_iv, _, lh1) = decode_load(bc, pc)?;
    if !matches!(bc[pc], 0x15 | 0x1a..=0x1d) || h_iv != iv_local {
        return None; // must be `iload iv`
    }
    pc += lh1;
    let (limit_local, lim_const, lh2) = if matches!(bc[pc], 0x15 | 0x1a..=0x1d) {
        let (l, _, ln) = decode_load(bc, pc)?;
        (Some(l), 0, ln)
    } else if let Some((c, cn)) = decode_iconst(bc, pc) {
        (None, c, cn)
    } else {
        return None;
    };
    pc += lh2;
    if *bc.get(pc)? != 0xa2 {
        return None; // if_icmpge (loop runs while i < L)
    }
    let exit_pc = (pc as i32 + s16(bc, pc + 1)) as usize;
    pc += 3;

    // Body: lload acc ; <INV expr> ; i2l ; ladd ; lstore acc ; iinc iv,1 ; goto header.
    let (b_acc, _, lb1) = decode_load(bc, pc)?;
    if !matches!(bc[pc], 0x16 | 0x1e..=0x21) || b_acc != acc_local {
        return None; // `lload acc`
    }
    pc += lb1;
    let inv_start = pc;
    // Scan the invariant expression up to `i2l` (0x85).
    while *bc.get(pc)? != 0x85 {
        let op = bc[pc];
        if !is_invariant_expr_op(op) {
            return None;
        }
        pc += insn_len_at(bc, pc)?;
    }
    let inv_end = pc;
    if inv_end == inv_start {
        return None; // empty invariant — nothing to accumulate
    }
    pc += 1; // i2l
    if *bc.get(pc)? != 0x61 {
        return None; // ladd
    }
    pc += 1;
    let (s_acc, ls) = decode_store(bc, pc)?;
    if !matches!(bc[pc], 0x37 | 0x3f..=0x42) || s_acc != acc_local {
        return None; // `lstore acc`
    }
    pc += ls;
    // iinc iv, 1
    if *bc.get(pc)? != 0x84 || bc.get(pc + 1).copied()? as usize != iv_local || bc.get(pc + 2).copied()? != 1 {
        return None;
    }
    pc += 3;
    // goto header
    if *bc.get(pc)? != 0xa7 || (pc as i32 + s16(bc, pc + 1)) as usize != header {
        return None;
    }
    pc += 3;

    // Exit (must equal the if_icmpge target): lload acc ; lreturn — nothing else.
    if pc != exit_pc {
        return None;
    }
    let (e_acc, _, le) = decode_load(bc, pc)?;
    if !matches!(bc[pc], 0x16 | 0x1e..=0x21) || e_acc != acc_local {
        return None;
    }
    pc += le;
    if *bc.get(pc)? != 0xad {
        return None; // lreturn
    }
    pc += 1;
    if pc != bc.len() {
        return None; // trailing code → not a clean fold
    }

    Some(LoopFold { acc_init, c0, limit_local, lim_const, inv_range: (inv_start, inv_end) })
}

/// What `partial_inline_gc_safe` decided, and — when it admits — whether the splice it admits
/// still contains a GC-capable point.
///
/// ⭐**IT USED TO RETURN `bool`, AND THE MISSING HALF WAS THE BUG.** `Admit`/`No` is what the
/// caller needs to build the entry; `AdmitGcPoint` is what every OTHER reader of that entry needs
/// and never got. A `real_calls == 1` body has always been admitted here with no guard and no
/// rooted window, and `InlineCallee::leaves_gc_point()` then reported `false` for it — so
/// `field_loads_invariant` could stamp the caller's arena field loads `readonly` across a real
/// guest call. Returning the fact instead of dropping it is what closes that, and it is a
/// separate question from whether the ROOTING is covered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PartialSplice {
    /// Refused — not inlinable through this band.
    No,
    /// Admitted and FULLY COLLAPSED: every invoke folded or intrinsic'd, so nothing inside the
    /// splice can collect, allocate or write the heap.
    Admit,
    /// Admitted with one or more GC-capable points surviving inside the splice.
    AdmitGcPoint {
        /// At least one of the four rooting-derived clauses would have refused this body, i.e.
        /// it is admitted ONLY because `stackmaps::declaring()` discharges that proof. Always
        /// `false` in the OFF arm, so an ON/OFF census pair attributes the whole row to the
        /// protocol rather than to the flag.
        free_only: bool,
    },
}

/// ⭐**FOUR OF THIS PREDICATE'S FIVE REFUSALS ARE ONE ROOTING PROOF, AND `partial_free_enabled()`
/// IS ITS RETIREMENT** (`RAVA_NO_PARTIAL_FREE=1`, or the maps off, reads exactly as it did).
///
/// The clauses below say it in their own comments, four times, in the same words the interface
/// band used before `inline_free_enabled()` retired it there: *"emit_inline spills the callee's
/// `bvars` from offset 0 of the SHARED frame buffer — that covers the callee's state, not the
/// caller's."* Under `stackmaps::declaring()` no `spill_for_gc` writes that buffer at all; the
/// caller's locals, the callee's locals and both operand stacks are named by declaration at every
/// safepoint the splice introduces, including the CALLER's own stack values, which `emit_inline`
/// declares for every splice. The four clauses lose their subject together:
///
///   * `has_getstatic` beside a surviving call — its stated reason is the shared buffer, verbatim.
///     (The `<clinit>` half of the getstatic hazard is proved SEPARATELY and unconditionally, at
///     the `clinit_done` gate in `inline_target_for_exact_opts`, which no arm here can reach past.)
///   * `has_control_flow` — "any branch makes the *ref live across the call* analysis unsound".
///     That analysis is the tail walk below; with the maps naming the roots there is no analysis
///     to make unsound, and `emit_inline` has built basic blocks for a branchy splice since V4.2.
///   * `real_calls != 1` — "a ref result could be live across the second call". It is, and it is
///     mapped.
///   * the TAIL walk — the same sentence about the first call, spelled as an opcode whitelist.
///
/// ⛔**AND ONE THAT IS KEPT, BECAUSE IT IS NOT THAT PROOF.** `callee_footprint > max_frame` is the
/// frame-buffer CAPACITY bound every self-registering callee is admitted against — a bound on the
/// SHAPE, not on the argument — so it is enforced in both arms, exactly as
/// `iface_partial_gc_safe` keeps its twin.
///
/// ⚠**ADMITTING IS NOT THE SAME AS THE SPLICE BEING GC-FREE.** Whatever this returns, a surviving
/// call still moves and mutates the heap, so the verdict carries `AdmitGcPoint` and the entry
/// carries `partial_gc_points`. See `InlineCallee::leaves_gc_point`.
pub(crate) fn partial_inline_gc_safe(
    bc: &[u8],
    inlined: &std::collections::HashMap<u16, InlineCallee>,
    max_frame: usize,
    callee_footprint: usize,
    pool: &ConstantPool,
) -> PartialSplice {
    // THE MODE IS ASKED ONCE, HERE, and threaded down as a value. It is never re-read below and
    // never cached in a second place — see `partial_free_enabled`'s own warning.
    partial_inline_gc_safe_mode(
        bc,
        inlined,
        max_frame,
        callee_footprint,
        pool,
        crate::vm::inline_targets::partial_free_enabled(),
    )
}

/// The body of [`partial_inline_gc_safe`] with the mode as an explicit argument, so the unit
/// tests can assert BOTH arms without racing a process-wide `OnceLock`. No other caller passes
/// anything but the answer above.
pub(crate) fn partial_inline_gc_safe_mode(
    bc: &[u8],
    inlined: &std::collections::HashMap<u16, InlineCallee>,
    max_frame: usize,
    callee_footprint: usize,
    pool: &ConstantPool,
    free: bool,
) -> PartialSplice {
    use crate::vm::inline_census as cen;
    let mut real_calls = 0usize;
    let mut last_real_pc = 0usize;
    let mut has_control_flow = false;
    // GETSTATIC-INLINE: is_inlinable now admits getstatic, so a spliced body can contain one.
    // It is NOT free — it emits a `rava_getstatic` callback (box_ref for a ref static), i.e. a
    // GC-capable call this function otherwise knows nothing about. Tracked so the surviving-
    // real-call path below can refuse the combination.
    let mut has_getstatic = false;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return PartialSplice::No };
        // V4.2: control flow no longer refuses OUTRIGHT. A body with ZERO GC-capable real
        // calls is safe to splice via emit_inline (which builds basic blocks) even with
        // branches — no mid-body call ⇒ no ref-live-across-GC hazard, so the single-linear-
        // path invariant is unnecessary. Only TRACK it; refuse below iff a real call remains.
        if matches!(op, 0x99..=0xa8 | 0xaa | 0xab | 0xc6 | 0xc7 | 0xc8) {
            has_control_flow = true;
        }
        if op == 0xb2 {
            has_getstatic = true;
        }
        if matches!(op, 0xb6 | 0xb7 | 0xb8) {
            let cp = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            // Math.max/min(II|JJ) becomes a native smax/smin in emit_op — not a real call.
            let intrinsic = op == 0xb8 && math_minmax_noncall(pool, cp);
            // An entry that `leaves_gc_point()` is a real call for this predicate's purposes —
            // its splice still allocates or still dispatches.
            let folded = matches!(inlined.get(&cp), Some(t) if !t.leaves_gc_point());
            if !folded && !intrinsic {
                real_calls += 1;
                last_real_pc = pc;
            }
        }
        pc += len;
    }
    if real_calls == 0 {
        // Fully collapsed (all invokes inlined or intrinsic'd) ⇒ no GC-capable call ⇒
        // safe to splice via emit_inline INCLUDING branches (the V4.2 branchy relaxation).
        //
        // ⛔**A LONE `getstatic` IS DELIBERATELY STILL A COLLAPSE HERE, AND THAT IS A SCOPE
        // DECISION, NOT A PROOF.** A ref `getstatic` emits a `rava_getstatic` callback that
        // `box_ref`s — a heap event this verdict does not report. It is left alone because the
        // LEAF band (`is_inlinable_kind` admits 0xb2 outright, `nested.is_empty()`) produces
        // exactly the same entry with the same `leaves_gc_point() == false`, so marking it here
        // would tighten one band and leave the wider one untouched — a half-fix that costs
        // coverage and proves nothing. The `<clinit>` half, which is the one that can run
        // arbitrary Java, IS proved, unconditionally and for every band, by the `clinit_done`
        // gate in `inline_target_for_exact_opts`.
        return PartialSplice::Admit;
    }
    // GETSTATIC-INLINE: a getstatic AND a surviving real call must never share a body. The
    // reasoning below only tracks the CALLEE's refs across that call; the CALLER's live refs
    // are NOT rooted across it, because emit_inline spills the callee's `bvars` from offset 0
    // of the SHARED frame buffer — that covers the callee's state, not the caller's. A body
    // that fully collapses (real_calls == 0, returned above) has no GC point, so it is safe;
    // this is exactly the ImprovedNoise.gradDot shape (getstatic + an INLINED invokestatic).
    // What this refuses is the enum values() shape — `getstatic $VALUES; invokevirtual
    // clone(); checkcast; areturn` — whose array-clone survives as a real call AND allocates:
    // it passed the tail check, spliced, and a GC inside clone() left the caller's refs stale,
    // surfacing as `UnsatisfiedLinkError: Integer.isEmpty()` (a ref arriving as Int(0)).
    // ROOTING-DERIVED (1/4). Retired by `free` — the sentence it states is the shadow frame.
    if has_getstatic && !free {
        cen::note(cen::X_PF_GETSTATIC);
        return PartialSplice::No;
    }
    // ROOTING-DERIVED (2/4). A real call remains ⇒ the single-linear-path invariant applies: any
    // branch makes the "ref live across the call" analysis unsound, so refuse control flow here.
    // Under `free` there is no such analysis — the maps name the roots at every safepoint in
    // every block — and `emit_inline` has built basic blocks for a branchy splice since V4.2.
    if has_control_flow && !free {
        cen::note(cen::X_PF_BRANCH);
        return PartialSplice::No;
    }
    // ⛔**KEPT IN BOTH ARMS.** The frame-buffer CAPACITY bound each self-registering callee is
    // admitted against — a bound on the shape, not the rooting proof. `iface_partial_gc_safe`
    // keeps its twin for the same reason, and on the same side of its own `free` test.
    if callee_footprint > max_frame {
        cen::note(cen::X_PF_FRAME);
        return PartialSplice::No;
    }
    // ROOTING-DERIVED (3/4): "a ref result could be live across the second call". It is — and
    // under the maps it is named at that call and forwarded in place.
    if real_calls != 1 && !free {
        cen::note(cen::X_PF_CALLS);
        return PartialSplice::No;
    }
    // ROOTING-DERIVED (4/4). TAIL: after the single real call, only result-forwarding ops
    // (nothing reads a local/heap). Under `free` it is not a gate any more, but it is still
    // EVALUATED, because "would the historic clauses have refused this body" is the round's
    // population and the only honest way to count it is to ask all four in the same walk.
    let tail_ok = real_calls == 1 && tail_only_forwards(bc, last_real_pc);
    if free {
        // ⭐**THE ROUND'S POPULATION, COUNTED AT THE SITE THAT CREATES IT.** `free_only` is true
        // exactly when at least one of the four rooting-derived clauses above would have said no.
        return PartialSplice::AdmitGcPoint {
            free_only: has_getstatic || has_control_flow || real_calls != 1 || !tail_ok,
        };
    }
    if !tail_ok {
        cen::note(cen::X_PF_TAIL);
        return PartialSplice::No;
    }
    // ⭐**THE HISTORIC ADMISSION, NOW TELLING THE TRUTH ABOUT ITSELF.** This is the
    // `real_calls == 1 + forwarding tail` population that has always spliced with no guard and no
    // window. Its verdict changes from `true` to `AdmitGcPoint`, and that single word is what
    // `field_loads_invariant`, `method_no_alloc`, `no_alloc_and_loop` and the field-promotion
    // walk needed and never had.
    PartialSplice::AdmitGcPoint { free_only: false }
}

/// The TAIL half of [`partial_inline_gc_safe_mode`]'s fourth rooting-derived clause, split out so
/// the `free` arm can ASK it (to count the population it newly admits) without it being a gate.
fn tail_only_forwards(bc: &[u8], last_real_pc: usize) -> bool {
    let Some(call_len) = insn_len_at(bc, last_real_pc) else { return false };
    let mut pc = last_real_pc + call_len;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            0xc0 => return false, // checkcast may allocate an exception after the call
            0x85..=0x93 => {} // i2l/i2f/…/i2s numeric conversions of the result
            0xac..=0xb0 => {} // ireturn/lreturn/freturn/dreturn/areturn
            _ => return false, // aload/getfield/invoke/store/dup/arith after the call → refuse
        }
        pc += len;
    }
    true
}
