//! Block liveness for register allocation: backward dataflow over per-block use and def sets
//! computed once, so each fixpoint round is a few word-wise set operations per block.
//!
//! Two kinds of reads: real ones (instruction and terminator inputs, phi inputs on edges) and
//! reads by deoptimization states, which register allocation counts only for the intervals
//! that include them (`states`). Phis read by states always count: they get no home slot.

use super::ir::*;
use std::collections::HashMap;
use std::rc::Rc;

/// A set of values as a bit vector.
#[derive(Clone, PartialEq)]
pub struct Set(Vec<u64>);

impl Set {
    pub fn new(n: usize) -> Set {
        Set(vec![0; n.div_ceil(64)])
    }
    pub fn insert(&mut self, v: V) {
        self.0[v as usize / 64] |= 1 << (v % 64);
    }
    pub fn union(&mut self, o: &Set) {
        self.0.iter_mut().zip(&o.0).for_each(|(a, b)| *a |= b);
    }
    /// `self := (self - kill) | add`.
    fn transfer(&mut self, kill: &Set, add: &Set) {
        for ((a, k), g) in self.0.iter_mut().zip(&kill.0).zip(&add.0) {
            *a = (*a & !k) | g;
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = V> + '_ {
        self.0.iter().enumerate().filter(|(_, &w)| w != 0).flat_map(|(i, &w)| {
            let mut w = w;
            std::iter::from_fn(move || {
                (w != 0).then(|| {
                    let b = w.trailing_zeros();
                    w &= w - 1;
                    (i * 64) as V + b
                })
            })
        })
    }
}

/// Every value a state reads, callers included (aliases resolved).
pub fn state_values(g: &Graph, s: &State, out: &mut Vec<V>) {
    out.extend(s.locals.iter().chain(s.stack.iter()).flatten().map(|&v| g.resolve(v)));
    if let Some(c) = &s.caller {
        state_values(g, c, out);
    }
}

/// The values each state reads, computed once per state (instructions share states).
pub struct StateReads {
    cache: HashMap<*const State, Rc<[V]>>,
}

impl StateReads {
    pub fn new() -> StateReads {
        StateReads { cache: HashMap::new() }
    }

    pub fn of(&mut self, g: &Graph, s: &Rc<State>) -> Rc<[V]> {
        self.cache
            .entry(Rc::as_ptr(s))
            .or_insert_with(|| {
                let mut out = Vec::new();
                state_values(g, s, &mut out);
                out.into()
            })
            .clone()
    }
}

/// Values live on entry to each block; with `states`, state reads keep values live.
pub fn liveness(g: &Graph, order: &[B], reads: &mut StateReads, states: bool) -> Vec<Set> {
    let n = g.values.len();
    let counts = |u: V| states || matches!(g.inst(u), Inst::Phi(_));
    // Per block: values it defines, and values it reads that are defined elsewhere (SSA: a
    // value defined in the block is only read after its definition, phis aside).
    let mut defs = vec![Set::new(n); g.blocks.len()];
    let mut uses = vec![Set::new(n); g.blocks.len()];
    for &b in order {
        let blk = &g.blocks[b as usize];
        let (d, u) = (&mut defs[b as usize], &mut uses[b as usize]);
        blk.term.inputs().into_iter().for_each(|x| u.insert(g.resolve(x)));
        if let Term::Deopt(s) = &blk.term {
            reads.of(g, s).iter().filter(|&&x| counts(x)).for_each(|&x| u.insert(x));
        }
        for &v in &blk.insts {
            d.insert(v);
            let inst = g.inst(v);
            if matches!(inst, Inst::Phi(_)) || g.resolve(v) != v {
                continue;
            }
            inst.inputs().into_iter().for_each(|x| u.insert(g.resolve(x)));
            if let Some(s) = inst.state() {
                reads.of(g, s).iter().filter(|&&x| counts(x)).for_each(|&x| u.insert(x));
            }
        }
        let d = d.clone();
        u.0.iter_mut().zip(&d.0).for_each(|(a, k)| *a &= !k);
    }
    let mut live_in = vec![Set::new(n); g.blocks.len()];
    loop {
        let mut changed = false;
        for &b in order.iter().rev() {
            let blk = &g.blocks[b as usize];
            let mut live = Set::new(n);
            for s in blk.term.successors() {
                live.union(&live_in[s as usize]);
                for ph in super::regalloc::phis(g, s) {
                    live.insert(super::regalloc::phi_input(g, ph, b, s));
                }
            }
            live.transfer(&defs[b as usize], &uses[b as usize]);
            if live != live_in[b as usize] {
                live_in[b as usize] = live;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}
