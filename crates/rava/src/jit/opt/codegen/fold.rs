//! Loads folded into the instruction that reads them (C2's memory operands): a double or float
//! field or element read only by the arithmetic right after it is that instruction's operand,
//! `addsd xmm, [obj + off]`; an int one read only by its conversion is `cvtsi2sd xmm, dword
//! [obj + off]`, and an int or long one read only by integer arithmetic its operand as well
//! (`add r32, [obj + off]`). The load emits nothing; nothing runs in between that could write
//! the memory, and no state reads the loaded value.

use super::super::ir::*;
use super::Gen;
use crate::jit::asm::M;
use crate::types::BasicType;
use std::collections::HashSet;

/// The loads to fold: each the sole read of the instruction after it (constants aside).
pub(super) fn folded_loads(g: &Graph) -> HashSet<V> {
    let reads = super::super::thread::reads(g);
    let mut out = HashSet::new();
    for blk in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        let real: Vec<V> = blk.insts.iter().copied().filter(|&x| g.resolve(x) == x && !matches!(g.inst(x), Inst::Const(_))).collect();
        for (i, w) in real.windows(2).enumerate() {
            let (l, u) = (w[0], w[1]);
            if reads[l as usize] != 1 {
                continue;
            }
            let (object, bt) = match *g.inst(l) {
                Inst::LoadField(o, _, bt) => (g.resolve(o), bt),
                Inst::LoadIndexed(a, _, bt) => (g.resolve(a), bt),
                _ => continue,
            };
            // Not the access an implicit null check of the object leaves its test to: the
            // instruction writes its result before the access would fault.
            if i > 0 && matches!(*g.inst(real[i - 1]), Inst::NullCheck(o, _) if g.resolve(o) == object) {
                continue;
            }
            if folds_into(g, l, bt, u) {
                out.insert(l);
            }
        }
    }
    out
}

/// Whether instruction `u` can read load `l` (of `bt`) from memory.
fn folds_into(g: &Graph, l: V, bt: BasicType, u: V) -> bool {
    let is = |x: V| g.resolve(x) == l;
    match *g.inst(u) {
        Inst::Bin(BinOp::Add | BinOp::Mul, x, y) if matches!(bt, BasicType::Double | BasicType::Float) => is(x) != is(y) && float_of(g, u, bt),
        Inst::Bin(BinOp::Sub | BinOp::Div, x, y) if matches!(bt, BasicType::Double | BasicType::Float) => is(y) && !is(x) && float_of(g, u, bt),
        Inst::Bin(BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor, x, y) if int_of(g, u, bt) => is(x) != is(y),
        Inst::Bin(BinOp::Sub, x, y) if int_of(g, u, bt) => is(y) && !is(x),
        // i2f, i2d.
        Inst::Convert(0x86 | 0x87, x) => bt == BasicType::Int && is(x),
        _ => false,
    }
}

/// Whether `u` is int or long arithmetic on a load of its own width.
fn int_of(g: &Graph, u: V, bt: BasicType) -> bool {
    matches!((g.ty(u), bt), (Ty::I, BasicType::Int) | (Ty::J, BasicType::Long))
}

/// Whether `u` computes in the load's own precision.
fn float_of(g: &Graph, u: V, bt: BasicType) -> bool {
    g.ty(u) == if bt == BasicType::Double { Ty::D } else { Ty::F }
}

impl Gen<'_> {
    /// Whether `v` (an alias resolved) is a folded load.
    pub(in super::super) fn is_folded(&self, v: V) -> bool {
        self.folded.contains(&self.g.resolve(v))
    }

    /// Whether folded load `l`'s address needs no register but its object's and index's, none
    /// of them `r`: code may then fill `r` before the access.
    pub(in super::super) fn folded_apart(&self, l: V, r: crate::jit::asm::R) -> bool {
        use super::super::regalloc::Loc;
        let held = |x: V| matches!(self.loc[self.g.resolve(x) as usize], Loc::Reg(q) if q != r);
        match *self.g.inst(self.g.resolve(l)) {
            Inst::LoadField(o, ..) => held(o),
            Inst::LoadIndexed(a, i, _) => held(a) && (held(i) || matches!(self.loc[self.g.resolve(i) as usize], Loc::Const(_))),
            _ => false,
        }
    }

    /// The address folded load `l` reads (its object and index loaded first, through rax and r11).
    pub(in super::super) fn folded_mem(&mut self, l: V) -> M {
        match *self.g.inst(self.g.resolve(l)) {
            Inst::LoadField(o, off, _) => self.field(o, off as i32),
            Inst::LoadIndexed(a, i, bt) => self.element(a, i, bt),
            _ => unreachable!("a folded load"),
        }
    }
}
