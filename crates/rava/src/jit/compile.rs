//! Baseline compiler driver: frame setup, per-instruction emission (`emit.rs`), exception
//! dispatch and slow paths.
//!
//! Registers: `r12` = thread, `r13` = frame base (locals); rbx, rbp, r14 and r15 cache hot
//! locals, and the operand stack is cached in scratch registers (`vstack.rs`). Whenever control
//! may leave compiled code, the frame in memory is a consistent interpreter state.

use super::analysis::{analyze, insn_len, Analysis};
use super::asm::{mem, Asm, Cond, Label, M, R};
use super::inline::InlineCtx;
use super::vstack::{plan_local_regs, Slot};
use super::{code_cache, runtime, CompiledMethod};
use crate::classfile::{ClassFile, CodeInfo};
use crate::method::Method;
use crate::thread::{header, offsets};

/// A deferred slow path: entry label, bci, depth, the registers to reload from their slots
/// afterwards and the stack slots to write back first.
pub(super) type SlowPath = (Label, usize, u16, Vec<(super::vstack::Loc, i32)>, Vec<(super::vstack::Loc, i32)>);

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
    /// Deferred slow paths: (entry label, bci, depth, registers to reload from their slots,
    /// stack slots to write back first).
    pub slow: Vec<SlowPath>,
    /// Safepoint-poll stubs: (entry label, label to resume at).
    pub polls: Vec<(Label, Label)>,
    /// Slots (from the frame base) of local 0 and operand-stack slot 0 of the code being emitted.
    pub local_base: i32,
    pub stack_base: i32,
    /// Scratch slots for leaf callees, above this body's operand stack.
    pub scratch_base: i32,
    /// The methods whose bodies are being emitted, outermost first (inlining decisions).
    pub chain: Vec<(&'static Method, Option<i64>)>,
    /// The highest frame slot any emitted body uses (the frame size, patched into the prologue).
    pub extent: i32,
    /// An inlined body could not be compiled: the method is refused.
    pub failed: bool,
    pub tier: u8,
    /// Bytecode inlined so far (bounded per compilation).
    pub inlined_bytes: usize,
    /// Header slots of the visible inlined frames around the current point, outermost first.
    pub vis_scopes: Vec<i32>,
    /// With `RAVA_JIT_DUMP`: (code offset, method, bci) of each emitted instruction.
    pub dump_map: Option<Vec<(u32, String, usize)>>,
    /// Constant operand-stack values at the current call site, captured before the stack is
    /// written back (they become constant locals of an inlined callee).
    pub site_consts: Vec<Option<i64>>,
    /// Operand-stack values at the current call site known not to be null.
    pub site_nonnull: Vec<bool>,
    /// Local 0 of the body being emitted is `this` throughout (an instance method that never
    /// writes it); inlined bodies are entered with a receiver checked non-null.
    pub this_nonnull: bool,
    /// Locals of this body known to hold a constant: for the whole body (`lconst_base`: parameters
    /// it never writes) and at the current point (`lconst`: also constants stored in this block).
    pub lconst_base: Vec<Option<i64>>,
    pub lconst: Vec<Option<i64>>,
    /// Set while emitting an inlined body.
    pub inl: Option<InlineCtx>,
    /// The virtual operand stack and the registers pinned to locals (`vstack.rs`).
    pub vs: Vec<Slot>,
    pub lregs: Vec<Option<R>>,
    /// Slow-path trampolines used so far (`trampoline.rs`).
    pub tramps: Vec<(super::trampoline::Key, Label)>,
}

