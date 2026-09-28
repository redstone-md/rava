//! FRAME-FREE EXECUTION OF TRIVIAL CALLEE BODIES.
//!
//! WHY. Ledger row 344 profiles the interpreter gate at **39.3% call machinery**, and every part
//! of that machinery is already the product of an optimisation pass — so the remaining move is
//! FEWER FRAMES rather than cheaper ones. Row 360 counted the population before anyone built for
//! it (`RAVA_SHAPE_CENSUS`, 354,094,235 interpreted calls on `ChunkFill 1 16`):
//!
//! ```text
//! GETTER        15,235,298    4.3%     aload_0; getfield #i; <T>return
//! CONST         15,717,978    4.4%     <const>; <T>return   /   return
//! SETTER             2,089    0.0%
//! SMALL(<16B)  124,462,158   35.1%
//! OTHER        198,676,712   56.1%
//! ```
//!
//! This module serves the first two rows — **8.7% of every interpreted call** — and nothing else.
//! That is a TRACER BULLET on purpose: the 35.1% `SMALL` row needs a second interpreter over a
//! subset of the opcode set, which is 200+ lines of duplicated arithmetic semantics, and this tree
//! has repeatedly built machinery for populations that turned out not to pay. Prove the MECHANISM
//! (does eliding a frame buy anything at all?) on the two rows that need no interpreter, then
//! decide whether the third is worth writing.
//!
//! WHAT A FRAME COSTS, and therefore what is skipped here: `Frame::new` (a pooled `Vec` take, a
//! `max_locals` resize, a `max_stack` reserve), an `Arc<LoadedClass>` clone, `frames::depth_inc`,
//! the `opcodes::run` prologue and its dispatch loop, the `Frame` drop and its pool gives.
//!
//! ADMISSION IS BY BYTECODE ONLY. The classifier reads `code.bytecode` and nothing else — no class
//! name, no method name, no descriptor text, no receiver identity, no timing. A body either has
//! one of the two shapes or it does not.
//!
//! SOUNDNESS. Both shapes are PURE READS: no store of any kind, no call, no allocation, no branch.
//! So a refusal at any point — including one taken after the classifier has already said yes, such
//! as a null receiver — can fall back to the ordinary framed path with nothing to undo. That is
//! why the null-receiver case below REFUSES instead of throwing: the framed path raises the same
//! NullPointerException, and raises it with the stack-trace line this path would have been missing.
//!
//! ⚠**GC ROOTING — WHY THERE IS NONE, AS AN AUDIT AND NOT AN ASSUMPTION.** `args` here is a plain
//! `Vec<Value>`, not a `Frame`'s `locals`, so it is NOT in the root set: an allocation on this
//! path could collect and leave every value in it stale. That is precisely the use-after-free
//! class rows 340-342 swept out of seven sites. So the served path was audited for allocation
//! rather than assumed free of it. `Flat::Void`, the constants and `Flat::Local0` execute no guest
//! code at all. `Flat::Getter` reaches `getfield_value`, whose only allocations are the two
//! `make_exception` calls on a NULL receiver — which this path refuses before calling it. Every
//! other line there is a pure read (`tlcache` lookup, `heap.get_field_hint`, an arena cell deref,
//! `field_or_default`); `resolve_field_id` interns on the RUST side, which moves no guest object.
//! ⇒ no collection can occur between entry and return, so there is nothing for a root to protect.
//! **Any widening of the admitted shapes must redo this audit** — a `new`, a `ldc` of a String, or
//! a call would all break it, and all three are refused today.
//!
//! ⭐**THE WIDENING HAPPENED, AND THE AUDIT WAS REDONE** (`dag_blocker`, kill switch
//! `RAVA_NO_FLAT_DAG=1`). A body whose control flow is a DAG with FORWARD edges only is now
//! admitted, which is what it takes to reach the shapes that dominate the `[STUCK]` census —
//! `return size == 0;` and fastutil's `int n = end - start; return n >= 0 ? n : n + length;`. The
//! added opcodes are branches, local stores and `iinc`: none of them calls, allocates, or writes
//! anything that outlives the runner, so the no-allocation audit above still holds and a bail is
//! still free. The new field read (`flatbody::arena_field`) reads an arena CELL and refuses every
//! other receiver — allocating nothing, and never reaching `make_exception`.

use crate::class_loader::LoadedClass;
use crate::value::Value;
use crate::Vm;
use crate::VmError;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

/// The two shapes, decoded. `Copy` so the memo below can be a plain array.
#[derive(Clone, Copy, PartialEq)]
enum Flat {
    /// Not one of the shapes. Cached like any other answer — a method body never changes after
    /// load, so a `No` can never later become a `Yes`.
    No,
    /// `aload_0; getfield #i; if{ne,eq} +7; iconst_1; goto +4; iconst_0; ireturn` — the shape
    /// javac emits for `return field == 0;` (and, with `ifeq`, for `!= 0`). Carries the
    /// constant-pool index and whether the test is inverted.
    ///
    /// ⭐**THIS IS THE SINGLE BIGGEST NAMED POPULATION IN THE BOOT.**
    /// `RAVA_PROFILE_BOOT_DISPATCH` reports 15,212,176 interpreted entries against 7,712,120
    /// compiled ones, and `[STUCK]` says **24 methods carry 13,463,451 of the interpreted ones —
    /// 88.5%**. Three of the top five are byte-identical bodies of exactly this shape:
    ///
    /// ```text
    ///   3,828,505  it/unimi/dsi/fastutil/longs/LongOpenHashSet.isEmpty()Z
    ///   1,044,696  it/unimi/dsi/fastutil/objects/ReferenceOpenHashSet.isEmpty()Z
    ///   1,043,468  it/unimi/dsi/fastutil/objects/ObjectArrayList.isEmpty()Z
    /// ```
    ///
    /// Each is `0: aload_0 / 1: getfield size:I / 4: ifne 11 / 7: iconst_1 / 8: goto 12 /
    /// 11: iconst_0 / 12: ireturn` — thirteen bytes that build a whole `Frame` (two pooled
    /// `Vec`s, a `max_locals` resize, an `Arc<LoadedClass>` clone, a registry push/pop and a drop)
    /// to compare an `int` against zero. They were refused only because the shape BRANCHES, which
    /// `Getter` does not allow and `Straight` (straight-line only) cannot reach.
    ///
    /// ⛔Neither of the two obvious cures works, both measured: compiling them is **+2.54%**
    /// (`RAVA_NS_BYTECODE`, the sound per-method gate) and inlining them into the caller is
    /// blocked because inlining SKIPS the dispatch where the native would have won. Serving them
    /// frame-free is the third option and the only one that changes nothing about dispatch.
    ///
    /// ⛔**AND THIS ARM IS NOT HOW IT GETS SERVED — see `dag_blocker`.** Measured, this shape came
    /// out **2.03% SLOWER**: `getfield_value` on a `Value::Ref` receiver costs about what the
    /// `Frame` costs, so the frame came off and nothing was gained. The shape stays opt-in
    /// (`RAVA_FLAT_ZEROTEST`) as the record of that; with it off, `hint_of` deliberately declines
    /// to claim the body here so the forward-DAG runner can take it and read the ARENA CELL
    /// directly, which is the only version of this that is cheaper than the frame.
    FieldZeroTest(u16, bool),
    /// `aload_0; getfield #i; <T>return` — carries the constant-pool index.
    Getter(u16),
    ConstInt(i32),
    ConstLong(i64),
    ConstFloat(f32),
    ConstDouble(f64),
    ConstNull,
    /// A bare `return`: nothing to push.
    Void,
    /// `<X>load_0; <X>return` — hand local slot 0 straight back. `return this` in a builder,
    /// `identity`, and every one-argument static passthrough.
    ///
    /// ⚠**SLOT 0 ONLY, and that restriction is load-bearing.** This hook runs BEFORE
    /// `expand_locals_into`, so `args` is one entry per ARGUMENT while the bytecode indexes JVM
    /// SLOTS — and a category-2 parameter occupies two slots. The two agree at index 0 for every
    /// descriptor and can disagree at any index after a `long`/`double`: in
    /// `double h(double a, double b)`, `dload_3` is `b` but `args[3]` does not exist. Slots 1..n
    /// are therefore left to the framed path rather than served with a descriptor walk that would
    /// cost what the frame does.
    Local0,
    /// A straight-line body `flatbody::run` can execute. Carries nothing — the executor reads the
    /// body itself.
    Straight,
}

