//! Register templates over the virtual operand stack (`vstack.rs`): constants, locals,
//! arithmetic, conversions, comparisons, branches and returns. Array and field access live in
//! `emit_reg_mem.rs`. An instruction without a register template falls back to the memory
//! templates of `emit.rs` once the stack has been written back.

use super::analysis::i2;
use super::asm::{Alu, Cond, Shift, R, X};
use super::compile::Emitter;
use super::emit::{int_cond, rel};
use super::vstack::{Loc, Src};
use crate::classfile::Cp;
use crate::link::Resolved;

impl Emitter {
    /// Emits `pc` with a register template: `Some(false)` when there is none (the caller falls
    /// back), `None` refuses the method.
    pub(super) fn emit_reg(&mut self, pc: usize, depth: u16) -> Option<bool> {
        let op = self.code[pc];
        let kind_wide = |k: u8| k == 1 || k == 3;
        match op {
            0x01 => self.push(Loc::Imm(0)),
            0x02..=0x08 => self.push(Loc::Imm(op as i64 - 3)),
            0x09 | 0x0a => self.push_w(true, Loc::Imm((op - 0x09) as i64)),
            0x0b..=0x0d => self.push(Loc::Imm(((op - 0x0b) as f32).to_bits() as i64)),
            0x0e | 0x0f => self.push_w(true, Loc::Imm(((op - 0x0e) as f64).to_bits() as i64)),
            0x10 => self.push(Loc::Imm(self.code[pc + 1] as i8 as i64)),
            0x11 => self.push(Loc::Imm(i2(self.code, pc + 1) as i64)),
            0x12..=0x14 => return Some(self.ldc_reg(pc)),
            0x15..=0x19 => self.load_local(self.code[pc + 1] as usize, op == 0x16 || op == 0x18),
            0x1a..=0x2d => self.load_local(((op - 0x1a) % 4) as usize, kind_wide((op - 0x1a) / 4)),
            0x36..=0x3a => self.store_local(self.code[pc + 1] as usize, op == 0x37 || op == 0x39),
            0x3b..=0x4e => self.store_local(((op - 0x3b) % 4) as usize, kind_wide((op - 0x3b) / 4)),
            0x84 => self.iinc_reg(self.code[pc + 1] as usize, self.code[pc + 2] as i8 as i32),
            0xc4 => return Some(self.wide_reg(pc)),
            0x57 => {
                self.pop();
            }
            0x58 => {
                self.pop();
                self.pop();
            }
            0x59 => self.dup_reg(),
            0x60 | 0x64 | 0x7e | 0x80 | 0x82 => self.int_alu(op, false),
            0x61 | 0x65 | 0x7f | 0x81 | 0x83 => self.int_alu(op, true),
            0x68 | 0x69 => self.mul_reg(op == 0x69),
            0x74 | 0x75 => self.neg_reg(op == 0x75),
            0x78..=0x7d => self.shift_reg(op),
            0x62 | 0x63 | 0x66 | 0x67 | 0x6a | 0x6b | 0x6e | 0x6f => self.fp_arith(op),
            0x76 | 0x77 => self.fneg_reg(op == 0x77),
            0x85..=0x93 => return Some(self.convert_reg(op)),
            0x94 => self.lcmp_reg(),
            0x95..=0x98 => self.fcmp_reg(op),
            0x99..=0xa7 | 0xc6 | 0xc7 => self.branch_reg(pc, op),
            0xaa | 0xab => {
                let key = self.pop();
                self.to_rax(key);
                self.flush();
                self.switch_rax(pc, op);
            }
            0xac..=0xb0 => {
                let v = self.pop_w(op == 0xad || op == 0xaf);
                self.to_rax(v);
                self.ret(true);
            }
            0xb1 => self.ret(false),
            0xc0 | 0xc1 => return Some(self.fold_type_check(pc, op)),
            _ => return Some(self.emit_reg_mem(pc, depth, op)),
        }
        Some(true)
    }

