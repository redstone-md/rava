//! Phi simplification (C1's `PhiSimplifier`): a phi whose inputs are one value and itself (a
//! local a loop never changes, or equal values merging) is that value. Such phis become
//! aliases: they get no location of their own, and every use reads the value they stand for.

use super::ir::*;

pub fn simplify(g: &mut Graph) {
    g.alias = (0..g.values.len() as V).collect();
    fold_trivial(g);
}

/// `simplify` keeping the aliases other passes already made.
pub fn fold_trivial(g: &mut Graph) {
    loop {
        let mut changed = false;
        for v in 0..g.values.len() as V {
            let Inst::Phi(inputs) = &g.values[v as usize].inst else { continue };
            if g.alias[v as usize] != v {
                continue;
            }
            let mut only = None;
            let trivial = inputs.iter().map(|&i| g.resolve(i)).filter(|&i| i != v).all(|i| *only.get_or_insert(i) == i);
            if let (true, Some(x)) = (trivial, only) {
                g.alias[v as usize] = x;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}
