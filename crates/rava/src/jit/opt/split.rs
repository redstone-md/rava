//! Live-range splitting at loop boundaries, by SSA copies (`Inst::Copy`): the register
//! allocator gives each value one interval, so a value used in a hot loop but live far beyond
//! it competes as one long, sparsely used interval and tends to be spilled inside the loop too.
//!
//! For each innermost loop:
//! - a value defined before the loop, used in it and live after it gets a copy at the end of
//!   the loop's single entry block, which the loop's uses read (the original, now unused in the
//!   loop, is the one to spill under pressure);
//! - a value defined in the loop and used after it gets a copy at the start of the loop's
//!   single exit block, which the later uses read (the loop's interval ends at the exit).
//!
//! Deoptimization states in the loop keep reading the original values; those after the exit
//! read the copy (a phi read by a state keeps its register, having no home slot to read from).

use super::dom::{Dom, Loop};
use super::ir::*;

pub fn split(g: &mut Graph) {
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    let loops = dom.loops(g);
    let headers: Vec<B> = loops.iter().map(|l| l.header).collect();
    for l in &loops {
        let innermost = !headers.iter().any(|&h| h != l.header && l.body[h as usize]);
        if innermost {
            split_loop(g, &dom, &index, l);
        }
    }
}

/// Adds `Copy(v)` to block `b` (at its end), a value of its own.
fn copy(g: &mut Graph, b: B, v: V) -> V {
    g.add(b, Inst::Copy(v), g.ty(v))
}

/// A read of a value: in block `b` by instruction or terminator, or on the edge from `b` into
/// phi `phi` (the read happens at the end of `b`).
#[derive(Clone, Copy)]
pub(super) enum Use {
    Inst(B, V),
    Term(B),
    Edge(B, V),
}

impl Use {
    pub(super) fn block(self) -> B {
        match self {
            Use::Inst(b, _) | Use::Term(b) | Use::Edge(b, _) => b,
        }
    }
}

/// The real (non-state) reads of every value, aliases resolved.
pub(super) fn uses(g: &Graph) -> Vec<Vec<Use>> {
    let mut out = vec![Vec::new(); g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        if matches!(blk.term, Term::None) {
            continue;
        }
        let b = b as B;
        for &v in &blk.insts {
            match g.inst(v) {
                Inst::Phi(inputs) => {
                    for (i, &x) in inputs.iter().enumerate() {
                        out[g.resolve(x) as usize].push(Use::Edge(blk.preds[i], v));
                    }
                }
                inst => inst.inputs().into_iter().for_each(|x| out[g.resolve(x) as usize].push(Use::Inst(b, v))),
            }
        }
        blk.term.inputs().into_iter().for_each(|x| out[g.resolve(x) as usize].push(Use::Term(b)));
    }
    out
}

fn split_loop(g: &mut Graph, dom: &Dom, index: &[usize], l: &Loop) {
    let inside = |b: B| l.body[b as usize];
    let entries: Vec<B> = g.blocks[l.header as usize].preds.iter().copied().filter(|&p| !inside(p)).collect();
    let exits: Vec<B> = (0..g.blocks.len() as B)
        .filter(|&b| !inside(b) && !matches!(g.blocks[b as usize].term, Term::None) && g.blocks[b as usize].preds.iter().any(|&p| inside(p)))
        .collect();
    let [entry] = entries[..] else { return };
    let exit = match exits[..] {
        [e] if g.blocks[e as usize].preds.iter().all(|&p| inside(p)) => Some(e),
        _ => None,
    };
    // The blocks the exit dominates (all after the loop).
    let after_exit: Vec<B> = exit.map_or(Vec::new(), |e| {
        (0..g.blocks.len() as B).filter(|&b| !inside(b) && index[b as usize] != usize::MAX && dom.dominates(index[e as usize], index[b as usize])).collect()
    });
    let uses = uses(g);
    let mut def = vec![B::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        blk.insts.iter().for_each(|&v| def[v as usize] = b as B);
    }
    // Blocks control reaches after leaving the loop (possibly back into it via an outer loop).
    let mut after = vec![false; g.blocks.len()];
    let mut work: Vec<B> = exits.clone();
    while let Some(b) = work.pop() {
        if !std::mem::replace(&mut after[b as usize], true) {
            work.extend(g.blocks[b as usize].term.successors());
        }
    }
    let in_loop = |u: &Use| match *u {
        // An edge read inside the loop: into a loop phi from a loop block.
        Use::Edge(p, phi) => inside(p) && inside(def[phi as usize]),
        u => inside(u.block()),
    };
    let candidates: Vec<V> = (0..uses.len() as V).filter(|&v| g.resolve(v) == v && !matches!(g.inst(v), Inst::Const(_)) && !uses[v as usize].is_empty()).collect();
    for v in candidates {
        let us = &uses[v as usize];
        let d = def[v as usize];
        if d == B::MAX {
            continue;
        }
        if !inside(d) {
            // Defined before the loop: copy in the entry block when also live after it.
            let (within, rest): (Vec<Use>, Vec<Use>) = us.iter().partition(|u| in_loop(u));
            if within.is_empty() || !rest.iter().any(|u| after[u.block() as usize] && !inside(u.block())) {
                continue;
            }
            let c = copy(g, entry, v);
            rename(g, &def, &within, v, c);
        } else if let Some(e) = exit {
            // Defined in the loop: copy at the exit when every later read comes after it.
            let (_, outside): (Vec<Use>, Vec<Use>) = us.iter().partition(|u| in_loop(u) || matches!(u, Use::Edge(p, phi) if def[*phi as usize] == e && inside(*p)));
            let ei = index[e as usize];
            if outside.is_empty() || !outside.iter().all(|u| index[u.block() as usize] != usize::MAX && dom.dominates(ei, index[u.block() as usize])) {
                continue;
            }
            let c = copy(g, e, v);
            let insts = &mut g.blocks[e as usize].insts;
            insts.pop();
            let at = insts.iter().position(|&x| !matches!(g.values[x as usize].inst, Inst::Phi(_))).unwrap_or(insts.len());
            insts.insert(at, c);
            rename(g, &def, &outside, v, c);
            rename_states(g, &after_exit, v, c);
        }
    }
}

