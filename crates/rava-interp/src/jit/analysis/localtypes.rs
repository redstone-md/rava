//! **THE TYPE MAP DEOPTIMISATION NEEDS.**
//!
//! A JIT cell is a raw `i64`; an interpreter `Frame` holds typed `Value`s, and the interpreter is
//! not tolerant about it (`Value::as_int()` answers `None` for a `Long`). So rebuilding a frame at
//! a guard failure needs to know what every local IS at that bytecode index.
//!
//! The emitter cannot say: it learns a local's type from the OPCODE that touches it (`iload` →
//! `Int`, `aload` → `Ref`) and keeps no table. Class files carry the answer in `StackMapTable`,
//! which this tree's parser skips. So the types are recovered here, by walking the bytecode the
//! way the verifier would.
//!
//! ## What it computes
//!
//! For every bytecode index that begins an instruction, the type of every local slot — or
//! `Unknown`, when two paths reach that point with different types for the slot. **`Unknown` is
//! not a failure, it is a refusal**: a deopt site at such a pc simply is not emitted, and the
//! guard there keeps whatever shape it had. Being wrong is not an option (a mistyped local
//! silently corrupts a resumed frame), so anything not proven is refused.
//!
//! ## Control-flow convergence
//!
//! A forward sweep merges normal successors and conservative exception edges,
//! repeated until stable. Exceptional edges carry the PRE-instruction locals;
//! they do not carry the normal operand stack. JIT admission still refuses
//! handler-bearing methods until protected emission can reconstruct these states.
//!
//! ⭐**TWO FACTS THIS WALK WAS THROWING AWAY, AND WHAT EACH COST IN EMITTED CODE.**
//!
//! Every pc this map proves is a pc `emit_deopt_exit` may use. A pc it refuses forces the
//! guarded-inline site there back onto the DIAMOND shape — the fallback dispatch stays inside the
//! method, adding a merge and a block parameter per live reference, which is the shape that
//! measured +0.81% and +2.42% and is the whole reason the trap exists. So a refusal here is not
//! free: it is a merge, a phi and a longer live range in the emitted body.
//!
//!  1. **THE HIGH HALF OF A WIDE LOCAL WAS POISONED, AND NOTHING WRITES IT.** After `lstore i`
//!     the walk set slot `i+1` to `Unknown`, permanently (the lattice never climbs back), so
//!     `all_known` answered `false` at every instruction after the FIRST `long` or `double` store
//!     in the method — i.e. **no method with a wide local could ever emit a deopt exit.** The
//!     poison had no basis: `store_local` (support/locals.rs) is one `def_var` of `vars[i]`, and
//!     the interpreter's wide-store arm is one `loc_set!(i, v)` — **neither engine touches slot
//!     `i+1`**, so its type is exactly whatever the last real store left there. Dropping the
//!     poison is not a widening of what is claimed, it is deleting a claim that was never true.
//!  2. **SWITCH ARMS WERE NEVER WALKED.** `tableswitch`/`lookupswitch` reported NO successors, so
//!     a block reachable only through a switch was never `seen` — no deopt there — and, worse,
//!     its stores never merged into the blocks it reaches, so a downstream row could stand at
//!     `Known(Int)` while a switch path had stored a `Ref` in that slot. That is a resumed frame
//!     with a mistyped local, and it was the DEFAULT (`RAVA_DEOPT_NOSWITCH` was the opt-in
//!     refusal). The targets are now decoded once per method and published like any other edge.
//!
//! Kill switch **`RAVA_NO_FAST_TYPES=1`** (see `fast_types`) restores the poison and, rather than
//! restoring the mis-walk, REFUSES any method containing a switch — the safe half of the old
//! behaviour, so the bisection lever can never be the unsound direction.

use crate::jit::JT;
use rava_classfile::Code;

mod exception_edges;
#[cfg(test)]
mod tests;

/// Kill switch, DEFAULT ON, for the facts this round taught `localtypes` and `range` to prove:
/// the wide-half de-poisoning and the switch walk here, and the induction-variable widenings in
/// `range::iv_nonneg_monotonic`. `RAVA_NO_FAST_TYPES=1` turns all of them off together, and it
/// consults `emit_more_disabled` so the whole round prices as one block.
///
/// ⚠It does NOT gate the two things that are corrections rather than widenings — the
/// branch-target test on an induction variable's initialiser and the `invokedynamic`/`instanceof`
/// rejections in `field_loads_invariant`. Those remove FALSE facts; there is nothing to bisect,
/// and an off-switch for them would be an off-switch for correctness.
pub(crate) fn fast_types() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_TYPES").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// A local's type at a point, or a refusal.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum LT {
    Unset,
    Known(JT),
    Unknown,
}

