//! Dominators of the graph's built blocks (Cooper, Harvey and Kennedy), over reverse postorder
//! from the entry block.

use super::ir::*;

pub struct Dom {
    /// Reachable built blocks in reverse postorder.
    pub rpo: Vec<B>,
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
                return Dom { rpo, idom };
            }
        }
    }
}

/// A natural loop: its header and body (the header included), by block.
pub struct Loop {
    pub header: B,
    pub body: Vec<bool>,
}

impl Dom {
    /// Position of each block in `rpo` (`usize::MAX` for unreachable ones).
    pub fn index(&self, blocks: usize) -> Vec<usize> {
        let mut index = vec![usize::MAX; blocks];
        for (i, &b) in self.rpo.iter().enumerate() {
            index[b as usize] = i;
        }
        index
    }

    /// Whether the block at rpo position `h` dominates the one at `i`.
    pub fn dominates(&self, h: usize, mut i: usize) -> bool {
        loop {
            if i == h {
                return true;
            }
            if i == 0 || self.idom[i] == usize::MAX {
                return false;
            }
            i = self.idom[i];
        }
    }

    /// The natural loops, one per header (back edges: edges to a block that dominates their
    /// source), outermost headers first.
    pub fn loops(&self, g: &Graph) -> Vec<Loop> {
        let n = g.blocks.len();
        let index = self.index(n);
        let mut loops = Vec::new();
        for (hi, &h) in self.rpo.iter().enumerate() {
            let tails: Vec<B> = g.blocks[h as usize].preds.iter().copied().filter(|&p| index[p as usize] != usize::MAX && self.dominates(hi, index[p as usize])).collect();
            if tails.is_empty() {
                continue;
            }
            // The body: the header and every block reaching a tail without passing it.
            let mut body = vec![false; n];
            body[h as usize] = true;
            let mut work = tails;
            while let Some(b) = work.pop() {
                if !std::mem::replace(&mut body[b as usize], true) {
                    work.extend(g.blocks[b as usize].preds.iter().copied().filter(|&p| index[p as usize] != usize::MAX));
                }
            }
            loops.push(Loop { header: h, body });
        }
        loops
    }
}
