//! WHICH RULE REFUSES AN INLINE, COUNTED — the inliner's own `compile_bail`.
//!
//! ⭐The inliner had exactly two numbers about itself (`devirt.admitted` / `devirt.refused`) and
//! one gated log line per ADMITTED callee. Every refusal — the SAM chain's nine `continue`s, the
//! body gate's dozen `return None`s, the opcode list in `is_inlinable_kind` — was invisible, so
//! "which rule refuses the most calls in this population" could only be answered by reading code.
//! `RAVA_DBG_INLBAIL` counts one of those layers (the structural opcode walk) and only when armed.
//!
//! This counts all three layers, always, at COMPILE time only: one relaxed add per admission
//! decision, never per call. A warm reload compiles ~2,100 bodies, so the whole census is a few
//! tens of thousands of relaxed adds across the run — below any measurable floor, and it is the
//! only way the next round can rank rules by what they actually block instead of by guesswork.
//!
//! Reported from the periodic profiler dump (`pc_profile/dump.rs`), beside `compile_bail::report`,
//! for the reason that census records: a server boot exits through `System.exit`, so a teardown
//! dump never runs.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Row ids. Grouped: `S_*` = a site was OFFERED to the inliner, `A_*` = it was ADMITTED,
/// `X_*` = a refusal, named for the gate that made it.
pub(crate) const S_STATIC: usize = 0;
pub(crate) const S_SPECIAL: usize = 1;
pub(crate) const S_FINALVIRT: usize = 2;
pub(crate) const S_SAM: usize = 3;
pub(crate) const S_DEVIRT: usize = 4;

pub(crate) const A_LEAF: usize = 5;
pub(crate) const A_NESTED: usize = 6;
pub(crate) const A_IFACE: usize = 7;
pub(crate) const A_SAM: usize = 8;
pub(crate) const A_DEVIRT: usize = 9;

// --- SAM chain (`try_collect_inline_targets`, the lambda arm) ---
pub(crate) const X_SAM_NOPROFILE: usize = 10;
pub(crate) const X_SAM_NOTLAMBDA: usize = 11;
pub(crate) const X_SAM_KIND: usize = 12;
pub(crate) const X_SAM_SITE: usize = 13;
pub(crate) const X_SAM_MEMBER: usize = 14;
pub(crate) const X_SAM_RET: usize = 15;
pub(crate) const X_SAM_ARITY: usize = 16;
pub(crate) const X_SAM_CAPTYPES: usize = 17;
pub(crate) const X_SAM_PINNED: usize = 18;
pub(crate) const X_SAM_NATIVECOLL: usize = 19;
pub(crate) const X_SAM_BODY: usize = 20;

// --- devirt chain ---
pub(crate) const X_DEVIRT_NOPROFILE: usize = 21;
pub(crate) const X_DEVIRT_NAME: usize = 22;
pub(crate) const X_DEVIRT_BODY: usize = 23;

// --- body gate (`inline_target_for_exact_opts`), in the order it tests ---
pub(crate) const X_CTOR: usize = 24;
pub(crate) const X_RESOLVE: usize = 25;
pub(crate) const X_UNINIT: usize = 26;
pub(crate) const X_NATIVEMAP: usize = 27;
pub(crate) const X_NATIVESERVED: usize = 28;
pub(crate) const X_STATICNESS: usize = 29;
pub(crate) const X_NOTFINAL: usize = 30;
pub(crate) const X_NOCODE: usize = 31;
pub(crate) const X_NESTED_NONE: usize = 32;
pub(crate) const X_GCUNSAFE: usize = 33;
pub(crate) const X_FIELDS: usize = 34;
pub(crate) const X_GETSTATIC: usize = 35;

// --- the structural shape walk, split by the FIRST rule that blocks (see `nested_blocker`) ---
pub(crate) const X_SH_EXCEPTIONS: usize = 36;
pub(crate) const X_SH_IFACE: usize = 37;
pub(crate) const X_SH_INDY: usize = 38;
pub(crate) const X_SH_NEW: usize = 39;
pub(crate) const X_SH_PUTFIELD: usize = 40;
pub(crate) const X_SH_PUTSTATIC: usize = 41;
pub(crate) const X_SH_INSTANCEOF: usize = 42;
pub(crate) const X_SH_SWITCH: usize = 43;
pub(crate) const X_SH_ARRAYSTORE: usize = 44;
pub(crate) const X_SH_VOID: usize = 45;
pub(crate) const X_SH_SIZE96: usize = 46;
pub(crate) const X_SH_SIZE_T0: usize = 47;
pub(crate) const X_SH_DEPTH: usize = 48;
pub(crate) const X_SH_LEADERS: usize = 49;
pub(crate) const X_SH_DATAPAR: usize = 50;
pub(crate) const X_SH_OTHER: usize = 51;

