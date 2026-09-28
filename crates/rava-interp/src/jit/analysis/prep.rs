//! What the compile driver has to decide before it can build an Env.
//!
//! Three hoisting questions -- which field pairs a loop may lift, which fields stay in
//! registers for the whole method, and which sites read a declared array -- plus whether a
//! bare dereference is allowed at all. All four are pure functions of bytecode and pool.

use crate::jit::*;

/// ⭐**ROUND 6 KILL SWITCH FOR THE PRE-CODEGEN ANALYSES** (`RAVA_NO_FAST_ANALYSIS=1`, default ON).
///
/// Cranelift runs on the boot's SERIAL critical path and these analyses run BEFORE it, once per
/// each of the ~1,950 compiled bodies. Nothing under this switch changes what an analysis
/// CONCLUDES — the emitted code is byte-identical either way, which is why the switch exists at
/// all: it bisects a regression by restoring the ORIGINAL SHAPE of the walk, not a different
/// answer. Memoised (one `OnceLock` read per compile, never per opcode), and it consults
/// `emit_more_disabled` so the block prices with the rest of them.
pub(crate) fn fast_analysis() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_ANALYSIS").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**TWO WALKS OF THE SAME BYTES, FOLDED INTO ONE.**
///
/// `prepare_hoists` asked `method_no_alloc` (walk #1) and then called `licm_field_pairs`, whose
/// FIRST act is `has_back_edge` (walk #2) — and `has_back_edge` is the one that cannot early-exit
/// in the common case, because it only returns early when it FINDS a back edge, so a loop-free
/// body pays a full decode of every instruction to learn nothing. `method_no_alloc` is the
/// opposite: it returns at the first `new`/non-inlined invoke, which most bodies have in their
/// first few instructions.
///
/// So the two are fused with the cheap-to-refute question driving the loop: the moment allocation
/// is proven the walk stops (exactly where `method_no_alloc` stopped) and the back-edge answer is
/// never needed, because a `no_alloc == false` method takes no LICM pairs. Only when the body IS
/// allocation-free — the rare case that used to pay for BOTH full walks — does the walk run to the
/// end, and then it has already answered the back-edge question for free.
///
/// WAS: two decodes of the whole body per compile whose cost grows with BYTECODE LENGTH.
/// IS: one, and the surviving call to `licm_field_pairs` is made only when a back edge exists —
/// where `has_back_edge` re-asks the question but stops at the first back edge instead of the end.
///
/// Byte-identical by construction: the branch rule is `has_back_edge`'s (range.rs) and the
/// refusal rule is `method_no_alloc`'s (admission.rs), evaluated over the same linear pc sequence.
/// When the decode fails, `no_alloc` is `false` exactly as `method_no_alloc` returns `false`, and
/// the back-edge answer is then unused.
#[cfg(feature = "next-gen-gc")]
fn no_alloc_and_loop(
    bc: &[u8],
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
) -> (bool, bool) {
    let mut back_edge = false;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return (false, back_edge) };
        match op {
            0xbb | 0xbc | 0xbd | 0xc5 | 0xbf => return (false, back_edge), // allocation / athrow null
            0xb6 | 0xb7 | 0xb9 => {
                let idx = if pc + 2 < bc.len() {
                    ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16
                } else {
                    return (false, back_edge);
                };
                // Membership alone is not "no GC point" — see `InlineCallee::leaves_gc_point`.
                match inline_targets.get(&idx) {
                    Some(t) if !t.leaves_gc_point() => {}
                    _ => return (false, back_edge),
                }
            }
            0xb8 => {
                let idx = if pc + 2 < bc.len() {
                    ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16
                } else {
                    return (false, back_edge);
                };
                let folded = matches!(inline_targets.get(&idx), Some(t) if !t.leaves_gc_point());
                if !folded && !static_intrinsics.contains_key(&idx) {
                    return (false, back_edge);
                }
            }
            // `has_back_edge`'s rule, verbatim: a 2-byte-offset branch whose target is at or
            // before the branch itself. The `pc + 2` bound is the one thing NOT taken verbatim —
            // `has_back_edge` indexes the offset unguarded, and this arm is now reached for
            // bodies whose allocation refusal comes LATER, so a truncated trailing branch would
            // index past the end where before it never got the chance. Out of bounds leaves the
            // flag alone, which costs at most a LICM opportunity in a body that cannot decode.
            0x99..=0xa7 | 0xc6 | 0xc7 => {
                if pc + 2 < bc.len() && ((pc as i32 + s16(bc, pc + 1)) as usize) <= pc {
                    back_edge = true;
                }
            }
            _ => {}
        }
        pc += len;
    }
    (true, back_edge)
}

/// The four hoisting facts, computed once per compile.
pub(crate) struct Hoists {
    pub(crate) licm_pairs: Vec<LicmPair>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) promo_written: Option<std::collections::HashSet<(u16, u32, u32)>>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) array_fields: std::collections::HashSet<u16>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bare_allowed: bool,
}

