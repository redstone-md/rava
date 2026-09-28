//! Plans produced before emission: inline callees, constructor stores, escape verdicts, LICM pairs.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Shape of the Deep-IC v3 self-registering prologue, computed by the compile
/// driver (it knows the method's staticness and the Vm's ctx slot).
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
pub(crate) struct DeepShape {
    /// Slots occupied by the incoming arguments (receiver + params, wide = 2):
    /// the prologue zeroes [args_slots .. frame_len) so a GC scanning the
    /// registered buffer never sees stale bits.
    pub(crate) args_slots: u16,
    /// max_locals + max_stack — the length the prologue registers (the spill
    /// area above the locals is part of the scanned root set).
    pub(crate) frame_len: u16,
    /// Address of the per-Vm `jit_ctx_slot` (leaked AtomicUsize) naming the
    /// executing thread's `JitThreadCtx` on the normal GIL-held path.
    pub(crate) ctx_slot: usize,
}

pub(crate) struct TrivialCtorStore {
    pub field_owner: String,
    pub field_name: String,
    pub jt: JT,
    pub arg_index: usize,
}

/// A resolved trivial-constructor inline plan: each store maps an operand-stack
/// argument to a receiver field cell. `n_params` is the constructor's argument
/// count (one operand-stack slot each, matching `count_args`), so the emitter
/// pops the right number before the receiver.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone)]
pub(crate) struct CtorInline {
    /// (arg_index, field layout slot, interned field id, field type).
    pub stores: Vec<(usize, u32, u32, JT)>,
    pub n_params: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EscapeReason {
    /// Provably method-local (straight-line) → alloc erasable, fields scalar-replaceable.
    Confined,
    /// Conservatively escaped ONLY because the method has branches/loops the
    /// straight-line taint walk cannot track. The true VOA-addressable frontier.
    ControlFlow,
    /// Really escapes: stored to a heap field/static/array, returned, thrown, or
    /// passed as a call receiver/argument. Not scalar-replaceable regardless of CF.
    Sink,
}

/// FIELD-LICM: one (invariant local, field) pair whose cell POINTER is resolved
/// once in the function prologue instead of per getfield. `slot` is the local
/// holding the receiver, `fid`/`cslot` the interned field id + class-layout slot.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LicmPair {
    pub slot: u16,
    pub fid: u32,
    pub cslot: u32,
}