// --- the widened interface-body arm's own refusals (`iface_nested_admissible`) ---
pub(crate) const X_IF_OFF: usize = 52;
pub(crate) const X_IF_SHAPE: usize = 53;
pub(crate) const X_IF_BRANCH: usize = 54;
pub(crate) const X_IF_CALLS: usize = 55;
pub(crate) const X_IF_TAIL: usize = 56;
pub(crate) const X_IF_FRAME: usize = 57;
pub(crate) const X_IF_SIZE: usize = 58;

/// Sites the widened arm actually put into a compiled body, and the interface dispatch each one
/// removed from the caller (one per emitted splice, counted at EMIT time — `emit_inline` runs once
/// per site per compile, never per call).
pub(crate) const E_IFACE_EMIT: usize = 59;

/// The LEARN path. Both rows count TRANSITIONS, never calls: a site is claimed once and poisoned
/// at most once, so these are bounded by the site count and carry no per-call atomic. See
/// `note_sam_site_from_compiled`.
pub(crate) const L_POISONED: usize = 60;
pub(crate) const L_LEARNED: usize = 61;
/// The compiled-code learn hook's own funnel — see the five NAMES rows at the end of the table.
pub(crate) const L_HOOK_REACHED: usize = 135;
pub(crate) const L_HOOK_NO_CALLER: usize = 136;
pub(crate) const L_HOOK_NOT_ARENA: usize = 137;
pub(crate) const L_HOOK_NOT_LAMBDA: usize = 138;
pub(crate) const L_HOOK_KIND: usize = 139;
pub(crate) const L_HOOK_HANDLE_LAMBDA: usize = 140;
pub(crate) const L_HOOK_HANDLE_OTHER: usize = 141;
/// Class-hierarchy analysis, the profile-free source of a devirtualisation guess.
pub(crate) const CHA_ASKED: usize = 142;
pub(crate) const CHA_UNPROVABLE: usize = 143;
pub(crate) const CHA_POLY: usize = 144;
pub(crate) const CHA_HIT: usize = 145;
pub(crate) const CHA_NOID: usize = 146;
pub(crate) const CHA_MULTI: usize = 147;
// Opt-in (`RAVA_CHA_STATS`) split of the coarse `CHA_POLY` row above. These remain separate from
// the always-on census so a diagnostic full-hierarchy probe cannot become part of admission.
pub(crate) const CHA_OVER_CAP: usize = 148;
pub(crate) const CHA_RESOLVE_FAIL: usize = 149;
pub(crate) const CHA_MULTI_BODY: usize = 150;
pub(crate) const CHA_ONE_BODY_GT4: usize = 151;
pub(crate) const CHA_ASK_VIRTUAL: usize = 152;
pub(crate) const CHA_ASK_INTERFACE: usize = 153;
pub(crate) const CHA_INLINABLE_VIRTUAL: usize = 154;
pub(crate) const CHA_INLINABLE_INTERFACE: usize = 155;
pub(crate) const CHA_EMITTABLE_OPCODE: usize = 156;

