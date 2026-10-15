//! Fill loops (C2's `OptimizeFill`, and its vectorized stores): a counted loop whose one body
//! block only stores one value, computed before the loop, into the counter's element of an
//! array, `for (i = from; i < to; i++) a[i] = x`, becomes one `FillRange` of the elements
//! from the counter's value to the limit (`codegen_intr.rs`, sixteen bytes per store), after
//! which the counter takes the limit, so the loop's test ends it.
//!
//! The body holds no bounds check (range check elimination proved the range, or the loop would
//! not have this shape), so the elements are all the array's. A matcher's `reset` clears its
//! group and local arrays this way on every search.

use super::dom::Dom;
use super::ir::*;
use crate::jit::intrinsics::Intrinsic;
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
    let found: Vec<Fill> = dom.loops(g).iter().filter_map(|l| candidate(g, l, &def)).collect();
    for f in found {
        g.values[f.store as usize].inst = Inst::Intrinsic(Intrinsic::FillRange(f.shift), vec![f.arr, f.i, f.limit, f.x], None);
        // The counter's next value is the limit: the header's test leaves the loop.
        g.alias[f.next as usize] = f.limit;
        for blk in &mut g.blocks {
            blk.insts.retain(|&v| v != f.next);
        }
    }
}

struct Fill {
    store: V,
    arr: V,
    i: V,
    limit: V,
    x: V,
    /// Log2 of the element size.
    shift: u8,
    /// The counter's increment.
    next: V,
}

fn candidate(g: &Graph, l: &super::dom::Loop, def: &[B]) -> Option<Fill> {
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
    // The header holds the counter's phi and constants only: no other value crosses the loop.
    let live = |v: &&V| g.resolve(**v) == **v;
    if !hb.insts.iter().filter(live).all(|&v| v == i || matches!(g.inst(v), Inst::Const(_))) || !hb.insts.contains(&i) {
        return None;
    }
    let Inst::Phi(ins) = g.inst(i) else { return None };
    let back = hb.preds.iter().position(|&p| p == body)?;
    let next = g.resolve(ins[back]);
    if !matches!(*g.inst(next), Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == i && g.constant(g.resolve(c)) == Some(1)) {
        return None;
    }
    // Parameters and values of earlier blocks; a constant's block may be any.
    let defined_outside = |v: V| def.get(v as usize).is_some_and(|&d| d != B::MAX && !l.body[d as usize]);
    if !defined_outside(limit) {
        return None;
    }
    let mut store = None;
    for &v in bb.insts.iter().filter(live) {
        match *g.inst(v) {
            Inst::Const(_) => {}
            _ if v == next => {}
            Inst::StoreIndexed(arr, idx, bt, x) if store.is_none() && g.resolve(idx) == i && !g.ordered.contains(&v) => {
                let shift = match bt {
                    BasicType::Int | BasicType::Float => 2,
                    BasicType::Long | BasicType::Double => 3,
                    _ => return None,
                };
                let (arr, x) = (g.resolve(arr), g.resolve(x));
                if !defined_outside(arr) || !(defined_outside(x) || matches!(g.inst(x), Inst::Const(_))) {
                    return None;
                }
                store = Some((v, arr, x, shift));
            }
            _ => return None,
        }
    }
    let (store, arr, x, shift) = store?;
    Some(Fill { store, arr, i, limit, x, shift, next })
}
