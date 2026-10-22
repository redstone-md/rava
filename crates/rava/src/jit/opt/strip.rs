//! Strip mining of counted loops (C2's `LoopStripMiningIter`): a loop counting `i` by one up to
//! a limit from before it polls for a safepoint on its back edge at every iteration; here it
//! runs strips of at most `STRIP` iterations that do not poll, and polls between them. The
//! header tests `i` against a phi `m`, the strip's end (`min(limit, i + STRIP)`, computed as
//! longs where it could wrap), set once in a new preheader; its exit goes to a block that
//! tests `i` against the limit itself and, while the loop goes on, to one that sets the next
//! strip's end: that block's edge back to the header polls, the body's does not
//! (`Graph::strip_latches`).
//!
//! Only headers of phis and constants qualify (the second test of a strip's end repeats no
//! other work), with one entry and one back edge, an int counter on the left of the test
//! (`i < limit`, or `!(i >= limit)` towards the exit) and a limit that is not a constant (a
//! short constant trip count polls not at all: `polls.rs`).

use super::dom::Dom;
use super::ir::*;

/// Iterations between polls (C2 polls once per 1000).
const STRIP: i64 = 1024;

pub fn mine(g: &mut Graph) {
    let dom = Dom::new(g);
    let found: Vec<(B, B, B, Vec<B>)> = dom
        .loops(g)
        .iter()
        .filter_map(|l| {
            let (pre, latch) = candidate(g, l)?;
            let body: Vec<B> = (0..g.blocks.len() as B).filter(|&b| l.body[b as usize]).collect();
            Some((l.header, pre, latch, body))
        })
        .collect();
    if found.is_empty() {
        return;
    }
    // Room in the build order for a block before each loop and two after it.
    for blk in g.blocks.iter_mut().filter(|b| b.order != u32::MAX) {
        blk.order *= 4;
    }
    for (h, pre, latch, body) in found {
        expand(g, h, pre, latch, &body);
    }
}

/// `(preheader, latch)` of a loop of the strip-mined shape.
fn candidate(g: &Graph, l: &super::dom::Loop) -> Option<(B, B)> {
    let h = l.header;
    let hb = &g.blocks[h as usize];
    let [p0, p1] = hb.preds[..] else { return None };
    let (pre, latch, entry) = match (l.body[p0 as usize], l.body[p1 as usize]) {
        (false, true) => (p0, p1, 0),
        (true, false) => (p1, p0, 1),
        _ => return None,
    };
    // A loop entered straight from another loop's test (the rest of an unrolled loop, under
    // four iterations) does not run long enough to poll.
    if g.blocks[pre as usize].loop_header {
        return None;
    }
    let Term::If { cmp, ty: Ty::I, a, b, then, other, .. } = hb.term else { return None };
    if then == other || l.body[then as usize] == l.body[other as usize] {
        return None;
    }
    let stay = if l.body[then as usize] { cmp } else { super::rce::negate(cmp) };
    let (i, limit) = (g.resolve(a), g.resolve(b));
    if stay != Cmp::Lt || g.constant(limit).is_some() {
        return None;
    }
    if !hb.insts.iter().all(|&v| matches!(g.inst(v), Inst::Phi(_) | Inst::Const(_)) || g.resolve(v) != v) || !hb.insts.contains(&i) {
        return None;
    }
    // The limit comes from before the loop.
    if (0..g.blocks.len()).any(|b| l.body[b] && g.blocks[b].insts.contains(&limit)) {
        return None;
    }
    let Inst::Phi(ins) = g.inst(i) else { return None };
    let next = g.resolve(ins[1 - entry]);
    if !matches!(*g.inst(next), Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == i && g.constant(g.resolve(c)) == Some(1)) {
        return None;
    }
    Some((pre, latch))
}