// --- STATIC LAMBDA PROVENANCE (`vm/lambda_prov.rs`). No profile is consulted on this path: the
// implementation, the kind and the capture kinds are operands of the `invokedynamic` instruction.
// `P_*` = a candidate the caller's own dataflow proved, `A_*` = admitted, `X_*` = refused.
/// A `0xb9` in the CALLER whose RECEIVER is a closure this same body minted (shape A).
pub(crate) const P_SAM_RECV: usize = 62;
/// A call the caller is ALREADY inlining, one of whose ARGUMENTS is such a closure (shape B).
pub(crate) const P_CTX_ARG: usize = 63;
pub(crate) const A_PROV_SAM: usize = 64;
pub(crate) const A_CTX: usize = 65;
/// The producing `invokedynamic` is not a `LambdaMetafactory` site (a concat, a record, a
/// pattern switch) — nothing to name.
pub(crate) const X_PROV_NOTLMF: usize = 66;
pub(crate) const X_PROV_KIND: usize = 67;
pub(crate) const X_PROV_DESC: usize = 68;
pub(crate) const X_PROV_BODY: usize = 69;
/// Shape B only: the spliced callee has no SINGLE interface site whose receiver is that
/// parameter (none, two of them, or a cp shared with another site in the same body).
pub(crate) const X_CTX_NOSITE: usize = 70;
/// Shape B only: the site the plan would take is already bound by the callee's own nested map.
pub(crate) const X_CTX_TAKEN: usize = 71;
/// Spliced sites, counted at EMIT time — one per emitted guarded SAM diamond, split by whether
/// the plan came from the interpreter's profile or from static provenance.
pub(crate) const E_SAM_PROFILE: usize = 72;
pub(crate) const E_SAM_PROV: usize = 73;

// --- provenance SCAN instrumentation: is the population absent, or is the walk giving up? ---
pub(crate) const D_SCAN_BODY: usize = 74;
pub(crate) const D_SCAN_STOP: usize = 75;
pub(crate) const D_SCAN_INDY: usize = 76;
pub(crate) const D_SCAN_SITE: usize = 77;
pub(crate) const D_SCAN_NOLEAD: usize = 78;
/// WHERE A MINTED CLOSURE GOES. One note per consumption, so the four rows sum (near enough) to
/// `scan.indy-token-pushed` and say which consumer the provenance path would have to serve.
pub(crate) const D_TOK_RECV: usize = 79;
pub(crate) const D_TOK_ARG: usize = 80;
pub(crate) const D_TOK_STORE: usize = 81;
pub(crate) const D_TOK_OTHER: usize = 82;
/// Shape B, split by WHY it did not apply.
pub(crate) const D_ARG_NOT_INLINED: usize = 83;
pub(crate) const D_ARG_UNGUARDED: usize = 84;
/// The 1,300-a-run population that decides this whole round: a closure passed straight into a
/// call the inliner does NOT splice, split by the call's opcode and by whether the receiver
/// profile that would have admitted it exists at all.
pub(crate) const D_ARG_NI_IFACE: usize = 85;
pub(crate) const D_ARG_NI_VIRT: usize = 86;
pub(crate) const D_ARG_NI_SPECIAL: usize = 87;
pub(crate) const D_ARG_NI_STATIC: usize = 88;
pub(crate) const D_ARG_NI_HASMONO: usize = 89;

// --- THE ROOTED-SPLICE ARM (`gcpoint_body_shape`). A callee the historic rules refuse for a
// reason that is really ONE reason: its live references would not be rooted at a GC point inside
// the splice. Counted at the refusal site so the population is a COUNT, not an estimate.
/// Refused by the straight-line/opcode shape before any rooting question is asked.
pub(crate) const X_GP_SHAPE: usize = 90;
/// The shape would be admitted but the splice is NOT OUTERMOST (`depth != 0`). The window roots
/// the callee's frame and `locals_ptr` roots the caller's, and `locals_ptr` is the compiled
/// method's own frame only at depth 0 — so a nested splice keeps the historic refusal.
pub(crate) const X_GP_DEPTH: usize = 91;
/// A surviving real call inside the splice with the stage-2 half switched off
/// (`RAVA_NO_INLINE_GCPOINT_CALLS=1`).
pub(crate) const X_GP_S2: usize = 92;
/// Shape is fine but the callee's frame does not fit the rooted window budget.
pub(crate) const X_GP_FRAME: usize = 93;
/// Shape is fine but a NESTED spliced callee is not GC-leaf — its own frame would need a window
/// too, and only the outermost splice gets one.
pub(crate) const X_GP_NESTED: usize = 94;
/// Admitted by the rooted-splice arm, and emitted (one note per spliced site, at compile time).
pub(crate) const A_GP: usize = 95;
pub(crate) const E_GP_EMIT: usize = 96;
/// What the emitted splices actually contain — the population the round exists to move.
pub(crate) const E_GP_ALLOCS: usize = 97;
pub(crate) const E_GP_CALLS: usize = 98;

