//! Inline admission: what a callee has to be for its body to be pasted into a caller.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

pub(crate) fn analyze_trivial_getter(bc: &[u8], pool: &ConstantPool) -> Option<(String, String, JT)> {
    if bc.len() != 5 || bc[0] != 0x2a || bc[1] != 0xb4 {
        return None; // aload_0 ; getfield ; Xreturn
    }
    let fidx = ((bc[2] as u16) << 8) | bc[3] as u16;
    let fm = pool.member_ref(fidx)?;
    let jt = desc_to_jt(fm.descriptor);
    let ret_ok = matches!(
        (jt, bc[4]),
        (JT::Int, 0xac) | (JT::Long, 0xad) | (JT::Float, 0xae) | (JT::Double, 0xaf) | (JT::Ref, 0xb0)
    );
    if !ret_ok {
        return None; // return opcode must match the field width
    }
    Some((fm.owner.to_string(), fm.name.to_string(), jt))
}

/// Weaker than `field_loads_invariant`: `true` iff the method performs NO allocation and NO
/// non-inlined call — i.e. no GC / heap-move point can occur while it runs. Crucially it ALLOWS
/// putfield/putstatic (writing a cell changes a field VALUE but never moves the object or the
/// ObjDesc table). Under this, an object's DESCRIPTOR structure (the ObjDesc base, its ids/cells
/// pointers and field count) is loop-invariant even in a method that mutates fields — so those
/// loads can be marked readonly and CSE'd across getfields, while the cell VALUE load stays
/// mutable. This is what lets a putfield-bearing hot loop (e.g. bench/Fields) hoist the per-field
/// descriptor re-decode that `invariant_fields` (which bails on any putfield) cannot.
#[allow(dead_code)]
pub(crate) fn method_no_alloc(
    bc: &[u8],
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            0xbb | 0xbc | 0xbd | 0xc5 | 0xbf => return false, // allocation, including athrow null
            0xb6 | 0xb7 | 0xb9 => {
                let idx = if pc + 2 < bc.len() { ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16 } else { return false };
                // ⚠MEMBERSHIP IS NOT ENOUGH — see `InlineCallee::leaves_gc_point`. A guarded
                // entry still dispatches on its miss arm and a ROOTED entry still allocates
                // and/or calls inside the splice, so neither is "no GC point".
                match inline_targets.get(&idx) {
                    Some(t) if !t.leaves_gc_point() => {}
                    _ => return false,
                }
            }
            0xb8 => {
                let idx = if pc + 2 < bc.len() { ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16 } else { return false };
                let folded = matches!(inline_targets.get(&idx), Some(t) if !t.leaves_gc_point());
                if !folded && !static_intrinsics.contains_key(&idx) {
                    return false;
                }
            }
            _ => {}
        }
        pc += len;
    }
    true
}

/// Is the method body GC-LEAF — i.e. contains no opcode that can trigger a
/// garbage collection or arena move (no invoke, no allocation, no getstatic/
/// putstatic that could run a `<clinit>`, no instanceof/multianewarray class load,
/// no athrow/monitor)? getfield/putfield and array loads/stores are allowed (their
/// callbacks read/write but never allocate). A GC-leaf callee never spills, so the
/// dispatch-IC can call it on a non-zeroed buffer (the slots beyond its args are
/// never read by a collector) and the caller needs no spill/reload across the call.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn is_gc_leaf(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            0xb2 | 0xb3                      // get/putstatic (<clinit>)
            | 0x12 | 0x13                    // ldc / ldc_w — a Class (or non-internable String)
                                             // constant routes to env.ldc, which ALLOCATES and
                                             // can move the arena (emit_ldc_ref spills for it).
                                             // A "leaf" callee cached in the dispatch IC is called
                                             // via the HIT arm with NO spill (its caller's live
                                             // arena refs thread unchanged) — so an ldc-carrying
                                             // callee MUST NOT be treated as a GC-leaf, else the
                                             // caller's refs go stale across the cached call.
            | 0xb6 | 0xb7 | 0xb8 | 0xb9 | 0xba  // invoke* + invokedynamic
            | 0xbb | 0xbc | 0xbd | 0xc5      // new / newarray / anewarray / multianewarray
            | 0xc0 | 0xc1                    // checked cast / instanceof (class load, throw)
            | 0xbf                            // athrow (unwind)
            | 0xc2 | 0xc3 => return false,   // monitorenter / monitorexit
            _ => {}
        }
        pc += len;
    }
    true
}

