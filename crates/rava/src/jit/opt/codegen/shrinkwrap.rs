//! A deliberately small callee-saved-register shrink-wrap plan.
//!
//! One straight-line integer return block may save a register only when all of its allocated
//! values are defined and consumed in that block. Keeping the register's incoming value in a
//! fixed spill slot leaves RSP unchanged and lets all paths that skip the block leave the
//! register untouched.

use super::super::ir::{BinOp, Graph, Inst, Term, Ty, B, V};
use super::super::regalloc::{Allocation, Loc, CALLEE_SAVED};
use crate::jit::asm::{mem, Asm, R};
use std::collections::HashSet;

const MAX_BLOCK_INSTS: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) block: B,
    pub(super) reg: R,
    pub(super) slot: i32,
}

impl Plan {
    pub(super) fn save(self, a: &mut Asm) {
        a.store(true, mem(R::Rsp, 8 * self.slot), self.reg);
    }

    pub(super) fn restore(self, a: &mut Asm) {
        a.load(true, self.reg, mem(R::Rsp, 8 * self.slot));
    }
}

pub(super) fn spill_bytes(spill_slots: i32, reserve: bool) -> i32 {
    let slots = spill_slots + i32::from(reserve);
    8 * (slots + (slots & 1))
}

pub(super) fn analyze(g: &Graph, al: &Allocation, enabled: bool) -> Option<Plan> {
    if !enabled || al.loc.len() != g.values.len() {
        return None;
    }

    for &block in &al.order {
        if block == 0 || block as usize >= g.blocks.len() {
            continue;
        }
        let blk = &g.blocks[block as usize];
        if blk.loop_header
            || blk.preds.contains(&block)
            || blk.insts.is_empty()
            || blk.insts.len() > MAX_BLOCK_INSTS
        {
            continue;
        }
        let Term::Return(Some(result)) = blk.term else {
            continue;
        };
        if !matches!(g.ty(g.resolve(result)), Ty::I | Ty::J)
            || blk.insts.iter().any(|&v| !safe_int_inst(g, v))
        {
            continue;
        }

        for &reg in &CALLEE_SAVED {
            let roots: HashSet<V> = (0..g.values.len() as V)
                .filter(|&v| g.resolve(v) == v && al.loc[v as usize] == Loc::Reg(reg))
                .collect();
            if roots.is_empty()
                || roots.iter().any(|&v| definition_block(g, v) != Some(block))
                || uses_outside(g, block, &roots)
            {
                continue;
            }
            return Some(Plan {
                block,
                reg,
                slot: al.spill_slots,
            });
        }
    }
    None
}

fn safe_int_inst(g: &Graph, v: V) -> bool {
    if g.resolve(v) != v || !matches!(g.ty(v), Ty::I | Ty::J) {
        return false;
    }
    let integer = |x: V| matches!(g.ty(g.resolve(x)), Ty::I | Ty::J);
    match g.inst(v) {
        Inst::Const(_) => true,
        Inst::Neg(x) => integer(*x),
        Inst::Bin(op, x, y) => {
            integer(*x)
                && integer(*y)
                && matches!(
                    op,
                    BinOp::Add
                        | BinOp::Sub
                        | BinOp::Mul
                        | BinOp::And
                        | BinOp::Or
                        | BinOp::Xor
                        | BinOp::Shl
                        | BinOp::Shr
                        | BinOp::Ushr
                )
        }
        _ => false,
    }
}

fn definition_block(g: &Graph, value: V) -> Option<B> {
    g.blocks.iter().enumerate().find_map(|(b, blk)| {
        blk.insts
            .iter()
            .any(|&v| g.resolve(v) == value)
            .then_some(b as B)
    })
}