// --- THE STACK-MAP ROUND (`RAVA_NO_INLINE_FREE=1` restores the rows above verbatim). Every one
// of these is zero unless `jit::runtime::stackmaps::declaring()`, so an ON/OFF census pair
// attributes the whole delta to the protocol and not to the flag.
/// A callee the widened interface arm admits ONLY because the single-surviving-call rule and its
/// tail rule are rooting proofs the stack maps discharge (`real_calls != 1`). This is the row
/// `iface.calls!=1` becomes.
pub(crate) const A_IF_MULTI: usize = 99;
/// A spliced site that emitted NO root window at all: no zero loop, no cell-root push/pop, no
/// caller spill and no caller reload. One note per emitted splice.
pub(crate) const E_FREE_WINDOW: usize = 100;
/// A splice at which the CALLER's live ref operand-stack values were DECLARED (zero instructions)
/// so the maps name them across the body's safepoints — the one part of a compiled frame
/// `spill_for_gc` cannot reach from inside a spliced body.
pub(crate) const E_FREE_DECL: usize = 101;
/// Surviving real calls now inside an emitted interface-arm splice — the `E_GP_CALLS` row's twin
/// for the arm that carries no window.
pub(crate) const E_IF_CALLS: usize = 102;

// --- THE PARTIAL (TIER-0 NESTED) BAND, SPLIT BY CLAUSE. `body.partial-gc-unsafe` counted every
// refusal of `partial_inline_gc_safe` as one row, so "which of its four rooting-derived clauses
// blocks the population" could only be answered by reading code. One note per refusal, at the
// clause that made it, so the ON/OFF pair attributes the widening to a named rule.
pub(crate) const X_PF_GETSTATIC: usize = 103;
pub(crate) const X_PF_BRANCH: usize = 104;
pub(crate) const X_PF_CALLS: usize = 105;
pub(crate) const X_PF_TAIL: usize = 106;
/// KEPT in both arms — the frame-buffer capacity bound, not a rooting proof.
pub(crate) const X_PF_FRAME: usize = 107;
/// An admitted nested entry whose splice still carries a GC-capable point — `InlineCallee::
/// partial_gc_points`, the FOURTH SHAPE. Non-zero with the round switched OFF too: the historic
/// `real_calls == 1` population has always been this and was never counted.
pub(crate) const A_PF_GCP: usize = 108;
/// A nested entry admitted ONLY because the stack maps discharge a rooting proof — i.e. one at
/// least one historic clause would have refused. This is the row the round exists to move.
pub(crate) const A_PF_FREE: usize = 109;

// --- DEPTH (`RAVA_NO_DEEP_INLINE=1` restores every row above verbatim and leaves these at zero).
//
// ⭐**THE HISTOGRAM IS IN C2's OWN BUCKETS ON PURPOSE.** `-XX:+PrintInlining` on this exact
// workload prints 27,978 inlined sites, of which 83% sit at depth ≥ 3 and the single largest
// bucket is 11-20. Our own admissions were never bucketed at all, so "how far from C2's SHAPE are
// we" could only be answered by reading code. One note per ADMITTED site, at the one choke point
// every arm returns through, bucketed by the depth of the CALLER frame the site sits in — depth 0
// is a call site in the method being compiled, exactly as C2's depth 0 is.
pub(crate) const H_D0_2: usize = 110;
pub(crate) const H_D3_5: usize = 111;
pub(crate) const H_D6_10: usize = 112;
pub(crate) const H_D11_20: usize = 113;
pub(crate) const H_D21_40: usize = 114;
pub(crate) const H_D41: usize = 115;
/// A callee refused because this compile's whole inline TREE has already absorbed
/// `RAVA_INLINE_BUDGET` bytes of callee body. Per-callee ceilings bound one splice; this bounds
/// the cascade.
pub(crate) const X_BUDGET: usize = 116;
/// Admitted at depth > 0 by an arm that was offered only at depth 0 before this round, split by
/// arm so the population each lifted refusal moves is a COUNT and not an estimate.
pub(crate) const A_DEEP_IFACE: usize = 117;
pub(crate) const A_DEEP_DEVIRT: usize = 118;
pub(crate) const A_DEEP_GP: usize = 119;
/// A nested splice emitted from INSIDE another splice — the emission `Env::in_splice` refused
/// outright until the maps made the parent's variables rooted at every level. One note per
/// emitted nested splice.
pub(crate) const E_DEEP_SPLICE: usize = 120;

