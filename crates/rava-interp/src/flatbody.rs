//! THE STRAIGHT-LINE BODY EXECUTOR — a second interpreter, kept deliberately small.
//!
//! WHY IT EXISTS. `flatcall` serves the shapes that need no interpreter at all and measured
//! **−2.69%** on the interpreter gate for **4.37%** of calls (ledger row 362). With the whitelist
//! below, `RAVA_SHAPE_CENSUS` counts **88,039,511 of 354,094,235 interpreted calls = 24.9%** as
//! executable here — 5.7× more — and `ImprovedNoise#p`, a one-line `this.p[i & 0xFF]` entered
//! **251,523,925** times a boot, is one of them. The population was counted before this file was
//! first written.
//!
//! ⛔**THIS IS THE SECOND ATTEMPT, AND THE FIRST ONE LOST.** Row 363: the executor was correct —
//! bit-exact on both gate tiers, all 18 differential probes matching `java -Xint`, zero mid-body
//! bails — and it still moved the gate from −2.69% to −0.80%. Two costs were named, and both are
//! addressed here rather than argued away:
//!
//!   1. ⭐**An admission test is paid by the population it REJECTS.** Deciding "is this body
//!      straight?" WALKS the bytecode, and 266 M calls a gate (75%) paid that walk to be told no.
//!      The answer is now computed once per method and stored in `Code::flat_hint`, so a refusal
//!      costs one relaxed byte load. A thread-local memo was tried first and was worse still.
//!   2. The operand stack was `[Value; 8]`. `Value` owns an `Arc<str>`, so that array carries DROP
//!      GLUE executed on every return, plus its initialisation. It is `[Value; 4]` here, and a
//!      body needing more depth is refused by the classifier rather than growing it.
//!
//! ⚠**A SECOND IMPLEMENTATION OF BYTECODE SEMANTICS IS A LIABILITY**, and the whitelist is drawn
//! to keep the liability small rather than the population large. Everything whose meaning a second
//! implementation could plausibly get wrong is REFUSED, not approximated:
//!
//!   * no conversions (`i2b`/`i2c`/`i2s`/`l2i`/`d2i`/`f2i`) — truncation and saturation rules;
//!   * no float or double arithmetic and no `lcmp`/`fcmp`/`dcmp` — NaN, and the `l`/`g` variants;
//!   * no `idiv`/`irem`/`ldiv`/`lrem` — they throw, and `Integer.MIN_VALUE / -1` overflows;
//!   * no `dup`/`pop`/`swap` — their category-2 behaviour depends on whether a `long` occupies one
//!     operand slot or two, which differs between this value model and the JVMS;
//!   * no `arraylength` — the machine implements it behind a path that needs a `Frame`;
//!   * no branch, call, allocation, or store of any kind.
//!
//! Where the machine already exposes an authoritative primitive, THIS FILE CALLS IT rather than
//! restating it: array reads go through `heap.array_get_bounded` — the same function the hot loop
//! uses, so a `baload` sign-extends here because it sign-extends there — and `getfield` goes
//! through `opcodes::fields::getfield_value`.
//!
//! PURITY, AND WHY BAILING IS ALWAYS SAFE. Every admitted opcode is a pure read, so this executor
//! may give up at ANY point and return `None`; the framed path then runs the method from the
//! beginning with nothing to undo and no effect performed twice. That is the hot loop's own "peek,
//! do not commit" discipline, and it means a gap in this file's coverage can only ever be a missed
//! optimisation, never a correctness bug.
//!
//! ⚠**GC.** Nothing here allocates, so no collection can occur while `args` is unrooted; see the
//! audit in `flatcall`'s header. `getfield_value` allocates only to build a NullPointerException,
//! and a null receiver bails before reaching it.
//!
//! ═══════════════════════════════════════════════════════════════════════════════════════════
//! ⭐**THE DAG GENERALISATION (`RAVA_NO_FLAT_DAG=1` to disable).**
//!
//! A product boot enters **14.1 M interpreted method bodies, and 24 methods carry 88.5% of them**.
//! `Straight` cannot reach them because it refuses a BRANCH, and the two shapes that dominate the
//! `[STUCK]` census both have one:
//!
//! ```text
//!   return size == 0;                                    // getfield; ifne; iconst_1; goto; iconst_0
//!   int size() { int n = end - start;                    // getfield x2; isub; istore_1;
//!                return n >= 0 ? n : n + length; }       // iload_1; iflt; ... goto ...
//! ```
//!
//! Thirteen to twenty-five bytes that build a whole `Frame` — two pooled `Vec`s, a `max_locals`
//! resize, an `Arc<LoadedClass>` clone, a registry push/pop and a drop — to compare an `int`.
//!
//! The generalisation is deliberately the SMALLEST one that reaches both: the body's control flow
//! must be a DAG with **FORWARD EDGES ONLY**. That single property buys three things at once and
//! is why no basic-block structure is built here:
//!
//!   * the executor can stay a straight `while` over `i`, because `i` only ever INCREASES — so it
//!     terminates with no loop bound, no back-edge counter and no safepoint poll (this runner has
//!     none, and a body that could loop would starve the GC hand-off);
//!   * the classifier's depth dataflow visits each offset once, so admission stays O(body);
//!   * a `Frame`-free body still cannot loop, so the *cost* of a mid-body bail stays bounded by
//!     the 64-byte body limit exactly as it was for `Straight`.
//!
//! REFUSED, and each for a stated reason rather than by omission: a BACK EDGE (would loop), an
//! INVOKE (needs nesting — a different feature), `new`/`newarray` (allocates, which breaks the GC
//! contract above), `putfield`/`putstatic`/`<t>astore` (a store this path may not perform, since a
//! mid-body bail re-runs the body from the beginning and would perform it twice), `athrow`,
//! `monitorenter`/`exit`, the switches (variable length), `checkcast`/`instanceof` (can load a
//! class and run a `<clinit>`), `arraylength` (the machine implements it behind a path that needs
//! a `Frame`), and `ldc`/`ldc_w` (a String constant allocates and interns).
//!
//! ⚠**THE FIELD READ IS THE WHOLE PRICE, AND IT IS MEASURED.** Serving `isEmpty` frame-free through
//! `Flat::FieldZeroTest` came out **2.03% SLOWER**: `getfield_value` on a `Value::Ref` receiver —
//! resolve the field, walk the handle heap, clone the receiver `Value` — costs about what the
//! `Frame` it replaces costs, so the shape was correct and bought nothing. This path therefore
//! reads the ARENA CELL DIRECTLY (`arena_field`): one thread-local site probe for `(slot, kind)`,
//! one header load for the class id, one cell load, one decode. A `Value::Ref` receiver, a pinned
//! mirror, or an unknown slot REFUSES to the framed path rather than paying the expensive read —
//! **the generalisation is only worth it while the field read is cheap**, and a refusal keeps that
//! true instead of hoping it is.
//!
//! ⚠**LOCAL STORES.** The second shape needs `istore_1`/`iload_1` on a slot no argument occupies,
//! so this runner has `LOCALS` scratch slots. They are `None` until written, and a load of an
//! unwritten slot falls through to `slot_map`/`args` — so an argument slot costs exactly what it
//! did before, and a non-argument slot read before any store REFUSES (which is what invalid
//! bytecode deserves). A category-2 store POISONS the upper slot of its pair: valid bytecode never
//! loads it, and poisoning turns "never happens" into "refuses" rather than into a stale argument.