/// Compiles `m` at `tier` (1, or 2 for hot methods: larger inlining budget).
pub fn compile(m: &'static Method, tier: u8) -> Option<CompiledMethod> {
    if m.is_native() || m.is_synchronized() && m.is_static() && m.holder.mirror().is_null() {
        return None;
    }
    let started = std::time::Instant::now();
    let cm = compile_timed(m, tier);
    if let Some(s) = super::stats::get() {
        s.baseline_nanos.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    cm
}

fn compile_timed(m: &'static Method, tier: u8) -> Option<CompiledMethod> {
    let info: &'static CodeInfo = m.code?;
    let an = analyze(m)?;
    let stack_base = info.max_locals as i32 + header::SLOTS as i32;
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
        stack_base,
        scratch_base: stack_base + info.max_stack as i32 + 2,
        chain: vec![(m, None)],
        extent: stack_base + info.max_stack as i32 + 2,
        failed: false,
        tier,
        inlined_bytes: 0,
        vis_scopes: Vec::new(),
        dump_map: env_once!("RAVA_JIT_DUMP").filter(|f| m.to_string().contains(f.to_str().unwrap_or(""))).map(|_| Vec::new()),
        site_consts: Vec::new(),
        site_nonnull: Vec::new(),
        this_nonnull: !m.is_static() && super::fold::unwritten_locals(m).first() == Some(&true),
        lconst_base: vec![None; info.max_locals as usize],
        lconst: vec![None; info.max_locals as usize],
        inl: None,
        vs: Vec::new(),
        lregs: Vec::new(),
        tramps: Vec::new(),
    };
    if env_once!("RAVA_NO_LOCAL_REGS").is_none() {
        e.lregs = plan_local_regs(e.code, info.max_locals as usize, &e.an);
    }
    if e.dump_map.is_none() {
        // Short branches; the dump map records raw positions, which relaxation would move.
        e.a.enable_peephole();
    }
    let frame_patch = e.prologue();
    let mut leaders = Vec::new();
    e.emit_body(Some(&mut leaders))?;
    if e.failed {
        return None;
    }
    let frame_slots = e.extent;
    if frame_slots > 0xffff || e.max_locals > 0xffff {
        return None;
    }
    e.a.patch_i32(frame_patch, e.max_locals | frame_slots << 16);
    let leaders = e.emit_entry_stubs(leaders, frame_slots);
    e.emit_slow_paths();
    e.emit_poll_stubs();
    e.emit_trampolines();
    e.emit_exception_dispatch();
    e.emit_epilogue();
    let (bcis, stubs): (Vec<u32>, Vec<Label>) = leaders.into_iter().unzip();
    let (code, _, offs) = e.a.finish_labels(e.epilogue, &stubs);
    let entry = code_cache::install(&code);
    let blocks = bcis.into_iter().zip(offs).map(|(bci, off)| (bci, entry + off as usize)).collect();
    super::code_cache::perf_map(entry, code.len(), &format!("{m} [b{tier}]"));
    if let Some(filter) = env_once!("RAVA_JIT_DUMP") {
        // Raw code for `objdump -D -b binary -mi386:x86-64 --adjust-vma=<entry>`.
        let name = m.to_string();
        if name.contains(filter.to_str().unwrap_or("")) {
            let file = super::code_cache::dump_path(&format!("rava-jit-{entry:x}.bin"));
            // SAFETY: the code just installed (calls resolved).
            let _ = std::fs::write(&file, unsafe { std::slice::from_raw_parts(entry as *const u8, code.len()) });
            let map: String = e.dump_map.iter().flatten().map(|(off, m, bci)| format!("{off:x} {m}@{bci}\n")).collect();
            let _ = std::fs::write(format!("{file}.map"), map);
            eprintln!("[jit-dump] {name} -> {file} (entry {entry:#x})");
        }
    }
    if env_once!("RAVA_JIT_LOG").is_some() {
        eprintln!("[jit] {m} {} bytes of bytecode -> {} bytes", info.code.len(), code.len());
    }
    Some(CompiledMethod { entry, reg_entry: 0, unresolved_deferrals: Default::default(), size: code.len(), blocks, frame_slots: frame_slots as usize, tier, optimized: false, deopts: Default::default() })
}

