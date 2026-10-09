//! `System.arraycopy` into a primitive array whose class is known (a fresh `new byte[]`, as
//! string concatenation and `String.getBytes` make), compiled in place as C2's arraycopy
//! intrinsic is: the source's class, the signs and both bounds are checked inline (a failure
//! deoptimizes before anything is written, for the interpreter to throw or copy), and the
//! bytes move with unaligned loads covering the range from both ends, all loads before any
//! store, so an overlapping range copies correctly. Each site's lengths then predict its own
//! branches; only a copy longer than 16 bytes calls `memmove`. The runtime helper, shared by
//! every site, paid a call and a jump table its callers' lengths kept mispredicting.

use super::codegen::{Arg, Gen};
use super::ir::*;
use crate::heap::{ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::jit::asm::{mem, mem_idx, Alu, Cond, Label, Shift, R};
use crate::klass::{Klass, KlassKind};

/// The class of the destination when it is a primitive array allocated in this code, or (with
/// `refs`) a reference array.
pub(super) fn known_destination(g: &Graph, src: V, dst: V, refs: bool) -> Option<&'static Klass> {
    let mut d = g.resolve(dst);
    if let Inst::CheckCast(x, ..) = *g.inst(d) {
        d = g.resolve(x);
    }
    let Inst::NewArray(k, ..) = *g.inst(d) else { return None };
    let kind = match k.kind {
        KlassKind::TypeArray { .. } => true,
        KlassKind::ObjArray { .. } => refs,
        _ => false,
    };
    (kind && g.resolve(src) != d).then_some(k)
}

/// Whether int `v` is never negative by its definition: an array length, or masked or shifted
/// down to a non-negative range.
fn non_negative(g: &Graph, v: V) -> bool {
    match *g.inst(g.resolve(v)) {
        Inst::ArrayLength(_) => true,
        Inst::Bin(BinOp::And, a, b) => [a, b].into_iter().any(|x| g.constant(g.resolve(x)).is_some_and(|c| c as i32 >= 0)),
        Inst::Bin(BinOp::Ushr, _, b) => g.constant(g.resolve(b)).is_some_and(|c| c & 31 != 0),
        _ => false,
    }
}