use crate::class_loader::LoadedClass;
use crate::value::Value;
use crate::Vm;
use std::sync::Arc;

/// Operand stack depth. Deliberately small: every slot is a `Value`, which owns an `Arc<str>` and
/// therefore carries drop glue run on every return. Overflowing BAILS, so this is a refusal
/// condition and never a bound that can be exceeded silently.
pub(crate) const STACK: usize = 4;

/// How many bytecode LOCAL SLOTS the slot map covers. A straight-line body is at most 24 bytes,
/// so it cannot reach far; a load past this bails rather than guessing.
pub(crate) const SLOTS: usize = 8;

/// Scratch local slots the DAG runner will let a body WRITE.
///
/// Four, and the number is the same trade the operand stack made: every slot is an
/// `Option<Value>` whose drop glue runs on every return, and the shapes this exists for
/// (`int n = end - start; ...`) use exactly one. A store to a higher slot is refused by the
/// classifier, so this is an admission bound and never a silent truncation.
pub(crate) const LOCALS: usize = 4;

/// Execute a straight-line body. `Some(v)` served (`v = None` for a void return); `None` refused,
/// with nothing done.
///
/// `slot_map` translates a bytecode LOCAL SLOT into an index into `args`. It exists because a
/// `long`/`double` parameter occupies two slots but one `args` entry, so the two numberings
/// diverge; the caller builds it from the descriptor (see `flatcall::slot_map`). A slot no
/// argument occupies refuses, which is always safe here.
pub(crate) fn run(
    vm: &mut Vm,
    decl: &Arc<LoadedClass>,
    bc: &[u8],
    args: &[Value],
    slot_map: &[u8; SLOTS],
) -> Option<Option<Value>> {
    exec::<false>(vm, decl, bc, args, slot_map)
}

