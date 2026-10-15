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
            break;
        }
    }
    fold_webs(g);
}

/// Phis reached from one another through their inputs: when everything entering the group
/// from outside is one value, every phi of it is that value (Braun et al.'s SCC rule). An
/// on-stack-replacement entry carries each local into both an inner and an outer loop header,
/// whose phis feed each other: a quaternion an inner loop only reads stayed a phi, and so did
/// every load of its fields.
fn fold_webs(g: &mut Graph) {
    const MAX_WEB: usize = 64;
    for v in 0..g.values.len() as V {
        if g.alias[v as usize] != v || !matches!(g.values[v as usize].inst, Inst::Phi(_)) {
            continue;
        }
        let (mut web, mut stack, mut outside) = (vec![v], vec![v], None);
        let mut ok = true;
        'walk: while let Some(p) = stack.pop() {
            let Inst::Phi(inputs) = &g.values[p as usize].inst else { unreachable!() };
            for &i in inputs {
                let i = source(g, i);
                if web.contains(&i) {
                    continue;
                }
                if matches!(g.values[i as usize].inst, Inst::Phi(_)) {
                    if web.len() == MAX_WEB {
                        ok = false;
                        break 'walk;
                    }
                    web.push(i);
                    stack.push(i);
                } else if *outside.get_or_insert(i) != i {
                    ok = false;
                    break 'walk;
                }
            }
        }
        if let (true, Some(x)) = (ok, outside) {
            for p in web {
                g.alias[p as usize] = x;
            }
        }
    }
}

/// The value `v` stands for: aliases resolved, copies looked through.
fn source(g: &Graph, v: V) -> V {
    let mut v = g.resolve(v);
    while let Inst::Copy(x) = g.values[v as usize].inst {
        v = g.resolve(x);
    }
    v
}
