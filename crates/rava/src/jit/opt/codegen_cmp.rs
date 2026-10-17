//! Code generation of comparisons: `ucomis` of float values, the three-way compares and
//! conditional selects (`select.rs`).

use super::codegen::Gen;
use super::regalloc::Loc;
use super::ir::*;
use crate::jit::asm::{Alu, Cond, R, X};

impl Gen<'_> {
    /// `ucomis a, y`, with `y` from memory when it is a constant, spilled or a parameter.
    pub(super) fn fcmp_val(&mut self, double: bool, a: X, y: V) {
        let m = match self.loc[y as usize] {
            Loc::Const(c) => Some(self.const_mem(c)),
            Loc::Stack(s) => Some(super::moves::spill(s)),
            Loc::Frame(s) => Some(self.slot(s)),
            _ => None,
        };
        match m {
            Some(m) => self.a.fcmp_m(double, a, m),
            None => {
                let b = self.xreg(y, X(1));
                self.a.fcmp(double, a, b);
            }
        }
    }

    /// `a cmp b ? t : f`: compare, then the false value into rax and a conditional move of the
    /// true one (moves and loads leave the flags alone).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn select(&mut self, v: V, cmp: Cmp, cty: Ty, a: V, b: V, t: V, f: V) {
        let c = if matches!(cty, Ty::F | Ty::D) {
            self.order(cty == Ty::D, cmp, a, b)
        } else {
            let ra = self.reg(a, R::Rax);
            self.alu_val(cty != Ty::I, Alu::Cmp, ra, b);
            super::codegen::cond(cmp)
        };
        // Straight into the result's register (moves keep the flags): the false value, then the
        // true one where the condition holds; when that register already holds the true value,
        // the false one where it fails.
        let out = self.out(v);
        if self.loc[t as usize] == Loc::Reg(out) {
            let rf = self.reg(f, R::R11);
            if rf != out {
                self.a.cmov(true, super::codegen::invert(c), out, rf);
            }
        } else {
            self.load(out, f);
            let rt = self.reg(t, R::R11);
            self.a.cmov(true, c, out, rt);
        }
        self.put(v, out);
    }

    /// Compares floats `a` and `b` by `cmp` (an order), returning the condition that holds
    /// exactly when it does, false for NaN: `ucomis` sets CF, ZF and PF for unordered operands,
    /// so "above" and "above or equal" fail then, with the operands swapped for `<` and `<=`.
    fn order(&mut self, double: bool, cmp: Cmp, a: V, b: V) -> Cond {
        let (x, y) = if matches!(cmp, Cmp::Lt | Cmp::Le) { (b, a) } else { (a, b) };
        let rx = self.xreg(x, X(0));
        self.fcmp_val(double, rx, y);
        if matches!(cmp, Cmp::Lt | Cmp::Gt) { Cond::A } else { Cond::Ae }
    }

    /// lcmp / fcmpl / fcmpg / dcmpl / dcmpg: -1, 0 or 1.
    pub(super) fn compare(&mut self, v: V, op: u8, x: V, y: V) {
        // Through rax and r11 (`scratch.rs`).
        if op == 0x94 {
            let rx = self.reg(x, R::Rax);
            self.alu_val(true, Alu::Cmp, rx, y);
            self.a.setcc(Cond::G, R::Rax);
            self.a.setcc(Cond::L, R::R11);
        } else {
            let double = op >= 0x97;
            let nan = if op == 0x95 || op == 0x97 { -1 } else { 1 };
            let a = self.xreg(x, X(0));
            let (ordered, done) = (self.a.new_label(), self.a.new_label());
            self.fcmp_val(double, a, y);
            self.a.jcc(Cond::Np, ordered);
            self.a.mov_ri(R::Rax, nan);
            self.a.jmp(done);
            self.a.bind(ordered);
            self.a.setcc(Cond::A, R::Rax);
            self.a.setcc(Cond::B, R::R11);
            self.finish_compare();
            self.a.bind(done);
            return self.put(v, R::Rax);
        }
        self.finish_compare();
        self.put(v, R::Rax);
    }

    /// rax := (al) - (r11b) as 32-bit values.
    fn finish_compare(&mut self) {
        self.a.movzx8(R::Rax, R::Rax);
        self.a.movzx8(R::R11, R::R11);
        self.a.alu_rr(false, Alu::Sub, R::Rax, R::R11);
    }
}