/// ⭐**THE LAMBDA SAM INLINE PLAN — what a closure call site needs that an ordinary
/// devirtualised site does not.**
///
/// At an ordinary guarded site the operand stack already holds every value the callee reads:
/// `[receiver, arg0 .. argN-1]`, and the callee descriptor names them all. A SAM call site does
/// not. The stack holds `[closure, samArg0 .. samArgM-1]`, while the body about to be spliced is
/// the lambda's IMPLEMENTATION method, whose parameter list is `captures ++ samArgs` — the exact
/// splice `Vm::splice_call_args` performs on the interpreted path. The captures are not on the
/// stack at all; they live in the closure object's own cells.
///
/// So this plan carries the three facts the emitter cannot recover from `descriptor` alone:
///
///  * `site_idx` — **the guard's subject.** A lambda's arena CLASS is its capture SIGNATURE, one
///    per `(kind, kind, …)` tuple and shared by every closure of that shape in the program, so a
///    class-id compare is satisfied by a closure minted at a DIFFERENT indy site and would splice
///    another site's body with the guard reporting a hit. The interned SITE index is what pins the
///    implementation, and it is read from the receiver's own `AL_SITE_SLOT` cell — the same two
///    loads `emit_table_probe` already emits for its lambda key, so there is one rule with two
///    transcriptions.
///  * `cap_is_ref` — one entry per capture CELL, in cell order (cell `i` at
///    `HDR + (AL_HDR_CELLS + i) * 8`). A capture cell carries the ARENA encoding, not the JIT
///    operand encoding, and the two agree ONLY on the arena-pointer/null form: arena tags a heap
///    handle `(h<<2)|1`, an array `(h<<2)|2` and a box `(idx<<2)|3`, where the JIT operand cell
///    uses `h|1<<40`, `h|1<<42` and `idx|1<<41`. A PRIMITIVE cell needs no translation at all —
///    `encode_arena_cell` and `to_cell` produce bit-identical words for every one of I/J/F/D
///    (sign-extended i32, raw i64, zero-extended f32 bits, raw f64 bits) — so only the reference
///    positions are marked, and for each of those the guard proves the cell is UNTAGGED before the
///    splice may read it. Anything tagged takes the cold arm.
///  * `sam_nargs` — how many operand-stack entries sit above the closure. `count_args(descriptor)`
///    answers for the IMPLEMENTATION (captures included) and would pop the wrong number.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct SamInline {
    /// The interned `LambdaSite` index the profile observed and the guard compares.
    pub site_idx: u32,
    /// One flag per capture cell, in cell order: true = the implementation types this position as
    /// a reference, so the guard must prove the arena cell is untagged (`cell & 3 == 0`).
    pub cap_is_ref: Vec<bool>,
    /// Operand-stack entries the call site supplies above the closure receiver.
    pub sam_nargs: usize,
    /// ⚠**THE IMPLEMENTATION'S staticness, which is NOT `InlineCallee::is_static`.** The two
    /// answer different questions and a SAM site is where they come apart. `is_static` is read by
    /// the EMITTERS as "does this site push a receiver?", and a SAM site does — the closure. So a
    /// SAM plan always sets `is_static = false`, whatever the implementation is, or the emitter
    /// would look for the site's guarded form and not find it. This field is the other question:
    /// does the spliced BODY have a `this` in local slot 0? A `REF_invokeStatic` lambda body does
    /// not, and its captures start at slot 0; a `REF_invokeSpecial` one does, and capture 0 IS
    /// that `this` — which is also why the guard requires that capture to be pointer-shaped and
    /// non-null, the check `invokespecial` would have made.
    pub impl_is_static: bool,
    /// Where the identity above came from: `false` = the interpreter's SAM-apply profile
    /// (`lambda_site_mono`), `true` = the CALLER's own bytecode (`vm/lambda_prov.rs` — the
    /// `invokedynamic`'s `BootstrapMethods` operand). Read ONLY by the emit-time census, so the
    /// two populations can be told apart in `[INLCEN]`; nothing branches on it.
    pub static_prov: bool,
}