/// `min(limit, from + STRIP)` appended to `b`: `from + STRIP` only when that is below the limit,
/// so its wrap never matters.
fn strip_end(g: &mut Graph, b: B, from: V, limit: V) -> V {
    let wide_from = g.add(b, Inst::Convert(0x85, from), Ty::J);
    let wide_limit = g.add(b, Inst::Convert(0x85, limit), Ty::J);
    let left = g.add(b, Inst::Bin(BinOp::Sub, wide_limit, wide_from), Ty::J);
    let strip = g.add(b, Inst::Const(STRIP), Ty::J);
    let step = g.add(b, Inst::Const(STRIP), Ty::I);
    let ahead = g.add(b, Inst::Bin(BinOp::Add, from, step), Ty::I);
    g.add(b, Inst::Select(Cmp::Gt, Ty::J, left, strip, ahead, limit), Ty::I)
}

fn expand(g: &mut Graph, h: B, pre: B, latch: B, body: &[B]) {
    let entry = g.blocks[h as usize].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
    let Term::If { cmp, ty, a, b, then, other, unordered } = g.blocks[h as usize].term else { unreachable!("a candidate's header tests") };
    let (i, limit) = (g.resolve(a), g.resolve(b));
    let Inst::Phi(ins) = g.inst(i).clone() else { unreachable!("the counter is a header phi") };
    let init = g.resolve(ins[entry]);
    let (exit, stay) = if body.contains(&then) { (other, then) } else { (then, other) };
    // Orders are multiples of four now: the preheader goes right before the header, `x` and
    // `y` after every block of the loop.
    let last = body.iter().map(|&b| g.blocks[b as usize].order).max().unwrap_or(0);
    let (bci, freq, horder) = (g.blocks[h as usize].bci, g.blocks[h as usize].freq, g.blocks[h as usize].order);
    let (p, x, y) = (g.new_block(bci), g.new_block(bci), g.new_block(bci));
    // The first strip's end, once on entry; the next one's, where a strip ends and the loop
    // goes on.
    let first = strip_end(g, p, init, limit);
    let next = strip_end(g, y, i, limit);
    retarget(&mut g.blocks[pre as usize].term, h, p);
    g.blocks[h as usize].preds[entry] = p;
    let pfreq = g.blocks[pre as usize].freq;
    let blk = &mut g.blocks[p as usize];
    (blk.term, blk.preds, blk.order, blk.freq) = (Term::Goto(h), vec![pre], horder - 1, pfreq);
    g.values.push(Value { inst: Inst::Phi(Vec::new()), ty: Ty::I });
    let m = g.values.len() as V - 1;
    if !g.alias.is_empty() {
        let n = g.alias.len() as V;
        g.alias.extend(n..=m);
    }
    // Inputs in the order of the header's predecessors: the entry, the latch, then `y`.
    let mut inputs = vec![m, m, next];
    inputs[entry] = first;
    g.values[m as usize].inst = Inst::Phi(inputs);
    g.blocks[h as usize].insts.insert(0, m);
    // Every other phi comes back unchanged from the strip's end.
    for v in g.blocks[h as usize].insts.clone() {
        if v == m || g.resolve(v) != v {
            continue;
        }
        if let Inst::Phi(ins) = &mut g.values[v as usize].inst {
            ins.push(v);
        }
    }
    g.blocks[h as usize].preds.push(y);
    // The header leaves for `x` where it left the loop; `x` goes on to `y` and the header, or
    // leaves the loop.
    let to = |t: B, from: B, into: B| if t == from { into } else { t };
    g.blocks[h as usize].term = Term::If { cmp, ty, a, b: m, then: to(then, exit, x), other: to(other, exit, x), unordered };
    for q in g.blocks[exit as usize].preds.iter_mut().filter(|q| **q == h) {
        *q = x;
    }
    let term = Term::If { cmp, ty, a, b: limit, then: to(then, stay, y), other: to(other, stay, y), unordered };
    let blk = &mut g.blocks[x as usize];
    (blk.term, blk.preds, blk.order, blk.freq) = (term, vec![h], last + 1, freq / STRIP as f32);
    let blk = &mut g.blocks[y as usize];
    (blk.term, blk.preds, blk.order, blk.freq) = (Term::Goto(h), vec![x], last + 2, freq / STRIP as f32);
    g.strip_latches.insert(latch);
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
