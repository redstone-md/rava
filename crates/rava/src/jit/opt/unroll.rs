//! Full unrolling of short counted loops (C2 unrolls them by its trip count): a loop of a
//! header that only holds phis and tests `i < limit`, and one body block, counting `i` by 1
//! from a constant to a constant at most `MAX_TRIPS` steps apart, becomes that many copies of
//! the body in its preheader. The loop test, the counter's increment and the back edge go;
//! each copy reads the values the previous one computed (constant indices, so its bounds
//! checks fold and its loads issue together). The body makes no call, allocation or monitor
//! operation; its deoptimization states are copied with the iteration's values.
//!
//! The climate tree's distance loop (`for (i = 0; i < 7; i++)`) ran its test and increment
//! seven times per node.

use super::dom::Dom;
use super::ir::*;
use std::collections::HashMap;
use std::rc::Rc;

const MAX_TRIPS: i64 = 8;
/// Instructions of the unrolled body, all copies together.
const MAX_INSTS: usize = 160;

pub fn unroll(g: &mut Graph) {
    loop {
        let dom = Dom::new(g);
        let Some((h, body, pre, exit, trips)) = dom.loops(g).iter().find_map(|l| candidate(g, l)) else { return };
        expand(g, h, body, pre, exit, trips);
    }
}

/// `(header, body, preheader, exit, trips)` of a loop of the unrollable shape.
fn candidate(g: &Graph, l: &super::dom::Loop) -> Option<(B, B, B, B, i64)> {
    let h = l.header;
    let hb = &g.blocks[h as usize];
    let blocks: Vec<B> = (0..g.blocks.len() as B).filter(|&b| l.body[b as usize]).collect();
    let [_, _] = blocks[..] else { return None };
    let body = *blocks.iter().find(|&&b| b != h)?;
    let bb = &g.blocks[body as usize];
    let [p0, p1] = hb.preds[..] else { return None };
    let (pre, entry) = if p1 == body { (p0, 0) } else if p0 == body { (p1, 1) } else { return None };
    if bb.preds[..] != [h] || !matches!(bb.term, Term::Goto(t) if t == h) || !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t == h) {
        return None;
    }
    let Term::If { cmp, ty: Ty::I, a, b, then, other, .. } = hb.term else { return None };
    let (stay, exit) = if then == body { (cmp, other) } else if other == body { (super::rce::negate(cmp), then) } else { return None };
    let (a, b) = (g.resolve(a), g.resolve(b));
    let (i, limit) = match stay {
        Cmp::Lt => (a, b),
        Cmp::Gt => (b, a),
        _ => return None,
    };
    // The header: phis (the counter among them) and constants only.
    if !hb.insts.iter().all(|&v| matches!(g.inst(v), Inst::Phi(_) | Inst::Const(_)) || g.resolve(v) != v) || !hb.insts.contains(&i) {
        return None;
    }
    let Inst::Phi(ins) = g.inst(i) else { return None };
    let (init, next) = (g.constant(g.resolve(ins[entry]))?, g.resolve(ins[1 - entry]));
    let limit = g.constant(limit)?;
    if !matches!(*g.inst(next), Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == i && g.constant(g.resolve(c)) == Some(1)) {
        return None;
    }
    let trips = limit - init;
    let insts = bb.insts.iter().filter(|&&v| g.resolve(v) == v).count();
    if !(1..=MAX_TRIPS).contains(&trips) || insts * trips as usize > MAX_INSTS {
        return None;
    }
    let simple = bb.insts.iter().all(|&v| !matches!(g.inst(v), Inst::Call(..) | Inst::Monitor(..) | Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) | Inst::Phi(_)));
    // The exit takes no phi input from the header that a later copy would have to supply on
    // another edge: it becomes the preheader's successor in the header's place.
    simple.then_some((h, body, pre, exit, trips))
}

