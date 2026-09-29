//! Baseline compiler driver: frame setup, per-instruction emission (`emit.rs`), exception
//! dispatch and slow paths.
//!
//! Registers: `r12` = thread, `r13` = frame base (locals). Everything else is scratch between
//! instructions: the operand stack lives in memory at depth-indexed offsets, so every
//! instruction boundary is a consistent interpreter state.

use super::analysis::{analyze, insn_len, Analysis};
use super::asm::{mem, Asm, Cond, Label, M, R};
use super::{code_cache, runtime, CompiledMethod};
use crate::classfile::{ClassFile, CodeInfo};
use crate::method::Method;
use crate::thread::{header, offsets};

pub(super) struct Emitter {
    pub a: Asm,
    pub m: &'static Method,
    pub cf: &'static ClassFile,
    pub code: &'static [u8],
    pub max_locals: i32,
    pub an: Analysis,
    /// One label per bytecode index.
    pub labels: Vec<Label>,
    pub exc: Label,
    pub epilogue: Label,
    /// Deferred slow paths: (entry label, bci, depth).
    pub slow: Vec<(Label, usize, u16)>,
}

pub fn compile(m: &'static Method) -> Option<CompiledMethod> {
    if m.is_synchronized() || m.is_native() {
        return None;
    }
    let info: &'static CodeInfo = m.code?;
    let an = analyze(m)?;
    let mut a = Asm::default();
    let labels = (0..=info.code.len()).map(|_| a.new_label()).collect();
    let (exc, epilogue) = (a.new_label(), a.new_label());
    let mut e = Emitter {
        a,
        m,
        cf: m.holder.cf.as_ref()?,
        code: info.code,
        max_locals: info.max_locals as i32,
        an,
        labels,
        exc,
        epilogue,
        slow: Vec::new(),
    };
    let frame_slots = info.max_locals as i32 + header::SLOTS as i32 + info.max_stack as i32 + 2;
    let overflow = e.a.new_label();
    e.prologue(frame_slots, overflow);
    let mut pc = 0;
    let mut leaders = Vec::new();
    while pc < e.code.len() {
        e.a.bind(e.labels[pc]);
        let len = insn_len(e.code, pc);
        if let Some(depth) = e.an.depth[pc] {
            if e.an.leader[pc] {
                leaders.push((pc as u32, e.a.pos()));
            }
            e.emit(pc, depth)?;
        }
        pc += len;
    }
    e.a.bind(e.labels[e.code.len()]);
    e.emit_slow_paths();
    e.emit_exception_dispatch();
    e.emit_epilogue(overflow);
    let code = e.a.finish();
    let entry = code_cache::install(&code);
    let blocks = leaders.into_iter().map(|(bci, off)| (bci, entry + off as usize)).collect();
    super::code_cache::perf_map(entry, code.len(), &m.to_string());
    if std::env::var_os("RAVA_CORE_JIT_LOG").is_some() {
        eprintln!("[jit] {m} {} bytes of bytecode -> {} bytes", info.code.len(), code.len());
    }
    Some(CompiledMethod { entry, unresolved_deferrals: Default::default(), size: code.len(), blocks })
}

impl Emitter {
    pub fn local(&self, n: usize) -> M {
        mem(R::R13, 8 * n as i32)
    }

    /// Operand-stack slot `i` (0 = bottom).
    pub fn stk(&self, i: i32) -> M {
        mem(R::R13, 8 * (self.max_locals + header::SLOTS as i32 + i))
    }

    pub fn hdr(&self, field: usize) -> M {
        mem(R::R13, 8 * (self.max_locals + field as i32))
    }

    /// Records `bci` as the frame's current instruction (before calls and slow paths).
    pub fn set_bci(&mut self, bci: usize) {
        let at = self.hdr(header::BCI);
        self.a.store_imm(at, (bci as i32) << 1);
    }

    pub fn label(&self, bci: usize) -> Label {
        self.labels[bci]
    }

    /// A slow path that re-executes this instruction in the interpreter, then continues with
    /// the next instruction.
    pub fn slow_path(&mut self, bci: usize, depth: u16) -> Label {
        let l = self.a.new_label();
        self.slow.push((l, bci, depth));
        l
    }

    /// Emits the interpreter deferral for `bci` inline (the instruction is always slow here).
    pub fn defer(&mut self, bci: usize, depth: u16) {
        self.defer_via(bci, depth, runtime::interp_one as *const () as usize);
    }

    /// Defers an instruction whose constant was unresolved at compile time; frequent use of such
    /// sites triggers recompilation once they have been resolved.
    pub fn defer_unresolved(&mut self, bci: usize, depth: u16) {
        self.defer_via(bci, depth, runtime::interp_one_unresolved as *const () as usize);
    }

    fn defer_via(&mut self, bci: usize, depth: u16, helper: usize) {
        self.set_bci(bci);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_ri(R::Rsi, self.m as *const Method as i64);
        self.a.mov_rr(true, R::Rdx, R::R13);
        self.a.mov_ri(R::Rcx, (bci as i64) | (depth as i64) << 32);
        self.a.call_abs(helper);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::Ne, self.exc);
    }

    /// Jumps to exception dispatch if a call left an exception pending.
    pub fn check_pending(&mut self) {
        self.a.alu_mi(true, super::asm::Alu::Cmp, mem(R::R12, offsets::PENDING), 0);
        self.a.jcc(Cond::Ne, self.exc);
    }

    fn prologue(&mut self, frame_slots: i32, overflow: Label) {
        let a = &mut self.a;
        a.push(R::Rbp);
        a.mov_rr(true, R::Rbp, R::Rsp);
        for r in [R::R12, R::R13, R::R14, R::R15] {
            a.push(r);
        }
        a.mov_rr(true, R::R12, R::Rdi);
        a.mov_rr(true, R::R13, R::Rsi);
        a.lea(R::Rax, mem(R::R13, 8 * frame_slots));
        a.alu_rm(true, super::asm::Alu::Cmp, R::Rax, mem(R::R12, offsets::LIMIT));
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
    }

    fn emit_slow_paths(&mut self) {
        for (l, bci, depth) in std::mem::take(&mut self.slow) {
            self.a.bind(l);
            self.defer(bci, depth);
            let next = bci + insn_len(self.code, bci);
            self.a.jmp(self.labels[next]);
        }
    }

    fn emit_exception_dispatch(&mut self) {
        let unwind = self.a.new_label();
        self.a.bind(self.exc);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_rr(true, R::Rsi, R::R13);
        self.a.call_abs(runtime::handle_exception as *const () as usize);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, unwind);
        self.a.jmp_r(R::Rax);
        self.a.bind(unwind);
        self.a.alu_rr(false, super::asm::Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(self.epilogue);
    }

    fn emit_epilogue(&mut self, overflow: Label) {
        let restore = self.a.new_label();
        self.a.bind(self.epilogue);
        let h = |f: usize| mem(R::R13, 8 * (self.max_locals + f as i32));
        self.a.load(true, R::Rcx, h(header::CALLER));
        self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        self.a.load(true, R::Rcx, h(header::SAVED_SP));
        self.a.store(true, mem(R::R12, offsets::SP), R::Rcx);
        self.a.bind(restore);
        for r in [R::R15, R::R14, R::R13, R::R12] {
            self.a.pop(r);
        }
        self.a.pop(R::Rbp);
        self.a.ret();
        // The frame was never linked: raise, then return without unlinking.
        self.a.bind(overflow);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::stack_overflow as *const () as usize);
        self.a.alu_rr(false, super::asm::Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(restore);
    }
}
