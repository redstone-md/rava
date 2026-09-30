//! Dominators of the graph's built blocks (Cooper, Harvey and Kennedy), over reverse postorder
//! from the entry block.

use super::ir::*;

pub struct Dom {
    /// Reachable built blocks in reverse postorder.
    pub rpo: Vec<B>,
    /// Each block's position in `rpo` (`usize::MAX` when unreachable).
    pub index: Vec<usize>,
    /// Immediate dominator of each `rpo` position (the entry dominates itself).
    pub idom: Vec<usize>,
}

impl Dom {
    pub fn new(g: &Graph) -> Dom {
        let n = g.blocks.len();
        let built = |b: B| !matches!(g.blocks[b as usize].term, Term::None);
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
                return Dom { rpo, index, idom };
            }
        }
    }

    /// Whether block `a` dominates block `b` (both reachable).
    pub fn dominates(&self, a: B, b: B) -> bool {
        let (a, mut b) = (self.index[a as usize], self.index[b as usize]);
        if a == usize::MAX || b == usize::MAX {
            return false;
        }
        while b > a {
            b = self.idom[b];
        }
        a == b
    }
}