impl Gen<'_> {
    /// `arraycopy(src, sp, dst, dp, len)` into `dst` of class `k` (`known_destination`).
    pub(super) fn array_copy_inline(&mut self, args: &[V], k: &'static Klass, fail: Label) {
        let [src, sp, dst, dp, len] = args[..] else { unreachable!("arraycopy takes five arguments") };
        let es = k.elem_size() as u8;
        let scale = es.trailing_zeros() as u8;
        // The source: non-null and of the destination's class; a constant one (a literal's
        // bytes) is checked here, its length known.
        let constant = self.int_const(src).filter(|&o| o != 0 && std::ptr::eq(crate::jit::fold::class_of(o as usize), k));
        let src_len = constant.map(|o| crate::jit::fold::array_length(o as usize) as i64);
        if constant.is_none() {
            let rs = self.reg(src, R::Rax);
            self.a.test_rr(true, rs, rs);
            self.a.jcc(Cond::E, fail);
            let addr = k as *const Klass as usize;
            if crate::low::is_low(addr) {
                self.a.alu_mi(false, Alu::Cmp, mem(rs, 0), addr as i32);
            } else {
                self.a.mov_ri(R::Rdx, addr as i64);
                self.a.alu_rm(false, Alu::Cmp, R::Rdx, mem(rs, 0));
            }
            self.a.jcc(Cond::Ne, fail);
        }
        // No negative position or length.
        for x in [sp, dp, len] {
            if self.int_const(x).is_none() && !non_negative(self.g, x) {
                let r = self.reg(x, R::Rcx);
                self.a.test_rr(false, r, r);
                self.a.jcc(Cond::S, fail);
            }
        }
        // Both ranges in bounds (sums of two non-negative ints fit in 32 unsigned bits); a copy of
        // the whole source from its start needs no check there.
        let whole = self.int_const(sp) == Some(0) && matches!(*self.g.inst(self.g.resolve(len)), Inst::ArrayLength(a) if self.g.resolve(a) == self.g.resolve(src));
        if !whole {
            match (self.int_const(sp), self.int_const(len), src_len) {
                (Some(p), Some(n), Some(l)) => {
                    if (p as u32 as i64) + (n as u32 as i64) > l {
                        self.a.jmp(fail);
                    }
                }
                _ => {
                    self.end_within(sp, len, src, src_len, fail);
                }
            }
        }
        self.end_within(dp, len, dst, None, fail);
        // from = rcx, to = r11, bytes = rdx.
        let base = ARRAY_BASE_OFFSET as i32;
        let rs = self.reg(src, R::Rax);
        match self.int_const(sp) {
            Some(c) => self.a.lea(R::Rcx, mem(rs, base + c as i32 * es as i32)),
            None => {
                self.zext(R::Rcx, sp);
                self.a.lea(R::Rcx, mem_idx(rs, R::Rcx, es, base));
            }
        }
        let rd = self.reg(dst, R::R11);
        match self.int_const(dp) {
            Some(c) => self.a.lea(R::R11, mem(rd, base + c as i32 * es as i32)),
            None => {
                self.zext(R::Rax, dp);
                let rd = self.reg(dst, R::R11);
                self.a.lea(R::R11, mem_idx(rd, R::Rax, es, base));
            }
        }
        match self.int_const(len) {
            Some(n) => self.copy_fixed(n as usize * es as usize),
            None => {
                self.zext(R::Rdx, len);
                if scale > 0 {
                    self.a.shift_ri(true, Shift::Shl, R::Rdx, scale);
                }
                self.copy_variable();
            }
        }
    }

    /// Fails unless `pos + len` (both non-negative) is at most the length of array `a`, known
    /// (`known`) or in its header.
    fn end_within(&mut self, pos: V, len: V, a: V, known: Option<i64>, fail: Label) {
        let len_ = ARRAY_LENGTH_OFFSET as i32;
        match (self.int_const(pos), self.int_const(len), known) {
            (Some(p), Some(n), None) if (p as u32 as i64) + (n as u32 as i64) <= i32::MAX as i64 => {
                let ra = self.reg(a, R::R11);
                self.a.alu_mi(false, Alu::Cmp, mem(ra, len_), (p as u32 as i64 + n as u32 as i64) as i32);
                self.a.jcc(Cond::B, fail);
            }
            _ => {
                self.zext(R::Rcx, pos);
                self.zext(R::Rdx, len);
                self.a.alu_rr(false, Alu::Add, R::Rcx, R::Rdx);
                match known {
                    Some(l) => self.a.alu_ri(false, Alu::Cmp, R::Rcx, l as i32),
                    None => {
                        let ra = self.reg(a, R::R11);
                        self.a.alu_rm(false, Alu::Cmp, R::Rcx, mem(ra, len_));
                    }
                }
                self.a.jcc(Cond::A, fail);
            }
        }
    }

    /// The int constant `v` is, if any.
    fn int_const(&self, v: V) -> Option<i64> {
        self.g.constant_value(v)
    }

    /// `r := v` zero-extended from 32 bits.
    fn zext(&mut self, r: R, v: V) {
        match self.int_const(v) {
            Some(c) => self.a.mov_ri(r, c as u32 as i64),
            // One 32-bit move from wherever the value is (a 32-bit write clears the top half).
            None => match self.loc[v as usize] {
                super::regalloc::Loc::Reg(src) => self.a.mov_rr(false, r, src),
                _ => {
                    self.load(r, v);
                    self.a.mov_rr(false, r, r);
                }
            },
        }
    }

    /// Copies `n` bytes from rcx to r11 (rax and rdx free).
    fn copy_fixed(&mut self, n: usize) {
        let (f, t) = (R::Rcx, R::R11);
        let n32 = n as i32;
        match n {
            0 => {}
            1 => {
                self.a.load_zx(1, R::Rax, mem(f, 0));
                self.a.store8(mem(t, 0), R::Rax);
            }
            2 => {
                self.a.load_zx(2, R::Rax, mem(f, 0));
                self.a.store16(mem(t, 0), R::Rax);
            }
            3 => {
                self.a.load_zx(2, R::Rax, mem(f, 0));
                self.a.load_zx(1, R::Rdx, mem(f, 2));
                self.a.store16(mem(t, 0), R::Rax);
                self.a.store8(mem(t, 2), R::Rdx);
            }
            4..=8 => {
                let w = n >= 8;
                let size = if w { 8 } else { 4 };
                self.a.load(w, R::Rax, mem(f, 0));
                self.a.load(w, R::Rdx, mem(f, n32 - size));
                self.a.store(w, mem(t, 0), R::Rax);
                self.a.store(w, mem(t, n32 - size), R::Rdx);
            }
            9..=16 => {
                self.a.load(true, R::Rax, mem(f, 0));
                self.a.load(true, R::Rdx, mem(f, n32 - 8));
                self.a.store(true, mem(t, 0), R::Rax);
                self.a.store(true, mem(t, n32 - 8), R::Rdx);
            }
            _ => {
                self.a.mov_ri(R::Rdx, n as i64);
                self.call_memmove();
            }
        }
    }

    /// Copies rdx bytes from rcx to r11 (rax free).
    fn copy_variable(&mut self) {
        let (f, t, n) = (R::Rcx, R::R11, R::Rdx);
        let [small, tiny, long, done] = [(); 4].map(|_| self.a.new_label());
        self.a.alu_ri(true, Alu::Cmp, n, 8);
        self.a.jcc(Cond::B, small);
        self.a.alu_ri(true, Alu::Cmp, n, 16);
        self.a.jcc(Cond::A, long);
        // 8..=16: the first and the last eight bytes.
        self.a.load(true, R::Rax, mem(f, 0));
        self.a.load(true, f, mem_idx(f, n, 1, -8));
        self.a.store(true, mem(t, 0), R::Rax);
        self.a.store(true, mem_idx(t, n, 1, -8), f);
        self.a.jmp(done);
        self.a.bind(small);
        self.a.alu_ri(true, Alu::Cmp, n, 4);
        self.a.jcc(Cond::B, tiny);
        // 4..=7: the first and the last four.
        self.a.load(false, R::Rax, mem(f, 0));
        self.a.load(false, f, mem_idx(f, n, 1, -4));
        self.a.store(false, mem(t, 0), R::Rax);
        self.a.store(false, mem_idx(t, n, 1, -4), f);
        self.a.jmp(done);
        self.a.bind(tiny);
        // 1..=3: the first byte, and the last two from two bytes on.
        let one = self.a.new_label();
        self.a.test_rr(true, n, n);
        self.a.jcc(Cond::E, done);
        self.a.load_zx(1, R::Rax, mem(f, 0));
        self.a.alu_ri(true, Alu::Cmp, n, 1);
        self.a.jcc(Cond::E, one);
        self.a.load_zx(2, f, mem_idx(f, n, 1, -2));
        self.a.store8(mem(t, 0), R::Rax);
        self.a.store16(mem_idx(t, n, 1, -2), f);
        self.a.jmp(done);
        self.a.bind(one);
        self.a.store8(mem(t, 0), R::Rax);
        self.a.jmp(done);
        self.a.bind(long);
        self.call_memmove();
        self.a.bind(done);
    }

    /// `memmove(r11, rcx, rdx)` with the live registers preserved.
    fn call_memmove(&mut self) {
        self.call_preserving(copy_bytes as *const () as usize, &[Arg::Reg(R::R11), Arg::Reg(R::Rcx), Arg::Reg(R::Rdx)]);
    }
}

/// `memmove` for compiled code (no collection, no checks: the caller checked the ranges).
extern "sysv64" fn copy_bytes(to: *mut u8, from: *const u8, n: usize) {
    // SAFETY: the compiled caller checked both ranges against their arrays.
    unsafe { std::ptr::copy(from, to, n) }
}
