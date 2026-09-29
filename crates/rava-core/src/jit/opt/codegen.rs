//! Machine code from an allocated graph: the frame protocol of the baseline compiler (locals,
//! header and outgoing arguments in the value-stack frame at `r13`, `r12` = thread), with SSA
//! values in registers and spill slots on the native stack. Guards and exceptions leave through
//! deoptimization stubs that write the interpreter state into the frame (`deopt.rs`).

use super::ir::*;
use super::moves::{self, spill};
use super::regalloc::{phi_input, phis, Allocation, Loc, CALLER_SAVED};
use crate::jit::asm::{mem, Alu, Asm, Cond, Label, M, R};
use crate::jit::compile::{restore_registers, save_registers};
use crate::jit::runtime;
use crate::method::Method;
use crate::thread::{header, offsets};
use std::collections::HashMap;
use std::rc::Rc;

/// An argument of a runtime helper call.
#[derive(Clone, Copy)]
pub enum Arg {
    Thread,
    Imm(i64),
    Val(V),
    /// A scratch register already holding the argument (only rcx or rdx).
    Reg(R),
}

pub(super) struct Gen<'a> {
    pub a: Asm,
    pub g: &'a Graph,
    pub loc: &'a [Loc],
    pub max_locals: i32,
    pub stack_base: i32,
    labels: Vec<Label>,
    pub epilogue: Label,
    deopts: HashMap<*const State, Label>,
    deopt_list: Vec<(Label, Rc<State>)>,
    polls: Vec<(Label, Label)>,
    edge_stubs: Vec<(Label, B, B)>,
    /// Bytecode index of the block being emitted.
    pub bci: i32,
}

/// Emits `g` for `m`; returns the code and the value-stack slots of its frame.
pub(super) fn generate(g: &Graph, al: &Allocation, m: &'static Method) -> (Vec<u8>, usize) {
    let info = m.code.expect("compiled method has code");
    let max_locals = info.max_locals as i32;
    let stack_base = max_locals + header::SLOTS as i32;
    let frame_slots = stack_base + info.max_stack as i32 + 2;
    let mut a = Asm::default();
    let labels = (0..g.blocks.len()).map(|_| a.new_label()).collect();
    let epilogue = a.new_label();
    let mut gen = Gen {
        a,
        g,
        loc: &al.loc,
        max_locals,
        stack_base,
        labels,
        epilogue,
        deopts: HashMap::new(),
        deopt_list: Vec::new(),
        polls: Vec::new(),
        edge_stubs: Vec::new(),
        bci: -1,
    };
    let spill_bytes = 8 * (al.spill_slots + (al.spill_slots & 1));
    let overflow = gen.a.new_label();
    gen.prologue(spill_bytes, frame_slots, overflow);
    for (i, &b) in al.order.iter().enumerate() {
        gen.a.bind(gen.labels[b as usize]);
        gen.bci = g.blocks[b as usize].bci;
        for &v in &g.blocks[b as usize].insts {
            gen.inst(v);
        }
        gen.term(b, al.order.get(i + 1).copied());
    }
    gen.emit_stubs();
    gen.emit_epilogue(spill_bytes, overflow);
    (gen.a.finish(), frame_slots as usize)
}

