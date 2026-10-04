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