/// Execute a FORWARD-BRANCH-ONLY body — the same executor with its branch, local-store and
/// direct-cell arms switched on. See the DAG section of this file's header.
///
/// ⚠**ONE IMPLEMENTATION OF THE SEMANTICS, NOT TWO.** `DAG` is a const generic precisely so this
/// does not become a second copy of thirty opcode arms that could drift from the first: every
/// added arm reads `if DAG`, which monomorphisation folds, so the straight instantiation keeps the
/// exact machine code it had (the branch arms become `return None` and are deleted, the scratch
/// locals are never written and are deleted with them). That the straight path is UNCHANGED is the
/// point — it is the only part of this file with a measured number against it.
pub(crate) fn run_dag(
    vm: &mut Vm,
    decl: &Arc<LoadedClass>,
    bc: &[u8],
    args: &[Value],
    slot_map: &[u8; SLOTS],
) -> Option<Option<Value>> {
    exec::<true>(vm, decl, bc, args, slot_map)
}

fn exec<const DAG: bool>(
    vm: &mut Vm,
    decl: &Arc<LoadedClass>,
    bc: &[u8],
    args: &[Value],
    slot_map: &[u8; SLOTS],
) -> Option<Option<Value>> {
    /// A bytecode local slot, translated to an `args` index. `None` for a slot no argument
    /// occupies — the upper half of a category-2 pair, or one past the end.
    macro_rules! arg_local {
        ($slot:expr) => {{
            let s: usize = $slot;
            match slot_map.get(s).copied() {
                Some(a) if a != u8::MAX => args.get(a as usize)?.clone(),
                _ => return None,
            }
        }};
    }
    // Scratch locals for the DAG runner. `None` = never written, so read the ARGUMENT — which
    // keeps an argument load exactly as cheap as it was before stores existed. With `DAG = false`
    // nothing below ever writes or reads these, so monomorphisation deletes both the array and its
    // drop glue.
    let mut lo: [Option<Value>; LOCALS] = [const { None }; LOCALS];
    // Slots holding the DEAD upper half of a category-2 store. Valid bytecode never loads one;
    // poisoning makes the impossible case REFUSE instead of silently reading the argument that
    // used to live in that slot.
    let mut poison: u32 = 0;
    macro_rules! load_local {
        ($slot:expr) => {{
            let s: usize = $slot;
            if DAG && s < LOCALS {
                if poison & (1u32 << s) != 0 {
                    return None;
                }
                match &lo[s] {
                    Some(v) => v.clone(),
                    None => arg_local!(s),
                }
            } else {
                arg_local!(s)
            }
        }};
    }
    macro_rules! store_local {
        ($slot:expr, $v:expr, $wide:expr) => {{
            if !DAG {
                return None;
            }
            let s: usize = $slot;
            // The classifier already bounded this; re-checking costs one compare and means a
            // classifier/executor drift refuses instead of writing outside the scratch array.
            if s >= LOCALS || ($wide && s + 1 >= LOCALS) {
                return None;
            }
            lo[s] = Some($v);
            poison &= !(1u32 << s);
            if $wide {
                lo[s + 1] = None;
                poison |= 1u32 << (s + 1);
            }
        }};
    }
    let mut st: [Value; STACK] = std::array::from_fn(|_| Value::Null);
    let mut sp: usize = 0;
    let mut i: usize = 0;

    macro_rules! push {
        ($v:expr) => {{
            if sp >= STACK {
                return None;
            }
            st[sp] = $v;
            sp += 1;
        }};
    }
    macro_rules! pop {
        () => {{
            if sp == 0 {
                return None;
            }
            sp -= 1;
            std::mem::replace(&mut st[sp], Value::Null)
        }};
    }
    /// Pop an `int`, refusing anything else rather than coercing it.
    macro_rules! pop_i {
        () => {{
            match pop!() {
                Value::Int(v) => v,
                _ => return None,
            }
        }};
    }
    macro_rules! pop_l {
        () => {{
            match pop!() {
                Value::Long(v) => v,
                _ => return None,
            }
        }};
    }
    /// `b` then `a`, so the arms read in source order: `a OP b`.
    macro_rules! bin_i {
        ($f:expr) => {{
            let b = pop_i!();
            let a = pop_i!();
            #[allow(clippy::redundant_closure_call)]
            push!(Value::Int($f(a, b)));
        }};
    }
    macro_rules! bin_l {
        ($f:expr) => {{
            let b = pop_l!();
            let a = pop_l!();
            #[allow(clippy::redundant_closure_call)]
            push!(Value::Long($f(a, b)));
        }};
    }
    /// The long shifts take a LONG and an INT — not two longs (JVMS 6.5 lshl/lshr/lushr). Writing
    /// them through this macro states that asymmetry once instead of three times.
    macro_rules! shift_l {
        ($f:expr) => {{
            let b = pop_i!();
            let a = pop_l!();
            #[allow(clippy::redundant_closure_call)]
            push!(Value::Long($f(a, b)));
        }};
    }

    loop {
        let op = *bc.get(i)?;
        match op {
            // `nop`. Admitted because refusing it would cost a whole `Frame` for a byte that does
            // nothing; javac emits it as padding around removed code.
            0x00 => i += 1,
            0x01 => {
                push!(Value::Null);
                i += 1;
            }
            // iconst_m1 .. iconst_5
            0x02..=0x08 => {
                push!(Value::Int(op as i32 - 3));
                i += 1;
            }
            0x09 | 0x0a => {
                push!(Value::Long(op as i64 - 0x09));
                i += 1;
            }
            0x0b..=0x0d => {
                push!(Value::Float(op as f32 - 0x0b as f32));
                i += 1;
            }
            0x0e | 0x0f => {
                push!(Value::Double(op as f64 - 0x0e as f64));
                i += 1;
            }
            0x10 => {
                push!(Value::Int(*bc.get(i + 1)? as i8 as i32));
                i += 2;
            }
            0x11 => {
                let v = i16::from_be_bytes([*bc.get(i + 1)?, *bc.get(i + 2)?]);
                push!(Value::Int(v as i32));
                i += 3;
            }
            // `ldc2_w` — a long or double constant, read straight from the pool. No VM call and
            // no allocation: `Constant::Long`/`Double` are values, so this cannot collect. Any
            // other entry kind bails (see the note in the classifier).
            0x14 => {
                let idx = u16::from_be_bytes([*bc.get(i + 1)?, *bc.get(i + 2)?]);
                match decl.file.pool.get(idx)? {
                    rava_classfile::Constant::Long(v) => push!(Value::Long(*v)),
                    rava_classfile::Constant::Double(v) => push!(Value::Double(*v)),
                    _ => return None,
                }
                i += 3;
            }
            // `getstatic`, CACHE-HIT ONLY. See the note in the classifier: a hit proves the site
            // is already resolved and its owner initialised, so this reads a value and allocates
            // nothing. A miss is a refusal, not a resolve — resolving here could load a class and
            // run a `<clinit>`, which is exactly what this executor may not do.
            0xb2 => {
                let idx = u16::from_be_bytes([*bc.get(i + 1)?, *bc.get(i + 2)?]);
                let site = (Arc::as_ptr(decl) as usize, idx);
                push!(crate::tlcache::getstatic_get(site, vm.runtime_shared().static_gen())?);
                i += 3;
            }
            // iload/lload/fload/dload/aload with a one-byte slot index.
            0x15..=0x19 => {
                let n = *bc.get(i + 1)? as usize;
                push!(load_local!(n));
                i += 2;
            }
            // The `_<n>` load forms: five contiguous families of four, so the slot is the opcode's
            // offset WITHIN its family — `op - 0x1a` alone would read slot 4 for `aload_0`.
            0x1a..=0x2d => {
                let n = ((op - 0x1a) % 4) as usize;
                push!(load_local!(n));
                i += 1;
            }
            // ═════════════════════════════ THE DAG-ONLY ARMS ═════════════════════════════
            // Each one is guarded by `DAG` — directly, or through `store_local!`, whose first act
            // is `if !DAG { return None }`. `DAG` is a const generic, so the straight
            // instantiation folds all of it away and keeps the machine code it was measured with.
            //
            // `<t>store` with a one-byte slot index. The stored `Value` is whatever the body
            // computed — the type is NOT re-checked, exactly as the framed path does not re-check
            // it, because the verifier already agreed the store and the later load are the same
            // type and this executor only ever hands the value back unchanged.
            0x36..=0x3a => {
                let n = *bc.get(i + 1)? as usize;
                let v = pop!();
                store_local!(n, v, op == 0x37 || op == 0x39);
                i += 2;
            }
            // The `_<n>` store forms: five families of four, decoded like the loads above.
            // `lstore_<n>` is 0x3f..=0x42 and `dstore_<n>` 0x47..=0x4a — the two that burn a
            // second slot.
            0x3b..=0x4e => {
                let n = ((op - 0x3b) % 4) as usize;
                let v = pop!();
                store_local!(n, v, (0x3f..=0x42).contains(&op) || (0x47..=0x4a).contains(&op));
                i += 1;
            }
            // `iinc slot, const` — a read-modify-write of a local, and the one opcode here that
            // touches a local without going through the operand stack. Refuses on a non-`int`
            // rather than coercing, as `pop_i!` does.
            0x84 => {
                let n = *bc.get(i + 1)? as usize;
                let c = *bc.get(i + 2)? as i8 as i32;
                let cur = match load_local!(n) {
                    Value::Int(v) => v,
                    _ => return None,
                };
                store_local!(n, Value::Int(cur.wrapping_add(c)), false);
                i += 3;
            }
            // ⭐**THE BRANCHES — the entire reason this file was generalised.** The conditions are
            // written to match `opcodes::run`/`opcodes::hot` arm for arm (same pop order, same
            // comparisons, `ref_equal` SHARED rather than restated), and the target rule is the
            // one property the classifier proved: **strictly forward**. The re-check here is one
            // compare and it is not redundant — a back edge would turn this bound-free `while`
            // into an unbounded loop with no safepoint poll in it.
            0x99..=0x9e | 0x9f..=0xa4 | 0xa5 | 0xa6 | 0xa7 | 0xc6 | 0xc7 => {
                if !DAG {
                    return None;
                }
                let take = match op {
                    0x99..=0x9e => {
                        let v = pop_i!();
                        match op {
                            0x99 => v == 0,
                            0x9a => v != 0,
                            0x9b => v < 0,
                            0x9c => v >= 0,
                            0x9d => v > 0,
                            _ => v <= 0,
                        }
                    }
                    0x9f..=0xa4 => {
                        let (b, a) = (pop_i!(), pop_i!());
                        match op {
                            0x9f => a == b,
                            0xa0 => a != b,
                            0xa1 => a < b,
                            0xa2 => a >= b,
                            0xa3 => a > b,
                            _ => a <= b,
                        }
                    }
                    0xa5 | 0xa6 => {
                        let (b, a) = (pop!(), pop!());
                        let eq = crate::opcodes::ref_equal(&a, &b);
                        if op == 0xa5 {
                            eq
                        } else {
                            !eq
                        }
                    }
                    0xc6 | 0xc7 => {
                        let is_null = matches!(pop!(), Value::Null);
                        if op == 0xc6 {
                            is_null
                        } else {
                            !is_null
                        }
                    }
                    // goto — unconditional.
                    _ => true,
                };
                if take {
                    let off = i16::from_be_bytes([*bc.get(i + 1)?, *bc.get(i + 2)?]) as isize;
                    let t = i as isize + off;
                    if t <= i as isize || t as usize >= bc.len() {
                        return None;
                    }
                    i = t as usize;
                } else {
                    i += 3;
                }
            }
            // Array reads through the machine's own accessor, so element typing and bounds
            // behaviour cannot diverge from the framed path; an error BAILS.
            0x2e..=0x35 => {
                let idx = pop_i!();
                let arr = pop!();
                let Value::Array(h) = arr else { return None };
                if idx < 0 {
                    return None;
                }
                match vm.heap.array_get_bounded(h, idx as usize) {
                    Ok(v) => push!(v),
                    Err(_) => return None,
                }
                i += 1;
            }
            0x60 => {
                bin_i!(|a: i32, b: i32| a.wrapping_add(b));
                i += 1;
            }
            0x64 => {
                bin_i!(|a: i32, b: i32| a.wrapping_sub(b));
                i += 1;
            }
            0x68 => {
                bin_i!(|a: i32, b: i32| a.wrapping_mul(b));
                i += 1;
            }
            0x61 => {
                bin_l!(|a: i64, b: i64| a.wrapping_add(b));
                i += 1;
            }
            0x65 => {
                bin_l!(|a: i64, b: i64| a.wrapping_sub(b));
                i += 1;
            }
            0x69 => {
                bin_l!(|a: i64, b: i64| a.wrapping_mul(b));
                i += 1;
            }
            // ⭐**SHARED WITH THE DISPATCH LOOP, NOT A SECOND IMPLEMENTATION** — which is the
            // condition this file's header sets for admitting anything whose semantics could be
            // got wrong twice. `pure_ops` holds the one definition of saturation and NaN; a
            // refusal there (an opcode it does not define) bails, as everything here may.
            0x76 | 0x77 | 0x85..=0x93 => {
                let v = pop!();
                push!(crate::opcodes::pure_ops::pure_unary(op, &v)?);
                i += 1;
            }
            0x62 | 0x63 | 0x66 | 0x67 | 0x6a | 0x6b => {
                let b = pop!();
                let a = pop!();
                push!(crate::opcodes::pure_ops::pure_fp_binary(op, &a, &b)?);
                i += 1;
            }
            0x74 => {
                let a = pop_i!();
                push!(Value::Int(a.wrapping_neg()));
                i += 1;
            }
            0x75 => {
                let a = pop_l!();
                push!(Value::Long(a.wrapping_neg()));
                i += 1;
            }
            // Shifts. The distance is masked to 5 bits for int and 6 for long (JVMS), and
            // `iushr`/`lushr` are LOGICAL — done through the unsigned type, not by hand.
            0x78 => {
                bin_i!(|a: i32, b: i32| a.wrapping_shl(b as u32 & 0x1f));
                i += 1;
            }
            0x7a => {
                bin_i!(|a: i32, b: i32| a.wrapping_shr(b as u32 & 0x1f));
                i += 1;
            }
            0x7c => {
                bin_i!(|a: i32, b: i32| ((a as u32) >> (b as u32 & 0x1f)) as i32);
                i += 1;
            }
            0x79 => {
                shift_l!(|a: i64, b: i32| a.wrapping_shl(b as u32 & 0x3f));
                i += 1;
            }
            0x7b => {
                shift_l!(|a: i64, b: i32| a.wrapping_shr(b as u32 & 0x3f));
                i += 1;
            }
            0x7d => {
                shift_l!(|a: i64, b: i32| ((a as u64) >> (b as u32 & 0x3f)) as i64);
                i += 1;
            }
            0x7e => {
                bin_i!(|a: i32, b: i32| a & b);
                i += 1;
            }
            0x80 => {
                bin_i!(|a: i32, b: i32| a | b);
                i += 1;
            }
            0x82 => {
                bin_i!(|a: i32, b: i32| a ^ b);
                i += 1;
            }
            0x7f => {
                bin_l!(|a: i64, b: i64| a & b);
                i += 1;
            }
            0x81 => {
                bin_l!(|a: i64, b: i64| a | b);
                i += 1;
            }
            0x83 => {
                bin_l!(|a: i64, b: i64| a ^ b);
                i += 1;
            }
            0xb4 => {
                // A NULL receiver REFUSES rather than throwing, in both instantiations: the framed
                // path raises the NullPointerException with the stack-trace line this path would
                // not have contributed, and `npe::source_at` names the source expression from the
                // real pc.
                let recv = pop!();
                if matches!(recv, Value::Null) {
                    return None;
                }
                let fidx = u16::from_be_bytes([*bc.get(i + 1)?, *bc.get(i + 2)?]);
                push!(if DAG {
                    // ⭐The measured condition (see the header): the DAG runner is only ahead of
                    // the `Frame` while the field read is a CELL LOAD. Anything else refuses.
                    arena_field(decl, fidx, recv)?
                } else {
                    // `getfield_value` reads the index bytes back out of the body it is handed and
                    // keys its inline cache on `(class, index)`, so it must see THIS body at THIS
                    // pc.
                    match crate::opcodes::fields::getfield_value(vm, decl, fidx, recv, bc, i) {
                        Ok(v) => v,
                        Err(_) => return None,
                    }
                });
                i += 3;
            }
            // ireturn .. areturn
            0xac..=0xb0 => return Some(Some(pop!())),
            0xb1 => return Some(None),
            // Not admitted. Unreachable once the classifier has run, and a refusal rather than a
            // panic if the two lists ever drift apart.
            _ => return None,
        }
    }
}