/// 20.4 deep-inline (C2-style, gated RAVA_JIT_TIER0): under Tier-0 the inliner goes NON-LEAF —
/// depth 3→8 and a nested callee inlines its body while leaving any non-inlinable call as a real
/// dispatch mid-body (partial nested map). Non-leaf callees are capped at INLINE_MAX_BYTECODE so
/// the depth-8 splice can't blow up code size. Default (no Tier-0) keeps the conservative 16.x path.
/// `RAVA_INLINE_DEPTH=<n>` overrides the chained-inline depth. Default is unchanged (8 under
/// Tier-0, else 3), so an ordinary build is byte-identical.
///
/// WHY THE OVERRIDE EXISTS. The depth cap has only ever been swept UPWARD here — the recorded
/// inlining verdict raised `INLINE_MAX_BYTECODE` and found the boot regressing monotonically
/// (86.9 -> 93.0s) — and the DOWNWARD direction was untestable because this returned a literal.
/// Dumping the ring's hottest method makes the question concrete: at depth 8,
/// `DensityFunctions$ShiftedNoise.compute(FunctionContext)D` is **852 blocks and ~8900
/// instructions for four lines of Java**, because the whole density-combinator tree is spliced
/// into it. The standing code-size lesson points the same way: a boot COMPILES ONCE AND MOVES ON
/// (~2000 bodies, ~4.5s of Cranelift on the serial critical path), so a body-duplicating
/// optimisation pays its cost every time and collects its benefit only where a body is re-entered
/// enough to matter. Less inlining should cut compile time AND instruction-cache pressure.
///
/// That is an argument, not a result. The A/B is what decides it.
/// ⛔MEASURED IN BOTH DIRECTIONS, 2026-08-15 — THE CAP DOES NOT MATTER. ChunkFill
/// 1×64, n=11 each, paired and hash-verified (`bench/gate_ab.ps1`):
///
/// ```text
/// RAVA_INLINE_DEPTH=12   -1.37%   6/11 pairs
/// RAVA_INLINE_DEPTH=3    -0.53%   6/11
/// RAVA_INLINE_DEPTH=2    +1.21%   4/11
/// ```
///
/// Halving the depth costs about a percent, tripling it buys about a percent,
/// and the default sits between. The worry recorded below — that depth 8 splices
/// a density-combinator tree into 852 blocks — is real as a code-size fact and
/// costs nothing measurable in time. The downward direction this override was
/// added to test has now been tested.
pub(crate) fn max_inline_depth() -> u8 {
    static N: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        if let Some(n) = std::env::var("RAVA_INLINE_DEPTH")
            .ok()
            .and_then(|v| v.trim().parse::<u8>().ok())
        {
            return n;
        }
        if crate::jit_tier0() {
            8
        } else {
            MAX_INLINE_DEPTH
        }
    })
}
/// Phase-2 experiment (RAVA_INLINE_MAX_BYTECODE): the non-leaf deep-inline size cap. The hot worldgen
/// density combinators are monomorphic (78% of sites per the devirt probe) but bigger than 35 bytes,
/// so they aren't spliced into the caller → Cranelift optimises them in isolation while C2 inlines the
/// whole tree. Raising this cap inlines more of the monomorphic density chain, exposing it to
/// Cranelift SROA/LICM/const-fold. Bit-exact (only already-is_inlinable-safe callees; inlining is
/// semantically transparent). Trade-off = bigger compiled bodies + longer compile vs faster exec.
pub(crate) fn inline_max_bytecode() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_INLINE_MAX_BYTECODE")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(INLINE_MAX_BYTECODE)
    })
}

