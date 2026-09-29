//! Code generation of intrinsics (`jit/intrinsics.rs`): `Math` rounding, square root, min, max
//! and abs, `Unsafe` memory access, `Class.isInstance` and `Object.getClass`.

use super::codegen::{Arg, Gen};
use super::ir::*;
use crate::jit::asm::{mem, mem_idx, Alu, Cond, R, X};
use crate::jit::emit_obj::{load_typed_to, store_typed_from};
use crate::jit::intrinsics::Intrinsic;
use crate::jit::runtime;
use crate::klass::Klass;
use crate::types::BasicType;
use std::mem::offset_of;
use std::rc::Rc;

impl Gen<'_> {
    pub(super) fn intrinsic(&mut self, v: V, k: Intrinsic, args: &[V], s: Option<&Rc<State>>) {
        match k {
            Intrinsic::Round(mode) => {
                let a = self.xreg(args[0], X(1));
                let r = self.xout(v);
                self.a.round_sd(r, a, mode);
                self.xput(v, r);
            }
            Intrinsic::Sqrt => {
                let a = self.xreg(args[0], X(1));
                let r = self.xout(v);
                self.a.fsqrt(r, a);
                self.xput(v, r);
            }
            // A null base makes the offset an absolute address.
            Intrinsic::UnsafeGet(ty) => {
                let (b, o) = (self.reg(args[1], R::Rax), self.reg(args[2], R::Rcx));
                let r = self.out(v);
                load_typed_to(&mut self.a, r, ty, mem_idx(b, o, 1, 0));
                self.put(v, r);
            }
            Intrinsic::UnsafePut(ty, volatile) => {
                let (b, o) = (self.reg(args[1], R::Rax), self.reg(args[2], R::Rcx));
                self.load(R::Rdx, args[3]);
                store_typed_from(&mut self.a, R::Rdx, ty, mem_idx(b, o, 1, 0));
                if volatile {
                    self.a.mfence();
                }
            }
            Intrinsic::IsInstance => self.is_instance(v, args[0], args[1]),
            Intrinsic::GetClass => {
                let fail = self.deopt(s.expect("getClass has a deopt state"));
                let o = self.reg(args[0], R::Rax);
                self.a.load(true, R::Rax, mem(o, 0));
                self.a.load(true, R::Rax, mem(R::Rax, offset_of!(Klass, mirror) as i32));
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, fail);
                self.put(v, R::Rax);
            }
            Intrinsic::MinMax(ty, max) => self.min_max(v, ty, max, args[0], args[1]),
            Intrinsic::Abs(ty) => self.abs(v, ty, args[0]),
        }
    }

    /// `mirror.isInstance(o)` for a non-null mirror: null → false, a primitive mirror → false,
    /// the exact class → true, else the subtype check.
    fn is_instance(&mut self, v: V, mirror: V, obj: V) {
        let done = self.a.new_label();
        let m = self.reg(mirror, R::Rcx);
        self.a.load(true, R::Rcx, mem(m, crate::runtime::mirror_klass_offset() as i32));
        let o = self.reg(obj, R::Rdx);
        self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        self.a.test_rr(true, o, o);
        self.a.jcc(Cond::E, done);
        self.a.test_rr(true, R::Rcx, R::Rcx);
        self.a.jcc(Cond::E, done);
        self.a.load(true, R::Rdx, mem(o, 0));
        self.a.mov_ri(R::Rax, 1);
        self.a.alu_rr(true, Alu::Cmp, R::Rdx, R::Rcx);
        self.a.jcc(Cond::E, done);
        self.call_preserving(runtime::subtype as *const () as usize, &[Arg::Reg(R::Rdx), Arg::Reg(R::Rcx)]);
        self.a.bind(done);
        self.put(v, R::Rax);
    }

    /// `Math.min/max` with Java semantics: NaN wins, and -0.0 is below +0.0 (for equal floats
    /// the bitwise and/or of the operands picks the right zero).
    fn min_max(&mut self, v: V, ty: BasicType, max: bool, x: V, y: V) {
        let wide = matches!(ty, BasicType::Long | BasicType::Double);
        self.load(R::Rax, x);
        self.load(R::Rcx, y);
        if matches!(ty, BasicType::Int | BasicType::Long) {
            self.a.alu_rr(wide, Alu::Cmp, R::Rax, R::Rcx);
            self.a.cmov(true, if max { Cond::L } else { Cond::G }, R::Rax, R::Rcx);
        } else {
            let (nan, a_above, b_above, done) = (self.a.new_label(), self.a.new_label(), self.a.new_label(), self.a.new_label());
            self.a.mov_xr(wide, X(0), R::Rax);
            self.a.mov_xr(wide, X(1), R::Rcx);
            self.a.fcmp(wide, X(0), X(1));
            self.a.jcc(Cond::P, nan);
            self.a.jcc(Cond::A, a_above);
            self.a.jcc(Cond::B, b_above);
            self.a.alu_rr(wide, if max { Alu::And } else { Alu::Or }, R::Rax, R::Rcx);
            self.a.jmp(done);
            self.a.bind(nan);
            self.a.farith_rr(wide, 0x58, X(0), X(1));
            self.a.mov_rx(wide, R::Rax, X(0));
            self.a.jmp(done);
            // The larger operand is kept for max, the other one for min.
            self.a.bind(if max { b_above } else { a_above });
            self.a.mov_rr(true, R::Rax, R::Rcx);
            self.a.bind(if max { a_above } else { b_above });
            self.a.bind(done);
        }
        self.put(v, R::Rax);
    }

    /// `Math.abs`: integers negate when negative (MIN_VALUE stays), floats clear the sign bit.
    fn abs(&mut self, v: V, ty: BasicType, x: V) {
        if matches!(ty, BasicType::Float | BasicType::Double) {
            let r = self.xout(v);
            self.xload(r, x);
            self.xmask(if ty == BasicType::Double { i64::MAX } else { 0x7fff_ffff });
            self.a.fand(ty == BasicType::Double, r, X(1));
            return self.xput(v, r);
        }
        self.load(R::Rax, x);
        match ty {
            BasicType::Int | BasicType::Long => {
                let wide = ty == BasicType::Long;
                self.a.mov_rr(true, R::Rcx, R::Rax);
                self.a.neg(wide, R::Rcx);
                self.a.cmov(wide, Cond::S, R::Rcx, R::Rax);
                if wide {
                    self.a.mov_rr(true, R::Rax, R::Rcx);
                } else {
                    self.a.movsxd(R::Rax, R::Rcx);
                }
            }
            BasicType::Float => self.a.alu_ri(false, Alu::And, R::Rax, i32::MAX),
            _ => {
                self.a.mov_ri(R::Rcx, i64::MAX);
                self.a.alu_rr(true, Alu::And, R::Rax, R::Rcx);
            }
        }
        self.put(v, R::Rax);
    }
}