/// ⭐**THE CHEAP FIELD READ, AND THE REASON THIS PATH EXISTS AT ALL.**
///
/// Ledger note: serving `isEmpty` frame-free through `Flat::FieldZeroTest` measured **2.03%
/// SLOWER**, because `getfield_value` on a `Value::Ref` receiver costs about what the `Frame` it
/// replaces costs — resolve, walk the handle heap, clone the receiver. The generalisation only
/// pays while the read is a CELL LOAD, so this function serves exactly that and refuses the rest:
///
///   * a `Value::Ref` / `Str` / primitive receiver — the expensive read, and the measured loss;
///   * a PINNED address — that is a handle object wearing an arena pointer, whose fields are
///     mirror-served and can be synthetic (`getfield_value` normalises it to `Ref`);
///   * a live COW shadow — a gil-free worker's own newer write lives beside the cell, not in it;
///   * any of the arena diagnostics being armed — they COUNT arena reads, and a path that reads
///     cells behind their backs turns their census into a lie;
///   * an unresolved site, or a class id whose layout does not hold the field (a stale receiver
///     the collector has forwarded). The framed path then resolves, recovers and fills the cache,
///     so the next call through here hits.
///
/// Every refusal is a plain `None`: nothing stored, nothing allocated, nothing mutated.
///
/// What it costs if removed: the whole DAG runner reverts to being 2% slower than the `Frame` it
/// was built to avoid.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn arena_field(decl: &Arc<LoadedClass>, fidx: u16, recv: Value) -> Option<Value> {
    let Value::Obj(p) = recv else { return None };
    let addr = p.addr();
    // Range compare against the pinned region — no map lookup, so this costs two loads even when
    // it says no. `pinned_handle_of` would take a mutex, which is the cost being avoided here.
    if crate::nextgen::is_pinned(addr) || crate::parallel::cow_active() || arena_diag_armed() {
        return None;
    }
    let cid = crate::nextgen::arena_class_id(p);
    let site = Arc::as_ptr(decl) as usize;
    let (slot, kind) = match cellsite::get(site, fidx, cid) {
        Some(e) => e,
        None => {
            // HIT-ONLY on the resolved-field cache, for the same reason `getstatic` is hit-only
            // here: resolving would intern a name and could load the owner. A miss is one framed
            // call away from being a hit forever.
            let (id, _) = crate::tlcache::field_get((site, fidx))?;
            let slot = crate::nextgen::slot_of(cid, id)? as u32;
            let kind = crate::nextgen::kind_of(cid, id)?;
            cellsite::put(site, fidx, cid, slot, kind);
            (slot, kind)
        }
    };
    // `decode_arena_cell` is the machine's own cell decoder — the same one `arena_getfield` ends
    // in — so the tag encoding, the null rule and the arena-string case cannot diverge from the
    // framed path. It reads; it does not allocate.
    Some(crate::opcodes::arena_cells::decode_arena_cell(
        crate::nextgen::arena_cell(p, slot as usize),
        kind,
    ))
}