    /// `checkcast` / `instanceof` on a constant object, decided now.
    fn fold_type_check(&mut self, pc: usize, op: u8) -> bool {
        let Some(Loc::Imm(o)) = self.vs.last().map(|s| s.loc) else { return false };
        let Some(Resolved::Class(k)) = self.m.holder.resolved[self.cp_index(pc) as usize].get() else { return false };
        let is = o != 0 && super::fold::class_of(o as usize).is_subtype_of(k);
        match op {
            0xc0 if o == 0 || is => {}
            0xc0 => return false,
            _ => {
                self.pop();
                self.push(Loc::Imm(is as i64));
            }
        }
        true
    }

    /// Moves `o` into rax.
    pub(super) fn to_rax(&mut self, o: super::vstack::Opnd) {
        let r = self.read(o, R::Rax);
        if r != R::Rax {
            self.a.mov_rr(true, R::Rax, r);
        }
    }

    fn ldc_reg(&mut self, pc: usize) -> bool {
        let index = if self.code[pc] == 0x12 { self.code[pc + 1] as u16 } else { self.cp_index(pc) };
        let (v, wide) = match self.cf.pool[index as usize] {
            Cp::Int(v) => (v as i64, false),
            Cp::Float(v) => (v.to_bits() as i64, false),
            Cp::Long(v) => (v, true),
            Cp::Double(v) => (v.to_bits() as i64, true),
            Cp::String(_) | Cp::MethodType(_) | Cp::MethodHandle { .. } => match self.m.holder.resolved[index as usize].get() {
                Some(Resolved::String(o)) | Some(Resolved::Constant(o)) => (o as i64, false),
                _ => return false,
            },
            _ => return false,
        };
        self.push_w(wide, Loc::Imm(v));
        true
    }

    fn load_local(&mut self, n: usize, wide: bool) {
        if let Some(v) = self.lconst.get(n).copied().flatten() {
            return self.push_w(wide, Loc::Imm(v));
        }
        if self.lreg(n).is_some() {
            return self.push_w(wide, Loc::Local(n as u16));
        }
        let d = self.alloc(&[]);
        self.a.load(true, d, self.local(n));
        self.push_w(wide, Loc::Reg(d));
    }

    fn store_local(&mut self, n: usize, wide: bool) {
        let v = self.pop_w(wide);
        if let Some(c) = self.lconst.get_mut(n) {
            *c = if let Loc::Imm(x) = v.loc { Some(x) } else { None };
        }
        let excl: Vec<R> = Self::reg_of(v).into_iter().collect();
        self.kill_aliases(n, &excl);
        match self.lreg(n) {
            Some(l) => {
                match v.loc {
                    Loc::Imm(x) => self.a.mov_ri(l, x),
                    Loc::Mem => self.a.load(true, l, self.stk(v.idx)),
                    _ => {
                        let r = self.read(v, R::Rax);
                        if r != l {
                            self.a.mov_rr(true, l, r);
                        }
                    }
                }
                self.a.store(true, self.local(n), l);
            }
            None => match v.loc {
                Loc::Imm(x) => self.store_imm64(self.local(n), x),
                _ => {
                    let r = self.read(v, R::Rax);
                    self.a.store(true, self.local(n), r);
                }
            },
        }
    }

    fn iinc_reg(&mut self, n: usize, delta: i32) {
        if let Some(c) = self.lconst.get_mut(n) {
            *c = None;
        }
        self.kill_aliases(n, &[]);
        let r = self.lreg(n).unwrap_or(R::Rax);
        if r == R::Rax {
            self.a.load(false, R::Rax, self.local(n));
        }
        self.a.alu_ri(false, Alu::Add, r, delta);
        self.a.movsxd(r, r);
        self.a.store(true, self.local(n), r);
    }

    fn wide_reg(&mut self, pc: usize) -> bool {
        let n = u16::from_be_bytes([self.code[pc + 2], self.code[pc + 3]]) as usize;
        match self.code[pc + 1] {
            op @ 0x15..=0x19 => self.load_local(n, op == 0x16 || op == 0x18),
            op @ 0x36..=0x3a => self.store_local(n, op == 0x37 || op == 0x39),
            0x84 => self.iinc_reg(n, i2(self.code, pc + 4) as i32),
            _ => return false,
        }
        true
    }