fn expand(g: &mut Graph, h: B, body: B, pre: B, exit: B, trips: i64) {
    let entry = g.blocks[h as usize].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
    let phis: Vec<V> = g.blocks[h as usize].insts.iter().copied().filter(|&v| matches!(g.inst(v), Inst::Phi(_)) && g.resolve(v) == v).collect();
    // Each phi's value in the current iteration, and what it takes for the next.
    let mut env: HashMap<V, V> = phis.iter().map(|&p| {
        let Inst::Phi(ins) = g.inst(p) else { unreachable!() };
        (p, g.resolve(ins[entry]))
    }).collect();
    let next: Vec<(V, V)> = phis.iter().map(|&p| {
        let Inst::Phi(ins) = g.inst(p) else { unreachable!() };
        (p, g.resolve(ins[1 - entry]))
    }).collect();
    let insts: Vec<V> = g.blocks[body as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v).collect();
    for _ in 0..trips {
        let mut states: HashMap<*const State, Rc<State>> = HashMap::new();
        for &v in &insts {
            let map = |env: &HashMap<V, V>, x: V| env.get(&g.resolve(x)).copied().unwrap_or(x);
            let mut inst = g.inst(v).clone();
            inst.map_inputs(|x| map(&env, x));
            if let Some(s) = inst.state() {
                let copied = states.entry(Rc::as_ptr(s)).or_insert_with(|| copy_state(g, s, &env)).clone();
                inst.set_state(copied);
            }
            let ty = g.ty(v);
            let c = g.add(pre, inst, ty);
            if g.ordered.contains(&v) {
                g.ordered.insert(c);
            }
            env.insert(v, c);
        }
        let advanced: Vec<(V, V)> = next.iter().map(|&(p, n)| (p, env.get(&n).copied().unwrap_or(n))).collect();
        for (p, n) in advanced {
            env.insert(p, n);
        }
    }
    // After the loop the phis hold the last values.
    if g.alias.is_empty() {
        g.alias = (0..g.values.len() as V).collect();
    }
    for &p in &phis {
        g.alias[p as usize] = env[&p];
    }
    g.blocks[pre as usize].term = Term::Goto(exit);
    for q in &mut g.blocks[exit as usize].preds {
        if *q == h {
            *q = pre;
        }
    }
    for b in [h, body] {
        let blk = &mut g.blocks[b as usize];
        blk.term = Term::None;
        blk.insts.clear();
        blk.preds.clear();
        blk.loop_header = false;
    }
    g.loop_entry.remove(&h);
}

/// `s` with the current iteration's values (callers included).
fn copy_state(g: &Graph, s: &Rc<State>, env: &HashMap<V, V>) -> Rc<State> {
    let map = |v: &Option<V>| v.map(|x| env.get(&g.resolve(x)).copied().unwrap_or(x));
    Rc::new(State {
        method: s.method,
        bci: s.bci,
        locals: s.locals.iter().map(map).collect(),
        stack: s.stack.iter().map(map).collect(),
        caller: s.caller.as_ref().map(|c| copy_state(g, c, env)),
        frame: s.frame,
    })
}

/// Copies of the body per iteration of a partially unrolled loop.
const FACTOR: usize = 4;
/// Instructions of a body worth unrolling partially (a larger one gains little from saving the
/// test, increment and safepoint poll of three iterations in four).
const MAX_PARTIAL_INSTS: usize = 24;

/// Partial unrolling of counted loops of unknown trip count (C2 unrolls its counted loops the
/// same way): a loop of a header holding only phis and constants that tests `i < limit`
/// (`limit` defined before the loop) and one body block counting `i` by 1 gets a main loop in
/// front of it running `FACTOR` copies of the body per test, while `i + FACTOR - 1 < limit`
/// (compared as longs, so no bound can wrap); the original loop runs the remaining iterations.
/// Each copy keeps its deoptimization states with its iteration's values, so a trap in any copy
/// resumes the loop in the interpreter at that iteration.
pub fn unroll_partially(g: &mut Graph) {
    let dom = Dom::new(g);
    let found: Vec<(B, B, B)> = dom.loops(g).iter().filter_map(|l| partial_candidate(g, l)).collect();
    if found.is_empty() {
        return;
    }
    // Room in the build order for two blocks in front of each header.
    for blk in g.blocks.iter_mut().filter(|b| b.order != u32::MAX) {
        blk.order *= 4;
    }
    for (h, body, pre) in found {
        expand_partially(g, h, body, pre);
    }
}

/// `(header, body, preheader)` of a loop of the partially unrollable shape.
fn partial_candidate(g: &Graph, l: &super::dom::Loop) -> Option<(B, B, B)> {
    let h = l.header;
    let hb = &g.blocks[h as usize];
    let blocks: Vec<B> = (0..g.blocks.len() as B).filter(|&b| l.body[b as usize]).collect();
    let [_, _] = blocks[..] else { return None };
    let body = *blocks.iter().find(|&&b| b != h)?;
    let bb = &g.blocks[body as usize];
    let [p0, p1] = hb.preds[..] else { return None };
    let (pre, entry) = if p1 == body { (p0, 0) } else if p0 == body { (p1, 1) } else { return None };
    if bb.preds[..] != [h] || !matches!(bb.term, Term::Goto(t) if t == h) || !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t == h) {
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
    if !hb.insts.iter().all(|&v| matches!(g.inst(v), Inst::Phi(_) | Inst::Const(_)) || g.resolve(v) != v) || !hb.insts.contains(&i) {
        return None;
    }
    // The limit comes from before the loop.
    if hb.insts.contains(&limit) || bb.insts.contains(&limit) {
        return None;
    }
    let Inst::Phi(ins) = g.inst(i) else { return None };
    let next = g.resolve(ins[1 - entry]);
    if !matches!(*g.inst(next), Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == i && g.constant(g.resolve(c)) == Some(1)) {
        return None;
    }
    // A short constant trip count is not worth a second loop.
    if let (Some(init), Some(n)) = (g.constant(g.resolve(ins[entry])), g.constant(limit)) {
        if n - init < 4 * FACTOR as i64 {
            return None;
        }
    }
    let insts: Vec<V> = bb.insts.iter().copied().filter(|&v| g.resolve(v) == v).collect();
    if insts.is_empty() || insts.len() > MAX_PARTIAL_INSTS {
        return None;
    }
    // A monitor region may repeat: its copies merge into one region (`coarsen.rs`).
    let simple = insts.iter().all(|&v| !matches!(g.inst(v), Inst::Call(..) | Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) | Inst::Phi(_)));
    simple.then_some((h, body, pre))
}