/// The same population at EXACT depth, 0..=7 with 7 meaning "7 or deeper". C2's buckets lump
/// 0, 1 and 2 together, and the first reading of this histogram showed 93% of our admissions in
/// that one bucket — which says nothing about whether the chain dies at depth 1 or at depth 2.
/// `jit::max_inline_depth()` is 8 under Tier-0, so eight rows cover the whole reachable range.
pub(crate) const D_EXACT: usize = 121;

/// --- CONTROL FLOW IN AN INTERFACE-ARM BODY (`RAVA_NO_INLINE_CF=1` restores every row above
/// verbatim and leaves these at zero).
///
/// ⭐**`iface.control-flow` WAS THE LARGEST REFUSAL AND IT NEVER HAD A SUBJECT IN THE EMITTER.**
/// `emit_inline`'s general path has built real Cranelift blocks for a spliced body since V4.2 —
/// `find_leaders` in the CALLEE's own pc space, block parameters carrying the operand stack across
/// every edge, one `cont` join whose single parameter is the return value. The straight-line test
/// in that file is a FAST PATH ("this changes no emitted instruction"), not a capability bound.
/// The refusal lived in `iface_body_admissible` alone, and its stated reason was
/// `iface_partial_gc_safe`'s single-linear-path premise — one of the four clauses
/// `inline_free_enabled()` had already retired in that same predicate.
///
/// What is NOT relaxed, and why it is a decoder fact rather than a proof: `emit_inline` walks a
/// spliced body with `insn_len(op)`, which has no arm for `tableswitch`/`lookupswitch`/`jsr`/
/// `ret`/`goto_w`/`jsr_w`. Admitting one of those does not merely fail to inline — the `None`
/// propagates out through `emit_op` and THE WHOLE CALLER FAILS TO COMPILE. Refused, and counted.
pub(crate) const X_IF_CF_UNDEC: usize = 129;
/// An interface-arm callee admitted WITH control flow — the population the round moves.
pub(crate) const A_IF_CF: usize = 130;
/// …and the subset of those that contain a BACK EDGE. Admitted: the splice emits each opcode
/// once whether or not an edge returns to it, so the emitted size stays bounded by the same
/// `INLINE_LEAF_BYTECODE` ceiling every other callee in this arm is admitted against — a loop is
/// a back edge in the IR, never an unrolling.
pub(crate) const A_IF_CF_LOOP: usize = 131;
/// A spliced interface-arm site whose body actually carried a branch into machine code. Counted
/// at EMIT and not at admission, because an admitted callee whose caller then fails to compile
/// removes no dispatch at all.
pub(crate) const E_IF_CF: usize = 132;

/// The ~600-a-run `iface.control-flow` row DECOMPOSED. It counted one refusal per OFFER, and an
/// offer is a (site, compile) pair — so a single branchy body refused at three hundred sites is
/// three hundred rows. Lifting the gate therefore does NOT convert that number into admissions:
/// most of those bodies are ALSO refused by another rule in the same predicate (`getstatic`,
/// `putfield`, `new`, a body with no `invokeinterface` left in it at all). These two rows split
/// the population at the gate, so the next round reads a decomposition instead of a headline.
pub(crate) const A_IF_CF_SHAPE: usize = 133;
pub(crate) const X_IF_CF_OTHER: usize = 134;

const N: usize = 157;

