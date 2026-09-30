//! Machine code from an allocated graph: the frame protocol of the baseline compiler (locals,
//! header and outgoing arguments in the value-stack frame at `r13`, `r12` = thread), with SSA
//! values in registers and spill slots on the native stack. Guards and exceptions leave through
//! deoptimization stubs that write the interpreter state into the frame (`deopt.rs`).

use super::ir::*;
use super::moves;
use super::regalloc::{phi_input, phis, Allocation, Loc, CALLEE_SAVED, CALLER_SAVED, XMMS};
use crate::jit::asm::{mem, Alu, Asm, Cond, Label, M, R, X};
use crate::jit::runtime;
use crate::method::Method;
use crate::thread::{header, offsets};
use std::collections::HashMap;
use std::rc::Rc;

mod stubs;

/// The monitor a synchronized method holds while it runs.
#[derive(Clone, Copy)]
pub(super) enum Monitor {
    /// The receiver (instance methods).
    Receiver,
    /// The class's mirror (static methods).
    Mirror(usize),
}

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
    /// Home slots (`Allocation::home`), written at each value's definition.
    home: &'a [Option<Loc>],
    pub max_locals: i32,
    labels: Vec<Label>,
    pub epilogue: Label,
    deopts: HashMap<*const State, Label>,
    deopt_list: Vec<(Label, Rc<State>)>,
    polls: Vec<(Label, Label)>,
    edge_stubs: Vec<(Label, B, B)>,
    /// Callee-saved registers the prologue saves (the frame, and those allocated).
    saved: Vec<R>,
    /// Bytes below them: spill slots and alignment.
    frame_bytes: i32,
    /// Caller-saved registers holding values somewhere in this method (runtime calls from
    /// slow paths preserve them).
    live_gprs: Vec<R>,
    live_xmms: Vec<u8>,
    /// A synchronized method's monitor and where on the native stack (from rsp) it is kept for
    /// the exit (not in the frame: callees' frames start at the outgoing arguments).
    lock: Option<(Monitor, i32)>,
    /// A leaf that cannot stop, throw, deoptimize or call Java: no frame header is written or
    /// linked (nothing walks the stack while it runs), no poll or stack check is needed.
    frameless: bool,
    /// Registers kept around each call (`Allocation::saves`) and their slots; slots from
    /// `save_base` are written before the call, home slots already hold the value.
    saves: &'a HashMap<V, Vec<(Loc, Loc)>>,
    save_base: i32,
    /// Stubs restoring a call's saved registers before a deoptimization: (entry, call, target).
    restore_stubs: Vec<(Label, V, Label)>,
    /// Constants placed after the code for RIP-relative operands.
    consts: HashMap<u64, Label>,
    /// Out-of-line runtime calls: (entry, helper, argument, where to continue with rax).
    pub slow_calls: Vec<(Label, usize, V, Label)>,
    /// Value-stack slots of the frame, inlined frames included (published as `Thread::sp`).
    frame_slots: i32,
    /// Reference stores that need no card mark (`barrier.rs`).
    pub unbarriered: Vec<bool>,
}

/// Whether `g` qualifies for a frameless body: straight-line or branching code (no loops,
/// hence no polls) without calls, allocation, checks that throw, monitors or deoptimization.
fn frameless(g: &Graph) -> bool {
    g.blocks.iter().filter(|b| !matches!(b.term, Term::None)).all(|b| {
        !b.loop_header
            && matches!(b.term, Term::Goto(_) | Term::If { .. } | Term::Switch { .. } | Term::Return(_))
            && b.insts.iter().all(|&v| match &g.values[v as usize].inst {
                Inst::Param(_) | Inst::Const(_) | Inst::Copy(_) | Inst::Phi(_) | Inst::Neg(_) | Inst::Bin(..) | Inst::Convert(..) | Inst::Compare(..) => true,
                Inst::LoadField(..) | Inst::StoreField(..) | Inst::LoadStatic(..) | Inst::StoreStatic(..) | Inst::ArrayLength(_) => true,
                Inst::LoadIndexed(..) | Inst::InstanceOf(..) => true,
                Inst::Intrinsic(k, _, s) => s.is_none() && k.is_pure(),
                _ => false,
            })
    })
}