// ---------------------------------------------------------------------------
// WIDE LEAF INLINING (`RAVA_NO_WIDE_INLINE=1` to restore the old numbers exactly)
// ---------------------------------------------------------------------------
//
// A call that is inlined makes NO host crossing. Inside the compiled region 76.1% of the time is
// host callbacks, so admission width is the most direct lever on that ratio — and the leaf budget
// was a bare literal 96 with nothing behind it. HotSpot ships TWO budgets: `MaxInlineSize` = 35 for
// a callee it has no reason to believe is hot, and `FreqInlineSize` = 325 once it is. This engine
// under `RAVA_JIT_TIER0` compiles a method on its FIRST ENTRY and never recompiles it, so there is
// no second chance: a callee refused here is a crossing for the life of the process. There is also
// no legitimate way to ask "is it hot" — timings are forbidden as a routing input — so the only
// coherent choice is ONE budget, and the cold one is the wrong one to standardise on.
//
// The widened band is LEAF-ONLY on purpose. A leaf (`allow_invokes == false`) contains no invoke at
// all, so splicing it cannot cascade: code growth is exactly `bc.len()` per site, linear and
// bounded, with none of the depth-8 multiplication that turned `ShiftedNoise.compute` into 852
// blocks. The non-leaf path keeps its own separate caps (`max_inline_depth`, `inline_max_bytecode`,
// `partial_inline_gc_safe`, `callee_footprint > max_frame`) untouched.
//
// WHY A SIZE WIDENING IS SAFE WHERE AN OPCODE WIDENING WOULD NOT BE. The wide band admits a
// STRICT SUBSET of the narrow band's opcode set (it additionally excludes ldc/ldc_w, getstatic and
// the switches). Every invariant the rest of the compiler asserts about an inlined callee is
// phrased over OPCODES, never over length, and each of those assertions is checked in a file that
// walks only the CALLER's bytecode and trusts `inline_targets` membership as proof about the
// callee — `field_loads_invariant` (range.rs) marks the caller's field loads `readonly`,
// `method_no_alloc` (below) declares no GC point, `promo_scan_body` (licm.rs) drops the
// putfield/invoke cases. Widening only the length therefore leaves every one of them true by
// construction. Widening an OPCODE (putfield, array-store, new) would silently falsify them from
// here, which is why this change does not touch the opcode list.

/// The classic leaf budget. A body at or under this is admitted exactly as before — the wide band
/// only ever ADDS callees, never removes one, so `RAVA_NO_WIDE_INLINE=1` is a true kill switch.
pub(crate) const INLINE_LEAF_BYTECODE: usize = 96;

/// HotSpot `FreqInlineSize`. Override with `RAVA_INLINE_LEAF_MAX=<bytes>`.
pub(crate) const WIDE_INLINE_LEAF_MAX: usize = 325;

/// A widened leaf declares one Cranelift `Variable` per local slot at every splice site. The byte
/// budget does not bound `max_locals` (the attribute is independent of code length), so bound it
/// directly rather than let a pathological frame declaration explode the caller's IR.
const WIDE_INLINE_MAX_LOCALS: usize = 64;

/// Kill switch, consulted at EMIT time (admission runs during target collection, i.e. inside the
/// compile) and ANDed with the global `emit_more_disabled` bisect gate.
pub(crate) fn wide_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_WIDE_INLINE").is_none() && !crate::flags::emit_more_disabled()
    })
}

fn wide_inline_leaf_max() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_INLINE_LEAF_MAX")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(WIDE_INLINE_LEAF_MAX)
    })
}

