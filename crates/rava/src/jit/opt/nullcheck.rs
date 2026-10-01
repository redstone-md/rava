//! Redundant null-check elimination over the dominator tree: a check of a value (aliases
//! resolved) that a dominating check or allocation already proved non-null is dropped. This
//! catches what the graph builder cannot see while building, such as a check before a loop of
//! a value the loop reads through a phi that turned out trivial (`phis.rs`).

use super::ir::*;

pub fn eliminate(g: &mut Graph) {
    let super::dom::Dom { rpo, idom } = super::dom::Dom::new(g);
    let mut children = vec![Vec::new(); rpo.len()];
    for i in 1..rpo.len() {
        if idom[i] != usize::MAX {
            children[idom[i]].push(i);
        }
    }
    // Walk the dominator tree with the values known non-null on entry to each block.
    let mut known: Vec<V> = Vec::new();
    let mut dead = vec![false; g.values.len()];
    let mut walk = vec![(0usize, false)];
    let mut marks = Vec::new();
    while let Some((i, done)) = walk.pop() {
        if done {
            known.truncate(marks.pop().unwrap());
            continue;
        }
        marks.push(known.len());
        walk.push((i, true));
        for &v in &g.blocks[rpo[i] as usize].insts {
            match g.values[v as usize].inst {
                Inst::NullCheck(x, _) => {
                    let x = g.resolve(x);
                    if known.contains(&x) {
                        dead[v as usize] = true;
                    } else {
                        known.push(x);
                    }
                }
                Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) => known.push(v),
                _ => {}
            }
        }
        walk.extend(children[i].iter().map(|&c| (c, false)));
    }
    for b in &mut g.blocks {
        b.insts.retain(|&v| !dead[v as usize]);
    }
}