pub(crate) const NAMES: [&str; N] = [
    "site.invokestatic",
    "site.invokespecial",
    "site.final-virtual",
    "site.sam(0xb9)",
    "site.devirt(0xb6|0xb9)",
    "ADMIT.leaf",
    "ADMIT.nested",
    "ADMIT.nested+iface (this round)",
    "ADMIT.sam",
    "ADMIT.devirt",
    "sam.no-profile",
    "sam.not-lambda-cid",
    "sam.mh-kind",
    "sam.site-gone",
    "sam.no-memberref",
    "sam.return-mismatch",
    "sam.arity-mismatch",
    "sam.capture-types",
    "sam.pinned-ptrs",
    "sam.native-collection",
    "sam.body-refused",
    "devirt.no-profile",
    "devirt.no-class-name",
    "devirt.body-refused",
    "body.<init>/<clinit>",
    "body.unresolved",
    "body.class-uninitialised",
    "body.native-map",
    "body.native-served-ns",
    "body.static-mismatch",
    "body.not-final",
    "body.no-Code",
    "body.nested-map-refused",
    "body.partial-gc-unsafe",
    "body.field-resolve",
    "body.getstatic-not-clinit",
    "shape.exception-table",
    "shape.invokeinterface",
    "shape.invokedynamic",
    "shape.new/newarray",
    "shape.putfield",
    "shape.putstatic",
    "shape.instanceof",
    "shape.switch",
    "shape.array-store",
    "shape.void-return",
    "shape.size>96",
    "shape.size>tier0-cap",
    "shape.depth-cap",
    "shape.bad-leaders",
    "shape.data-parallel",
    "shape.other",
    "iface.gate-off",
    "iface.shape",
    "iface.control-flow",
    "iface.calls!=1",
    "iface.tail-not-forwarding",
    "iface.frame>caller",
    "iface.size",
    "EMIT.iface-splice",
    "LEARN.site-poisoned-by-2nd-closure",
    "LEARN.site-profiled-from-compiled-code",
    "PROV.sam-receiver-minted-here",
    "PROV.lambda-arg into an inlined call",
    "ADMIT.sam-by-provenance (no profile)",
    "ADMIT.ctx-inline (this round)",
    "prov.indy-not-metafactory",
    "prov.mh-kind",
    "prov.descriptor-mismatch",
    "prov.body-refused",
    "ctx.no-sole-iface-site-on-param",
    "ctx.site-already-bound",
    "EMIT.sam-splice (profile)",
    "EMIT.sam-splice (provenance)",
    "scan.bodies-walked",
    "scan.stopped-before-end",
    "scan.indy-token-pushed",
    "scan.invoke-sites-recorded",
    "scan.no-leaders",
    "tok.consumed-as-receiver",
    "tok.consumed-as-argument",
    "tok.stored-to-a-local",
    "tok.consumed-elsewhere",
    "ctx.arg-site-not-inlined",
    "ctx.arg-site-unguarded",
    "ctx.not-inlined: invokeinterface",
    "ctx.not-inlined: invokevirtual",
    "ctx.not-inlined: invokespecial",
    "ctx.not-inlined: invokestatic",
    "ctx.not-inlined BUT the site HAS a receiver profile",
    "gcpoint.shape-refused",
    "gcpoint.not-outermost (depth>0)",
    "gcpoint.surviving-call, calls half off",
    "gcpoint.frame>window-budget",
    "gcpoint.nested-callee-not-gc-leaf",
    "ADMIT.rooted-splice (this round)",
    "EMIT.rooted-splice",
    "EMIT.rooted-splice: allocations now inside a spliced body",
    "EMIT.rooted-splice: calls now inside a spliced body",
    "ADMIT.iface calls!=1 (stack maps discharge the rooting proof)",
    "EMIT.splice with NO root window (stack maps)",
    "EMIT.caller ref-stack DECLARED at a splice (0 instructions)",
    "EMIT.iface-splice: calls now inside a spliced body",
    "partial.getstatic beside a surviving call",
    "partial.control-flow beside a surviving call",
    "partial.calls!=1",
    "partial.tail-not-forwarding",
    "partial.frame>caller",
    "ADMIT.nested carrying a surviving GC point (the 4th shape)",
    "ADMIT.nested that only the stack maps allow (this round)",
    "DEPTH admitted @ 0-2",
    "DEPTH admitted @ 3-5",
    "DEPTH admitted @ 6-10",
    "DEPTH admitted @ 11-20",
    "DEPTH admitted @ 21-40",
    "DEPTH admitted @ 41+",
    "depth.tree-budget exhausted",
    "ADMIT.iface-body at depth>0 (this round)",
    "ADMIT.devirt at depth>0 (this round)",
    "ADMIT.rooted-splice at depth>0 (this round)",
    "EMIT.splice nested inside another splice (this round)",
    "depth==0 admitted",
    "depth==1 admitted",
    "depth==2 admitted",
    "depth==3 admitted",
    "depth==4 admitted",
    "depth==5 admitted",
    "depth==6 admitted",
    "depth>=7 admitted",
    "iface.control-flow: switch/jsr/goto_w (emit_inline's decoder)",
    "ADMIT.iface body WITH control flow (this round)",
    "ADMIT.iface body with a BACK EDGE (this round)",
    "EMIT.iface-splice carrying control flow",
    "iface body WITH control flow: the shape gate passed it",
    "iface body WITH control flow: blocked by another iface rule",
    // --- WHY THE COMPILED-CODE LEARN HOOK MEASURES ZERO. The hook exists and its census row
    // reads 0; these five say which of its early returns eats the population.
    "LEARN.hook reached from compiled code",
    "LEARN.refused: caller==0 or chain disabled",
    "LEARN.refused: receiver word is not a bare arena pointer",
    "LEARN.refused: arena object is not a lambda",
    "LEARN.refused: MethodHandle kind not 6/7",
    "LEARN.receiver is a HANDLE-backed LAMBDA (the shape test cannot see it)",
    "LEARN.receiver is a handle that is not a lambda",
    // --- CLASS HIERARCHY ANALYSIS: the answer the profile cannot give.
    "CHA.asked (site had no receiver profile)",
    "CHA.refused: closure unprovable (interface owner / unloadable)",
    "CHA.refused: more than one loaded implementor",
    "CHA.sole implementor found -> guarded devirt",
    "CHA.sole implementor has no arena class id yet",
    "CHA.one body shared by SEVERAL types -> widened guard",
    "CHA.stats refused: concrete subtype count exceeded active cap",
    "CHA.stats refused: a concrete subtype did not resolve to bytecode",
    "CHA.stats refused: concrete subtypes resolve to several bodies",
    "CHA.stats diagnostic: one bytecode body shared by more than four types",
    "CHA.stats asked: invokevirtual",
    "CHA.stats asked: invokeinterface",
    "CHA.stats inlinable plan: invokevirtual",
    "CHA.stats inlinable plan: invokeinterface",
    "CHA.stats inlinable plan whose opcode emission gate is enabled",
];

