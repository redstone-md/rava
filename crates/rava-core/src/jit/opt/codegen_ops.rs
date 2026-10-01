//! Code generation of arithmetic, conversions, comparisons and guards. An int value's register
//! holds it in the low half, the high half zero or its sign extension (x86 32-bit operations
//! clear it): 32-bit operations read it as is, a non-negative index addresses memory as is, and
//! `i2l` and the frame (call arguments, returns, deoptimization) sign-extend it. Float values
//! keep their bits in the low half (slot encoding).
//! A result shares a register only with its first operand, when that operand dies there
//! (`regalloc.rs`): every sequence reads the other operands before writing the result.

use super::codegen::{Arg, Gen};
use super::regalloc::Loc;
use super::ir::*;
use crate::jit::asm::{mem, Alu, Cond, Shift, R, X};
use crate::jit::runtime;

impl Gen<'_> {
    pub(super) fn inst(&mut self, v: V) {
        let ty = self.g.ty(v);
        match self.g.inst(v).clone() {
            // A spilled parameter is read from its frame slot where it is used.
            Inst::Param(_) if matches!(self.loc[v as usize], Loc::Frame(_)) => {}
            Inst::Param(slot) if matches!(ty, Ty::F | Ty::D) => {
                let r = self.xout(v);
                self.a.fload(true, r, self.slot(slot as i32));
                self.xput(v, r);
            }
            Inst::Param(slot) => {
                let r = self.out(v);
                self.a.load(true, r, self.slot(slot as i32));
                self.put(v, r);
            }
            Inst::Const(_) | Inst::Phi(_) => {}
            Inst::Fence => self.a.storeload(),
            Inst::Copy(x) => {
                let (dst, src) = (self.loc[v as usize], self.loc[x as usize]);
                if dst != Loc::None && dst != src {
                    super::moves::mov(&mut self.a, dst, src);
                }
            }
            Inst::Neg(x) => self.neg(v, ty, x),
            Inst::Bin(op, x, y) => match ty {
                Ty::F | Ty::D => self.float_bin(v, ty == Ty::D, op, x, y),
                _ => self.int_bin(v, ty == Ty::J, op, x, y),
            },
            Inst::Convert(op, x) => self.convert(v, op, x),
            Inst::Compare(op, x, y) => self.compare(v, op, x, y),
            Inst::FlagCheck(addr, s) => {
                let fail = self.deopt(&s);
                self.a.mov_ri(R::Rax, addr as i64);
                self.a.cmp8_mi(mem(R::Rax, 0), 0);
                self.a.jcc(Cond::Ne, fail);
            }
            Inst::NullCheck(x, s) | Inst::ZeroCheck(x, s) => {
                let fail = self.deopt(&s);
                match self.loc[x as usize] {
                    super::regalloc::Loc::Const(0) => self.a.jmp(fail),
                    super::regalloc::Loc::Const(_) => {}
                    _ => {
                        let r = self.reg(x, R::Rax);
                        self.a.test_rr(true, r, r);
                        self.a.jcc(Cond::E, fail);
                    }
                }
            }
            Inst::BoundsCheck(arr, idx, s) => {
                let fail = self.deopt(&s);
                let ra = self.reg(arr, R::Rax);
                if let Loc::Const(c) = self.loc[idx as usize].clone() {
                    // The index as an unsigned 32-bit immediate: length <= index fails.
                    self.a.alu_mi(false, Alu::Cmp, mem(ra, 16), c as i32);
                    self.a.jcc(Cond::Be, fail);
                } else {
                    let ri = self.reg(idx, R::R11);
                    self.a.alu_rm(false, Alu::Cmp, ri, mem(ra, 16));
                    self.a.jcc(Cond::Ae, fail);
                }
            }
            Inst::InitCheck(k, s) => {
                let fail = self.deopt(&s);
                crate::jit::emit_obj::init_check(&mut self.a, k, R::Rax, fail);
            }
            _ => self.memory_inst(v),
        }
    }

    fn neg(&mut self, v: V, ty: Ty, x: V) {
        if matches!(ty, Ty::F | Ty::D) {
            let r = self.xout(v);
            self.xload(r, x);
            self.xmask(if ty == Ty::D { i64::MIN } else { 0x8000_0000 });
            self.a.fxor(ty == Ty::D, r, X(1));
            return self.xput(v, r);
        }
        let r = self.out(v);
        self.load(r, x);
        match ty {
            Ty::I => self.a.neg(false, r),
            Ty::J => self.a.neg(true, r),
            Ty::F => self.a.alu_ri(false, Alu::Xor, r, i32::MIN),
            _ => {
                self.a.mov_ri(R::R11, i64::MIN);
                self.a.alu_rr(true, Alu::Xor, r, R::R11);
            }
        }
        self.put(v, r);
    }

    fn int_bin(&mut self, v: V, w: bool, op: BinOp, x: V, y: V) {
        if matches!(op, BinOp::Div | BinOp::Rem) {
            return self.div(v, w, op == BinOp::Rem, x, y);
        }
        if matches!(op, BinOp::Shl | BinOp::Shr | BinOp::Ushr) {
            return self.shift(v, w, op, x, y);
        }
        let r = self.out(v);
        if let (BinOp::Mul, Some(c)) = (op, self.g.constant(y).and_then(|c| i32::try_from(c).ok())) {
            let x = self.reg(x, r);
            self.a.imul_rri(w, r, x, c);
            return self.put(v, r);
        }
        self.load(r, x);
        match op {
            BinOp::Mul => {
                let s = self.reg(y, R::R11);
                self.a.imul_rr(w, r, s);
            }
            _ => {
                let alu = match op {
                    BinOp::Add => Alu::Add,
                    BinOp::Sub => Alu::Sub,
                    BinOp::And => Alu::And,
                    BinOp::Or => Alu::Or,
                    _ => Alu::Xor,
                };
                self.alu_val(w, alu, r, y);
            }
        }
        self.put(v, r);
    }

    fn shift(&mut self, v: V, w: bool, op: BinOp, x: V, y: V) {
        let sh = match op {
            BinOp::Shl => Shift::Shl,
            BinOp::Shr => Shift::Sar,
            _ => Shift::Shr,
        };
        let count = self.g.constant(y);
        if count.is_none() {
            self.load(R::Rcx, y);
        }
        let r = self.out(v);
        self.load(r, x);
        match count {
            Some(c) => self.a.shift_ri(w, sh, r, (c & if w { 63 } else { 31 }) as u8),
            None => self.a.shift_cl(w, sh, r),
        }
        self.put(v, r);
    }

    /// Division by a non-zero divisor (checked before): `MIN / -1` would trap in `idiv`.
    fn div(&mut self, v: V, w: bool, rem: bool, x: V, y: V) {
        if let Some(d) = self.g.constant(y).filter(|&d| if w { d != 0 } else { d as i32 != 0 }) {
            return self.div_const(v, w, rem, x, d);
        }
        let (special, done) = (self.a.new_label(), self.a.new_label());
        self.load(R::Rcx, y);
        self.load(R::Rax, x);
        self.a.alu_ri(w, Alu::Cmp, R::Rcx, -1);
        self.a.jcc(Cond::E, special);
        self.a.idiv(w, R::Rcx);
        if rem {
            self.a.mov_rr(true, R::Rax, R::Rdx);
        }
        self.a.jmp(done);
        self.a.bind(special);
        if rem {
            self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        } else {
            self.a.neg(w, R::Rax);
        }
        self.a.bind(done);
        self.put(v, R::Rax);
    }

    fn float_bin(&mut self, v: V, double: bool, op: BinOp, x: V, y: V) {
        if op == BinOp::Rem {
            let helper = if double { runtime::drem as *const () as usize } else { runtime::frem as *const () as usize };
            self.call_preserving(helper, &[Arg::Val(x), Arg::Val(y)]);
            return self.put(v, R::Rax);
        }
        let sse = match op {
            BinOp::Add => 0x58,
            BinOp::Sub => 0x5C,
            BinOp::Mul => 0x59,
            _ => 0x5E,
        };
        // The result register is x's own when x dies here, else not y's (x op x aside).
        let r = self.xout(v);
        if let Loc::Const(c) = self.loc[y as usize] {
            let m = self.const_mem(c);
            self.xload(r, x);
            self.a.farith_m(double, sse, r, m);
            return self.xput(v, r);
        }
        let b = self.xreg(y, X(1));
        self.xload(r, x);
        self.a.farith_rr(double, sse, r, b);
        self.xput(v, r);
    }

    fn convert(&mut self, v: V, op: u8, x: V) {
        let r = self.out(v);
        match op {
            0x85 => {
                self.load(r, x);
                self.a.movsxd(r, r);
            }
            0x88 => {
                self.load(r, x);
                self.a.movsxd(r, r);
            }
            0x91 | 0x93 => {
                self.load(r, x);
                self.a.movsx_rr(if op == 0x91 { 1 } else { 2 }, r, r);
            }
            0x92 => {
                self.load(r, x);
                self.a.movzx16_rr(r, r);
            }
            0x86 | 0x87 | 0x89 | 0x8a => {
                let from_long = op >= 0x89;
                let to_double = op == 0x87 || op == 0x8a;
                let s = self.reg(x, R::Rax);
                let r = self.xout(v);
                self.a.cvt_i2f(to_double, from_long, r, s);
                return self.xput(v, r);
            }
            0x8d | 0x90 => {
                let a = self.xreg(x, X(1));
                let r = self.xout(v);
                self.a.cvt_f2f(op == 0x8d, r, a);
                return self.xput(v, r);
            }
            _ => return self.float_to_int(v, op, x),
        }
        self.put(v, r);
    }

    /// f2i/f2l/d2i/d2l: `cvtt` is exact except for NaN and out-of-range values, which yield the
    /// minimum value; only then the saturating runtime helper decides.
    fn float_to_int(&mut self, v: V, op: u8, x: V) {
        let from_double = op >= 0x8e;
        let to_long = op == 0x8c || op == 0x8f;
        let helper = match op {
            0x8b => runtime::f2i as *const () as usize,
            0x8c => runtime::f2l as *const () as usize,
            0x8e => runtime::d2i as *const () as usize,
            _ => runtime::d2l as *const () as usize,
        };
        let a = self.xreg(x, X(0));
        self.a.cvtt_f2i(from_double, to_long, R::Rax, a);
        let done = self.a.new_label();
        if to_long {
            self.a.mov_ri(R::R11, i64::MIN);
            self.a.alu_rr(true, Alu::Cmp, R::Rax, R::R11);
        } else {
            self.a.alu_ri(false, Alu::Cmp, R::Rax, i32::MIN);
        }
        // The saturating slow path is out of line (after the method's code).
        let slow = self.a.new_label();
        self.a.jcc(Cond::E, slow);
        self.a.bind(done);
        self.slow_calls.push(super::codegen::SlowCall { entry: slow, back: done, addr: helper, args: vec![Arg::Val(x)], link: None, fail: None });
        if !to_long {
            self.a.movsxd(R::Rax, R::Rax);
        }
        self.put(v, R::Rax);
    }

    /// lcmp / fcmpl / fcmpg / dcmpl / dcmpg: -1, 0 or 1.
    fn compare(&mut self, v: V, op: u8, x: V, y: V) {
        if op == 0x94 {
            let rx = self.reg(x, R::Rcx);
            self.alu_val(true, Alu::Cmp, rx, y);
            self.a.setcc(Cond::G, R::Rax);
            self.a.setcc(Cond::L, R::Rdx);
        } else {
            let double = op >= 0x97;
            let nan = if op == 0x95 || op == 0x97 { -1 } else { 1 };
            let a = self.xreg(x, X(0));
            let b = self.xreg(y, X(1));
            let (ordered, done) = (self.a.new_label(), self.a.new_label());
            self.a.fcmp(double, a, b);
            self.a.jcc(Cond::Np, ordered);
            self.a.mov_ri(R::Rax, nan);
            self.a.jmp(done);
            self.a.bind(ordered);
            self.a.setcc(Cond::A, R::Rax);
            self.a.setcc(Cond::B, R::Rdx);
            self.finish_compare();
            self.a.bind(done);
            return self.put(v, R::Rax);
        }
        self.finish_compare();
        self.put(v, R::Rax);
    }

    /// rax := (al) - (dl) as 32-bit values.
    fn finish_compare(&mut self) {
        self.a.movzx8(R::Rax, R::Rax);
        self.a.movzx8(R::Rdx, R::Rdx);
        self.a.alu_rr(false, Alu::Sub, R::Rax, R::Rdx);
    }
}