impl LT {
    fn merge(self, other: LT) -> LT {
        match (self, other) {
            (LT::Unset, x) | (x, LT::Unset) => x,
            (LT::Known(a), LT::Known(b)) if a == b => LT::Known(a),
            _ => LT::Unknown,
        }
    }
}

/// Local types at each instruction start. `None` for an index that is not an instruction start.
///
/// ⭐**ONE TABLE, NOT ONE ALLOCATION PER INSTRUCTION.** This was
/// `Vec<Option<Vec<LT>>>`: a separate heap `Vec` for every pc the walk reached, ALLOCATED on
/// first reach and then CLONED again on every visit of every sweep (`at[pc].clone()` at the top
/// of the loop body). The allocation count grew with instructions × sweeps — a per-method cost on
/// the serial compile path that buys nothing, because the rows are all exactly `max_locals` wide
/// and never resize. It is now one flat row-major buffer plus a reached-bit per pc, which is the
/// same information with three allocations for the whole method.
///
/// TABLE SIZE: `(bytecode_len + 1) × max_locals` bytes of `types` (`LT` niche-packs into `JT`'s
/// spare tags, so one byte per slot), plus one byte per pc of `seen`. A 400-byte body with 8
/// locals is 3.6 KB; the largest bodies a boot compiles are a few tens of KB. It is a scratch
/// table walked in pc order at most four times and then read only at emitted deopt sites — not a
/// lookup structure on a hot path, so the L1 rule that killed the interner's 14 KB slot table
/// does not apply to it.
pub(crate) struct LocalTypes {
    /// Row-major `(bc.len()+1) × n`: slot types for pc `p` are `types[p*n .. p*n+n]`.
    types: Vec<LT>,
    /// Was a row ever published? (The `Option` discriminant of the old shape, unpacked.)
    seen: Vec<bool>,
    /// Slots per row = `code.max_locals`.
    n: usize,
}

impl LocalTypes {
    /// Types at `pc`, or `None` when the pc is not an instruction boundary.
    pub(crate) fn at(&self, pc: usize) -> Option<&[LT]> {
        if pc < self.seen.len() && self.seen[pc] {
            Some(&self.types[pc * self.n..pc * self.n + self.n])
        } else {
            None
        }
    }

    /// True when every slot below `n` is proven — the precondition for emitting a deopt here.
    pub(crate) fn all_known(&self, pc: usize, n: usize) -> bool {
        match self.at(pc) {
            Some(v) => v.iter().take(n).all(|t| !matches!(t, LT::Unknown)),
            None => false,
        }
    }
}

