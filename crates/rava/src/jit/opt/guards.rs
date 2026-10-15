//! Loop-invariant guards leave their loop (C2's loop predication of invariant tests): a branch
//! inside a loop that compares two values defined outside it, and on one side only
//! deoptimizes (a failed assumption, such as an iterator's modification count), becomes one
//! `Inst::Guard` in the preheader that deoptimizes to the state on loop entry, and the loop
//! goes straight on. When the guard fails, the interpreter runs the loop with its tests; that
//! counts as a trap at the header, so the recompilation keeps the test in the loop.

use super::dom::{Dom, Loop};
use super::ir::*;
use crate::types::BasicType;
use std::collections::HashSet;

pub fn hoist(g: &mut Graph) {
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    let nonnull = super::licm::nonnull_by_block(g, &dom);
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
        let outside_preds: Vec<B> = g.blocks[h].preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside_preds[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t as usize == h) {
            continue;
        }
        branches(g, l, pre, &block_of, &nonnull[index[pre as usize]]);
        block_of.resize(g.values.len(), usize::MAX);
        g.blocks[pre as usize].insts.iter().for_each(|&v| block_of[v as usize] = pre as usize);
        let Some(entry_state) = g.loop_entry.get(&l.header).cloned() else { continue };
        let outside = |v: V| g.constant(v).is_some() || block_of.get(v as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
        // Guards already made (an inner loop's predicates, null-check predicates) whose inputs
        // this loop does not change, in blocks that run on every iteration: one evaluation
        // before the loop does.
        let latches: Vec<usize> = g.blocks[h].preds.iter().filter(|&&p| l.body[p as usize]).map(|&p| index[p as usize]).collect();
        let mut predicates: Vec<(usize, V)> = Vec::new();
        if latches.iter().all(|&i| i != usize::MAX) {
            for (bi, blk) in g.blocks.iter().enumerate() {
                if !l.body[bi] || index[bi] == usize::MAX || !latches.iter().all(|&i| dom.dominates(index[bi], i)) {
                    continue;
                }
                for &v in &blk.insts {
                    let invariant = match *g.inst(v) {
                        Inst::Guard(_, _, a, b, _) => outside(g.resolve(a)) && outside(g.resolve(b)),
                        Inst::RangeGuard(arr, init, limit, _) => [arr, init, limit].into_iter().all(|x| outside(g.resolve(x))),
                        _ => false,
                    };
                    if invariant {
                        predicates.push((bi, v));
                    }
                }
            }
        }
        if predicates.is_empty() {
            continue;
        }
        let entry = g.blocks[h].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
        let Some(ps) = super::rce::entry_state_at(g, &entry_state, h, entry) else { continue };
        for (bi, v) in predicates {
            let moved = match g.inst(v).clone() {
                Inst::Guard(cmp, ty, a, b, _) => Inst::Guard(cmp, ty, a, b, ps.clone()),
                Inst::RangeGuard(arr, init, limit, _) => Inst::RangeGuard(arr, init, limit, ps.clone()),
                _ => unreachable!(),
            };
            g.blocks[bi].insts.retain(|&i| i != v);
            let v = g.add(pre, moved, Ty::I);
            block_of.resize(g.values.len(), usize::MAX);
            block_of[v as usize] = pre as usize;
        }
    }
}

