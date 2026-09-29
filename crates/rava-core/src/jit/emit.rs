//! Per-instruction machine-code templates for the baseline compiler.

use super::analysis::{i2, i4};
use super::asm::{mem, mem_idx, Alu, Cond, M, R};
use super::compile::Emitter;
use super::runtime;
use crate::heap::{ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::klass::Klass;
use crate::link::Resolved;
use crate::types::BasicType;


impl Emitter {
    /// Emits instruction `pc` (operand stack `depth` deep). Register templates come first; any
    /// other instruction runs a memory template on a written-back stack. `None` refuses the
    /// method.
    pub(super) fn emit(&mut self, pc: usize, depth: u16) -> Option<()> {
        debug_assert_eq!(self.vs.len(), depth as usize, "virtual stack out of step at {pc}");
        if self.emit_reg(pc, depth)? {
            return Some(());
        }
        if matches!(self.code[pc], 0xb6..=0xba) {
            self.site_consts = self.vs.iter().map(|s| if let super::vstack::Loc::Imm(v) = s.loc { Some(v) } else { None }).collect();
        }
        self.flush_to_mem();
        self.emit_memory(pc, depth)?;
        let next = pc + super::analysis::insn_len(self.code, pc);
        let after = self.an.depth.get(next).copied().flatten().unwrap_or(depth);
        self.vs_reset(after);
        Some(())
    }

    /// The memory templates: operands and results in frame slots.
    fn emit_memory(&mut self, pc: usize, depth: u16) -> Option<()> {
        let d = depth as i32;
        let op = self.code[pc];
        match op {
            0x12..=0x14 => self.ldc(pc, depth),
            0x53 => self.array_store(pc, depth, op),
            0x5a..=0x5f => self.stack_shuffle(op, d),
            0x6c | 0x6d | 0x70 | 0x71 | 0x72 | 0x73 => self.arith(pc, depth, op)?,
            0xa8 | 0xa9 | 0xc4 | 0xc9 => return None,
            0xc8 => {
                if i4(self.code, pc + 1) <= 0 {
                    self.poll();
                    self.count_backedge(rel(pc, i4(self.code, pc + 1)));
                }
                self.a.jmp(self.label(rel(pc, i4(self.code, pc + 1))));
            }
            0xb2..=0xb5 => self.field(pc, depth, op),
            0xb6..=0xb9 => self.invoke(pc, depth, op),
            0xc0 => self.checkcast(pc, depth),
            0xc1 => self.instanceof(pc, depth),
            0xba => self.invokedynamic(pc, depth),
            0xbb => self.new_object(pc, depth),
            0xbc | 0xbd => self.new_array(pc, depth, op),
            // athrow, monitors, multianewarray and anything unresolved: the interpreter's
            // implementation, on this frame.
            _ => self.defer(pc, depth),
        }
        Some(())
    }

    fn const64(&mut self, d: i32, v: i64) {
        self.a.mov_ri(R::Rax, v);
        self.a.store(true, self.stk(d), R::Rax);
    }

    pub(super) fn branch(&mut self, pc: usize, c: Cond) {
        let target = rel(pc, i2(self.code, pc + 1) as i32);
        if target <= pc {
            // Poll only when the backward branch is taken.
            let skip = self.a.new_label();
            self.a.jcc(invert(c), skip);
            self.poll();
            self.count_backedge(target);
            self.a.jmp(self.label(target));
            self.a.bind(skip);
        } else {
            self.a.jcc(c, self.label(target));
        }
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
                _ => self.defer_unresolved(pc, depth),
            },
        }
    }

    /// Division and remainder (a zero divisor takes the slow path; float remainder calls fmod).
    fn arith(&mut self, pc: usize, depth: u16, op: u8) -> Option<()> {
        let d = depth as i32;
        let a = &mut self.a;
        match op {
            0x6c | 0x6d | 0x70 | 0x71 => {
                let w = op & 1 == 1;
                let (x, y) = if w { (d - 4, d - 2) } else { (d - 2, d - 1) };
                let slow = self.slow_path(pc, depth);
                let (special, done) = (self.a.new_label(), self.a.new_label());
                let a = &mut self.a;
                a.load(w, R::Rcx, self_stk(self.stack_base, y));
                a.test_rr(w, R::Rcx, R::Rcx);
                a.jcc(Cond::E, slow);
                a.load(w, R::Rax, self_stk(self.stack_base, x));
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
                a.store(true, self_stk(self.stack_base, x), R::Rax);
            }
            0x72 | 0x73 => {
                let double = op == 0x73;
                let (x, y) = if double { (d - 4, d - 2) } else { (d - 2, d - 1) };
                a.load(true, R::Rdi, self_stk(self.stack_base, x));
                a.load(true, R::Rsi, self_stk(self.stack_base, y));
                a.call_abs(if double { runtime::drem as *const () as usize } else { runtime::frem as *const () as usize });
                a.store(true, self_stk(self.stack_base, x), R::Rax);
            }
            _ => return None,
        }
        Some(())
    }

    /// `tableswitch` / `lookupswitch` on the key in rax (the stack already written back).
    pub(super) fn switch_rax(&mut self, pc: usize, op: u8) {
        let p = (pc + 4) & !3;
        let default = self.label(rel(pc, i4(self.code, p)));
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

/// Operand slot `i` of a stack starting at frame slot `base` (for use while `self.a` is borrowed).
pub(super) fn self_stk(base: i32, i: i32) -> M {
    mem(R::R13, 8 * (base + i))
}

pub(super) fn rel(pc: usize, off: i32) -> usize {
    (pc as i64 + off as i64) as usize
}

fn invert(c: Cond) -> Cond {
    // Condition codes pair up: flipping the low bit negates the condition.
    // SAFETY: every value 0..16 is a `Cond`.
    unsafe { std::mem::transmute::<u8, Cond>(c as u8 ^ 1) }
}

pub(super) fn int_cond(c: u8) -> Cond {
    [Cond::E, Cond::Ne, Cond::L, Cond::Ge, Cond::G, Cond::Le][c as usize]
}
