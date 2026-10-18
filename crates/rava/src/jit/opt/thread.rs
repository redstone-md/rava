//! Jump threading of branches on merged constants: an inlined boolean method (`equals`,
//! `isEmpty`, `contains`) returns 1 on one path and 0 on another, the paths merge into a phi,
//! and the caller branches on it. A predecessor that brings a constant to such a merge knows
//! where the branch goes, so it jumps there directly (C2 splits the merge the same way), and
//! the merge keeps only the predecessors whose value it does not know.
//!
//! The merge block may hold only the branched-on phi (read by nothing else, no deoptimization
//! state included), phis that stand for other values, and constants (never allocated). A
//! predecessor is redirected only when it ends in a jump to the merge and the target is a
//! later, non-header block, so the edge stays a forward one.

use super::ir::*;

pub fn thread(g: &mut Graph) {
    let reads = reads(g);
    for m in 0..g.blocks.len() as B {
        if let Some(p) = switch_shape(g, m, &reads) {
            thread_switch(g, m, p);
            continue;
        }
        let Some((p, cmp, ty, k, phi_left, then, other)) = shape(g, m) else { continue };
        // A phi read by more than the branch: its readers after the branch read a phi of the
        // target instead, which the redirected edges feed their constants.
        if reads.get(p as usize).is_none_or(|&r| r > 1) && !(any_known(g, m, p) && own_readers(g, m, p, [then, other])) {
            continue;
        }
        let mut i = 0;
        while i < g.blocks[m as usize].preds.len() {
            let pred = g.blocks[m as usize].preds[i];
            let Inst::Phi(inputs) = g.inst(p) else { unreachable!() };
            let input = g.resolve(inputs[i]);
            let known = g.constant(input);
            let decide = |c: i64| {
                let (x, y) = if phi_left { (c, k) } else { (k, c) };
                if super::canon::branch(cmp, ty, x, y) { then } else { other }
            };
            let target = known.map(decide);
            // A choice between two constants (an inlined boolean test, `ASCII.isDigit`): the
            // predecessor branches on the choice's own condition.
            let chosen = match (target, g.inst(input)) {
                (None, &Inst::Select(c2, ty2 @ (Ty::I | Ty::J | Ty::A), a2, b2, t, f)) => match (g.constant(g.resolve(t)), g.constant(g.resolve(f))) {
                    (Some(x), Some(y)) => Some((c2, ty2, a2, b2, decide(x), decide(y))),
                    _ => None,
                },
                _ => None,
            };
            match (target, chosen) {
                (Some(t), _) if redirect_ok(g, pred, m, t) => redirect(g, pred, i, m, t),
                (None, Some((_, _, _, _, tt, tf))) if tt == tf && redirect_ok(g, pred, m, tt) => redirect(g, pred, i, m, tt),
                (None, Some((c2, ty2, a2, b2, tt, tf))) if tt != tf && redirect_ok(g, pred, m, tt) && redirect_ok(g, pred, m, tf) => {
                    add_edge(g, pred, i, m, tt);
                    add_edge(g, pred, i, m, tf);
                    g.blocks[pred as usize].term = Term::If { cmp: c2, ty: ty2, a: a2, b: b2, then: tt, other: tf, unordered: false };
                    detach(g, i, m);
                }
                _ => i += 1,
            }
        }
        if g.blocks[m as usize].preds.is_empty() {
            let blk = &mut g.blocks[m as usize];
            let succ = std::mem::replace(&mut blk.term, Term::None).successors();
            blk.insts.clear();
            for s in succ {
                drop_pred(g, s, m);
            }
        }
    }
    super::phis::fold_trivial(g);
}

/// How many times each value is read (instructions, terminators and deoptimization states).
pub(super) fn reads(g: &Graph) -> Vec<u32> {
    let mut reads = vec![0u32; g.values.len()];
    let mut states = Vec::new();
    for blk in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for &v in &blk.insts {
            if g.resolve(v) == v {
                g.inst(v).inputs().into_iter().for_each(|x| reads[g.resolve(x) as usize] += 1);
                states.extend(g.inst(v).state().cloned());
            }
        }
        blk.term.inputs().into_iter().for_each(|x| reads[g.resolve(x) as usize] += 1);
        if let Term::Deopt(s) = &blk.term {
            states.push(s.clone());
        }
    }
    for s in &states {
        let mut xs = Vec::new();
        super::liveness::state_values(g, s, &mut xs);
        xs.into_iter().for_each(|x| reads[g.resolve(x) as usize] += 1);
    }
    reads
}