/// Makes the reads `at` of `v` read `c` instead (`def`: each value's block).
pub(super) fn rename(g: &mut Graph, def: &[B], at: &[Use], v: V, c: V) {
    let alias = std::mem::take(&mut g.alias);
    let is_v = |x: V| {
        let mut x = x;
        while let Some(&a) = alias.get(x as usize).filter(|&&a| a != x) {
            x = a;
        }
        x == v
    };
    for u in at {
        match *u {
            Use::Inst(_, i) => g.values[i as usize].inst.map_inputs(|x| if is_v(x) { c } else { x }),
            Use::Term(b) => g.blocks[b as usize].term.map_inputs(|x| if is_v(x) { c } else { x }),
            Use::Edge(p, phi) => {
                let k = g.blocks[def[phi as usize] as usize].preds.iter().position(|&x| x == p).expect("edge into the phi's block");
                if let Inst::Phi(inputs) = &mut g.values[phi as usize].inst {
                    if is_v(inputs[k]) {
                        inputs[k] = c;
                    }
                }
            }
        }
    }
    g.alias = alias;
}

/// Makes the states of the instructions and deoptimizing terminators of `blocks` read `c`
/// where they read `v` (callers' states included), sharing rewritten states like the originals.
pub(super) fn rename_states(g: &mut Graph, blocks: &[B], v: V, c: V) {
    use std::collections::HashMap;
    use std::rc::Rc;
    fn remap(g: &Graph, s: &Rc<State>, v: V, c: V, done: &mut HashMap<*const State, Rc<State>>) -> Rc<State> {
        if let Some(r) = done.get(&Rc::as_ptr(s)) {
            return r.clone();
        }
        let caller = s.caller.as_ref().map(|p| remap(g, p, v, c, done));
        let hit = |x: &Option<V>| x.is_some_and(|x| g.resolve(x) == v);
        let changed = s.locals.iter().chain(&s.stack).any(hit) || match (&caller, &s.caller) {
            (Some(a), Some(b)) => !Rc::ptr_eq(a, b),
            _ => false,
        };
        let out = if changed {
            let map = |xs: &[Option<V>]| xs.iter().map(|&x| if hit(&x) { Some(c) } else { x }).collect();
            Rc::new(State { method: s.method, bci: s.bci, locals: map(&s.locals), stack: map(&s.stack), caller, frame: s.frame })
        } else {
            s.clone()
        };
        done.insert(Rc::as_ptr(s), out.clone());
        out
    }
    let mut done = HashMap::new();
    for &b in blocks {
        for i in 0..g.blocks[b as usize].insts.len() {
            let x = g.blocks[b as usize].insts[i];
            let Some(s) = g.values[x as usize].inst.state().cloned() else { continue };
            let r = remap(g, &s, v, c, &mut done);
            if !Rc::ptr_eq(&r, &s) {
                g.values[x as usize].inst.set_state(r);
            }
        }
        if let Term::Deopt(s) = &g.blocks[b as usize].term {
            let r = remap(g, &s.clone(), v, c, &mut done);
            g.blocks[b as usize].term = Term::Deopt(r);
        }
        // The entry state of a loop headed here, which hoisted checks deoptimize to (`rce.rs`).
        if let Some(s) = g.loop_entry.get(&b).cloned() {
            let r = remap(g, &s, v, c, &mut done);
            g.loop_entry.insert(b, r);
        }
    }
}
