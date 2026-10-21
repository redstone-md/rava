//! `l2i` results read only as 32-bit values: every instruction using one reads its low half
//! (32-bit arithmetic, comparisons and branches, conversions from int, int-or-narrower field
//! and element stores). Such an `l2i` needs no sign extension (`movsxd`): it is a move, or
//! nothing when the result shares its source's register. One read as a 64-bit value (an array
//! index, a phi, an argument, a deoptimization state) keeps it.

use super::super::ir::*;
use crate::types::BasicType;
use std::collections::HashSet;

pub(super) fn low_half_only(g: &Graph) -> HashSet<V> {
    let reads = super::super::thread::reads(g);
    let mut cand: HashSet<V> = HashSet::new();
    for blk in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for &v in &blk.insts {
            if g.resolve(v) == v && matches!(g.inst(v), Inst::Convert(0x88, _)) {
                cand.insert(v);
            }
        }
    }
    if cand.is_empty() {
        return cand;
    }
    // Reads by the instructions that take only the low half, per value.
    let mut low = vec![0u32; g.values.len()];
    for blk in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for &u in &blk.insts {
            if g.resolve(u) != u {
                continue;
            }
            let ok: Vec<V> = match *g.inst(u) {
                Inst::Bin(_, x, y) if g.ty(u) == Ty::I => vec![x, y],
                Inst::Compare(..) => Vec::new(),
                // i2l re-extends what it reads; i2f, i2d, i2b, i2c, i2s read the low half.
                Inst::Convert(0x85 | 0x86 | 0x87 | 0x91 | 0x92 | 0x93, x) => vec![x],
                Inst::StoreField(_, _, BasicType::Int | BasicType::Short | BasicType::Char | BasicType::Byte | BasicType::Boolean, x) => vec![x],
                Inst::StoreIndexed(_, _, BasicType::Int | BasicType::Short | BasicType::Char | BasicType::Byte | BasicType::Boolean, x) => vec![x],
                _ => Vec::new(),
            };
            // The value operand only: an object, array or index operand is no low-half read.
            for x in ok {
                let x = g.resolve(x);
                if cand.contains(&x) {
                    low[x as usize] += 1;
                }
            }
        }
        if let Term::If { ty: Ty::I, a, b, .. } = blk.term {
            for x in [a, b] {
                let x = g.resolve(x);
                if cand.contains(&x) {
                    low[x as usize] += 1;
                }
            }
        }
    }
    cand.retain(|&v| reads[v as usize] > 0 && low[v as usize] == reads[v as usize]);
    cand
}
