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
        for state in g.loop_entry.values() {
            reads.of(g, state).iter().for_each(|&v| mark(v, &mut used));
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
        Inst::Bin(..) | Inst::ArrayLength(_) | Inst::LoadIndexed(..) => true,
        Inst::LoadStatic(..) => !g.ordered.contains(&v),
        Inst::LoadField(..) => !g.ordered.contains(&v),
        Inst::Intrinsic(k, _, None) => k.is_pure(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::eliminate;
    use crate::jit::opt::ir::{Graph, Inst, State, Term, Ty};
    use crate::klass::{Klass, KlassKind, KlassParts, ACC_STATIC};
    use crate::method::Method;
    use crate::symbol::Symbol;
    use crate::types::BasicType;
    use std::rc::Rc;

    fn test_method() -> &'static Method {
        let holder = crate::klass::leak(Klass::new(KlassParts {
            name: Symbol::intern("DceTest"),
            kind: KlassKind::Instance,
            access: 0,
            super_klass: None,
            interfaces: Vec::new(),
            cf: None,
            fields: Vec::new(),
            instance_size: 0,
            ref_offsets: Vec::new(),
            static_slots: 0,
            initialized: true,
            hidden: false,
        }));
        crate::low::leak(Method::new(holder, Symbol::intern("test"), Symbol::intern("()V"), ACC_STATIC, None, false))
    }

    #[test]
    fn dce_keeps_ordered_static_reads_and_removes_plain_reads() {
        let mut g = Graph::default();
        let block = g.new_block(0);
        let ordered = g.add(block, Inst::LoadStatic(0, BasicType::Int), Ty::I);
        let plain = g.add(block, Inst::LoadStatic(8, BasicType::Int), Ty::I);
        g.ordered.insert(ordered);
        g.blocks[block as usize].term = Term::Return(None);

        eliminate(&mut g);

        assert!(g.blocks[block as usize].insts.contains(&ordered));
        assert!(!g.blocks[block as usize].insts.contains(&plain));
    }

    #[test]
    fn dce_keeps_value_used_by_loop_entry_state() {
        let mut g = Graph::default();
        let block = g.new_block(0);
        let method = test_method();
        let value = g.add(block, Inst::Const(42), Ty::I);
        g.loop_entry.insert(
            block,
            Rc::new(State {
                method,
                bci: 1,
                locals: vec![Some(value)],
                stack: Vec::new(),
                caller: None,
                frame: 0,
            }),
        );
        g.blocks[block as usize].term = Term::Return(None);

        eliminate(&mut g);

        assert!(g.blocks[block as usize].insts.contains(&value));
    }
}