/// THE PROOF THE WIDE BAND RESTS ON: the body emits no `spill_for_gc`.
///
/// `emit_inline` splices a callee by declaring its locals as fresh Cranelift `Variable`s and
/// running its body through the caller's `emit_op` with the CALLER's `locals_ptr`. Any GC-capable
/// callback inside that body calls `spill_for_gc(b, &bvars, ..., locals_ptr)`, which writes
/// `bvars.len()` cells from offset 0 of a buffer the CALLER sized to its own
/// `max_locals + max_stack`. So a callee whose frame is larger than its caller's overruns that
/// buffer, and (the 20.4 crash class) the caller's own live references are not rooted across the
/// callback because the spill covers the CALLEE's state only. Today's 96-byte leaves keep that
/// hazard small by accident; a 325-byte one would not.
///
/// Rather than reason about frame sizes this admission cannot see, the wide band removes the
/// premise: it admits only bodies where no opcode reaches a `spill_for_gc` at all, so `locals_ptr`
/// is never written and `max_locals` is irrelevant to memory safety. Enumerating the spilling
/// opcodes against the emitters, everything else is already refused by `is_inlinable` — putstatic,
/// instanceof, every allocation, every invoke (leaf mode), and `wide`/`jsr`/monitors (which have no
/// `insn_len`, so `find_leaders` rejects them). What is left to exclude here is exactly two:
///   * `ldc` / `ldc_w` (0x12/0x13) — a String/Class constant routes to `rava_ldc`, which allocates
///     and can move the arena (`emit_ldc`, misc.rs). `ldc2_w` (0x14) is long/double only: allowed.
///   * `getstatic` (0xb2) — both arms (cached diamond and generic) spill, because the callback can
///     run a `<clinit>`. It stays admitted in the NARROW band, where the collector-side gate in
///     `inline_target_for_call` proves `clinit_done` first.
/// `athrow` is allowed: `rava_throw` only latches the pending exception (no allocation, no spill),
/// and `getfield`/array loads/`arraylength`/`checkcast`/`if_acmp` emit no spill in their arms.
fn wide_leaf_spill_free(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        if matches!(op, 0x12 | 0x13 | 0xb2) {
            return false;
        }
        pc += len;
    }
    true
}

/// The size gate `is_inlinable` applies. Narrow band = unchanged. Wide band = leaf-only, spill-free,
/// bounded frame. Split out so `inlinable_bail_reason` cannot drift from it.
pub(crate) fn inline_size_admitted(code: &Code, allow_invokes: bool) -> bool {
    let bc = &code.bytecode;
    if bc.len() <= INLINE_LEAF_BYTECODE {
        return true;
    }
    // Non-leaf callees keep the old cap: their splice cascades through `max_inline_depth`, so code
    // growth is multiplicative and the 852-block worldgen shape lives there.
    if allow_invokes || !wide_inline_enabled() {
        return false;
    }
    bc.len() <= wide_inline_leaf_max()
        && code.max_locals as usize <= WIDE_INLINE_MAX_LOCALS
        && wide_leaf_spill_free(bc)
}

/// A pure primitive LEAF (I1a scope): no GC point (no invoke/new/field/array/athrow/
/// monitor) and no ref-bearing ldc — so the register entry needs neither the self-reg
/// GC-root buffer nor spill. Conservative: rejects ldc/ldc_w (a String/Class constant
/// would push a Ref); double constants use ldc2_w (0x14), which stays allowed.
pub(crate) fn is_prim_leaf_bc(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            0x12 | 0x13 => return false,        // ldc / ldc_w (may be a ref constant)
            0x2e..=0x35 => return false,        // array loads
            0x4f..=0x56 => return false,        // array stores
            0xb2..=0xba => return false,        // get/putstatic/field, invoke*
            0xbb..=0xbe | 0xc5 => return false, // new / newarray / anewarray / arraylength / multianewarray
            0xbf => return false,               // athrow
            0xa8 | 0xa9 | 0xc9 => return false, // jsr / ret / jsr_w
            0xc2 | 0xc3 => return false,        // monitorenter / monitorexit
            0xc6 | 0xc7 => return false,        // ifnull / ifnonnull (ref compare)
            0xb0 => return false,               // areturn (ref return — not primitive)
            _ => {}
        }
        pc += len;
    }
    true
}