static C: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

#[inline]
pub(crate) fn note(row: usize) {
    C[row].fetch_add(1, Relaxed);
}

/// `RAVA_CHA_STATS=1`: split CHA's coarse polymorphic refusal without changing a verdict.
/// The only expensive part it enables is the full-hierarchy diagnostic probe at an over-cap site;
/// production admission never reads any result produced by that probe.
pub(crate) fn cha_stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CHA_STATS").is_some())
}

#[inline]
pub(crate) fn note_cha(row: usize) {
    if cha_stats_enabled() {
        note(row);
    }
}

/// Bucket one ADMITTED site by the depth of the frame it sits in, in C2's own buckets.
#[inline]
pub(crate) fn note_depth(depth: u8) {
    note(D_EXACT + (depth.min(7) as usize));
    note(match depth {
        0..=2 => H_D0_2,
        3..=5 => H_D3_5,
        6..=10 => H_D6_10,
        11..=20 => H_D11_20,
        21..=40 => H_D21_40,
        _ => H_D41,
    });
}

/// Map `nested_blocker`'s verdict onto a row.
#[inline]
pub(crate) fn note_shape(reason: &'static str) {
    note(match reason {
        "exception-table" => X_SH_EXCEPTIONS,
        "invokeinterface" => X_SH_IFACE,
        "invokedynamic" => X_SH_INDY,
        "new" => X_SH_NEW,
        "putfield" => X_SH_PUTFIELD,
        "putstatic" => X_SH_PUTSTATIC,
        "instanceof" => X_SH_INSTANCEOF,
        "switch" => X_SH_SWITCH,
        "array-store" => X_SH_ARRAYSTORE,
        "void-return" => X_SH_VOID,
        "size>96" => X_SH_SIZE96,
        "size>tier0" => X_SH_SIZE_T0,
        "depth" => X_SH_DEPTH,
        "bad-leaders" => X_SH_LEADERS,
        "data-parallel" => X_SH_DATAPAR,
        _ => X_SH_OTHER,
    });
}