impl Gen<'_> {
    pub fn hdr(&self, field: usize) -> M {
        mem(R::R13, 8 * (self.max_locals + field as i32))
    }

    /// Frame slot `n` (from the frame base).
    pub fn slot(&self, n: i32) -> M {
        mem(R::R13, 8 * n)
    }

    fn prologue(&mut self, spill_bytes: i32, frame_slots: i32, overflow: Label) {
        let a = &mut self.a;
        save_registers(a);
        if spill_bytes > 0 {
            a.alu_ri(true, Alu::Sub, R::Rsp, spill_bytes);
        }
        a.mov_rr(true, R::R12, R::Rdi);
        a.mov_rr(true, R::R13, R::Rsi);
        a.lea(R::Rax, mem(R::R13, 8 * frame_slots));
        a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::LIMIT));
        a.jcc(Cond::A, overflow);
        let h = |f: usize| mem(R::R13, 8 * (self.max_locals + f as i32));
        a.store(true, h(header::METHOD), R::Rdx);
        a.store_imm(h(header::BCI), 0);
        a.load(true, R::Rcx, mem(R::R12, offsets::TOP));
        a.store(true, h(header::CALLER), R::Rcx);
        a.load(true, R::Rcx, mem(R::R12, offsets::SP));
        a.store(true, h(header::SAVED_SP), R::Rcx);
        a.lea(R::Rcx, h(header::METHOD));
        a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        a.store(true, mem(R::R12, offsets::SP), R::Rax);
        self.poll();
    }

    fn emit_epilogue(&mut self, spill_bytes: i32, overflow: Label) {
        let restore = self.a.new_label();
        self.a.bind(self.epilogue);
        self.a.load(true, R::Rcx, self.hdr(header::CALLER));
        self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        self.a.load(true, R::Rcx, self.hdr(header::SAVED_SP));
        self.a.store(true, mem(R::R12, offsets::SP), R::Rcx);
        self.a.bind(restore);
        if spill_bytes > 0 {
            self.a.alu_ri(true, Alu::Add, R::Rsp, spill_bytes);
        }
        restore_registers(&mut self.a);
        self.a.ret();
        // The frame was never linked: raise, then return without unlinking.
        self.a.bind(overflow);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::stack_overflow as *const () as usize);
        self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(restore);
    }

    // ---- operands ----

    /// `dst := v`.
    pub fn load(&mut self, dst: R, v: V) {
        moves::mov(&mut self.a, Loc::Reg(dst), self.loc[v as usize]);
    }

    /// The register holding `v`, or `scratch` loaded with it.
    pub fn reg(&mut self, v: V, scratch: R) -> R {
        match self.loc[v as usize] {
            Loc::Reg(r) => r,
            _ => {
                self.load(scratch, v);
                scratch
            }
        }
    }

    /// The register to compute `v` into: its own, or rax when it is spilled.
    pub fn out(&self, v: V) -> R {
        match self.loc[v as usize] {
            Loc::Reg(r) => r,
            _ => R::Rax,
        }
    }

    /// Moves a computed result from `src` to the location of `v`.
    pub fn put(&mut self, v: V, src: R) {
        let dst = self.loc[v as usize];
        if dst != Loc::None {
            moves::mov(&mut self.a, dst, Loc::Reg(src));
        }
    }

    /// Stores `v` (slot encoding) into the memory at `m`.
    pub fn store_val(&mut self, m: M, v: V) {
        match self.loc[v as usize] {
            Loc::Reg(r) => self.a.store(true, m, r),
            Loc::Const(c) if i32::try_from(c).is_ok() => self.a.store_imm(m, c as i32),
            Loc::None => {}
            _ => {
                self.load(R::Rax, v);
                self.a.store(true, m, R::Rax);
            }
        }
    }

    /// `dst op= v`.
    pub fn alu_val(&mut self, w: bool, op: Alu, dst: R, v: V) {
        match self.loc[v as usize] {
            Loc::Reg(r) => self.a.alu_rr(w, op, dst, r),
            Loc::Stack(s) => self.a.alu_rm(w, op, dst, spill(s)),
            Loc::Const(c) if i32::try_from(c).is_ok() => self.a.alu_ri(w, op, dst, c as i32),
            _ => {
                self.load(R::R11, v);
                self.a.alu_rr(w, op, dst, R::R11);
            }
        }
    }

    // ---- calls into the runtime ----

    /// Calls a Rust helper with the allocatable caller-saved registers preserved (pushed where
    /// the collector's stack scan sees them). The result is in rax.
    pub fn call_preserving(&mut self, addr: usize, args: &[Arg]) {
        const SCRATCH: [R; 3] = [R::R11, R::Rcx, R::Rdx];
        const PARAMS: [R; 3] = [R::Rdi, R::Rsi, R::Rdx];
        for (i, arg) in args.iter().enumerate() {
            match *arg {
                Arg::Val(v) => self.load(SCRATCH[i], v),
                Arg::Reg(r) if r != SCRATCH[i] => self.a.mov_rr(true, SCRATCH[i], r),
                _ => {}
            }
        }
        for r in CALLER_SAVED {
            self.a.push(r);
        }
        self.a.push(R::Rax);
        for (i, arg) in args.iter().enumerate() {
            match *arg {
                Arg::Thread => self.a.mov_rr(true, PARAMS[i], R::R12),
                Arg::Imm(c) => self.a.mov_ri(PARAMS[i], c),
                _ if PARAMS[i] != SCRATCH[i] => self.a.mov_rr(true, PARAMS[i], SCRATCH[i]),
                _ => {}
            }
        }
        self.a.call_abs(addr);
        self.a.pop(R::R11);
        for r in CALLER_SAVED.iter().rev() {
            self.a.pop(*r);
        }
    }

    /// Records bytecode index `bci` in the frame header (before calls that may walk the stack).
    pub fn set_bci(&mut self, bci: u32) {
        let at = self.hdr(header::BCI);
        self.a.store_imm(at, (bci as i32) << 1);
    }

    /// Polls for a stop-the-world request.
    fn poll(&mut self) {
        let (stub, back) = (self.a.new_label(), self.a.new_label());
        self.a.mov_ri(R::Rax, &crate::gc::safepoint::REQUESTED as *const _ as i64);
        self.a.cmp8_mi(mem(R::Rax, 0), 0);
        self.a.jcc(Cond::Ne, stub);
        self.a.bind(back);
        self.polls.push((stub, back));
    }

    /// The deoptimization stub of `s` (shared by every guard with that state).
    pub fn deopt(&mut self, s: &Rc<State>) -> Label {
        if let Some(&l) = self.deopts.get(&Rc::as_ptr(s)) {
            return l;
        }
        let l = self.a.new_label();
        self.deopts.insert(Rc::as_ptr(s), l);
        self.deopt_list.push((l, s.clone()));
        l
    }

    // ---- control flow ----

    fn term(&mut self, b: B, next: Option<B>) {
        match self.g.blocks[b as usize].term.clone() {
            Term::Goto(t) => self.edge(b, t, next == Some(t)),
            Term::If { cmp, ty, a, b: y, then, other } => {
                if then == other {
                    return self.edge(b, then, next == Some(then));
                }
                let x = self.reg(a, R::Rax);
                self.alu_val(ty != Ty::I, Alu::Cmp, x, y);
                let c = cond(cmp);
                let (c, taken, fall) = if next == Some(then) { (invert(c), other, then) } else { (c, then, other) };
                let l = self.edge_target(b, taken);
                self.a.jcc(c, l);
                self.edge(b, fall, next == Some(fall));
            }
            Term::Switch { key, cases, default } => {
                self.load(R::Rax, key);
                let mut targets = HashMap::new();
                for (k, t) in cases {
                    let l = *targets.entry(t).or_insert_with(|| self.edge_target(b, t));
                    self.a.alu_ri(false, Alu::Cmp, R::Rax, k);
                    self.a.jcc(Cond::E, l);
                }
                self.edge(b, default, next == Some(default));
            }
            Term::Return(v) => {
                if let Some(v) = v {
                    self.load(R::Rax, v);
                }
                self.a.jmp(self.epilogue);
            }
            Term::Deopt(s) => {
                let l = self.deopt(&s);
                self.a.jmp(l);
            }
            Term::None => unreachable!("an unbuilt block in the code order"),
        }
    }

    fn back_edge(&self, from: B, to: B) -> bool {
        self.g.blocks[to as usize].bci <= self.g.blocks[from as usize].bci
    }

    /// Where a conditional jump from `from` to `to` goes: the block itself, or a stub doing the
    /// edge's phi moves and poll first.
    fn edge_target(&mut self, from: B, to: B) -> Label {
        if phis(self.g, to).next().is_none() && !self.back_edge(from, to) {
            return self.labels[to as usize];
        }
        let l = self.a.new_label();
        self.edge_stubs.push((l, from, to));
        l
    }

    /// Leaves `from` for `to`: phi moves, a poll on back-edges, then the jump.
    fn edge(&mut self, from: B, to: B, fallthrough: bool) {
        if self.back_edge(from, to) {
            self.poll();
        }
        let moves: Vec<(Loc, Loc)> =
            phis(self.g, to).map(|p| (self.loc[p as usize], self.loc[phi_input(self.g, p, from, to) as usize])).collect();
        moves::parallel(&mut self.a, &moves);
        if !fallthrough {
            self.a.jmp(self.labels[to as usize]);
        }
    }

    fn emit_stubs(&mut self) {
        while let Some((l, from, to)) = self.edge_stubs.pop() {
            self.a.bind(l);
            self.edge(from, to, false);
        }
        for (stub, back) in std::mem::take(&mut self.polls) {
            self.a.bind(stub);
            self.call_preserving(runtime::safepoint as *const () as usize, &[Arg::Thread]);
            self.a.jmp(back);
        }
        for (l, s) in std::mem::take(&mut self.deopt_list) {
            self.a.bind(l);
            self.deopt_stub(&s);
        }
    }

    /// Writes the state into the frame, then finishes the method in the interpreter.
    fn deopt_stub(&mut self, s: &State) {
        assert!(s.caller.is_none(), "inlined scopes are not compiled yet");
        for (i, v) in s.locals.iter().enumerate() {
            if let Some(v) = *v {
                self.store_val(self.slot(s.frame + i as i32), v);
            }
        }
        for (i, v) in s.stack.iter().enumerate() {
            if let Some(v) = *v {
                self.store_val(self.slot(self.stack_base + i as i32), v);
            }
        }
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_ri(R::Rsi, s.method as *const Method as i64);
        self.a.mov_rr(true, R::Rdx, R::R13);
        self.a.mov_ri(R::Rcx, s.bci as i64 | (s.stack.len() as i64) << 32);
        self.a.call_abs(super::deopt::deopt as *const () as usize);
        self.a.jmp(self.epilogue);
    }
}

fn cond(c: Cmp) -> Cond {
    match c {
        Cmp::Eq => Cond::E,
        Cmp::Ne => Cond::Ne,
        Cmp::Lt => Cond::L,
        Cmp::Ge => Cond::Ge,
        Cmp::Gt => Cond::G,
        Cmp::Le => Cond::Le,
    }
}

pub fn invert(c: Cond) -> Cond {
    // x86 condition codes come in complementary pairs differing in the low bit.
    // SAFETY: `Cond` is `repr(u8)` with all 16 codes.
    unsafe { std::mem::transmute::<u8, Cond>(c as u8 ^ 1) }
}