/// Classify a body. Runs once per (class, method); every call after is a memo hit.
fn classify(bc: &[u8]) -> Flat {
    // `aload_0 (0x2a); getfield (0xb4) hi lo; <T>return`. The return opcode is not cross-checked
    // against the field's type: the verifier already guarantees they agree, and this path pushes
    // whatever `getfield` yields, exactly as the framed path would.
    if let [0x2a, 0xb4, hi, lo, r] = bc {
        if (0xac..=0xb0).contains(r) {
            return Flat::Getter(u16::from_be_bytes([*hi, *lo]));
        }
    }
    // `aload_0; getfield #f; if{ne,eq} 11; iconst_1; goto 12; iconst_0; ireturn`.
    //
    // The branch offsets are matched as LITERAL BYTES rather than decoded, which is the whole
    // safety argument: only this exact thirteen-byte encoding classifies, so there is no shape
    // whose control flow this arm could mis-model. `0x9a` is `ifne` (jump when the field is
    // non-zero, so the body yields `field == 0`); `0x99` is `ifeq` (yields `field != 0`).
    if let [0x2a, 0xb4, hi, lo, br @ (0x99 | 0x9a), 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac] =
        bc
    {
        return Flat::FieldZeroTest(u16::from_be_bytes([*hi, *lo]), *br == 0x99);
    }
    match bc {
        // `return` — a void body with nothing in it: empty constructors, no-op overrides.
        [0xb1] => Flat::Void,
        [0x01, 0xb0] => Flat::ConstNull,
        // iconst_m1 .. iconst_5 map to -1 ..= 5, then ireturn.
        [c @ 0x02..=0x08, 0xac] => Flat::ConstInt(*c as i32 - 3),
        [0x09, 0xad] => Flat::ConstLong(0),
        [0x0a, 0xad] => Flat::ConstLong(1),
        [0x0b, 0xae] => Flat::ConstFloat(0.0),
        [0x0c, 0xae] => Flat::ConstFloat(1.0),
        [0x0d, 0xae] => Flat::ConstFloat(2.0),
        [0x0e, 0xaf] => Flat::ConstDouble(0.0),
        [0x0f, 0xaf] => Flat::ConstDouble(1.0),
        // bipush b; ireturn   /   sipush hi lo; ireturn
        [0x10, b, 0xac] => Flat::ConstInt(*b as i8 as i32),
        [0x11, hi, lo, 0xac] => Flat::ConstInt(i16::from_be_bytes([*hi, *lo]) as i32),
        // `<X>load_0; <X>return`, the five type pairs. The load and the return must agree on
        // type — a mismatched pair is not valid bytecode, but pairing them explicitly means a
        // malformed body classifies as `No` instead of moving a value of the wrong width.
        [0x1a, 0xac] | [0x1e, 0xad] | [0x22, 0xae] | [0x26, 0xaf] | [0x2a, 0xb0] => Flat::Local0,
        _ => Flat::No,
    }
}

/// NO MEMO, ON PURPOSE. The obvious design here is a direct-mapped `(class, method) -> Flat`
/// cache, and a first version had one. `classify` is a slice pattern match whose every arm is
/// rejected by a LENGTH compare — an eleven-byte body fails all of them in a handful of
/// instructions — so a thread-local table would have added a `RefCell` borrow, a pointer mix and a
/// tag compare to save work that costs less than the lookup. Measure the classifier before caching
/// it, not after.
fn _memo_rationale() {}


/// Would `flatbody::run` be allowed to execute this body?
///
/// ⚠**THIS WALKS THE BYTECODE, SO IT MUST NEVER RUN ON THE CALL PATH.** Ledger row 363: it did,
/// and the 75% of calls it REFUSES paid the walk — 266 M of them a gate — which dragged a −2.69%
/// change to −0.80%. The answer now lives in `Code::flat_hint`, computed once per method.
///
/// Beyond the opcode whitelist it also simulates the operand stack, because `flatbody` runs on a
/// fixed four-slot array: a body that would need a fifth slot is refused HERE rather than bailing
/// half way through the executor.
pub fn is_straight_candidate(bc: &[u8]) -> bool {
    straight_blocker(bc).is_none()
}

/// `RAVA_STRAIGHT_CEILING` — see the arm that uses it. Inert unless set.
fn ceiling_probe() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_STRAIGHT_CEILING").is_some())
}

/// The candidate opcodes the ceiling probe pretends to support: every remaining entry in the
/// blocker table that a wider whitelist could plausibly reach. Stack deltas are NOT modelled for
/// these (the probe reports depth as unchanged), which is why it may only ever be used to COUNT.
/// Every remaining candidate a wider whitelist could plausibly reach, EXCEPT the invokes and
/// `new`. Those are excluded on purpose: a body that calls cannot be run frame-free without
/// nesting, which is a different feature, and `new` allocates, which breaks the GC contract this
/// path rests on. So `served + bailed` under this probe is the ceiling of *the whitelist line* —
/// what a frame-free executor could reach if it did everything short of calling and allocating.
const CEILING_SET: &[u8] = &[
    0x12, 0x13, // ldc / ldc_w
    0x36, 0x37, 0x38, 0x39, 0x3a, // <t>store
    0x3b, 0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46,
    0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, // <t>store_<n>
    0x4f, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, // <t>astore
    0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, // pop/dup/swap
    0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73, // div / rem
    0x84, // iinc
    0x94, 0x95, 0x96, 0x97, 0x98, // lcmp / fcmp / dcmp
    0x99, 0x9a, 0x9b, 0x9c, 0x9d, 0x9e, // if<cond>
    0x9f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, // if_icmp<cond>
    0xa5, 0xa6, 0xa7, // if_acmp / goto
    0xb3, 0xb5, // putstatic / putfield
    0xbe, // arraylength
    0xc0, 0xc1, // checkcast / instanceof
    0xc6, 0xc7, // ifnull / ifnonnull
];

/// Byte length of an opcode the probe steps over. `None` for anything variable-length or unknown —
/// the walk must never guess a length, because a wrong one desynchronises it and the census then
/// reports opcodes that are not there.
fn opcode_len(op: u8) -> Option<usize> {
    Some(match op {
        0x36..=0x3a => 2,
        0x12 => 2,
        0x3b..=0x5f | 0x6c..=0x73 | 0x94..=0x98 | 0xbe => 1,
        0x13 | 0x84 | 0x99..=0xa7 | 0xb3 | 0xb5 | 0xc0 | 0xc1 | 0xc6 | 0xc7 => 3,
        _ => return None,
    })
}

/// The longest body the straight-line executor will consider.
///
/// ⭐**64, RAISED FROM 24 ON THE CENSUS, NOT ON TASTE.** Splitting the structural bucket showed
/// `body > 24 bytes` alone refusing **70,222,987 calls a gate — 32.13% of everything still on the
/// framed path**, the largest single entry left. The limit costs nothing per call to raise: the
/// classifier's walk is memoised in `Code::flat_hint` and runs once per METHOD. What a longer body
/// does cost is a longer wasted run when the executor bails mid-way — and `RAVA_FLAT_STAT` reads
/// `refused: bailed 0.00%`, so that risk is measured, not assumed.
pub(crate) const MAX_STRAIGHT_BYTES: usize = 64;