pub(crate) struct InlineCallee {
    /// ⭐**GUARDED DEVIRTUALISATION.** `None` means the site binds by TYPE - a static, a
    /// special, or a final-effective virtual - and the body is spliced unconditionally, which is
    /// the only thing this inliner has ever done.
    ///
    /// `Some(class_id)` means the site is an ordinary virtual or an INTERFACE call that the
    /// receiver profile says has only ever seen one concrete arena class. That body may only run
    /// behind a guard: `receiver.class_id == class_id`, with the ordinary call as the else arm.
    /// A stale or colliding profile then costs one compare, never a wrong answer - which is what
    /// makes speculating on a profile safe at all.
    ///
    /// This is the optimisation the engine did not have. `collect_inline_targets` refused
    /// `invokeinterface` outright, so `List.size()`, `Iterator.hasNext()` and `Map.get()` - the
    /// collection API as real code writes it - were real calls where a JVM has a guarded field
    /// load. Two months of levers made the call cheaper; a cheap call cannot beat an absent one.
    pub guard_class: Option<u32>,
    /// ⭐**THE HEAP-REF HALF OF THE SAME IDEA, AND THE HALF THAT MATTERS.** `guard_class` compares
    /// an ARENA class id, and the receiver census says every collection receiver is a heap Ref —
    /// so arena-only devirtualisation reached 190 sites of Minecraft's own classes and NOT ONE
    /// collection. This carries the class WORD instead, compared exactly as the four working
    /// inline arms compare it (`emit_heap_ref_class_key`).
    pub guard_ref_word: Option<u64>,
    /// ⭐**THE POLYMORPHIC HALF OF `guard_class`: MORE IDS THAT SHARE ONE BODY.**
    ///
    /// Class-hierarchy analysis refuses 6,071 sites a run with "more than one loaded implementor",
    /// but "more than one TYPE" is not "more than one BODY": a hierarchy where four subclasses
    /// inherit the same method has one body and four receiver classes. One spliced body then
    /// serves all of them, and the only thing that has to widen is the compare.
    ///
    /// Every id here resolves to the SAME declaring class as `guard_class` — checked before the
    /// entry is built, not assumed — so the guard stays a disjunction over ids of one body, and a
    /// receiver that is none of them lands on the ordinary dispatch exactly as before. Empty for
    /// every profile-sourced entry.
    pub guard_class_more: Vec<u32>,
    pub class: std::sync::Arc<crate::class_loader::LoadedClass>,
    /// Stable identity of this exact Code body. Numeric bytecode PCs are scoped
    /// to it; structural substitutions must never match a nested body's same PC.
    pub code_key: usize,
    pub bytecode: Vec<u8>,
    pub max_locals: u16,
    pub descriptor: String,
    /// false for an instance callee (invokespecial/virtual): the receiver is an
    /// implicit extra arg occupying local slot 0, descriptor args start at slot 1.
    pub is_static: bool,
    /// The callee's getfield refs pre-resolved (cp-index → interned id + type +
    /// layout slot), so its inlined body reads fields natively (and via a
    /// constant slot when layout pre-population is active) in the caller's frame.
    pub fields: std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    /// 16.2 phase 2: the callee's OWN inlinable invoke sites (keyed by the callee's cp indices),
    /// each recursively resolved. emit_inline runs the callee body with `env.inline_targets` swapped
    /// to THIS map, so a nested invoke inlines instead of bailing → chained depth up to MAX_INLINE_DEPTH.
    pub nested: std::collections::HashMap<u16, InlineCallee>,
    /// Getstatic value cells for the CALLEE's own getstatic sites, keyed by the
    /// CALLEE's cp indices (the caller's map must never leak in — a colliding cp
    /// would read the wrong cell). Populated only under RAVA_REF_STATIC_CELLS by
    /// the jit_run driver (which owns the reserve/rollback protocol); empty
    /// otherwise, leaving every inlined getstatic on the callback — the fallback
    /// tally billed ONE such site (SimplexNoise.GRADIENT read from an inlined
    /// noise leaf) at 101M full interpreter getstatic gauntlets per ChunkFill.
    pub statics: std::collections::HashMap<u16, (u32, JT)>,
    /// Structural HOF pairs proven for this exact bytecode body. This lets a
    /// small wrapper carrying an adjacent indy→consumer pair remain fully
    /// collapsed when the wrapper itself is inlined into a hotter caller.
    #[cfg(feature = "next-gen-gc")]
    pub hof_fusions: std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
    /// `Some` ⇒ this callee is a lambda IMPLEMENTATION spliced at a SAM call site, and the
    /// receiver on the stack is the CLOSURE, not the callee's receiver. See `SamInline`: the
    /// guard keys on the site index, the captures come out of the closure's cells, and only
    /// `sam.sam_nargs` operands are popped. `None` everywhere else — every existing site keeps
    /// exactly the shape it had.
    #[cfg(feature = "next-gen-gc")]
    pub sam: Option<SamInline>,
    /// ⭐**THE ROOTED SPLICE.** `Some(window_slots)` ⇒ this callee's body contains a GC point of
    /// its own — an allocation, a surviving real call, or both — and `emit_inline` MUST give the
    /// splice a REGISTERED frame window of `window_slots` cells and root the CALLER's frame across
    /// it. Every historic entry is `None`, and for those `emit_inline` emits exactly what it
    /// always did.
    ///
    /// The two refusals this replaces (`shape.new` and `iface.calls != 1`) are one rule: a spliced
    /// callee's locals are fresh Cranelift `Variable`s written from offset 0 of the CALLER's frame
    /// buffer, so a `spill_for_gc` inside the splice roots the callee at the cost of the caller.
    /// See `vm/inline_targets.rs::gcpoint_body_shape` for what may ask, and `emit_inline` for how
    /// it is paid.
    #[cfg(feature = "next-gen-gc")]
    pub splice_roots: Option<u16>,
    /// The CALLEE's own trivial-`<init>` plans, keyed by the CALLEE's cp indices. Only a rooted
    /// splice may contain `new`, and only it needs these; every other entry leaves the map empty
    /// and `emit_inline` does not swap `env.ctor_inlines` for it. The caller's map must never
    /// leak in — a colliding cp index would store the wrong constructor's arguments into the
    /// wrong object's cells, which is silent heap corruption, not a wrong value.
    #[cfg(feature = "next-gen-gc")]
    pub ctor_inlines: std::collections::HashMap<u16, CtorInline>,
    /// ⭐**THE FOURTH SHAPE, WHICH ALREADY EXISTED AND WAS NOT DECLARED.**
    ///
    /// `true` ⇒ this entry is UNGUARDED and carries NO `splice_roots` window, yet a GC-capable
    /// point still SURVIVES inside its spliced body: a real call the partial nested map did not
    /// fold, or a `getstatic` whose callback can allocate. It is set by the nested (tier-0
    /// partial-map) band and by nothing else, because that band is the only one that admits a
    /// body with a surviving call while leaving both the guard and the window empty.
    ///
    /// ⚠**THIS IS A PRE-EXISTING SHAPE, NOT ONE THIS ROUND INVENTED.** `partial_inline_gc_safe`
    /// has always admitted `real_calls == 1` with a forwarding tail, and `leaves_gc_point()`
    /// answered `false` for those entries — which is exactly the exposure `range.rs`'s own
    /// comment names in one line (*"Tier-0 partial maps have the same exposure"*) and then does
    /// not close. Widening that band multiplies the population, so the flag is set for the WHOLE
    /// population and in BOTH arms of the round's switch: a consumer that must know "does a GC
    /// point survive here" is now told the truth about the historic entries too.
    #[cfg(feature = "next-gen-gc")]
    pub partial_gc_points: bool,
}

