//! Copy-on-write guards: the unswitched arm, the flat-arena reads it enables, and the callback timer that prices them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use super::*;

#[cfg(feature = "next-gen-gc")]
pub(crate) static COW_GET_CALLBACKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static COW_GET_CALLBACK_NS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static COW_PUT_CALLBACKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static COW_PUT_CALLBACK_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
pub(crate) struct CowCallbackTimer {
    start: Option<std::time::Instant>,
    elapsed: &'static AtomicU64,
}

#[cfg(feature = "next-gen-gc")]
impl CowCallbackTimer {
    #[inline]
    pub(crate) fn new(active: bool, calls: &'static AtomicU64, elapsed: &'static AtomicU64) -> Self {
        let start = (active && crate::parallel::cow_profile_enabled()).then(|| {
            calls.fetch_add(1, Ordering::Relaxed);
            std::time::Instant::now()
        });
        Self { start, elapsed }
    }
}

#[cfg(feature = "next-gen-gc")]
impl Drop for CowCallbackTimer {
    #[inline]
    fn drop(&mut self) {
        if let Some(start) = self.start {
            self.elapsed
                .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn cow_callback_profile() -> (u64, u64, u64, u64) {
    (
        COW_GET_CALLBACKS.load(Ordering::Relaxed),
        COW_GET_CALLBACK_NS.load(Ordering::Relaxed),
        COW_PUT_CALLBACKS.load(Ordering::Relaxed),
        COW_PUT_CALLBACK_NS.load(Ordering::Relaxed),
    )
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn cow_array_skip() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COW_ARRAY_SKIP").is_some())
}

/// What the CURRENT body knows about `JitCtx::cow_armed` (`[ctx+12]`) for the
/// whole invocation. Threaded through `emit_blocks`/`emit_op`/`emit_inline`.
///
/// The flag is invocation-invariant (see the hoist in `build_body`), so the
/// method-entry COW UNSWITCH can test it ONCE and emit two bodies: a DISARMED
/// one that carries no COW code at all — no `[ctx+12]` test, no bloom probe, no
/// merge phi at any field site — and an ARMED one that keeps the per-site
/// diamond exactly as before. The disarmed body is the hot one (the main thread
/// is disarmed for all of boot); removing the diamond also removes the egraph
/// optimization barrier it imposed, so FIELD-LICM bases and the hoisted guard
/// operand stay in registers across field sites instead of being respilled.
#[derive(Clone, Copy)]
pub(crate) enum CowArm {
    /// Arm state unknown at this site: load + test `[ctx+12]` here and emit the
    /// diamond. Used when no entry hoist was emitted (e.g. a body with neither
    /// field sites nor inline targets that nonetheless reaches a field site
    /// through some other path).
    PerSite,
    /// The invocation-invariant arm flag, computed once in the entry block. Field
    /// sites reuse the value but still emit the diamond. This is the shape of the
    /// ARMED half of the unswitch (where the test is statically true but the cold
    /// path does not care), and of the whole body when the unswitch is opted out.
    Hoisted(Value),
    /// The method-entry COW UNSWITCH proved this invocation DISARMED: field sites
    /// emit ZERO COW instructions.
    Disarmed,
}

/// FLAT-GETFIELD opt-in (`RAVA_FLAT_GETFIELD`, default OFF). The method-entry arena
/// unswitch (see `flat_unswitch_slot` and the `build_body` site) removes the per-access
/// arena/handle diamond for ONE proven receiver. UNSET ⇒ the whole path is dead and the
/// emitted code is byte-identical to a build without the feature.
///
/// STILL OPT-IN, on measurement. The unswitch does exactly what it claims — on
/// `bench/BigGraph.sum` it takes the executed (arena) body from 445 to 321 machine
/// instructions and its arena guards from 3 to 1 — but it emits the body TWICE, and
/// canonical ChunkFill is SETUP(compile)-dominated. An interleaved ABBA A/B could not
/// separate ON from OFF there (ON 97.9/120.1/135.0s vs OFF 103.2/103.3/110.4s: the
/// spread tracks run ORDER, not the arm), and BigGraph moved only -2.3% median with
/// overlapping runs, because that probe is pointer-chase/miss-bound rather than
/// instruction-bound. No clean win ⇒ no default flip.
///
/// **RE-AUDITED AND STILL REFUSED, deliberately.** This is a recorded NULL result, not a stale
/// justification: the flag was measured and could not be separated from noise, and the cost it
/// pays — a second copy of the body — has since gone UP, not down, because the COW unswitch
/// (`cow_unswitch_min` = 1, default ON) already emits most bodies twice, so arming this one
/// would quadruple them. Flipping it on an unmeasured guess is precisely the move that got two
/// earlier rounds demoted to opt-in. The way to revisit it is to make the two unswitches SHARE
/// (nest FLAT inside the COW-disarmed arm only, in `build_body`), not to change this line.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn flat_getfield_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FLAT_GETFIELD").is_some())
}

/// Minimum COW-checkable sites a method needs before the COW method-entry
/// UNSWITCH is worth its second body (`RAVA_COW_UNSWITCH_MIN`, default 1 = every
/// method that can carry a COW check). Raise it to trade some of the win back
/// for code size: the census (`RAVA_JIT_STATS` → `[JIT-CODESIZE]`) prices the
/// emitted machine code and Cranelift's time on the exact same corpus.
pub(crate) fn cow_unswitch_min() -> usize {
    static MIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("RAVA_COW_UNSWITCH_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1)
    })
}

/// Minimum `getfield` sites on the SAME receiver before the FLAT-GETFIELD unswitch is
/// worth emitting the body twice. Density-shaped combinators read several fields of one
/// object per call and get the whole guard chain deleted; a one-field getter would just
/// pay doubled compile time and code size for a single guard.
#[cfg(feature = "next-gen-gc")]
pub(crate) const FLAT_GETFIELD_MIN_GETFIELDS: usize = 3;

/// Pick the local slot the method-entry arena unswitch should speculate on: the PINNED
/// (`pinned_ref_slots`: aloaded, never stored) ref local with the most `getfield` sites
/// directly on it, if that count reaches `FLAT_GETFIELD_MIN_GETFIELDS`.
///
/// Pinning is what makes ONE entry test cover the whole body — a slot no store can name
/// designates one object for the invocation, and object kind survives the moving GC (see
/// the unswitch's own comment). A slot that IS stored is never a candidate, which is
/// exactly where the proof would otherwise expire.
///
/// Generalises the original local-0-only rule: `this` is slot 0 for an instance method
/// and the first parameter is slot 0 for a static one, but a density combinator often
/// reads its fields off a later parameter, and those methods were previously left with a
/// diamond per access. Only ONE slot is ever chosen — the unswitch doubles the body, and
/// a second one would square it.
///
/// Counts a receiver only in the `aload k; getfield` adjacency (the shape the emitter
/// can actually prove), and bails to None on an opcode this walk cannot length-decode,
/// same as `pinned_ref_slots`.
///
/// **THE ADJACENCY RULE WAS RE-CHECKED AND IT IS NOT AN UNDERCOUNT WORTH FIXING.** A getfield's
/// receiver must be the stack top, so javac emits `aload k; getfield` and nothing else, with two
/// exceptions this walk misses: a receiver reached through `dup` (`promo_scan_body` shows the
/// tree does track dup-provenance elsewhere) and one reached through a swap. Both are vanishingly
/// rare in field-dense combinators, and either way the miss can only LOWER a count — it can never
/// pick a slot the emitter cannot serve, so it is safe in the direction that matters.
///
/// **PUTFIELD DELIBERATELY DOES NOT COUNT.** The obvious widening — also count `putfield` sites on
/// the same pinned receiver, so a method that reads two fields and writes two clears the
/// threshold — is WRONG here, and the reason is in `emit_op`: the putfield path has no flat
/// emitter at all. It consults `licm`, then falls to `native_putfield_const_prim_arena_dual`; it
/// never reads `SV::arena`. Counting stores would therefore admit methods the unswitch cannot pay
/// back. Check `ops/fields.rs` again before changing this.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn flat_unswitch_slot(bc: &[u8], max_locals: usize) -> Option<usize> {
    let pinned = pinned_ref_slots(bc, max_locals);
    let mut counts = vec![0usize; max_locals];
    let mut prev_aload: Option<usize> = None;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        if op == 0xb4 {
            if let Some(k) = prev_aload.filter(|&k| k < counts.len() && pinned[k]) {
                counts[k] += 1;
            }
        }
        prev_aload = match op {
            // Wide `aload k` — `pinned_ref_slots` does not mark a wide-only load, so
            // such a slot is never pinned and this can only ever count a slot that is
            // ALSO loaded narrowly somewhere. Harmless either way.
            0xc4 if bc.get(pc + 1).copied() == Some(0x19) => Some(
                ((*bc.get(pc + 2)? as usize) << 8) | *bc.get(pc + 3)? as usize,
            ),
            _ => aload_local(bc, pc),
        };
        pc += len;
    }
    counts
        .iter()
        .enumerate()
        .max_by_key(|&(_, &n)| n)
        .filter(|&(_, &n)| n >= FLAT_GETFIELD_MIN_GETFIELDS)
        .map(|(k, _)| k)
}