/// Emits `g` for `m`, whose frame (inlined frames included) takes `frame_slots` value-stack slots;
/// a synchronized method holds `monitor` while it runs. Returns the code and the size of its
/// main part (without the out-of-line stubs after it, as C2 sizes code for inlining).
pub(super) fn generate(g: &Graph, al: &Allocation, m: &'static Method, frame_slots: i32, monitor: Option<Monitor>) -> (Vec<u8>, usize) {
    let max_locals = m.code.expect("compiled method has code").max_locals as i32;
    let mut a = Asm::default();
    a.enable_peephole();
    let labels = (0..g.blocks.len()).map(|_| a.new_label()).collect();
    let epilogue = a.new_label();
    let mut gen = Gen {
        a,
        g,
        loc: &al.loc,
        home: &al.home,
        max_locals,
        labels,
        epilogue,
        deopts: HashMap::new(),
        deopt_list: Vec::new(),
        polls: Vec::new(),
        edge_stubs: Vec::new(),
        saved: Vec::new(),
        frame_bytes: 0,
        live_gprs: CALLER_SAVED.into_iter().filter(|r| al.loc.contains(&Loc::Reg(*r))).collect(),
        live_xmms: XMMS.into_iter().filter(|x| al.loc.contains(&Loc::Xmm(*x))).collect(),
        lock: None,
        frameless: monitor.is_none() && frameless(g),
        saves: &al.saves,
        save_base: al.save_base,
        restore_stubs: Vec::new(),
        consts: HashMap::new(),
        slow_calls: Vec::new(),
        unbarriered: super::barrier::unbarriered(g),
        frame_slots,
    };
    // Entry rsp is 8 mod 16: an odd number of pushes realigns it, spill slots come in pairs.
    // r12 holds the thread in all compiled code (Rust enters through `jit::enter_trampoline`).
    gen.saved = [R::R13].into_iter().chain(CALLEE_SAVED.into_iter().filter(|r| al.loc.contains(&Loc::Reg(*r)))).collect();
    let spill_bytes = 8 * (al.spill_slots + (al.spill_slots & 1));
    gen.lock = monitor.map(|mon| (mon, spill_bytes));
    gen.frame_bytes = spill_bytes + if monitor.is_some() { 16 } else { 0 } + if gen.saved.len() % 2 == 0 { 8 } else { 0 };
    let overflow = gen.a.new_label();
    gen.prologue(frame_slots, overflow);
    for (i, &b) in al.order.iter().enumerate() {
        gen.a.bind(gen.labels[b as usize]);
        for &v in &g.blocks[b as usize].insts {
            gen.inst(v);
            if let (Some(h @ Loc::Stack(_)), true) = (al.home[v as usize], g.resolve(v) == v && !matches!(g.inst(v), Inst::Phi(_))) {
                moves::mov(&mut gen.a, h, al.loc[v as usize]);
            }
        }
        gen.term(b, al.order.get(i + 1).copied());
    }
    let stubs = gen.a.new_label();
    gen.a.bind(stubs);
    gen.emit_stubs();
    gen.emit_epilogue(overflow);
    let mut pool: Vec<(u64, Label)> = gen.consts.drain().collect();
    pool.sort_by_key(|&(bits, _)| bits);
    for (bits, l) in pool {
        gen.a.data8(l, bits);
    }
    let (code, main) = gen.a.finish_at(stubs);
    (code, main as usize)
}