pub(crate) fn prepare_hoists(
    bc: &[u8],
    code: &Code,
    fields: &std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    pool: &ConstantPool,
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
    #[cfg(feature = "next-gen-gc")] tagged_fields: Option<&std::collections::HashSet<u16>>,
) -> Hoists {
    // FIELD-LICM pair discovery — only meaningful when the const-slot fast paths
    // are active and no GC point can move an object mid-call (method_no_alloc).
    //
    // ⭐**THE PROCESS CONSTANTS ARE ASKED BEFORE THE BYTECODE WALK.** All three gates below are
    // memoised `OnceLock` reads of the environment — constants of the PROCESS, identical for
    // every one of the ~1,950 compiles — and the walk was in front of them, so a run with the
    // field fast paths switched off still decoded every body to throw the answer away. Pure
    // functions with no side effects, so `&&` may be reordered: byte-identical, and under the
    // default (verify off, arena inline on, pinned read off) all three are `true` anyway, which
    // is why this reorder is a no-op on a default boot and a whole pass per method off a bisect.
    #[cfg(feature = "next-gen-gc")]
    let licm_pairs: Vec<LicmPair> = if field_verify_disabled()
        && !arena_inline_disabled()
        // PINNED-MIRROR COHERENCE (RAVA_PINNED_READ, default OFF): FIELD-LICM base
        // hoisting + PROMO defer field stores to `[base + cslot*8]` (emit_licm_putfield
        // fast arm / emit_promo_flush). For a PINNED_PTRS pointer receiver `base` IS
        // the mirror, so those raw stores update the mirror but leave the authoritative
        // FieldBlob stale (the storage/BitSet divergence). Disable the hoist under the
        // read-flip flag so every putfield funnels through the callback-coherent
        // arena-dual emitters. Byte-identical when the flag is off.
        && !crate::nextgen::pinned_read_enabled()
    {
        if fast_analysis() {
            // ONE decode of the body answers both "can this allocate?" and "is there a loop?".
            // `licm_field_pairs` returns an empty vector for a loop-free body anyway (its first
            // line is `has_back_edge`), so not calling it there is the same result without the
            // second full decode.
            match no_alloc_and_loop(bc, inline_targets, static_intrinsics) {
                (true, true) => {
                    licm_field_pairs(bc, &fields, inline_targets, code.max_locals as usize)
                }
                _ => Vec::new(),
            }
        } else if method_no_alloc(bc, inline_targets, static_intrinsics) {
            licm_field_pairs(bc, &fields, inline_targets, code.max_locals as usize)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    // PROMO: a strict-safe method keeps every hoisted field in a REGISTER for the
    // whole call (what C2's scalar promotion does). Gated per-method by promo_scan;
    // handler-bearing methods excluded (a caught guest exception could resume past
    // a skipped flush).
    #[cfg(feature = "next-gen-gc")]
    let promo_written: Option<std::collections::HashSet<(u16, u32, u32)>> =
        if !licm_pairs.is_empty() && field_promo_enabled() && code.exceptions.is_empty() {
            promo_scan(
                bc,
                &fields,
                pool,
                inline_targets,
                static_intrinsics,
                &licm_pairs,
                code.max_locals as usize,
            )
        } else {
            None
        };
    // Which getfield/putfield sites read a field DECLARED as an array. Computed once per
    // compile, next to `fields`, because the field map cannot carry it: `desc_to_jt` maps
    // `[…` and `L…;` to the same `JT::Ref`.
    // The caller supplies this when it has a `Vm`, which is what lets it also mark fields
    // whose declared type can never be arena-born (`Vm::keeps_tag_by_declared_type`).
    //
    // THE FALLBACK IS THE UNSOUND DIRECTION, and the earlier note here had it exactly
    // backwards. This set names the fields that KEEP their tag, and `bare_ok` is its
    // COMPLEMENT — so a smaller set means MORE bare dereferences, not fewer. The
    // descriptor-only answer is a strict subset, therefore strictly more dangerous, and
    // calling it "still sound: keeping a tag costs a test, never correctness" described a
    // set that grows when this one shrinks. Under RAVA_JIT_BARE_REF_LOAD the full boot
    // stopped dead just past `Environment:` with no panic — a wild pointer, silently.
    //
    // Without a `Vm` nothing about declared types is knowable, so the flip is refused
    // outright for that compilation rather than approximated.
    #[cfg(feature = "next-gen-gc")]
    let bare_allowed = tagged_fields.is_some();
    #[cfg(feature = "next-gen-gc")]
    let array_fields = match tagged_fields {
        Some(s) => s.clone(),
        None => array_field_indices(bc, pool),
    };
    Hoists {
        licm_pairs,
        #[cfg(feature = "next-gen-gc")]
        promo_written,
        #[cfg(feature = "next-gen-gc")]
        array_fields,
        #[cfg(feature = "next-gen-gc")]
        bare_allowed,
    }
}