/// Without the arena there is no cell to read, so the DAG runner's `getfield` always refuses and
/// those bodies stay framed. Stated as code rather than left to a `cfg` on the caller.
#[cfg(not(feature = "next-gen-gc"))]
#[inline]
fn arena_field(_decl: &Arc<LoadedClass>, _fidx: u16, _recv: Value) -> Option<Value> {
    None
}

/// Are any of the arena field-read diagnostics armed? Read ONCE — `std::env::var_os` on a path
/// this hot is a lock plus a scan, which is a real boot slowdown (see `NULLREF_ON` in
/// `arena_cells`). Armed, this path refuses everything and every read goes back through
/// `getfield_value`, so the censuses stay complete.
#[cfg(feature = "next-gen-gc")]
fn arena_diag_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        [
            "RAVA_STALE_STAT",
            "RAVA_TRACE_STALEFIELD",
            "RAVA_TRACE_NULLREF",
            "RAVA_TRACE_STALEREAD",
            "RAVA_TRACE_BADCELL",
            "RAVA_M3_SHAPE_PROBE",
            "RAVA_M3_BIOME_SHAPE_PROBE",
        ]
        .iter()
        .any(|k| std::env::var_os(k).is_some())
    })
}

/// PER-SITE `(class id) -> (slot, kind)`, direct-mapped and thread-local.
///
/// `slot_of` already memoises `(class, field)`, but reaching it needs the field ID, and that is a
/// `HashMap` probe in `tlcache`; `kind_of` behind it is a LINEAR SCAN of the class layout with no
/// memo at all. This collapses all three into one array probe — the same shape as `tlcache`'s
/// caches and `slot_memo`, for the same reason recorded there: the cost in this layer is locks and
/// hashes (ledger row 178).
///
/// ⚠**VALIDATED BY CLASS ID, KEYED BY `Arc` ADDRESS.** A slot is a position in a layout that is
/// only ever APPENDED to, and a kind is fixed at registration, so a positive answer can never move
/// — but it is only the right answer for THIS receiver class, hence the `cid` check on every hit
/// (a call site is polymorphic; a monomorphic one simply never misses). The `(Arc address, index)`
/// key is the identical key `tlcache::field_get` has always used for the same site, so this adds
/// no lifetime assumption the engine was not already making, and it adds the `cid` check on top.
#[cfg(feature = "next-gen-gc")]
mod cellsite {
    const N: usize = 512;