/// Structural refusal codes, kept out of the 0x00-0xCA opcode range so the census can tell
/// "an opcode I do not run" from "a body I will not consider". They were ONE code (`0xFF`) and
/// that hid four different reasons behind 30.40% of the table — a bucket that large is not a
/// finding, it is a missing measurement.
pub(crate) const BLK_EMPTY: u8 = 0xFB;
pub(crate) const BLK_DEPTH: u8 = 0xFC;
pub(crate) const BLK_RETURN: u8 = 0xFD;
pub(crate) const BLK_TOO_LONG: u8 = 0xFE;
/// A branch whose target is not STRICTLY FORWARD, is outside the body, or lands somewhere the
/// linear decode did not call an instruction start. DAG admission only.
pub(crate) const BLK_BACKEDGE: u8 = 0xFA;
/// A local WRITE naming a slot outside the DAG runner's scratch array. DAG admission only.
pub(crate) const BLK_SLOT: u8 = 0xF9;

/// ⭐**WHY was a body refused, not just THAT it was** — the census behind
/// `RAVA_STRAIGHT_BLOCKERS`. Returns `None` when the straight-line executor may run this body,
/// and otherwise the FIRST opcode that disqualified it (`0xFF` for a structural refusal: empty,
/// over-long, ran off the end, or a stack-depth violation).
///
/// The frame-free path serves 8.7% of interpreted calls; the other 91% pay a frame measured at
/// 122-138 ns against HotSpot's 7.2. Widening admission is worth more than making the frame
/// cheaper — but only for opcodes whose population is actually large, and "MC is full of X" is a
/// belief until counted. This is the counter.
pub fn straight_blocker(bc: &[u8]) -> Option<u8> {
    let ceiling = ceiling_probe();
    if bc.is_empty() {
        return Some(BLK_EMPTY);
    }
    if bc.len() > MAX_STRAIGHT_BYTES {
        return Some(BLK_TOO_LONG);
    }
    let (mut i, mut depth): (usize, i32) = (0, 0);
    while i < bc.len() {
        // (bytes consumed, net stack effect)
        let (step, delta): (usize, i32) = match bc[i] {
            0x01..=0x0f => (1, 1),
            0x1a..=0x2d => (1, 1),
            0x10 => (2, 1),
            0x11 => (3, 1),
            // ⭐**`ldc2_w` — 8.68% of the remaining refusals, and safe where `ldc` is not.** A
            // `ldc2_w` operand can ONLY be a `long` or a `double` (JVMS 4.4.5); it can never be a
            // `String` or a `Class`, so it cannot allocate, intern, or trigger a `<clinit>` — the
            // three reasons `0x12`/`0x13` stay out. The executor re-checks the pool entry's kind
            // and bails on anything else, so a malformed class refuses rather than guessing.
            0x14 => (3, 1),
            0x15..=0x19 => (2, 1),
            // arrayref, index -> value
            0x2e..=0x35 => (1, -1),
            // ineg/lneg leave the depth alone; the binary forms consume two and leave one.
            0x74 | 0x75 => (1, 0),
            0x60 | 0x61 | 0x64 | 0x65 | 0x68 | 0x69 => (1, -1),
            // ⭐**fneg/dneg and every numeric conversion** — admitted once `pure_ops` made them
            // SHARED with the dispatch loop rather than restated (row 402: 19.3% of the framed
            // path's calls are refused by a cast alone). Depth is unchanged: in THIS value model
            // a long or double occupies ONE operand slot, so `i2l` is not a widening push — the
            // same reason `0x74 | 0x75` above are `(1, 0)`.
            0x76 | 0x77 | 0x85..=0x93 => (1, 0),
            // fadd/dadd, fsub/dsub, fmul/dmul — also shared. Division and remainder stay out.
            0x62 | 0x63 | 0x66 | 0x67 | 0x6a | 0x6b => (1, -1),
            0x78..=0x83 => (1, -1),
            // objectref -> value
            0xb4 => (3, 0),
            // ⭐**`getstatic` — 12.97% of the remaining refusals, and the largest single item
            // left.** Admitted on a strict condition the executor enforces: it is served ONLY from
            // the generation-guarded per-site cache (`tlcache::getstatic_get`), never resolved.
            // A cache HIT is proof the framed path already resolved this exact site, so the owner
            // is initialised, nothing is loaded, no `<clinit>` runs and nothing allocates — the
            // three things that would otherwise break this path's GC contract. A MISS bails, and
            // the framed path then resolves and fills the cache for next time.
            0xb2 => (3, 1),
            // A return must be the LAST opcode, and must have something to return.
            0xac..=0xb0 => {
                return if i + 1 == bc.len() && depth == 1 { None } else { Some(BLK_RETURN) }
            }
            0xb1 => return if i + 1 == bc.len() && depth == 0 { None } else { Some(BLK_RETURN) },
            // ⭐**CEILING PROBE (`RAVA_STRAIGHT_CEILING`, inert by default).**
            //
            // A FIRST-BLOCKER histogram cannot predict what admitting an opcode is WORTH: the
            // bodies it counts have other blockers behind the first. It said `ldc2_w` 8.68% and
            // delivered 2.68%; it said `getstatic` 12.97% and delivered 0.52%. Three misses is a
            // defect in the instrument, not bad luck.
            //
            // This answers the question the histogram cannot: admit the whole candidate set in the
            // CLASSIFIER only, leave the executor alone, and read how many calls reach it. Every
            // body it wrongly admits simply BAILS — always safe here — and shows up as
            // `refused: bailed`, so `served + bailed` is the CEILING of this entire line of work.
            // Sound, just slower; never a shipping configuration.
            other if ceiling && CEILING_SET.contains(&other) => (opcode_len(other)?, 0),
            other => return Some(other),
        };
        depth += delta;
        if depth < 0 || depth > crate::flatbody::STACK as i32 {
            return Some(BLK_DEPTH);
        }
        i += step;
    }
    // Ran off the end without a return: not a complete body.
    Some(BLK_RETURN)
}

// ══════════════════════════ THE FORWARD-DAG CLASSIFIER ══════════════════════════
//
// ⭐**WHY THIS EXISTS.** A product boot enters 14.1 M interpreted method bodies and the `[STUCK]`
// census says **24 methods carry 88.5% of them**. `straight_blocker` cannot reach them: it refuses
// a BRANCH, and both dominant shapes have one —
//
//     return size == 0;                                   (getfield; ifne; iconst_1; goto; iconst_0)
//     int size() { int n = end - start; return n >= 0 ? n : n + length; }
//
// — so thirteen to twenty-five bytes build a whole `Frame` to compare an `int`.
//
// The admission rule is ONE property: the body's control flow is a DAG whose every edge points
// FORWARD. Nothing else generalises as cheaply — see the header of `flatbody` for what forward-only
// buys the executor (no loop bound, no safepoint, no basic-block structure, a bounded bail).

/// Control-flow class of an admitted opcode.
#[derive(Clone, Copy, PartialEq)]
enum Flow {
    /// Falls through to the next instruction.
    Fall,
    /// Conditional branch: BOTH the target and the next instruction are successors.
    Cond,
    /// Unconditional branch: the target only.
    Goto,
    /// Ends the body. Carries the operand depth the return requires.
    Ret(i32),
}

