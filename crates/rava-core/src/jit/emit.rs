//! Per-instruction machine-code templates for the baseline compiler.

use super::analysis::{i2, i4};
use super::asm::{mem, mem_idx, Alu, Cond, Shift, M, R, X};
use super::compile::Emitter;
use super::runtime;
use crate::heap::{ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::klass::Klass;
use crate::link::Resolved;
use crate::types::BasicType;


impl Emitter {
    /// Emits instruction `pc` (operand stack `depth` deep, `len` bytes). `None` refuses the
    /// method.
    pub(super) fn emit(&mut self, pc: usize, depth: u16) -> Option<()> {
        let d = depth as i32;
        let op = self.code[pc];
        match op {
            0x00 => {}
            0x01 => self.a.store_imm(self.stk(d), 0),
            0x02..=0x08 => self.a.store_imm(self.stk(d), op as i32 - 3),
            0x09 | 0x0a => self.a.store_imm(self.stk(d), (op - 0x09) as i32),
            0x0b..=0x0d => self.const64(d, ((op - 0x0b) as f32).to_bits() as i64),
            0x0e | 0x0f => self.const64(d, ((op - 0x0e) as f64).to_bits() as i64),
            0x10 => self.a.store_imm(self.stk(d), self.code[pc + 1] as i8 as i32),
            0x11 => self.a.store_imm(self.stk(d), i2(self.code, pc + 1) as i32),
            0x12..=0x14 => self.ldc(pc, depth),
            0x15..=0x19 => self.copy(self.local(self.code[pc + 1] as usize), self.stk(d)),
            0x1a..=0x2d => self.copy(self.local(((op - 0x1a) % 4) as usize), self.stk(d)),
            0x2e..=0x35 => self.array_load(pc, depth, op),
            0x36..=0x3a => {
                let wide = op == 0x37 || op == 0x39;
                self.copy(self.stk(d - 1 - wide as i32), self.local(self.code[pc + 1] as usize));
            }
            0x3b..=0x4e => {
                let n = ((op - 0x3b) % 4) as usize;
                let wide = (0x3f..=0x42).contains(&op) || (0x47..=0x4a).contains(&op);
                self.copy(self.stk(d - 1 - wide as i32), self.local(n));
            }
            0x4f..=0x56 => self.array_store(pc, depth, op),
            0x57 | 0x58 => {}
            0x59 => self.copy(self.stk(d - 1), self.stk(d)),
            0x5a..=0x5f => self.stack_shuffle(op, d),
            0x60..=0x83 => self.arith(pc, depth, op)?,
            0x84 => {
                let n = self.code[pc + 1] as usize;
                self.iinc(n, self.code[pc + 2] as i8 as i32);
            }
            0x85..=0x93 => self.convert(op, d),
            0x94 => self.lcmp(d),
            0x95..=0x98 => self.fcmp(op, d),
            0x99..=0x9e => {
                self.a.load(false, R::Rax, self.stk(d - 1));
                self.a.alu_ri(false, Alu::Cmp, R::Rax, 0);
                self.branch(pc, int_cond(op - 0x99));
            }
            0x9f..=0xa4 => {
                self.a.load(false, R::Rax, self.stk(d - 2));
                self.a.alu_rm(false, Alu::Cmp, R::Rax, self.stk(d - 1));
                self.branch(pc, int_cond(op - 0x9f));
            }
            0xa5 | 0xa6 => {
                self.a.load(true, R::Rax, self.stk(d - 2));
                self.a.alu_rm(true, Alu::Cmp, R::Rax, self.stk(d - 1));
                self.branch(pc, if op == 0xa5 { Cond::E } else { Cond::Ne });
            }
            0xa7 => self.a.jmp(self.label(rel(pc, i2(self.code, pc + 1) as i32))),
            0xc8 => self.a.jmp(self.label(rel(pc, i4(self.code, pc + 1)))),
            0xaa | 0xab => self.switch(pc, d, op),
            0xac | 0xae | 0xb0 => {
                self.a.load(true, R::Rax, self.stk(d - 1));
                self.a.jmp(self.epilogue);
            }
            0xad | 0xaf => {
                self.a.load(true, R::Rax, self.stk(d - 2));
                self.a.jmp(self.epilogue);
            }
            0xb1 => {
                self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
                self.a.jmp(self.epilogue);
            }
            0xb2..=0xb5 => self.field(pc, depth, op),
            0xb6..=0xb9 => self.invoke(pc, depth, op),
            0xbe => {
                let slow = self.slow_path(pc, depth);
                self.a.load(true, R::Rax, self.stk(d - 1));
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, slow);
                self.a.load_sx(4, R::Rax, mem(R::Rax, ARRAY_LENGTH_OFFSET as i32));
                self.a.store(true, self.stk(d - 1), R::Rax);
            }
            0xc0 => self.checkcast(pc, depth),
            0xc1 => self.instanceof(pc, depth),
            0xba => self.invokedynamic(pc, depth),
            0xbb => self.new_object(pc, depth),
            0xbc | 0xbd => self.new_array(pc, depth, op),
            0xc6 | 0xc7 => {
                self.a.alu_mi(true, Alu::Cmp, self.stk(d - 1), 0);
                self.branch(pc, if op == 0xc6 { Cond::E } else { Cond::Ne });
            }
            0xc4 => self.wide(pc, d)?,
            0xa8 | 0xa9 | 0xc9 => return None,
            // new, newarray, anewarray, athrow, instanceof, monitors, multianewarray,
            // invokedynamic: the interpreter's implementation, on this frame.
            _ => self.defer(pc, depth),
        }
        Some(())
    }

    fn const64(&mut self, d: i32, v: i64) {
        self.a.mov_ri(R::Rax, v);
        self.a.store(true, self.stk(d), R::Rax);
    }

    fn copy(&mut self, from: M, to: M) {
        self.a.load(true, R::Rax, from);
        self.a.store(true, to, R::Rax);
    }

    fn branch(&mut self, pc: usize, c: Cond) {
        let target = rel(pc, i2(self.code, pc + 1) as i32);
        self.a.jcc(c, self.label(target));
    }

    fn iinc(&mut self, n: usize, delta: i32) {
        self.a.load(false, R::Rax, self.local(n));
        self.a.alu_ri(false, Alu::Add, R::Rax, delta);
        self.a.movsxd(R::Rax, R::Rax);
        self.a.store(true, self.local(n), R::Rax);
    }

    fn wide(&mut self, pc: usize, d: i32) -> Option<()> {
        let n = u16::from_be_bytes([self.code[pc + 2], self.code[pc + 3]]) as usize;
        match self.code[pc + 1] {
            0x15..=0x19 => self.copy(self.local(n), self.stk(d)),
            0x36 | 0x38 | 0x3a => self.copy(self.stk(d - 1), self.local(n)),
            0x37 | 0x39 => self.copy(self.stk(d - 2), self.local(n)),
            0x84 => self.iinc(n, i2(self.code, pc + 4) as i32),
            _ => return None,
        }
        Some(())
    }

    fn stack_shuffle(&mut self, op: u8, d: i32) {
        // Permute by copying the affected top slots through registers.
        let (take, order): (i32, &[i32]) = match op {
            0x5a => (2, &[1, 0, 1]),          // dup_x1: b a -> a b a
            0x5b => (3, &[2, 0, 1, 2]),       // dup_x2: c b a -> a c b a
            0x5c => (2, &[0, 1, 0, 1]),       // dup2: b a -> b a b a
            0x5d => (3, &[1, 2, 0, 1, 2]),    // dup2_x1: c b a -> b a c b a
            0x5e => (4, &[2, 3, 0, 1, 2, 3]), // dup2_x2: d c b a -> b a d c b a
            _ => (2, &[1, 0]),                // swap: b a -> a b
        };
        let base = d - take;
        let regs = [R::Rax, R::Rcx, R::Rdx, R::Rsi];
        for i in 0..take {
            self.a.load(true, regs[i as usize], self.stk(base + i));
        }
        for (k, &src) in order.iter().enumerate() {
            self.a.store(true, self.stk(base + k as i32), regs[src as usize]);
        }
    }

    fn ldc(&mut self, pc: usize, depth: u16) {
        let index = if self.code[pc] == 0x12 { self.code[pc + 1] as u16 } else { u16::from_be_bytes([self.code[pc + 1], self.code[pc + 2]]) };
        let d = depth as i32;
        match self.cf.pool[index as usize] {
            crate::classfile::Cp::Int(v) => self.a.store_imm(self.stk(d), v),
            crate::classfile::Cp::Float(v) => self.const64(d, v.to_bits() as i64),
            crate::classfile::Cp::Long(v) => self.const64(d, v),
            crate::classfile::Cp::Double(v) => self.const64(d, v.to_bits() as i64),
            _ => match self.m.holder.resolved[index as usize].get() {
                Some(Resolved::String(o)) | Some(Resolved::Constant(o)) if matches!(
                    self.cf.pool[index as usize],
                    crate::classfile::Cp::String(_) | crate::classfile::Cp::MethodType(_) | crate::classfile::Cp::MethodHandle { .. }
                ) => self.const64(d, *o as i64),
                _ => self.defer(pc, depth),
            },
        }
    }

    fn arith(&mut self, pc: usize, depth: u16, op: u8) -> Option<()> {
        let d = depth as i32;
        let a = &mut self.a;
        match op {
            // int add/sub/and/or/xor
            0x60 | 0x64 | 0x7e | 0x80 | 0x82 => {
                let alu = match op { 0x60 => Alu::Add, 0x64 => Alu::Sub, 0x7e => Alu::And, 0x80 => Alu::Or, _ => Alu::Xor };
                a.load(false, R::Rax, self_stk(self.max_locals, d - 2));
                a.alu_rm(false, alu, R::Rax, self_stk(self.max_locals, d - 1));
                a.movsxd(R::Rax, R::Rax);
                a.store(true, self_stk(self.max_locals, d - 2), R::Rax);
            }
            // long add/sub/and/or/xor
            0x61 | 0x65 | 0x7f | 0x81 | 0x83 => {
                let alu = match op { 0x61 => Alu::Add, 0x65 => Alu::Sub, 0x7f => Alu::And, 0x81 => Alu::Or, _ => Alu::Xor };
                a.load(true, R::Rax, self_stk(self.max_locals, d - 4));
                a.alu_rm(true, alu, R::Rax, self_stk(self.max_locals, d - 2));
                a.store(true, self_stk(self.max_locals, d - 4), R::Rax);
            }
            0x68 | 0x69 => {
                let (w, x, y) = if op == 0x68 { (false, d - 2, d - 1) } else { (true, d - 4, d - 2) };
                a.load(w, R::Rax, self_stk(self.max_locals, x));
                a.load(w, R::Rcx, self_stk(self.max_locals, y));
                a.imul_rr(w, R::Rax, R::Rcx);
                if !w {
                    a.movsxd(R::Rax, R::Rax);
                }
                a.store(true, self_stk(self.max_locals, x), R::Rax);
            }
            0x6c | 0x6d | 0x70 | 0x71 => {
                let w = op & 1 == 1;
                let (x, y) = if w { (d - 4, d - 2) } else { (d - 2, d - 1) };
                let slow = self.slow_path(pc, depth);
                let (special, done) = (self.a.new_label(), self.a.new_label());
                let a = &mut self.a;
                a.load(w, R::Rcx, self_stk(self.max_locals, y));
                a.test_rr(w, R::Rcx, R::Rcx);
                a.jcc(Cond::E, slow);
                a.load(w, R::Rax, self_stk(self.max_locals, x));
                a.alu_ri(w, Alu::Cmp, R::Rcx, -1);
                a.jcc(Cond::E, special);
                a.idiv(w, R::Rcx);
                if op >= 0x70 {
                    a.mov_rr(true, R::Rax, R::Rdx);
                }
                if !w {
                    a.movsxd(R::Rax, R::Rax);
                }
                a.jmp(done);
                // x / -1 = -x (wrapping), x % -1 = 0: idiv would trap on MIN / -1.
                a.bind(special);
                if op >= 0x70 {
                    a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
                } else {
                    a.neg(w, R::Rax);
                    if !w {
                        a.movsxd(R::Rax, R::Rax);
                    }
                }
                a.bind(done);
                a.store(true, self_stk(self.max_locals, x), R::Rax);
            }
            0x74 | 0x75 => {
                let (w, x) = if op == 0x74 { (false, d - 1) } else { (true, d - 2) };
                a.load(w, R::Rax, self_stk(self.max_locals, x));
                a.neg(w, R::Rax);
                if !w {
                    a.movsxd(R::Rax, R::Rax);
                }
                a.store(true, self_stk(self.max_locals, x), R::Rax);
            }
            0x78..=0x7d => {
                let w = op & 1 == 1;
                let x = if w { d - 3 } else { d - 2 };
                let sh = match op { 0x78 | 0x79 => Shift::Shl, 0x7a | 0x7b => Shift::Sar, _ => Shift::Shr };
                a.load(w, R::Rax, self_stk(self.max_locals, x));
                a.load(false, R::Rcx, self_stk(self.max_locals, d - 1));
                a.shift_cl(w, sh, R::Rax);
                if !w {
                    a.movsxd(R::Rax, R::Rax);
                }
                a.store(true, self_stk(self.max_locals, x), R::Rax);
            }
            // float / double + - * /
            0x62 | 0x63 | 0x66 | 0x67 | 0x6a | 0x6b | 0x6e | 0x6f => {
                let double = op & 1 == 1;
                let (x, y) = if double { (d - 4, d - 2) } else { (d - 2, d - 1) };
                let sse = match op { 0x62 | 0x63 => 0x58, 0x66 | 0x67 => 0x5C, 0x6a | 0x6b => 0x59, _ => 0x5E };
                a.fload(double, X(0), self_stk(self.max_locals, x));
                a.farith_m(double, sse, X(0), self_stk(self.max_locals, y));
                a.fstore(double, self_stk(self.max_locals, x), X(0));
            }
            0x72 | 0x73 => {
                let double = op == 0x73;
                let (x, y) = if double { (d - 4, d - 2) } else { (d - 2, d - 1) };
                a.load(true, R::Rdi, self_stk(self.max_locals, x));
                a.load(true, R::Rsi, self_stk(self.max_locals, y));
                a.call_abs(if double { runtime::drem as *const () as usize } else { runtime::frem as *const () as usize });
                a.store(true, self_stk(self.max_locals, x), R::Rax);
            }
            0x76 | 0x77 => {
                let (x, bit) = if op == 0x76 { (d - 1, 1i64 << 31) } else { (d - 2, i64::MIN) };
                a.load(true, R::Rax, self_stk(self.max_locals, x));
                a.mov_ri(R::Rcx, bit);
                a.alu_rr(true, Alu::Xor, R::Rax, R::Rcx);
                a.store(true, self_stk(self.max_locals, x), R::Rax);
            }
            _ => return None,
        }
        Some(())
    }

    fn convert(&mut self, op: u8, d: i32) {
        let ml = self.max_locals;
        let a = &mut self.a;
        match op {
            0x85 => {} // i2l: an int slot is already sign-extended
            0x88 => {
                a.load(true, R::Rax, self_stk(ml, d - 2));
                a.movsxd(R::Rax, R::Rax);
                a.store(true, self_stk(ml, d - 2), R::Rax);
            }
            0x86 | 0x87 | 0x89 | 0x8a => {
                let (w, src) = if op <= 0x87 { (false, d - 1) } else { (true, d - 2) };
                let double = op == 0x87 || op == 0x8a;
                a.load(w, R::Rax, self_stk(ml, src));
                a.cvt_i2f(double, w, X(0), R::Rax);
                a.store_imm(self_stk(ml, src), 0);
                a.fstore(double, self_stk(ml, src), X(0));
            }
            0x8d | 0x90 => {
                let to_double = op == 0x8d;
                let src = if to_double { d - 1 } else { d - 2 };
                a.fload(!to_double, X(0), self_stk(ml, src));
                a.cvt_f2f(to_double, X(0), X(0));
                a.store_imm(self_stk(ml, src), 0);
                a.fstore(to_double, self_stk(ml, src), X(0));
            }
            0x8b | 0x8c | 0x8e | 0x8f => {
                let src = if op <= 0x8c { d - 1 } else { d - 2 };
                let f = match op { 0x8b => runtime::f2i as *const () as usize, 0x8c => runtime::f2l as *const () as usize, 0x8e => runtime::d2i as *const () as usize, _ => runtime::d2l as *const () as usize };
                a.load(true, R::Rdi, self_stk(ml, src));
                a.call_abs(f);
                a.store(true, self_stk(ml, src), R::Rax);
            }
            _ => {
                // i2b, i2c, i2s
                a.load(true, R::Rax, self_stk(ml, d - 1));
                match op {
                    0x91 => a.movsx_rr(1, R::Rax, R::Rax),
                    0x92 => a.movzx16_rr(R::Rax, R::Rax),
                    _ => a.movsx_rr(2, R::Rax, R::Rax),
                }
                a.store(true, self_stk(ml, d - 1), R::Rax);
            }
        }
    }

    fn lcmp(&mut self, d: i32) {
        let ml = self.max_locals;
        let a = &mut self.a;
        a.load(true, R::Rcx, self_stk(ml, d - 4));
        a.alu_rm(true, Alu::Cmp, R::Rcx, self_stk(ml, d - 2));
        a.setcc(Cond::G, R::Rax);
        a.setcc(Cond::L, R::Rdx);
        a.movzx8(R::Rax, R::Rax);
        a.movzx8(R::Rdx, R::Rdx);
        a.alu_rr(false, Alu::Sub, R::Rax, R::Rdx);
        a.movsxd(R::Rax, R::Rax);
        a.store(true, self_stk(ml, d - 4), R::Rax);
    }

    fn fcmp(&mut self, op: u8, d: i32) {
        let double = op >= 0x97;
        let nan = if op == 0x95 || op == 0x97 { -1 } else { 1 };
        let (x, y) = if double { (d - 4, d - 2) } else { (d - 2, d - 1) };
        let ml = self.max_locals;
        let done = self.a.new_label();
        let a = &mut self.a;
        a.fload(double, X(0), self_stk(ml, x));
        a.fload(double, X(1), self_stk(ml, y));
        a.mov_ri(R::Rax, nan);
        a.fcmp(double, X(0), X(1));
        a.jcc(Cond::P, done);
        a.setcc(Cond::A, R::Rax);
        a.setcc(Cond::B, R::Rdx);
        a.movzx8(R::Rax, R::Rax);
        a.movzx8(R::Rdx, R::Rdx);
        a.alu_rr(false, Alu::Sub, R::Rax, R::Rdx);
        a.bind(done);
        a.movsxd(R::Rax, R::Rax);
        a.store(true, self_stk(ml, x), R::Rax);
    }

    fn switch(&mut self, pc: usize, d: i32, op: u8) {
        let p = (pc + 4) & !3;
        let default = self.label(rel(pc, i4(self.code, p)));
        self.a.load(false, R::Rax, self.stk(d - 1));
        if op == 0xaa {
            let (low, high) = (i4(self.code, p + 4), i4(self.code, p + 8));
            for (i, key) in (low..=high).enumerate() {
                self.a.alu_ri(false, Alu::Cmp, R::Rax, key);
                self.a.jcc(Cond::E, self.label(rel(pc, i4(self.code, p + 12 + 4 * i))));
            }
        } else {
            for i in 0..i4(self.code, p + 4) as usize {
                self.a.alu_ri(false, Alu::Cmp, R::Rax, i4(self.code, p + 8 + 8 * i));
                self.a.jcc(Cond::E, self.label(rel(pc, i4(self.code, p + 12 + 8 * i))));
            }
        }
        self.a.jmp(default);
    }

    /// Loads `array` into rax and `index` into rcx, bounds-checked (else the slow path throws).
    fn array_access(&mut self, pc: usize, depth: u16, array_slot: i32) {
        let slow = self.slow_path(pc, depth);
        self.a.load(true, R::Rax, self.stk(array_slot));
        self.a.load(false, R::Rcx, self.stk(array_slot + 1));
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, slow);
        self.a.alu_rm(false, Alu::Cmp, R::Rcx, mem(R::Rax, ARRAY_LENGTH_OFFSET as i32));
        self.a.jcc(Cond::Ae, slow);
    }

    fn array_load(&mut self, pc: usize, depth: u16, op: u8) {
        let d = depth as i32;
        self.array_access(pc, depth, d - 2);
        let base = ARRAY_BASE_OFFSET as i32;
        let a = &mut self.a;
        match op {
            0x2e => a.load_sx(4, R::Rax, mem_idx(R::Rax, R::Rcx, 4, base)),
            0x30 => a.load(false, R::Rax, mem_idx(R::Rax, R::Rcx, 4, base)),
            0x2f | 0x31 | 0x32 => a.load(true, R::Rax, mem_idx(R::Rax, R::Rcx, 8, base)),
            0x33 => a.load_sx(1, R::Rax, mem_idx(R::Rax, R::Rcx, 1, base)),
            0x34 => a.load_zx(2, R::Rax, mem_idx(R::Rax, R::Rcx, 2, base)),
            _ => a.load_sx(2, R::Rax, mem_idx(R::Rax, R::Rcx, 2, base)),
        }
        a.store(true, self_stk(self.max_locals, d - 2), R::Rax);
    }

    fn array_store(&mut self, pc: usize, depth: u16, op: u8) {
        let d = depth as i32;
        let wide = op == 0x50 || op == 0x52;
        let array_slot = d - 3 - wide as i32;
        let value = self.stk(d - 1 - wide as i32);
        self.array_access(pc, depth, array_slot);
        let base = ARRAY_BASE_OFFSET as i32;
        self.a.load(true, R::Rdx, value);
        match op {
            0x4f | 0x51 => self.a.store(false, mem_idx(R::Rax, R::Rcx, 4, base), R::Rdx),
            0x50 | 0x52 => self.a.store(true, mem_idx(R::Rax, R::Rcx, 8, base), R::Rdx),
            0x53 => {
                let (store, slow) = (self.a.new_label(), self.slow_path(pc, depth));
                self.a.test_rr(true, R::Rdx, R::Rdx);
                self.a.jcc(Cond::E, store);
                let (array, index) = (self.stk(array_slot), self.stk(array_slot + 1));
                self.aastore_check(array, index, value, slow);
                self.a.bind(store);
                self.a.store(true, mem_idx(R::Rax, R::Rcx, 8, base), R::Rdx);
            }
            0x54 => {
                // boolean[] stores value & 1; byte[] stores the low byte.
                let bools = crate::vm::vm().primitive_array_class(BasicType::Boolean).ok().map_or(0, |k| k as *const Klass as i64);
                let plain = self.a.new_label();
                self.a.mov_ri(R::Rsi, bools);
                self.a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::Rax, 0));
                self.a.jcc(Cond::Ne, plain);
                self.a.alu_ri(false, Alu::And, R::Rdx, 1);
                self.a.bind(plain);
                self.a.store8(mem_idx(R::Rax, R::Rcx, 1, base), R::Rdx);
            }
            _ => self.a.store16(mem_idx(R::Rax, R::Rcx, 2, base), R::Rdx),
        }
    }

}

pub(super) fn self_stk(max_locals: i32, i: i32) -> M {
    mem(R::R13, 8 * (max_locals + crate::thread::header::SLOTS as i32 + i))
}

pub(super) fn rel(pc: usize, off: i32) -> usize {
    (pc as i64 + off as i64) as usize
}

fn int_cond(c: u8) -> Cond {
    [Cond::E, Cond::Ne, Cond::L, Cond::Ge, Cond::G, Cond::Le][c as usize]
}