    fn dup_reg(&mut self) {
        let top = *self.vs.last().expect("dup on an empty stack");
        let loc = match top.loc {
            Loc::Reg(r) => {
                let d = self.alloc(&[]);
                self.a.mov_rr(true, d, r);
                Loc::Reg(d)
            }
            Loc::Mem => {
                let d = self.alloc(&[]);
                self.a.load(true, d, self.stk(self.vs.len() as i32 - 1));
                Loc::Reg(d)
            }
            other => other,
        };
        self.push(loc);
    }

    /// Pops `b` then `a` and returns an owned register holding `a` plus `b` as a source.
    fn binary(&mut self, wide: bool, b_temp: R) -> (R, Src) {
        let b = self.pop_w(wide);
        let a = self.pop_w(wide);
        let excl: Vec<R> = Self::reg_of(b).into_iter().collect();
        let d = self.owned(a, &excl);
        let s = self.src(b, b_temp);
        (d, s)
    }

    fn finish_int(&mut self, wide: bool, d: R) {
        if !wide {
            self.a.movsxd(d, d);
        }
        self.push_w(wide, Loc::Reg(d));
    }

    fn int_alu(&mut self, op: u8, wide: bool) {
        let alu = match op {
            0x60 | 0x61 => Alu::Add,
            0x64 | 0x65 => Alu::Sub,
            0x7e | 0x7f => Alu::And,
            0x80 | 0x81 => Alu::Or,
            _ => Alu::Xor,
        };
        let (d, s) = self.binary(wide, R::Rcx);
        self.alu_src(wide, alu, d, s);
        self.finish_int(wide, d);
    }

    fn mul_reg(&mut self, wide: bool) {
        let (d, s) = self.binary(wide, R::Rcx);
        let r = match s {
            Src::R(r) => r,
            Src::I(v) => {
                self.a.mov_ri(R::Rcx, v as i64);
                R::Rcx
            }
            Src::M(m) => {
                self.a.load(true, R::Rcx, m);
                R::Rcx
            }
        };
        self.a.imul_rr(wide, d, r);
        self.finish_int(wide, d);
    }

    fn neg_reg(&mut self, wide: bool) {
        let a = self.pop_w(wide);
        let d = self.owned(a, &[]);
        self.a.neg(wide, d);
        self.finish_int(wide, d);
    }

    fn shift_reg(&mut self, op: u8) {
        let wide = op & 1 == 1;
        let sh = match op {
            0x78 | 0x79 => Shift::Shl,
            0x7a | 0x7b => Shift::Sar,
            _ => Shift::Shr,
        };
        let count = self.pop();
        let a = self.pop_w(wide);
        let excl: Vec<R> = Self::reg_of(count).into_iter().collect();
        let d = self.owned(a, &excl);
        if let Loc::Imm(c) = count.loc {
            self.a.shift_ri(wide, sh, d, (c & if wide { 63 } else { 31 }) as u8);
        } else {
            let rc = self.read(count, R::Rcx);
            if rc != R::Rcx {
                self.a.mov_rr(false, R::Rcx, rc);
            }
            self.a.shift_cl(wide, sh, d);
        }
        self.finish_int(wide, d);
    }

    fn fp_arith(&mut self, op: u8) {
        let double = op & 1 == 1;
        let sse = match op {
            0x62 | 0x63 => 0x58,
            0x66 | 0x67 => 0x5C,
            0x6a | 0x6b => 0x59,
            _ => 0x5E,
        };
        let b = self.pop_w(double);
        let a = self.pop_w(double);
        if double {
            // Keep the result in an SSE register so chained double arithmetic (the bulk of
            // procedural-generation math) stays in xmm instead of round-tripping through GPRs.
            // `b` is already popped, so when it is in an xmm, `a`'s register must exclude it or
            // loading `a` would clobber `b` (turning `a OP b` into `a OP a`).
            let xa = match b.loc {
                Loc::Xmm(xb) => {
                    let xa = self.xmm_owned(a, &[xb]);
                    self.a.farith_rr(true, sse, xa, xb);
                    xa
                }
                Loc::Mem => {
                    let xa = self.xmm_owned(a, &[]);
                    self.a.farith_m(true, sse, xa, self.stk(b.idx));
                    xa
                }
                _ => {
                    let xa = self.xmm_owned(a, &[]);
                    let xb = self.xmm_owned(b, &[xa]);
                    self.a.farith_rr(true, sse, xa, xb);
                    xa
                }
            };
            self.push_w(true, Loc::Xmm(xa));
            return;
        }
        let ra = self.read(a, R::Rax);
        self.a.mov_xr(double, X(0), ra);
        let rb = self.read(b, R::Rcx);
        self.a.mov_xr(double, X(1), rb);
        self.a.farith_rr(double, sse, X(0), X(1));
        let d = self.alloc(&[]);
        self.a.mov_rx(double, d, X(0));
        self.push_w(double, Loc::Reg(d));
    }

