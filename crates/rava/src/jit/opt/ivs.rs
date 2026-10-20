//! Non-negative induction variables (C2's range of a counted loop's phi): a loop header's int
//! phi entered with non-negative constants whose every value around the back edges is itself
//! plus 0 or 1, added where a dominating signed test keeps it below some int (so the addition
//! cannot wrap), is never negative. An iterator's cursor (`cursor = i + 1` after `i < size`)
//! then needs no lower-bound test before `elementData[i]` (`conds.rs`).


use super::dom::Dom;
use super::ir::*;
use std::collections::HashSet;

pub fn nonneg(g: &Graph) -> HashSet<V> {
    let mut out = HashSet::new();
    let dom = Dom::new(g);
    let loops = dom.loops(g);
    if loops.is_empty() {
        return out;
    }
    let index = dom.index(g.blocks.len());
    let mut def = vec![B::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            def[v as usize] = b as B;
        }
    }
    for l in &loops {
        let h = &g.blocks[l.header as usize];
        for &p in h.insts.iter().filter(|&&v| g.resolve(v) == v && g.ty(v) == Ty::I) {
            let Inst::Phi(ins) = g.inst(p) else { continue };
            let ok = h.preds.iter().zip(ins).all(|(&pred, &x)| {
                let x = through_trivial(g, x);
                if !l.body[pred as usize] {
                    return g.constant(x).is_some_and(|c| (0..=i32::MAX as i64).contains(&c));
                }
                let Inst::Bin(BinOp::Add, y, c) = *g.inst(x) else { return false };
                g.resolve(y) == p && g.constant(g.resolve(c)).is_some_and(|c| (0..=1).contains(&c)) && bounded_above(g, &dom, &index, def.get(x as usize).copied().unwrap_or(B::MAX), p, l.header)
            });
            if ok {
                out.insert(p);
            }
        }
    }
    out
}

/// `x`, or what a phi all of whose inputs are one value stands for.
fn through_trivial(g: &Graph, x: V) -> V {
    let x = g.resolve(x);
    match g.inst(x) {
        Inst::Phi(ins) if !ins.is_empty() && ins.iter().all(|&i| g.resolve(i) == g.resolve(ins[0])) => g.resolve(ins[0]),
        _ => x,
    }
}

/// Whether block `b` lies below a branch edge, between it and the loop header `h`, on which
/// `p < y` holds (signed, int).
fn bounded_above(g: &Graph, dom: &Dom, index: &[usize], b: B, p: V, h: B) -> bool {
    if b == B::MAX || index[b as usize] == usize::MAX {
        return false;
    }
    let mut cur = b;
    while cur != h {
        if let [q] = g.blocks[cur as usize].preds[..] {
            if let Term::If { cmp, ty: Ty::I, a, b: y, then, other, .. } = g.blocks[q as usize].term {
                if then != other {
                    let taken = if cur == then { cmp } else { super::rce::negate(cmp) };
                    let (a, y) = (g.resolve(a), g.resolve(y));
                    if a == p && taken == Cmp::Lt || y == p && taken == Cmp::Gt {
                        return true;
                    }
                }
            }
        }
        let i = dom.idom[index[cur as usize]];
        if i == usize::MAX {
            return false;
        }
        cur = dom.rpo[i];
    }
    false
}

