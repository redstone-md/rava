//! Drops the checks that an inlined method is still unique which a dominating check already
//! answers. All of a compilation's checks test one byte (`Graph::deps`), set before any instance
//! of a class overriding one of its methods exists (`Method::mark_overridden`): a check passing
//! at some point proves that no receiver existing then has such a class, so a later check on a
//! receiver defined before it has nothing left to prove, whatever runs in between. The
//! accessors a matrix copy inlines (`m00()`, `m01()`, ...) each had a check on one argument.

use super::dom::Dom;
use super::ir::*;
use std::collections::{HashMap, HashSet};

pub fn merge(g: &mut Graph) {
    if g.flag_receivers.len() < 2 {
        return;
    }
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    // Where each value is defined: the block's rpo position and the position in it.
    let mut def: HashMap<V, (usize, usize)> = HashMap::new();
    for &b in &dom.rpo {
        for (at, &v) in g.blocks[b as usize].insts.iter().enumerate() {
            def.insert(v, (index[b as usize], at));
        }
    }
    let before = |x: (usize, usize), y: (usize, usize)| if x.0 == y.0 { x.1 < y.1 } else { dom.dominates(x.0, y.0) };
    let mut kept: Vec<(usize, usize)> = Vec::new();
    let mut gone: HashSet<V> = HashSet::new();
    for &b in &dom.rpo {
        for (at, &v) in g.blocks[b as usize].insts.iter().enumerate() {
            if g.resolve(v) != v || !matches!(g.inst(v), Inst::FlagCheck(..)) {
                continue;
            }
            let here = (index[b as usize], at);
            // The receiver exists at a kept check: defined before it, or a parameter.
            let exists_at = |c: (usize, usize)| match g.flag_receivers.get(&v).map(|&r| g.resolve(r)) {
                Some(r) => def.get(&r).map_or(matches!(g.inst(r), Inst::Param(_)), |&p| before(p, c)),
                None => false,
            };
            let covered = kept.iter().any(|&c| before(c, here) && exists_at(c));
            if covered {
                gone.insert(v);
            } else {
                kept.push(here);
            }
        }
    }
    if !gone.is_empty() {
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}
