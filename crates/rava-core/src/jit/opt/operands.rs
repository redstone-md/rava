//! Operand access for code generation: reading a value from wherever it was allocated
//! (register, SSE register, spill slot or constant) and writing a result back.

use super::codegen::Gen;
use super::ir::V;
use super::moves::{self, spill};
use super::regalloc::Loc;
use crate::jit::asm::{Alu, M, R, X};

impl Gen<'_> {
    /// `dst := v`.
    pub fn load(&mut self, dst: R, v: V) {
        moves::mov(&mut self.a, Loc::Reg(dst), self.loc[v as usize]);
    }

    /// The register holding `v`, or `scratch` loaded with it.
    pub fn reg(&mut self, v: V, scratch: R) -> R {
        match self.loc[v as usize] {
            Loc::Reg(r) => r,
            _ => {
                self.load(scratch, v);
                scratch
            }
        }
    }

    /// The register to compute `v` into: its own, or rax when it is spilled.
    pub fn out(&self, v: V) -> R {
        match self.loc[v as usize] {
            Loc::Reg(r) => r,
            _ => R::Rax,
        }
    }

    /// Moves a computed result from `src` to the location of `v`.
    pub fn put(&mut self, v: V, src: R) {
        let dst = self.loc[v as usize];
        if dst != Loc::None {
            moves::mov(&mut self.a, dst, Loc::Reg(src));
        }
    }

    /// The SSE register holding `v`, or `scratch` loaded with it (constants go through r11).
    pub fn xreg(&mut self, v: V, scratch: X) -> X {
        match self.loc[v as usize] {
            Loc::Xmm(x) => X(x),
            _ => {
                self.xload(scratch, v);
                scratch
            }
        }
    }

    /// `dst := v` for a float or double value.
    pub fn xload(&mut self, dst: X, v: V) {
        match self.loc[v as usize] {
            Loc::Const(c) => {
                self.a.mov_ri(R::R11, c);
                self.a.mov_xr(true, dst, R::R11);
            }
            l => moves::mov(&mut self.a, Loc::Xmm(dst.0), l),
        }
    }

    /// The SSE register to compute `v` into: its own, or xmm0 when it is spilled.
    pub fn xout(&self, v: V) -> X {
        match self.loc[v as usize] {
            Loc::Xmm(x) => X(x),
            _ => X(0),
        }
    }

    /// Moves a computed float or double result from `src` to the location of `v`.
    pub fn xput(&mut self, v: V, src: X) {
        let dst = self.loc[v as usize];
        if dst != Loc::None {
            moves::mov(&mut self.a, dst, Loc::Xmm(src.0));
        }
    }

    /// A bit mask (sign or magnitude) in xmm1.
    pub fn xmask(&mut self, bits: i64) {
        self.a.mov_ri(R::R11, bits);
        self.a.mov_xr(true, X(1), R::R11);
    }

    /// Stores `v` (slot encoding) into the memory at `m`.
    pub fn store_val(&mut self, m: M, v: V) {
        match self.loc[v as usize] {
            Loc::Reg(r) => self.a.store(true, m, r),
            Loc::Xmm(x) => self.a.fstore(true, m, X(x)),
            Loc::Const(c) if i32::try_from(c).is_ok() => self.a.store_imm(m, c as i32),
            Loc::None => {}
            _ => {
                self.load(R::Rax, v);
                self.a.store(true, m, R::Rax);
            }
        }
    }

    /// `dst op= v`.
    pub fn alu_val(&mut self, w: bool, op: Alu, dst: R, v: V) {
        match self.loc[v as usize] {
            Loc::Reg(r) => self.a.alu_rr(w, op, dst, r),
            Loc::Stack(s) => self.a.alu_rm(w, op, dst, spill(s)),
            Loc::Const(c) if i32::try_from(c).is_ok() => self.a.alu_ri(w, op, dst, c as i32),
            _ => {
                self.load(R::R11, v);
                self.a.alu_rr(w, op, dst, R::R11);
            }
        }
    }
}
