//! Machine-code emission for vectorized primitive array fills.

use super::codegen::Gen;
use super::ir::*;
use crate::jit::asm::{mem, mem_idx, Alu, Cond, R, X};
use crate::klass::Klass;
use crate::types::BasicType;

impl Gen<'_> {
    /// `a[from..to] = x` for elements of `1 << shift` bytes: xmm0 holds the value in every lane,
    /// the body writes sixteen bytes at a time, and a final overlapping store covers the tail.
    /// Ranges shorter than sixteen bytes use element-sized stores.
    pub(super) fn fill_range(&mut self, shift: u8, args: &[V]) {
        let wide = shift == 3;
        let lanes = if wide { 0x44 } else { 0 };
        if shift == 0 || shift == 1 {
            let r = self.reg(args[3], R::Rdx);
            self.a.mov_rr(false, R::Rdx, r);
            if shift == 0 {
                // bastore targets byte[] and boolean[] through the same opcode. A byte keeps
                // eight low bits, while boolean keeps only the low bit, so select by klass.
                let arr = self.reg(args[0], R::Rax);
                let bools = crate::vm::vm()
                    .primitive_array_class(BasicType::Boolean)
                    .ok()
                    .map_or(0, |k| k as *const Klass as i64);
                let (boolean, ready) = (self.a.new_label(), self.a.new_label());
                if bools != 0 {
                    self.a.mov_ri(R::R11, bools);
                    self.a.alu_rm(false, Alu::Cmp, R::R11, mem(arr, 0));
                    self.a.jcc(Cond::E, boolean);
                }
                self.a.alu_ri(false, Alu::And, R::Rdx, 0xff);
                self.a.imul_rri(false, R::Rdx, R::Rdx, 0x0101_0101);
                self.a.jmp(ready);
                self.a.bind(boolean);
                self.a.alu_ri(false, Alu::And, R::Rdx, 1);
                self.a.imul_rri(false, R::Rdx, R::Rdx, 0x0101_0101);
                self.a.bind(ready);
            } else {
                // Repeat the low sixteen bits twice in each dword; sastore truncates to a word.
                self.a.alu_ri(false, Alu::And, R::Rdx, 0xffff);
                self.a.imul_rri(false, R::Rdx, R::Rdx, 0x0001_0001);
            }
            self.a.mov_xr(false, X(0), R::Rdx);
            self.a.pshufd(X(0), X(0), 0);
        } else if matches!(self.g.ty(args[3]), Ty::F | Ty::D) {
            let x = self.xreg(args[3], X(0));
            self.a.pshufd(X(0), x, lanes);
        } else {
            let r = self.reg(args[3], R::Rdx);
            self.a.mov_xr(wide, X(0), r);
            self.a.pshufd(X(0), X(0), lanes);
        }
        let ra = self.reg(args[0], R::Rax);
        let rf = self.reg(args[1], R::Rcx);
        self.a.movsxd(R::Rcx, rf);
        let rt = self.reg(args[2], R::Rdx);
        self.a.movsxd(R::Rdx, rt);
        let base = crate::heap::ARRAY_BASE_OFFSET as i32;
        self.a.lea(R::R11, mem_idx(ra, R::Rdx, 1 << shift, base));
        self.a.lea(R::Rax, mem_idx(ra, R::Rcx, 1 << shift, base));
        self.a.mov_rr(true, R::Rcx, R::R11);
        self.a.alu_rr(true, Alu::Sub, R::Rcx, R::Rax);
        let (small, vector, done) = (self.a.new_label(), self.a.new_label(), self.a.new_label());
        self.a.alu_ri(true, Alu::Cmp, R::Rcx, 16);
        self.a.jcc(Cond::B, small);
        self.a.alu_ri(true, Alu::Sub, R::R11, 16);
        self.a.bind(vector);
        self.a.store_x128(mem(R::Rax, 0), X(0));
        self.a.alu_ri(true, Alu::Add, R::Rax, 16);
        self.a.alu_rr(true, Alu::Cmp, R::Rax, R::R11);
        self.a.jcc(Cond::B, vector);
        self.a.store_x128(mem(R::R11, 0), X(0));
        self.a.jmp(done);
        self.a.bind(small);
        match shift {
            0 => {
                self.a.mov_rx(false, R::Rdx, X(0));
                self.a.store8(mem(R::Rax, 0), R::Rdx);
            }
            1 => {
                self.a.mov_rx(false, R::Rdx, X(0));
                self.a.store16(mem(R::Rax, 0), R::Rdx);
            }
            _ => self.a.fstore(wide, mem(R::Rax, 0), X(0)),
        }
        self.a.alu_ri(true, Alu::Add, R::Rax, 1 << shift);
        self.a.alu_rr(true, Alu::Cmp, R::Rax, R::R11);
        self.a.jcc(Cond::B, small);
        self.a.bind(done);
    }
}
