//! Baseline compiler driver: frame setup, per-instruction emission (`emit.rs`), exception
//! dispatch and slow paths.
//!
//! Registers: `r12` = thread, `r13` = frame base (locals); rbx, rbp, r14 and r15 cache hot
//! locals, and the operand stack is cached in scratch registers (`vstack.rs`). Whenever control
//! may leave compiled code, the frame in memory is a consistent interpreter state.

use super::analysis::{analyze, insn_len, Analysis};
use super::asm::{mem, Asm, Cond, Label, M, R};
use super::inline::{inline_slots, inline_target, InlineCtx};
use super::vstack::{plan_local_regs, Slot};
use super::{code_cache, runtime, CompiledMethod};
use std::collections::HashMap;
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
    /// Deferred slow paths: (entry label, bci, depth, registers to reload from their slots).
    pub slow: Vec<(Label, usize, u16, Vec<(R, i32)>)>,
    /// Safepoint-poll stubs: (entry label, label to resume at).
    pub polls: Vec<(Label, Label)>,
    /// Slots (from the frame base) of local 0 and operand-stack slot 0 of the code being emitted.
    pub local_base: i32,
    pub stack_base: i32,
    /// Scratch slots for inlined callees, above the caller's operand stack.
    pub inline_base: i32,
    /// Call sites compiled in place, decided before emission (`inline.rs`).
    pub inlines: HashMap<usize, &'static Method>,
    /// Set while emitting an inlined body.
    pub inl: Option<InlineCtx>,
    /// The virtual operand stack and the registers pinned to locals (`vstack.rs`).
    pub vs: Vec<Slot>,
    pub lregs: Vec<Option<R>>,
}

pub fn compile(m: &'static Method) -> Option<CompiledMethod> {
    if m.is_synchronized() || m.is_native() {
        return None;
    }
    let info: &'static CodeInfo = m.code?;
    let an = analyze(m)?;
    let (inlines, inline_extra) = plan_inlining(m, info.code, &an);
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
        polls: Vec::new(),
        local_base: 0,
        stack_base: info.max_locals as i32 + header::SLOTS as i32,
        inline_base: info.max_locals as i32 + header::SLOTS as i32 + info.max_stack as i32 + 2,
        inlines,
        inl: None,
        vs: Vec::new(),
        lregs: Vec::new(),
    };
    if std::env::var_os("RAVA_CORE_NO_LOCAL_REGS").is_none() {
        e.lregs = plan_local_regs(e.code, info.max_locals as usize, &e.an);
    }
    let frame_slots = e.inline_base + inline_extra;
    let overflow = e.a.new_label();
    e.prologue(frame_slots, overflow);
    let mut pc = 0;
    let mut leaders = Vec::new();
    while pc < e.code.len() {
        let len = insn_len(e.code, pc);
        let Some(depth) = e.an.depth[pc] else {
            e.a.bind(e.labels[pc]);
            pc += len;
            continue;
        };
        if e.an.leader[pc] {
            // Blocks start with the operand stack in memory.
            e.flush_to_mem();
            e.vs_reset(depth);
            leaders.push(pc as u32);
        }
        e.a.bind(e.labels[pc]);
        let first_slow = e.slow.len();
        e.emit(pc, depth)?;
        let live = e.live_regs();
        e.slow[first_slow..].iter_mut().for_each(|s| s.3 = live.clone());
        pc += len;
    }
    e.a.bind(e.labels[e.code.len()]);
    let leaders = e.emit_entry_stubs(leaders);
    e.emit_slow_paths();
    e.emit_poll_stubs();
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

/// The call sites to inline and the scratch slots they need; decided once, since resolution may
/// progress concurrently while the method is being emitted.
fn plan_inlining(m: &'static Method, code: &[u8], an: &Analysis) -> (HashMap<usize, &'static Method>, i32) {
    let (mut sites, mut extra) = (HashMap::new(), 0);
    if std::env::var_os("RAVA_CORE_NO_INLINE").is_some() {
        return (sites, extra);
    }
    let mut pc = 0;
    while pc < code.len() {
        if an.depth[pc].is_some() && matches!(code[pc], 0xb6..=0xb8) {
            if let Some(t) = inline_target(m, code, pc) {
                extra = extra.max(inline_slots(t));
                sites.insert(pc, t);
            }
        }
        pc += insn_len(code, pc);
    }
    (sites, extra)
}

impl Emitter {
    pub fn local(&self, n: usize) -> M {
        mem(R::R13, 8 * (self.local_base + n as i32))
    }

    /// Operand-stack slot `i` (0 = bottom).
    pub fn stk(&self, i: i32) -> M {
        mem(R::R13, 8 * (self.stack_base + i))
    }