impl InlineCallee {
    /// ⭐**DOES SPLICING THIS ENTRY STILL LEAVE A GC-CAPABLE POINT IN THE CALLER'S BODY?**
    ///
    /// Membership in an `inline_targets` map used to answer "no" by itself — the callee was a
    /// leaf spliced in whole and nothing survived that could collect or write the heap. Two
    /// things have since made that false, and this is the one place that names them:
    ///   * a GUARDED entry (`guard_class` / `guard_ref_word`) emits the body on the hit arm and a
    ///     real dispatch on the miss arm;
    ///   * a ROOTED entry (`splice_roots`) emits a body that allocates and/or still calls.
    ///
    /// Every predicate that reads a map to conclude "no GC point survives" —
    /// `field_loads_invariant`'s `readonly` stamp, `method_no_alloc`, the two partial-inline
    /// liveness gates, the field-promotion whitelist — asks THIS, so a future third shape has one
    /// definition to extend instead of four call sites to find.
    ///
    /// ⭐**AND A FOURTH, WHICH WAS ALREADY BEING SPLICED AND WAS ANSWERING `false`:**
    ///   * a PARTIAL entry (`partial_gc_points`) — unguarded, windowless, and still carrying a
    ///     surviving real call inside the splice. See the field's own comment: the historic
    ///     `real_calls == 1 + forwarding tail` population is exactly this, and this predicate
    ///     used to call it a fold.
    ///
    /// ⚠**"ROOTED" AND "NO GC POINT" ARE DIFFERENT QUESTIONS, AND THIS ONE IS THE SECOND.** The
    /// stack maps make a surviving call SAFE TO SPLICE — they name and forward every root across
    /// it — and they change nothing about what the call may DO. It still writes fields and still
    /// MOVES objects. So `stackmaps::declaring()` is deliberately not consulted here: the
    /// consumers below stamp `readonly`, hold a raw interior cell pointer across a region, or
    /// keep a field value in a register, and every one of those is falsified by a heap move that
    /// forwarding a root does not undo. The round that discharges a ROOTING proof asks
    /// `partial_free_enabled()`; a consumer asking about heap MUTATION asks this.
    pub(crate) fn leaves_gc_point(&self) -> bool {
        if self.guard_class.is_some() || self.guard_ref_word.is_some() {
            return true;
        }
        #[cfg(feature = "next-gen-gc")]
        if self.splice_roots.is_some() || self.partial_gc_points {
            return true;
        }
        false
    }
}