pub fn report() -> String {
    let v: Vec<u64> = C.iter().map(|c| c.load(Relaxed)).collect();
    let total: u64 = v.iter().sum();
    if total == 0 {
        return String::new();
    }
    let offered: u64 = v[S_STATIC] + v[S_SPECIAL] + v[S_FINALVIRT] + v[S_SAM] + v[S_DEVIRT];
    let admitted: u64 = v[A_LEAF] + v[A_NESTED] + v[A_IFACE] + v[A_SAM] + v[A_DEVIRT];
    let mut out = format!(
        "[INLCEN] {offered} sites offered, {admitted} admitted ({:.1}%) | \
         widened iface-body arm: {} admitted, {} spliced |          learn: {} sites profiled from compiled code, {} poisoned\n\
         [INLCEN] provenance: {} sam-receivers + {} lambda-args proved -> {} sam-by-provenance + {} ctx-inline admitted; \
         emitted {} profile / {} provenance",
        if offered == 0 { 0.0 } else { admitted as f64 * 100.0 / offered as f64 },
        v[A_IFACE],
        v[E_IFACE_EMIT],
        v[L_LEARNED],
        v[L_POISONED],
        v[P_SAM_RECV],
        v[P_CTX_ARG],
        v[A_PROV_SAM],
        v[A_CTX],
        v[E_SAM_PROFILE],
        v[E_SAM_PROV],
    );
    // CONTROL FLOW IN THE INTERFACE ARM, beside the arm's own totals: what the round admits,
    // how much of it loops, what reached machine code, and the one shape still refused (and
    // refused for a decoder reason, not a proof — see `X_IF_CF_UNDEC`).
    if v[A_IF_CF] + v[E_IF_CF] + v[X_IF_BRANCH] + v[X_IF_CF_UNDEC] + v[X_IF_CF_OTHER] > 0 {
        out.push_str(&format!(
            "
[INLCEN] iface-body control flow: {} branchy offers -> {} past the shape gate ({} blocked by another iface rule) -> {} admitted ({} with a back edge) -> {} spliced; still refused: {} by the gate, {} switch/jsr/goto_w",
            v[A_IF_CF_SHAPE] + v[X_IF_CF_OTHER], v[A_IF_CF_SHAPE], v[X_IF_CF_OTHER],
            v[A_IF_CF], v[A_IF_CF_LOOP], v[E_IF_CF], v[X_IF_BRANCH], v[X_IF_CF_UNDEC],
        ));
    }
    // THE DEPTH HISTOGRAM, in the buckets `-XX:+PrintInlining` was counted into on this same
    // workload, so the two are read side by side rather than converted. `deep` is what the round
    // added: admissions by an arm that was depth-0-only before it.
    let hist = [v[H_D0_2], v[H_D3_5], v[H_D6_10], v[H_D11_20], v[H_D21_40], v[H_D41]];
    let hsum: u64 = hist.iter().sum();
    if hsum > 0 {
        let deep: u64 = hsum - hist[0];
        out.push_str(&format!(
            "\n[INLCEN] depth histogram (C2 buckets)  0-2: {} | 3-5: {} | 6-10: {} | 11-20: {} | \
             21-40: {} | 41+: {}   => {:.1}% at depth>=3\
             \n[INLCEN] depth>0 admissions this round: {} iface-body + {} devirt + {} rooted-splice; \
             {} nested splices emitted; {} refused on the tree budget",
            hist[0], hist[1], hist[2], hist[3], hist[4], hist[5],
            deep as f64 * 100.0 / hsum as f64,
            v[A_DEEP_IFACE], v[A_DEEP_DEVIRT], v[A_DEEP_GP], v[E_DEEP_SPLICE], v[X_BUDGET],
        ));
        out.push_str(&format!(
            "
[INLCEN] depth exact  d0: {} | d1: {} | d2: {} | d3: {} | d4: {} | d5: {} |              d6: {} | d7+: {}   (cap RAVA_INLINE_DEPTH={}, tree budget RAVA_INLINE_BUDGET={})",
            v[D_EXACT], v[D_EXACT + 1], v[D_EXACT + 2], v[D_EXACT + 3],
            v[D_EXACT + 4], v[D_EXACT + 5], v[D_EXACT + 6], v[D_EXACT + 7],
            crate::jit::max_inline_depth(),
            crate::vm::inline_targets::inline_tree_budget(),
        ));
    }
    let mut rows: Vec<(u64, &str)> =
        v.iter().copied().zip(NAMES).filter(|r| r.0 > 0).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    for (n, s) in rows {
        out.push_str(&format!("\n[INLCEN]   {n:>9}  {s}"));
    }
    out
}