/// One `JT` per DECLARED parameter of `descriptor`, in order (the receiver is not included — a
/// descriptor does not mention it). Same walk as `arg_slot_widths`, which answers the WIDTH of
/// each parameter; this answers its KIND, and the two must agree position for position.
///
/// The lambda SAM chain needs it for a check nothing else in the inliner has ever had to make: at
/// an ordinary site the operand values and the callee parameters come from ONE descriptor, so they
/// agree by construction. At a SAM site there are TWO — the interface method the bytecode calls
/// and the implementation method the closure names — and `LambdaMetafactory` is allowed to adapt
/// between them (box, widen, cast). This engine has no adapter: `invoke_lambda_parts` hands the
/// SAM's argument values straight to the implementation and pushes its return value straight back.
/// The interpreter survives that because a `Value` carries its own tag; the compiler does not,
/// because its operand stack is statically typed. So the admission compares the two descriptors
/// position by position and refuses any site where they disagree, rather than emitting a splice
/// that would put a reference cell into an int-typed slot.
pub(crate) fn descriptor_param_jts(descriptor: &str) -> Vec<JT> {
    let b = descriptor.as_bytes();
    let mut out = Vec::new();
    let mut i = match descriptor.find('(') {
        Some(p) => p + 1,
        None => return out,
    };
    while i < b.len() && b[i] != b')' {
        // An array of anything is one narrow reference: consume the `[`s and the element
        // WITHOUT reading a `J`/`D` element as a wide scalar (the `arg_slot_widths` rule).
        let was_array = b[i] == b'[';
        let start = i;
        while i < b.len() && b[i] == b'[' {
            i += 1;
        }
        match b.get(i) {
            Some(b'L') => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1;
                out.push(JT::Ref);
            }
            Some(_) => {
                i += 1;
                out.push(if was_array { JT::Ref } else { desc_to_jt(&descriptor[start..]) });
            }
            None => break,
        }
    }
    out
}

/// Any long/double parameter in the descriptor? Wide params take TWO local slots,
/// which would break emit_inline's slot mapping (it places one value per slot), so
/// such callees are not inlined. (Wide *internal* locals are fine — the body's own
/// store/load ops index them correctly.)
pub(crate) fn has_wide_params(descriptor: &str) -> bool {
    let b = descriptor.as_bytes();
    let mut i = match descriptor.find('(') { Some(p) => p + 1, None => return false };
    while i < b.len() && b[i] != b')' {
        // An array of anything is a single narrow reference — consume the `[`s and
        // its element WITHOUT treating a J/D element as a wide scalar.
        let was_array = b[i] == b'[';
        while i < b.len() && b[i] == b'[' { i += 1; }
        match b.get(i) {
            Some(b'L') => { while i < b.len() && b[i] != b';' { i += 1; } i += 1; }
            Some(b'J') | Some(b'D') if !was_array => return true,
            Some(_) => i += 1,
            None => break,
        }
    }
    false
}