#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
pub(crate) struct ActiveSamBinding<'a> {
    pub(crate) owner_code_key: usize,
    pub(crate) sam_call_pc: usize,
    pub(crate) captures: &'a [SV],
    pub(crate) implementation: &'a InlineCallee,
}

/// FIELD-LICM emission context threaded through `emit_op`: the prologue-resolved
/// cell-pointer variables keyed by (outer local, field id, layout slot), plus the
/// CURRENT body's local→outer-local aliasing (identity in the outer body; the
/// popped-arg provenance inside an inlined callee).
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
pub(crate) struct Licm<'a> {
    pub(crate) vars: &'a std::collections::HashMap<(u16, u32, u32), Variable>,
    pub(crate) alias: &'a [Option<u16>],
    /// PROMO value registers per pair; empty map on the plain (non-promoted)
    /// body — sites then fall back to the base-pointer diamonds.
    pub(crate) vals: &'a std::collections::HashMap<(u16, u32, u32), Variable>,
    /// PROMO write-back set: (base var, value var, cslot) per WRITTEN pair.
    /// Only ever non-empty inside the promoted body, where every base is
    /// proven non-zero at the unswitch — the flush stores unconditionally.
    pub(crate) flush: &'a [(Variable, Variable, u32)],
}

/// SROA resolution context for the body emitter — the compile-time maps that say
/// which `new`/ctor/field cp indices are arena-eligible + their field plans, so a
/// confined object's allocation is erased into field registers.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct SroaCtx<'a> {
    pub(crate) new_arena: &'a std::collections::HashMap<u16, (u32, u32)>,
    pub(crate) ctor_inlines: &'a std::collections::HashMap<u16, CtorInline>,
    pub(crate) fields: &'a std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    /// invokevirtual cp → (field id, type) for trivial getters (synergy Layer 1).
    pub(crate) getters: &'a std::collections::HashMap<u16, (u32, JT)>,
}

// ═════════════════════════ THE LAMBDA SAM RECEIVER PROFILE ═════════════════════════
//
// ⭐**BREAK (1): A LAMBDA CALL SITE WAS NEVER PROFILED AT ALL.**
//
// `emit_guarded_inline` only ever fires for a callee that `collect_inline_targets` built from
// `sitemono::mono_class`, and `sitemono`'s only writer is `tlcache::obj_inline_put`, reached from
// one line late in `opcodes::invoke_virtual`. The interpreter's arena-lambda arm resolves the SAM
// and RETURNS several hundred lines before that line — so `mono_class` is `None` for every lambda
// site in the program, and the guarded inliner has never seen one.
//
// This is the missing writer, and it is deliberately NOT an extension of `sitemono`: that table's
// slot records ONE class id, while a lambda site needs the (class id, SITE index) PAIR — the class
// alone is a capture signature shared program-wide and names no implementation. Widening
// `sitemono`'s slot would put four more atomics on the path every ordinary virtual cache miss
// walks, to carry two fields only closures use.
//
// ## Why an imprecise table is safe here, and safe for a STRONGER reason than `sitemono`'s
//
// The emitted guard proves `class_id == cid` AND `AL_SITE_SLOT == site_idx`. That pair uniquely
// determines the implementation (`LambdaSite` is content-interned over owner/name/descriptor) AND
// the capture layout (the class id IS the capture-kind signature). So a stale slot, a lost race, a
// hash collision, or a site that only ever reached this table on SOME of its receivers can each
// make the speculation MISS — never make it wrong. A miss is one load and one compare on a path
// that was making a full closure dispatch.
//
// The one thing that must not happen is a slot answering `monomorphic` while holding a value the
// site never saw, so a slot whose identity does not match is left ALONE rather than overwritten:
// a site that loses its slot goes unprofiled, which refuses the inline. Silence, never a guess.

