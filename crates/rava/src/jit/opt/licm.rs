//! Loop-invariant code motion (C2's loop invariant hoisting): from each loop with a plain
//! preheader (its single outside predecessor, ending in a jump to the header), instructions
//! whose inputs are all defined outside the loop move to the end of the preheader, innermost
//! loops first so values can climb several levels:
//! - pure arithmetic (no division), conversions, compares, conditional moves and pure
//!   intrinsics, which may run once before the loop whether or not the loop would have;
//! - array lengths and field loads of objects known non-null at the preheader (a dominating null
//!   check, an allocation, or the receiver), which cannot fault there; a field load only when the
//!   loop makes no call, takes no monitor, has no fence or volatile read, and stores to no field
//!   at that offset.
//!
//! A null check inside the loop of a value defined outside it, in a block that runs on every
//! iteration, is predicated first (C2 peels such loops for the same effect): it becomes one
//! `Inst::Guard` in the preheader that deoptimizes to the loop's entry state, so the value is
//! known non-null there and its field loads can leave the loop. A failing guard counts as a trap
//! at the header, so the recompilation keeps the check in the loop.
//!
//! The climate tree's distance loop reloaded `node.parameterSpace` every iteration.

use super::dom::Dom;
use super::ir::*;
use std::collections::HashSet;

pub fn hoist(g: &mut Graph) {
    let dom = Dom::new(g);
    let n = g.blocks.len();
    let index = dom.index(n);
    let loops = dom.loops(g);
    // Values known non-null at the end of each block (by rpo position), through dominators.
    let nonnull = nonnull_by_block(g, &dom);
    // The block each instruction is in now: a value hoisted into an inner loop's preheader is
    // still inside the enclosing loop, invariant there only if hoisted again.
    let mut block_of = vec![usize::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            block_of[v as usize] = b;
        }
    }
    for l in loops.iter().rev() {
        let h = &g.blocks[l.header as usize];
        let outside: Vec<B> = h.preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t == l.header) || index[pre as usize] == usize::MAX {
            continue;
        }
        // The loop's memory effects.
        let mut barrier = false;
        let mut stored: HashSet<u32> = HashSet::new();
        for b in (0..n).filter(|&b| l.body[b]) {
            for &v in &g.blocks[b].insts {
                match g.inst(v) {
                    Inst::Call(..) | Inst::Monitor(..) | Inst::Fence => barrier = true,
                    Inst::Intrinsic(k, ..) if !k.is_pure() => barrier = true,
                    Inst::LoadField(..) if g.ordered.contains(&v) => barrier = true,
                    Inst::StoreField(_, off, ..) => {
                        stored.insert(*off);
                    }
                    _ => {}
                }
            }
        }
        let mut known = nonnull[index[pre as usize]].clone();
        predicate_null_checks(g, &dom, &index, l, pre, &block_of, &mut known);
        let known = &known;
        let order: Vec<B> = dom.rpo.iter().copied().filter(|&b| l.body[b as usize]).collect();
        let mut moved = Vec::new();
        for b in order {
            let insts = g.blocks[b as usize].insts.clone();
            let mut keep = Vec::with_capacity(insts.len());
            for v in insts {
                let invariant = |x: V| block_of.get(g.resolve(x) as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
                let inst = g.inst(v);
                let movable = g.resolve(v) == v
                    && inst.inputs().into_iter().all(invariant)
                    && match inst {
                        Inst::Bin(op, ..) => !matches!(op, BinOp::Div | BinOp::Rem) || matches!(g.ty(v), Ty::F | Ty::D),
                        Inst::Neg(_) | Inst::Convert(..) | Inst::Compare(..) | Inst::Select(..) => true,
                        Inst::Intrinsic(k, _, None) => k.is_pure(),
                        Inst::ArrayLength(o) => known.contains(&g.resolve(*o)),
                        Inst::LoadField(o, off, _) => !barrier && !stored.contains(off) && !g.ordered.contains(&v) && known.contains(&g.resolve(*o)),
                        _ => false,
                    };
                if movable {
                    block_of[v as usize] = pre as usize;
                    moved.push(v);
                } else {
                    keep.push(v);
                }
            }
            g.blocks[b as usize].insts = keep;
        }
        g.blocks[pre as usize].insts.extend(moved);
    }
}

/// For each block (by rpo position), the values non-null at its end: checked by a null check in
/// it or a dominator, allocated there, or the receiver.
fn nonnull_by_block(g: &Graph, dom: &Dom) -> Vec<HashSet<V>> {
    let mut out: Vec<HashSet<V>> = vec![HashSet::new(); dom.rpo.len()];
    for (i, &b) in dom.rpo.iter().enumerate() {
        let mut set = if i == 0 || dom.idom[i] == usize::MAX { HashSet::new() } else { out[dom.idom[i]].clone() };
        if i == 0 {
            set.extend(g.receiver);
        }
        for &v in &g.blocks[b as usize].insts {
            match g.inst(v) {
                Inst::NullCheck(x, _) => {
                    set.insert(g.resolve(*x));
                }
                Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) => {
                    set.insert(v);
                }
                _ => {}
            }
        }
        out[i] = set;
    }
    out
}

/// Moves the null checks of loop-invariant values that run on every iteration of `l` to its
/// preheader `pre` as guards (adding the values to `known`).
fn predicate_null_checks(g: &mut Graph, dom: &Dom, index: &[usize], l: &super::dom::Loop, pre: B, block_of: &[usize], known: &mut HashSet<V>) {
    let h = l.header as usize;
    let Some(entry_state) = g.loop_entry.get(&l.header).cloned() else { return };
    // Blocks that run on every iteration dominate every back edge's source.
    let latches: Vec<usize> = g.blocks[h].preds.iter().filter(|&&p| l.body[p as usize]).map(|&p| index[p as usize]).collect();
    if latches.iter().any(|&i| i == usize::MAX) {
        return;
    }
    let invariant = |x: V| block_of.get(x as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
    let mut checks = Vec::new();
    for b in (0..g.blocks.len()).filter(|&b| l.body[b] && index[b] != usize::MAX) {
        if !latches.iter().all(|&i| dom.dominates(index[b], i)) {
            continue;
        }
        for &v in &g.blocks[b].insts {
            if let Inst::NullCheck(x, _) = g.inst(v) {
                let x = g.resolve(*x);
                if invariant(x) && !known.contains(&x) {
                    checks.push((b, v, x));
                }
            }
        }
    }
    if checks.is_empty() {
        return;
    }
    let entry = g.blocks[h].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
    let Some(ps) = super::rce::entry_state_at(g, &entry_state, h, entry) else { return };
    let null = g.add(0, Inst::Const(0), Ty::A);
    for (b, v, x) in checks {
        if known.insert(x) {
            g.add(pre, Inst::Guard(Cmp::Ne, Ty::A, x, null, ps.clone()), Ty::I);
        }
        g.blocks[b].insts.retain(|&i| i != v);
    }
}