/// Is `code` a side-effect-free leaf suitable for inlining? Arithmetic / stack /
/// locals / numeric-ldc / branches / a non-void return — but NO invokes,
/// allocations, field/array/static access, instanceof, or exception handlers.
/// Branches are allowed (emit_inline builds the callee's own basic blocks); the
/// callee body translates via the SAME emit_op as the caller, in the caller's
/// frame, so there is no runtime call and no divergence risk.
/// DIAGNOSTIC (RAVA_DBG_INLBAIL=1, inert otherwise): why `is_inlinable` refused a callee,
/// tallied per reason across a boot. The getstatic gate ([[Getstatic-Inline]]) proved these
/// structural refusals — not per-op codegen — are where the boot time is, so this ranks the
/// REMAINING gates by how many real call sites each one blocks instead of by guesswork.
/// Reasons are counted per inline-target ATTEMPT, so a hot site refused on every compile
/// counts every time — a crude but useful hotness weighting.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inlbail_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DBG_INLBAIL").is_some())
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn inlbail_note(reason: &str, callee: &str) {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static T: OnceLock<Mutex<HashMap<String, (u64, HashMap<String, u64>)>>> = OnceLock::new();
    let mut g = T
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let e = g.entry(reason.to_string()).or_insert((0, HashMap::new()));
    e.0 += 1;
    *e.1.entry(callee.to_string()).or_insert(0) += 1;
    // Dump on the TOTAL refusal count, not per-reason: a rare-but-blocking gate would never
    // reach a per-reason threshold, and a server boot has no clean exit to print from.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if n % 500 != 0 {
        return;
    }
    // Periodic dump: a server boot never exits cleanly, so there is no exit hook to print from.
    let mut rows: Vec<(&String, &(u64, HashMap<String, u64>))> = g.iter().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    eprintln!("[INLBAIL] ---- inline refusals by reason (attempts) ----");
    for (r, (n, callees)) in rows.iter().take(12) {
        let mut cs: Vec<(&String, &u64)> = callees.iter().collect();
        cs.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = cs.iter().take(3).map(|(c, n)| format!("{c}({n})")).collect();
        eprintln!("[INLBAIL] {n:>9}  {r:<22} top: {}", top.join(", "));
    }
}

/// The first structural reason `is_inlinable` would refuse `code`, or None if it accepts.
/// Mirrors is_inlinable's order exactly — keep the two in sync. It mirrors the DEFAULT answer,
/// i.e. `is_inlinable_kind(.., allow_void = false)`: a SAM callee admitted with `allow_void` never
/// reaches this logger, so the `void-return` row keeps meaning what it always meant.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn inlinable_bail_reason(
    code: &Code,
    descriptor: &str,
    allow_invokes: bool,
) -> Option<&'static str> {
    if !code.exceptions.is_empty() {
        return Some("exception-table");
    }
    let bc = &code.bytecode;
    if bc.is_empty() {
        return Some("empty");
    }
    if !inline_size_admitted(code, allow_invokes) {
        return Some("size");
    }
    if ret_jt(descriptor).is_none() {
        return Some("void-return");
    }
    let wide = wide_inline_enabled();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else {
            return Some("undecodable");
        };
        match op {
            0xb3 => return Some("putstatic"),
            0xb5 => return Some("putfield"),
            0xb9 => return Some("invokeinterface"),
            0xba => return Some("invokedynamic"),
            0xbb => return Some("new"),
            0xbc | 0xbd => return Some("newarray"),
            0xc1 => return Some("instanceof"),
            0xaa | 0xab if wide => return Some("switch"),
            0x4f..=0x56 => return Some("array-store"),
            0xb6 | 0xb7 | 0xb8 => {
                if !allow_invokes {
                    return Some("invoke(leaf-mode)");
                }
            }
            _ => {}
        }
        pc += len;
    }
    if find_leaders(bc).is_none() {
        return Some("bad-leaders");
    }
    None
}