/// A merge `m` branching on its phi against a constant: (the phi, the comparison, its type,
/// the constant, whether the phi is the left operand, the two targets).
fn shape(g: &Graph, m: B) -> Option<(V, Cmp, Ty, i64, bool, B, B)> {
    let blk = &g.blocks[m as usize];
    let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b, then, other, .. } = blk.term else { return None };
    if blk.loop_header || g.loop_entry.contains_key(&m) || then == other || blk.preds.len() < 2 {
        return None;
    }
    let (a, b) = (g.resolve(a), g.resolve(b));
    let (p, k, phi_left) = match (g.constant(a), g.constant(b)) {
        (None, Some(k)) => (a, k, true),
        (Some(k), None) => (b, k, false),
        _ => return None,
    };
    let only_phi = |v: V| match g.inst(v) {
        Inst::Phi(_) => g.resolve(v) != v || v == p,
        Inst::Const(_) => true,
        _ => false,
    };
    let own = |v: V| blk.insts.contains(&v);
    (own(p) && matches!(g.inst(p), Inst::Phi(_)) && blk.insts.iter().all(|&v| only_phi(v))).then_some((p, cmp, ty, k, phi_left, then, other))
}

/// Whether a predecessor jumping to `m` brings a constant (or a choice of two) into phi `p`.
fn any_known(g: &Graph, m: B, p: V) -> bool {
    let Inst::Phi(inputs) = g.inst(p) else { return false };
    g.blocks[m as usize].preds.iter().zip(inputs).any(|(&pred, &x)| {
        let x = g.resolve(x);
        matches!(g.blocks[pred as usize].term, Term::Goto(t) if t == m) && (g.constant(x).is_some() || matches!(g.inst(x), Inst::Select(..)))
    })
}

/// Gives each target of `m`'s branch whose only predecessor is `m` a phi of `p`, read by what
/// read `p` in the blocks the target dominates (deoptimization states included); false, with
/// nothing changed, when something else reads `p`.
pub(super) fn own_readers(g: &mut Graph, m: B, p: V, targets: [B; 2]) -> bool {
    use super::split::{self, Use};
    let dom = super::dom::Dom::new(g);
    let index = dom.index(g.blocks.len());
    let under = |t: B, b: B| index[t as usize] != usize::MAX && index[b as usize] != usize::MAX && dom.dominates(index[t as usize], index[b as usize]);
    let mut single: Vec<B> = targets.iter().copied().filter(|&t| g.blocks[t as usize].preds == [m]).collect();
    single.dedup();
    let owner = |b: B| single.iter().position(|&t| under(t, b));
    let mut groups: Vec<Vec<Use>> = vec![Vec::new(); single.len()];
    for u in split::uses(g).swap_remove(p as usize) {
        // The branch, and the targets' phis on edges from `m` (redirected edges read them
        // through `m`'s phis, `add_edge`).
        // (Also what `m` itself computes for its branch alone: `fact_shape`'s field loads.)
        if matches!(u, Use::Term(b) | Use::Edge(b, _) | Use::Inst(b, _) if b == m) {
            continue;
        }
        let Some(t) = owner(u.block()) else {
            return false;
        };
        groups[t].push(u);
    }
    let mut xs = Vec::new();
    for (b, blk) in g.blocks.iter().enumerate() {
        if matches!(blk.term, Term::None) || owner(b as B).is_some() {
            continue;
        }
        let states = blk.insts.iter().filter_map(|&v| g.inst(v).state()).chain(match &blk.term {
            Term::Deopt(s) => Some(s),
            _ => None,
        });
        for s in states {
            xs.clear();
            super::liveness::state_values(g, s, &mut xs);
            if xs.iter().any(|&x| g.resolve(x) == p) {
                return false;
            }
        }
    }
    let mut def = vec![B::MAX; g.values.len() + single.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        blk.insts.iter().for_each(|&v| def[v as usize] = b as B);
    }
    let blocks: Vec<Vec<B>> = single.iter().map(|&t| (0..g.blocks.len() as B).filter(|&b| under(t, b)).collect()).collect();
    for (k, &t) in single.iter().enumerate() {
        let q = g.add(t, Inst::Phi(vec![p]), g.ty(p));
        let insts = &mut g.blocks[t as usize].insts;
        insts.pop();
        insts.insert(0, q);
        def[q as usize] = t;
        split::rename(g, &def, &groups[k], p, q);
        split::rename_states(g, &blocks[k], p, q);
    }
    true
}

/// A merge `m` switching on its phi (read by nothing else), holding only phis and constants.
fn switch_shape(g: &Graph, m: B, reads: &[u32]) -> Option<V> {
    let blk = &g.blocks[m as usize];
    let Term::Switch { key, .. } = blk.term else { return None };
    if blk.loop_header || g.loop_entry.contains_key(&m) || blk.preds.len() < 2 {
        return None;
    }
    let p = g.resolve(key);
    let only_phi = |v: V| match g.inst(v) {
        Inst::Phi(_) => g.resolve(v) != v || v == p,
        Inst::Const(_) => true,
        _ => false,
    };
    (blk.insts.contains(&p) && matches!(g.inst(p), Inst::Phi(_)) && reads[p as usize] == 1 && blk.insts.iter().all(|&v| only_phi(v))).then_some(p)
}

