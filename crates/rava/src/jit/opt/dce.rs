//! Dead code elimination: instructions without side effects whose value nothing reads (no
//! instruction, terminator, phi or deoptimization state) are dropped, repeatedly, so what
//! earlier passes left behind (a class word loaded for a test that became a guard before the
//! loop, a copy no use took) is not computed. Null checks stay separate instructions, so a
//! dropped load drops no check; volatile reads stay for their ordering.

use super::ir::*;
use super::liveness::StateReads;

pub fn eliminate(g: &mut Graph) {
    loop {
        let mut used = vec![false; g.values.len()];
        let mut reads = StateReads::new();
        let mark = |v: V, used: &mut Vec<bool>| used[g.resolve(v) as usize] = true;
        for b in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
            for u in b.term.inputs() {
                mark(u, &mut used);
            }
            if let Term::Deopt(s) = &b.term {
                reads.of(g, s).iter().for_each(|&u| used[u as usize] = true);
            }
            for &v in &b.insts {
                let inst = g.inst(v);
                match inst {
                    Inst::Phi(ins) => ins.iter().for_each(|&u| mark(u, &mut used)),
                    _ => inst.inputs().into_iter().for_each(|u| mark(u, &mut used)),
                }
                if let Some(s) = inst.state() {
                    reads.of(g, s).iter().for_each(|&u| used[u as usize] = true);
                }
            }
        }
        let mut removed = false;
        for b in 0..g.blocks.len() {
            let before = g.blocks[b].insts.len();
            let insts = std::mem::take(&mut g.blocks[b].insts);
            let kept: Vec<V> = insts.into_iter().filter(|&v| used[v as usize] || g.resolve(v) != v || !removable(g, v)).collect();
            removed |= kept.len() != before;
            g.blocks[b].insts = kept;
        }
        if !removed {
            return;
        }
    }
}

/// Computes a value and nothing else.
fn removable(g: &Graph, v: V) -> bool {
    match g.inst(v) {
        Inst::Const(_) | Inst::Copy(_) | Inst::Neg(_) | Inst::Convert(..) | Inst::Compare(..) | Inst::Select(..) => true,
        Inst::Bin(..) | Inst::ArrayLength(_) | Inst::LoadIndexed(..) | Inst::LoadStatic(..) => true,
        Inst::LoadField(..) => !g.ordered.contains(&v),
        Inst::Intrinsic(k, _, None) => k.is_pure(),
        _ => false,
    }
}