/// THE FOUR REFUSALS EXAMINED AND KEPT, so the next round does not re-derive them:
///
/// * **exception table** — real. The splice models control flow with `find_leaders`, which knows
///   nothing about handler ranges; a spliced body's throw would unwind past a handler the caller
///   never built.
/// * **`invokedynamic` (0xba)** — real. Needs a bootstrap linkage the inliner cannot perform.
/// * **`invokeinterface` (0xb9)** — real, and NOT merely conservative. `partial_inline_gc_safe`
///   (trivial.rs) counts surviving real calls over `0xb6|0xb7|0xb8` only, so an admitted 0xb9 would
///   read as "fully collapsed", and `try_collect_inline_targets` never builds a nested entry for it.
///   The result is a real GC-capable dispatch mid-splice whose caller-side references are not
///   rooted — the 20.4 crash class, exactly.
/// * **`putfield` / `putstatic` / array-store / `new` / `newarray` / `instanceof`** — each would
///   falsify an invariant asserted OUTSIDE this file, where the check walks only the caller and
///   treats `inline_targets` membership as proof about the callee: `field_loads_invariant`
///   (range.rs) would keep marking the caller's field loads `readonly` across a spliced store, and
///   the allocating opcodes would additionally break `method_no_alloc` and `emit_inline`'s "inline
///   callees never contain `new`" precondition (it passes `None` for the arena-new context).
///   `instanceof` also spills and would need the VM-state proof the getstatic gate does, which
///   lives in `inline_target_for_call`. None of those files are reachable from here, so the opcode
///   list is unchanged and only the LENGTH budget moved.
/// * **void return** — ✅**LIFTED, and it was exactly what this note said it was.** A refusal about
///   the RETURN VALUE, not about safety: purely `emit_inline`'s shape (it typed one shared
///   continuation block on `ret_jt` and hard-`None`d on 0xb1). `emit_inline` now has both shapes,
///   so the gate moved behind an `allow_void` parameter — see `is_inlinable_kind`. It is passed
///   `true` by the lambda SAM chain alone, because a void body that can neither `putfield`,
///   `putstatic`, array-store nor allocate has no observable effect anywhere else.
pub(crate) fn is_inlinable(code: &Code, descriptor: &str, allow_invokes: bool) -> bool {
    is_inlinable_kind(code, descriptor, allow_invokes, false)
}