/// `(length, net stack effect, control flow)` for an opcode the DAG runner may execute — so this
/// table IS the whitelist, and `None` IS the refusal. It is a superset of `straight_blocker`'s set
/// plus local stores, `iinc` and the branches.
///
/// ⚠**KEEP IT IN LOCKSTEP WITH `flatbody::exec`'s ARMS.** A drift in the ADMITTED direction is a
/// mid-body bail (safe, just slower); a drift in the LENGTH is a desynchronised decode, which is
/// why every entry here is fixed-length and everything variable-length (`wide`, the two switches)
/// is refused outright rather than measured.
fn dag_step(op: u8) -> Option<(usize, i32, Flow)> {
    use Flow::*;
    Some(match op {
        0x00 => (1, 0, Fall),
        // aconst_null, iconst_*, lconst_*, fconst_*, dconst_*
        0x01..=0x0f => (1, 1, Fall),
        0x10 => (2, 1, Fall),
        // sipush, and ldc2_w (long/double only — it cannot intern or run a <clinit>).
        0x11 | 0x14 => (3, 1, Fall),
        0x15..=0x19 => (2, 1, Fall),
        0x1a..=0x2d => (1, 1, Fall),
        // arrayref, index -> value
        0x2e..=0x35 => (1, -1, Fall),
        // <t>store and <t>store_<n>: the operand leaves the stack for a scratch local.
        0x36..=0x3a => (2, -1, Fall),
        0x3b..=0x4e => (1, -1, Fall),
        // add/sub/mul for all four types. Division and remainder stay OUT: they throw, and
        // `Integer.MIN_VALUE / -1` overflows.
        0x60..=0x6b => (1, -1, Fall),
        // <t>neg — one operand in, one out.
        0x74..=0x77 => (1, 0, Fall),
        // shifts and the bitwise families.
        0x78..=0x83 => (1, -1, Fall),
        0x84 => (3, 0, Fall),
        // numeric conversions. Depth is UNCHANGED because a long or double occupies ONE operand
        // slot in this value model — the same reason `<t>neg` above is zero.
        0x85..=0x93 => (1, 0, Fall),
        // if<cond> and ifnull/ifnonnull consume one; if_icmp<cond>/if_acmp<cond> consume two.
        // `lcmp`/`fcmp`/`dcmp` (0x94..=0x98) stay out — NaN and the l/g variants.
        0x99..=0x9e | 0xc6 | 0xc7 => (3, -1, Cond),
        0x9f..=0xa6 => (3, -2, Cond),
        0xa7 => (3, 0, Goto),
        // getstatic, CACHE-HIT ONLY in the executor; getfield, ARENA-CELL ONLY.
        0xb2 => (3, 1, Fall),
        0xb4 => (3, 0, Fall),
        0xac..=0xb0 => (1, -1, Ret(1)),
        0xb1 => (1, 0, Ret(0)),
        _ => return None,
    })
}

/// `None` when the DAG runner may execute this body, otherwise the first thing that disqualified
/// it (an opcode byte, or one of the `BLK_*` structural codes).
///
/// Runs ONCE PER METHOD — the answer lives in `Code::flat_hint`. Ledger row 363: an admission test
/// that walks the bytecode on the CALL path is paid by the 75% of calls it refuses, and that alone
/// dragged a −2.69% change to −0.80%.
///
/// ⚠**`inline(never)` IS LOAD-BEARING, not tidiness.** This body holds three fixed arrays, and a
/// function reserves its WHOLE frame in the prologue — the exact reason `invoke_framed` was split
/// out of `invoke_idx` (row 395). Inlined into `hint_of` and thence into `try_run`, every one of
/// the millions of calls that only READ the memoised hint would pay this walk's stack adjustment.
/// The arrays are `u8`/`i8` for the same reason: a 64-byte body has at most 64 instructions and
/// the operand depth is bounded by `flatbody::STACK`, so nothing wider is needed.
#[inline(never)]
#[cold]
pub fn dag_blocker(bc: &[u8]) -> Option<u8> {
    if bc.is_empty() {
        return Some(BLK_EMPTY);
    }
    if bc.len() > MAX_STRAIGHT_BYTES {
        return Some(BLK_TOO_LONG);
    }

    // ── PASS 1 · LINEAR DECODE ──────────────────────────────────────────────────────────────
    // Records every instruction START and refuses the first opcode outside the whitelist.
    //
    // ⚠**THE START SET IS WHAT MAKES PASS 2 SOUND.** A branch target that is not a start would be
    // executed against a different instruction boundary than the one decoded here, and two
    // disagreeing decodings of the same bytes is exactly how a second interpreter runs a program
    // that is not the one in the class file. The body is at most 64 bytes, so the set is a `u64`.
    let mut starts: u64 = 0;
    let mut i = 0usize;
    while i < bc.len() {
        let op = bc[i];
        let len = match dag_step(op) {
            Some((l, _, _)) => l,
            None => return Some(op),
        };
        if i + len > bc.len() {
            return Some(BLK_RETURN); // operands truncated: not a complete body
        }
        // A WRITING form must name a slot inside the runner's scratch array, and a category-2
        // store must own the slot above it as well — that slot gets POISONED, and it can only be
        // poisoned if it exists.
        let write = match op {
            0x36..=0x3a | 0x84 => Some((bc[i + 1] as usize, op == 0x37 || op == 0x39)),
            0x3b..=0x4e => Some((
                ((op - 0x3b) % 4) as usize,
                (0x3f..=0x42).contains(&op) || (0x47..=0x4a).contains(&op),
            )),
            _ => None,
        };
        if let Some((slot, wide)) = write {
            if slot >= crate::flatbody::LOCALS || (wide && slot + 1 >= crate::flatbody::LOCALS) {
                return Some(BLK_SLOT);
            }
        }
        starts |= 1u64 << i;
        i += len;
    }

    // ── PASS 2 · OPERAND-DEPTH DATAFLOW ─────────────────────────────────────────────────────
    // The executor runs on ONE `sp`, so every path reaching an instruction must reach it at the
    // same depth. A linear depth walk cannot answer that and would reject the very shape this was
    // built for: `iconst_1; goto; iconst_0; ireturn` reads as depth 2 at the return unless the
    // `goto` is followed.
    const UNSEEN: i8 = -1;
    let mut depth = [UNSEEN; MAX_STRAIGHT_BYTES];
    // Each offset is ASSIGNED a depth at most once and only an assigning pop pushes successors, so
    // the whole run pushes at most `1 + 2 * 64` entries. An overflow refuses rather than truncating.
    let mut wpc = [0u8; 132];
    let mut wd = [0i8; 132];
    let mut n = 1usize;
    macro_rules! push {
        ($p:expr, $d:expr) => {{
            if n >= wpc.len() {
                return Some(BLK_DEPTH);
            }
            wpc[n] = $p as u8;
            wd[n] = $d as i8;
            n += 1;
        }};
    }
    while n > 0 {
        n -= 1;
        let (pc, d) = (wpc[n] as usize, wd[n]);
        if depth[pc] != UNSEEN {
            if depth[pc] != d {
                return Some(BLK_DEPTH); // a merge whose two paths disagree
            }
            continue;
        }
        depth[pc] = d;
        let (len, delta, flow) = match dag_step(bc[pc]) {
            Some(s) => s,
            None => return Some(bc[pc]),
        };
        if let Flow::Ret(req) = flow {
            if d as i32 != req {
                return Some(BLK_RETURN);
            }
            continue;
        }
        let nd = d as i32 + delta;
        if nd < 0 || nd > crate::flatbody::STACK as i32 {
            return Some(BLK_DEPTH);
        }
        if matches!(flow, Flow::Fall | Flow::Cond) {
            let next = pc + len;
            if next >= bc.len() {
                return Some(BLK_RETURN); // control falls off the end of the body
            }
            push!(next, nd);
        }
        if matches!(flow, Flow::Cond | Flow::Goto) {
            let off = i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as isize;
            let t = pc as isize + off;
            // ⭐**FORWARD ONLY — the one property the whole design rests on.** `t < pc` is a back
            // edge; `t == pc` is a self-loop, which is a back edge that happens to be short. Either
            // would turn a runner with no safepoint poll into an unbounded loop. The third test is
            // the one a reader skips: the target must be an instruction START, or the executor and
            // this walk would be decoding two different programs out of the same bytes.
            if t <= pc as isize || t as usize >= bc.len() || starts & (1u64 << t) == 0 {
                return Some(BLK_BACKEDGE);
            }
            push!(t as usize, nd);
        }
    }
    // Every path either hit a `Ret` or was refused, and offsets only ever increase — so the
    // executor terminates for the same reason this walk did.
    None
}

/// Default ON, opt-out `RAVA_NO_FLAT_DAG=1`. Read once and consulted at CLASSIFY time, so the
/// per-method hint a boot computes is stable for that boot.
pub fn dag_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FLAT_DAG").is_none())
}