/// The incoming locals of a method: `this` (unless static) then the descriptor's parameters, each
/// occupying one or two slots exactly as the JVM lays them out.
fn entry_locals(descriptor: &str, is_static: bool, max_locals: usize) -> Vec<LT> {
    let mut v = vec![LT::Unset; max_locals];
    let mut i = 0usize;
    if !is_static {
        if i < v.len() {
            v[i] = LT::Known(JT::Ref);
        }
        i += 1;
    }
    let bytes = descriptor.as_bytes();
    let mut k = 1; // skip '('
    while k < bytes.len() && bytes[k] != b')' {
        let (jt, wide) = match bytes[k] {
            b'J' => (JT::Long, true),
            b'D' => (JT::Double, true),
            b'F' => (JT::Float, false),
            b'L' | b'[' => (JT::Ref, false),
            _ => (JT::Int, false),
        };
        // Skip the rest of a class name or array descriptor.
        match bytes[k] {
            b'L' => {
                while k < bytes.len() && bytes[k] != b';' {
                    k += 1;
                }
                k += 1;
            }
            b'[' => {
                while k < bytes.len() && bytes[k] == b'[' {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'L' {
                    while k < bytes.len() && bytes[k] != b';' {
                        k += 1;
                    }
                }
                k += 1;
            }
            _ => k += 1,
        }
        if i < v.len() {
            v[i] = LT::Known(jt);
        }
        i += if wide { 2 } else { 1 };
    }
    v
}

/// What a store opcode writes, if it is one: (slot, type, wide).
///
/// `wide` is retained ONLY for the `RAVA_NO_FAST_TYPES` arm, which restores the old poisoning of
/// slot+1. Nothing else reads it: a wide store writes ONE local slot in both engines (one
/// `def_var` in `store_local`, one `loc_set!` in the interpreter's `0x36..=0x3a` arm), so the
/// high half keeps whatever type it already had. See the module header.
fn store_of(bc: &[u8], pc: usize) -> Option<(usize, JT, bool)> {
    let op = bc[pc];
    let one = |t: JT, w: bool| -> Option<(usize, JT, bool)> {
        bc.get(pc + 1).map(|&s| (s as usize, t, w))
    };
    match op {
        0x36 => one(JT::Int, false),    // istore
        0x37 => one(JT::Long, true),    // lstore
        0x38 => one(JT::Float, false),  // fstore
        0x39 => one(JT::Double, true),  // dstore
        0x3a => one(JT::Ref, false),    // astore
        0x3b..=0x3e => Some(((op - 0x3b) as usize, JT::Int, false)),
        0x3f..=0x42 => Some(((op - 0x3f) as usize, JT::Long, true)),
        0x43..=0x46 => Some(((op - 0x43) as usize, JT::Float, false)),
        0x47..=0x4a => Some(((op - 0x47) as usize, JT::Double, true)),
        0x4b..=0x4e => Some(((op - 0x4b) as usize, JT::Ref, false)),
        0x84 => bc.get(pc + 1).map(|&s| (s as usize, JT::Int, false)), // iinc
        _ => None,
    }
}

/// Branch target of the instruction at `pc`, and whether control also falls through.
///
/// Every case here has AT MOST ONE target, so this returned a freshly heap-allocated one-element
/// `Vec` for every branch on every sweep. `Option` says the same thing with no allocation and no
/// drop glue. The many-target opcodes — `tableswitch`/`lookupswitch` — still answer `(None,
/// false)` HERE and are published from the `SwitchEdges` table the caller decoded once for the
/// whole method; that keeps this function allocation-free and keeps the switch decode off the
/// per-sweep path.
fn successors(bc: &[u8], pc: usize, len: usize) -> (Option<usize>, bool) {
    let op = bc[pc];
    let s16 = |o: usize| -> usize {
        let hi = bc.get(pc + o).copied().unwrap_or(0) as i16;
        let lo = bc.get(pc + o + 1).copied().unwrap_or(0) as i16;
        ((pc as i64) + (((hi << 8) | (lo & 0xff)) as i64)) as usize
    };
    match op {
        // if<cond>, if_icmp<cond>, if_acmp<cond>, ifnull, ifnonnull — branch or fall through
        0x99..=0xa6 | 0xc6 | 0xc7 => (Some(s16(1)), true),
        0xa7 => (Some(s16(1)), false), // goto
        0xac..=0xb1 => (None, false),  // returns
        0xbf => (None, false),         // athrow
        // tableswitch / lookupswitch: no single target and NO fall-through. The default and
        // every case target are published by the caller from `SwitchEdges` — see `compute`.
        0xaa | 0xab => (None, false),
        _ => (None, pc + len < bc.len()),
    }
}

/// Merge `v` into the row at `dst`, growing the lattice only.
///
/// `back` is "this edge goes to a pc at or before the one we are standing on" — the only kind of
/// edge that can make the sweep miss a change, which is what the fast convergence rule below
/// keys on. See `compute`.
#[inline]
fn publish(
    types: &mut [LT],
    seen: &mut [bool],
    n: usize,
    dst: usize,
    v: &[LT],
    changed: &mut bool,
    fast: bool,
    back: bool,
) {
    if dst >= seen.len() {
        return;
    }
    if seen[dst] {
        for (o, x) in types[dst * n..dst * n + n].iter_mut().zip(v.iter()) {
            let m = o.merge(*x);
            if m != *o {
                *o = m;
                if back || !fast {
                    *changed = true;
                }
            }
        }
    } else {
        types[dst * n..dst * n + n].copy_from_slice(v);
        seen[dst] = true;
        if back || !fast {
            *changed = true;
        }
    }
}

fn noswitch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DEOPT_NOSWITCH").is_some())
}

/// Every `tableswitch`/`lookupswitch` edge in the method, decoded ONCE.
///
/// A switch's target list is a constant of the bytecode, and `switch_targets` allocates a `Vec`
/// of `(match, target)` pairs to produce it — so decoding one inside the sweep would allocate
/// per switch per sweep, which is the exact cost this file already paid once to remove. Instead
/// pass 0 flattens all of them into one buffer: `idx` holds `(switch pc, start, end)` ascending,
/// `flat` holds default-then-cases. Empty — three words, no heap — for the overwhelming majority
/// of bodies, which contain no switch at all.
struct SwitchEdges {
    idx: Vec<(usize, u32, u32)>,
    flat: Vec<usize>,
}