impl Gen<'_> {
    pub fn hdr(&self, field: usize) -> M {
        mem(R::R13, 8 * (self.max_locals + field as i32))
    }

    /// Frame slot `n` (from the frame base).
    pub fn slot(&self, n: i32) -> M {
        mem(R::R13, 8 * n)
    }

    fn prologue(&mut self, frame_slots: i32, overflow: Label) {
        let a = &mut self.a;
        for &r in &self.saved {
            a.push(r);
        }
        if self.frame_bytes > 0 {
            a.alu_ri(true, Alu::Sub, R::Rsp, self.frame_bytes);
        }
        a.mov_rr(true, R::R13, R::Rsi);
        if self.frameless {
            return;
        }
        a.lea(R::Rax, mem(R::R13, 8 * frame_slots));
        a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::LIMIT));
        a.jcc(Cond::A, overflow);
        let h = |f: usize| mem(R::R13, 8 * (self.max_locals + f as i32));
        // The frame is linked to its caller (every caller publishes its innermost header as
        // `Thread::top` before a call), but `top` and `sp` are only published here once the
        // method calls out (`link`, `anchor`); see the module comment.
        a.store(true, h(header::METHOD), R::Rdx);
        a.store_imm(h(header::BCI), 0);
        a.load(true, R::Rcx, mem(R::R12, offsets::TOP));
        a.store(true, h(header::CALLER), R::Rcx);
        self.poll();
        if let Some((mon, at)) = self.lock {
            match mon {
                Monitor::Receiver => self.a.load(true, R::Rcx, self.slot(0)),
                Monitor::Mirror(o) => self.a.mov_ri(R::Rcx, o as i64),
            }
            self.a.store(true, mem(R::Rsp, at), R::Rcx);
            self.method_monitor(true);
        }
    }

    /// Enters or exits the synchronized method's monitor in rcx: the thin-lock fast path, else
    /// the runtime.
    fn method_monitor(&mut self, enter: bool) {
        let (slow, done) = (self.a.new_label(), self.a.new_label());
        if enter {
            crate::jit::emit_obj::monitor_enter_fast(&mut self.a, R::Rcx, slow);
        } else {
            crate::jit::emit_obj::monitor_exit_fast(&mut self.a, R::Rcx, slow);
        }
        self.a.jmp(done);
        self.a.bind(slow);
        self.anchor();
        let helper = if enter { runtime::monitor_enter as *const () as usize } else { runtime::monitor_exit as *const () as usize };
        self.call_preserving(helper, &[Arg::Thread, Arg::Reg(R::Rcx)]);
        self.a.bind(done);
    }

    fn emit_epilogue(&mut self, overflow: Label) {
        let restore = self.a.new_label();
        self.a.bind(self.epilogue);
        if let Some((_, at)) = self.lock {
            // Every exit (return, exception, deoptimization) releases the monitor; the result
            // stays in rax, and an IllegalMonitorStateException replaces it.
            self.a.load(true, R::Rcx, mem(R::Rsp, at));
            self.a.push(R::Rax);
            self.a.push(R::Rax);
            self.method_monitor(false);
            self.a.pop(R::Rax);
            self.a.pop(R::Rax);
        }
        if !self.frameless {
            // The caller's frame is innermost again; `sp` is left as published (callers do
            // not rely on it, see the module comment).
            self.a.load(true, R::Rcx, self.hdr(header::CALLER));
            self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        }
        self.a.bind(restore);
        if self.frame_bytes > 0 {
            self.a.alu_ri(true, Alu::Add, R::Rsp, self.frame_bytes);
        }
        for &r in self.saved.iter().rev() {
            self.a.pop(r);
        }
        self.a.ret();
        // The frame was never linked: raise, then return without unlinking.
        self.a.bind(overflow);
        // The caller's frame is innermost; its arguments end where this frame would start.
        self.a.mov_rr(true, R::Rax, R::R13);
        self.raise_sp();
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::stack_overflow as *const () as usize);
        self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(restore);
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
        let gprs = self.live_gprs.clone();
        let xmms = self.live_xmms.clone();
        for &r in &gprs {
            self.a.push(r);
        }
        let pad = gprs.len() % 2 == 1;
        if pad {
            self.a.push(R::Rax);
        }
        let xbytes = (8 * xmms.len() as i32 + 15) & !15;
        if xbytes > 0 {
            self.a.alu_ri(true, Alu::Sub, R::Rsp, xbytes);
            for (i, &x) in xmms.iter().enumerate() {
                self.a.fstore(true, mem(R::Rsp, 8 * i as i32), X(x));
            }
        }
        for (i, arg) in args.iter().enumerate() {
            match *arg {
                Arg::Thread => self.a.mov_rr(true, PARAMS[i], R::R12),
                Arg::Imm(c) => self.a.mov_ri(PARAMS[i], c),
                _ if PARAMS[i] != SCRATCH[i] => self.a.mov_rr(true, PARAMS[i], SCRATCH[i]),
                _ => {}
            }
        }
        self.a.call_abs(addr);
        if xbytes > 0 {
            for (i, &x) in xmms.iter().enumerate() {
                self.a.fload(true, X(x), mem(R::Rsp, 8 * i as i32));
            }
            self.a.alu_ri(true, Alu::Add, R::Rsp, xbytes);
        }
        if pad {
            self.a.pop(R::R11);
        }
        for r in gprs.iter().rev() {
            self.a.pop(*r);
        }
    }

    /// Before a call out: stores the call's bytecode index in each frame header, links the
    /// visible inlined frames (hidden ones, method-handle plumbing, are never walked) and
    /// publishes the innermost header as `Thread::top`; with `sp`, also the frame's end as
    /// `Thread::sp` (for runtime calls, which may collect or run Java code at `sp`). Clobbers
    /// rax and rcx.
    pub fn link(&mut self, s: &State, sp: bool) {
        let scopes = scopes(s);
        let (outer, inner) = scopes.split_last().expect("a state has a scope");
        let at = self.hdr(header::BCI);
        self.a.store_imm(at, (outer.bci as i32) << 1);
        self.a.lea(R::Rcx, self.hdr(header::METHOD));
        for s in inner.iter().rev().filter(|s| !s.method.is_hidden_frame()) {
            let hdr = s.frame + max_locals(s.method);
            self.a.mov_ri(R::Rax, s.method as *const Method as i64);
            self.a.store(true, self.slot(hdr + header::METHOD as i32), R::Rax);
            self.a.store_imm(self.slot(hdr + header::BCI as i32), (s.bci as i32) << 1);
            self.a.store(true, self.slot(hdr + header::CALLER as i32), R::Rcx);
            self.a.lea(R::Rcx, self.slot(hdr + header::METHOD as i32));
        }
        self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        if sp {
            self.publish_sp();
        }
    }

    /// Publishes this frame as the innermost one (`Thread::top` and `sp`) for a runtime call
    /// without a state (polls, monitors, deoptimization). Clobbers rax.
    pub fn anchor(&mut self) {
        self.a.lea(R::Rax, self.hdr(header::METHOD));
        self.a.store(true, mem(R::R12, offsets::TOP), R::Rax);
        self.publish_sp();
    }

    /// Raises `Thread::sp` to the end of this frame. Clobbers rax and the flags.
    pub fn publish_sp(&mut self) {
        self.a.lea(R::Rax, self.slot(self.frame_slots));
        self.raise_sp();
    }

    /// `Thread::sp := max(sp, rax)`: `sp` only ever rises while optimized code runs, as code
    /// that called it (baseline code, the interpreter) needs it at or above its own frame's end
    /// when the call returns (it runs Java code at `sp`).
    fn raise_sp(&mut self) {
        let keep = self.a.new_label();
        self.a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::SP));
        self.a.jcc(Cond::Be, keep);
        self.a.store(true, mem(R::R12, offsets::SP), R::Rax);
        self.a.bind(keep);
    }

    /// Frame slot of operand-stack slot 0 of the scope of `s`.
    pub fn stack_base_of(s: &State) -> i32 {
        s.frame + max_locals(s.method) + header::SLOTS as i32
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
            Term::If { cmp, ty, a, b: y, then, other, unordered } => {
                if then == other {
                    return self.edge(b, then, next == Some(then));
                }
                let c = if matches!(ty, Ty::F | Ty::D) {
                    // ucomis: unordered sets PF (and ZF, CF), ordered results read as unsigned.
                    let (xa, xb) = (self.xreg(a, X(0)), self.xreg(y, X(1)));
                    self.a.fcmp(ty == Ty::D, xa, xb);
                    let nan = self.edge_target(b, if unordered { then } else { other });
                    self.a.jcc(Cond::P, nan);
                    unsigned(cmp)
                } else {
                    let x = self.reg(a, R::Rax);
                    self.alu_val(ty != Ty::I, Alu::Cmp, x, y);
                    cond(cmp)
                };
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
                    if self.g.ty(v) == Ty::I {
                        self.a.movsxd(R::Rax, R::Rax);
                    }
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
        self.g.blocks[to as usize].order <= self.g.blocks[from as usize].order
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

}

/// The scopes of a state, innermost first.
fn scopes(s: &State) -> Vec<&State> {
    let mut out = vec![s];
    while let Some(c) = &out.last().unwrap().caller {
        out.push(c);
    }
    out
}

fn max_locals(m: &Method) -> i32 {
    m.code.map_or(0, |c| c.max_locals as i32)
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

/// The condition of `cmp` on flags set by an unsigned-style comparison (ucomisd).
fn unsigned(c: Cmp) -> Cond {
    match c {
        Cmp::Eq => Cond::E,
        Cmp::Ne => Cond::Ne,
        Cmp::Lt => Cond::B,
        Cmp::Ge => Cond::Ae,
        Cmp::Gt => Cond::A,
        Cmp::Le => Cond::Be,
    }
}

pub fn invert(c: Cond) -> Cond {
    // x86 condition codes come in complementary pairs differing in the low bit.
    // SAFETY: `Cond` is `repr(u8)` with all 16 codes.
    unsafe { std::mem::transmute::<u8, Cond>(c as u8 ^ 1) }
}