impl Emitter {
    /// Emits the current body; the method's own body collects its entry blocks
    /// (`Analysis::entry`: OSR and handler targets) into `leaders`.
    pub(super) fn emit_body(&mut self, mut leaders: Option<&mut Vec<u32>>) -> Option<()> {
        // The entry state (constant arguments) holds at bci 0 unless a branch comes back there.
        let reentered = super::analysis::targets_start(self.m);
        let mut pc = 0;
        while pc < self.code.len() {
            let len = insn_len(self.code, pc);
            let Some(depth) = self.an.depth[pc] else {
                self.a.bind(self.labels[pc]);
                pc += len;
                continue;
            };
            if self.an.leader[pc] {
                // Blocks start with the operand stack in memory and only whole-body constants.
                self.flush_to_mem();
                self.vs_reset(depth);
                if pc != 0 || reentered {
                    self.lconst.clone_from(&self.lconst_base);
                }
                if let Some(l) = leaders.as_deref_mut().filter(|_| self.an.entry[pc]) {
                    l.push(pc as u32);
                }
            }
            self.a.bind(self.labels[pc]);
            if let Some(map) = self.dump_map.as_mut() {
                map.push((self.a.pos(), self.m.name.to_string(), pc));
            }
            let first_slow = self.slow.len();
            self.emit(pc, depth)?;
            if self.failed {
                return None;
            }
            let live = self.live_regs();
            self.slow[first_slow..].iter_mut().for_each(|s| s.3 = live.clone());
            pc += len;
        }
        self.a.bind(self.labels[self.code.len()]);
        Some(())
    }

    pub fn local(&self, n: usize) -> M {
        slot(self.local_base + n as i32)
    }

    /// Operand-stack slot `i` (0 = bottom).
    pub fn stk(&self, i: i32) -> M {
        slot(self.stack_base + i)
    }

    pub fn hdr(&self, field: usize) -> M {
        slot(self.local_base + self.max_locals + field as i32)
    }

    /// Records `bci` as the frame's current instruction (before calls and slow paths).
    pub fn set_bci(&mut self, bci: usize) {
        debug_assert!(!self.in_leaf(), "leaf bodies never record a bci");
        if self.inl.is_some_and(|c| !c.visible) {
            // A hidden frame is never walked; its caller's bci already names the call site.
            return;
        }
        let at = self.hdr(header::BCI);
        self.a.store_imm(at, (bci as i32) << 1);
    }

    pub fn label(&self, bci: usize) -> Label {
        self.labels[bci]
    }

    /// A slow path that re-executes this instruction in the interpreter, then continues with
    /// the next instruction.
    pub fn slow_path(&mut self, bci: usize, depth: u16) -> Label {
        if let (true, Some(ctx)) = (self.in_leaf(), self.inl) {
            return ctx.restart;
        }
        let l = self.a.new_label();
        self.slow.push((l, bci, depth, Vec::new(), Vec::new()));
        l
    }

    /// `slow_path` for an instruction whose operands may still be only in registers: the slow
    /// path writes back `spill` (from `lazy_flush`) before re-executing it.
    pub fn slow_path_spilling(&mut self, bci: usize, depth: u16, spill: Vec<(super::vstack::Loc, i32)>) -> Label {
        let l = self.slow_path(bci, depth);
        if let Some(s) = self.slow.last_mut().filter(|s| s.0 == l) {
            s.4 = spill;
        }
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
        if let (true, Some(ctx)) = (self.in_leaf(), self.inl) {
            return self.a.jmp(ctx.restart);
        }
        self.call_trampoline(bci, depth, helper);
    }

