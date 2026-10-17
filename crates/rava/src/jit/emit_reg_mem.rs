//! Register templates for heap access: primitive and reference array loads, primitive array
//! stores, `arraylength` and resolved field access. Instructions that can fail leave the stack
//! in registers: their slow path writes it back before the interpreter re-executes the
//! instruction (`lazy_flush`).

use super::asm::{mem, mem_idx, Alu, Cond, R};
use super::compile::Emitter;
use super::emit_obj::{encode_ref, load_static_to, load_typed_to, normalize_into, store_typed_from};
use super::vstack::Loc;
use crate::heap::{ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::klass::{InitState, Klass};
use crate::link::Resolved;
use crate::types::BasicType;

const LEN: i32 = ARRAY_LENGTH_OFFSET as i32;
const BASE: i32 = ARRAY_BASE_OFFSET as i32;

impl Emitter {
    pub(super) fn emit_reg_mem(&mut self, pc: usize, depth: u16, op: u8) -> bool {
        match op {
            0x2e..=0x35 => self.array_load_reg(pc, depth, op),
            0x4f..=0x52 | 0x54..=0x56 => self.array_store_reg(pc, depth, op),
            0xbe => {
                let spill = self.lazy_flush();
                let arr = self.pop();
                let ra = self.read(arr, R::Rax);
                let slow = self.slow_path_spilling(pc, depth, spill);
                self.a.test_rr(true, ra, ra);
                self.a.jcc(Cond::E, slow);
                let d = self.alloc(&[]);
                self.a.load_sx(4, d, mem(ra, LEN));
                self.push(Loc::Reg(d));
            }
            0xb2..=0xb5 => return self.field_reg(pc, depth, op),
            _ => return false,
        }
        true
    }

    /// Pops index and array into registers and jumps to the slow path on null or out of bounds.
    fn checked_element(&mut self, pc: usize, depth: u16, spill: Vec<(Loc, i32)>) -> (R, R) {
        let idx = self.pop();
        let arr = self.pop();
        let ra = self.read(arr, R::Rax);
        let ri = self.read(idx, R::Rcx);
        let slow = self.slow_path_spilling(pc, depth, spill);
        self.a.test_rr(true, ra, ra);
        self.a.jcc(Cond::E, slow);
        self.a.alu_rm(false, Alu::Cmp, ri, mem(ra, LEN));
        self.a.jcc(Cond::Ae, slow);
        (ra, ri)
    }

    fn array_load_reg(&mut self, pc: usize, depth: u16, op: u8) {
        let spill = self.lazy_flush();
        let (ra, ri) = self.checked_element(pc, depth, spill);
        // The destination may reuse the array or index register: the load reads both first.
        let d = self.alloc(&[]);
        let a = &mut self.a;
        match op {
            0x2e => a.load_sx(4, d, mem_idx(ra, ri, 4, BASE)),
            0x30 => a.load(false, d, mem_idx(ra, ri, 4, BASE)),
            0x2f | 0x31 => a.load(true, d, mem_idx(ra, ri, 8, BASE)),
            0x32 => load_typed_to(a, d, BasicType::Reference, mem_idx(ra, ri, crate::heap::REF_SIZE as u8, BASE)),
            0x33 => a.load_sx(1, d, mem_idx(ra, ri, 1, BASE)),
            0x34 => a.load_zx(2, d, mem_idx(ra, ri, 2, BASE)),
            _ => a.load_sx(2, d, mem_idx(ra, ri, 2, BASE)),
        }
        self.push_w(op == 0x2f || op == 0x31, Loc::Reg(d));
    }

    fn array_store_reg(&mut self, pc: usize, depth: u16, op: u8) {
        let spill = self.lazy_flush();
        let wide = op == 0x50 || op == 0x52;
        let v = self.pop_w(wide);
        let mut rv = self.read(v, R::Rdx);
        let (ra, ri) = self.checked_element(pc, depth, spill);
        match op {
            0x4f | 0x51 => self.a.store(false, mem_idx(ra, ri, 4, BASE), rv),
            0x50 | 0x52 => self.a.store(true, mem_idx(ra, ri, 8, BASE), rv),
            0x54 => {
                // boolean[] stores value & 1; byte[] stores the low byte.
                if rv != R::Rdx {
                    self.a.mov_rr(true, R::Rdx, rv);
                    rv = R::Rdx;
                }
                let bools = crate::vm::vm().primitive_array_class(BasicType::Boolean).ok().map_or(0, |k| k as *const Klass as i64);
                let plain = self.a.new_label();
                self.a.mov_ri(R::R11, bools);
                self.a.alu_rm(false, Alu::Cmp, R::R11, mem(ra, 0));
                self.a.jcc(Cond::Ne, plain);
                self.a.alu_ri(false, Alu::And, rv, 1);
                self.a.bind(plain);
                self.a.store8(mem_idx(ra, ri, 1, BASE), rv);
            }
            _ => self.a.store16(mem_idx(ra, ri, 2, BASE), rv),
        }
    }

    /// Branches to the slow path (re-executing `pc`, which throws) if `r` is null, unless the
    /// value is known not to be.
    fn null_check(&mut self, pc: usize, depth: u16, spill: Vec<(Loc, i32)>, nonnull: bool, r: R) {
        if nonnull {
            return;
        }
        let slow = self.slow_path_spilling(pc, depth, spill);
        self.a.test_rr(true, r, r);
        self.a.jcc(Cond::E, slow);
    }

    fn field_reg(&mut self, pc: usize, depth: u16, op: u8) -> bool {
        let index = self.cp_index(pc);
        let resolved = self.m.holder.slot(index as usize).get();
        // Constant fields fold (fold.rs).
        match (op, resolved, self.vs.last().map(|s| s.loc)) {
            (0xb4, Some(Resolved::InstanceField { ty, .. }), Some(Loc::Imm(o))) if o != 0 => {
                if let Some(v) = super::fold::instance_field(self.m, index, o as usize) {
                    self.pop();
                    self.push_w(ty.slots() == 2, Loc::Imm(v));
                    return true;
                }
            }
            (0xb2, Some(Resolved::StaticField { ty, .. }), _) => {
                if let Some(v) = super::fold::static_field(self.m, index) {
                    self.push_w(ty.slots() == 2, Loc::Imm(v));
                    return true;
                }
            }
            _ => {}
        }
        match (op, resolved) {
            (0xb4, Some(Resolved::InstanceField { offset, ty, .. })) => {
                let spill = self.lazy_flush();
                let o = self.pop();
                let ro = self.read(o, R::Rax);
                self.null_check(pc, depth, spill, o.nonnull, ro);
                let d = self.alloc(&[]);
                load_typed_to(&mut self.a, d, ty, mem(ro, offset as i32));
                self.push_w(ty.slots() == 2, Loc::Reg(d));
            }
            (0xb5, Some(Resolved::InstanceField { offset, ty, volatile })) => {
                let spill = self.lazy_flush();
                let v = self.pop_w(ty.slots() == 2);
                let o = self.pop();
                let ro = self.read(o, R::Rax);
                let mut rv = self.read(v, R::Rdx);
                self.null_check(pc, depth, spill, o.nonnull, ro);
                if ty == BasicType::Boolean && rv != R::Rdx {
                    self.a.mov_rr(true, R::Rdx, rv);
                    rv = R::Rdx;
                }
                if ty == BasicType::Reference {
                    rv = encode_ref(&mut self.a, rv, R::Rdx);
                }
                store_typed_from(&mut self.a, rv, ty, mem(ro, offset as i32));
                if volatile {
                    self.a.storeload();
                }
            }
            (0xb2, Some(Resolved::StaticField { holder, slot, ty, .. })) if holder.init.state() == InitState::Initialized => {
                let d = self.alloc(&[]);
                self.a.mov_ri(R::R11, slot as *const _ as i64);
                load_static_to(&mut self.a, d, ty, mem(R::R11, 0));
                self.push_w(ty.slots() == 2, Loc::Reg(d));
            }
            (0xb3, Some(Resolved::StaticField { holder, slot, ty, volatile })) if holder.init.state() == InitState::Initialized => {
                let v = self.pop_w(ty.slots() == 2);
                let rv = self.read(v, R::Rdx);
                if rv != R::Rdx {
                    self.a.mov_rr(true, R::Rdx, rv);
                }
                normalize_into(&mut self.a, ty);
                self.a.mov_ri(R::R11, slot as *const _ as i64);
                self.a.store(true, mem(R::R11, 0), R::Rdx);
                if volatile {
                    self.a.storeload();
                }
            }
            _ => return false,
        }
        true
    }
}