fn expand_partially(g: &mut Graph, h: B, body: B, pre: B) {
    let entry = g.blocks[h as usize].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
    let Term::If { cmp, a, b, then, .. } = g.blocks[h as usize].term else { unreachable!("a candidate's header tests") };
    let stay = if then == body { cmp } else { super::rce::negate(cmp) };
    let (i, limit) = if stay == Cmp::Lt { (g.resolve(a), g.resolve(b)) } else { (g.resolve(b), g.resolve(a)) };
    let (order, hfreq, bfreq, hbci, bbci) = {
        let (hb, bb) = (&g.blocks[h as usize], &g.blocks[body as usize]);
        (hb.order, hb.freq, bb.freq, hb.bci, bb.bci)
    };
    let (h4, b4) = (g.new_block(hbci), g.new_block(bbci));
    // The main loop's bound: the last index the copies reach is still below the limit.
    let wide = g.add(pre, Inst::Convert(0x85, limit), Ty::J);
    let back = g.add(pre, Inst::Const(-(FACTOR as i64 - 1)), Ty::J);
    let bound = g.add(pre, Inst::Bin(BinOp::Add, wide, back), Ty::J);
    // The main loop's phis: entered with the loop's initial values, carried around its back edge.
    let header: Vec<V> = g.blocks[h as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v).collect();
    let mut env: HashMap<V, V> = HashMap::new();
    let mut phis: Vec<(V, V, V)> = Vec::new();
    for &v in &header {
        match g.inst(v).clone() {
            Inst::Phi(ins) => {
                let p = g.add(h4, Inst::Phi(vec![g.resolve(ins[entry]), g.resolve(ins[entry])]), g.ty(v));
                env.insert(v, p);
                phis.push((v, p, g.resolve(ins[1 - entry])));
            }
            inst => {
                let c = g.add(h4, inst, g.ty(v));
                env.insert(v, c);
            }
        }
    }
    let wi = g.add(h4, Inst::Convert(0x85, env[&i]), Ty::J);
    let insts: Vec<V> = g.blocks[body as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v).collect();
    let mut cur = env.clone();
    for _ in 0..FACTOR {
        let mut states: HashMap<*const State, Rc<State>> = HashMap::new();
        for &v in &insts {
            let map = |env: &HashMap<V, V>, x: V| env.get(&g.resolve(x)).copied().unwrap_or(x);
            let mut inst = g.inst(v).clone();
            inst.map_inputs(|x| map(&cur, x));
            if let Some(s) = inst.state() {
                let copied = states.entry(Rc::as_ptr(s)).or_insert_with(|| copy_state(g, s, &cur)).clone();
                inst.set_state(copied);
            }
            let ty = g.ty(v);
            let c = g.add(b4, inst, ty);
            if g.ordered.contains(&v) {
                g.ordered.insert(c);
            }
            cur.insert(v, c);
        }
        let advanced: Vec<(V, V)> = phis.iter().map(|&(p, _, n)| (p, cur.get(&n).copied().unwrap_or(n))).collect();
        for (p, n) in advanced {
            cur.insert(p, n);
        }
    }
    // The main loop's back edge (its second predecessor) carries the last copy's values.
    for &(p, p4, _) in &phis {
        if let Inst::Phi(ins) = &mut g.values[p4 as usize].inst {
            ins[1] = cur[&p];
        }
    }
    {
        let blk = &mut g.blocks[h4 as usize];
        (blk.term, blk.preds, blk.loop_header, blk.order, blk.freq) =
            (Term::If { cmp: Cmp::Lt, ty: Ty::J, a: wi, b: bound, then: b4, other: h, unordered: false }, vec![pre, b4], true, order - 2, hfreq);
    }
    {
        let blk = &mut g.blocks[b4 as usize];
        (blk.term, blk.preds, blk.order, blk.freq) = (Term::Goto(h4), vec![h4], order - 1, bfreq);
    }
    g.blocks[pre as usize].term = Term::Goto(h4);
    // The remaining iterations start from the main loop's values.
    g.blocks[h as usize].preds[entry] = h4;
    for (p, p4, _) in phis {
        if let Inst::Phi(ins) = &mut g.values[p as usize].inst {
            ins[entry] = p4;
        }
    }
}