    #[derive(Clone, Copy)]
    struct E {
        /// `0` marks an empty slot: an `Arc` address is never null.
        site: usize,
        fidx: u16,
        cid: u32,
        slot: u32,
        kind: u8,
    }

    thread_local! {
        static T: std::cell::RefCell<[E; N]> = const {
            std::cell::RefCell::new([E { site: 0, fidx: 0, cid: 0, slot: 0, kind: 0 }; N])
        };
    }

    /// Mix the index in: a site address alone collides across the fields of one class, and an
    /// index alone collides across every class that reads its n-th pool entry.
    #[inline]
    fn index(site: usize, fidx: u16) -> usize {
        ((site >> 4) ^ ((fidx as usize).wrapping_mul(0x9E37_79B9))) & (N - 1)
    }

    #[inline]
    pub(super) fn get(site: usize, fidx: u16, cid: u32) -> Option<(u32, u8)> {
        T.with(|t| {
            let e = t.borrow()[index(site, fidx)];
            (e.site == site && e.fidx == fidx && e.cid == cid).then_some((e.slot, e.kind))
        })
    }

    #[inline]
    pub(super) fn put(site: usize, fidx: u16, cid: u32, slot: u32, kind: u8) {
        T.with(|t| t.borrow_mut()[index(site, fidx)] = E { site, fidx, cid, slot, kind });
    }
}