/// ⭐**BREAK (3): THE VOID REFUSAL, LIFTED — AND LIFTED ONLY WHERE IT WAS ASKED FOR.**
///
/// The comment on the four kept refusals named this one as the one worth revisiting, and named it
/// correctly: *"a refusal about the RETURN VALUE, not about safety … purely `emit_inline`'s shape
/// (it types one shared continuation block on `ret_jt` and hard-`None`s on 0xb1)"*. `emit_inline`
/// now has that shape — a void body takes no continuation parameter, its `return` (0xb1) jumps to
/// an argument-less continuation on the general path and simply ends the splice on the
/// straight-line path, and neither pushes anything, so the caller's operand stack is left exactly
/// as the call would have left it.
///
/// ⚠**`allow_void` IS A PARAMETER RATHER THAN A NEW DEFAULT, ON PURPOSE.** Lifting the refusal
/// for every callee would widen the ordinary inliner (statics, specials, final-virtuals) as a side
/// effect of a lambda change, and the population it would add is close to worthless anyway: a void
/// body that can neither `putfield`, `putstatic`, array-store, allocate nor `invokeinterface` has
/// no observable effect at all, so admitting it buys code growth for a body whose only legal
/// content is dead. The population that IS worth having is the SAM one — `Consumer.accept`, an
/// `IntLineConsumer`, every void functional interface — where under Tier-0's partial nested map a
/// surviving real dispatch mid-body is the side effect. So exactly one caller passes `true`:
/// `Vm::inline_target_for_exact_opts`, for a callee admitted by the lambda SAM chain.
///
/// `is_inlinable` keeps its old signature and its old answer, so no caller outside this round
/// changes and `RAVA_NO_LAMBDA_INLINE_CHAIN=1` restores byte-identical admission.
pub(crate) fn is_inlinable_kind(
    code: &Code,
    descriptor: &str,
    allow_invokes: bool,
    allow_void: bool,
) -> bool {
    if !code.exceptions.is_empty() {
        return false;
    }
    let bc = &code.bytecode;
    if bc.is_empty() || !inline_size_admitted(code, allow_invokes) {
        return false;
    }
    if ret_jt(descriptor).is_none() && !allow_void {
        // void — `emit_inline` types its shared continuation block on `ret_jt`, so a void callee
        // has no block param to build. Admitted only for the lambda SAM chain, where the void
        // splice above is the whole point; see `is_inlinable_kind`'s doc.
        return false;
    }
    // 16.2: wide params (double/long) are now supported — emit_inline places each arg at its
    // cumulative 2-slot-aware local index (arg_slot_widths), leaving the dead pair-slot unused.
    // A data-parallel REDUCE (a whole-method counted associative fold, e.g. dot-product
    // `sum += a[i]*b[i]`) must compile as its OWN method so the fork fires — inlining it into
    // a caller would bury the loop mid-body where the whole-method analyzer can't see it, so
    // it would run serially. (Void maps are already excluded by the ret-is-none check above.)
    // Every data-parallel shape is a counted LOOP, so a method with no back-edge can't be one
    // — the cheap `has_back_edge` scan gates the five (heavier) analyzers away from the vast
    // majority of inline candidates (getters/small helpers), which have no loop.
    #[cfg(feature = "next-gen-gc")]
    if jit_parallel_enabled()
        && has_back_edge(bc)
        && (analyze_data_parallel_loop(bc, ret_jt(descriptor)).is_some()
            || analyze_data_parallel_count(bc).is_some()
            || analyze_data_parallel_filter(bc).is_some()
            || analyze_data_parallel_argmax(bc).is_some()
            || analyze_data_parallel_reduce2(bc).is_some())
    {
        // The int-returning parallel shapes (count/filter/argmax) must compile as their OWN
        // method for the whole-method analyzer to fire the fork; inlining buries the loop.
        return false;
    }
    let wide = wide_inline_enabled();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if is_excluded(op) {
            return false;
        }
        let Some(len) = insn_len_at(bc, pc) else { return false };
        match op {
            // SWITCHES ARE A LANDMINE, NOT A BUDGET QUESTION — and this refusal makes MORE code
            // compile, not less. `is_inlinable` and `find_leaders` walk with `insn_len_at`, which
            // decodes the variable-length 0xaa/0xab payload, so a switch-bearing callee passes
            // admission today. `emit_inline` then walks the spliced body with plain `insn_len(op)`,
            // which has no arm for 0xaa/0xab and returns `None` — and that `None` propagates out
            // through `emit_op`, so the callee does not merely fail to inline: THE WHOLE CALLER
            // FAILS TO COMPILE and falls back to the interpreter. Refusing the callee here leaves a
            // real dispatch at that one site and lets the caller compile. Small switches fit inside
            // 96 bytes, so this is live at the old budget too; the wide band would only meet it far
            // more often. Gated with the rest so the kill switch restores byte-identical behaviour.
            0xaa | 0xab if wide => return false,
            // putstatic/putfield, interface/dynamic invokes, allocations, instanceof —
            // not inlinable (need the callee's class context / are side-effecting heap
            // writes). getfield (0xb4) IS allowed (pre-resolved field map).
            0xb3 | 0xb5 | 0xb9 | 0xba | 0xbb..=0xbd | 0xc1 => return false,
            // getstatic (0xb2) IS allowed: emit_inline already gives the callee body its own
            // class_ptr and an EMPTY `statics` map, so the read falls to the generic
            // rava_getstatic callback, which resolves (callee_class_ptr, callee_cp) against the
            // CALLEE's pool — correct by construction, no cp-collision. This is a STRUCTURAL
            // admission only: the splice is sound solely when the field's class is already
            // clinit-DONE, which needs VM state that is_inlinable cannot see, so
            // inline_target_for_call gates it (see `getstatic_refs` there). Without that gate
            // the callback could run <clinit> → arbitrary Java → a moving GC, and the spliced
            // body spills only the CALLEE's vars, leaving the CALLER's live refs unrooted.
            0xb2 => {}
            // invokestatic/special/virtual: leaf mode rejects; nested mode (allow_invokes) permits
            // them — inline_target_for_call then recursively validates each resolves to an inlinable
            // callee and builds the nested target map (else the whole inline is refused).
            0xb6 | 0xb7 | 0xb8 => {
                if !allow_invokes {
                    return false;
                }
            }
            0x4f..=0x56 => return false, // array stores (heap writes)
            _ => {}
        }
        pc += len;
    }
    // Must be a compilable shape (valid leaders) so emit_inline can build blocks.
    find_leaders(bc).is_some()
}