/// Turns the invariant branches of loop `l` that deoptimize on one side into guards at the end
/// of its preheader `pre` (`block_of`: each value's block, `usize::MAX` for none; `nonnull`:
/// the values non-null there). Whether any moved.
pub(super) fn branches(g: &mut Graph, l: &Loop, pre: B, block_of: &[usize], nonnull: &HashSet<V>) -> bool {
    let h = l.header as usize;
    let Some(entry_state) = g.loop_entry.get(&l.header).cloned() else { return false };
    let outside = |v: V| g.constant(v).is_some() || block_of.get(v as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
    // The class word of an object defined outside the loop and non-null before it (a type
    // check of a loop-invariant receiver): loaded again in the preheader for the guard.
    let class_word = |v: V| match *g.inst(v) {
        Inst::LoadField(o, 0, BasicType::Int) => Some(g.resolve(o)).filter(|&o| outside(o) && nonnull.contains(&o)),
        _ => None,
    };
    let cold = |b: B| b != 0 && g.blocks[b as usize].insts.is_empty() && matches!(g.blocks[b as usize].term, Term::Deopt(_));
    // (block, test, type, operands, each operand's object whose class word to reload, stay, exit)
    let mut hoisted: Vec<(usize, Cmp, Ty, V, V, [Option<V>; 2], B, B)> = Vec::new();
    for (bi, blk) in g.blocks.iter().enumerate() {
        if !l.body[bi] || bi == h {
            continue;
        }
        let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b, then, other, .. } = blk.term else { continue };
        let (a, b) = (g.resolve(a), g.resolve(b));
        if !(outside(a) || class_word(a).is_some()) || !(outside(b) || class_word(b).is_some()) {
            continue;
        }
        let reload = [a, b].map(|x| if outside(x) { None } else { class_word(x) });
        match (cold(then), cold(other)) {
            (true, false) if l.body[other as usize] => hoisted.push((bi, super::rce::negate(cmp), ty, a, b, reload, other, then)),
            (false, true) if l.body[then as usize] => hoisted.push((bi, cmp, ty, a, b, reload, then, other)),
            _ => {}
        }
    }
    if hoisted.is_empty() {
        return false;
    }
    let entry = g.blocks[h].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
    let Some(ps) = super::rce::entry_state_at(g, &entry_state, h, entry) else { return false };
    for (bi, cmp, ty, a, b, reload, stay, exit) in hoisted {
        let mut operand = |x: V, o: Option<V>| o.map_or(x, |o| g.add(pre, Inst::LoadField(o, 0, BasicType::Int), Ty::J));
        let (a, b) = (operand(a, reload[0]), operand(b, reload[1]));
        g.add(pre, Inst::Guard(cmp, ty, a, b, ps.clone()), Ty::I);
        g.blocks[bi].term = Term::Goto(stay);
        g.blocks[exit as usize].preds.retain(|&p| p as usize != bi);
    }
    true
}

/// A guard that a dominating guard of the same test already passed goes (two loops' hoisted
/// checks of one receiver's class, landing in one preheader, or two inlined bodies'). Operands
/// are compared after numbering, constants by value.
pub fn dedupe(g: &mut Graph) {
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    let operand = |g: &Graph, x: V| -> (bool, i64) {
        let x = g.resolve(x);
        match g.constant(x) {
            Some(c) => (true, c),
            None => (false, x as i64),
        }
    };
    type Key = (Cmp, Ty, (bool, i64), (bool, i64));
    let mut seen: Vec<(B, usize, Key, V)> = Vec::new();
    let mut gone: Vec<(V, V)> = Vec::new();
    for &b in &dom.rpo {
        for (pos, &v) in g.blocks[b as usize].insts.iter().enumerate() {
            if g.resolve(v) != v {
                continue;
            }
            let Inst::Guard(cmp, ty, x, y, _) = *g.inst(v) else { continue };
            let key = (cmp, ty, operand(g, x), operand(g, y));
            let earlier = seen.iter().find(|&&(sb, sp, ref sk, _)| *sk == key && if sb == b { sp < pos } else { dom.dominates(index[sb as usize], index[b as usize]) });
            match earlier {
                Some(&(.., w)) => gone.push((v, w)),
                None => seen.push((b, pos, key, v)),
            }
        }
    }
    if gone.is_empty() {
        return;
    }
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    for &(v, w) in &gone {
        g.alias[v as usize] = w;
    }
    let gone: HashSet<V> = gone.into_iter().map(|(v, _)| v).collect();
    for blk in &mut g.blocks {
        blk.insts.retain(|v| !gone.contains(v));
    }
}