impl SwitchEdges {
    /// Targets of the switch at `pc`. Only ever called when `bc[pc]` is already known to be
    /// `0xaa`/`0xab`, so the binary search runs at most once per switch per sweep.
    fn at(&self, pc: usize) -> &[usize] {
        match self.idx.binary_search_by_key(&pc, |e| e.0) {
            Ok(k) => {
                let (_, s, e) = self.idx[k];
                &self.flat[s as usize..e as usize]
            }
            Err(_) => &[],
        }
    }
}

/// Compute the local-type map for a method body. Returns `None` if the bytecode cannot be walked.
///
/// ⭐**THE SWEEP DECODED THE BODY ONCE PER SWEEP AND ALWAYS RAN ONE SWEEP TOO MANY.**
///
/// Three costs came off this, none of them changing a single entry of the map it returns:
///
///  * **Instruction lengths are a constant of the METHOD, and were recomputed per SWEEP.**
///    `insn_len_at` (a match plus, for switches, an aligned re-read of the pad and the case
///    count) ran on every instruction of every sweep. The linear pc sequence is identical in
///    every sweep — the walk always starts at 0 and steps by the decoded length — so it is
///    decoded ONCE into `lens`. The refusals it can produce (undecodable opcode, and the
///    `RAVA_DEOPT_NOSWITCH` switch bail) happen at exactly the same pc as before, because the
///    first sweep visited that same pc first.
///  * **A whole confirming sweep, for a body that never needed one.** The old rule was "sweep
///    again if ANY publish changed anything", and a forward publish always changes something the
///    FIRST time — so even a straight-line body with no branch at all paid two full sweeps to
///    learn that the second changed nothing. A sweep visits pcs in increasing order, so a
///    publish to `dst > pc` is consumed later in the SAME sweep; only an edge to `dst <= pc` (a
///    back edge, or a merge target already behind us) can leave a change unpropagated. If a
///    sweep made no such change, every row was final when its instruction was visited, so the
///    next sweep is provably a no-op. WAS: 2 sweeps minimum, growing with BYTECODE LENGTH each.
///    IS: 1 for any loop-free body, and one fewer for every other. A sweep that does change
///    something backwards still sets the flag in both rules, so the two agree sweep for sweep.
///    (The cap itself is now 8-and-refuse rather than 4-and-return — see the sweep loop.)
///  * **A `Vec` per row, per visit** — see `LocalTypes`; the row is now copied into one reused
///    scratch buffer.
///
/// `RAVA_NO_FAST_ANALYSIS=1` restores the original convergence rule (see `fast_analysis`), which
/// is the half worth bisecting; the table shape produces the same entries by construction.
pub(crate) fn compute(code: &Code, descriptor: &str, is_static: bool) -> Option<LocalTypes> {
    let bc = &code.bytecode;
    let n = code.max_locals as usize;
    let rows = bc.len() + 1;

    // Pass 0: the instruction-length table, plus every switch's edge list. `u32` because a
    // `tableswitch` is longer than 255 bytes. 0 marks a pc that is not an instruction start; the
    // sweep never reads those.
    //
    // ⭐**THE SWITCH ARMS ARE WALKED NOW** (module header, point 2). Treating a switch as having
    // no successors left every block reachable only through one unwalked, so its stores never
    // merged into what it reaches — a way for this map to claim `Known` where a real path stored
    // a different type, i.e. a mistyped resumed frame. Both refusal levers are still here:
    //
    //  * `RAVA_DEOPT_NOSWITCH=1` — refuse any method containing a switch (unchanged meaning; now
    //    a redundant belt, kept because it is the cheapest way to attribute a fault to this walk).
    //  * `RAVA_NO_FAST_TYPES=1` — the round's kill switch takes the SAME refusal, never the old
    //    mis-walk, so turning the round off cannot turn a soundness hole back on.
    let fast_types = fast_types();
    let no_switch = noswitch() || !fast_types;
    let mut lens: Vec<u32> = vec![0; rows];
    let mut sw = SwitchEdges { idx: Vec::new(), flat: Vec::new() };
    {
        let mut pc = 0usize;
        while pc < bc.len() {
            let len = crate::jit::insn_len_at(bc, pc)?;
            if len == 0 || pc.checked_add(len)? > bc.len() {
                return None;
            }
            if matches!(bc[pc], 0xaa | 0xab) {
                if no_switch {
                    return None;
                }
                // `switch_targets` returns ABSOLUTE pcs and `None` on a malformed payload; a
                // refusal here is the same refusal the whole method already takes on an
                // undecodable opcode.
                let (default_t, cases) = crate::jit::switch_targets(bc, pc)?;
                let start = sw.flat.len() as u32;
                sw.flat.push(default_t);
                for (_, t) in cases {
                    sw.flat.push(t);
                }
                sw.idx.push((pc, start, sw.flat.len() as u32));
            }
            lens[pc] = len as u32;
            pc += len;
        }
        // A target that is not an instruction start would be published into a row the sweep never
        // visits — the block still goes unwalked, which is the hole this is closing. `lens` is
        // complete only now, so the check has to come after the walk. Refuse instead of guessing.
        if sw.flat.iter().any(|&t| t >= bc.len() || lens[t] == 0) {
            return None;
        }
    }

    let exception_edges = exception_edges::ExceptionEdges::new(code, &lens)?;
    let mut types: Vec<LT> = vec![LT::Unset; rows * n];
    let mut seen: Vec<bool> = vec![false; rows];
    types[..n].copy_from_slice(&entry_locals(descriptor, is_static, n));
    seen[0] = true;

    let fast = super::fast_analysis();
    let mut cur: Vec<LT> = vec![LT::Unset; n]; // the one scratch row, reused by every visit

    // One step of the lattice per slot, so a bounded number of sweeps settles every loop.
    //
    // ⚠**AN EXHAUSTED SWEEP BUDGET IS A REFUSAL, NOT AN ANSWER.** The old loop ran the cap and
    // returned whatever it had. A map that has not reached its fixpoint can hold `Known(X)` in a
    // row that a not-yet-propagated path would have driven to `Unknown` — which is precisely a
    // mistyped local in a resumed frame, the one failure this file exists to prevent. The budget
    // is now 8 (a sweep is a linear walk over `lens` with no allocation, and every loop-free body
    // still leaves after ONE), and a body that still has not settled produces NO map at all: its
    // guarded-inline sites keep the diamond, which is what they had before deopt existed. Nesting
    // depth is what drives the count, and 8 covers deeper nesting than 4 did, so this is more
    // coverage AND a refusal instead of a guess.
    let mut settled = false;
    for _ in 0..8 {
        let mut changed = false;
        let mut pc = 0usize;
        while pc < bc.len() {
            let len = lens[pc] as usize;
            if seen[pc] {
                cur.copy_from_slice(&types[pc * n..pc * n + n]);
                for target in exception_edges.at(pc) {
                    publish(&mut types, &mut seen, n, target, &cur,
                        &mut changed, fast, target <= pc);
                }
                if let Some((slot, jt, wide)) = store_of(bc, pc) {
                    if slot < n {
                        cur[slot] = LT::Known(jt);
                    }
                    // ⭐**THE HIGH HALF IS NOT WRITTEN, SO IT IS NOT POISONED.** `lstore i` /
                    // `dstore i` emit ONE `def_var(vars[i], …)` and the interpreter runs ONE
                    // `loc_set!(i, v)`; slot `i+1` keeps the value AND the type it already had.
                    // Poisoning it was a permanent `Unknown` (the lattice never climbs back),
                    // which made `all_known` false for the rest of the method — every deopt exit
                    // after the first wide store in the body was refused, and its guarded-inline
                    // site kept the diamond with its merge and per-reference block parameter.
                    // Restored under `RAVA_NO_FAST_TYPES=1` so the change can be bisected.
                    if wide && !fast_types && slot + 1 < n {
                        cur[slot + 1] = LT::Unknown;
                    }
                }
                let (target, falls) = successors(bc, pc, len);
                if let Some(t) = target {
                    publish(&mut types, &mut seen, n, t, &cur, &mut changed, fast, t <= pc);
                }
                // Switch edges: the default and every case. A case target at or before `pc` is a
                // back edge for the fast convergence rule exactly as a `goto` target is.
                if matches!(bc[pc], 0xaa | 0xab) {
                    for &t in sw.at(pc) {
                        publish(&mut types, &mut seen, n, t, &cur, &mut changed, fast, t <= pc);
                    }
                }
                if falls {
                    // Fall-through is always forward (`len >= 1`), so it can never be the edge
                    // that hides a change from this sweep.
                    publish(&mut types, &mut seen, n, pc + len, &cur, &mut changed, fast, false);
                }
            }
            pc += len;
        }
        if !changed {
            settled = true;
            break;
        }
    }
    if !settled {
        return None;
    }
    Some(LocalTypes { types, seen, n })
}