    fn fneg_reg(&mut self, double: bool) {
        let a = self.pop_w(double);
        let d = self.owned(a, &[]);
        if double {
            self.a.mov_ri(R::Rax, i64::MIN);
            self.a.alu_rr(true, Alu::Xor, d, R::Rax);
        } else {
            self.a.alu_ri(false, Alu::Xor, d, i32::MIN);
        }
        self.push_w(double, Loc::Reg(d));
    }

    fn convert_reg(&mut self, op: u8) -> bool {
        match op {
            0x85 => {
                let a = self.pop();
                self.push_w(true, a.loc);
            }
            0x88 => {
                let a = self.pop_w(true);
                let d = self.owned(a, &[]);
                self.finish_int(false, d);
            }
            0x91..=0x93 => {
                let a = self.pop();
                let d = self.owned(a, &[]);
                match op {
                    0x91 => self.a.movsx_rr(1, d, d),
                    0x92 => self.a.movzx16_rr(d, d),
                    _ => self.a.movsx_rr(2, d, d),
                }
                self.push(Loc::Reg(d));
            }
            // int/long → float/double
            0x86 | 0x87 | 0x89 | 0x8a => {
                let from_long = op >= 0x89;
                let to_double = op == 0x87 || op == 0x8a;
                let a = self.pop_w(from_long);
                let r = self.read(a, R::Rax);
                self.a.cvt_i2f(to_double, from_long, X(0), r);
                let d = self.alloc(&[]);
                self.a.mov_rx(to_double, d, X(0));
                self.push_w(to_double, Loc::Reg(d));
            }
            0x8d | 0x90 => {
                let to_double = op == 0x8d;
                let a = self.pop_w(!to_double);
                let r = self.read(a, R::Rax);
                self.a.mov_xr(!to_double, X(0), r);
                self.a.cvt_f2f(to_double, X(0), X(0));
                let d = self.alloc(&[]);
                self.a.mov_rx(to_double, d, X(0));
                self.push_w(to_double, Loc::Reg(d));
            }
            0x8b | 0x8c | 0x8e | 0x8f => self.float_to_int(op),
            _ => return false,
        }
        true
    }

    /// f2i/f2l/d2i/d2l: `cvtt` is exact except for NaN and out-of-range values, which yield the
    /// minimum value; only then the saturating runtime helper decides (with the stack written
    /// back beforehand, so the registers it clobbers can be reloaded).
    fn float_to_int(&mut self, op: u8) {
        let from_double = op >= 0x8e;
        let to_long = op == 0x8c || op == 0x8f;
        let helper = match op {
            0x8b => super::runtime::f2i as *const () as usize,
            0x8c => super::runtime::f2l as *const () as usize,
            0x8e => super::runtime::d2i as *const () as usize,
            _ => super::runtime::d2l as *const () as usize,
        };
        self.flush();
        let a = self.pop_w(from_double);
        let r = self.read(a, R::Rax);
        self.a.mov_xr(from_double, X(0), r);
        let d = self.alloc(&[]);
        self.a.cvtt_f2i(from_double, to_long, d, X(0));
        let done = self.a.new_label();
        if to_long {
            self.a.mov_ri(R::Rax, i64::MIN);
            self.a.alu_rr(true, Alu::Cmp, d, R::Rax);
        } else {
            self.a.alu_ri(false, Alu::Cmp, d, i32::MIN);
        }
        self.a.jcc(Cond::Ne, done);
        self.a.mov_rx(from_double, R::Rdi, X(0));
        self.a.call_abs(helper);
        self.a.mov_rr(true, d, R::Rax);
        let live = self.live_regs();
        self.reload_live(&live);
        self.a.bind(done);
        self.finish_int(to_long, d);
    }

