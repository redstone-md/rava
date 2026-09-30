//! Redundant null-check elimination over the dominator tree: a check of a value (aliases
//! resolved) that a dominating check or allocation already proved non-null is dropped. This
//! catches what the graph builder cannot see while building, such as a check before a loop of
//! a value the loop reads through a phi that turned out trivial (`phis.rs`).

use super::ir::*;

pub fn eliminate(g: &mut Graph) {
    let n = g.blocks.len();
    let built = |b: B| !matches!(g.blocks[b as usize].term, Term::None);
    // Reverse postorder from the entry block.
    let (mut rpo, mut seen, mut stack) = (Vec::new(), vec![false; n], vec![(0 as B, 0usize)]);
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        let succ = g.blocks[b as usize].term.successors();
        if let Some(&s) = succ.get(i) {
            stack.push((b, i + 1));
            if !seen[s as usize] && built(s) {
                seen[s as usize] = true;
                stack.push((s, 0));
            }
        } else {
            rpo.push(b);
        }
    }
    rpo.reverse();
    let mut index = vec![usize::MAX; n];
    for (i, &b) in rpo.iter().enumerate() {
        index[b as usize] = i;
    }
    // Immediate dominators (Cooper, Harvey and Kennedy).
    let mut idom = vec![usize::MAX; rpo.len()];
    idom[0] = 0;
    let intersect = |idom: &[usize], mut a: usize, mut b: usize| {
        while a != b {
            while a > b {
                a = idom[a];
            }
            while b > a {
                b = idom[b];
            }
        }
        a
    };
    loop {
        let mut changed = false;
        for i in 1..rpo.len() {
            let preds = g.blocks[rpo[i] as usize].preds.iter().map(|&p| index[p as usize]).filter(|&p| p != usize::MAX && idom[p] != usize::MAX);
            let new = preds.reduce(|a, b| intersect(&idom, a, b)).unwrap_or(usize::MAX);
            if new != idom[i] {
                idom[i] = new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
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
                Inst::New(..) | Inst::NewArray(..) => known.push(v),
                _ => {}
            }
        }
        walk.extend(children[i].iter().map(|&c| (c, false)));
    }
    for b in &mut g.blocks {
        b.insts.retain(|&v| !dead[v as usize]);
    }
}
