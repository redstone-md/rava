//! Sum loops (C2's SuperWord reductions): a counted loop whose one body block only adds the
//! counter's element of an array to an accumulator, `for (i = from; i < to; i++) s += a[i]`
//! (int elements into an int or a long, long elements into a long), becomes one call of a
//! vectorized sum of the elements from the counter's value to the limit (`leaf.rs`), after
//! which the counter takes the limit, so the loop's test ends it.
//!
//! As for fills (`fill.rs`), the body holds no bounds check, so the elements are all the
//! array's, and the array is known non-null.

use super::dom::Dom;
use super::ir::*;
use crate::jit::intrinsics::Intrinsic;
use crate::jit::leaf::{Leaf, SumKind};
use crate::types::BasicType;

pub fn convert(g: &mut Graph) {
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    let dom = Dom::new(g);
    let mut def = vec![B::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            def[v as usize] = b as B;
        }
    }
    let found: Vec<Sum> = dom.loops(g).iter().filter_map(|l| candidate(g, l, &def)).collect();
    for s in found {
        g.values[s.add as usize].inst = Inst::Intrinsic(Intrinsic::Leaf(Leaf::Sum(s.kind)), vec![s.arr, s.i, s.limit, s.acc], None);
        // The counter's next value is the limit: the header's test leaves the loop.
        g.alias[s.next as usize] = s.limit;
        let gone = [s.next, s.load, s.widen.unwrap_or(s.next)];
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}

struct Sum {
    /// The accumulator's addition, which becomes the call.
    add: V,
    load: V,
    /// The element's widening to the accumulator's type, if any.
    widen: Option<V>,
    arr: V,
    i: V,
    limit: V,
    /// The accumulator's phi.
    acc: V,
    kind: SumKind,
    /// The counter's increment.
    next: V,
}

fn candidate(g: &Graph, l: &super::dom::Loop, def: &[B]) -> Option<Sum> {
    let h = l.header;
    let hb = &g.blocks[h as usize];
    let blocks: Vec<B> = (0..g.blocks.len() as B).filter(|&b| l.body[b as usize]).collect();
    let [_, _] = blocks[..] else { return None };
    let body = *blocks.iter().find(|&&b| b != h)?;
    let bb = &g.blocks[body as usize];
    if bb.preds[..] != [h] || !matches!(bb.term, Term::Goto(t) if t == h) || hb.preds.len() != 2 || !hb.preds.contains(&body) {
        return None;
    }
    let Term::If { cmp, ty: Ty::I, a, b, then, other, .. } = hb.term else { return None };
    let stay = if then == body { cmp } else if other == body { super::rce::negate(cmp) } else { return None };
    let (a, b) = (g.resolve(a), g.resolve(b));
    let (i, limit) = match stay {
        Cmp::Lt => (a, b),
        Cmp::Gt => (b, a),
        _ => return None,
    };
    let defined_outside = |v: V| def.get(v as usize).is_some_and(|&d| d != B::MAX && !l.body[d as usize]);
    if !defined_outside(limit) {
        return None;
    }
    let back = hb.preds.iter().position(|&p| p == body)?;
    let incoming = |phi: V| match g.inst(phi) {
        Inst::Phi(ins) => Some(g.resolve(ins[back])),
        _ => None,
    };
    let next = incoming(i)?;
    if !matches!(*g.inst(next), Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == i && g.constant(g.resolve(c)) == Some(1)) {
        return None;
    }
    // The header holds the counter's and the accumulator's phis and constants only.
    let live = |v: &&V| g.resolve(**v) == **v;
    let mut phis = hb.insts.iter().filter(live).filter(|&&v| !matches!(g.inst(v), Inst::Const(_)));
    let (p, q) = (*phis.next()?, *phis.next()?);
    if phis.next().is_some() || (p != i && q != i) {
        return None;
    }
    let acc = if p == i { q } else { p };
    let add = incoming(acc)?;
    let Inst::Bin(BinOp::Add, x, y) = *g.inst(add) else { return None };
    let (x, y) = (g.resolve(x), g.resolve(y));
    let elem = if x == acc { y } else if y == acc { x } else { return None };
    let (load, widen) = match *g.inst(elem) {
        Inst::Convert(0x85, e) => (g.resolve(e), Some(elem)),
        _ => (elem, None),
    };
    let Inst::LoadIndexed(arr, idx, bt) = *g.inst(load) else { return None };
    let arr = g.resolve(arr);
    if g.resolve(idx) != i || !defined_outside(arr) || g.ordered.contains(&load) {
        return None;
    }
    let kind = match (bt, g.ty(acc), widen.is_some()) {
        (BasicType::Int, Ty::I, false) => SumKind::IntInt,
        (BasicType::Int, Ty::J, true) => SumKind::IntLong,
        (BasicType::Long, Ty::J, false) => SumKind::LongLong,
        _ => return None,
    };
    // Nothing else in the body: the load, its widening, the addition and the increment.
    let mine = |v: V| v == next || v == load || v == add || Some(v) == widen;
    if !bb.insts.iter().filter(live).all(|&v| mine(v) || matches!(g.inst(v), Inst::Const(_))) {
        return None;
    }
    Some(Sum { add, load, widen, arr, i, limit, acc, kind, next })
}