/// The per-method answer, computed once and kept in the `Code` itself.
///
/// `0` unclassified · `1` neither · `2` a closed-form shape · `3` straight-line · `4` forward DAG.
/// A refusal — the common case by a factor of three — costs one relaxed byte load and one compare.
///
/// The store is `Relaxed` and unsynchronised on purpose: two threads racing to classify one body
/// compute the SAME value from the same immutable bytes, so the worst case is duplicated work and
/// never a wrong answer.
fn hint_of(code: &rava_classfile::Code, descriptor: &str) -> u8 {
    use std::sync::atomic::Ordering::Relaxed;
    let h = code.flat_hint.load(Relaxed);
    if h != 0 {
        return h;
    }
    // ⭐**A DISABLED SHAPE MUST NOT CLAIM THE BODY.** `Flat::FieldZeroTest` decodes exactly
    // `return field == 0;` — the single biggest named population in the boot — but it is OPT-IN
    // (`RAVA_FLAT_ZEROTEST`) because serving it through `getfield_value` measured **2.03% SLOWER**:
    // the read costs what the `Frame` costs. If it still classified as hint 2, `try_run` would
    // refuse at the disabled check and the body would never be offered to the DAG runner — which
    // exists to serve precisely this shape through a direct CELL LOAD instead. So a shape that
    // will not be served does not get to answer.
    let shape = classify(&code.bytecode);
    let shape_serves = match shape {
        Flat::No | Flat::Straight => false,
        Flat::FieldZeroTest(..) => !zerotest_disabled(),
        _ => true,
    };
    let computed = if shape_serves {
        2
    } else {
        match straight_blocker(&code.bytecode) {
            // ⚠**THE ARGUMENT-LAYOUT TEST BELONGS HERE, NOT PER CALL** — and putting it in the
            // wrong place cost 5.5% the first time. Widening the classifier (row 402) moved a
            // large population from "refused by hint" to "classified straight", and 47.4M calls a
            // gate then entered `run_straight` only to be turned away by the category-2 slot test
            // and run framed ANYWAY — a per-call test added in front of the path they already
            // took. A method's descriptor is as fixed as its bytecode, so the verdict is part of
            // the per-method answer: such a body is hint 1 and costs one byte load, exactly as it
            // did before it was admitted.
            None if straight_layout_ok(descriptor) => 3,
            None => 1,
            Some(b) => {
                // Kept for the census only; a refusal reads it, nothing else does. Stored BEFORE
                // the DAG attempt so `RAVA_STRAIGHT_BLOCKERS` keeps reporting what it always
                // reported — the opcode that kept the body off the STRAIGHT path — instead of
                // silently changing meaning when a wider path picks the body up.
                code.flat_blocker.store(b, Relaxed);
                // ⭐**THE DAG WIDENING.** A body the straight runner refused may still be a
                // forward-only DAG, which is the shape 88.5% of the boot's interpreted entries
                // have. Three preconditions, all per-method and all free at call time:
                //   * the layout test — `flatbody` indexes `args` by bytecode LOCAL SLOT, the same
                //     requirement the straight path has (see `straight_layout_ok`);
                //   * an EMPTY exception table — this runner has no handler machinery, and while
                //     every admitted opcode refuses rather than throwing, a body with a `catch` is
                //     one whose author expected a throw to be observable, so it stays framed;
                //   * the DAG walk itself.
                if dag_enabled()
                    && straight_layout_ok(descriptor)
                    && code.exceptions.is_empty()
                    && dag_blocker(&code.bytecode).is_none()
                {
                    4
                } else {
                    1
                }
            }
        }
    };
    code.flat_hint.store(computed, Relaxed);
    computed
}

/// May `flatbody` run a body with this descriptor?
///
/// ⭐**A category-2 parameter is no longer a refusal, and dropping that was the whole point.**
/// `flatbody` indexes `args` by bytecode LOCAL SLOT, and a `long`/`double` takes two slots but one
/// `args` entry — so the two numberings diverge. The old rule refused any such descriptor, and the
/// census showed exactly why that mattered: admitting numeric casts and float/double arithmetic
/// (row 402) unlocked **47.4M calls a gate that the layout test then threw straight back**, because
/// **a method doing double arithmetic takes double parameters.** The population the new opcodes
/// reach IS the population this test rejected. `slot_map` translates instead of refusing.
///
/// `boolean` stays excluded: those parameters are unboxed on the way into locals by the framed
/// path, which this one does not do.
fn straight_layout_ok(descriptor: &str) -> bool {
    matches!(crate::argcall::arg_layout(descriptor), Some(l) if l.unbox == 0)
}

/// Bytecode LOCAL SLOT → index into `args`, or `u8::MAX` for a slot no argument occupies (the
/// upper half of a category-2 pair, which valid bytecode never loads — a body that tries bails).
///
/// The receiver, when there is one, is slot 0 and `args[0]`; it is detected by comparing `args`
/// against the descriptor's parameter count rather than being passed in, so this cannot disagree
/// with the caller about staticness.
fn slot_map(l: &crate::argcall::ArgLayout, argc: usize) -> Option<[u8; crate::flatbody::SLOTS]> {
    let recv = argc.checked_sub(l.n as usize)?;
    if recv > 1 {
        return None; // not a shape this path models
    }
    let mut map = [u8::MAX; crate::flatbody::SLOTS];
    let mut slot = recv;
    if recv == 1 {
        map[0] = 0;
    }
    for i in 0..l.n as usize {
        if slot >= crate::flatbody::SLOTS {
            break; // deeper slots stay unmapped; a body that loads one refuses
        }
        map[slot] = (i + recv) as u8;
        slot += 1 + ((l.wide >> i) & 1) as usize;
    }
    Some(map)
}

/// `RAVA_FLAT_STAT` — served vs refused, BY REASON.
///
/// The rule this tree keeps relearning: instrument the REFUSAL before widening admission. A
/// `served` count on its own cannot tell "the population is small" from "the population is large
/// and one precondition rejects all of it" — which is exactly how the lambda PIC reached
/// `ADMITTED = 0` and the dispatch table sat at 17% coverage.
static SERVED: AtomicU64 = AtomicU64::new(0);
static REF_SHAPE: AtomicU64 = AtomicU64::new(0);
static REF_RECV: AtomicU64 = AtomicU64::new(0);
static SERVED_STRAIGHT: AtomicU64 = AtomicU64::new(0);
static REF_LAYOUT: AtomicU64 = AtomicU64::new(0);
static REF_BAILED: AtomicU64 = AtomicU64::new(0);
/// The DAG runner gets its OWN served/bailed pair rather than sharing the straight one. A bail
/// there means something different — most often "the receiver's field is not an arena cell" — and
/// merging the two would hide exactly the number that decides whether this path is worth keeping.
static SERVED_DAG: AtomicU64 = AtomicU64::new(0);
static REF_DAG_BAILED: AtomicU64 = AtomicU64::new(0);

/// `RAVA_STRAIGHT_BLOCKERS` — call-weighted histogram of the opcode that keeps a callee on the
/// framed path. Inert unless set: one cached bool on the refusal path.
static BLOCKERS: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];

pub fn blockers_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_STRAIGHT_BLOCKERS").is_some())
}