    pub fn hdr(&self, field: usize) -> M {
        mem(R::R13, 8 * (self.max_locals + field as i32))
    }

    /// Records `bci` as the frame's current instruction (before calls and slow paths).
    pub fn set_bci(&mut self, bci: usize) {
        debug_assert!(self.inl.is_none(), "inlined bodies never record a bci");
        let at = self.hdr(header::BCI);
        self.a.store_imm(at, (bci as i32) << 1);
    }

    pub fn label(&self, bci: usize) -> Label {
        self.labels[bci]
    }

    /// A slow path that re-executes this instruction in the interpreter, then continues with
    /// the next instruction.
    pub fn slow_path(&mut self, bci: usize, depth: u16) -> Label {
        if let Some(ctx) = self.inl {
            return ctx.restart;
        }
        let l = self.a.new_label();
        self.slow.push((l, bci, depth, Vec::new()));
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
        if let Some(ctx) = self.inl {
            return self.a.jmp(ctx.restart);
        }
        self.set_bci(bci);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_ri(R::Rsi, self.m as *const Method as i64);
        self.a.mov_rr(true, R::Rdx, R::R13);
        self.a.mov_ri(R::Rcx, (bci as i64) | (depth as i64) << 32);
        self.a.call_abs(helper);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::Ne, self.exc);
    }

    /// Polls for a stop-the-world request (method entry and loop back-edges): frames are fully in
    /// memory at every instruction boundary, so the collector sees everything.
    pub fn poll(&mut self) {
        let (stub, back) = (self.a.new_label(), self.a.new_label());
        self.a.mov_ri(R::Rax, &crate::gc::safepoint::REQUESTED as *const _ as i64);
        self.a.cmp8_mi(mem(R::Rax, 0), 0);
        self.a.jcc(Cond::Ne, stub);
        self.a.bind(back);
        self.polls.push((stub, back));
    }

    fn emit_poll_stubs(&mut self) {
        for (stub, back) in std::mem::take(&mut self.polls) {
            self.a.bind(stub);
            self.a.mov_rr(true, R::Rdi, R::R12);
            self.a.call_abs(runtime::safepoint as *const () as usize);
            self.a.jmp(back);
        }
    }

    /// Jumps to exception dispatch if a call left an exception pending.
    pub fn check_pending(&mut self) {
        self.a.alu_mi(true, super::asm::Alu::Cmp, mem(R::R12, offsets::PENDING), 0);
        self.a.jcc(Cond::Ne, self.exc);
    }

    fn prologue(&mut self, frame_slots: i32, overflow: Label) {
        let a = &mut self.a;
        save_registers(a);
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
        self.poll();
        self.load_local_regs();
    }

    /// Loads the registers pinned to locals from their slots.
    fn load_local_regs(&mut self) {
        for n in 0..self.lregs.len() {
            if let Some(r) = self.lregs[n] {
                self.a.load(true, r, self.local(n));
            }
        }
    }

    /// Entry points for OSR and exception handlers at each block: load the local registers,
    /// then continue at the block. Returns (bci, code offset) pairs.
    fn emit_entry_stubs(&mut self, leaders: Vec<u32>) -> Vec<(u32, u32)> {
        let mut out = Vec::with_capacity(leaders.len());
        for bci in leaders {
            out.push((bci, self.a.pos()));
            self.load_local_regs();
            self.a.jmp(self.labels[bci as usize]);
        }
        out
    }

    /// Slow paths re-execute one instruction in the interpreter, reload the operand-stack
    /// registers the fast path left live, and continue after the instruction.
    fn emit_slow_paths(&mut self) {
        for (l, bci, depth, reload) in std::mem::take(&mut self.slow) {
            self.a.bind(l);
            self.defer(bci, depth);
            for (r, slot) in reload {
                self.a.load(true, r, self.stk(slot));
            }
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
        restore_registers(&mut self.a);
        self.a.ret();
        // The frame was never linked: raise, then return without unlinking.
        self.a.bind(overflow);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::stack_overflow as *const () as usize);
        self.a.alu_rr(false, super::asm::Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(restore);
    }
}

/// Callee-saved registers compiled code uses (thread, frame, pinned locals), saved in this
/// order; the extra 8 bytes keep calls 16-byte aligned.
const SAVED: [R; 6] = [R::Rbp, R::Rbx, R::R12, R::R13, R::R14, R::R15];

pub(super) fn save_registers(a: &mut Asm) {
    for r in SAVED {
        a.push(r);
    }
    a.alu_ri(true, super::asm::Alu::Sub, R::Rsp, 8);
}

pub(super) fn restore_registers(a: &mut Asm) {
    a.alu_ri(true, super::asm::Alu::Add, R::Rsp, 8);
    for r in SAVED.iter().rev() {
        a.pop(*r);
    }
}
