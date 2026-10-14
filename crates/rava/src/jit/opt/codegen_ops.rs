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
            // A spilled parameter is read from its frame slot where it is used; one that came
            // in a register is in place already.
            Inst::Param(_) if matches!(self.loc[v as usize], Loc::Frame(_)) || self.placed(v) => {}
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
            Inst::Select(cmp, cty, a, b, t, f) => self.select(v, cmp, cty, a, b, t, f),
            Inst::FlagCheck(addr, s) => {
                let fail = self.deopt(&s);
                if crate::low::is_low(addr) {
                    self.a.cmp8_mi(crate::jit::asm::abs32(addr), 0);
                } else {
                    self.a.mov_ri(R::Rax, addr as i64);
                    self.a.cmp8_mi(mem(R::Rax, 0), 0);
                }
                self.a.jcc(Cond::Ne, fail);
            }
            Inst::NullCheck(_, s) if self.implicit.contains(&v) => {
                self.pending_null = Some(self.deopt(&s));
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
                    self.fault_site();
                    self.a.alu_mi(false, Alu::Cmp, mem(ra, crate::heap::ARRAY_LENGTH_OFFSET as i32), c as i32);
                    self.a.jcc(Cond::Be, fail);
                } else {
                    let ri = self.reg(idx, R::R11);
                    self.fault_site();
                    self.a.alu_rm(false, Alu::Cmp, ri, mem(ra, crate::heap::ARRAY_LENGTH_OFFSET as i32));
                    self.a.jcc(Cond::Ae, fail);
                }
            }
            Inst::Guard(cmp, ty, x, y, s) => {
                let fail = self.deopt(&s);
                let rx = self.reg(x, R::Rax);
                self.alu_val(ty != Ty::I, Alu::Cmp, rx, y);
                self.a.jcc(super::codegen::invert(super::codegen::cond(cmp)), fail);
            }
            Inst::RangeGuard(arr, init, limit, s) => {
                let consts = (self.g.constant(init), self.g.constant(limit));
                if let (Some(i), Some(n)) = consts {
                    // A counted loop over constant bounds (`for (i = 0; i < 7; i++)`): whether
                    // it runs and whether `init >= 0` are known here.
                    if i as i32 >= n as i32 {
                        return;
                    }
                    let fail = self.deopt(&s);
                    if (i as i32) < 0 {
                        self.a.jmp(fail);
                        return;
                    }
                    let ra = self.reg(arr, R::Rax);
                    self.a.test_rr(true, ra, ra);
                    self.a.jcc(Cond::E, fail);
                    self.a.alu_mi(false, Alu::Cmp, mem(ra, crate::heap::ARRAY_LENGTH_OFFSET as i32), n as i32);
                    self.a.jcc(Cond::L, fail);
                    return;
                }
                let (fail, ok) = (self.deopt(&s), self.a.new_label());
                let ri = self.reg(init, R::Rax);
                self.alu_val(false, Alu::Cmp, ri, limit);
                self.a.jcc(Cond::Ge, ok);
                self.a.test_rr(false, ri, ri);
                self.a.jcc(Cond::S, fail);
                let rl = self.reg(limit, R::R11);
                let ra = self.reg(arr, R::Rax);
                self.a.test_rr(true, ra, ra);
                self.a.jcc(Cond::E, fail);
                self.a.alu_rm(false, Alu::Cmp, rl, mem(ra, crate::heap::ARRAY_LENGTH_OFFSET as i32));
                self.a.jcc(Cond::G, fail);
                self.a.bind(ok);
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
            match c {
                // A power of two is a shift; 3, 5 and 9 one `lea` (a cycle each, not three).
                2.. if c.count_ones() == 1 => {
                    if x != r {
                        self.a.mov_rr(w, r, x);
                    }
                    self.a.shift_ri(w, crate::jit::asm::Shift::Shl, r, c.trailing_zeros() as u8);
                }
                3 | 5 | 9 => self.a.lea_w(w, r, crate::jit::asm::mem_idx(x, x, (c - 1) as u8, 0)),
                _ => self.a.imul_rri(w, r, x, c),
            }
            return self.put(v, r);
        }
        // The register allocator gave a commutative result its second operand's register
        // (`regalloc.rs`): combine the first into it.
        if self.loc[y as usize] == Loc::Reg(r) && self.loc[x as usize] != Loc::Reg(r) {
            match op {
                BinOp::Mul => match self.loc[x as usize] {
                    Loc::Stack(s) => self.a.imul_rm(w, r, super::moves::spill(s)),
                    _ => {
                        let s = self.reg(x, R::R11);
                        self.a.imul_rr(w, r, s);
                    }
                },
                _ => {
                    let alu = match op {
                        BinOp::Add => Alu::Add,
                        BinOp::And => Alu::And,
                        BinOp::Or => Alu::Or,
                        _ => Alu::Xor,
                    };
                    self.alu_val(w, alu, r, x);
                }
            }
            return self.put(v, r);
        }
        // A constant added to a value in another register: one `lea`, not a move and an add.
        if let (BinOp::Add | BinOp::Sub, Some(c), Loc::Reg(xr)) = (op, self.g.constant(y), self.loc[x as usize]) {
            let c = if op == BinOp::Sub { c.checked_neg() } else { Some(c) };
            if let Some(c) = c.and_then(|c| i32::try_from(c).ok()).filter(|_| xr != r) {
                self.a.lea_w(w, r, crate::jit::asm::mem(xr, c));
                return self.put(v, r);
            }
        }
        self.load(r, x);
        match op {
            BinOp::Mul => match self.loc[y as usize] {
                Loc::Stack(s) => self.a.imul_rm(w, r, super::moves::spill(s)),
                Loc::Frame(s) => {
                    let m = self.slot(s);
                    self.a.imul_rm(w, r, m)
                }
                _ => {
                    let s = self.reg(y, R::R11);
                    self.a.imul_rr(w, r, s);
                }
            },
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
        // The result register is x's own when x dies here, else not y's (x op x aside) but
        // for a commutative operation, which then combines x into y's register.
        let r = self.xout(v);
        if matches!(op, BinOp::Add | BinOp::Mul) && self.loc[y as usize] == Loc::Xmm(r.0) && self.loc[x as usize] != Loc::Xmm(r.0) {
            let m = match self.loc[x as usize] {
                Loc::Const(c) => Some(self.const_mem(c)),
                Loc::Stack(s) => Some(super::moves::spill(s)),
                Loc::Frame(s) => Some(self.slot(s)),
                _ => None,
            };
            match m {
                Some(m) => self.a.farith_m(double, sse, r, m),
                None => {
                    let a = self.xreg(x, X(1));
                    self.a.farith_rr(double, sse, r, a);
                }
            }
            return self.xput(v, r);
        }
        // A constant, spilled or parameter operand is used from memory.
        let m = match self.loc[y as usize] {
            Loc::Const(c) => Some(self.const_mem(c)),
            Loc::Stack(s) => Some(super::moves::spill(s)),
            Loc::Frame(s) => Some(self.slot(s)),
            _ => None,
        };
        if let Some(m) = m {
            self.xload(r, x);
            self.a.farith_m(double, sse, r, m);
            return self.xput(v, r);
        }
        let b = self.xreg(y, X(1));
        self.xload(r, x);
        self.a.farith_rr(double, sse, r, b);
        self.xput(v, r);
    }

    /// Whether int `x` was loaded from memory, which sign-extends it to 64 bits (or zero-extends
    /// a char or boolean, the same for those): `i2l` of it needs no `movsxd`.
    fn sign_extended(&self, x: V) -> bool {
        use crate::types::BasicType::{Double, Float};
        match *self.g.inst(self.g.resolve(x)) {
            Inst::LoadField(_, _, bt) | Inst::LoadIndexed(_, _, bt) | Inst::LoadStatic(_, bt) => !matches!(bt, Float | Double),
            Inst::ArrayLength(_) => true,
            _ => false,
        }
    }

    fn convert(&mut self, v: V, op: u8, x: V) {
        let r = self.out(v);
        // A narrowing or widening move reads a source register straight into the result.
        let src = match self.loc[x as usize] {
            Loc::Reg(s) => s,
            _ => {
                if matches!(op, 0x85 | 0x88 | 0x91 | 0x92 | 0x93) {
                    self.load(r, x);
                }
                r
            }
        };
        match op {
            0x85 => {
                if !self.sign_extended(x) {
                    self.a.movsxd(r, src);
                } else if src != r {
                    self.a.mov_rr(true, r, src);
                }
            }
            0x88 => self.a.movsxd(r, src),
            0x91 | 0x93 => self.a.movsx_rr(if op == 0x91 { 1 } else { 2 }, r, src),
            0x92 => self.a.movzx16_rr(r, src),
            0x86 | 0x87 | 0x89 | 0x8a => {
                let from_long = op >= 0x89;
                let to_double = op == 0x87 || op == 0x8a;
                let r = self.xout(v);
                // A spilled or parameter int converts straight from its slot.
                match self.loc[x as usize] {
                    Loc::Stack(s) => self.a.cvt_i2f_m(to_double, from_long, r, super::moves::spill(s)),
                    Loc::Frame(s) => {
                        let m = self.slot(s);
                        self.a.cvt_i2f_m(to_double, from_long, r, m)
                    }
                    _ => {
                        let s = self.reg(x, R::Rax);
                        self.a.cvt_i2f(to_double, from_long, r, s);
                    }
                }
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
        self.slow_calls.push(super::codegen::SlowCall { entry: slow, back: done, addr: helper, args: vec![Arg::Val(x)], link: None, fail: None, preserve: self.preserve });
        if !to_long {
            self.a.movsxd(R::Rax, R::Rax);
        }
        self.put(v, R::Rax);
    }

    /// `a cmp b ? t : f`: compare, then the false value into rax and a conditional move of the
    /// true one (moves and loads leave the flags alone).
    #[allow(clippy::too_many_arguments)]
    fn select(&mut self, v: V, cmp: Cmp, cty: Ty, a: V, b: V, t: V, f: V) {
        let ra = self.reg(a, R::Rax);
        self.alu_val(cty != Ty::I, Alu::Cmp, ra, b);
        // Straight into the result's register (moves keep the flags): the false value, then the
        // true one where the condition holds; when that register already holds the true value,
        // the false one where it fails.
        let (c, out) = (super::codegen::cond(cmp), self.out(v));
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
            let (ordered, done) = (self.a.new_label(), self.a.new_label());
            self.fcmp_val(double, a, y);
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