/// The blocker histogram, most frequent first. Empty when the census is off.
pub fn blockers_report() -> String {
    if !blockers_on() {
        return String::new();
    }
    let mut rows: Vec<(usize, u64)> = (0..256)
        .map(|i| (i, BLOCKERS[i].load(Relaxed)))
        .filter(|&(_, n)| n > 0)
        .collect();
    let total: u64 = rows.iter().map(|(_, n)| *n).sum();
    if total == 0 {
        return String::new();
    }
    rows.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    let mut out = format!(
        "[STRAIGHT-BLOCKERS] {total} refused interpreted calls, by the opcode that refused them
"
    );
    for (op, n) in rows.iter().take(24) {
        let pct = *n as f64 * 100.0 / total as f64;
        let name = match *op as u8 {
            BLK_EMPTY => "(structural: empty body)",
            BLK_DEPTH => "(structural: operand depth > 4)",
            BLK_RETURN => "(structural: no return / not last)",
            BLK_TOO_LONG => "(structural: body > 24 bytes)",
            BLK_BACKEDGE => "(structural: back edge / bad target)",
            BLK_SLOT => "(structural: local write out of range)",
            o => opcode_name(o),
        };
        out.push_str(&format!("  0x{op:02X} {name:<34} {n:>14} {pct:>6.2}%
"));
    }
    out
}

/// Names for the opcodes this census actually reports. Unknown codes print as a bare number —
/// a wrong name would be worse than none on a table meant to pick the next thing to implement.
fn opcode_name(op: u8) -> &'static str {
    match op {
        0x00 => "nop", 0x12 => "ldc", 0x13 => "ldc_w", 0x14 => "ldc2_w",
        0x36 => "istore", 0x37 => "lstore", 0x38 => "fstore", 0x39 => "dstore", 0x3a => "astore",
        0x3b..=0x3e => "istore_<n>", 0x3f..=0x42 => "lstore_<n>",
        0x43..=0x46 => "fstore_<n>", 0x47..=0x4a => "dstore_<n>", 0x4b..=0x4e => "astore_<n>",
        0x4f..=0x56 => "<t>astore",
        0x57 => "pop", 0x58 => "pop2", 0x59 => "dup", 0x5a => "dup_x1", 0x5b => "dup_x2",
        0x5c => "dup2", 0x5f => "swap",
        0x62 | 0x63 => "fadd/dadd", 0x66 | 0x67 => "fsub/dsub",
        0x6a | 0x6b => "fmul/dmul", 0x6c..=0x6f => "<t>div", 0x70..=0x73 => "<t>rem",
        0x76 | 0x77 => "fneg/dneg",
        0x84 => "iinc",
        0x85..=0x93 => "<t>2<t> (numeric cast)",
        0x94 => "lcmp", 0x95 | 0x96 => "fcmp<op>", 0x97 | 0x98 => "dcmp<op>",
        0x99..=0x9e => "if<cond>", 0x9f..=0xa4 => "if_icmp<cond>", 0xa5 | 0xa6 => "if_acmp<cond>",
        0xa7 => "goto", 0xa8 => "jsr", 0xa9 => "ret",
        0xaa => "tableswitch", 0xab => "lookupswitch",
        0xb2 => "getstatic", 0xb3 => "putstatic", 0xb5 => "putfield",
        0xb6 => "invokevirtual", 0xb7 => "invokespecial", 0xb8 => "invokestatic",
        0xb9 => "invokeinterface", 0xba => "invokedynamic",
        0xbb => "new", 0xbc => "newarray", 0xbd => "anewarray", 0xbe => "arraylength",
        0xbf => "athrow", 0xc0 => "checkcast", 0xc1 => "instanceof",
        0xc2 | 0xc3 => "monitorenter/exit", 0xc4 => "wide", 0xc5 => "multianewarray",
        0xc6 | 0xc7 => "ifnull/ifnonnull", 0xc8 => "goto_w",
        _ => "?",
    }
}

pub fn stats_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FLAT_STAT").is_some())
}

/// Default ON, opt-out `RAVA_NO_FLATCALL=1` — the flag an A/B compares, and the flag a
/// divergence is bisected with.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FLATCALL").is_none())
}

pub fn report() -> String {
    if !stats_on() {
        return String::new();
    }
    let (s, sh, r, ss, rl, rb) = (
        SERVED.load(Relaxed),
        REF_SHAPE.load(Relaxed),
        REF_RECV.load(Relaxed),
        SERVED_STRAIGHT.load(Relaxed),
        REF_LAYOUT.load(Relaxed),
        REF_BAILED.load(Relaxed),
    );
    let (sd, rd) = (SERVED_DAG.load(Relaxed), REF_DAG_BAILED.load(Relaxed));
    let tot = s + sh + r + ss + rl + rb + sd + rd;
    if tot == 0 {
        return String::new();
    }
    let pct = |v: u64| v as f64 * 100.0 / tot as f64;
    format!(
        "[FLAT-STAT] {tot} interpreted calls reached the frame-free path\n\
         [FLAT-STAT]   served: shape     {s:>12}  {:5.2}%\n\
         [FLAT-STAT]   served: straight  {ss:>12}  {:5.2}%\n\
         [FLAT-STAT]   served: dag       {sd:>12}  {:5.2}%\n\
         [FLAT-STAT]   refused: hint     {sh:>12}  {:5.2}%\n\
         [FLAT-STAT]   refused: layout   {rl:>12}  {:5.2}%\n\
         [FLAT-STAT]   refused: bailed   {rb:>12}  {:5.2}%\n\
         [FLAT-STAT]   refused: dag bail {rd:>12}  {:5.2}%\n\
         [FLAT-STAT]   refused: receiver {r:>12}  {:5.2}%",
        pct(s),
        pct(ss),
        pct(sd),
        pct(sh),
        pct(rl),
        pct(rb),
        pct(rd),
        pct(r)
    )
}

/// Run a trivial callee without building a frame.
///
/// `Some(v)` — served; `v` is what the caller must push (`None` for a void body).
/// `None`     — refused; the caller must take its ordinary framed path, which this function has
///              left completely undisturbed (no store, no allocation, no mutation of `args`).
/// Flat-served lambda bodies are compiled on first use only when explicitly enabled with
/// `RAVA_FLAT_TIER=1`; `RAVA_NO_FLAT_TIER=1` remains an overriding kill switch.
/// ⛔**OPT-IN (`RAVA_FLAT_ZEROTEST=1`), because it does not pay.** The shape is correct — 5/5
/// gates bit-exact, and the counted effect is exactly what it was built for: all three target
/// methods LEAVE the `[STUCK]` list, 5.9 M interpreted frames a boot that no longer exist. The
/// boot A/B, same binary, 4 rounds, CPU-s: with the shape OFF **38.65 median against 39.45**, so
/// serving those 5.9 M calls frame-free is **2.03% SLOWER** (arms overlap, so read it as *no win*
/// rather than a loss).
///
/// ⭐What that prices is the alternative, not the frame: `getfield_value` on a `Value::Ref`
/// receiver — resolve the field, walk the handle heap, clone the receiver `Value` — costs about
/// what the `Frame` it replaces costs. The estimate that justified building this was 5.9 M x
/// ~300 ns = 5% of the boot, taken from the interpreter region's AVERAGE cost per entry; these
/// entries are far below that average. **The average cost of a population is not the cost of its
/// cheapest members**, and that is the shape of the error, recorded where the next person will
/// find it before repeating it.
fn zerotest_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_FLAT_ZEROTEST").is_none())
}

pub(crate) fn lambda_compilation_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_FLAT_TIER").is_some()
            && std::env::var_os("RAVA_NO_FLAT_TIER").is_none()
    })
}