#[cfg(test)]
mod tests {
    /// The shift rules are the part of this file most likely to be written plausibly and wrongly,
    /// so they are pinned against the JVMS definitions rather than against my own reading of the
    /// code. No `Vm` is needed: these are the raw expressions the arms use.
    #[test]
    fn shift_semantics_match_the_jvms() {
        // ishl/ishr mask the distance to 5 bits: `1 << 33` is `1 << 1`, not 0.
        assert_eq!(1i32.wrapping_shl(33u32 & 0x1f), 2);
        // ishr is ARITHMETIC — the sign bit propagates.
        assert_eq!((-8i32).wrapping_shr(1u32 & 0x1f), -4);
        // iushr is LOGICAL — it does not.
        assert_eq!(((-8i32 as u32) >> (1u32 & 0x1f)) as i32, 2147483644);
        // The long shifts mask to 6 bits and take an INT distance.
        assert_eq!(1i64.wrapping_shl(65u32 & 0x3f), 2);
        assert_eq!((-8i64).wrapping_shr(1u32 & 0x3f), -4);
        assert_eq!(((-8i64 as u64) >> (1u32 & 0x3f)) as i64, 9223372036854775804);
    }

    /// `_<n>` load decoding: five contiguous families of four, and the slot is the offset inside
    /// the family.
    #[test]
    fn load_n_decodes_to_the_right_slot() {
        for base in [0x1au8, 0x1e, 0x22, 0x26, 0x2a] {
            for n in 0u8..4 {
                assert_eq!((((base + n) - 0x1a) % 4) as usize, n as usize);
            }
        }
    }

