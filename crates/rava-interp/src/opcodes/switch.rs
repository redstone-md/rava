//! `tableswitch` / `lookupswitch` target selection, lifted out of the interpreter loop.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! ⭐**THE ONLY LINEAR SCAN LEFT IN THE BRANCH OPCODES, AND ITS INPUT IS THE GUEST'S.**
//!
//! `tableswitch` was already O(1) — `low`/`high` bound a dense offset array and the index is a
//! subtraction — so nothing below touches it beyond moving it here next to its sibling.
//!
//! `lookupswitch` was a scan from pair 0 forward, comparing every match value until one hit:
//! **was O(npairs) per execution, is O(log npairs) on a hit.** The quantity that grows is
//! `npairs` — the number of CASE LABELS the guest wrote — and it grows without any bound this
//! engine controls. It is not a theoretical growth either: javac lowers a `switch` on a `String`
//! into a `lookupswitch` over `hashCode()` with one pair PER CASE, which is the exact shape MC's
//! codec and registry decode paths run (field-name dispatch, dozens of labels), and it lowers a
//! sparse `int`/enum switch the same way. A 40-label switch cost an average of 20 pair reads per
//! execution; the search costs six.
//!
//! **NO NEW TABLE, NO NEW ALLOCATION, NO HASH.** This is the round-4/round-6 shape and
//! deliberately not the round-3/round-7 one: the search reads the SAME bytes out of the SAME
//! `&[u8]` the interpreter already holds in a register and has already touched this instruction.
//! There is no memo to probe, nothing to invalidate and nothing to size against L1 — the only
//! storage this file introduces is the two `i32` cursors of the search, in registers.
//!
//! ⚠**AND IT IS NOT ALLOWED TO BE CLEVER ABOUT SORTEDNESS.** JVMS §6.5 requires a
//! `lookupswitch`'s pairs sorted ascending by match value with no duplicates, which is what makes
//! the search legal — but that is a property of the CLASS FILE, and this interpreter is the thing
//! that would have to prove it. Proving it is itself O(npairs), which is the cost being removed,
//! and caching the proof per site is a hash probe per execution, which is the cost that has lost
//! twice in this tree. So the search is allowed to CONFIRM a hit and never to CONCLUDE a miss:
//! a hit returns immediately (with no duplicates there is at most one, so it is the same pair the
//! scan would have found), and a miss falls through into the original scan, which then decides.
//! For any table whatsoever — sorted, unsorted, duplicated, empty — the answer is bit-identical to
//! the code this replaces. Nothing is refused, because nothing is assumed.
//!
//! The price of that honesty is `log n` wasted probes on an execution that ends at `default`.
//! Break-even is a 30% hit rate (`h*20 + (1-h)*40` against `h*6 + (1-h)*46` at n=40); a switch
//! whose default arm takes more than two executions in three is not a shape javac emits a large
//! `lookupswitch` for — the String-switch lowering in particular reaches it only for a hash that
//! matches no case at all.

use crate::opcodes::*;

/// ⭐**FAST LDC/SWITCH** — `RAVA_NO_FAST_LDC=1` opts out, and the block-wide `RAVA_NO_EMIT_MORE`
/// disables it with everything else, so one same-binary boot A/B prices the whole block.
///
/// Read behind a `&&` whose left operand is the size threshold, so a small switch — the common
/// one — never performs this load at all. The run-loop's own header note is about `OnceLock`
/// reads on the path EVERY body pays; this one is on the path only a >8-label switch pays.
pub(crate) fn fast_ldc_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_LDC").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Below this many pairs the scan wins and the search is not attempted.
///
/// ⚠**THE THRESHOLD IS THE POINT, NOT A HEDGE.** This tree has measured a plain scan at 30.7 ns
/// against 34.1 for a memo, and two rounds were demoted to opt-in for putting a smarter structure
/// on a path whose input is two to four entries. At `npairs = 8` the scan averages four pair
/// reads and the search takes three of them plus the branch mispredicts of a data-dependent
/// descent — a wash — so the search is only taken where the ratio is real. At 40 labels it is
/// 20 against 6, at 100 it is 50 against 7.
const LOOKUP_LINEAR_MAX: i32 = 8;

/// Offset of a `lookupswitch`/`tableswitch`'s first table word: the opcode, then zero to three
/// pad bytes aligning to a 4-byte boundary measured from the START of the method's code array
/// (JVMS §6.5 — `pc` here is already that method-relative index).
#[inline(always)]
fn table_at(pc: usize) -> usize {
    let p = pc + 1;
    p + ((4 - (p % 4)) % 4)
}

/// `tableswitch` (0xaa): `default`, `low`, `high`, then `high - low + 1` jump offsets. Already
/// O(1) in the number of labels — the index IS the subscript — so this is a relocation, not a
/// change. Returns the branch offset relative to `pc`.
#[inline]
pub(crate) fn tableswitch_target(bc: &[u8], pc: usize, idx: i32) -> i32 {
    let p = table_at(pc);
    let default = read_i32(bc, p);
    let low = read_i32(bc, p + 4);
    let high = read_i32(bc, p + 8);
    if idx < low || idx > high {
        default
    } else {
        // `idx >= low` is established, and a real table's span is bounded by the code array, so
        // the difference is in range. Kept as the original wrote it.
        read_i32(bc, p + 12 + ((idx - low) as usize) * 4)
    }
}

/// `lookupswitch` (0xab): `default`, `npairs`, then `npairs` (match, offset) pairs sorted
/// ascending. **Was O(npairs) per execution, is O(log npairs) on a hit** — see the module note
/// for why a miss still walks. Returns the branch offset relative to `pc`.
pub(crate) fn lookupswitch_target(bc: &[u8], pc: usize, key: i32) -> i32 {
    let p = table_at(pc);
    let default = read_i32(bc, p);
    let npairs = read_i32(bc, p + 4);
    let base = p + 8;

    if npairs > LOOKUP_LINEAR_MAX && fast_ldc_enabled() {
        // Binary search over the pairs IN PLACE. Signed compare: JVMS orders the match values as
        // ints, and a negative case label (`case -1:`) is ordinary — an unsigned compare would
        // sort it above every positive one and walk past its own answer.
        let (mut lo, mut hi) = (0i32, npairs - 1);
        while lo <= hi {
            // `lo + (hi - lo) / 2`, not `(lo + hi) / 2`: npairs is bounded by the 64 KB code
            // array so the sum cannot overflow today, but the form that cannot is free.
            let mid = lo + ((hi - lo) >> 1);
            let off = base + (mid as usize) * 8;
            let m = read_i32(bc, off);
            if m == key {
                // JVMS forbids duplicate match values, so this IS the pair the scan below would
                // have stopped at. Confirmed hit — return without touching the rest of the table.
                return read_i32(bc, off + 4);
            }
            if m < key {
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        // MISS. The search cannot conclude one — that would require the sortedness it is not
        // permitted to assume — so fall through and let the scan decide, exactly as before.
    }

    // The original scan, unchanged in meaning. `.max(0)` where the old code wrote `npairs as
    // usize`: a malformed negative count used to sign-extend into a ~2^64 loop bound and panic on
    // the first out-of-range read; it now takes `default`. No valid class file reaches either.
    for i in 0..npairs.max(0) as usize {
        let off = base + i * 8;
        if read_i32(bc, off) == key {
            return read_i32(bc, off + 4);
        }
    }
    default
}