pub(crate) fn try_run(
    vm: &mut Vm,
    decl: &Arc<LoadedClass>,
    code: &rava_classfile::Code,
    descriptor: &str,
    args: &[Value],
) -> Option<Option<Value>> {
    if !enabled() {
        return None;
    }
    match hint_of(code, descriptor) {
        // Three quarters of all interpreted calls end here, on one byte load and one compare.
        1 => {
            if stats_on() {
                REF_SHAPE.fetch_add(1, Relaxed);
            }
            // ⚠**CALL-weighted, not METHOD-weighted, and that is the whole point.** The hint is
            // memoised per `Code`, so classifying counts each method once — but one blocker in a
            // method called ten million times is worth more than a hundred methods called once.
            if blockers_on() {
                BLOCKERS[code.flat_blocker.load(Relaxed) as usize].fetch_add(1, Relaxed);
            }
            return None;
        }
        3 => return run_flat::<false>(vm, decl, &code.bytecode, descriptor, args),
        4 => return run_flat::<true>(vm, decl, &code.bytecode, descriptor, args),
        _ => {}
    }
    let flat = classify(&code.bytecode);
    let v = match flat {
        Flat::No | Flat::Straight => unreachable!("both handled by the hint above"),
        Flat::Void => None,
        Flat::ConstNull => Some(Value::Null),
        Flat::ConstInt(i) => Some(Value::Int(i)),
        Flat::ConstLong(l) => Some(Value::Long(l)),
        Flat::ConstFloat(f) => Some(Value::Float(f)),
        Flat::ConstDouble(d) => Some(Value::Double(d)),
        Flat::Local0 => match args.first() {
            Some(v) => Some(v.clone()),
            // A body that loads slot 0 from a call that passed nothing is malformed. Refuse
            // rather than invent a value; the framed path will produce whatever the machine
            // produces today.
            None => {
                if stats_on() {
                    REF_RECV.fetch_add(1, Relaxed);
                }
                return None;
            }
        },
        Flat::FieldZeroTest(fidx, invert) => {
            // Opt out of just this shape (`RAVA_NO_FLAT_ZEROTEST=1`) so it can be priced on its
            // own; `RAVA_NO_FLATCALL` still turns the whole hook off.
            if zerotest_disabled() {
                return None;
            }
            let recv = match args.first() {
                Some(r) if !matches!(r, Value::Null) => r.clone(),
                // Null receiver: REFUSE, so the framed path raises the NullPointerException with
                // the frame this path would not have contributed to the trace. Same rule as
                // `Getter`, and for the same reason.
                _ => {
                    if stats_on() {
                        REF_RECV.fetch_add(1, Relaxed);
                    }
                    return None;
                }
            };
            // Reconstructed exactly as `Getter` does, and for the same borrow reason. `pc = 1` is
            // where the framed path's PC sits when it reads these two index bytes.
            let body = [0x2a, 0xb4, (fidx >> 8) as u8, fidx as u8, 0xb0];
            let v = match crate::opcodes::fields::getfield_value(vm, decl, fidx, recv, &body, 1) {
                Ok(v) => v,
                Err(_) => {
                    if stats_on() {
                        REF_RECV.fetch_add(1, Relaxed);
                    }
                    return None;
                }
            };
            // ⚠**ONLY A 32-BIT INTEGER FIELD.** The encoding above admits any field whose
            // `getfield` is followed by `ifne`/`ifeq`, and the verifier guarantees that is an
            // int-shaped value — but `getfield_value` can yield a boxed or mirror `Value` on this
            // engine's representations, and comparing one of those to zero here would be this
            // path inventing semantics. Anything else falls through to the frame, which computes
            // the same answer the way it always has.
            match v {
                Value::Int(n) => Some(Value::Int(((n == 0) != invert) as i32)),
                _ => {
                    if stats_on() {
                        REF_RECV.fetch_add(1, Relaxed);
                    }
                    return None;
                }
            }
        }
        Flat::Getter(fidx) => {
            // `args[0]` is the receiver: a staged instance call carries it in local slot 0, which
            // is exactly what this body's `aload_0` loads.
            let recv = match args.first() {
                Some(r) if !matches!(r, Value::Null) => r.clone(),
                // A null receiver, or a static body that somehow classified as a getter. REFUSE:
                // the framed path raises the NullPointerException, and does so with the frame this
                // path would not have contributed to the stack trace.
                _ => {
                    if stats_on() {
                        REF_RECV.fetch_add(1, Relaxed);
                    }
                    return None;
                }
            };
            // The body is reconstructed rather than borrowed: `getfield_value` reads the index
            // bytes back out of the bytecode it is handed, and borrowing `decl`'s own `Code` here
            // would hold an immutable borrow across the `&mut Vm` it also needs. `pc = 1` is the
            // offset of the `getfield` inside the five-byte body — where the framed path's PC sits
            // when it reads the same two bytes.
            let body = [0x2a, 0xb4, (fidx >> 8) as u8, fidx as u8, 0xb0];
            match crate::opcodes::fields::getfield_value(vm, decl, fidx, recv, &body, 1) {
                Ok(v) => Some(v),
                // Any refusal inside `getfield` (an unresolvable field, a mirror miss) falls back
                // rather than propagating: the read is pure, so redoing it under a frame cannot
                // double any effect, and it produces the error with the correct stack trace.
                Err(_) => {
                    if stats_on() {
                        REF_RECV.fetch_add(1, Relaxed);
                    }
                    return None;
                }
            }
        }
    };
    if stats_on() {
        SERVED.fetch_add(1, Relaxed);
    }
    Some(v)
}