/// `RAVA_JIT_FLAT_TAGSELECT` (default OFF, and it stays off) - fold the PURE arms of the arena
/// ref-cell tag classify into branchless `select`s, leaving one branch for the boxed arm that needs
/// a callback. Applied to BOTH the dual and the flat emitter.
///
/// ⛔ IT REACHES ALMOST NOTHING, and that is the useful result. On the ring's hottest method:
///
///   unfolded            852 blocks / 8775 instrs / 534 branch-only
///   dual folded         836 blocks / 9165 instrs / 518 branch-only
///   dual + flat folded  836 blocks / 9165 instrs / 518 branch-only   <- IDENTICAL
///
/// So 16 of that method's 435 merge blocks come from the arena ref-field classify, and 419 do not -
/// folding the flat emitter as well changed nothing at all, which also refutes the guess that the
/// hot method uses it. The merges are emitted by something else (dispatch PIC diamonds, null
/// checks, COW/ring-store barriers, inline-callee splices are the candidates, unmeasured).
///
/// The trade is bad on its own terms too: +390 instructions for 16 blocks. Kept flagged off because
/// it is a measured negative - it says where the control-flow density is NOT - and because the fold
/// itself is correct (gates bit-exact ARMED, 411 tests). Do not enable it hoping for a win; find
/// the emitter that produces the other 419 merges first.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn flat_tagselect_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_FLAT_TAGSELECT").is_some())
}

/// `RAVA_NO_EMIT_MISC2=1` — kill switch for the emit-misc round that owns `licm.rs`,
/// this file and `dispatch_table_probe.rs`. Read at EMIT time, so with it set (or with
/// `RAVA_NO_EMIT_MORE=1`) not one instruction of the block is emitted and the machine
/// code is byte-identical to the previous round's.
///
/// What it currently gates: the FIELD-LICM prologue's SINGLE dominating bounds check
/// (`emit_licm_base`) in place of one range branch per hoisted pair.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_misc2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_EMIT_MISC2").is_none() && !crate::flags::emit_more_disabled()
    })
}