/// Power of two, sized like `sitemono`'s: a boot links a few thousand indy sites and calls them
/// from tens of thousands of places; a collision costs a prediction, never an answer.
#[cfg(feature = "next-gen-gc")]
const LSITE_SLOTS: usize = 1 << 16;

#[cfg(feature = "next-gen-gc")]
struct LambdaSiteSlot {
    /// Caller's `LoadedClass` pointer; 0 = empty. Claimed once, never reassigned.
    caller: std::sync::atomic::AtomicUsize,
    cp: std::sync::atomic::AtomicU32,
    /// The receiver's arena class id (its capture-kind signature).
    cid: std::sync::atomic::AtomicU32,
    /// The interned `LambdaSite` index — the fact that names the implementation.
    site: std::sync::atomic::AtomicU32,
    /// MethodHandle reference kind (6 = REF_invokeStatic, 7 = REF_invokeSpecial, …).
    kind: std::sync::atomic::AtomicU32,
    /// Capture cell count, cross-checked against the descriptor arity at admission.
    ncaps: std::sync::atomic::AtomicU32,
    /// 0 = unseen, 1 = one (cid, site, kind, ncaps) tuple, 3 = more than one (refused).
    distinct: std::sync::atomic::AtomicU32,
}

#[cfg(feature = "next-gen-gc")]
impl LambdaSiteSlot {
    const fn new() -> Self {
        use std::sync::atomic::{AtomicU32, AtomicUsize};
        LambdaSiteSlot {
            caller: AtomicUsize::new(0),
            cp: AtomicU32::new(0),
            cid: AtomicU32::new(0),
            site: AtomicU32::new(0),
            kind: AtomicU32::new(0),
            ncaps: AtomicU32::new(0),
            distinct: AtomicU32::new(0),
        }
    }
}

#[cfg(feature = "next-gen-gc")]
fn lambda_site_table() -> &'static [LambdaSiteSlot] {
    static T: std::sync::OnceLock<Vec<LambdaSiteSlot>> = std::sync::OnceLock::new();
    T.get_or_init(|| (0..LSITE_SLOTS).map(|_| LambdaSiteSlot::new()).collect())
}

#[cfg(feature = "next-gen-gc")]
#[inline]
fn lambda_slot_of(caller: usize, cp: u16) -> usize {
    // Allocation granularity leaves the bottom bits of a pointer constant, so mix them out —
    // the same mix `sitemono::slot_of` uses, for the same reason.
    let h = (caller >> 4) ^ ((cp as usize) << 13) ^ (caller >> 27);
    h & (LSITE_SLOTS - 1)
}