    /// Polls for a stop-the-world request (method entry and loop back-edges): frames are fully in
    /// memory at every instruction boundary, so the collector sees everything.
    pub fn poll(&mut self) {
        let (stub, back) = (self.a.new_label(), self.a.new_label());
        self.a.cmp8_mi(mem(R::R12, offsets::POLL), 0);
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

    /// Frame setup through the shared `frame_stubs::enter`; returns the position of its
    /// parameter, max_locals | frame_slots << 16, patched once every body is emitted.
    fn prologue(&mut self) -> u32 {
        self.a.mov_ri(R::Rcx, 0x7fff_0000);
        let frame_patch = self.a.pos() - 4;
        let counted = self.tier < 2;
        self.a.call_abs(super::frame_stubs::enter(counted));
        if self.m.is_synchronized() {
            self.enter_method_monitor();
        }
        self.load_local_regs();
        frame_patch
    }

    /// A synchronized method's monitor (the receiver, or the class's mirror) is taken once the
    /// frame is linked and kept in the 8 bytes `save_registers` leaves below the saved
    /// registers, for the epilogue to release. An OSR entry finds 0 there: the interpreter took
    /// the monitor and releases it itself (`osr_trampoline`).
    fn enter_method_monitor(&mut self) {
        let (slow, done) = (self.a.new_label(), self.a.new_label());
        if self.m.is_static() {
            self.a.mov_ri(R::Rcx, self.m.holder.mirror() as usize as i64);
        } else {
            self.a.load(true, R::Rcx, self.local(0));
        }
        self.a.store(true, mem(R::Rsp, 0), R::Rcx);
        super::emit_obj::monitor_enter_fast(&mut self.a, R::Rcx, slow);
        self.a.jmp(done);
        self.a.bind(slow);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_rr(true, R::Rsi, R::Rcx);
        self.a.call_abs(runtime::monitor_enter as *const () as usize);
        self.a.bind(done);
    }

    /// Releases the monitor `enter_method_monitor` took, if this frame took one; the result in
    /// rax stays (an IllegalMonitorStateException is left pending instead).
    fn exit_method_monitor(&mut self) {
        let (slow, done) = (self.a.new_label(), self.a.new_label());
        self.a.load(true, R::Rcx, mem(R::Rsp, 0));
        self.a.test_rr(true, R::Rcx, R::Rcx);
        self.a.jcc(Cond::E, done);
        self.a.push(R::Rax);
        self.a.push(R::Rax);
        let release = self.a.new_label();
        super::emit_obj::monitor_exit_fast(&mut self.a, R::Rcx, slow);
        self.a.jmp(release);
        self.a.bind(slow);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_rr(true, R::Rsi, R::Rcx);
        self.a.call_abs(runtime::monitor_exit as *const () as usize);
        self.a.bind(release);
        self.a.pop(R::Rax);
        self.a.pop(R::Rax);
        self.a.bind(done);
    }

    /// Loads the registers pinned to locals from their slots.
    fn load_local_regs(&mut self) {
        for n in 0..self.lregs.len() {
            if let Some(r) = self.lregs[n] {
                self.a.load(true, r, self.local(n));
            }
        }
    }

    /// Entry points for OSR and exception handlers at their blocks: claim the whole compiled
    /// frame (an interpreted frame is smaller than one with inlined frames), load the local
    /// registers, then continue at the block. Returns (bci, stub label) pairs.
    fn emit_entry_stubs(&mut self, leaders: Vec<u32>, frame_slots: i32) -> Vec<(u32, Label)> {
        let mut out = Vec::with_capacity(leaders.len());
        for bci in leaders {
            let l = self.a.new_label();
            self.a.bind(l);
            out.push((bci, l));
            self.a.lea(R::Rax, slot(frame_slots));
            self.a.store(true, mem(R::R12, offsets::SP), R::Rax);
            self.load_local_regs();
            self.a.jmp(self.labels[bci as usize]);
        }
        out
    }

    /// Slow paths re-execute one instruction in the interpreter, reload the operand-stack
    /// registers the fast path left live, and continue after the instruction.
    pub(super) fn emit_slow_paths(&mut self) {
        for (l, bci, depth, reload, spill) in std::mem::take(&mut self.slow) {
            self.a.bind(l);
            self.write_back_all(&spill);
            self.defer(bci, depth);
            self.reload_live(&reload);
            let next = bci + insn_len(self.code, bci);
            self.a.jmp(self.labels[next]);
        }
    }

    fn emit_exception_dispatch(&mut self) {
        self.a.bind(self.exc);
        self.a.call_abs(super::frame_stubs::dispatch());
        // No handler here: unwind.
        self.a.alu_rr(false, super::asm::Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(self.epilogue);
    }

    fn emit_epilogue(&mut self) {
        self.a.bind(self.epilogue);
        if self.m.is_synchronized() {
            // Every exit (return or exception) releases it.
            self.exit_method_monitor();
        }
        self.a.mov_ri(R::Rcx, self.max_locals as i64);
        let float = matches!(self.m.sig.ret, crate::types::BasicType::Float | crate::types::BasicType::Double);
        self.a.jmp_abs(super::frame_stubs::leave(float));
    }
}

/// r13 points this many bytes past the frame base, so that the first 32 slots (locals, header
/// and the operand stack of most methods) are reached with 8-bit displacements.
pub(super) const FRAME_BIAS: i32 = 128;

/// Frame slot `n` of baseline code.
pub(super) fn slot(n: i32) -> M {
    mem(R::R13, 8 * n - FRAME_BIAS)
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