/// The executor arm: check the ARGUMENT LAYOUT, then execute. `DAG` picks which instantiation of
/// `flatbody::exec` runs — straight-line, or the forward-DAG generalisation. Both need the same
/// slot map, and sharing the wrapper is what keeps the two from drifting on it.
///
/// ⚠**The layout check is not a detail.** `flatbody::run` indexes `args` with the bytecode LOCAL
/// SLOT number, and this hook runs BEFORE `expand_locals_into` — so `args` holds one entry per
/// ARGUMENT while the slots count a `long` or `double` twice. The two numberings agree exactly when
/// the descriptor has no category-2 parameter, which `arg_layout` already computes and memoises, so
/// the test is an integer compare on a value the call was going to look up anyway. `boolean` is
/// excluded for the same reason the framed path handles it separately: those parameters are unboxed
/// on the way into locals, which this path does not do.
fn run_flat<const DAG: bool>(
    vm: &mut Vm,
    decl: &Arc<LoadedClass>,
    bc: &[u8],
    descriptor: &str,
    args: &[Value],
) -> Option<Option<Value>> {
    let map = match crate::argcall::arg_layout(descriptor) {
        Some(l) if l.unbox == 0 => match slot_map(&l, args.len()) {
            Some(m) => m,
            None => {
                if stats_on() {
                    REF_LAYOUT.fetch_add(1, Relaxed);
                }
                return None;
            }
        },
        _ => {
            if stats_on() {
                REF_LAYOUT.fetch_add(1, Relaxed);
            }
            return None;
        }
    };
    let r = if DAG {
        crate::flatbody::run_dag(vm, decl, bc, args, &map)
    } else {
        crate::flatbody::run(vm, decl, bc, args, &map)
    };
    match r {
        Some(v) => {
            if stats_on() {
                if DAG {
                    SERVED_DAG.fetch_add(1, Relaxed);
                } else {
                    SERVED_STRAIGHT.fetch_add(1, Relaxed);
                }
            }
            Some(v)
        }
        // The executor gave up part way — a null array, an out-of-range index, an unexpected value
        // type, a receiver whose field does not live in an arena cell. Every opcode it ran was a
        // pure read and no local it wrote outlives it, so the framed path now runs the method from
        // the beginning with nothing performed twice.
        None => {
            if stats_on() {
                if DAG {
                    REF_DAG_BAILED.fetch_add(1, Relaxed);
                } else {
                    REF_BAILED.fetch_add(1, Relaxed);
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_two_shapes_and_refuses_everything_else() {
        assert!(matches!(classify(&[0x2a, 0xb4, 0x00, 0x07, 0xb0]), Flat::Getter(7)));
        assert!(matches!(classify(&[0x2a, 0xb4, 0x01, 0x00, 0xac]), Flat::Getter(256)));
        assert!(matches!(classify(&[0xb1]), Flat::Void));
        assert!(matches!(classify(&[0x03, 0xac]), Flat::ConstInt(0)));
        assert!(matches!(classify(&[0x02, 0xac]), Flat::ConstInt(-1)));
        assert!(matches!(classify(&[0x08, 0xac]), Flat::ConstInt(5)));
        assert!(matches!(classify(&[0x10, 0xff, 0xac]), Flat::ConstInt(-1)));
        assert!(matches!(classify(&[0x11, 0x00, 0xff, 0xac]), Flat::ConstInt(255)));
        assert!(matches!(classify(&[0x01, 0xb0]), Flat::ConstNull));
        assert!(matches!(classify(&[0x2a, 0xb0]), Flat::Local0));
        assert!(matches!(classify(&[0x26, 0xaf]), Flat::Local0));
        // A load and a return that disagree on type are not valid bytecode and must not be
        // served with a value of the wrong width.
        assert!(matches!(classify(&[0x2a, 0xac]), Flat::No));
        assert!(matches!(classify(&[0x1a, 0xb0]), Flat::No));
        // Slot 1 is NOT served: after a category-2 parameter its index no longer matches the
        // argument index this path indexes with.
        assert!(matches!(classify(&[0x2b, 0xb0]), Flat::No));
        // A getfield body that does not return immediately is not a getter.
        assert!(matches!(classify(&[0x2a, 0xb4, 0x00, 0x07, 0x60, 0xac]), Flat::No));
        // A SETTER is a store, so it is out of scope by construction rather than by omission.
        assert!(matches!(classify(&[0x2a, 0x1b, 0xb5, 0x00, 0x07, 0xb1]), Flat::No));
        // `this.p[i & 0xFF]` — the row-359 target. SMALL, not GETTER: the population this tracer
        // bullet deliberately does not serve yet.
        assert!(matches!(
            classify(&[0x2a, 0xb4, 0x00, 0x07, 0x1b, 0x11, 0x00, 0xff, 0x7e, 0x2e, 0xac]),
            Flat::No
        ));
    }

    /// ⭐**THE TWO SHAPES THE GENERALISATION WAS BUILT FOR**, byte for byte as javac emits them.
    /// They are the top of the `[STUCK]` census, and every one of them is refused by
    /// `straight_blocker` — so this test is simultaneously the admission proof and the statement
    /// of what the DAG path is FOR. If it ever stops passing, the path has stopped earning itself.
    #[test]
    fn the_stuck_census_shapes_are_admitted() {
        // `return size == 0;`
        //   0: aload_0  1: getfield #2  4: ifne 11  7: iconst_1  8: goto 12  11: iconst_0
        //  12: ireturn
        let is_empty =
            [0x2a, 0xb4, 0x00, 0x02, 0x9a, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac];
        assert!(straight_blocker(&is_empty).is_some(), "must be off the straight path");
        assert_eq!(dag_blocker(&is_empty), None);

        // fastutil `int size() { int n = end - start; return n >= 0 ? n : n + length; }`
        //   0: aload_0     1: getfield end    4: aload_0     5: getfield start
        //   8: isub        9: istore_1       10: iload_1    11: iflt 18
        //  14: iload_1    15: goto 24        18: aload_0    19: getfield length
        //  22: iload_1    23: iadd           24: ireturn
        let size = [
            0x2a, 0xb4, 0x00, 0x02, 0x2a, 0xb4, 0x00, 0x03, 0x64, 0x3c, 0x1b, 0x9b, 0x00, 0x07,
            0x1b, 0xa7, 0x00, 0x09, 0x2a, 0xb4, 0x00, 0x04, 0x1b, 0x60, 0xac,
        ];
        assert_eq!(size.len(), 25);
        assert!(straight_blocker(&size).is_some(), "must be off the straight path");
        assert_eq!(dag_blocker(&size), None);
    }

    /// A BACK EDGE is the one refusal the executor's termination argument rests on: it runs a
    /// `while` with no iteration bound and no safepoint poll, so a loop would not merely be slow.
    #[test]
    fn a_back_edge_is_refused() {
        // 0: iconst_0  1: goto -1  4: ireturn
        assert_eq!(dag_blocker(&[0x03, 0xa7, 0xff, 0xff, 0xac]), Some(BLK_BACKEDGE));
        // A self-loop is a back edge that happens to be zero long.
        assert_eq!(dag_blocker(&[0x03, 0xa7, 0x00, 0x00, 0xac]), Some(BLK_BACKEDGE));
    }

    /// A target that is not an instruction START would have the executor decode different
    /// instructions out of the same bytes than this walk did — the one way a "correct" second
    /// interpreter runs a program that is not in the class file.
    #[test]
    fn a_target_inside_an_instruction_is_refused() {
        // 0: iconst_0  1: ifeq +4 (-> 5, which is the MIDDLE of the sipush at 4)
        // 4: sipush 1  7: ireturn
        let bc = [0x03, 0x99, 0x00, 0x04, 0x11, 0x00, 0x01, 0xac];
        assert_eq!(dag_blocker(&bc), Some(BLK_BACKEDGE));
    }

    /// Two paths into one instruction must agree on the operand depth — the executor has one `sp`
    /// and cannot model both.
    #[test]
    fn a_merge_with_disagreeing_depth_is_refused() {
        //  0: iconst_0  1: iconst_0  2: ifeq 9  5: iconst_0  6: goto 9  9: ireturn
        // The fall-through reaches 9 with one operand, the goto with two.
        let bc = [0x03, 0x03, 0x99, 0x00, 0x07, 0x03, 0xa7, 0x00, 0x03, 0xac];
        assert_eq!(dag_blocker(&bc), Some(BLK_DEPTH));
    }

    /// Everything with an EFFECT stays framed: this runner may bail mid-body, and the framed path
    /// then re-runs the method from the beginning — so any store it had already performed would be
    /// performed twice.
    #[test]
    fn effects_and_calls_are_refused() {
        for (op, body) in [
            (0xb6u8, vec![0x2a, 0xb6, 0x00, 0x05, 0xac]), // invokevirtual
            (0xb8, vec![0xb8, 0x00, 0x05, 0xac]),         // invokestatic
            (0xb9, vec![0x2a, 0xb9, 0x00, 0x05, 0x01, 0x00, 0xac]), // invokeinterface
            (0xba, vec![0xba, 0x00, 0x05, 0x00, 0x00, 0xb0]), // invokedynamic
            (0xbb, vec![0xbb, 0x00, 0x05, 0xb0]),         // new
            (0xb5, vec![0x2a, 0x03, 0xb5, 0x00, 0x05, 0xb1]), // putfield
            (0xb3, vec![0x03, 0xb3, 0x00, 0x05, 0xb1]),   // putstatic
            (0x4f, vec![0x2a, 0x03, 0x03, 0x4f, 0xb1]),   // iastore
            (0xbf, vec![0x2a, 0xbf]),                     // athrow
            (0x12, vec![0x12, 0x05, 0xb0]),               // ldc — a String constant allocates
            (0xbe, vec![0x2a, 0xbe, 0xac]),               // arraylength
            (0xaa, vec![0x03, 0xaa, 0x00, 0x00, 0x00]),   // tableswitch — variable length
            (0xc4, vec![0xc4, 0x15, 0x00, 0x01, 0xac]),   // wide — variable length
        ] {
            assert_eq!(dag_blocker(&body), Some(op), "opcode {op:#04x} must be refused");
        }
    }

    /// A local WRITE must land inside the runner's scratch array, and a category-2 write must own
    /// the slot it poisons.
    #[test]
    fn out_of_range_local_writes_are_refused() {
        // istore 5 — past LOCALS.
        assert_eq!(dag_blocker(&[0x03, 0x36, 0x05, 0xb1]), Some(BLK_SLOT));
        // lstore_3 — slot 3 is in range, but the upper half of the pair is not.
        assert_eq!(dag_blocker(&[0x09, 0x42, 0xb1]), Some(BLK_SLOT));
        // istore_3 is fine: one slot, and it is the last one.
        assert_eq!(dag_blocker(&[0x03, 0x3e, 0x1d, 0xac]), None);
    }

    /// The straight set is a SUBSET of the DAG set, so nothing that used to be served can become
    /// unservable by the widening. (Both paths still have to agree on the layout test, which is
    /// applied outside both.)
    #[test]
    fn every_straight_body_is_also_a_dag() {
        for bc in [
            vec![0x2a, 0xb4, 0x00, 0x07, 0xac],
            vec![0x03, 0xac],
            vec![0xb1],
            vec![0x2a, 0xb4, 0x00, 0x07, 0x1b, 0x11, 0x00, 0xff, 0x7e, 0x2e, 0xac],
            vec![0x1a, 0x1b, 0x60, 0xac],
            vec![0x14, 0x00, 0x05, 0xad],
        ] {
            if straight_blocker(&bc).is_none() {
                assert_eq!(dag_blocker(&bc), None, "straight-admitted body refused by the DAG walk");
            }
        }
    }
}
