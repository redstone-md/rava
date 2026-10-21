//! Whether a value may share the register of a phi it flows into while that phi's interval is
//! still open: a loop's `s = s + x` computed straight into `s`'s register, with no move on the
//! back edge. Intervals are contiguous, so the phi's reaches past the loop to its last use
//! there; the two may still share when nothing reads the phi once the value is defined, up to
//! the phi's own block again (where the phi takes the value).

use super::ir::*;
use super::liveness::StateReads;
use super::regalloc::{phi_input, phis};

/// The block of each instruction.
pub fn blocks_of(g: &Graph) -> Vec<B> {
    let mut at = vec![B::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        if matches!(blk.term, Term::None) {
            continue;
        }
        for &v in &blk.insts {
            at[v as usize] = b as B;
        }
    }
    at
}

/// Whether nothing reads phi `p` on any path from `v`'s definition that does not come back
/// through `p`'s block first: no instruction, terminator or state there, nor a phi taking `p`
/// along an edge from there.
pub fn dead_after(g: &Graph, reads: &mut StateReads, block_of: &[B], p: V, v: V) -> bool {
    let (h, bv) = (block_of[p as usize], block_of[v as usize]);
    if h == B::MAX || bv == B::MAX || matches!(g.inst(v), Inst::Phi(_)) {
        return false;
    }
    let mut seen = vec![false; g.blocks.len()];
    // `v`'s own block from `v` on; again whole if a path inside the loop comes back to it.
    let mut work = vec![(bv, true)];
    while let Some((b, partial)) = work.pop() {
        let blk = &g.blocks[b as usize];
        let from = if partial { blk.insts.iter().position(|&x| x == v).map_or(0, |i| i + 1) } else { 0 };
        for &x in &blk.insts[from..] {
            if g.resolve(x) != x {
                continue;
            }
            let inst = g.inst(x);
            if !matches!(inst, Inst::Phi(_)) && inst.inputs().into_iter().any(|u| g.resolve(u) == p) {
                return false;
            }
            if inst.state().is_some_and(|s| reads.of(g, s).contains(&p)) {
                return false;
            }
        }
        if blk.term.inputs().into_iter().any(|u| g.resolve(u) == p) {
            return false;
        }
        if let Term::Deopt(s) = &blk.term {
            if reads.of(g, s).contains(&p) {
                return false;
            }
        }
        for s in blk.term.successors() {
            if phis(g, s).any(|ph| ph != p && phi_input(g, ph, b, s) == p) {
                return false;
            }
            if s != h && !seen[s as usize] {
                seen[s as usize] = true;
                work.push((s, false));
            }
        }
    }
    true
}

