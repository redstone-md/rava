//! A loop header several back edges reach, one of them far hotter than the others, becomes two
//! headers (C2's `split_outer_loop`): the hot back edge gets an inner loop of its own. An
//! on-stack-replacement entry at an inner loop makes its header the target of the outer loop's
//! back edge too, so the inner loop's natural loop took in the outer loop's body (its calls,
//! allocations and stores) and nothing in it was invariant.
//!
//! The header keeps its predecessors but the hot back edge, with a phi per phi of its own for
//! what they bring; the new header gets that back edge and the header itself, with the original
//! phis (so every use of them stays valid: all of the header's code moves into the new header,
//! which therefore dominates everything the header did).

use super::ir::*;

/// How much hotter than all the other back edges together the split one must be.
const HOT_RATIO: f32 = 4.0;

pub fn split(g: &mut Graph) {
    let headers: Vec<B> = (0..g.blocks.len() as B).filter(|&h| g.blocks[h as usize].loop_header && !matches!(g.blocks[h as usize].term, Term::None)).collect();
    let mut renumbered = false;
    for h in headers {
        let order = g.blocks[h as usize].order;
        let back: Vec<usize> = g.blocks[h as usize].preds.iter().enumerate().filter(|&(_, &p)| g.blocks[p as usize].order >= order).map(|(i, _)| i).collect();
        if back.len() < 2 || g.blocks[h as usize].preds.len() == back.len() {
            continue;
        }
        let freq = |i: usize| g.blocks[g.blocks[h as usize].preds[i] as usize].freq;
        let &hot = back.iter().max_by(|&&a, &&b| freq(a).total_cmp(&freq(b))).unwrap();
        let rest: f32 = back.iter().filter(|&&i| i != hot).map(|&i| freq(i)).sum();
        if !(freq(hot) > HOT_RATIO * rest) {
            continue;
        }
        if !std::mem::replace(&mut renumbered, true) {
            for blk in g.blocks.iter_mut().filter(|b| b.order != u32::MAX) {
                blk.order *= 2;
            }
        }
        let latch = g.blocks[h as usize].preds[hot];
        if latch == h || g.blocks[h as usize].preds.iter().filter(|&&p| p == latch).count() > 1 {
            continue;
        }
        let inner = g.new_block(g.blocks[h as usize].bci);
        // The header's code and phis move to the inner header; the header keeps a phi of each
        // for its remaining predecessors.
        let insts = std::mem::take(&mut g.blocks[h as usize].insts);
        let mut outer_phis = Vec::new();
        let mut moved = Vec::with_capacity(insts.len());
        for v in insts {
            if let Inst::Phi(inputs) = &g.values[v as usize].inst {
                let mut rest = inputs.clone();
                let back_in = rest.remove(hot);
                let ty = g.values[v as usize].ty;
                g.values.push(Value { inst: Inst::Phi(rest), ty });
                let o = g.values.len() as V - 1;
                if !g.alias.is_empty() {
                    let n = g.alias.len() as V;
                    g.alias.extend(n..=o);
                }
                outer_phis.push(o);
                g.values[v as usize].inst = Inst::Phi(vec![o, back_in]);
            }
            moved.push(v);
        }
        let term = std::mem::replace(&mut g.blocks[h as usize].term, Term::Goto(inner));
        for s in term.successors() {
            for p in g.blocks[s as usize].preds.iter_mut().filter(|p| **p == h) {
                *p = inner;
            }
        }
        g.blocks[h as usize].preds.remove(hot);
        g.blocks[h as usize].insts = outer_phis;
        retarget(&mut g.blocks[latch as usize].term, h, inner);
        let (order, f) = (g.blocks[h as usize].order + 1, g.blocks[h as usize].freq);
        let blk = &mut g.blocks[inner as usize];
        (blk.insts, blk.term, blk.preds, blk.loop_header, blk.order, blk.freq) = (moved, term, vec![h, latch], true, order, f);
        // The state the loop's entry deoptimizes to names the phis, now the inner header's.
        if let Some(s) = g.loop_entry.remove(&h) {
            g.loop_entry.insert(inner, s);
        }
    }
}

/// Points `term`'s edges to `from` at `to`.
fn retarget(term: &mut Term, from: B, to: B) {
    match term {
        Term::Goto(t) if *t == from => *t = to,
        Term::If { then, other, .. } => [then, other].into_iter().filter(|t| **t == from).for_each(|t| *t = to),
        Term::Switch { cases, default, .. } => cases.iter_mut().map(|c| &mut c.1).chain(std::iter::once(default)).filter(|t| **t == from).for_each(|t| *t = to),
        _ => {}
    }
}