/// Record that call site `(caller, cp)` dispatched a SAM apply onto an arena closure of class
/// `cid`, minted at interned site `site_idx`, MethodHandle kind `kind`, with `ncaps` captures.
///
/// ⚠**COST, STATED.** Unlike `sitemono::note` — which hangs off a cache MISS and so runs once per
/// (site, class) — this runs on every interpreted SAM apply, because the arena-lambda arm has no
/// miss path to hang off. It is a `OnceLock` load, a hash, and between one and five RELAXED loads
/// before the early return; a site that has settled never writes. When the chain is switched off
/// it is a single load and a branch. If that ever shows up in a profile the fix is a per-thread
/// one-entry filter, not a cheaper slot — but do not add one before it measures.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lambda_site_note(
    caller: usize,
    cp: u16,
    cid: u32,
    site_idx: u32,
    kind: u8,
    ncaps: u32,
) {
    use std::sync::atomic::Ordering::Relaxed;
    if caller == 0 || !lambda_inline_chain_enabled() {
        return;
    }
    let s = &lambda_site_table()[lambda_slot_of(caller, cp)];
    if s.caller.load(Relaxed) == 0 {
        // Claim it. A racing claimer simply wins; the loser's site goes unprofiled, which
        // refuses the inline — the safe direction.
        s.caller.store(caller, Relaxed);
        s.cp.store(u32::from(cp), Relaxed);
        s.cid.store(cid, Relaxed);
        s.site.store(site_idx, Relaxed);
        s.kind.store(u32::from(kind), Relaxed);
        s.ncaps.store(ncaps, Relaxed);
        s.distinct.store(1, Relaxed);
        return;
    }
    if s.caller.load(Relaxed) != caller || s.cp.load(Relaxed) != u32::from(cp) {
        return; // another site owns this slot
    }
    if s.cid.load(Relaxed) != cid
        || s.site.load(Relaxed) != site_idx
        || s.kind.load(Relaxed) != u32::from(kind)
        || s.ncaps.load(Relaxed) != ncaps
    {
        // A SECOND closure identity at this site. Saturate straight to "refused": unlike a
        // virtual receiver class, a differing site index means a different BODY, and there is
        // no bimorphic shape here to graduate to.
        s.distinct.store(3, Relaxed);
    }
}

/// The single closure identity this site has ever dispatched, as
/// `(class_id, site_idx, kind, ncaps)`; `None` when the site is unprofiled, saw more than one, or
/// lost its slot to a collision.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lambda_site_mono(caller: usize, cp: u16) -> Option<(u32, u32, u8, u32)> {
    use std::sync::atomic::Ordering::Relaxed;
    let s = &lambda_site_table()[lambda_slot_of(caller, cp)];
    if s.caller.load(Relaxed) != caller || s.cp.load(Relaxed) != u32::from(cp) {
        return None;
    }
    if s.distinct.load(Relaxed) != 1 {
        return None;
    }
    Some((
        s.cid.load(Relaxed),
        s.site.load(Relaxed),
        s.kind.load(Relaxed) as u8,
        s.ncaps.load(Relaxed),
    ))
}

/// ✅**KILL SWITCH FOR THE WHOLE LAMBDA SAM INLINE CHAIN, DEFAULT ON**
/// (`RAVA_NO_LAMBDA_INLINE_CHAIN=1` disables), ANDed with the block-level `RAVA_NO_EMIT_MORE`
/// handle so one same-binary A/B prices the round as ONE arm.
///
/// ⭐**IT IS CONSULTED AT ADMISSION, NOT AT EMIT, AND THAT IS DELIBERATE.** Three separate
/// mechanisms hang off it — the interpreter's profile write, the void-return admission for a SAM
/// body, and the site-keyed guard plus capture splice in the emitters — and a switch read
/// independently by each could half-apply the feature (a `SamInline` plan built by an admission
/// that believed the switch was ON, then spliced by an emitter that believed it was OFF, is the
/// miscompile this whole file exists to avoid). With the switch consulted only where the PLAN is
/// built, an off run builds no plan, so every emitter sees exactly the world it saw before. The
/// profile write consults it too, purely so a disabled run does not pay the table write.
pub(crate) fn lambda_inline_chain_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_LAMBDA_INLINE_CHAIN").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

/// Deep-IC v3 prologue state threaded to the return sites: the ctx pointer,
/// the saved cell-stack top (restoring it IS the pop) and the box-table base.
#[cfg(feature = "next-gen-gc")]
#[derive(Clone, Copy)]
pub(crate) struct DeepVars {
    pub(crate) ctx: Variable,
    pub(crate) top: Variable,
    pub(crate) base: Variable,
}