/// Sends each predecessor of `m` that brings a constant key straight to its case's block.
fn thread_switch(g: &mut Graph, m: B, p: V) {
    let mut i = 0;
    while i < g.blocks[m as usize].preds.len() {
        let pred = g.blocks[m as usize].preds[i];
        let Inst::Phi(inputs) = g.inst(p) else { unreachable!() };
        let target = g.constant(g.resolve(inputs[i])).map(|c| match &g.blocks[m as usize].term {
            Term::Switch { cases, default, .. } => cases.iter().find(|&&(k, _)| k as i64 == c as i32 as i64).map_or(*default, |&(_, b)| b),
            _ => unreachable!(),
        });
        match target {
            Some(t) if redirect_ok(g, pred, m, t) => redirect(g, pred, i, m, t),
            _ => i += 1,
        }
    }
    if g.blocks[m as usize].preds.is_empty() {
        let blk = &mut g.blocks[m as usize];
        let succ = std::mem::replace(&mut blk.term, Term::None).successors();
        blk.insts.clear();
        for s in succ {
            drop_pred(g, s, m);
        }
    }
}

/// Whether `pred` (jumping to `m`) may jump to `t` instead.
pub(super) fn redirect_ok(g: &Graph, pred: B, m: B, t: B) -> bool {
    let (pb, tb) = (&g.blocks[pred as usize], &g.blocks[t as usize]);
    matches!(pb.term, Term::Goto(x) if x == m) && !tb.loop_header && !g.loop_entry.contains_key(&t) && tb.order > pb.order && !tb.preds.contains(&pred)
}

/// Makes `pred` (the `i`th predecessor of `m`) jump to `t`: `t`'s phis take, for the new edge,
/// what they took from `m`, read through `m`'s phis as `pred` brought them.
fn redirect(g: &mut Graph, pred: B, i: usize, m: B, t: B) {
    redirect_with(g, pred, i, m, t, &Default::default());
}

/// `redirect`, where `extra` gives what `m`'s other values (field loads its branch reads) are
/// on the edge from `pred`.
pub(super) fn redirect_with(g: &mut Graph, pred: B, i: usize, m: B, t: B, extra: &std::collections::HashMap<V, V>) {
    add_edge_with(g, pred, i, m, t, extra);
    g.blocks[pred as usize].term = Term::Goto(t);
    detach(g, i, m);
}

/// Adds the edge `pred` -> `t` (`pred` the `i`th predecessor of `m`, which precedes `t`): `t`'s
/// phis take what they took from `m`, read through `m`'s phis as `pred` brought them.
fn add_edge(g: &mut Graph, pred: B, i: usize, m: B, t: B) {
    add_edge_with(g, pred, i, m, t, &Default::default())
}

fn add_edge_with(g: &mut Graph, pred: B, i: usize, m: B, t: B, extra: &std::collections::HashMap<V, V>) {
    let j = g.blocks[t as usize].preds.iter().position(|&x| x == m).expect("m precedes t");
    let m_insts = g.blocks[m as usize].insts.clone();
    let through = |g: &Graph, x: V| -> V {
        let r = g.resolve(x);
        match g.inst(r) {
            Inst::Phi(ins) if m_insts.contains(&r) => ins[i],
            _ => extra.get(&r).copied().unwrap_or(x),
        }
    };
    let t_phis: Vec<V> = g.blocks[t as usize].insts.iter().copied().filter(|&v| matches!(g.inst(v), Inst::Phi(_))).collect();
    for q in t_phis {
        let Inst::Phi(ins) = g.inst(q) else { unreachable!() };
        let x = through(g, ins[j]);
        if let Inst::Phi(ins) = &mut g.values[q as usize].inst {
            ins.push(x);
        }
    }
    g.blocks[t as usize].preds.push(pred);
}

/// Removes the `i`th predecessor of `m` (and the matching phi inputs).
fn detach(g: &mut Graph, i: usize, m: B) {
    let m_insts = g.blocks[m as usize].insts.clone();
    g.blocks[m as usize].preds.remove(i);
    for v in m_insts {
        if let Inst::Phi(ins) = &mut g.values[v as usize].inst {
            ins.remove(i);
        }
    }
}

/// Removes `m` from the predecessors of `s` (and the matching phi inputs).
pub(super) fn drop_pred(g: &mut Graph, s: B, m: B) {
    while let Some(j) = g.blocks[s as usize].preds.iter().position(|&x| x == m) {
        g.blocks[s as usize].preds.remove(j);
        for v in g.blocks[s as usize].insts.clone() {
            if let Inst::Phi(ins) = &mut g.values[v as usize].inst {
                ins.remove(j);
            }
        }
    }
}
