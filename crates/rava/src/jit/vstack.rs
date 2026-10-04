//! The virtual operand stack and the local-register cache (a Liftoff-style register cache over
//! the baseline's memory frame).
//!
//! **Locals.** The hottest locals are pinned to callee-saved registers for the whole method. Every
//! store writes through to the local's memory slot, so memory is always current. Calls and runtime
//! helpers preserve these registers. Code entered from outside (OSR, exception handlers) comes in
//! through stubs that reload the registers.
//!
//! **Operand stack.** Each slot is `Mem` (its frame slot holds the value), an immediate, a
//! scratch register, or an alias of a cached local. `synced` records whether the frame slot
//! also holds the value. The stack is written back before any instruction that may leave
//! compiled code (a slow path, a call, a generic template), so the interpreter always finds a
//! consistent frame. At block boundaries the stack is entirely `Mem`.

use super::asm::{Alu, M, R, X};
use super::compile::Emitter;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Loc {
    Mem,
    Reg(R),
    /// A double value cached in an SSE register (baseline floating-point stack caching).
    Xmm(X),
    Imm(i64),
    /// The value of cached local `n` (read-only alias of its register).
    Local(u16),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Slot {
    pub loc: Loc,
    pub synced: bool,
}

const MEM: Slot = Slot { loc: Loc::Mem, synced: true };

/// A popped operand: its slot and the operand-stack index it occupied.
#[derive(Clone, Copy, Debug)]
pub(super) struct Opnd {
    pub loc: Loc,
    pub idx: i32,
}

/// An operand usable directly by an instruction.
pub(super) enum Src {
    R(R),
    I(i32),
    M(M),
}

/// Scratch registers for operand-stack values (caller-saved; rax, rcx, rdx and r11 stay free as
/// template temporaries).
const POOL: [R; 5] = [R::Rsi, R::Rdi, R::R8, R::R9, R::R10];
/// Callee-saved registers pinned to hot locals.
pub(super) const LOCAL_REGS: [R; 4] = [R::Rbx, R::Rbp, R::R14, R::R15];
/// SSE registers caching double operand-stack values (xmm0/xmm1 stay free as template temporaries,
/// mirroring the GPR temporaries). All are caller-saved, so values in them are written back and
/// reloaded around calls and slow paths exactly as the GPR scratch values are.
const XPOOL: [X; 6] = [X(2), X(3), X(4), X(5), X(6), X(7)];

impl Emitter {
    pub(super) fn vs_reset(&mut self, depth: u16) {
        self.vs.clear();
        self.vs.resize(depth as usize, MEM);
    }

    pub(super) fn lreg(&self, n: usize) -> Option<R> {
        self.lregs.get(n).copied().flatten()
    }

    pub(super) fn push(&mut self, loc: Loc) {
        self.vs.push(Slot { loc, synced: loc == Loc::Mem });
    }

    /// Pushes a long/double: the value slot and its unused second slot.
    pub(super) fn push_w(&mut self, wide: bool, loc: Loc) {
        self.push(loc);
        if wide {
            self.vs.push(MEM);
        }
    }

    pub(super) fn pop(&mut self) -> Opnd {
        let s = self.vs.pop().expect("operand stack underflow");
        Opnd { loc: s.loc, idx: self.vs.len() as i32 }
    }

    pub(super) fn pop_w(&mut self, wide: bool) -> Opnd {
        if wide {
            self.vs.pop();
        }
        self.pop()
    }

    fn in_use(&self, r: R) -> bool {
        self.vs.iter().any(|s| s.loc == Loc::Reg(r))
    }

    fn in_use_xmm(&self, x: X) -> bool {
        self.vs.iter().any(|s| s.loc == Loc::Xmm(x))
    }

    /// A free scratch register not in `excl`, spilling the deepest register slot if needed.
    pub(super) fn alloc(&mut self, excl: &[R]) -> R {
        if let Some(r) = POOL.iter().copied().find(|r| !excl.contains(r) && !self.in_use(*r)) {
            return r;
        }
        let i = self.vs.iter().position(|s| matches!(s.loc, Loc::Reg(r) if !excl.contains(&r))).expect("a spillable register");
        let Loc::Reg(r) = self.vs[i].loc else { unreachable!() };
        self.write_back(i);
        self.vs[i].loc = Loc::Mem;
        r
    }

    /// A free SSE scratch register not in `excl`, spilling the deepest cached double if needed.
    pub(super) fn alloc_xmm(&mut self, excl: &[X]) -> X {
        if let Some(x) = XPOOL.iter().copied().find(|x| !excl.contains(x) && !self.in_use_xmm(*x)) {
            return x;
        }
        let i = self.vs.iter().position(|s| matches!(s.loc, Loc::Xmm(x) if !excl.contains(&x))).expect("a spillable xmm");
        let Loc::Xmm(x) = self.vs[i].loc else { unreachable!() };
        self.write_back(i);
        self.vs[i].loc = Loc::Mem;
        x
    }

    /// An SSE register holding double operand `o`, free for the caller to clobber. A value already
    /// in an SSE register is returned as-is (it was just popped, so nothing else aliases it).
    pub(super) fn xmm_owned(&mut self, o: Opnd, excl: &[X]) -> X {
        if let Loc::Xmm(x) = o.loc {
            return x;
        }
        let x = self.alloc_xmm(excl);
        match o.loc {
            Loc::Mem => self.a.fload(true, x, self.stk(o.idx)),
            Loc::Reg(r) => self.a.mov_xr(true, x, r),
            Loc::Local(n) => self.a.mov_xr(true, x, self.lreg(n as usize).expect("cached local")),
            Loc::Imm(v) => {
                self.a.mov_ri(R::Rax, v);
                self.a.mov_xr(true, x, R::Rax);
            }
            Loc::Xmm(_) => unreachable!(),
        }
        x
    }

    /// Reloads register-cached stack values from their frame slots (after a call or slow path
    /// clobbered the caller-saved registers).
    pub(super) fn reload_live(&mut self, live: &[(Loc, i32)]) {
        for &(loc, slot) in live {
            match loc {
                Loc::Reg(r) => self.a.load(true, r, self.stk(slot)),
                Loc::Xmm(x) => self.a.fload(true, x, self.stk(slot)),
                _ => {}
            }
        }
    }

    /// Stores a 64-bit immediate without a scratch register.
    pub(super) fn store_imm64(&mut self, m: M, v: i64) {
        if v as i32 as i64 == v {
            self.a.store_imm(m, v as i32);
        } else {
            self.a.store_imm32(m, v as i32);
            self.a.store_imm32(M { disp: m.disp + 4, ..m }, (v >> 32) as i32);
        }
    }

    fn write_back(&mut self, i: usize) {
        let s = self.vs[i];
        if s.synced {
            return;
        }
        let at = self.stk(i as i32);
        match s.loc {
            Loc::Reg(r) => self.a.store(true, at, r),
            Loc::Xmm(x) => self.a.fstore(true, at, x),
            Loc::Local(n) => self.a.store(true, at, self.lreg(n as usize).expect("cached local")),
            Loc::Imm(v) => self.store_imm64(at, v),
            Loc::Mem => {}
        }
        self.vs[i].synced = true;
    }

    /// Before an instruction that may take a slow path: the slots not yet written back, for
    /// that slow path to store (`write_back_all`), so the fast path keeps them in registers.
    /// Inlined leaves restart their call instead and still write back here.
    pub(super) fn lazy_flush(&mut self) -> Vec<(Loc, i32)> {
        if self.in_leaf() {
            self.flush();
            return Vec::new();
        }
        self.vs.iter().enumerate().filter(|(_, s)| !s.synced).map(|(i, s)| (s.loc, i as i32)).collect()
    }

    /// Stores slots captured by `lazy_flush` (their registers still hold them).
    pub(super) fn write_back_all(&mut self, spill: &[(Loc, i32)]) {
        for &(loc, i) in spill {
            let at = self.stk(i);
            match loc {
                Loc::Reg(r) => self.a.store(true, at, r),
                Loc::Xmm(x) => self.a.fstore(true, at, x),
                Loc::Local(n) => self.a.store(true, at, self.lreg(n as usize).expect("cached local")),
                Loc::Imm(v) => self.store_imm64(at, v),
                Loc::Mem => {}
            }
        }
    }

    /// Writes every slot back to the frame, keeping the register copies (no flags change).
    pub(super) fn flush(&mut self) {
        for i in 0..self.vs.len() {
            self.write_back(i);
        }
    }

    /// Writes everything back and forgets the registers (before code that clobbers them).
    pub(super) fn flush_to_mem(&mut self) {
        self.flush();
        self.vs.iter_mut().for_each(|s| s.loc = Loc::Mem);
    }

    /// Scratch registers (GPR or SSE) holding stack values: what a slow path must reload after
    /// its helper clobbers the caller-saved registers.
    pub(super) fn live_regs(&self) -> Vec<(Loc, i32)> {
        self.vs
            .iter()
            .enumerate()
            .filter_map(|(i, s)| matches!(s.loc, Loc::Reg(_) | Loc::Xmm(_)).then_some((s.loc, i as i32)))
            .collect()
    }

    /// Before local `n` changes: slots aliasing it get their own copy.
    pub(super) fn kill_aliases(&mut self, n: usize, excl: &[R]) {
        for i in 0..self.vs.len() {
            if self.vs[i].loc != Loc::Local(n as u16) {
                continue;
            }
            if self.vs[i].synced {
                self.vs[i].loc = Loc::Mem;
            } else {
                let r = self.alloc(excl);
                self.a.mov_rr(true, r, self.lreg(n).expect("cached local"));
                self.vs[i].loc = Loc::Reg(r);
            }
        }
    }

    /// A register holding `o` (its own register, a local's, or `temp` loaded with it).
    pub(super) fn read(&mut self, o: Opnd, temp: R) -> R {
        match o.loc {
            Loc::Reg(r) => r,
            Loc::Xmm(x) => {
                self.a.mov_rx(true, temp, x);
                temp
            }
            Loc::Local(n) => self.lreg(n as usize).expect("cached local"),
            Loc::Imm(v) => {
                self.a.mov_ri(temp, v);
                temp
            }
            Loc::Mem => {
                self.a.load(true, temp, self.stk(o.idx));
                temp
            }
        }
    }

    /// A scratch register owned by the caller that holds `o` (free to modify).
    pub(super) fn owned(&mut self, o: Opnd, excl: &[R]) -> R {
        if let Loc::Reg(r) = o.loc {
            return r;
        }
        let d = self.alloc(excl);
        match o.loc {
            Loc::Xmm(x) => self.a.mov_rx(true, d, x),
            Loc::Local(n) => self.a.mov_rr(true, d, self.lreg(n as usize).expect("cached local")),
            Loc::Imm(v) => self.a.mov_ri(d, v),
            _ => self.a.load(true, d, self.stk(o.idx)),
        }
        d
    }

    /// `o` as an instruction source operand (immediates up to 32 bits, frame slots in memory).
    pub(super) fn src(&mut self, o: Opnd, temp: R) -> Src {
        match o.loc {
            Loc::Imm(v) if v as i32 as i64 == v => Src::I(v as i32),
            Loc::Mem => Src::M(self.stk(o.idx)),
            _ => Src::R(self.read(o, temp)),
        }
    }

    pub(super) fn alu_src(&mut self, w: bool, op: Alu, d: R, s: Src) {
        match s {
            Src::R(r) => self.a.alu_rr(w, op, d, r),
            Src::I(v) => self.a.alu_ri(w, op, d, v),
            Src::M(m) => self.a.alu_rm(w, op, d, m),
        }
    }

    pub(super) fn reg_of(o: Opnd) -> Option<R> {
        if let Loc::Reg(r) = o.loc {
            Some(r)
        } else {
            None
        }
    }
}

/// Picks up to four hot locals for registers: uses weighted by loop nesting (back-edge ranges).
pub(super) fn plan_local_regs(code: &[u8], max_locals: usize, an: &super::analysis::Analysis) -> Vec<Option<R>> {
    use super::analysis::{i2, insn_len};
    let mut nest = vec![0u32; code.len() + 1];
    let mut uses: Vec<(usize, u32)> = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let op = code[pc];
        let back = match op {
            0x99..=0xa7 | 0xc6 | 0xc7 => Some((pc as i64 + i2(code, pc + 1) as i64) as usize),
            _ => None,
        };
        if let Some(target) = back.filter(|&t| t <= pc) {
            nest[target..=pc].iter_mut().for_each(|d| *d += 1);
        }
        let local = match op {
            0x15..=0x19 | 0x36..=0x3a | 0x84 => Some(code[pc + 1] as usize),
            0x1a..=0x2d => Some(((op - 0x1a) % 4) as usize),
            0x3b..=0x4e => Some(((op - 0x3b) % 4) as usize),
            0xc4 => Some(u16::from_be_bytes([code[pc + 2], code[pc + 3]]) as usize),
            _ => None,
        };
        if let (Some(n), Some(_)) = (local, an.depth[pc]) {
            uses.push((n, pc as u32));
        }
        pc += insn_len(code, pc);
    }
    let mut weight = vec![0u64; max_locals];
    for (n, pc) in uses {
        if let Some(w) = weight.get_mut(n) {
            *w += 1u64 << (3 * nest[pc as usize].min(4));
        }
    }
    let mut order: Vec<usize> = (0..max_locals).filter(|&n| weight[n] >= 3).collect();
    order.sort_by_key(|&n| std::cmp::Reverse(weight[n]));
    let mut regs = vec![None; max_locals];
    for (n, r) in order.into_iter().zip(LOCAL_REGS) {
        regs[n] = Some(r);
    }
    regs
}