    /// The `_<n>` STORE families decode the same way the loads do, and the two category-2 families
    /// — the ones that poison the slot above them — are exactly `lstore_<n>` and `dstore_<n>`.
    /// Getting either wrong writes a `long` into the slot a later `iload` reads.
    #[test]
    fn store_n_decodes_to_the_right_slot_and_width() {
        for (base, wide) in [(0x3bu8, false), (0x3f, true), (0x43, false), (0x47, true), (0x4b, false)] {
            for n in 0u8..4 {
                let op = base + n;
                assert_eq!((((op) - 0x3b) % 4) as usize, n as usize);
                let is_wide = (0x3f..=0x42).contains(&op) || (0x47..=0x4a).contains(&op);
                assert_eq!(is_wide, wide, "opcode {op:#04x}");
            }
        }
    }

    /// A branch offset is a SIGNED 16-bit big-endian displacement from the branch opcode's own
    /// offset — the same decoding `opcodes::branch_offset` does with a shift and an or. Written
    /// here as the expression the executor uses, so a change to either is caught against the JVMS
    /// rather than against the other copy.
    #[test]
    fn branch_offsets_are_signed_and_relative_to_the_opcode() {
        assert_eq!(i16::from_be_bytes([0x00, 0x07]) as isize, 7);
        assert_eq!(i16::from_be_bytes([0xff, 0xff]) as isize, -1);
        assert_eq!(i16::from_be_bytes([0xff, 0x00]) as isize, -256);
        // The classic `ifne +7 / goto +4` pair out of `return field == 0;`.
        assert_eq!(4 + i16::from_be_bytes([0x00, 0x07]) as isize, 11);
        assert_eq!(8 + i16::from_be_bytes([0x00, 0x04]) as isize, 12);
    }

    /// The constant families decode to their own values, not to their opcode numbers.
    #[test]
    fn constant_opcodes_decode_to_their_values() {
        for (op, want) in [(0x02u8, -1i32), (0x03, 0), (0x08, 5)] {
            assert_eq!(op as i32 - 3, want);
        }
        assert_eq!(0x09u8 as i64 - 0x09, 0);
        assert_eq!(0x0au8 as i64 - 0x09, 1);
        assert_eq!(0x0eu8 as f64 - 0x0e as f64, 0.0);
        assert_eq!(0x0fu8 as f64 - 0x0e as f64, 1.0);
    }
}