fn uses_outside(g: &Graph, candidate: B, roots: &HashSet<V>) -> bool {
    let mentions = |values: Vec<V>| values.into_iter().any(|v| roots.contains(&g.resolve(v)));
    for (b, blk) in g.blocks.iter().enumerate() {
        if b as B == candidate {
            continue;
        }
        for &v in &blk.insts {
            let inst = g.inst(v);
            if mentions(inst.inputs()) {
                return true;
            }
            if let Some(state) = inst.state() {
                let mut values = Vec::new();
                super::super::liveness::state_values(g, state, &mut values);
                if mentions(values) {
                    return true;
                }
            }
        }
        if mentions(blk.term.inputs()) {
            return true;
        }
        if let Term::Deopt(state) = &blk.term {
            let mut values = Vec::new();
            super::super::liveness::state_values(g, state, &mut values);
            if mentions(values) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::opt::ir::Graph;
    use std::collections::HashMap;

    fn allocation(g: &Graph, locations: &[(V, Loc)]) -> Allocation {
        let mut loc = vec![Loc::None; g.values.len()];
        for &(v, at) in locations {
            loc[v as usize] = at;
        }
        Allocation {
            loc,
            order: (0..g.blocks.len() as B).collect(),
            spill_slots: 2,
            saves: HashMap::new(),
            state_saves: HashMap::new(),
            save_base: 2,
            live_at: HashMap::new(),
            live_at_end: vec![(0u16, 0u16); g.blocks.len()],
            home: vec![None; g.values.len()],
            state_regs: vec![false; g.values.len()],
        }
    }

    fn simple_return() -> (Graph, Allocation, B, V) {
        let mut g = Graph::default();
        let entry = g.new_block(0);
        let block = g.new_block(1);
        let x = g.add(block, Inst::Const(20), Ty::I);
        let y = g.add(block, Inst::Const(22), Ty::I);
        let result = g.add(block, Inst::Bin(BinOp::Add, x, y), Ty::I);
        g.blocks[entry as usize].term = Term::Goto(block);
        g.blocks[block as usize].preds.push(entry);
        g.blocks[block as usize].term = Term::Return(Some(result));
        let al = allocation(
            &g,
            &[
                (x, Loc::Const(20)),
                (y, Loc::Const(22)),
                (result, Loc::Reg(R::Rbx)),
            ],
        );
        (g, al, block, result)
    }

    #[test]
    fn accepts_a_bounded_non_entry_integer_return_block() {
        let (g, al, block, _) = simple_return();
        assert_eq!(
            analyze(&g, &al, true),
            Some(Plan {
                block,
                reg: R::Rbx,
                slot: 2
            })
        );
        assert_eq!(spill_bytes(0, true), 16);
        assert_eq!(spill_bytes(1, true), 16);
        assert_eq!(spill_bytes(2, true), 32);
        assert_eq!(spill_bytes(3, true), 32);
    }

    #[test]
    fn save_restore_use_the_reserved_rsp_slot() {
        let plan = Plan {
            block: 1,
            reg: R::Rbx,
            slot: 2,
        };
        let mut asm = Asm::default();
        plan.save(&mut asm);
        asm.mov_ri(R::Rbx, 0);
        plan.restore(&mut asm);
        let bytes = asm.finish().bytes;
        assert!(bytes.starts_with(&[0x48, 0x89, 0x5c, 0x24, 0x10]));
        assert!(bytes.ends_with(&[0x48, 0x8b, 0x5c, 0x24, 0x10]));
    }

    #[test]
    fn rejects_abi_entries_and_register_values_shared_across_blocks() {
        let (g, al, _, _) = simple_return();
        assert_eq!(analyze(&g, &al, false), None);

        let mut g = g;
        let other = g.new_block(2);
        let x = g.add(other, Inst::Const(1), Ty::I);
        let y = g.add(other, Inst::Const(2), Ty::I);
        let shared = g.add(other, Inst::Bin(BinOp::Add, x, y), Ty::I);
        g.blocks[other as usize].term = Term::Return(Some(shared));
        let mut al = allocation(
            &g,
            &[
                (x, Loc::Const(1)),
                (y, Loc::Const(2)),
                (shared, Loc::Reg(R::Rbx)),
            ],
        );
        let result = match &g.blocks[1].term {
            Term::Return(Some(v)) => *v,
            _ => unreachable!(),
        };
        al.loc[result as usize] = Loc::Reg(R::Rbx);
        assert_eq!(analyze(&g, &al, true), None);
    }

    #[test]
    fn rejects_phi_loop_trapping_and_non_integer_shapes() {
        let (mut g, al, block, _) = simple_return();
        g.blocks[block as usize].loop_header = true;
        assert_eq!(analyze(&g, &al, true), None);

        let mut g = Graph::default();
        let entry = g.new_block(0);
        let block = g.new_block(1);
        let input = g.add(entry, Inst::Const(20), Ty::I);
        let phi = g.add(block, Inst::Phi(vec![input]), Ty::I);
        let one = g.add(block, Inst::Const(1), Ty::I);
        let result = g.add(block, Inst::Bin(BinOp::Add, phi, one), Ty::I);
        g.blocks[entry as usize].term = Term::Goto(block);
        g.blocks[block as usize].preds.push(entry);
        g.blocks[block as usize].term = Term::Return(Some(result));
        let al = allocation(
            &g,
            &[
                (input, Loc::Const(20)),
                (phi, Loc::Reg(R::Rbx)),
                (one, Loc::Const(1)),
                (result, Loc::Reg(R::Rbx)),
            ],
        );
        assert_eq!(analyze(&g, &al, true), None);

        let (mut g, mut al, block, result) = simple_return();
        let x = g.add(block, Inst::Const(1), Ty::I);
        let y = g.add(block, Inst::Const(0), Ty::I);
        let div = g.add(block, Inst::Bin(BinOp::Div, x, y), Ty::I);
        g.blocks[block as usize].term = Term::Return(Some(div));
        al.loc.resize(g.values.len(), Loc::None);
        al.loc[div as usize] = Loc::Reg(R::Rbx);
        assert_eq!(analyze(&g, &al, true), None);
        assert_ne!(result, div);
    }

    #[test]
    fn rejects_register_values_read_from_another_block() {
        let (mut g, mut al, block, result) = simple_return();
        let outside = g.new_block(2);
        let one = g.add(outside, Inst::Const(1), Ty::I);
        g.add(outside, Inst::Bin(BinOp::Add, result, one), Ty::I);
        g.blocks[outside as usize].term = Term::Return(None);
        al.loc.resize(g.values.len(), Loc::None);
        al.loc[one as usize] = Loc::Const(1);
        assert_eq!(analyze(&g, &al, true), None);
        assert_eq!(definition_block(&g, result), Some(block));
    }
}