    /// Leaves -1/0/1 from the flags of a completed compare in a fresh register.
    fn push_compare_result(&mut self, gt: Cond, lt: Cond) {
        self.a.setcc(gt, R::Rax);
        self.a.setcc(lt, R::Rdx);
        self.a.movzx8(R::Rax, R::Rax);
        self.a.movzx8(R::Rdx, R::Rdx);
        self.a.alu_rr(false, Alu::Sub, R::Rax, R::Rdx);
        self.a.movsxd(R::Rax, R::Rax);
        let d = self.alloc(&[]);
        self.a.mov_rr(true, d, R::Rax);
        self.push(Loc::Reg(d));
    }

    fn lcmp_reg(&mut self) {
        let b = self.pop_w(true);
        let a = self.pop_w(true);
        let ra = self.read(a, R::Rcx);
        let s = self.src(b, R::Rdx);
        self.alu_src(true, Alu::Cmp, ra, s);
        self.push_compare_result(Cond::G, Cond::L);
    }

    fn fcmp_reg(&mut self, op: u8) {
        let double = op >= 0x97;
        let nan = if op == 0x95 || op == 0x97 { -1 } else { 1 };
        let b = self.pop_w(double);
        let a = self.pop_w(double);
        let ra = self.read(a, R::Rax);
        self.a.mov_xr(double, X(0), ra);
        let rb = self.read(b, R::Rcx);
        self.a.mov_xr(double, X(1), rb);
        let (unordered, done) = (self.a.new_label(), self.a.new_label());
        self.a.fcmp(double, X(0), X(1));
        self.a.jcc(Cond::P, unordered);
        self.a.setcc(Cond::A, R::Rax);
        self.a.setcc(Cond::B, R::Rdx);
        self.a.movzx8(R::Rax, R::Rax);
        self.a.movzx8(R::Rdx, R::Rdx);
        self.a.alu_rr(false, Alu::Sub, R::Rax, R::Rdx);
        self.a.jmp(done);
        self.a.bind(unordered);
        self.a.mov_ri(R::Rax, nan);
        self.a.bind(done);
        self.a.movsxd(R::Rax, R::Rax);
        let d = self.alloc(&[]);
        self.a.mov_rr(true, d, R::Rax);
        self.push(Loc::Reg(d));
    }

    /// Conditional branches and `goto`: the operands are compared in registers and the rest of
    /// the stack is written back (branch targets start with the stack in memory).
    fn branch_reg(&mut self, pc: usize, op: u8) {
        let cond = match op {
            0x99..=0x9e | 0xc6 | 0xc7 => {
                let v = self.pop();
                let r = self.read(v, R::Rax);
                self.flush();
                let wide = op >= 0xc6;
                if wide {
                    self.a.test_rr(true, r, r);
                } else {
                    self.a.alu_ri(false, Alu::Cmp, r, 0);
                }
                match op {
                    0xc6 => Cond::E,
                    0xc7 => Cond::Ne,
                    _ => int_cond(op - 0x99),
                }
            }
            0x9f..=0xa6 => {
                let wide = op >= 0xa5;
                let b = self.pop();
                let a = self.pop();
                let ra = self.read(a, R::Rax);
                let s = self.src(b, R::Rcx);
                self.flush();
                self.alu_src(wide, Alu::Cmp, ra, s);
                match op {
                    0xa5 => Cond::E,
                    0xa6 => Cond::Ne,
                    _ => int_cond(op - 0x9f),
                }
            }
            _ => {
                self.flush();
                let target = rel(pc, i2(self.code, pc + 1) as i32);
                if target <= pc {
                    self.poll();
                    self.count_backedge(target);
                }
                return self.a.jmp(self.label(target));
            }
        };
        self.branch(pc, cond);
    }
}
