//! Loop-invariant guards leave their loop (C2's loop predication of invariant tests): a branch
//! inside a loop that compares two values defined outside it, and on one side only
//! deoptimizes (a failed assumption, such as an iterator's modification count), becomes one
//! `Inst::Guard` in the preheader that deoptimizes to the state on loop entry, and the loop
//! goes straight on. When the guard fails, the interpreter runs the loop with its tests; that
//! counts as a trap at the header, so the recompilation keeps the test in the loop.

use super::dom::Dom;
use super::ir::*;

pub fn hoist(g: &mut Graph) {
    let dom = Dom::new(g);
    let mut block_of = vec![usize::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            block_of[v as usize] = b;
        }
    }
    // Innermost loops first: a guard hoisted into an inner preheader may leave the outer loop
    // on its turn only if it was a branch there; it is not, so one level per guard.
    for l in dom.loops(g).iter().rev() {
        let h = l.header as usize;
        let Some(entry_state) = g.loop_entry.get(&l.header).cloned() else { continue };
        let outside_preds: Vec<B> = g.blocks[h].preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside_preds[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t as usize == h) {
            continue;
        }
        let outside = |v: V| g.constant(v).is_some() || block_of.get(v as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
        let cold = |b: B| b != 0 && g.blocks[b as usize].insts.is_empty() && matches!(g.blocks[b as usize].term, Term::Deopt(_));
        let mut hoisted: Vec<(usize, Cmp, Ty, V, V, B, B)> = Vec::new();
        for (bi, blk) in g.blocks.iter().enumerate() {
            if !l.body[bi] || bi == h {
                continue;
            }
            let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b, then, other, .. } = blk.term else { continue };
            let (a, b) = (g.resolve(a), g.resolve(b));
            if !outside(a) || !outside(b) {
                continue;
            }
            match (cold(then), cold(other)) {
                (true, false) if l.body[other as usize] => hoisted.push((bi, super::rce::negate(cmp), ty, a, b, other, then)),
                (false, true) if l.body[then as usize] => hoisted.push((bi, cmp, ty, a, b, then, other)),
                _ => {}
            }
        }
        if hoisted.is_empty() {
            continue;
        }
        let entry = g.blocks[h].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
        let Some(ps) = super::rce::entry_state_at(g, &entry_state, h, entry) else { continue };
        for (bi, cmp, ty, a, b, stay, exit) in hoisted {
            g.add(pre, Inst::Guard(cmp, ty, a, b, ps.clone()), Ty::I);
            g.blocks[bi].term = Term::Goto(stay);
            g.blocks[exit as usize].preds.retain(|&p| p as usize != bi);
        }
    }
}
